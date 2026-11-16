//! The `mycelium` command: the node, the far end of a link, and a few
//! requests of a running node's socket for looking into it by hand. Pi and
//! Quickshell read the socket themselves; nothing here is how a person uses
//! the network.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use mycelium::awake::Awake;
use mycelium::builders::{self, Builder};
use mycelium::carry::easing::Eases;
use mycelium::carry::through::Through;
use mycelium::client::Client;
use mycelium::flow::Flows;
use mycelium::group::{Group, Pattern};
use mycelium::hold::environment::ServiceManager;
use mycelium::hold::journal::SystemJournal;
use mycelium::hold::start::{Scope, Starter, login_shell};
use mycelium::link::dial::{Connection, Dial as _, Program, Ssh, Way};
use mycelium::link::discovery::Listed;
use mycelium::link::membership::{Membership, Proven, Realm};
use mycelium::link::refused::Refusals;
use mycelium::placement::{Need, Work};
use mycelium::record::Intent;
use mycelium::share::exchange::Places;
use mycelium::share::reached::{self, Reached};
use mycelium::ssh::Asked as KeyAsked;
use mycelium::store::Store as _;
use mycelium::store::folders::Folders;
use mycelium::web::Said;
use mycelium::wire::{Draft, Ran, Reply, Request, Response, Run, Start};
use mycelium::{carry, hold, hosted_attach, install, link, node, place, rejoin, served};

/// The widest a line of any command's help is drawn, so it reads in a
/// narrow pane.
const HELP_WIDTH: usize = 60;

