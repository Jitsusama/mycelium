//! The node's local socket, in niri's shape, through which pi, Quickshell
//! and a terminal reach it: a client writes one request as a JSON line and
//! reads one reply as a JSON line. A `Watch` reply is followed by events,
//! one per line, for as long as the client stays: first the whole state,
//! then each change.

use serde::{Deserialize, Serialize};

use crate::group::{Group, Pattern};
use crate::record::{Intent, Message, Session};

/// What a client asks of the node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Every session the node knows of, on this machine and others.
    Sessions,
    /// The latest messages to the groups `groups` hears, oldest first.
    Messages {
        /// Which groups.
        groups: Pattern,
        /// At most this many.
        limit: usize,
    },
    /// Send a message on the mesh, as a session would.
    Send(Draft),
    /// The whole state, then each change, for as long as the client stays.
    Watch {
        /// Which groups' messages to send; sessions are always sent.
        groups: Pattern,
        /// How many of the latest messages the first state holds.
        recent: usize,
    },
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    /// The sessions asked for.
    Sessions(Vec<Session>),
    /// The messages asked for.
    Messages(Vec<Message>),
    /// The message as sent.
    Sent(Message),
    /// Events follow, one per line.
    Watching,
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
