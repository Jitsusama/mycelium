//! The `mycelium` command: the node, the far end of a link, and a few
//! requests of a running node's socket for looking into it by hand. Pi and
//! Quickshell read the socket themselves; nothing here is how a person uses
//! the network.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use mycelium::awake::Awake;
use mycelium::carry::through::Through;
use mycelium::client::Client;
use mycelium::flow::Flows;
use mycelium::group::{Group, Pattern};
use mycelium::hold::start::{Starter, login_shell};
use mycelium::link::dial::{Connection, Program, Ssh, Way};
use mycelium::link::discovery::Listed;
use mycelium::link::membership::{Membership, Proven, Realm};
use mycelium::link::refused::Refusals;
use mycelium::placement::{Need, Work};
use mycelium::record::Intent;
use mycelium::share::exchange::Places;
use mycelium::ssh::Asked as KeyAsked;
use mycelium::store::folders::Folders;
use mycelium::wire::{Draft, Request, Start};
use mycelium::{carry, hold, install, link, node, place, served};

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
    Node(Node),
    /// Runs this machine's end of a link over standard input and output, as
    /// another machine's node starts it over SSH.
    Link {
        #[command(flatten)]
        here: Here,
    },
    /// Runs what another machine's link key asked, as its pinned
    /// `authorized_keys` line runs this: a link, whose far end must be
    /// that machine, or an attach; anything else is refused and kept in
    /// `refused-keys.jsonl` in mycelium's folder.
    Ssh {
        #[command(flatten)]
        here: Here,
        /// The machine whose key this is.
        #[arg(long)]
        from: String,
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
    /// Carries a quest to another machine of the realm, its sessions with
    /// it: each of its sessions here carries its work into the quest's
    /// handover and ends, `pi-carry` takes the rest, the quest is shared
    /// with its handover, and each session starts again there, held, once
    /// `pi-carry arrive` has its trees. Prints a JSON line for each step,
    /// `{"step", "state", "said"}`, and last where each session went.
    Carry {
        #[command(flatten)]
        here: Here,
        /// The quest's id, such as `QEST-20261013-4UHC5Q`.
        quest: String,
        /// The machine it goes to, by name.
        #[arg(long)]
        to: String,
    },
    /// Prints every session the node knows of, a JSON line each.
    Sessions,
    /// Prints what this machine is, as its node measures it.
    Machine,
    /// Prints what every machine of the realm is, this one first.
    Machines,
    /// Prints the shapes of machine the node's providers could make, and
    /// why any couldn't say.
    Offers,
    /// Prints which machine of the realm work should go to, or why none.
    Place(Asked),
    /// Asks a machine to update itself to its configuration's main, this
    /// one or one this machine links with directly, and prints how that
    /// ended, as its updater said it.
    Update {
        /// The machine, such as `penelope`.
        machine: String,
    },
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
        /// The realm it is for, by a flow, if not this one.
        #[arg(long = "to-realm")]
        to_realm: Option<String>,
    },
    /// Prints the realms this realm's text may cross into.
    Flows,
    /// Asks the node to start a program held, on the machine of the realm
    /// placement chooses, through that person's login shell; prints the
    /// machine, its id and the words that attach to it.
    Start(Starting),
    /// Puts what is said into the steer inbox of the session going by
    /// ADDRESS, on whichever machine it runs; reads the steer from input,
    /// as the harness writes one: `{"text": "..."}`, with `act` and `id` if
    /// it pauses, resumes, stops, closes or opens a talk, and `to` for a
    /// carry.
    Steer {
        /// The session's address.
        address: String,
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
        /// Runs each start of it through the person's login shell.
        #[arg(long)]
        login: bool,
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
/// A program to start, as its flags say it.
/// How this machine's node runs: what it links with, and what it serves.
#[derive(clap::Args)]
struct Node {
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
    /// A machine to keep linked with through a provider's program, as
    /// `PROGRAM:PLACE`, such as `aquifer-link:sandbox-1`; given again
    /// for each. PROTOCOL.md says what the program is asked.
    #[arg(long = "via", value_parser = Program::parse)]
    programs: Vec<Program>,
    /// A provider's program to ask what machines it could make, by its
    /// path or as found on `PATH`; given again for each. PROTOCOL.md says
    /// what it is asked.
    #[arg(long = "provider")]
    providers: Vec<String>,
    /// A JSON file naming files to serve in the daemon interface, each
    /// as a kind of records; PROTOCOL.md says its shape.
    #[arg(long)]
    serves: Option<PathBuf>,
    /// A realm this realm's text may cross into, one way, where a session
    /// names it; given again for each. PROTOCOL.md says what crosses.
    #[arg(long = "flow-to")]
    flows_to: Vec<String>,
    /// A realm whose text this realm takes, one way; given again for each.
    #[arg(long = "flow-from")]
    flows_from: Vec<String>,
}

impl Node {
    /// The ways its links are dialled: over SSH, then through programs.
    fn dials(&self) -> Vec<Way> {
        let over_ssh = self.destinations.iter().map(|destination| Ssh {
            destination: destination.clone(),
            options: self.options.clone(),
            command: self.remote_command.clone(),
        });
        over_ssh
            .map(Way::Ssh)
            .chain(self.programs.iter().cloned().map(Way::Program))
            .collect()
    }

    /// The kinds it serves from files, as its `--serves` file names them.
    fn serves(&self) -> io::Result<BTreeMap<String, served::Source>> {
        match &self.serves {
            Some(file) => served::configured(
                file,
                &node::HOLDS,
                &served::Here {
                    machine: place::machine(),
                    runtime: place::login_runtime(),
                },
            )
            .map_err(|why| io::Error::new(io::ErrorKind::InvalidInput, why)),
            None => Ok(BTreeMap::new()),
        }
    }
}

#[derive(clap::Args)]
struct Starting {
    /// What the machine must be, as for `place`.
    #[command(flatten)]
    work: Asked,
    /// The folder it starts in, found at the same place under the home of
    /// the machine it starts on when it is under this one's; if none, or
    /// none there, that machine's home.
    #[arg(long)]
    folder: Option<PathBuf>,
    /// Makes a machine for it from a provider's offer, which may cost
    /// money, where no machine of the realm takes it.
    #[arg(long)]
    may_cost: bool,
    /// The program and its arguments, after `--`.
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

/// Work to be placed, as its flags say it.
#[derive(clap::Args)]
struct Asked {
    /// What a machine must be, as `kind=value`: `machine`, `system`,
    /// `arch`, `reach` or `accelerator`, such as `system=linux`; given
    /// again for each.
    #[arg(long = "need", value_parser = need)]
    needs: Vec<Need>,
    /// What a machine had rather be, in the same words, most wanted first.
    #[arg(long = "prefer", value_parser = need)]
    prefers: Vec<Need>,
    /// A taint the work doesn't mind, by its key, such as `on-battery`.
    #[arg(long = "tolerate")]
    tolerates: Vec<String>,
    /// Waits for a machine out of reach rather than being refused.
    #[arg(long)]
    may_wait: bool,
    /// How long the work runs, such as `90m` or `2h`, so it goes nowhere
    /// that leaves before then.
    #[arg(long)]
    lasts: Option<jiff::SignedDuration>,
}

impl From<Asked> for Work {
    fn from(asked: Asked) -> Self {
        Work {
            needs: asked.needs,
            prefers: asked.prefers,
            tolerates: asked.tolerates,
            may_wait: asked.may_wait,
            lasts: asked.lasts,
        }
    }
}

#[derive(clap::Args)]
struct Here {
    /// The harness mesh's folders, if not where the harness keeps them.
    #[arg(long)]
    mesh: Option<PathBuf>,
    /// The sessions' steer inboxes, if not where the harness keeps them.
    #[arg(long)]
    steer: Option<PathBuf>,
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
    let realm = realm_named(realm)?;
    let socket = command.socket.unwrap_or_else(|| place::socket(&realm));
    match command.action {
        Action::Node(node) => run_node(node, realm, &socket),
        Action::Link { here } => run_link(here, realm, socket, None),
        Action::Ssh { here, from } => run_asked(here, realm, socket, from),
        Action::Share {
            here,
            quest,
            scratch,
        } => {
            let (_, _, shares) = open(here, realm)?;
            println!("{}", shares.offer(&quest, &scratch)?);
            Ok(())
        }
        Action::Carry { here, quest, to } => {
            let (_, _, shares) = open(here, realm)?;
            carry(&socket, shares, &quest, &to)
        }
        Action::Sessions => print_reply(&socket, &Request::Sessions),
        Action::Machine => print_reply(&socket, &Request::Machine),
        Action::Machines => print_reply(&socket, &Request::Machines),
        Action::Offers => print_reply(&socket, &Request::Offers),
        Action::Place(asked) => print_reply(&socket, &Request::Place(asked.into())),
        Action::Update { machine } => print_reply(&socket, &Request::Update { machine }),
        Action::Start(asked) => start(&socket, asked),
        Action::Steer { address } => {
            let steer = serde_json::from_reader(io::stdin().lock())
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            print_reply(&socket, &Request::Steer { address, steer })
        }
        Action::Messages { groups, limit } => {
            print_reply(&socket, &Request::Messages { groups, limit })
        }
        Action::Send {
            from,
            intent,
            groups,
            text,
            to_realm,
        } => print_answer(
            &socket,
            &Request::Send(Draft {
                from,
                groups,
                intent,
                text,
                re: None,
                realm: to_realm,
            }),
        ),
        Action::Flows => print_reply(&socket, &Request::Flows),
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
            login,
            command,
        } => {
            // A hold from a terminal has the person's environment already.
            let through = if login {
                login_shell(&realm)
            } else {
                Vec::new()
            };
            be_holder(&hold::holder::Holding {
                dir: &place::held(&realm),
                realm: &realm,
                id: &id,
                command: &command,
                through: &through,
                folder: &folder,
            })
        }
        Action::Watch { groups, recent } => {
            let mut client = Client::connect(&socket)?;
            print_line(&client.ask(&Request::Watch {
                groups: Some(groups),
                recent,
            })?)?;
            while let Some(event) = client.next_event()? {
                print_line(&event)?;
            }
            Ok(())
        }
    }
}

/// `realm`, once it reads as one. A realm names a socket and goes into the
/// command run over SSH, so it is held to letters, digits, `-` and `_`.
fn realm_named(realm: String) -> io::Result<String> {
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
    Ok(realm)
}

/// The realms a flow names, each held to a realm's name.
fn checked(realms: Vec<String>) -> io::Result<BTreeSet<String>> {
    realms.into_iter().map(realm_named).collect()
}

/// Runs this machine's node on `socket`, as `node` says, until it is asked
/// to stop.
fn run_node(node: Node, realm: String, socket: &Path) -> io::Result<()> {
    let (dials, serves) = (node.dials(), node.serves()?);
    let providers = node.providers;
    let here = node.here;
    let state = here.state.clone().unwrap_or_else(place::state);
    let (refused, book) = (state.join("refused"), state.join("made.jsonl"));
    let steers = here.steer.clone().unwrap_or_else(place::steer);
    let flows = Flows {
        to: checked(node.flows_to)?,
        from: checked(node.flows_from)?,
        sockets: place::runtime(),
        log: state.join("crossings.jsonl"),
    };
    let (store, membership, shares) = open(here, realm)?;
    let given = node::Given {
        sharing: Some(shares),
        refusals: Refusals::kept_in(refused),
        steers,
        starter: Some(Starter {
            program: std::env::current_exe()?,
            // A node that is a systemd unit, as on Linux, would stop
            // every session it started when the unit stops.
            scoped: cfg!(target_os = "linux") && std::env::var_os("INVOCATION_ID").is_some(),
        }),
        awake: Awake::here(),
        updater: place::updater(),
        serves,
        providers,
        book: mycelium::made::Book::at(book),
        flows,
    };
    runtime()?.block_on(async {
        let bound = node::bind(socket)?;
        // Probing ends with the runtime, when the node does.
        tokio::spawn(mycelium::reach::keep(place::declared(), place::reached()));
        node::run(store, membership, Listed(dials), given, bound, stopped()).await
    })
}

/// Runs this machine's end of a link over this process's own input and
/// output, as SSH carries it, handing the starts the other end asks of
/// this machine to its node at `socket`, which runs in its person's login
/// session, and writing the steers it carries into this machine's inboxes.
/// It carries what that node asks of the other machine too, asked of the
/// node over its socket. A link let in by a machine's key admits only that
/// machine, `far`.
fn run_link(here: Here, realm: String, socket: PathBuf, far: Option<String>) -> io::Result<()> {
    let held = place::held(&realm);
    let steers = here.steer.clone().unwrap_or_else(place::steer);
    let (store, membership, shares) = open(here, realm)?;
    let starting = link::starts::Starting {
        node: socket,
        held,
        asking: Some(link::starts::Asking::Dialled),
        steers,
    };
    match far {
        Some(far) => {
            let proven = Proven {
                by: membership,
                far,
            };
            serve_link(store, &proven, shares, starting)
        }
        None => serve_link(store, &membership, shares, starting),
    }
}

/// Serves this end of a link over this process's input and output, as
/// `membership` admits, until it ends or a newer build is installed.
fn serve_link(
    store: Folders,
    membership: &impl Membership,
    shares: Places,
    starting: link::starts::Starting,
) -> io::Result<()> {
    let running = install::running();
    runtime()?.block_on(async {
        let connection = Connection {
            reading: Box::new(tokio::io::stdin()),
            writing: Box::new(tokio::io::stdout()),
            keep: Box::new(()),
            refused: None,
            releases: false,
            hello_within: Some(link::dial::HELLO_WITHIN),
        };
        // A build installed since this end began ends it, for its dialler
        // to start the new one.
        tokio::select! {
            ended = link::run(store, membership, connection, Some(shares), Some(starting)) => {
                ended.map(|_| ())
            }
            newer = install::replaced(running) => {
                eprintln!("{} is installed now, so this link ends for it", newer.display());
                Ok(())
            }
        }
    })
}

/// Runs what `from`'s link key asked, as `SSH_ORIGINAL_COMMAND` says, or
/// refuses it, keeping the refusal.
fn run_asked(here: Here, realm: String, socket: PathBuf, from: String) -> io::Result<()> {
    let original = std::env::var("SSH_ORIGINAL_COMMAND").ok();
    match mycelium::ssh::asked(original.as_deref(), &realm) {
        Ok(KeyAsked::Link) => run_link(here, realm, socket, Some(from)),
        Ok(KeyAsked::Attach(id)) => attach(&place::held(&realm), &id),
        Err(why) => {
            let state = here.state.unwrap_or_else(place::state);
            keep_refused(&state, &from, original.as_deref(), &why);
            Err(io::Error::new(io::ErrorKind::PermissionDenied, why))
        }
    }
}

/// Keeps, in `state`'s `refused-keys.jsonl`, that `from`'s key asked
/// `original` and was refused for `why`, a JSON line each.
fn keep_refused(state: &Path, from: &str, original: Option<&str>, why: &str) {
    let line = serde_json::json!({
        "at": jiff::Timestamp::now().to_string(),
        "from": from,
        "asked": original,
        "why": why,
    });
    let kept = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.join("refused-keys.jsonl"))
        .and_then(|mut file| writeln!(file, "{line}"));
    if let Err(error) = kept {
        eprintln!("mycelium: the refusal wasn't kept: {error}");
    }
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

/// Carries `quest` to the machine `to`, a JSON line on standard output
/// for each step as it goes and the last saying where each session went.
fn carry(socket: &Path, shares: Places, quest: &str, to: &str) -> io::Result<()> {
    let mut hands = Through::new(socket.to_owned(), shares, quest, carry::through::PATIENCE);
    let mut failed = None;
    let carried = carry::carry(&mut hands, quest, &place::machine(), to, &mut |line| {
        if let Err(error) = print_line(line) {
            failed.get_or_insert(error);
        }
    });
    if let Some(error) = failed {
        return Err(error);
    }
    match carried {
        Ok(carried) => print_line(&carried),
        Err(failed) => Err(io::Error::other(format!(
            "{quest} wasn't carried to {to}: its {:?} failed, as it said",
            failed.step
        ))),
    }
}

/// Asks the node to start `command` where placement says `work` should
/// go, in `folder` as it stands under home there, or in home.
fn start(socket: &Path, asked: Starting) -> io::Result<()> {
    let folder = asked.folder.map(std::path::absolute).transpose()?;
    // A folder under home is said within it, since another machine's
    // home is elsewhere: /home/joel here is /Users/joel on a Mac.
    let folder = match (folder, std::env::home_dir()) {
        (Some(folder), Some(home)) => match folder.strip_prefix(&home) {
            Ok(within) => Some(within.to_owned()),
            Err(_) => Some(folder),
        },
        (folder, _) => folder,
    };
    let start = Start {
        work: asked.work.into(),
        folder,
        command: asked.command,
        may_cost: asked.may_cost,
    };
    print_reply(socket, &Request::Start(start))
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
    let starter = Starter {
        program: std::env::current_exe()?,
        scoped: false,
    };
    let id = starter.start(realm, &folder, command, false)?;
    if detached {
        println!("{id}");
        Ok(())
    } else {
        attach(&place::held(realm), &id)
    }
}

/// Is the holder `hold` starts: in a session of its own, so the window it
/// was started from never hangs up on it, saying `held` once a terminal
/// can join, or why it can't.
fn be_holder(holding: &hold::holder::Holding<'_>) -> io::Result<()> {
    rustix::process::setsid()?;
    let held = runtime()?.block_on(hold::holder::hold(holding, |_| {
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

/// Prints the node's answer to a request a flow may refuse.
fn print_answer(socket: &Path, request: &Request) -> io::Result<()> {
    print_line(&Client::connect(socket)?.answer(request)?)
}

fn print_line(value: &impl serde::Serialize) -> io::Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, value).map_err(io::Error::other)?;
    writeln!(out)
}

fn need(said: &str) -> Result<Need, String> {
    let (kind, value) = said
        .split_once('=')
        .ok_or_else(|| format!("{said:?} is not kind=value, such as system=linux"))?;
    serde_json::from_value(serde_json::json!({ kind: value })).map_err(|_| {
        format!("{said:?} is not a need: machine, system, arch, reach or accelerator (gpu, npu)")
    })
}

fn intent(name: &str) -> Result<Intent, String> {
    serde_json::from_value(serde_json::Value::String(name.to_owned()))
        .map_err(|_| format!("{name:?} is not hold, question, finding, fyi or pivot"))
}
