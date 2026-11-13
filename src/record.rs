//! What the network keeps of the mesh, as clients read it: messages said
//! in groups, and the sessions that say them.
//!
//! PROTOCOL.md says what each field means and which machine may write it.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// The realm it crossed from, by a flow, for one that did.
    #[serde(default, rename = "fromRealm", skip_serializing_if = "Option::is_none")]
    pub from_realm: Option<String>,
    /// The realm it waits to cross into, for one a link carries to a
    /// machine whose node hands it across; no one in its own realm hears
    /// it.
    #[serde(default, rename = "forRealm", skip_serializing_if = "Option::is_none")]
    pub for_realm: Option<String>,
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
    /// The quest it works on, by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quest: Option<String>,
    /// What it waits on its person for, while it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wants: Option<Want>,
    /// What it landed lately, newest first, as its presence says: what its
    /// orchestrator would otherwise be told in a message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub landings: Vec<Landing>,
    /// The window it runs in, for going to it. Only a session of this
    /// machine's has one, since a window is reached on its own machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<Window>,
    /// The id a holder on its machine keeps it by, past any window, which
    /// `mycelium attach` takes up there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<String>,
    /// The words that join it from a terminal on this machine, to be run
    /// as given, when it is held and its machine is in reach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach: Option<Vec<String>>,
    /// The pi host it runs in, by the host's own key, when it is one of
    /// many sessions sharing that one process rather than a process of
    /// its own. Its presence's owner is then the host's process, so no
    /// holder is its own, and a viewer joins it through the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The conversation it is within its host, by the id that outlives
    /// the host's restarts, which a viewer joining it through the host
    /// names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    /// When its machine last wrote its presence.
    pub at: String,
    /// Its presence whole, as its session wrote it, for a reader that
    /// shows more of it than the fields above; a session elsewhere's has
    /// its process taken out. Whether it lives is `state`, never this.
    /// Shared with the store's reading of it, as every copy of the
    /// sessions taken would otherwise copy each presence whole.
    #[serde(default, skip_serializing_if = "is_null")]
    pub presence: Arc<Value>,
}

/// Whether a presence held is none at all.
fn is_null(presence: &Arc<Value>) -> bool {
    presence.is_null()
}

/// What a session waits on its person for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Want {
    /// What kind of wait, in the mesh's words: a question, an approval,
    /// blocked or an action.
    pub kind: String,
    /// Since when, as an RFC 3339 instant.
    pub since: String,
    /// What it asked or said, in one short line.
    pub said: String,
}

/// Something a session landed: a change merged, or an item of its plan done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Landing {
    /// When, as an RFC 3339 instant.
    pub at: String,
    /// What landed, by the name its own place gives it, such as
    /// `mycelium#81` or a plan's item.
    #[serde(rename = "ref")]
    pub reference: String,
    /// What it is, in one plain line.
    pub what: String,
    /// What it means to the person, when it means something to them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
}

/// A terminal's window, as the terminal names it: for kitty, the window's
/// id and the socket kitty is told things on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// The terminal, such as `kitty`.
    pub terminal: String,
    /// The window's id, within its terminal.
    pub id: String,
    /// What the id is unique within, such as kitty's socket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<String>,
    /// The app id the terminal's window was opened with, where whoever
    /// opened it said so, unique to it: the name a compositor lists that
    /// window by (niri's `app_id`), since one terminal process can draw
    /// several windows and its own ids mean nothing to the compositor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
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
    /// Its machine is out of reach, so whether it runs is unknown until a
    /// link comes back.
    Unreachable,
}
