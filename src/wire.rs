//! The node's local socket, in niri's shape, through which pi, Quickshell
//! and a terminal reach it: a client writes one request as a JSON line and
//! reads one reply as a JSON line. A `Watch` reply is followed by events,
//! one per line, for as long as the client stays: first the whole state,
//! then each change. A `Present` connection is held open by its process
//! for as long as it runs.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::declared::Kind;
use crate::facts::Machine;
use crate::flow::{Crossing, Refusal};
use crate::group::{Group, Pattern};
use crate::offer::Offer;
use crate::placement::{Placement, Work};
use crate::record::{Intent, Message, Session};

/// What a client asks of the node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Every session the node knows of, on this machine and others.
    Sessions,
    /// What this machine is, as its node measures it now.
    Machine,
    /// What every machine of the realm is: this one measured now, then
    /// each other one as its links last carried it.
    Machines,
    /// The builders this machine sends Nix builds to, which are no
    /// machines of the realm, each as it was last heard.
    Builders,
    /// The cell this node's book says started each session held on this
    /// machine under the ids given, for an end of a link to say beside
    /// their presences, as one SSH started runs apart from the node.
    StartedBy(Vec<String>),
    /// The shapes of machine this node's providers' programs could make,
    /// each asked now.
    Offers,
    /// Which machine of the realm `work` should go to.
    Place(Work),
    /// Which machine of the realm `work` should go to, for an asker that
    /// starts it itself where the answer is this machine, and otherwise
    /// through [`Request::Start`]: an answer naming this machine counts
    /// as a start placed here, so a burst of such asks spreads.
    PlaceToStart(Work),
    /// Start a program held where placement chooses.
    Start(Start),
    /// Where [`Request::Start`] would place each of a carry's starts,
    /// starting and counting nothing, or the refusal one would answer:
    /// asked before a carry tells any session to end here. A variant of
    /// its own, since a node too old to know it refuses it, where one
    /// would start a start whose field it didn't know.
    Takes(Taking),
    /// Start a program here that the node of the linked machine `from`
    /// was asked to start, has judged, counted and booked, and placed
    /// here, so it starts here alone, where this machine's facts as they
    /// are now still take it. Only this node's own process, or one judged
    /// the person's, as a link's end is, relays it. From any other asker
    /// it is taken as an agent's start: counted and limited here, started
    /// here or nowhere, and never read for a carry. Where a person works,
    /// one marked as the person's carry is refused instead.
    Relayed {
        /// The machine whose node was asked.
        from: String,
        /// What it was asked to start.
        start: Start,
    },
    /// Start a session of new work, held in no window, as `mycelium
    /// sprout` says: the node finds its folder as the asker would see it,
    /// and starts it there as a [`Start`] would.
    Sprout(Sprout),
    /// The places work can begin in on this machine, as the asker sees
    /// its disk: answered with [`Response::Places`].
    Places {},
    /// Run a command where placement chooses, as the node's own: answered
    /// with [`Response::Running`], then a [`Ran`] a line for as long as it
    /// runs, ending with its exit or why it couldn't run. A client that
    /// hangs up stops it.
    Run(Run),
    /// Borrow a model call from the machine this one lends from, through
    /// the link reaching it: answered with a [`crate::lend::Lent`] a line,
    /// with no reply first, until it ends. A client that hangs up lets it
    /// go.
    Lend(crate::lend::Lend),
    /// Put what the person says into a session's steer inbox, on the one
    /// machine running the session that goes by `address`, in the
    /// harness's own shape: text, act, at, from and id.
    Steer {
        /// The session's address.
        address: String,
        /// What is said, as the launcher writes it.
        steer: serde_json::Value,
    },
    /// End the session held as `held` for good, on the machine holding
    /// it: here, or through the link reaching that machine. Only the
    /// person's own login ends one, or, on the machine where both run,
    /// the session the node's record proves orchestrates it.
    End {
        /// Its held id, as `holds` lists it.
        held: String,
    },
    /// The latest messages to the groups `groups` hears, oldest first.
    Messages {
        /// Which groups.
        groups: Pattern,
        /// At most this many.
        limit: usize,
    },
    /// Send a message on the mesh, as a session would, or into another
    /// realm by a flow, when it names one. A crossing is answered in
    /// [`Answer`]'s shape, since it may be refused.
    Send(Draft),
    /// The realms this realm's messages may go to, by its flows.
    Flows,
    /// Take a message another realm's node on this machine hands across
    /// by a flow, answered in [`Answer`]'s shape.
    Cross(Crossing),
    /// Update `machine` to its configuration's main, through its updater,
    /// and say how that ended. Nothing in it says what to build: the
    /// machine's configuration does. Another machine is asked only
    /// through a link that reaches it directly, and takes it only over a
    /// link whose key proved which machine asked.
    Update {
        /// The machine to update.
        machine: String,
    },
    /// Records that the session asking, as the kernel and pi's records
    /// prove it, heads the orchestrating quest `heads` and orchestrates
    /// `quests`, in place of what it said before; with neither, forgets
    /// what it said. Only that session hands a quest it holds on, so
    /// another claiming it is refused.
    Orchestrates {
        /// The orchestrating quest it has loaded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        heads: Option<String>,
        /// The quests it orchestrates.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        quests: Vec<String>,
    },
    /// The sessions this node started lately, and whether agents may
    /// start more: stopping, resuming and listing are each taken only
    /// from the person's own login, since a list names their folders.
    Starts {
        /// What to do.
        act: StartsAct,
    },
    /// What the session asking, as the kernel and pi's records prove it,
    /// orchestrates by the node's record, changing nothing. Its own
    /// request, so a node too old to know it refuses it rather than
    /// reading it as an `Orchestrates` naming nothing, which forgets.
    Orchestrating,
    /// Frees `quest` from the session holding it by the node's record of
    /// orchestrations, taken only from the person's own login.
    Free {
        /// The quest to free.
        quest: String,
    },
    /// Counts the connection's own process present for as long as it keeps
    /// the connection open, so a session it owns is judged ended the moment
    /// the connection closes, as the kernel closes it however the process
    /// ends. The process is the one the kernel says is at the other end, so
    /// no process can speak for another. It says nothing more.
    Present,
    /// A pi host's word that its child `pid`, started `started` clock
    /// ticks after boot as the host read it before handing it any command,
    /// is the pi-spawner it started for `session`, so a command asking
    /// below it asks for that session alone. Taken only from the host
    /// itself, as the kernel names the process at the other end, and only
    /// for a session it proves; see PROTOCOL.md's A Hosted Session's
    /// Spawner.
    Spawned {
        /// The spawner's process.
        pid: i32,
        /// When it started, in clock ticks since boot.
        started: u64,
        /// The session it serves.
        session: String,
    },
    /// What this node asks of `machine`, handed to the connection one ask
    /// a line for as long as it stays, each answered on a line of its own:
    /// a link `machine` dialled, which runs outside the node, carries them.
    Carry {
        /// The machine the link reaches.
        machine: String,
        /// Whether the link carries a reopening, which one older than
        /// reopenings can't read; a link that doesn't say carries none.
        #[serde(default)]
        reopens: bool,
    },
    /// This machine's own health as its health daemon says it, now and
    /// then each time a word moves, for as long as the connection stays:
    /// a link another machine dialled, which runs outside the node,
    /// carries it in this machine's facts.
    Health,
    /// A turn for the holder about to resume `session`, answered once it
    /// may start it: at once for a session no record names, else once
    /// fewer than a few resumed sessions are on their first turn. A
    /// holder that hangs up leaves the queue.
    Turn {
        /// The session its program resumes, by id.
        session: String,
    },
    /// The whole state, then each change, for as long as the client stays.
    Watch {
        /// Which groups' messages to send, or none when not given; sessions
        /// are always sent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        groups: Option<Pattern>,
        /// How many of the latest messages the first state holds, none when
        /// not given.
        #[serde(default)]
        recent: usize,
    },
}

