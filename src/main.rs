//! The `mycelium` command: the node, the far end of a link, and a few
//! requests of a running node's socket for looking into it by hand. Pi and
//! Quickshell read the socket themselves; nothing here is how a person uses
//! the network.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

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
use mycelium::{link, node, place};

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
    /// kept the same on each, written on any. A machine that held its own
    /// copy moves it aside first.
    Share {
        #[command(flatten)]
        here: Here,
        /// The quest's id, such as `QEST-20261013-4UHC5Q`.
        quest: String,
    },
    /// Prints every session the node knows of, a JSON line each.
    Sessions,
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
            let (store, membership, shares) = open(here, realm)?;
            let dials = destinations
                .into_iter()
                .map(|destination| Ssh {
                    destination,
                    options: options.clone(),
                    command: remote_command.clone(),
                })
                .collect();
            runtime()?.block_on(async {
                let bound = node::bind(&socket)?;
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
        Action::Share { here, quest } => {
            let (_, _, shares) = open(here, realm)?;
            share(&shares, &quest)
        }
        Action::Sessions => print_reply(&socket, &Request::Sessions),
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

/// Shares `quest`, which must be here to share.
fn share(shares: &Places, quest: &str) -> io::Result<()> {
    let record = shares.quest(quest).record;
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
    println!("{quest} is shared; each link carries it from its next look");
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

fn print_reply(socket: &std::path::Path, request: &Request) -> io::Result<()> {
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
