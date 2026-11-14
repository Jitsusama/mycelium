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
use mycelium::hold::environment::ServiceManager;
use mycelium::hold::journal::SystemJournal;
use mycelium::hold::start::{Scope, Starter, login_shell};
use mycelium::link::dial::{Connection, Program, Ssh, Way};
use mycelium::link::discovery::Listed;
use mycelium::link::membership::{Membership, Proven, Realm};
use mycelium::link::refused::Refusals;
use mycelium::placement::{Need, Work};
use mycelium::record::Intent;
use mycelium::share::exchange::Places;
use mycelium::ssh::Asked as KeyAsked;
use mycelium::store::Store as _;
use mycelium::store::folders::Folders;
use mycelium::web::Said;
use mycelium::wire::{Draft, Reply, Request, Response, Start};
use mycelium::{carry, hold, hosted_attach, install, link, node, place, served};

/// The widest a line of any command's help is drawn, so it reads in a
/// narrow pane.
const HELP_WIDTH: usize = 60;

#[derive(Parser)]
#[command(
    version,
    about = "A nimble core for agent networks",
    max_term_width = HELP_WIDTH
)]
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
    /// Shows the swarm: every team, nested as they are, with how many of
    /// its sessions need you, are stuck, move or wait, those needing you
    /// first, each by its short name; or, named by that or any part of its
    /// title, one team's sessions and where it meets the others. Its acts
    /// steer the swarm as its person: `say` words to a team, which its
    /// lead hears, or to one session, read once its turn ends, or into
    /// that turn when the words begin `now`; `pause` every session of a team and
    /// the teams within it, or one session, so each finishes its turn and
    /// waits; and `resume` them.
    Swarm {
        /// The team, by its short name or any words of its title, such as
        /// `part deux`, a leading `team` left out; or an act and its team,
        /// as `say harbour ship it`, `say harbour now stop that`,
        /// `pause harbour`, or the team's words first, as
        /// `part deux say ship it`.
        words: Vec<String>,
        #[command(flatten)]
        output: Output,
        /// Prints each team as it's typed, a line each, for a shell to
        /// complete.
        #[arg(long, hide = true)]
        names: bool,
    },
    /// Draws the networks as a tree, or one network.
    ///
    /// Draws every network as a tree under its shape:
    /// orchestrations, projects, repositories, groups
    /// and machines, each with how its sessions stand.
    /// Named by its short name, its name or words of its
    /// title, one network is drawn with its cells.
    /// `network` is the same.
    #[command(alias = "network", verbatim_doc_comment)]
    Networks(NetworksAsked),
    /// Lists the cells, or says what one is.
    ///
    /// Lists every cell: the sessions, worst first, then
    /// the machines. Named by its name or words of its
    /// title, a session is said with the networks it
    /// joined, and a machine as `machines NAME` says it.
    /// `cell` is the same.
    #[command(alias = "cell", verbatim_doc_comment)]
    Cells(CellsAsked),
    /// Prints how the realm's network stands, as one JSON line for a daily
    /// read: each link, `up` or `unreachable`, with how many times it
    /// dropped in the last day; each machine's sessions by how they stand;
    /// how many presences still say they run with their process gone; and
    /// the node's slowest answer to this read, in whole seconds. Exits 0
    /// whenever the node could be read, and within 10 seconds.
    Health {
        #[command(flatten)]
        here: Here,
    },
    /// Speaks in a quest's web, the murmurs and ties its sessions on every
    /// machine share: says into it, or folds it into what it says now.
    Web(WebAsked),
    /// Prints what every machine of the realm is, this one first;
    /// or, given its name, what one machine is. `machine` is the same,
    /// and bare prints what this machine is.
    Machines {
        /// The machine, such as `penelope`.
        name: Option<String>,
    },
    /// `machines`, as it was spelt before, when bare it printed this
    /// machine.
    #[command(hide = true)]
    Machine {
        /// The machine, if not this one.
        name: Option<String>,
    },
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
    #[command(
        override_usage = "mycelium send [OPTIONS] --from <FROM> --text <TEXT>\n       <GROUPS>..."
    )]
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
    /// Prints every session held on this machine, a JSON line each, or
    /// the one named. `holds add -- PROGRAM` starts a program on a
    /// terminal a holder of its own keeps, so its window can close and
    /// `holds ID attach` take it up again from any terminal, and joins
    /// it unless `--detached`. `holds ID again -- PROGRAM` names the
    /// command it starts with when it next starts again, over any its
    /// program names as it parks, and wakes it if it is parked, printing
    /// `{"id", "woke"}`; a pi held on an old conversation is moved onto
    /// a newer one this way. `hold` is the same.
    #[command(alias = "hold")]
    Holds(Holds),
    /// Joins a conversation a pi host holds, with `--host` and
    /// `--session`; closing it only leaves. It exits 3 when the host
    /// lets the conversation go back to plain pi, as a release does, and
    /// 0 on any other leaving. A held session is joined with `holds ID
    /// attach`, which `attach ID` still spells.
    Attach {
        /// A held session's id, as `holds` lists it.
        #[arg(required_unless_present = "host", conflicts_with = "host")]
        id: Option<String>,
        /// The pi host holding the conversation, by its key, such as
        /// `work-attended`.
        #[arg(long, requires = "session")]
        host: Option<String>,
        /// The conversation's pi session id within that host.
        #[arg(long, requires = "host")]
        session: Option<String>,
    },
    /// `holds`, as it was spelt before.
    #[command(hide = true)]
    Held,
    /// `holds ID again`, as it was spelt before.
    #[command(hide = true)]
    Again {
        /// Its id, as `holds` lists it.
        id: String,
        /// The program and its arguments, after `--`.
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Holds a program, as `holds add` starts it, in a process of its own.
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

/// How what is asked is printed.
#[derive(clap::Args)]
struct Output {
    /// How to print it: text, or JSON lines
    #[arg(short = 'o', long = "output", value_enum, default_value_t = Format::Text)]
    format: Format,
    /// `-o json`, as it was spelt before.
    #[arg(long, hide = true)]
    json: bool,
}

impl Output {
    /// Whether it is printed as JSON lines.
    fn json(&self) -> bool {
        self.json || self.format == Format::Json
    }
}

/// A way of printing what is asked.
#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Format {
    /// For a person, drawn to the terminal's width.
    Text,
    /// For a program, a JSON line each.
    Json,
}

