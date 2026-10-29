//! The `mycelium` command: the node, the far end of a link, and a few
//! requests of a running node's socket for looking into it by hand. Pi and
//! Quickshell read the socket themselves; nothing here is how a person uses
//! the network.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};

use clap::{Parser, Subcommand};
use mycelium::client::Client;
use mycelium::group::{Group, Pattern};
use mycelium::link::dial::{Connection, Ssh};
use mycelium::link::discovery::Listed;
use mycelium::link::membership::Realm;
use mycelium::record::Intent;
use mycelium::share::exchange::Places;
use mycelium::share::ledger::Ledgers;
use mycelium::store::folders::Folders;
use mycelium::wire::{Draft, Request};
use mycelium::{hold, link, node, place};

#[derive(Parser)]
#[command(version, about = "A nimble core for agent networks")]
struct Command {
    /// The realm: a set of machines that link, with nothing crossing
    /// between realms.
    #[arg(long, env = "MYCELIUM_REALM", global = true)]
    realm: Option<String>,
    /// The node's socket, if not where the realm's goes.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Runs this machine's node.
    Node {
        #[command(flatten)]
        here: Here,
        /// A machine to keep linked with over SSH, such as
        /// `joel.gerber@192.168.4.32`; given again for each.
        #[arg(long = "ssh")]
        destinations: Vec<String>,
        /// An SSH option for every link, as `-o` takes it.
        #[arg(long = "ssh-option")]
        options: Vec<String>,
        /// The `mycelium` command on the other machines.
        #[arg(long, default_value = "mycelium")]
        remote_command: String,
    },
    /// Runs this machine's end of a link over standard input and output, as
    /// another machine's node starts it over SSH.
    Link {
        #[command(flatten)]
        here: Here,
    },
    /// Shares a quest with every machine this one links with: its record,
    /// kept the same on each, written on any, and the folders of its
    /// scratch named. A machine that held its own copy of either moves it
    /// aside first.
    Share {
        #[command(flatten)]
        here: Here,
        /// The quest's id, such as `QEST-20261013-4UHC5Q`.
        quest: String,
        /// A folder of the quest's scratch to share too, such as `notes`;
        /// none is shared unless named, and one holding more than 10,000
        /// files or 1 GiB is refused.
        #[arg(long)]
        scratch: Vec<String>,
    },
    /// Prints every session the node knows of, a JSON line each.
    Sessions,
    /// Prints what this machine is, as its node measures it.
    Machine,
    /// Prints the latest messages, a JSON line each.
    Messages {
        /// Which groups, such as `quest.*`.
        #[arg(default_value = ">")]
        groups: Pattern,
        /// At most this many.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Sends a message on the mesh, as a session would.
    Send {
        /// Who sends it.
        #[arg(long)]
        from: String,
        /// What it asks of those who hear it: hold, question, finding, fyi
        /// or pivot.
        #[arg(long, value_parser = intent)]
        intent: Option<Intent>,
        /// The groups it goes to, such as `peer.4uhc5q`.
        #[arg(required = true)]
        groups: Vec<Group>,
        /// What is said.
        #[arg(long)]
        text: String,
    },
    /// Starts a program on a terminal a holder of its own keeps, so its
    /// window can close and `attach` take it up again from any terminal;
    /// then joins it, unless `--detached`.
    Hold {
        /// The folder it starts in, if not this one.
        #[arg(long)]
        folder: Option<PathBuf>,
        /// Prints its id and leaves it running, unjoined.
        #[arg(long)]
        detached: bool,
        /// The program and its arguments, after `--`.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Joins a held session from this terminal; closing it only leaves.
    Attach {
        /// Its id, as `held` lists it.
        id: String,
    },
    /// Prints every session held on this machine, a JSON line each.
    Held,
    /// Holds a program, as `hold` starts it, in a process of its own.
    #[command(hide = true)]
    Holder {
        #[arg(long)]
        id: String,
        #[arg(long)]
        folder: PathBuf,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Prints the node's events as they come, a JSON line each.
    Watch {
        /// Which groups' messages, such as `quest.*`.
        #[arg(default_value = ">")]
        groups: Pattern,
        /// How many of the latest messages come first.
        #[arg(long, default_value_t = 20)]
        recent: usize,
    },
}

/// What a node or a link's end needs of this machine.
#[derive(clap::Args)]
struct Here {
    /// The harness mesh's folders, if not where the harness keeps them.
    #[arg(long)]
    mesh: Option<PathBuf>,
    /// This machine's name, if not the system's.
    #[arg(long)]
    machine: Option<String>,
    /// Mycelium's own folder, if not where it goes by default.
    #[arg(long)]
    state: Option<PathBuf>,
}

fn main() -> ExitCode {
    let command = Command::parse();
    match run(command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mycelium: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> io::Result<()> {
    let realm = command.realm.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "say which realm, with --realm or MYCELIUM_REALM",
        )
    })?;
    // A realm names a socket and goes into the command run over SSH, so it
    // is held to letters, digits, `-` and `_`.
    if realm.is_empty()
        || !realm
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{realm:?} is not a realm: use letters, digits, - and _"),
        ));
    }
    let socket = command.socket.unwrap_or_else(|| place::socket(&realm));
    match command.action {
        Action::Node {
            here,
            destinations,
            options,
            remote_command,
        } => {
            let dials = destinations
                .into_iter()
                .map(|destination| Ssh {
                    destination,
                    options: options.clone(),
                    command: remote_command.clone(),
                })
                .collect();
            run_node(here, realm, &socket, dials)
        }
        Action::Link { here } => {
            let (store, membership, shares) = open(here, realm)?;
            runtime()?.block_on(async {
                let connection = Connection {
                    reading: Box::new(tokio::io::stdin()),
                    writing: Box::new(tokio::io::stdout()),
                    keep: Box::new(()),
                };
                link::run(store, &membership, connection, Some(shares)).await
            })
        }
        Action::Share {
            here,
            quest,
            scratch,
        } => {
            let (_, _, shares) = open(here, realm)?;
            share(&shares, &quest, &scratch)
        }
        Action::Sessions => print_reply(&socket, &Request::Sessions),
        Action::Machine => print_reply(&socket, &Request::Machine),
        Action::Messages { groups, limit } => {
            print_reply(&socket, &Request::Messages { groups, limit })
        }
        Action::Send {
            from,
            intent,
            groups,
            text,
        } => print_reply(
            &socket,
            &Request::Send(Draft {
                from,
                groups,
                intent,
                text,
                re: None,
            }),
        ),
        Action::Hold {
            folder,
            detached,
            command,
        } => hold(&realm, folder, detached, &command),
        Action::Attach { id } => attach(&place::held(&realm), &id),
        Action::Held => hold::held(&place::held(&realm))
            .iter()
            .try_for_each(print_line),
        Action::Holder {
            id,
            folder,
            command,
        } => be_holder(&place::held(&realm), &id, &folder, &command),
        Action::Watch { groups, recent } => {
            let mut client = Client::connect(&socket)?;
            print_line(&client.ask(&Request::Watch { groups, recent })?)?;
            while let Some(event) = client.next_event()? {
                print_line(&event)?;
            }
            Ok(())
        }
    }
}