#[derive(Parser)]
#[command(
    version,
    about = "A nimble core for agent networks",
    max_term_width = HELP_WIDTH,
    after_help = "Example:\n  mycelium networks"
)]
struct Command {
    /// The realm: a set of machines that link, with nothing crossing
    /// between realms.
    #[arg(long, env = "MYCELIUM_REALM", global = true)]
    realm: Option<String>,
    /// The node's socket, if not where the realm's goes.
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    /// The command, which bare `mycelium` leaves out to list them all.
    /// It is optional rather than clap's `arg_required_else_help`, which
    /// counts a realm set in the environment as an argument given, and a
    /// person's shell most often sets one.
    #[command(subcommand)]
    action: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// Runs this machine's node.
    ///
    /// The node links this machine with the others of its realm and
    /// answers every other command here.
    #[command(after_help = "Example:\n  mycelium node --ssh joel@skiff.local")]
    Node(Box<Node>),
    /// Runs this machine's end of a link to another machine.
    ///
    /// Another machine's node starts it over SSH and speaks through
    /// its standard input and output.
    #[command(after_help = "Example:\n  mycelium link")]
    Link {
        #[command(flatten)]
        here: Here,
    },
    /// Runs what another machine's link key asks of this one.
    ///
    /// Its pinned `authorized_keys` line runs this: a link, whose far
    /// end must be that machine, or an attach; anything else is refused
    /// and kept in `refused-keys.jsonl` in mycelium's folder.
    #[command(after_help = "Example:\n  mycelium ssh --from tern")]
    Ssh {
        #[command(flatten)]
        here: Here,
        /// The machine whose key this is.
        #[arg(long)]
        from: String,
    },
    /// Shares a quest with every machine this one links with.
    ///
    /// Its record is kept the same on each and written on any, with the
    /// folders of its scratch named, and its handover whenever it is
    /// there. A machine that held its own copy of either moves it aside
    /// first. With `--to` and `--wait`, it returns once that machine holds
    /// it, printing a JSON line, `{"quest", "machine", "held", ...}`.
    #[command(
        after_help = "Example:\n  mycelium share QEST-20261112-HARBOR --scratch notes \
                            --to skiff --wait"
    )]
    Share(Sharing),
    /// Carries a quest and its sessions to another machine.
    ///
    /// Each of its sessions here carries its work into the quest's
    /// handover and ends, `pi-carry` takes the rest, the quest is shared
    /// with its handover, and each session starts again there, held, once
    /// `pi-carry arrive` has its trees. Prints a JSON line for each step,
    /// `{"step", "state", "said"}`, and last where each session went.
    /// With `--why`, the quest's orchestrator orders it, and each session
    /// arrives as carried by it for that reason; one that refuses stops
    /// the carry with its own.
    #[command(after_help = "Example:\n  mycelium carry QEST-20261112-HARBOR --to tern")]
    Carry {
        #[command(flatten)]
        here: Here,
        /// The quest's id, such as `QEST-20261112-HARBOR`.
        quest: String,
        /// The machine it goes to, by name.
        #[arg(long)]
        to: String,
        /// Why its orchestrator orders it, which only the orchestrator's
        /// own session may.
        #[arg(long, value_parser = reason)]
        why: Option<String>,
    },
    /// Prints every session the node knows of.
    ///
    /// Each is a JSON line, as the node holds it.
    #[command(after_help = "Example:\n  mycelium sessions")]
    Sessions,
    /// Prints the questions and holds the mesh waits on.
    ///
    /// Each question of the last two hours still waiting on a peer it
    /// was put to, and each hold still standing, oldest first, a JSON
    /// line each, as the node holds it.
    #[command(after_help = "Example:\n  mycelium unanswered")]
    Unanswered,
    /// Prints what you said to sessions through this machine.
    ///
    /// Oldest first, with when each was taken up and answered, a JSON
    /// line each, as the node holds it.
    #[command(after_help = "Example:\n  mycelium said")]
    Said,
    /// Shows the teams and how their sessions stand, or steers them.
    ///
    /// Bare, it shows every team, nested as they are, with how many of
    /// its sessions need you, are stuck, move or wait, those needing you
    /// first, each by its short name; named by that or any part of its
    /// title, one team's sessions and where it meets the others.
    ///
    /// Its acts steer the swarm as its person: `say` words to a team,
    /// which its lead hears, or to one session, read once its turn ends,
    /// or into that turn when the words begin `now`; `pause` every
    /// session of a team and the teams within it, or one session, so each
    /// finishes its turn and waits; and `resume` them.
    #[command(after_help = "Example:\n  mycelium swarm say grow ship it")]
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
    /// Draws every network as a tree under its shape: orchestrations,
    /// projects, repositories, groups and machines, each with how its
    /// sessions stand. Named by its short name, its name or words of its
    /// title, one network is drawn with its cells. `network` is the same.
    #[command(alias = "network", after_help = "Example:\n  mycelium networks grow")]
    Networks(NetworksAsked),
    /// Lists the cells, or says what one is.
    ///
    /// Lists every cell: the sessions, worst first, then the machines.
    /// Named by its name or words of its title, a session is said with
    /// the networks it joined, and a machine as `machines NAME` says it.
    /// `cell` is the same.
    #[command(alias = "cell", after_help = "Example:\n  mycelium cells HARBOR")]
    Cells(CellsAsked),
    /// Says what each cell is for: its forage.
    ///
    /// Each cell working on a quest, by its address, with its purpose, the
    /// first sentence of its quest's Summary, and each of its documents:
    /// kind, stage, how many steps are ticked, and title. Named by its
    /// address or its quest's id, one cell's forage.
    #[command(after_help = "Example:\n  mycelium forage HARBOR")]
    Forage(ForageAsked),
    /// Says how a receptor stands, as the receptor.
    ///
    /// A receptor senses one outside source, intake only, and says how it
    /// stands in a file the node serves: counts and times alone, never what
    /// it read. `mycelium cells` lists each beside the sessions.
    #[command(
        alias = "receptor",
        after_help = "Example:\n  mycelium receptors say slack --state quiet \\\n    --every 300 --pid 4242"
    )]
    Receptors(ReceptorsAsked),
    /// Prints how the realm's network stands, for a daily read.
    ///
    /// One JSON line says each link, `up` or `unreachable`, with how many
    /// times it dropped in the last day; each machine's sessions by how
    /// they stand; how many presences still say they run with their
    /// process gone; and the node's slowest answer to this read, in whole
    /// seconds. Exits 0 whenever the node could be read, and within 10
    /// seconds.
    #[command(after_help = "Example:\n  mycelium health")]
    Health {
        #[command(flatten)]
        here: Here,
    },
    /// Prints how evenly the realm's machines carry its work.
    ///
    /// Says each machine's load, the largest of the memory it uses, the
    /// share of its processors it spends and how long its work stalls,
    /// whether it is over or has room, its busy and idle sessions, how far
    /// the loads spread, and what is wrong, each in a sentence saying what
    /// follows: a machine over while another has room, one resource spent
    /// while the other idles, loads spread too far, or facts too old to
    /// place by. With `-o json`, the same as one JSON line. With
    /// `--follow`, again each minute, each saying the flags seen in most
    /// of the last ten, each kept as a JSON line in the state folder's
    /// `balance`, one file a day, and the latest kept whole in its
    /// `now.json`.
    #[command(after_help = "Example:\n  mycelium balance --follow")]
    Balance(BalanceAsked),
    /// Says how well this node placed work, judged in hindsight.
    ///
    /// Reads the placements this node kept and the balance kept each
    /// minute, and says which placements its machine regretted a minute
    /// on, carrying more than another with room, and which herded onto
    /// one machine while another had more headroom, as a JSON line.
    ///
    /// With `--replay`, places each kept start again under the weights in
    /// a JSON file, any left out taking the starting ones, and says which
    /// it would have placed elsewhere and hindsight on both, and which other
    /// kept answers, asks of where and runs, it would have given elsewhere.
    #[command(after_help = "Example:\n  mycelium placements --hours 168 --replay weights.json")]
    Placements(PlacementsAsked),
    /// Speaks in a quest's web of murmurs and ties.
    ///
    /// A web is what a quest's sessions on every machine share: this says
    /// into it, or folds it into what it says now.
    #[command(after_help = "Example:\n  mycelium web QEST-20261112-HARBOR say look at the tide")]
    Web(WebAsked),
    /// Says what each machine of the realm is, or what one is.
    ///
    /// Bare, it prints every machine, this one first, then on a line of
    /// their own the builders this machine sends Nix builds to; given its
    /// name, one machine or builder. `machine` is the same, and bare
    /// prints what this machine is.
    #[command(after_help = "Example:\n  mycelium machines tern")]
    Machines {
        /// The machine or builder, such as `tern`.
        name: Option<String>,
    },
    /// `machines`, as it was spelt before, when bare it printed this
    /// machine.
    #[command(hide = true)]
    Machine {
        /// The machine, if not this one.
        name: Option<String>,
    },
    /// Prints the machines the node's providers could make.
    ///
    /// Each shape of machine a provider offers, and why any provider
    /// couldn't say.
    #[command(after_help = "Example:\n  mycelium offers")]
    Offers,
    /// Says which machine of the realm work should go to.
    ///
    /// Or, where none fits, why not.
    #[command(after_help = "Example:\n  mycelium place --need system=linux")]
    Place(Asked),
    /// Asks a machine to update itself.
    ///
    /// It updates to its configuration's main, this machine or one it
    /// links with directly, and this prints how that ended, as its
    /// updater said it.
    #[command(after_help = "Example:\n  mycelium update tern")]
    Update {
        /// The machine, such as `tern`.
        machine: String,
    },
    /// Prints the latest messages on the mesh.
    ///
    /// Each is a JSON line, the newest last.
    #[command(after_help = "Example:\n  mycelium messages quest.* --limit 5")]
    Messages {
        /// Which groups, such as `quest.*`.
        #[arg(default_value = ">")]
        groups: Pattern,
        /// At most this many.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Sends a message on the mesh, as a session would.
    ///
    /// It goes to every group named, from who `--from` says.
    #[command(
        override_usage = "mycelium send [OPTIONS] --from <FROM> --text <TEXT>\n       <GROUPS>...",
        after_help = "Example:\n  mycelium send --from HARBOR --text merged peer.kelp01"
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
    ///
    /// Each is a flow the node was started with.
    #[command(after_help = "Example:\n  mycelium flows")]
    Flows,
    /// Starts a program, held, on the machine that suits it.
    ///
    /// The node starts it on the machine of the realm placement chooses,
    /// through that person's login shell, and this prints the machine,
    /// its id and the words that attach to it.
    #[command(after_help = "Example:\n  mycelium start --need system=linux -- pi")]
    Start(Starting),
    /// Runs a command on the machine that suits it, and waits on it.
    ///
    /// The node runs it on the machine of the realm placement chooses,
    /// through that person's login shell, with nothing on its input. What
    /// it writes comes back as it writes it, and this ends with its exit
    /// code, or 255 when it couldn't run. Stopping this stops it.
    #[command(after_help = "Example:\n  mycelium run --need machine=kestrel -- nix flake check")]
    Run(Running),
    /// Steers a session by its address, from standard input.
    ///
    /// What is said goes into the steer inbox of the session going by
    /// ADDRESS, on whichever machine it runs. The steer is read from
    /// input, as the harness writes one: `{"text": "..."}`, with `act`
    /// and `id` if it pauses, resumes, stops, closes or opens a talk, and
    /// `to` for a carry.
    #[command(after_help = "Example:\n  echo '{\"text\": \"ship it\"}' | mycelium steer HARBOR")]
    Steer {
        /// The session's address.
        address: String,
    },
    /// Records which quests a session orchestrates.
    ///
    /// Run by an orchestrator's session, from its own process, as it loads
    /// the orchestrating quest HEADS and whenever the quests below it
    /// change; the node proves which session runs it and records the
    /// claim, refusing a quest another session holds. Without HEADS, it
    /// forgets what the session claimed, as it unloads or concludes.
    ///
    /// `--free QUEST`, from the person's own login, frees a quest from
    /// the session holding it, such as one another session took while its
    /// orchestrator was away.
    #[command(
        after_help = "Example:\n  mycelium orchestrates --heads QEST-20261007-WCJQSG \
                      QEST-20261108-OSPHT4 QEST-20261115-HARBOR"
    )]
    Orchestrates {
        /// The orchestrating quest the session has loaded.
        #[arg(long, conflicts_with = "free")]
        heads: Option<String>,
        /// The quests it orchestrates.
        #[arg(conflicts_with = "free")]
        quests: Vec<String>,
        /// A quest to free from whichever session holds it, as the person.
        #[arg(long)]
        free: Option<String>,
    },
    /// Lists the sessions held on this machine, or holds one.
    ///
    /// Bare, it prints every session held here, a JSON line each, or the
    /// one named. `holds add -- PROGRAM` starts a program on a terminal a
    /// holder of its own keeps, so its window can close and `holds ID
    /// attach` take it up again from any terminal, and joins it unless
    /// `--detached`.
    ///
    /// `holds ID again -- PROGRAM` names the command it starts with when
    /// it next starts again, over any its program names as it parks, and
    /// wakes it if it is parked, printing `{"id", "woke"}`; a pi held on
    /// an old conversation is moved onto a newer one this way. `hold` is
    /// the same.
    #[command(alias = "hold", after_help = "Example:\n  mycelium holds add -- pi")]
    Holds(Holds),
    /// Joins a conversation a pi host holds.
    ///
    /// Name it with `--host` and `--session`; closing it only leaves. It
    /// exits 3 when the host lets the conversation go back to plain pi,
    /// as a release does, and 0 on any other leaving. A held session is
    /// joined with `holds ID attach`, which `attach ID` still spells.
    ///
    /// With `--on MACHINE`, either is joined on that machine by the words
    /// after `--`, or SSH to it by its name, run again each time SSH ends
    /// 255 as a dropped link does, after saying `Reconnecting to MACHINE.`
    /// and waiting a second, then twice as long each time up to 30. It ends
    /// as the join there does, with its code; with 0 once this machine's
    /// node says the session has ended; and with 130 on Ctrl+C.
    #[command(
        after_help = "Example:\n  mycelium attach --host work-attended \\\n    --session 01a1d045-d580-7000-8000-000000000000"
    )]
    Attach(Attaching),
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
    /// Prints the node's events as they come.
    ///
    /// Each is a JSON line, the latest messages first.
    #[command(after_help = "Example:\n  mycelium watch quest.*")]
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
    /// `joel@skiff.local`; given again for each.
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
    /// What this machine's configuration declares of it, as a file only
    /// root writes, such as its path in the Nix store. The node reads it
    /// alone, never the account's own, so an account's link can't widen
    /// how its agents' commands run. Unnamed, the account's is read.
    /// Whatever writes the node's own unit can name another file, or
    /// another program for the node, so this guards nothing against it;
    /// it takes a restart that shows, and the node says at start what it
    /// reads and how its agents run, so the journal shows it too.
    #[arg(long)]
    declared: Option<PathBuf>,
    /// A realm this realm's text may cross into, one way, where a session
    /// names it; given again for each. PROTOCOL.md says what crosses.
    #[arg(long = "flow-to")]
    flows_to: Vec<String>,
    /// A realm whose text this realm takes, one way; given again for each.
    #[arg(long = "flow-from")]
    flows_from: Vec<String>,
    /// A Nix machines file naming builders this machine sends builds to,
    /// as `NAME=PATH`; each builder is dialled at most every five
    /// minutes to hear it answer. Given again for each.
    #[arg(long = "builders", value_parser = dialled_builders)]
    builders: Vec<builders::File>,
    /// A Nix machines file naming lent builders, as `NAME=PATH`, which
    /// are never dialled, since dialling one may wake what costs to
    /// run. Given again for each.
    #[arg(long = "lent-builders", value_parser = lent_builders)]
    lent_builders: Vec<builders::File>,
    /// Where a lent builder's lender says how it stands, as a path with
    /// `{host}` for the builder's host, such as `/run/{host}/health.json`.
    #[arg(long)]
    lent_health: Option<String>,
    /// The socket this machine's own health daemon answers on in the
    /// daemon interface, whose words its machine record carries as
    /// `health`; PROTOCOL.md says what is asked.
    #[arg(long)]
    health_socket: Option<PathBuf>,
    /// Look each minute at what this machine would do to ease itself
    /// while over its limits and another has room, keeping each choice
    /// in the state folder's `eases`, a file each day; it does none of
    /// it.
    #[arg(long)]
    eases: bool,
}