/// What is asked of the networks, by the words after `networks`.
#[derive(clap::Args)]
struct NetworksAsked {
    /// The network, such as `grow` or `world`
    words: Vec<String>,
    /// Puts each network's cells under it.
    #[arg(long)]
    cells: bool,
    #[command(flatten)]
    output: Output,
}

/// What is asked of the cells, by the words after `cells`.
#[derive(clap::Args)]
struct CellsAsked {
    /// The cell, such as `QQ1UAK` or `optimus`
    words: Vec<String>,
    #[command(flatten)]
    output: Output,
}

/// What is asked of the sessions held on this machine, by the words after
/// `holds`.
#[derive(clap::Args)]
struct Holds {
    /// A held session's id, as bare `holds` lists them; or `add`, to
    /// hold the program after `--`.
    id: Option<String>,
    /// What is done to it: `attach` joins it from this terminal, and
    /// `again` names the program after `--` as its next start.
    #[arg(requires = "id")]
    act: Option<HoldAct>,
    /// For `add`: the folder it starts in, if not this one.
    #[arg(long)]
    folder: Option<PathBuf>,
    /// For `add`: prints its id and leaves it running, unjoined.
    #[arg(long)]
    detached: bool,
    /// For `add` and `again`: the program and its arguments, after `--`.
    #[arg(last = true)]
    command: Vec<String>,
}

/// What is done to one held session.
#[derive(Clone, Copy, clap::ValueEnum)]
enum HoldAct {
    /// Joins it from this terminal; closing it only leaves.
    Attach,
    /// Names the program its next start runs.
    Again,
}

/// What a line asks of the machines or of what is held here, read the same
/// whichever spelling it was typed in.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tree {
    /// Every machine of the realm, this one first.
    Machines,
    /// This machine, as bare `machine` asks.
    ThisMachine,
    /// The machine of this name.
    Machine(String),
    /// Every session held here.
    Holds,
    /// The session held here by this id.
    Held(String),
    /// A program held, started in `folder` and joined unless `detached`.
    Hold {
        folder: Option<PathBuf>,
        detached: bool,
        command: Vec<String>,
    },
    /// The session held by this id, joined from this terminal.
    Attach(String),
    /// The program the session held as `id` runs at its next start.
    Again { id: String, command: Vec<String> },
}

/// What `action` asks of the machines or the holds, its old spellings
/// read as their new ones, or why its words don't fit; none for any other
/// action, and for an attach to a pi host's conversation.
fn tree(action: &Action) -> Option<Result<Tree, String>> {
    Some(Ok(match action {
        Action::Machines { name: None } => Tree::Machines,
        Action::Machine { name: None } => Tree::ThisMachine,
        Action::Machines { name: Some(name) } | Action::Machine { name: Some(name) } => {
            Tree::Machine(name.clone())
        }
        Action::Holds(holds) => return Some(holds_asked(holds)),
        Action::Held => Tree::Holds,
        Action::Attach { id: Some(id), .. } => Tree::Attach(id.clone()),
        Action::Again { id, command } => Tree::Again {
            id: id.clone(),
            command: command.clone(),
        },
        _ => return None,
    }))
}

