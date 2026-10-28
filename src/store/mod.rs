//! The store: where a node keeps the mesh's messages and presence on its
//! machine.
//!
//! Today that is the harness mesh's own folders ([`folders`]), so sessions
//! go on reading and writing them unchanged while the node carries them to
//! other machines. The node's own log takes their place later behind the
//! same trait. Shared files, kept by content, get a store of their own when
//! they come; PROTOCOL.md says how.
//!
//! Messages are written once by id, so a store holds the union of what
//! every machine sent. A presence has one writer, the machine its session
//! runs on; a store holds its own machine's and copies of others', and a
//! copy is written and let go only by the one link that holds a claim on
//! its machine.

mod ends;
pub mod folders;
#[cfg(test)]
pub mod memory;

use std::io;

use serde_json::Value;
use tokio::sync::mpsc;

/// A session's presence, as the store holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Presence {
    /// The session's id.
    pub session: String,
    /// The presence as its machine wrote it.
    pub body: Value,
    /// The machine it runs on, when that is another; `None` is this one.
    pub elsewhere: Option<String>,
    /// Whether it still lives: judged from its process on this machine,
    /// and taken as so for a session elsewhere while a link carries it.
    pub lives: bool,
}

/// What changed in a store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A message landed.
    Message(String),
    /// Some presence changed.
    Presence,
    /// Changes may have been missed, so everything should be read again.
    Rescan,
}

/// A store's changes as they happen, until this is dropped.
pub struct Changes {
    /// Each change, in the order it was seen.
    pub changes: mpsc::UnboundedReceiver<Change>,
    /// Whatever keeps the changes coming, let go when this is.
    pub keep: Box<dyn Send>,
}

/// The right to carry one machine's sessions into this store, held by one
/// link at a time and given up when dropped.
pub type Claim = Box<dyn Send>;

/// Where a node keeps the mesh's messages and presence.
pub trait Store: Clone + Send + Sync + 'static {
    /// The ids of the messages held that were sent at or after `since`, in
    /// milliseconds since the epoch.
    fn message_ids(&self, since: i64) -> Vec<String>;

    /// The message `id` as its sender wrote it.
    fn message(&self, id: &str) -> Option<Value>;

    /// Keeps the message `id`, unless it is already held.
    fn put_message(&self, id: &str, body: &Value) -> io::Result<()>;

    /// Every presence held, this machine's and others'.
    fn presences(&self) -> Vec<Presence>;

    /// Claims the right to carry `machine`'s sessions here, or `None` while
    /// another link holds it.
    fn claim(&self, machine: &str) -> io::Result<Option<Claim>>;

    /// Until when this store was last linked with `machine`, in
    /// milliseconds since the epoch, if it ever was.
    fn linked_until(&self, machine: &str) -> Option<i64>;

    /// Records that this store is linked with `machine` as of `at`.
    fn mark_linked(&self, machine: &str, at: i64) -> io::Result<()>;

    /// Keeps the presence of a session that runs on `machine`, another
    /// machine, as that machine wrote it. Refused when the id names this
    /// machine's own session or another machine's.
    fn put_presence(&self, session: &str, machine: &str, body: &Value) -> io::Result<()>;

    /// Says again that a session `machine` carries still stands, so a copy
    /// no node keeps saying so can be judged gone.
    fn keep_presence(&self, session: &str, machine: &str) -> io::Result<()>;

    /// Lets go of the presence of a session that runs on `machine`; this
    /// machine's own sessions, and other machines', are never touched.
    fn remove_presence(&self, session: &str, machine: &str) -> io::Result<()>;

    /// Starts telling of each change.
    fn changes(&self) -> io::Result<Changes>;
}

/// Lets go of every session `machine` carried here.
pub fn let_go(store: &impl Store, machine: &str) {
    for presence in store.presences() {
        if presence.elsewhere.as_deref() == Some(machine) {
            // Letting go is tidying: a presence left behind is let go when
            // the next link to that machine starts.
            let _ = store.remove_presence(&presence.session, machine);
        }
    }
}

/// Lets go of the sessions of every machine no link carries now, as left
/// behind by a link or node that ended without letting go.
pub fn let_go_of_unclaimed(store: &impl Store) -> io::Result<()> {
    let mut machines: Vec<String> = store
        .presences()
        .into_iter()
        .filter_map(|p| p.elsewhere)
        .collect();
    machines.sort_unstable();
    machines.dedup();
    for machine in machines {
        if let Some(_claim) = store.claim(&machine)? {
            let_go(store, &machine);
        }
    }
    Ok(())
}