/// Runs this machine's node on `socket`, keeping linked over `dials`,
/// until it is asked to stop.
fn run_node(here: Here, realm: String, socket: &Path, dials: Vec<Ssh>) -> io::Result<()> {
    let (store, membership, shares) = open(here, realm)?;
    runtime()?.block_on(async {
        let bound = node::bind(socket)?;
        node::run(
            store,
            membership,
            Listed(dials),
            Some(shares),
            bound,
            stopped(),
        )
        .await
    })
}

fn open(here: Here, realm: String) -> io::Result<(Folders, Realm, Places)> {
    let state = here.state.unwrap_or_else(place::state);
    place::settle_realm(&state, &realm)?;
    let store = Folders::open(
        &here.mesh.unwrap_or_else(place::mesh),
        &state.join("claims"),
    )?;
    let membership = Realm {
        name: realm,
        machine: here.machine.unwrap_or_else(place::machine),
    };
    Ok((store, membership, place::shares(&state)))
}

/// Shares `quest`, which must be here to share, with each of the folders
/// of its scratch named in `scratch`.
fn share(shares: &Places, quest: &str, scratch: &[String]) -> io::Result<()> {
    let record = shares.record(quest);
    if !record.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{quest} isn't on this machine: {} holds nothing",
                record.display()
            ),
        ));
    }
    let ledgers: Ledgers = shares.ledgers();
    ledgers.hold(quest)?.keep()?;
    for folder in scratch {
        shares.name_scratch(quest, folder)?;
    }
    let folders = ledgers.hold(quest)?.ledger.scratch;
    let with = if folders.is_empty() {
        "none of its scratch".to_owned()
    } else {
        let names: Vec<String> = folders.into_iter().collect();
        format!("its scratch's {}", names.join(", "))
    };
    println!("{quest} is shared, with {with}; each link carries it from its next look");
    Ok(())
}