/// What the words after `holds` ask, or why they don't fit.
fn holds_asked(holds: &Holds) -> Result<Tree, String> {
    let Holds {
        id,
        act,
        folder,
        detached,
        command,
    } = holds;
    let adding = match (id.as_deref(), act) {
        (Some("add"), None) => true,
        // `hold -- PROGRAM`, the old spelling, names no `add`.
        (None, None) => !command.is_empty(),
        _ => false,
    };
    if adding {
        if command.is_empty() {
            return Err("add what? name the program after --".to_owned());
        }
        return Ok(Tree::Hold {
            folder: folder.clone(),
            detached: *detached,
            command: command.clone(),
        });
    }
    if folder.is_some() || *detached {
        return Err("--folder and --detached are for holds add".to_owned());
    }
    match (id, act) {
        (Some(id), Some(HoldAct::Again)) if command.is_empty() => {
            Err(format!("{id} again as what? name the program after --"))
        }
        (Some(id), Some(HoldAct::Again)) => Ok(Tree::Again {
            id: id.clone(),
            command: command.clone(),
        }),
        _ if !command.is_empty() => {
            Err("a program after -- is for holds add and holds ID again".to_owned())
        }
        (Some(id), Some(HoldAct::Attach)) => Ok(Tree::Attach(id.clone())),
        (Some(id), None) => Ok(Tree::Held(id.clone())),
        (None, _) => Ok(Tree::Holds),
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
struct WebAsked {
    /// The quest whose web it is, such as `QEST-20261013-4UHC5Q`.
    quest: String,
    /// The quests' scratch folders, if not where the harness keeps them.
    #[arg(long)]
    scratch: Option<PathBuf>,
    /// This machine's name, which writes its own file, if not the system's.
    #[arg(long)]
    machine: Option<String>,
    #[command(subcommand)]
    act: WebAct,
}

/// What a person asks of the swarm, in the words they typed after it.
#[derive(Debug, PartialEq, Eq)]
enum SwarmAsked {
    /// Every team, or the one named.
    Show(Option<String>),
    /// Words said to a team or a session, as the person: after the turn
    /// under way, or `now`, into it.
    Say {
        name: String,
        words: String,
        now: bool,
    },
    /// A team's sessions, or one session, paused.
    Pause(String),
    /// A team's paused sessions, or one session, resumed.
    Resume(String),
}

/// The swarm's acts, in the words that ask for them.
const SWARM_ACTS: [&str; 3] = ["say", "pause", "resume"];

/// What `words` ask of the swarm. Every word that isn't an act names the
/// team, so a name of several words needs no quotes, and a leading `team`
/// is the person saying what follows, not a name. An act first names its
/// team in the one word after it; the team's words first name it whole.
fn swarm_asked(words: &[String]) -> Result<SwarmAsked, String> {
    let words = match words.split_first() {
        Some((first, rest)) if first.eq_ignore_ascii_case("team") => rest,
        _ => words,
    };
    let act = |word: &String| SWARM_ACTS.contains(&word.to_lowercase().as_str());
    let Some(at) = words.iter().position(act) else {
        return Ok(SwarmAsked::Show(
            (!words.is_empty()).then(|| words.join(" ")),
        ));
    };
    let (name, rest) = if at == 0 {
        let Some((name, rest)) = words[1..].split_first() else {
            return Err(format!("{} which team?", words[0]));
        };
        (name.clone(), rest)
    } else {
        (words[..at].join(" "), &words[at + 1..])
    };
    match (words[at].to_lowercase().as_str(), rest) {
        ("say", []) => Err(format!("say what to {name}?")),
        // A first word `now` sends the rest into the turn under way.
        ("say", [first, said @ ..]) if first.eq_ignore_ascii_case("now") => {
            if said.is_empty() {
                Err(format!("say what to {name} now?"))
            } else {
                Ok(SwarmAsked::Say {
                    name,
                    words: said.join(" "),
                    now: true,
                })
            }
        }
        ("say", said) => Ok(SwarmAsked::Say {
            name,
            words: said.join(" "),
            now: false,
        }),
        ("pause", []) => Ok(SwarmAsked::Pause(name)),
        ("resume", []) => Ok(SwarmAsked::Resume(name)),
        (act, more) => Err(format!("{act} takes only a team, not {}", more.join(" "))),
    }
}

#[derive(Subcommand)]
enum WebAct {
    /// Says a murmur, a line of intent for whoever heeds it, and prints
    /// the event, whose id ties and settles it.
    Say {
        /// Its words.
        #[arg(required = true)]
        words: Vec<String>,
        /// Who says it, such as a session's address.
        #[arg(long)]
        by: Option<String>,
    },
    /// Ties one thing to another, a murmur's id, a quest's or a change's,
    /// and prints the event.
    Tie {
        /// What it ties.
        from: String,
        /// What it's tied to.
        to: String,
        /// How, such as `about`.
        #[arg(long)]
        how: Option<String>,
        /// Who ties it.
        #[arg(long)]
        by: Option<String>,
    },
    /// Settles a murmur, saying how, and prints the event.
    Settle {
        /// The murmur's id.
        of: String,
        /// How, such as `filed` or `let go`.
        how: String,
        /// Who settles it.
        #[arg(long)]
        by: Option<String>,
    },
    /// Folds every machine's events into a file written whole, as a page
    /// reads it, and prints whether it wrote; with `--every`, again each
    /// so many seconds until stopped, writing only when it says something
    /// new.
    Fold {
        /// The file it's written to.
        #[arg(long)]
        into: PathBuf,
        /// Seconds between folds, if it keeps folding.
        #[arg(long)]
        every: Option<u64>,
    },
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
        Ok(code) => code,
        Err(error) => {
            eprintln!("mycelium: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> io::Result<ExitCode> {
    let realm = command.realm.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "say which realm, with --realm or MYCELIUM_REALM",
        )
    })?;
    let realm = realm_named(realm)?;
    let socket = command.socket.unwrap_or_else(|| place::socket(&realm));
    if let Some(asked) = tree(&command.action) {
        let asked = asked.map_err(|why| io::Error::new(io::ErrorKind::InvalidInput, why))?;
        return in_tree(&socket, &realm, asked);
    }
    let ran = match command.action {
        Action::Node(node) => run_node(node, realm, &socket),
        Action::Link { here } => run_link(here, realm, socket, None),
        Action::Ssh { here, from } => return run_asked(here, realm, socket, from),
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
        Action::Health { here } => health(&socket, here, realm),
        Action::Swarm {
            words,
            output,
            names,
        } => in_swarm(&socket, &realm, &words, output.json(), names),
        Action::Networks(asked) => networks(&socket, &realm, &asked),
        Action::Cells(asked) => cells(&socket, &realm, &asked),
        Action::Web(asked) => web(asked),
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
        Action::Attach {
            host: Some(host),
            session: Some(session),
            ..
        } => return attach_hosted(&realm, &host, &session),
        Action::Holder {
            id,
            folder,
            login,
            command,
        } => hold_here(&realm, &id, &folder, login, &command),
        Action::Watch { groups, recent } => watch(&socket, groups, recent),
        Action::Machines { .. }
        | Action::Machine { .. }
        | Action::Holds(_)
        | Action::Held
        | Action::Attach { .. }
        | Action::Again { .. } => unreachable!("each is read as its tree above"),
    };
    ran.map(|()| ExitCode::SUCCESS)
}

/// Does what `asked` asks of the machines, through the node at `socket`,
/// or of the sessions held here in `realm`.
fn in_tree(socket: &Path, realm: &str, asked: Tree) -> io::Result<ExitCode> {
    let held = place::held(realm);
    match asked {
        Tree::Machines => print_reply(socket, &Request::Machines),
        Tree::ThisMachine => print_reply(socket, &Request::Machine),
        Tree::Machine(name) => print_line(&one_machine(
            Client::connect(socket)?.ask(&Request::Machines)?,
            &name,
        )?),
        Tree::Holds => hold::held(&held).iter().try_for_each(print_line),
        Tree::Held(id) => match hold::held(&held).into_iter().find(|one| one.id == id) {
            Some(one) => print_line(&one),
            None => Err(not_held(&id)),
        },
        Tree::Hold {
            folder,
            detached,
            command,
        } => hold(realm, folder, detached, &command),
        Tree::Attach(id) => attach(&held, &id),
        Tree::Again { id, command } => hold::name_next(&held, &id, &command)
            .and_then(|woke| print_line(&serde_json::json!({"id": id, "woke": woke}))),
    }
    .map(|()| ExitCode::SUCCESS)
}

/// The node's answer for every machine, `reply`, cut to the one `name`d,
/// in the answer `machine` gives for this one.
fn one_machine(reply: Reply, name: &str) -> io::Result<Reply> {
    match reply {
        Ok(Response::Machines(machines)) => machines
            .into_iter()
            .find(|machine| machine.name == name)
            .map(|machine| Ok(Response::Machine(Box::new(machine))))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no machine of the realm is {name}; `mycelium machines` lists them"),
                )
            }),
        Ok(_) => Err(io::Error::other(
            "the node answered machines with something else",
        )),
        Err(why) => Ok(Err(why)),
    }
}

