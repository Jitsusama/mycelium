//! The node's local socket, in niri's shape, through which pi, Quickshell
//! and a terminal reach it: a client writes one request as a JSON line and
//! reads one reply as a JSON line. A `Watch` reply is followed by events,
//! one per line, for as long as the client stays: first the whole state,
//! then each change. A `Present` connection is held open by its process
//! for as long as it runs.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::facts::Machine;
use crate::group::{Group, Pattern};
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
    /// Which machine of the realm `work` should go to.
    Place(Work),
    /// Start a program held where placement chooses.
    Start(Start),
    /// Put what the person says into a session's steer inbox, on the one
    /// machine running the session that goes by `address`, in the
    /// harness's own shape: text, act, at, from and id.
    Steer {
        /// The session's address.
        address: String,
        /// What is said, as the launcher writes it.
        steer: serde_json::Value,
    },
    /// The latest messages to the groups `groups` hears, oldest first.
    Messages {
        /// Which groups.
        groups: Pattern,
        /// At most this many.
        limit: usize,
    },
    /// Send a message on the mesh, as a session would.
    Send(Draft),
    /// Counts the connection's own process present for as long as it keeps
    /// the connection open, so a session it owns is judged ended the moment
    /// the connection closes, as the kernel closes it however the process
    /// ends. The process is the one the kernel says is at the other end, so
    /// no process can speak for another. It says nothing more.
    Present,
    /// The whole state, then each change, for as long as the client stays.
    Watch {
        /// Which groups' messages to send; sessions are always sent.
        groups: Pattern,
        /// How many of the latest messages the first state holds.
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
}

/// The node's answer to one request.
pub type Reply = Result<Response, String>;

/// What a request is answered with when it succeeds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Response {
    /// The sessions asked for.
    Sessions(Vec<Session>),
    /// This machine, boxed since it is far larger than any other answer.
    Machine(Box<Machine>),
    /// Every machine, this one first.
    Machines(Vec<Machine>),
    /// Where the work asked about goes.
    Placement(Placement),
    /// The program asked for, started.
    Started(Started),
    /// The steer asked for, in its session's inbox on `machine`. A talk's
    /// replies follow in its replies file on this machine, as they would
    /// for a session here.
    Steered {
        /// The machine the session runs on.
        machine: String,
    },
    /// The messages asked for.
    Messages(Vec<Message>),
    /// The message as sent.
    Sent(Message),
    /// Events follow, one per line.
    Watching,
    /// The process is counted present until it hangs up.
    Present,
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