/// A program to start held, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Start {
    /// What the machine it starts on must be.
    pub work: Work,
    /// The folder it starts in on that machine: a full path, or a path
    /// within its home, since only that machine knows where its home is.
    /// It starts in its home when this names none or names no folder
    /// there, unless `strict`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
    /// Whether a `folder` named that the machine hasn't refuses the start
    /// rather than beginning it at home, as a sprout's must, its folder
    /// being the work's. Left out when not, so an older node still reads
    /// the start, and begins it at home.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strict: bool,
    /// The program and its arguments, run through the login shell there.
    pub command: Vec<String>,
    /// Whether a machine may be made for it from a provider's offer, which
    /// may cost money, where no machine of the realm takes it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub may_cost: bool,
    /// Whether it brings in a session the person carried: set by the node
    /// first asked, which judged who asked, only for the person's start
    /// of a carry's command, and passed on as it relays it. What an asker
    /// says of it is never taken, so no agent's start is taken for a carry.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub carries: bool,
    /// The asker's own name for this one start, which it asks again
    /// under when no answer came: an ask under a name the same asker
    /// already asked joins that start, landing or landed, rather than
    /// starting and counting another. A node that knows no such name
    /// passes over it and starts each ask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub once: Option<String>,
}

/// The starts a carry would ask, for [`Request::Takes`]: each session's
/// arrival and its quest's own, as it would ask them were nobody to
/// order it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Taking {
    /// Each arrival, in the order the carry would start them.
    pub starts: Vec<Start>,
    /// Whether an orchestrator orders the carry, so an agent's arrival is
    /// weighed as starting uncounted, as the carry line its order keeps
    /// would free it. Only words: the start itself is held to that line.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ordered: bool,
}