/// That nothing is held as `id`, and where to look for what is.
fn not_held(id: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("nothing is held as {id}; `mycelium holds` lists what is"),
    )
}

/// Prints what the node says of `groups` as it says it, after the
/// `recent` it already holds, until the node goes.
fn watch(socket: &Path, groups: Pattern, recent: usize) -> io::Result<()> {
    let mut client = Client::connect(socket)?;
    print_line(&client.ask(&Request::Watch {
        groups: Some(groups),
        recent,
    })?)?;
    while let Some(event) = client.next_event()? {
        print_line(&event)?;
    }
    Ok(())
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
            scope: if cfg!(target_os = "linux") && std::env::var_os("INVOCATION_ID").is_some() {
                Scope::Apart
            } else {
                Scope::None
            },
        }),
        awake: Awake::here(),
        updater: place::updater(),
        serves,
        providers,
        book: mycelium::made::Book::at(book),
        flows,
        parked: place::parked(),
        processes: node::Processes::here(),
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
fn run_asked(here: Here, realm: String, socket: PathBuf, from: String) -> io::Result<ExitCode> {
    let original = std::env::var("SSH_ORIGINAL_COMMAND").ok();
    match mycelium::ssh::asked(original.as_deref(), &realm) {
        Ok(KeyAsked::Link) => run_link(here, realm, socket, Some(from)).map(|()| ExitCode::SUCCESS),
        Ok(KeyAsked::Attach(id)) => attach(&place::held(&realm), &id).map(|()| ExitCode::SUCCESS),
        Ok(KeyAsked::AttachHosted { host, session }) => attach_hosted(&realm, &host, &session),
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

/// How long `health` waits on each of the node's answers, so its two asks
/// and its read of the store end inside the 10 seconds a daily read allows.
const HEALTH_WITHIN: std::time::Duration = std::time::Duration::from_secs(4);

/// Prints how the realm's network stands, as [`mycelium::health`] says.
fn health(socket: &Path, here: Here, realm: String) -> io::Result<()> {
    let (store, membership, _) = open(here, realm)?;
    let mut client = Client::connect_within(socket, HEALTH_WITHIN)?;
    let mut slowest = std::time::Duration::ZERO;
    let mut ask = |request: &Request| -> io::Result<Response> {
        let begun = std::time::Instant::now();
        let reply = client.ask(request)?;
        slowest = slowest.max(begun.elapsed());
        reply.map_err(io::Error::other)
    };
    let Response::Machines(machines) = ask(&Request::Machines)? else {
        return Err(io::Error::other(
            "the node answered machines with something else",
        ));
    };
    let Response::Sessions(sessions) = ask(&Request::Sessions)? else {
        return Err(io::Error::other(
            "the node answered sessions with something else",
        ));
    };
    let now = jiff::Timestamp::now();
    let day_ago = now.as_millisecond() - 24 * 60 * 60 * 1000;
    let read = mycelium::health::Read {
        realm: &membership.name,
        at: now.as_second(),
        here: &membership.machine,
        machines: &machines,
        sessions: &sessions,
        slowest,
    };
    print_line(&mycelium::health::line(
        &read,
        |machine| store.endings(machine, day_ago),
        |machine| store.link_refused(machine),
    ))
}

/// How long `swarm` waits on each of the node's answers, as `health` does.
const SWARM_WITHIN: std::time::Duration = std::time::Duration::from_secs(4);

/// How long a steer of the swarm waits on the node's answer: past the 30
/// seconds a link gives another machine to say it was told.
const STEER_WITHIN: std::time::Duration = std::time::Duration::from_secs(35);

/// How wide the swarm is drawn where its output isn't a terminal that says.
const SWARM_WIDTH: usize = 100;

/// Prints the swarm's teams, or the one `team` names, as
/// [`mycelium::swarm`] draws them, or each team's record as a JSON line.
fn swarm(socket: &Path, realm: &str, team: Option<&str>, json: bool) -> io::Result<()> {
    let swarm = mycelium::swarm::read(socket, realm, SWARM_WITHIN)?;
    if json {
        return swarm.teams.iter().try_for_each(print_line);
    }
    let look = look();
    let shown = match team {
        Some(team) => mycelium::swarm::team(&swarm, team, &look).map_err(not_found)?,
        None => mycelium::swarm::overview(&swarm, &look),
    };
    writeln!(io::stdout().lock(), "{shown}")
}

/// How a person's terminal is drawn on: as wide as it says, in colour
/// unless it isn't one or `NO_COLOR` is set.
fn look() -> mycelium::swarm::Look {
    use std::io::IsTerminal as _;
    let stdout = io::stdout();
    let terminal = stdout.is_terminal();
    let width = rustix::termios::tcgetwinsize(&stdout)
        .ok()
        .filter(|size| terminal && size.ws_col > 0)
        .map_or(SWARM_WIDTH, |size| usize::from(size.ws_col));
    mycelium::swarm::Look {
        width,
        colour: terminal && std::env::var_os("NO_COLOR").is_none(),
        now: jiff::Zoned::now(),
    }
}

/// That nothing, or more than one thing, goes by what was asked, as `why`
/// says.
fn not_found(why: String) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, why)
}

