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
    /// Start a session of new work, held in no window, as `mycelium
    /// sprout` says: the node finds its folder as the asker would see it,
    /// and starts it there as a [`Start`] would.
    Sprout(Sprout),
    /// Run a command where placement chooses, as the node's own: answered
    /// with [`Response::Running`], then a [`Ran`] a line for as long as it
    /// runs, ending with its exit or why it couldn't run. A client that
    /// hangs up stops it.
    Run(Run),
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
    /// start more: stopping or resuming them is taken only from the
    /// person's own login, listing from anyone.
    Starts {
        /// What to do.
        act: StartsAct,
    },
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
    /// What this node asks of `machine`, handed to the connection one ask
    /// a line for as long as it stays, each answered on a line of its own:
    /// a link `machine` dialled, which runs outside the node, carries them.
    Carry {
        /// The machine the link reaches.
        machine: String,
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
    /// there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
    /// The program and its arguments, run through the login shell there.
    pub command: Vec<String>,
    /// Whether a machine may be made for it from a provider's offer, which
    /// may cost money, where no machine of the realm takes it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub may_cost: bool,
}

/// New work to start a session on: the session's first words come from
/// `words`, a link or what the work is, and its folder is the one named,
/// the checkout a link's repository has under home, or where the asker
/// is, judged as the asker could reach it and never home itself. It
/// starts on `on`, or this machine, and never on one that costs money.
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
    /// What the node's providers offer.
    Offers(Offers),
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
    /// The message as sent.
    Sent(Message),
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
        /// The message.
        message: Message,
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
}