/// Holds `command`, started in `folder` or this one, and joins it from
/// this terminal unless `detached`, when it prints its id instead.
fn hold(
    realm: &str,
    folder: Option<PathBuf>,
    detached: bool,
    command: &[String],
) -> io::Result<()> {
    let folder = match folder {
        Some(folder) => folder,
        None => std::env::current_dir()?,
    };
    let id = start_holder(realm, &folder, command)?;
    if detached {
        println!("{id}");
        Ok(())
    } else {
        attach(&place::held(realm), &id)
    }
}

/// Starts the holder for `command` in a process of its own, answering its
/// id once it holds.
fn start_holder(realm: &str, folder: &Path, command: &[String]) -> io::Result<String> {
    let id = hold::new_id();
    let mut holder = std::process::Command::new(std::env::current_exe()?)
        .args(["--realm", realm, "holder", "--id", &id, "--folder"])
        .arg(folder)
        .arg("--")
        .args(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut said = String::new();
    if let Some(out) = holder.stdout.take() {
        io::BufReader::new(out).read_line(&mut said)?;
    }
    match said.trim() {
        "held" => Ok(id),
        "" => Err(io::Error::other("its holder ended without holding it")),
        why => Err(io::Error::other(format!(
            "couldn't hold {}: {why}",
            command.join(" ")
        ))),
    }
}

/// Is the holder `hold` starts: in a session of its own, so the window it
/// was started from never hangs up on it, saying `held` once a terminal
/// can join, or why it can't.
fn be_holder(dir: &Path, id: &str, folder: &Path, command: &[String]) -> io::Result<()> {
    rustix::process::setsid()?;
    let holding = hold::holder::Holding {
        dir,
        id,
        command,
        folder,
    };
    let held = runtime()?.block_on(hold::holder::hold(&holding, |_| {
        let mut out = io::stdout();
        writeln!(out, "held")?;
        out.flush()?;
        // Nobody reads it from here on, and writing to a pipe nobody
        // reads would end this process.
        let nowhere = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
        rustix::stdio::dup2_stdout(&nowhere)?;
        Ok(())
    }));
    if let Err(error) = &held {
        // Said to whoever started it, if it hadn't said `held` yet.
        let _ = writeln!(io::stdout(), "{error}");
    }
    held.map(|_| ())
}

/// Joins the session held as `id` from this terminal until it ends.
fn attach(dir: &Path, id: &str) -> io::Result<()> {
    let socket = hold::socket(dir, id);
    if !socket.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("nothing is held as {id}; `mycelium held` lists what is"),
        ));
    }
    let runtime = runtime()?;
    let joined = runtime.block_on(hold::attach::join_here(&socket));
    // Typing is read on a thread of its own, waiting on a key nobody will
    // press now.
    runtime.shutdown_background();
    joined?;
    eprintln!("mycelium: the session held as {id} has ended");
    Ok(())
}

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

/// Resolves when the node is asked to stop, by Ctrl-C or by its service
/// manager's terminate signal.
async fn stopped() {
    let Ok(mut terminate) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    else {
        // With no terminate signal to hear, Ctrl-C alone stops the node.
        let _ = tokio::signal::ctrl_c().await;
        return;
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}

fn print_reply(socket: &Path, request: &Request) -> io::Result<()> {
    print_line(&Client::connect(socket)?.ask(request)?)
}

fn print_line(value: &impl serde::Serialize) -> io::Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, value).map_err(io::Error::other)?;
    writeln!(out)
}

fn intent(name: &str) -> Result<Intent, String> {
    serde_json::from_value(serde_json::Value::String(name.to_owned()))
        .map_err(|_| format!("{name:?} is not hold, question, finding, fyi or pivot"))
}