/// New work to start a session on: the session's first words come from
/// `words`, a link or what the work is, and its folder is the one named,
/// the checkout a link's repository has under home, or where the asker
/// is, judged as the asker could reach it and never home itself. It
/// starts on `on`; else where placement chooses, where its folder is a
/// repository's own checkout under `~/src` fetching from a remote,
/// beginning here instead where the machine chosen hasn't it; here
/// otherwise; and never on one that costs money.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sprout {
    /// The work: a link alone, or words.
    pub words: Vec<String>,
    /// The repository's folder it works in, as the asker names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
    /// The cell it comes from, by address, which its quest is tied to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The machine it starts on, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    /// Where the asker says it is, which counts only for the person and
    /// only where the node can't see where the asker is itself, as on a
    /// Mac.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
}

/// A command to run, and where. It never makes a machine, since a run
/// is over long before one would link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    /// What the machine it runs on must be.
    pub work: Work,
    /// The folder it runs in there, as for [`Start`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
    /// The command and its arguments, run through the login shell there,
    /// with nothing on its standard input.
    pub command: Vec<String>,
    /// Who asks it, as the machine it was asked of judged them. A node
    /// takes what is said here only as stricter than what it judges, and
    /// a run from another machine that says nothing as a strictly fenced
    /// agent's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<By>,
    /// The machine it was asked of, for the record of runs, and so that a
    /// run asked elsewhere is told no session here (`PI_SESSION_ID`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// What is asked of a node's book of starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartsAct {
    /// Refuse every agent's start till the person resumes them.
    Stop,
    /// Let agents start again, within their limits.
    Resume,
    /// Only say how they stand.
    List,
}

/// Who asks a run: the person, or an agent and the fence it runs in.
/// Ordered from loosest to strictest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum By {
    /// The person's own login.
    Person,
    /// An agent, in this fence.
    Agent(Kind),
}

impl std::fmt::Display for By {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            By::Person => f.write_str("the person"),
            By::Agent(kind) => write!(f, "a {kind} agent"),
        }
    }
}

/// What a run says as it goes: its output as it comes, then how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ran {
    /// Bytes it wrote to its standard output, in base64.
    Out(String),
    /// Bytes it wrote to its standard error, in base64.
    Err(String),
    /// It ended with this exit code, or 128 and the signal that ended it.
    Exited(i32),
    /// It couldn't run, or its machine stopped answering, and why.
    Failed(String),
}

impl Ran {
    /// Whether nothing follows it.
    #[must_use]
    pub fn ends(&self) -> bool {
        matches!(self, Self::Exited(_) | Self::Failed(_))
    }
}

/// A program started held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Started {
    /// The machine it runs on.
    pub machine: String,
    /// Its held id there.
    pub held: String,
    /// The folder it began in there, which is its home when the one
    /// asked for isn't a folder there.
    #[serde(default)]
    pub folder: PathBuf,
    /// The words that join it, run from a terminal on this machine.
    pub attach: Vec<String>,
}