/// Draws every network, or the one `asked` names, as
/// [`mycelium::networks`] draws them, or prints each as a JSON line, the
/// cells after them.
fn networks(socket: &Path, realm: &str, asked: &NetworksAsked) -> io::Result<()> {
    use mycelium::networks;
    let mycelium = networks::read(socket, realm, SWARM_WITHIN)?;
    let name = asked.words.join(" ");
    if asked.output.json() {
        if name.is_empty() {
            return mycelium.lines().iter().try_for_each(print_line);
        }
        let network = mycelium.named_network(&name).map_err(not_found)?;
        print_line(network)?;
        let cells = network.cells.iter().filter_map(|cell| mycelium.cell(cell));
        return cells.into_iter().try_for_each(print_line);
    }
    let look = look();
    let shown = if name.is_empty() {
        networks::overview(&mycelium, &look, asked.cells)
    } else {
        networks::network(&mycelium, &name, &look).map_err(not_found)?
    };
    writeln!(io::stdout().lock(), "{shown}")
}

/// Lists every cell, or says the one `asked` names, a machine as
/// `machines NAME` says it, or prints each as a JSON line.
fn cells(socket: &Path, realm: &str, asked: &CellsAsked) -> io::Result<()> {
    use mycelium::networks::{self, Is};
    let mycelium = networks::read(socket, realm, SWARM_WITHIN)?;
    let name = asked.words.join(" ");
    if name.is_empty() {
        if asked.output.json() {
            return mycelium.cells.iter().try_for_each(print_line);
        }
        let shown = networks::cells(&mycelium, &look());
        return writeln!(io::stdout().lock(), "{shown}");
    }
    let cell = mycelium.named_cell(&name).map_err(not_found)?;
    if cell.is == Is::Machine {
        let machines = Client::connect(socket)?.ask(&Request::Machines)?;
        return print_line(&one_machine(machines, &cell.name)?);
    }
    if asked.output.json() {
        return print_line(cell);
    }
    let shown = networks::cell(&mycelium, cell, &look());
    writeln!(io::stdout().lock(), "{shown}")
}