/// A machines file of builders to dial, as `--builders` gives it.
fn dialled_builders(said: &str) -> Result<builders::File, String> {
    builders::File::parse(said, false)
}

/// A machines file of lent builders, as `--lent-builders` gives it.
fn lent_builders(said: &str) -> Result<builders::File, String> {
    builders::File::parse(said, true)
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

    /// The kinds it serves from files, as its `--serves` file names them,
    /// on the machine named `machine`.
    fn serves(&self, machine: &str) -> io::Result<BTreeMap<String, served::Source>> {
        match &self.serves {
            Some(file) => served::configured(
                file,
                &node::HOLDS,
                &served::Here {
                    machine: machine.to_owned(),
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

/// What is asked of the realm's balance.
#[derive(clap::Args)]
struct BalanceAsked {
    /// Reads each minute until stopped, keeping each line.
    #[arg(long)]
    follow: bool,
    #[command(flatten)]
    output: Output,
    #[command(flatten)]
    here: Here,
}

/// What is asked of the networks, by the words after `networks`.
#[derive(clap::Args)]
struct NetworksAsked {
    /// The network, such as `grow` or `world`
    words: Vec<String>,
    /// Puts each network's cells under it.
    #[arg(long)]
    cells: bool,
    /// Shows every network and cell, folding no long list.
    #[arg(long)]
    all: bool,
    /// With `-o json`, each network and its counts, without its cells.
    #[arg(long)]
    counts: bool,
    #[command(flatten)]
    watching: Watching,
    #[command(flatten)]
    output: Output,
}

/// Whether what is asked is drawn once, or again as it changes.
#[derive(clap::Args)]
struct Watching {
    /// Redraws it in place every two seconds, until stopped.
    #[arg(short = 'w', long, conflicts_with_all = ["format", "json"])]
    watch: bool,
}

/// What is asked of the cells, by the words after `cells`.
#[derive(clap::Args)]
struct CellsAsked {
    /// The cell, such as `HARBOR` or `skiff`
    words: Vec<String>,
    /// Shows every cell, folding no long list.
    #[arg(long)]
    all: bool,
    /// Only the sessions standing so, such as `needs-you,stuck`.
    #[arg(
        long,
        value_delimiter = ',',
        value_parser = clap::builder::PossibleValuesParser::new(mycelium::networks::STANDINGS)
    )]
    standing: Vec<String>,
    #[command(flatten)]
    watching: Watching,
    #[command(flatten)]
    output: Output,
}

/// What is asked of the forages, by the words after `forage`.
#[derive(clap::Args)]
struct ForageAsked {
    /// The cell, by its address such as `HARBOR`, or its quest's id
    cell: Option<String>,
    #[command(flatten)]
    output: Output,
}

/// Which session `attach` joins, and where.
#[derive(clap::Args)]
struct Attaching {
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
    /// The machine it runs on, when it isn't this one.
    #[arg(long)]
    on: Option<String>,
    /// With `--on`, the words that join it from here, after `--`.
    #[arg(last = true, requires = "on")]
    words: Vec<String>,
    /// With `--on`, the first wait to join again, in milliseconds.
    #[arg(long, hide = true, requires = "on")]
    first_wait: Option<u64>,
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
        Action::Attach(Attaching {
            id: Some(id),
            on: None,
            ..
        }) => Tree::Attach(id.clone()),
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

/// A quest to share, and the machine to wait for, if any.
#[derive(clap::Args)]
struct Sharing {
    #[command(flatten)]
    here: Here,
    /// The quest's id, such as `QEST-20261112-HARBOR`.
    quest: String,
    /// A folder of the quest's scratch to share too, such as `notes`;
    /// none is shared unless named, but its handover, and one holding
    /// more than 10,000 files or 1 GiB is refused.
    #[arg(long)]
    scratch: Vec<String>,
    /// The machine to wait for, by name.
    #[arg(long, requires = "wait")]
    to: Option<String>,
    /// Waits until `--to` holds the quest's record and named scratch, and
    /// the READMEs above it, at this machine's versions, failing past
    /// `--within`.
    #[arg(long, requires = "to")]
    wait: bool,
    /// How many seconds to wait at most.
    #[arg(long, requires = "wait", default_value_t = 120)]
    within: u64,
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
    /// Makes a machine for it from a provider's offer that costs money,
    /// where no machine of the realm takes it. One that costs nothing is
    /// made without it.
    #[arg(long)]
    may_cost: bool,
    /// The program and its arguments, after `--`.
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

#[derive(clap::Args)]
struct Running {
    /// What the machine must be, as for `place`.
    #[command(flatten)]
    work: Asked,
    /// The folder it runs in, found as for `start`.
    #[arg(long)]
    folder: Option<PathBuf>,
    /// The command and its arguments, after `--`.
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

/// What is asked of the receptors, by the words after `receptors`.
#[derive(clap::Args)]
struct ReceptorsAsked {
    #[command(subcommand)]
    act: ReceptorAct,
}

/// What a receptor does.
#[derive(Subcommand)]
enum ReceptorAct {
    /// Says how this receptor stands now.
    ///
    /// Written whole over what it said before, its counts and times
    /// alone: any other word is refused.
    #[command(
        override_usage = "mycelium receptors say [OPTIONS] --state <STATE>\n       --every <EVERY> --pid <PID> <SOURCE>",
        after_help = "Example:\n  mycelium receptors say slack --state reading \\\n    --today 4 --addressed 2 --every 300 --pid 4242"
    )]
    Say(ReceptorSaid),
}

/// How a receptor says it stands: counts and times alone.
#[derive(clap::Args)]
struct ReceptorSaid {
    /// What it senses.
    #[arg(value_parser = clap::builder::PossibleValuesParser::new(
        mycelium::receptor::Source::EACH.map(mycelium::receptor::Source::name)
    ))]
    source: String,
    /// How it stands.
    #[arg(long, value_parser = ["reading", "quiet", "failing", "off"])]
    state: String,
    /// Signals it brought in in the last hour.
    #[arg(long, default_value_t = 0)]
    hour: u32,
    /// Signals it brought in today.
    #[arg(long, default_value_t = 0)]
    today: u32,
    /// Of today's, those addressed to its person.
    #[arg(long, default_value_t = 0)]
    addressed: u32,
    /// How often it reads, in seconds.
    #[arg(long)]
    every: u32,
    /// The process that keeps it, which the node marks it off without.
    #[arg(long)]
    pid: u32,
    /// Mycelium's own folder, if not where it goes by default.
    #[arg(long = "state-folder")]
    state_folder: Option<PathBuf>,
}

/// Writes how the receptor `said` stands for `realm`, saying where.
fn receptor_said(said: ReceptorSaid, realm: String) -> io::Result<()> {
    use mycelium::receptor::{self, Counts, Record, Source, State};
    let word = |what: &str, value: &str| {
        serde_json::from_value(serde_json::Value::from(value)).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{value:?} is no receptor's {what}"),
            )
        })
    };
    let source = Source::named(&said.source).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{:?} is no receptor's source", said.source),
        )
    })?;
    let state: State = word("state", &said.state)?;
    let now = jiff::Timestamp::now();
    let record = Record {
        source,
        realm,
        state,
        since: now,
        read_at: now,
        every: said.every,
        pid: said.pid,
        started: receptor::started_at(said.pid),
        counts: Counts {
            hour: said.hour,
            today: said.today,
            addressed: said.addressed,
        },
    };
    let folder = receptor::folder(&said.state_folder.unwrap_or_else(place::state));
    let written = receptor::write(&folder, &record.realm.clone(), record, &receptor::Here)?;
    writeln!(io::stdout().lock(), "{}", written.display())
}