/// A message as a client writes it; the node gives it its id and time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    /// Who sends it: an address, or a person's name.
    pub from: String,
    /// The groups it goes to.
    pub groups: Vec<Group>,
    /// What it asks of those who hear it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<Intent>,
    /// What is said.
    pub text: String,
    /// The message it answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub re: Option<String>,
    /// The realm it is for, by a flow, if not this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realm: Option<String>,
}

/// The node's answer to one request.
pub type Reply = Result<Response, String>;

/// The node's answer to a request that may be refused by a flow: a
/// [`Reply`]'s `Ok` or `Err`, or `Refused`, with a code a sender tells
/// apart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Answer {
    /// Done, as a reply's `Ok` says it.
    Ok(Response),
    /// Failed, as a reply's `Err` says it.
    Err(String),
    /// Refused by a flow, and why.
    Refused(Refusal),
}

impl From<Reply> for Answer {
    fn from(reply: Reply) -> Self {
        match reply {
            Ok(response) => Self::Ok(response),
            Err(why) => Self::Err(why),
        }
    }
}

impl From<Refusal> for Answer {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

/// What a request is answered with when it succeeds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    /// The sessions asked for.
    Sessions(Vec<Session>),
    /// This machine, boxed since it is far larger than any other answer.
    Machine(Box<Machine>),
    /// Every machine, this one first.
    Machines(Vec<Machine>),
    /// The builders this machine sends Nix builds to.
    Builders(Vec<crate::builders::Builder>),
    /// The starter of each held id asked about that the book names one
    /// for, by held id; an id it names none for is left out.
    StartedBy(BTreeMap<String, String>),
    /// What the node's providers offer.
    Offers(Offers),
    /// The places work can begin in ([`crate::places::on`]).
    Places(Vec<crate::places::Place>),
    /// Where the work asked about goes.
    Placement(Placement),
    /// The program asked for, started.
    Started(Started),
    /// The command asked for is running on `machine`; [`Ran`] lines follow.
    Running {
        /// The machine it runs on.
        machine: String,
    },
    /// The steer asked for, in its session's inbox on `machine`. A talk's
    /// replies follow in its replies file on this machine, as they would
    /// for a session here.
    Steered {
        /// The machine the session runs on.
        machine: String,
        /// Who the node proved ordered it, for an `ordered-carry`: the
        /// orchestrator's address, which the carried session arrives as
        /// carried by.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<String>,
        /// Whether the node carries the rest of this `ordered-carry` on
        /// itself, once the session has carried and ended, so the command
        /// that asked may end without leaving it half done. A node too
        /// old to say leaves the rest to the command, as before.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        carries_on: bool,
        /// Whether it carries on, with this one, every session the steer
        /// named `with`, told the same carry before it, so the command
        /// that told them all has none left to carry. A node too old to
        /// say carries on this one alone, as `carries_on` says.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        carries_with: bool,
    },
    /// The held session asked for, ended on `machine`.
    Ended {
        /// The machine it was held on.
        machine: String,
        /// Its held id.
        held: String,
        /// Whether it went within its grace or was killed.
        ending: crate::hold::end::Ending,
    },
    /// The messages asked for.
    Messages(Vec<Message>),
    /// The message as sent, boxed since a message is far larger than any
    /// other answer.
    Sent(Box<Message>),
    /// The realms this realm's messages may go to.
    Flows {
        /// Each realm, by name.
        to: Vec<String>,
    },
    /// How the update asked for ended, as the machine's updater said it:
    /// one JSON object naming its `step`, such as `done` or `refused`, and
    /// the commit the machine runs after it.
    Updated(serde_json::Value),
    /// How the person's carry asked for ended: the carry's last line,
    /// naming the machine it went to and each session started there.
    Carried(serde_json::Value),
    /// The orchestrations asked for, recorded.
    Orchestrated,
    /// What the session asking orchestrates by the node's record: the
    /// quest it heads and those below it, or none where it claims none.
    Orchestrating {
        /// The orchestrating quest it heads.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        heads: Option<String>,
        /// The quests it orchestrates.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        quests: Vec<String>,
    },
    /// Whether agents' starts are stopped, after the act asked, and the
    /// newest starts the node keeps, newest last, as its book writes them.
    Starts {
        /// Whether the person has stopped agents starting sessions.
        stopped: bool,
        /// The newest starts.
        newest: Vec<serde_json::Value>,
    },
    /// Events follow, one per line.
    Watching,
    /// The process is counted present until it hangs up.
    Present,
    /// The spawner said, registered for its session.
    Spawned,
    /// Asks follow, one per line, each to be answered on a line back.
    Carrying,
    /// This machine's own health, or none while it is unknown: one at
    /// first, and another each time it moves.
    Health(Option<crate::own_health::Health>),
    /// The session asked about may start now.
    Turn,
}