/// Does what `words` ask of the swarm: shows it or a team, steers one,
/// or, for a shell to complete, prints each team as it's typed.
fn in_swarm(
    socket: &Path,
    realm: &str,
    words: &[String],
    json: bool,
    names: bool,
) -> io::Result<()> {
    use mycelium::swarm::steers::Act;
    if names {
        let swarm = mycelium::swarm::read(socket, realm, SWARM_WITHIN)?;
        let typed = mycelium::swarm::typed(&swarm);
        let mut out = io::stdout().lock();
        return typed.iter().try_for_each(|name| writeln!(out, "{name}"));
    }
    match swarm_asked(words).map_err(|why| io::Error::new(io::ErrorKind::InvalidInput, why))? {
        SwarmAsked::Show(team) => swarm(socket, realm, team.as_deref(), json),
        SwarmAsked::Say { name, words, now } => {
            steer_swarm(socket, realm, Act::Say { words, now }, &name)
        }
        SwarmAsked::Pause(name) => steer_swarm(socket, realm, Act::Pause, &name),
        SwarmAsked::Resume(name) => steer_swarm(socket, realm, Act::Resume, &name),
    }
}

/// Does `act` to the swarm as its person, saying back who was told.
fn steer_swarm(
    socket: &Path,
    realm: &str,
    act: mycelium::swarm::steers::Act,
    name: &str,
) -> io::Result<()> {
    use mycelium::swarm::steers;
    let swarm = mycelium::swarm::read(socket, realm, SWARM_WITHIN)?;
    let aim = steers::aim(&swarm, act, name)
        .map_err(|why| io::Error::new(io::ErrorKind::NotFound, why))?;
    let at = jiff::Timestamp::now().to_string();
    let answers = steers::send(socket, STEER_WITHIN, &aim, &at)?;
    let said = steers::told(&aim, &answers).map_err(io::Error::other)?;
    writeln!(io::stdout().lock(), "{said}")
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
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let starter = Starter {
        program: std::env::current_exe()?,
        scope: Scope::of_a_terminal(runtime.as_deref()),
    };
    let id = starter.start(realm, &folder, command, false)?;
    if detached {
        println!("{id}");
        Ok(())
    } else {
        attach(&place::held(realm), &id)
    }
}

/// The words a held program starts inside: a login shell, for a hold a
/// node starts, or none for a hold from a terminal, which has the
/// person's environment already.
/// Holds `command` here as the session `id`, in `folder`, until its
/// scope's stop tells it to end.
fn hold_here(
    realm: &str,
    id: &str,
    folder: &Path,
    login: bool,
    command: &[String],
) -> io::Result<()> {
    let through = held_through(login, realm);
    be_holder(&hold::holder::Holding {
        dir: &place::held(realm),
        realm,
        id,
        command,
        through: &through,
        folder,
        environment: &ServiceManager,
        journal: &SystemJournal::here(),
        ending: hold::holder::Ending::Signalled,
    })
}

fn held_through(login: bool, realm: &str) -> Vec<String> {
    if login {
        login_shell(realm)
    } else {
        Vec::new()
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
        return Err(not_held(id));
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

/// Joins `session` through the pi host `host` of `realm` on this machine
/// until the host lets it go, exiting [`hold::holder::RELEASED`] when it
/// lets the conversation go back to plain pi, and [`hold::holder::GONE`]
/// when it stayed gone past its patience for a restart.
fn attach_hosted(realm: &str, host: &str, session: &str) -> io::Result<ExitCode> {
    let runtime_dir = place::login_runtime().ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "this login has no runtime folder")
    })?;
    let socket = hosted_attach::socket(&runtime_dir, realm, host)
        .map_err(|why| io::Error::new(io::ErrorKind::InvalidInput, why))?;
    if !socket.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no pi host {host} is running here"),
        ));
    }
    let runtime = runtime()?;
    let left = runtime.block_on(hosted_attach::join_here(&socket, session));
    // Typing is read on a thread of its own, waiting on a key nobody will
    // press now.
    runtime.shutdown_background();
    let left = left?;
    match &left {
        hosted_attach::Left::Detached(why) => eprintln!("mycelium: {why}"),
        hosted_attach::Left::Released => {
            eprintln!("mycelium: the pi host {host} let the conversation go back to plain pi");
        }
        hosted_attach::Left::Gone => eprintln!("mycelium: the pi host {host} has gone"),
        // Joining here comes back from a suspend or a restart by itself.
        hosted_attach::Left::Suspended | hosted_attach::Left::Restarting => {}
    }
    Ok(ExitCode::from(left.exit_code()))
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