#[derive(clap::Args)]
struct PlacementsAsked {
    /// How many hours back to judge.
    #[arg(long, default_value_t = 1)]
    hours: u16,
    /// A JSON file of weights to place the kept starts again under.
    #[arg(long, value_name = "WEIGHTS")]
    replay: Option<PathBuf>,
    #[command(flatten)]
    here: Here,
}

#[derive(clap::Args)]
struct WebAsked {
    /// The quest whose web it is, such as `QEST-20261112-HARBOR`.
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
    /// Says a murmur into the web.
    ///
    /// A murmur is a line of intent for whoever heeds it. This prints the
    /// event, whose id ties and settles it.
    #[command(after_help = "Example:\n  mycelium web QEST-20261112-HARBOR say look at the tide")]
    Say {
        /// Its words.
        #[arg(required = true)]
        words: Vec<String>,
        /// Who says it, such as a session's address.
        #[arg(long)]
        by: Option<String>,
    },
    /// Ties one thing to another.
    ///
    /// Either may be a murmur's id, a quest's or a change's. This prints
    /// the event.
    #[command(after_help = "Example:\n  mycelium web QEST-20261112-HARBOR tie skiff-12 skiff-9")]
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
    /// Settles a murmur, saying how.
    ///
    /// This prints the event.
    #[command(after_help = "Example:\n  mycelium web QEST-20261112-HARBOR settle skiff-12 filed")]
    Settle {
        /// The murmur's id.
        of: String,
        /// How, such as `filed` or `let go`.
        how: String,
        /// Who settles it.
        #[arg(long)]
        by: Option<String>,
    },
    /// Folds every machine's events into one file.
    ///
    /// The file is written whole, as a page reads it, and this prints
    /// whether it wrote; with `--every`, again each so many seconds until
    /// stopped, writing only when it says something new.
    #[command(after_help = "Example:\n  mycelium web QEST-20261112-HARBOR fold --into web.json")]
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

/// The exit code for a command line that named no command, as clap gives
/// one that asked for nothing.
const NO_COMMAND: u8 = 2;

fn main() -> ExitCode {
    let Command {
        realm,
        socket,
        action,
    } = Command::parse();
    let Some(action) = action else {
        // Help that can't be written, as to a pipe closed early, has
        // nowhere else to go, and the exit code still says no command ran.
        let _ = Command::command().print_help();
        return ExitCode::from(NO_COMMAND);
    };
    match run(realm, socket, action) {
        Ok(code) => code,
        Err(error) if reader_left(&error, io::stdout()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mycelium: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Whether `error` is `out`'s reader having gone, as when `| head` has
/// read all it wanted. The answer was taken as far as it was wanted, so
/// nothing is said and the command succeeds. A broken pipe while `out`
/// is still read is the node hanging up, and is said. A platform whose
/// gone reader polls as neither an error nor a hang-up, as macOS may,
/// has the error said too, which errs on the safe side.
fn reader_left(error: &io::Error, out: impl AsFd) -> bool {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    if error.kind() != io::ErrorKind::BrokenPipe {
        return false;
    }
    // A pipe's writing end polls as an error once nothing reads it, so
    // the look is at `out` itself rather than at whatever broke.
    let mut looked = [PollFd::new(&out, PollFlags::OUT)];
    let now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    poll(&mut looked, Some(&now)).is_ok()
        && looked[0]
            .revents()
            .intersects(PollFlags::ERR | PollFlags::HUP)
}

fn run(realm: Option<String>, socket: Option<PathBuf>, action: Action) -> io::Result<ExitCode> {
    let realm = realm.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "say which realm, with --realm or MYCELIUM_REALM",
        )
    })?;
    let realm = realm_named(realm)?;
    let socket = socket.unwrap_or_else(|| place::socket(&realm));
    if let Some(asked) = tree(&action) {
        let asked = asked.map_err(|why| io::Error::new(io::ErrorKind::InvalidInput, why))?;
        return in_tree(&socket, &realm, asked);
    }
    let ran = match action {
        Action::Node(node) => run_node(*node, realm, &socket),
        Action::Link { here } => run_link(here, realm, socket, None),
        Action::Ssh { here, from } => return run_asked(here, realm, socket, from),
        Action::Share(sharing) => share(sharing, realm, &socket),
        Action::Carry {
            here,
            quest,
            to,
            why,
        } => {
            let (_, membership, shares) = open(here, realm)?;
            let carrying = (quest.as_str(), membership.machine.as_str(), to.as_str());
            carry(&socket, shares, carrying, why.as_deref())
        }
        Action::Sessions => print_reply(&socket, &Request::Sessions),
        Action::Unanswered => print_records(&socket, mycelium::unanswered::KIND),
        Action::Said => print_records(&socket, mycelium::words::KIND),
        Action::Health { here } => health(&socket, here, realm),
        Action::Balance(asked) => balance(&socket, &realm, asked),
        Action::Placements(asked) => placements(asked),
        Action::Swarm {
            words,
            output,
            names,
        } => in_swarm(&socket, &realm, &words, output.json(), names),
        Action::Networks(asked) => networks(&socket, &realm, &asked),
        Action::Cells(asked) => cells(&socket, &realm, &asked),
        Action::Forage(asked) => forage(&socket, &asked),
        Action::Receptors(ReceptorsAsked {
            act: ReceptorAct::Say(said),
        }) => receptor_said(said, realm),
        Action::Web(asked) => web(asked),
        Action::Offers => print_reply(&socket, &Request::Offers),
        Action::Place(asked) => print_reply(&socket, &Request::Place(asked.into())),
        Action::Update { machine } => print_reply(&socket, &Request::Update { machine }),
        Action::Start(asked) => start(&socket, asked),
        Action::Run(asked) => return run_there(&socket, asked),
        Action::Steer { address } => steer(&socket, address),
        Action::Orchestrates {
            free: Some(quest), ..
        } => print_reply(&socket, &Request::Free { quest }),
        Action::Orchestrates { heads, quests, .. } => {
            print_reply(&socket, &Request::Orchestrates { heads, quests })
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
        Action::Attach(asked) => return attach_asked(&realm, &socket, asked),
        Action::Holder {
            id,
            folder,
            login,
            command,
        } => hold_here(&realm, &id, &folder, login, &command, &socket),
        Action::Watch { groups, recent } => watch(&socket, groups, recent),
        Action::Machines { .. }
        | Action::Machine { .. }
        | Action::Holds(_)
        | Action::Held
        | Action::Again { .. } => unreachable!("each is read as its tree above"),
    };
    ran.map(|()| ExitCode::SUCCESS)
}

/// Steers the session going by `address` with what standard input says.
fn steer(socket: &Path, address: String) -> io::Result<()> {
    let steer = serde_json::from_reader(io::stdin().lock())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    print_reply(socket, &Request::Steer { address, steer })
}

/// Does what `asked` asks of the machines, through the node at `socket`,
/// or of the sessions held here in `realm`.
fn in_tree(socket: &Path, realm: &str, asked: Tree) -> io::Result<ExitCode> {
    let held = place::held(realm);
    match asked {
        Tree::Machines => {
            print_reply(socket, &Request::Machines)?;
            match builders_of(socket) {
                Some(builders) if !builders.is_empty() => {
                    print_line(&Reply::Ok(Response::Builders(builders)))
                }
                _ => Ok(()),
            }
        }
        Tree::ThisMachine => print_reply(socket, &Request::Machine),
        Tree::Machine(name) => print_line(&one_machine(
            Client::connect(socket)?.ask(&Request::Machines)?,
            builders_of(socket).unwrap_or_default(),
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
fn one_machine(reply: Reply, builders: Vec<Builder>, name: &str) -> io::Result<Reply> {
    match reply {
        Ok(Response::Machines(machines)) => machines
            .into_iter()
            .find(|machine| machine.name == name)
            .map(|machine| Ok(Response::Machine(Box::new(machine))))
            .or_else(|| {
                let builder = builders.into_iter().find(|builder| builder.name == name)?;
                Some(Ok(Response::Builders(vec![builder])))
            })
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "no machine of the realm or builder is {name}; `mycelium machines` lists them"
                    ),
                )
            }),
        Ok(_) => Err(io::Error::other(
            "the node answered machines with something else",
        )),
        Err(why) => Ok(Err(why)),
    }
}

/// The builders the node at `socket` sends Nix builds to, or none when it
/// can't say, as an older node can't.
fn builders_of(socket: &Path) -> Option<Vec<Builder>> {
    match Client::connect(socket).ok()?.ask(&Request::Builders) {
        Ok(Ok(Response::Builders(builders))) => Some(builders),
        _ => None,
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
    // The node names its machine for every command there: what it was
    // given, else the system's, recorded once its realm is settled.
    let machine = place::node_machine(node.here.machine.as_deref())?;
    let (dials, serves) = (node.dials(), node.serves(&machine)?);
    let providers = node.providers;
    let declared = node.declared.unwrap_or_else(place::declared);
    let builders = builders::Given {
        files: node
            .builders
            .into_iter()
            .chain(node.lent_builders)
            .collect(),
        lent_health: node.lent_health,
        ssh: "ssh".to_owned(),
    };
    let mut here = node.here;
    here.machine = Some(machine);
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
    place::record_machine(&state, &membership.machine)?;
    let scratch = Some(shares.scratch.clone());
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
        carries: state.join("carries.jsonl"),
        runs: mycelium::run::Given::at(&state),
        hosts: place::hosts(),
        orchestrations: state.join("orchestrations.json"),
        logs: place::logs(),
        words: mycelium::words::Book::at(state.join("said.jsonl")),
        declared: declared.clone(),
        receptors: Some(mycelium::receptor::folder(&state)),
        builders,
        placements: Some(mycelium::placements::Log::at(state.join("placements"))),
        health: node.health_socket,
        scratch,
        eases: node.eases.then(|| Eases {
            folder: state.join("eases"),
            every: carry::easing::EVERY,
        }),
    };
    eprintln!(
        "mycelium: {}",
        mycelium::declared::said_at_start(&declared, &given.runs.declarers)
    );
    let runtime = runtime()?;
    let ran = runtime.block_on(async {
        let bound = node::bind(socket)?;
        // Probing ends with the process, when the node does.
        tokio::spawn(mycelium::reach::keep(declared, place::reached()));
        node::run(store, membership, Listed(dials), given, bound, stopped()).await
    });
    // Once the node has stopped, its links ended and their sessions let go,
    // what its tasks still hold is left for the process's end to free.
    // Freed piece by piece, it is paged back in from swap first: the work
    // node, with 93 watchers and 93 MB swapped, took 2 s of the 5 its
    // service manager allows. A helper still running is ended with the
    // service.
    std::mem::forget(runtime);
    ran
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
        // A dialled link runs apart from the node, so asks it.
        health: None,
        declared: None,
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
            worker: false,
            named: None,
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
        machine: place::machine(here.machine.as_deref(), &state)?,
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

/// How often `balance --follow` reads the realm: as often as a link says
/// another machine's facts.
const BALANCE_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

/// Prints how evenly the realm's machines carry its work, as
/// [`mycelium::balance`] reads it, for a person or as a JSON line, once
/// or, following, each minute.
fn balance(socket: &Path, realm: &str, asked: BalanceAsked) -> io::Result<()> {
    let BalanceAsked {
        follow,
        output,
        here,
    } = asked;
    let print = |reading: &mycelium::balance::Reading| {
        if output.json() {
            return print_line(reading);
        }
        let look = look();
        let shown = mycelium::balance::said(reading, realm, look.width, look.colour);
        writeln!(io::stdout().lock(), "{shown}")
    };
    if !follow {
        return print(&balance_read(socket)?);
    }
    let kept = here.state.unwrap_or_else(place::state).join("balance");
    std::fs::create_dir_all(&kept)?;
    let mut watch = mycelium::balance::Watch::default();
    loop {
        // A node restarting or slow to answer misses a minute, not the
        // watch, so say why and read again at the next.
        match balance_read(socket) {
            Ok(mut reading) => {
                reading.held = Some(watch.take(&reading));
                mycelium::balance::keep(&kept, &reading, jiff::Zoned::now().date())?;
                // The latest whole is for a node to serve; missing it a
                // minute loses nothing the day's line doesn't keep.
                if let Err(error) = mycelium::balance::keep_latest(&kept, &reading) {
                    eprintln!("mycelium balance: couldn't keep the latest reading: {error}");
                }
                print(&reading)?;
            }
            Err(error) => eprintln!("mycelium balance: couldn't read the node: {error}"),
        }
        std::thread::sleep(BALANCE_EVERY);
    }
}

/// Prints what hindsight says of the placements kept over the last
/// `hours`, judged by the balance kept beside them, or, given a file of
/// weights to `replay`, what placing them again under those says.
fn placements(asked: PlacementsAsked) -> io::Result<()> {
    let PlacementsAsked {
        hours,
        replay,
        here,
    } = asked;
    let weights: Option<mycelium::placement::Weights> = replay
        .as_deref()
        .map(|path| {
            let named = |why: String| io::Error::other(format!("{}: {why}", path.display()));
            let said = std::fs::read_to_string(path).map_err(|error| named(error.to_string()))?;
            mycelium::placement::Weights::read(&said).map_err(named)
        })
        .transpose()?;
    let state = here.state.unwrap_or_else(place::state);
    let now = jiff::Zoned::now();
    let since = now
        .timestamp()
        .checked_sub(jiff::SignedDuration::from_hours(i64::from(hours)))
        .map_err(io::Error::other)?;
    let since_day = since.to_zoned(now.time_zone().clone()).date();
    let records: Vec<mycelium::placements::Record> =
        kept_lines(&state.join("placements"), since_day)?
            .into_iter()
            .filter(|record: &mycelium::placements::Record| {
                record
                    .at
                    .parse()
                    .is_ok_and(|at: jiff::Timestamp| at >= since)
            })
            .collect();
    let readings: Vec<mycelium::balance::Reading> = kept_lines(&state.join("balance"), since_day)?
        .into_iter()
        .filter(|reading: &mycelium::balance::Reading| {
            reading.measures == mycelium::balance::MEASURES
                && reading
                    .at
                    .parse()
                    .is_ok_and(|at: jiff::Timestamp| at >= since)
        })
        .collect();
    match weights {
        Some(weights) => print_line(&mycelium::replay::replay(&records, &readings, &weights)),
        None => print_line(&mycelium::hindsight::judge(&records, &readings)),
    }
}

/// Every line of the day files in `folder` from `since` on that reads as
/// a `T`, in day order; a folder not yet made holds none, and a line that
/// doesn't read is passed over, as one cut short by a crash would be.
fn kept_lines<T: serde::de::DeserializeOwned>(
    folder: &Path,
    since: jiff::civil::Date,
) -> io::Result<Vec<T>> {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut days: Vec<(jiff::civil::Date, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let day = path
                .file_name()?
                .to_str()?
                .strip_suffix(".jsonl")?
                .parse()
                .ok()?;
            Some((day, path))
        })
        .filter(|&(day, _)| day >= since)
        .collect();
    days.sort();
    let mut lines = Vec::new();
    for (_, path) in days {
        let text = std::fs::read_to_string(path)?;
        lines.extend(
            text.lines()
                .filter_map(|line| serde_json::from_str(line).ok()),
        );
    }
    Ok(lines)
}

/// Reads the realm's balance from the node's machines and sessions.
fn balance_read(socket: &Path) -> io::Result<mycelium::balance::Reading> {
    let mut client = Client::connect_within(socket, HEALTH_WITHIN)?;
    let Response::Machines(machines) = client.ask(&Request::Machines)?.map_err(io::Error::other)?
    else {
        return Err(io::Error::other(
            "the node answered machines with something else",
        ));
    };
    let Response::Sessions(sessions) = client.ask(&Request::Sessions)?.map_err(io::Error::other)?
    else {
        return Err(io::Error::other(
            "the node answered sessions with something else",
        ));
    };
    Ok(mycelium::balance::read(
        &machines,
        &sessions,
        jiff::Timestamp::now(),
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
/// unless it isn't one or `NO_COLOR` is set, with the theme's marks.
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
        shapes: mycelium::swarm::Shapes::read(),
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
    if asked.watching.watch {
        return watched(|| drawn_networks(socket, realm, asked));
    }
    if asked.output.json() {
        let mycelium = networks::read(socket, realm, SWARM_WITHIN)?;
        let name = asked.words.join(" ");
        if asked.counts {
            let counted = |network: &networks::Network| {
                let mut line = serde_json::json!(network);
                if let Some(fields) = line.as_object_mut() {
                    fields.remove("cells");
                }
                line
            };
            if name.is_empty() {
                return mycelium
                    .networks
                    .iter()
                    .map(counted)
                    .try_for_each(|line| print_line(&line));
            }
            let network = mycelium.named_network(&name).map_err(not_found)?;
            return print_line(&counted(network));
        }
        if name.is_empty() {
            return mycelium.lines().iter().try_for_each(print_line);
        }
        let network = mycelium.named_network(&name).map_err(not_found)?;
        print_line(network)?;
        let cells = network.cells.iter().filter_map(|cell| mycelium.cell(cell));
        return cells.into_iter().try_for_each(print_line);
    }
    let shown = drawn_networks(socket, realm, asked)?;
    writeln!(io::stdout().lock(), "{shown}")
}

/// Every network, or the one `asked` names, as [`mycelium::networks`]
/// draws them to this terminal now.
fn drawn_networks(socket: &Path, realm: &str, asked: &NetworksAsked) -> io::Result<String> {
    use mycelium::networks;
    let mycelium = networks::read(socket, realm, SWARM_WITHIN)?;
    let name = asked.words.join(" ");
    let look = look();
    if name.is_empty() {
        return Ok(networks::overview(&mycelium, &look, asked.cells, asked.all));
    }
    networks::network(&mycelium, &name, &look, asked.all).map_err(not_found)
}

/// Lists every cell, or says the one `asked` names, a machine as
/// `machines NAME` says it, or prints each as a JSON line.
fn cells(socket: &Path, realm: &str, asked: &CellsAsked) -> io::Result<()> {
    use mycelium::networks::{self, Is};
    if asked.watching.watch {
        return watched(|| drawn_cells(socket, realm, asked));
    }
    if asked.output.json() {
        let mycelium = networks::read(socket, realm, SWARM_WITHIN)?;
        let name = asked.words.join(" ");
        if name.is_empty() {
            let cells = mycelium.cells.iter();
            let standing = &asked.standing;
            return cells
                .filter(|cell| standing.is_empty() || cell.stands_as(standing))
                .try_for_each(print_line);
        }
        let cell = mycelium.named_cell(&name).map_err(not_found)?;
        if cell.is == Is::Machine {
            let machines = Client::connect(socket)?.ask(&Request::Machines)?;
            return print_line(&one_machine(machines, Vec::new(), &cell.name)?);
        }
        return print_line(cell);
    }
    let shown = drawn_cells(socket, realm, asked)?;
    writeln!(io::stdout().lock(), "{shown}")
}

/// Every cell, or the one `asked` names, as [`mycelium::networks`] draws
/// them to this terminal now; a machine as `machines NAME` says it.
fn drawn_cells(socket: &Path, realm: &str, asked: &CellsAsked) -> io::Result<String> {
    use mycelium::networks::{self, Is};
    let mycelium = networks::read(socket, realm, SWARM_WITHIN)?;
    let name = asked.words.join(" ");
    if name.is_empty() {
        return Ok(networks::cells(
            &mycelium,
            &look(),
            asked.all,
            &asked.standing,
        ));
    }
    let cell = mycelium.named_cell(&name).map_err(not_found)?;
    if cell.is == Is::Machine {
        let machines = Client::connect(socket)?.ask(&Request::Machines)?;
        return serde_json::to_string(&one_machine(machines, Vec::new(), &cell.name)?)
            .map_err(io::Error::other);
    }
    Ok(networks::cell(&mycelium, cell, &look()))
}

/// How often `--watch` draws again.
const WATCH_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// Draws what `draw` makes in place, again every [`WATCH_EVERY`], until
/// stopped or the terminal goes; a drawing that fails says why where it
/// would have been, and is tried again, as a node restarting comes back.
/// Refused where stdout is no terminal, which would take its escapes
/// every two seconds forever; and clap refuses it with `-o json`, since
/// a program asks again when it wants.
fn watched(mut draw: impl FnMut() -> io::Result<String>) -> io::Result<()> {
    use std::io::IsTerminal as _;
    if !io::stdout().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--watch redraws a terminal; without one, ask again when you want",
        ));
    }
    loop {
        let shown = draw().unwrap_or_else(|why| format!("mycelium: {why}; trying again"));
        let rows = rustix::termios::tcgetwinsize(io::stdout())
            .ok()
            .filter(|size| size.ws_row > 0)
            .map_or(usize::MAX, |size| usize::from(size.ws_row));
        let mut out = io::stdout().lock();
        out.write_all(in_place(&shown, rows).as_bytes())?;
        out.flush()?;
        drop(out);
        std::thread::sleep(WATCH_EVERY);
    }
}

/// `shown` drawn over what the terminal shows, from its top: each line
/// clears what was after it, and what was below the last is cleared, so
/// nothing blinks. It's cut to `rows` less one, so it never scrolls.
fn in_place(shown: &str, rows: usize) -> String {
    let mut framed = String::from("\x1b[H");
    for line in shown.lines().take(rows.saturating_sub(1).max(1)) {
        framed.push_str(line);
        framed.push_str("\x1b[K\n");
    }
    framed.push_str("\x1b[J");
    framed
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

/// Shares `quest`, and if told `to`, waits as long as `within` seconds for
/// that machine to hold it, printing what it found as a JSON line whatever
/// it was, and failing with why unless held.
fn share(sharing: Sharing, realm: String, socket: &Path) -> io::Result<()> {
    let Sharing {
        here,
        quest,
        scratch,
        to,
        within,
        ..
    } = sharing;
    let (quest, scratch) = (quest.as_str(), scratch.as_slice());
    let (_, membership, shares) = open(here, realm)?;
    let Some(to) = to.as_deref() else {
        println!("{}", shares.offer(quest, scratch)?);
        return Ok(());
    };
    let machine = membership.machine;
    let reached = if to == machine {
        Ok(Reached::refused(
            quest,
            to,
            format!("{to} is this machine, so {quest} is there already"),
        ))
    } else {
        shares.offer(quest, scratch).and_then(|_| {
            reached::reach(
                &shares,
                quest,
                (&machine, to),
                std::time::Duration::from_secs(within),
                &mut reached::Node::new(socket.to_owned()),
            )
        })
    };
    let reached = reached.unwrap_or_else(|error| Reached::refused(quest, to, error.to_string()));
    print_line(&reached)?;
    match reached.why {
        Some(why) if !reached.held => Err(io::Error::other(why)),
        _ => Ok(()),
    }
}

/// Carries `quest` from `machine`, as its node names it, to the machine
/// `to`, as the person or, given `why`, as its orchestrator, a JSON line
/// on standard output for each step as it goes and the last saying where
/// each session went.
fn carry(
    socket: &Path,
    shares: Places,
    (quest, machine, to): (&str, &str, &str),
    why: Option<&str>,
) -> io::Result<()> {
    let mut hands = Through::new(socket.to_owned(), shares, quest, carry::through::PATIENCE);
    let mut failed = None;
    let carried = carry::carry(&mut hands, quest, machine, to, why, &mut |line| {
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
    let start = Start {
        work: asked.work.into(),
        folder: said_within_home(asked.folder)?,
        command: asked.command,
        may_cost: asked.may_cost,
    };
    print_reply(socket, &Request::Start(start))
}

/// `folder` as another machine can find it: within home when it is under
/// this one's, since another machine's home is elsewhere: /home/joel here
/// is /Users/joel on a Mac.
fn said_within_home(folder: Option<PathBuf>) -> io::Result<Option<PathBuf>> {
    let folder = folder.map(std::path::absolute).transpose()?;
    Ok(match (folder, std::env::home_dir()) {
        (Some(folder), Some(home)) => match folder.strip_prefix(&home) {
            Ok(within) => Some(within.to_owned()),
            Err(_) => Some(folder),
        },
        (folder, _) => folder,
    })
}

/// What `mycelium run` ends with when its command couldn't run, as SSH
/// does.
const DIDNT_RUN: u8 = 255;

/// Asks the node to run `command` where placement says `work` should go,
/// writing what it writes as it comes, and ends as it ended.
fn run_there(socket: &Path, asked: Running) -> io::Result<ExitCode> {
    let run = Run {
        work: asked.work.into(),
        folder: said_within_home(asked.folder)?,
        command: asked.command,
        // The node judges who asks, and where from.
        by: None,
        from: None,
    };
    let mut client = Client::connect(socket)?;
    match client.ask(&Request::Run(run))? {
        Ok(Response::Running { .. }) => {}
        Ok(other) => return Err(io::Error::other(format!("the node answered {other:?}"))),
        Err(why) => {
            eprintln!("mycelium: {why}");
            return Ok(ExitCode::from(DIDNT_RUN));
        }
    }
    let piece = |said: &str| {
        mycelium::run::bytes(said)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    };
    loop {
        match client.next_ran()? {
            Some(Ran::Out(said)) => {
                let mut out = io::stdout().lock();
                out.write_all(&piece(&said)?)?;
                out.flush()?;
            }
            Some(Ran::Err(said)) => io::stderr().lock().write_all(&piece(&said)?)?,
            Some(Ran::Exited(code)) => {
                return Ok(ExitCode::from(u8::try_from(code).unwrap_or(DIDNT_RUN)));
            }
            Some(Ran::Failed(why)) => {
                eprintln!("mycelium: {why}");
                return Ok(ExitCode::from(DIDNT_RUN));
            }
            None => {
                eprintln!("mycelium: the node hung up before the run ended");
                return Ok(ExitCode::from(DIDNT_RUN));
            }
        }
    }
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

/// Holds `command` here as the session `id`, in `folder`, until its
/// scope's stop tells it to end, asking the node at `node` for a turn
/// first when it resumes a session.
fn hold_here(
    realm: &str,
    id: &str,
    folder: &Path,
    login: bool,
    command: &[String],
    node: &Path,
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
        node: Some(node),
    })
}

/// The words a held program starts inside: a login shell, for a hold a
/// node starts, or none for a hold from a terminal, which has the
/// person's environment already.
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

/// Joins what `asked` names, on another machine or through a pi host
/// here, asking the node at `socket` whether one elsewhere has ended; a
/// session held here is read as its tree instead.
fn attach_asked(realm: &str, socket: &Path, asked: Attaching) -> io::Result<ExitCode> {
    let Attaching {
        id,
        host,
        session,
        on,
        words,
        first_wait,
    } = asked;
    let target = match (id, host, session) {
        (Some(id), _, _) => rejoin::Target::Held(id),
        (None, Some(host), Some(conversation)) => rejoin::Target::Hosted { host, conversation },
        _ => unreachable!("an attach names a held id or a host and a session"),
    };
    match (on, target) {
        (Some(on), target) => {
            let waits = rejoin::Waits {
                first: first_wait.map_or(rejoin::Waits::default().first, |first| {
                    std::time::Duration::from_millis(first)
                }),
                ..rejoin::Waits::default()
            };
            attach_on(realm, socket, &on, target, words, waits)
        }
        (None, rejoin::Target::Hosted { host, conversation }) => {
            attach_hosted(realm, &host, &conversation)
        }
        (None, rejoin::Target::Held(_)) => unreachable!("a held session here is read as its tree"),
    }
}

/// Joins `target` on the machine `on` from this terminal by `words`, or
/// else SSH to it by its name, again each time its way drops, asking the
/// node at `socket` whether it has ended, until it ends otherwise.
fn attach_on(
    realm: &str,
    socket: &Path,
    on: &str,
    target: rejoin::Target,
    words: Vec<String>,
    waits: rejoin::Waits,
) -> io::Result<ExitCode> {
    let words = if words.is_empty() {
        let joins = match &target {
            rejoin::Target::Held(id) => hold::start::attach_here(realm, id),
            rejoin::Target::Hosted { host, conversation } => {
                hosted_attach::attach_here(realm, host, conversation)
            }
        };
        let ssh = Ssh {
            destination: on.to_owned(),
            options: Vec::new(),
            command: "mycelium".to_owned(),
        };
        ssh.there(&joins)
    } else {
        words
    };
    let mut over = rejoin::Over {
        words,
        node: socket.to_owned(),
        on: on.to_owned(),
        target,
    };
    let runtime = runtime()?;
    let ended = runtime.block_on(async {
        // Heard from here on, so Ctrl+C while it waits ends it as it says.
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        let interrupted = async move {
            interrupt.recv().await;
        };
        rejoin::rejoin(on, &mut over, waits, interrupted).await
    })?;
    Ok(ExitCode::from(ended))
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
    let writer = place::machine(asked.machine.as_deref(), &place::state())?;
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

/// Says each cell's forage the realm's nodes serve, or the one `asked`
/// names, drawn for a person or a JSON line each.
fn forage(socket: &Path, asked: &ForageAsked) -> io::Result<()> {
    use mycelium::forage;
    let records = mycelium::swarm::records(socket, forage::KIND, SWARM_WITHIN)?;
    let chosen = forage::chosen(records, asked.cell.as_deref());
    if asked.output.json() {
        return chosen.iter().try_for_each(print_line);
    }
    write!(
        io::stdout().lock(),
        "{}",
        forage::draw(&chosen, asked.cell.as_deref())
    )
}

/// Prints each record of `kind` the node holds, a JSON line each, oldest
/// first.
fn print_records(socket: &Path, kind: &str) -> io::Result<()> {
    let mut records = mycelium::swarm::records(socket, kind, SWARM_WITHIN)?;
    records.sort_by_key(|record| record["heard"].as_i64());
    records.iter().try_for_each(print_line)
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

/// A reason that says something.
fn reason(said: &str) -> Result<String, String> {
    if said.trim().is_empty() {
        Err("a reason says something, and a blank one says nothing".to_owned())
    } else {
        Ok(said.to_owned())
    }
}

fn intent(name: &str) -> Result<Intent, String> {
    serde_json::from_value(serde_json::Value::String(name.to_owned()))
        .map_err(|_| format!("{name:?} is not hold, question, finding, fyi or pivot"))
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory as _, Parser as _};

    use std::io;

    use super::{
        Action, Command, HELP_WIDTH, SwarmAsked, Tree, in_place, reader_left, swarm_asked, tree,
    };

    #[test]
    fn a_reader_that_left_ends_the_answer_quietly_and_only_it() {
        let broken = io::Error::from(io::ErrorKind::BrokenPipe);
        let (reader, gone) = io::pipe().expect("a pipe");
        drop(reader);
        let (_reading, read) = io::pipe().expect("a pipe");
        assert_eq!(
            (
                reader_left(&broken, &gone),
                reader_left(&broken, &read),
                reader_left(&io::Error::other("refused"), &gone),
            ),
            (true, false, false),
            "quiet only for a broken pipe whose reader has gone, not the node hanging up"
        );
    }

    #[test]
    fn a_watch_draws_over_the_last_drawing_and_never_scrolls() {
        assert_eq!(
            in_place("one\ntwo\nthree", 3),
            "\x1b[Hone\x1b[K\ntwo\x1b[K\n\x1b[J",
            "from the top, each line clearing its old tail, cut to the rows less one"
        );
        assert_eq!(
            in_place("one", usize::MAX),
            "\x1b[Hone\x1b[K\n\x1b[J",
            "whole where there's no terminal height to cut to"
        );
    }

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
    fn every_command_says_one_plain_line_and_an_example_that_parses() {
        let mut command = Command::command();
        command.build();
        let mut commands = vec![(String::new(), command)];
        let mut unsaid = Vec::new();
        while let Some((above, each)) = commands.pop() {
            if each.is_hide_set() || each.get_name() == "help" {
                continue;
            }
            let name = format!("{above} {}", each.get_name()).trim().to_owned();
            let about = each
                .get_about()
                .map(ToString::to_string)
                .unwrap_or_default();
            if about.is_empty() || about.contains('\n') || about.chars().count() > HELP_WIDTH {
                unsaid.push(format!("{name}: its first line is {about:?}"));
            }
            let after = each.get_after_help().map(ToString::to_string);
            match after
                .as_deref()
                .and_then(|help| help.strip_prefix("Example:\n"))
            {
                None => unsaid.push(format!("{name}: no example")),
                Some(example) => {
                    let line = example.replace("\\\n", " ");
                    let typed = line.rsplit("| ").next().unwrap_or_default();
                    let words: Vec<&str> = typed
                        .split_whitespace()
                        .map(|word| word.trim_matches('\''))
                        .collect();
                    let named = name.split(' ').skip(1).all(|part| words.contains(&part));
                    if words.first() != Some(&"mycelium") || !named {
                        unsaid.push(format!("{name}: its example is {example:?}"));
                    } else if let Err(error) = Command::try_parse_from(&words) {
                        unsaid.push(format!("{name}: {example:?} doesn't parse: {error}"));
                    }
                }
            }
            commands.extend(
                each.get_subcommands()
                    .map(|sub| (name.clone(), sub.clone())),
            );
        }
        assert!(unsaid.is_empty(), "{unsaid:#?}");
    }

    #[test]
    fn mycelium_alone_lists_its_commands_whatever_realm_is_set() {
        for line in [&["mycelium"][..], &["mycelium", "--realm", "work"]] {
            let asked = Command::try_parse_from(line)
                .unwrap_or_else(|error| panic!("{line:?} parses: {error}"));
            assert!(asked.action.is_none(), "{line:?} names no command");
        }
        let said = Command::command().render_help().to_string();
        assert!(
            said.lines().any(|line| {
                line.strip_prefix("  networks ").map(str::trim_start)
                    == Some("Draws the networks as a tree, or one network")
            }),
            "{said}"
        );
    }

    #[test]
    fn a_carry_is_ordered_only_with_a_reason_that_says_something() {
        let carrying = |why: &str| {
            Command::try_parse_from([
                "mycelium", "--realm", "work", "carry", "QEST-1", "--to", "tern", "--why", why,
            ])
        };
        for blank in ["", "  "] {
            assert!(carrying(blank).is_err(), "{blank:?} is refused");
        }
        assert!(carrying("it needs the camera").is_ok(), "a reason is taken");
    }

    #[test]
    fn a_watch_is_drawn_text_never_json() {
        for words in ["networks -w -o json", "cells --watch --json"] {
            let error = Command::try_parse_from(
                ["mycelium", "--realm", "work"]
                    .into_iter()
                    .chain(words.split(' ')),
            )
            .err()
            .expect("a watch of JSON is refused");
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ArgumentConflict,
                "{words}"
            );
        }
        assert!(
            Command::try_parse_from(["mycelium", "--realm", "work", "cells", "-w", "-o", "text"])
                .is_err(),
            "even -o text, said with it, is refused: -w is text already"
        );
        assert!(
            Command::try_parse_from(["mycelium", "--realm", "work", "networks", "-w"]).is_ok(),
            "a watch alone is asked"
        );
    }

    #[test]
    fn a_command_missing_its_own_says_which_it_misses() {
        let error = Command::try_parse_from(["mycelium", "--realm", "work", "web", "QEST-1"])
            .err()
            .expect("web needs its act");
        assert_eq!(error.kind(), clap::error::ErrorKind::MissingSubcommand);
        let said = error.render().to_string();
        assert!(said.contains("Usage: mycelium web "), "{said}");
    }

    #[test]
    fn networks_and_cells_take_their_singular_and_either_json() {
        let parsed = |words: &[&str]| {
            let line = ["mycelium", "--realm", "work"];
            Command::try_parse_from(line.iter().chain(words))
                .unwrap_or_else(|error| panic!("{words:?} parses: {error}"))
                .action
                .unwrap_or_else(|| panic!("{words:?} names a command"))
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
        parsed
            .action
            .as_ref()
            .and_then(tree)
            .unwrap_or_else(|| panic!("{words:?} is a tree"))
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
                "machine tern",
                "machines tern",
                Tree::Machine("tern".to_owned()),
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
                    .map(|line| line.action.as_ref().and_then(tree)),
                Ok(None)
            ),
            "a pi host's conversation is joined outside the tree"
        );
        assert!(
            matches!(
                Command::try_parse_from(["m", "attach", "--on", "skiff", "h0"])
                    .map(|line| line.action.as_ref().and_then(tree)),
                Ok(None)
            ),
            "and a session elsewhere is joined there"
        );
        assert!(
            Command::try_parse_from(["m", "attach", "h0", "--", "ssh"]).is_err(),
            "words to join by are for a session elsewhere"
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
            Some(Action::Swarm { words, output, .. }) => (swarm_asked(&words), output.json()),
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
                Ok(Some(Action::Swarm { names: true, .. }))
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