/// What a node's providers' programs offer, and why any couldn't say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offers {
    /// Every shape offered, by each program in the order it was given.
    pub offers: Vec<Offer>,
    /// Why a program couldn't say what it offers, by the program, so
    /// none of them is taken as offering nothing.
    pub unanswered: BTreeMap<String, String>,
}

/// A change, as a watch sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    /// Every session, replacing what the client held.
    SessionsChanged {
        /// All of them.
        sessions: Vec<Session>,
    },
    /// One session, new or changed.
    SessionChanged {
        /// As it now stands, boxed since a session is far larger than
        /// any other event.
        session: Box<Session>,
    },
    /// A session is gone from the mesh.
    SessionForgotten {
        /// Its id.
        id: String,
    },
    /// The latest messages, replacing what the client held.
    MessagesChanged {
        /// Oldest first.
        messages: Vec<Message>,
    },
    /// A message went to a group the watch hears.
    Message {
        /// The message, boxed so each event a watcher's backlog holds is
        /// no larger than a session's.
        message: Box<Message>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_watch_naming_no_groups_hears_only_sessions() {
        let sessions_only = Request::Watch {
            groups: None,
            recent: 0,
        };
        for said in [
            r#"{"Watch":{}}"#,
            r#"{"Watch":{"groups":null}}"#,
            r#"{"Watch":{"recent":0}}"#,
        ] {
            assert_eq!(
                serde_json::from_str::<Request>(said).expect("a watch reads"),
                sessions_only,
                "{said}"
            );
        }
        assert_eq!(
            serde_json::to_string(&sessions_only).expect("a watch writes"),
            r#"{"Watch":{"recent":0}}"#,
            "a watch naming no groups says none"
        );
        assert!(
            serde_json::from_str::<Request>(r#"{"Watch":{"groups":[]}}"#).is_err(),
            "a pattern is one string, never a list"
        );
    }

    #[test]
    fn a_spawner_is_said_and_answered_as_the_protocol_writes_it() {
        let said = r#"{"Spawned":{"pid":82,"started":5,"session":"s-one"}}"#;
        assert_eq!(
            serde_json::from_str::<Request>(said).expect("a spawner's word reads"),
            Request::Spawned {
                pid: 82,
                started: 5,
                session: "s-one".to_owned(),
            },
            "{said}"
        );
        assert_eq!(
            serde_json::to_string(&Response::Spawned).expect("its answer writes"),
            r#""Spawned""#,
            "the answer is the bare word"
        );
    }

    #[test]
    fn a_start_not_strict_says_nothing_of_it_so_an_older_node_reads_it() {
        let said = r#"{"work":{},"folder":"src/skiff","command":["pi"]}"#;
        let start = serde_json::from_str::<Start>(said).expect("a start reads");
        assert!(!start.strict, "a start that doesn't say is not strict");
        assert!(
            !serde_json::to_string(&start)
                .expect("a start writes")
                .contains("strict"),
            "one not strict is written as before it was said"
        );
        let strict = Start {
            strict: true,
            ..start
        };
        let written = serde_json::to_string(&strict).expect("a start writes");
        assert!(written.contains(r#""strict":true"#), "{written}");
        assert_eq!(
            serde_json::from_str::<Start>(&written).expect("a strict start reads"),
            strict
        );
        // A field this end doesn't know is passed over, as an older end
        // passes over strict.
        let later = r#"{"work":{},"folder":"src/skiff","command":["pi"],"said_later":true}"#;
        assert!(
            serde_json::from_str::<Start>(later).is_ok(),
            "a start saying more than this end knows still reads"
        );
    }
}
