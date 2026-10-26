//! What the network keeps of the mesh, as clients read it: messages said
//! in groups, and the sessions that say them.
//!
//! PROTOCOL.md says what each field means and which machine may write it.

use serde::{Deserialize, Serialize};

use crate::group::Group;

/// Something said to one or more groups. Written once and never changed:
/// a correction is a new message that names the one it supersedes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Unique across the realm.
    pub id: String,
    /// When it was sent, as an RFC 3339 instant.
    pub at: String,
    /// Who sent it: a peer's address or a person's name.
    pub from: String,
    /// The groups it went to.
    pub groups: Vec<Group>,
    /// What the sender wants from those who hear it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<Intent>,
    /// What was said.
    pub text: String,
    /// The message this one answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub re: Option<String>,
    /// The message this one replaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
}

/// What a message asks of those who hear it, in the mesh's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Intent {
    /// Put something off until its sender says the hold is over.
    Hold,
    /// Wants an answer.
    Question,
    /// For whoever owns what it is about.
    Finding,
    /// Context only.
    Fyi,
    /// Change course, sent by an orchestrator.
    Pivot,
}

/// A session as its presence says it stands. Only the machine it runs on
/// writes its presence, and only that machine can say whether it lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Its id, unique across the realm.
    pub id: String,
    /// Its address, by which peers reach it.
    pub address: String,
    /// The machine it runs on.
    pub machine: String,
    /// Its window's title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The line it tells its peers about its work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_on: Option<String>,
    /// The groups it hears.
    pub groups: Vec<Group>,
    /// How it stands.
    pub state: State,
    /// What it is doing now, such as a tool's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// Since when it has stood as it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// The orchestrator it answers to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrated_by: Option<String>,
    /// When its machine last wrote its presence.
    pub at: String,
}

/// How a session stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Running a turn.
    Busy,
    /// Running and waiting for something to do.
    Idle,
    /// Its process has gone.
    Ended,
}