fn web(asked: WebAsked) -> io::Result<()> {
    let scratch = asked
        .scratch
        .unwrap_or_else(|| place::shares(&place::state()).scratch);
    let web = mycelium::web::Web::of(&scratch, &asked.quest)?;
    let writer = asked.machine.unwrap_or_else(place::machine);
    let now = || jiff::Timestamp::now().as_second();
    let (by, said) = match asked.act {
        WebAct::Say { words, by } => (
            by,
            Said::Murmur {
                words: words.join(" "),
            },
        ),
        WebAct::Tie { from, to, how, by } => (by, Said::Tie { from, to, how }),
        WebAct::Settle { of, how, by } => (by, Said::Settle { of, how }),
        WebAct::Fold { into, every } => loop {
            let wrote = web.fold_into(&into, now())?;
            // A fold reads only files, so a loop of them is cheap; one that
            // isn't asked to keep folding folds once.
            let Some(every) = every else {
                return print_line(&serde_json::json!({ "wrote": wrote }));
            };
            // Kept folding, it says only what it wrote, so a quiet web
            // writes nothing to the journal either.
            if wrote {
                print_line(&serde_json::json!({ "wrote": into }))?;
            }
            std::thread::sleep(std::time::Duration::from_secs(every.max(1)));
        },
    };
    print_line(&web.say(&writer, by.as_deref(), said, now())?)
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

#[cfg(test)]
mod tests {
    use clap::{CommandFactory as _, Parser as _};

    use super::{Action, Command, HELP_WIDTH, SwarmAsked, Tree, swarm_asked, tree};

    #[test]
    fn every_command_s_help_fits_sixty_columns() {
        let mut command = Command::command();
        command.build();
        let mut commands = vec![command];
        let mut wide = Vec::new();
        while let Some(mut each) = commands.pop() {
            if each.is_hide_set() {
                continue;
            }
            let name = each.get_name().to_owned();
            for help in [each.render_help(), each.render_long_help()] {
                wide.extend(
                    help.to_string()
                        .lines()
                        .filter(|line| line.chars().count() > HELP_WIDTH)
                        .map(|line| format!("{name}: {line}")),
                );
            }
            commands.extend(each.get_subcommands().cloned());
        }
        assert!(wide.is_empty(), "help passes {HELP_WIDTH}: {wide:#?}");
    }

    #[test]
    fn networks_and_cells_take_their_singular_and_either_json() {
        let parsed = |words: &[&str]| {
            let line = ["mycelium", "--realm", "work"];
            Command::try_parse_from(line.iter().chain(words))
                .unwrap_or_else(|error| panic!("{words:?} parses: {error}"))
                .action
        };
        for words in [&["networks", "-o", "json"][..], &["network", "--json"]] {
            assert!(
                matches!(parsed(words), Action::Networks(asked) if asked.output.json()),
                "{words:?}"
            );
        }
        assert!(
            matches!(
                parsed(&["cell", "part", "deux"]),
                Action::Cells(asked) if asked.words == ["part", "deux"] && !asked.output.json()
            ),
            "a cell by words of its title, drawn for a person"
        );
        assert!(
            matches!(parsed(&["swarm", "-o", "json"]), Action::Swarm { output, .. } if output.json()),
            "the swarm takes -o json beside --json"
        );
    }

    /// What `words` after `mycelium --realm work` ask of the machines or
    /// the holds, or why they don't fit.
    fn asked(words: &str) -> Result<Tree, String> {
        let line = ["mycelium", "--realm", "work"];
        let parsed = Command::try_parse_from(line.into_iter().chain(words.split(' ')))
            .map_err(|error| error.kind().to_string())?;
        tree(&parsed.action).unwrap_or_else(|| panic!("{words:?} is a tree"))
    }

    // Joel's pivot to command trees (2026-11-12): one command for each
    // collection, the singular a silent alias, and every old spelling
    // still working for the callers that use it.
    #[test]
    fn every_old_spelling_asks_what_its_tree_does() {
        Command::command().debug_assert();
        let pi = || vec!["pi".to_owned()];
        for (old, new, both) in [
            (
                "machine penelope",
                "machines penelope",
                Tree::Machine("penelope".to_owned()),
            ),
            ("held", "holds", Tree::Holds),
            (
                "attach h0",
                "holds h0 attach",
                Tree::Attach("h0".to_owned()),
            ),
            (
                "again h0 -- pi",
                "holds h0 again -- pi",
                Tree::Again {
                    id: "h0".to_owned(),
                    command: pi(),
                },
            ),
            (
                "hold --detached -- pi",
                "holds add --detached -- pi",
                Tree::Hold {
                    folder: None,
                    detached: true,
                    command: pi(),
                },
            ),
        ] {
            assert_eq!(asked(old), Ok(both.clone()), "{old}");
            assert_eq!(asked(new), Ok(both), "{new}");
        }
        assert_eq!(asked("machines"), Ok(Tree::Machines), "bare, every machine");
        assert_eq!(
            asked("machine"),
            Ok(Tree::ThisMachine),
            "the old singular, bare, this one"
        );
        assert_eq!(
            asked("holds h0"),
            Ok(Tree::Held("h0".to_owned())),
            "one held"
        );
        assert_eq!(
            asked("hold h0 attach"),
            Ok(Tree::Attach("h0".to_owned())),
            "the singular is the collection"
        );
        assert!(
            matches!(
                Command::try_parse_from(["m", "attach", "--host", "h", "--session", "s"])
                    .map(|line| tree(&line.action)),
                Ok(None)
            ),
            "a pi host's conversation is joined outside the tree"
        );
    }

    #[test]
    fn words_that_dont_fit_the_holds_say_why() {
        for (words, why) in [
            ("holds add", "add what? name the program after --"),
            (
                "holds h0 again",
                "h0 again as what? name the program after --",
            ),
            (
                "holds h0 attach -- pi",
                "a program after -- is for holds add and holds ID again",
            ),
            (
                "holds h0 --detached",
                "--folder and --detached are for holds add",
            ),
        ] {
            assert_eq!(asked(words), Err(why.to_owned()), "{words}");
        }
        assert!(asked("holds h0 juggle").is_err(), "an act it doesn't know");
    }

    /// What the words after `mycelium --realm work swarm` ask, and whether
    /// they ask for JSON, as the command line parses them.
    fn swarm(words: &[&str]) -> (Result<SwarmAsked, String>, bool) {
        let line = ["mycelium", "--realm", "work", "swarm"];
        let asked = Command::try_parse_from(line.iter().chain(words))
            .unwrap_or_else(|error| panic!("{words:?} parses: {error}"));
        match asked.action {
            Action::Swarm { words, output, .. } => (swarm_asked(&words), output.json()),
            _ => panic!("{words:?} is the swarm"),
        }
    }

    fn show(team: &str) -> SwarmAsked {
        SwarmAsked::Show(Some(team.to_owned()))
    }

    // Joel typed `mycelium --realm work swarm team quick` first, and was
    // told "error: unexpected argument 'quick' found" (2026-11-12).
    #[test]
    fn every_word_after_the_swarm_names_its_team_but_a_leading_team() {
        Command::command().debug_assert();
        assert_eq!(
            swarm(&["team", "quick"]),
            (Ok(show("quick")), false),
            "his line"
        );
        assert_eq!(
            swarm(&[]),
            (Ok(SwarmAsked::Show(None)), false),
            "the overview"
        );
        assert_eq!(
            swarm(&["part", "deux"]),
            (Ok(show("part deux")), false),
            "two words, one team"
        );
        assert_eq!(
            swarm(&["quick", "wins", "--json"]),
            (Ok(show("quick wins")), true),
            "and asked for as JSON"
        );
        assert!(
            matches!(
                Command::try_parse_from(["mycelium", "swarm", "--names"]).map(|asked| asked.action),
                Ok(Action::Swarm { names: true, .. })
            ),
            "--names asks for the teams as they're typed"
        );
    }

    // A team's words sit where an act's would, so each must still be told
    // apart from the other, whichever comes first.
    #[test]
    fn an_act_takes_its_team_before_or_after_it() {
        let say = |name: &str, words: &str| {
            Ok(SwarmAsked::Say {
                name: name.to_owned(),
                words: words.to_owned(),
                now: false,
            })
        };
        let now = |name: &str, words: &str| {
            Ok(SwarmAsked::Say {
                name: name.to_owned(),
                words: words.to_owned(),
                now: true,
            })
        };
        assert_eq!(
            swarm(&["say", "harbour", "now", "stop", "that"]).0,
            now("harbour", "stop that"),
            "now first goes into the turn under way (Joel, 2026-11-12)"
        );
        assert_eq!(
            swarm(&["part", "deux", "say", "Now", "stop"]).0,
            now("part deux", "stop"),
            "after the team's words too, in any case"
        );
        assert_eq!(
            swarm(&["say", "harbour", "ship", "it", "now"]).0,
            say("harbour", "ship it now"),
            "a now among the words is only a word"
        );
        assert_eq!(
            swarm(&["say", "harbour", "now"]).0,
            Err("say what to harbour now?".to_owned()),
            "now without words"
        );
        assert_eq!(
            swarm(&["say", "harbour", "ship", "it"]).0,
            say("harbour", "ship it"),
            "the act first takes one word for its team"
        );
        assert_eq!(
            swarm(&["part", "deux", "say", "ship", "it"]).0,
            say("part deux", "ship it"),
            "the team's words first are all its name"
        );
        assert_eq!(
            swarm(&["team", "part", "deux", "pause"]).0,
            Ok(SwarmAsked::Pause("part deux".to_owned())),
            "pause after its team"
        );
        assert_eq!(
            swarm(&["Resume", "reef"]).0,
            Ok(SwarmAsked::Resume("reef".to_owned())),
            "resume before it, in any case"
        );
        assert_eq!(
            swarm(&["say", "harbour"]).0,
            Err("say what to harbour?".to_owned()),
            "say without words"
        );
        assert_eq!(
            swarm(&["pause"]).0,
            Err("pause which team?".to_owned()),
            "an act without its team"
        );
        assert_eq!(
            swarm(&["reef", "pause", "now"]).0,
            Err("pause takes only a team, not now".to_owned()),
            "words after a pause"
        );
    }
}
