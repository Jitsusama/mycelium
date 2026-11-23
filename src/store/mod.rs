//! The store: where a node keeps the mesh's messages, presence and leases
//! on its machine.
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
//! its machine. What another machine says it is has that one writer too.

mod ends;
pub mod folders;
#[cfg(test)]
pub mod memory;

use std::collections::BTreeMap;
use std::io;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::mpsc;

use crate::facts::{Machine, Readiness};

/// A session's presence, as the store holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Presence {
    /// The session's id.
    pub session: String,
    /// The presence as its machine wrote it, shared with every other
    /// reading of it until its file changes, since each pass over the
    /// sessions reads them all and copying each was most of its cost.
    pub body: Arc<Value>,
    /// The machine it runs on, when that is another; `None` is this one.
    pub elsewhere: Option<String>,
    /// Whether it still lives: judged from its process on this machine,
    /// and taken as so for a session elsewhere while a link carries it.
    pub lives: bool,
    /// Since when, in milliseconds since the epoch, no link has reached
    /// the machine it runs on, so whether it lives is unknown; `None` while
    /// a link carries it, and always for this machine's own.
    pub unreachable_since: Option<i64>,
}

/// What changed in a store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A message landed.
    Message(String),
    /// Some presence changed, or the leases a session holds.
    Presence,
    /// What a linked machine says of itself was said again, by its name.
    Machine(String),
    /// The carries a machine's node ordered were told again, by its
    /// name.
    Carries(String),
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

/// How many changes already waiting are taken with the next at most, so a
/// store changing without pause still lets its reader do its other work.
const GATHERED_AT_MOST: usize = 1024;

impl Changes {
    /// The next change and every other already waiting behind it, each
    /// once, in the order each was first seen; `None` once the store stops
    /// telling them. Whoever acts on a change reads the store as it stands
    /// then, so one telling of a change covers the same change told again
    /// behind it: a node whose thread a slow disk held finds hundreds of
    /// presence changes waiting, and reads its sessions once rather than
    /// once for each. It is cancel safe, as `select!` needs: nothing is
    /// taken but in the wait for the first.
    pub async fn gathered(&mut self) -> Option<Vec<Change>> {
        let mut gathered = vec![self.changes.recv().await?];
        for _ in 1..GATHERED_AT_MOST {
            let Ok(change) = self.changes.try_recv() else {
                break;
            };
            if !gathered.contains(&change) {
                gathered.push(change);
            }
        }
        Some(gathered)
    }
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

    /// Records that a link with `machine` ended at `at`, in milliseconds
    /// since the epoch, for counting how often it drops.
    fn mark_ended(&self, machine: &str, at: i64) -> io::Result<()>;

    /// How many links with `machine` ended at `since` or later, as
    /// [`Store::mark_ended`] kept them. Only the last day's are kept.
    fn endings(&self, machine: &str, since: i64) -> usize;

    /// Keeps why a link with `machine` was last refused, as its end said
    /// it, over any it kept before, until one is admitted.
    fn refuse_link(&self, machine: &str, why: &str) -> io::Result<()>;

    /// Why a link with `machine` was last refused, if none was admitted
    /// since, as [`Store::refuse_link`] kept it.
    fn link_refused(&self, machine: &str) -> Option<String>;

    /// Forgets why a link with `machine` was refused, once one is admitted.
    fn link_admitted(&self, machine: &str) -> io::Result<()>;

    /// Keeps the presence of a session that runs on `machine`, another
    /// machine, as that machine wrote it. Refused when the id names this
    /// machine's own session, unless that has ended here, as one carried
    /// to `machine` has, or another machine's.
    fn put_presence(&self, session: &str, machine: &str, body: &Value) -> io::Result<()>;

    /// Says again that a session `machine` carries still stands, so a copy
    /// no node keeps saying so can be judged gone.
    fn keep_presence(&self, session: &str, machine: &str) -> io::Result<()>;

    /// Keeps the presence of a session that runs on `machine` but marks it
    /// out of reach since `since`, unless it is marked so already; carrying
    /// it again clears the mark.
    fn mark_unreachable(&self, session: &str, machine: &str, since: i64) -> io::Result<()>;

    /// Lets go of the presence of a session that runs on `machine`, and of
    /// the leases it holds, keeping it among those [`Self::gone`]; this
    /// machine's own sessions, and other machines', are never touched.
    fn remove_presence(&self, session: &str, machine: &str) -> io::Result<()>;

    /// Each session another machine ran whose presence was let go here in
    /// the last week, as that machine last wrote it and living no more,
    /// so a session that ended there is still known by who ran it. Kept
    /// apart from the mesh's folders, so no session here reads it.
    fn gone(&self) -> Vec<Presence>;

    /// Every holding of leases kept here, as the mesh writes them: one per
    /// session, this machine's and those carried from others.
    fn holdings(&self) -> Vec<Value>;

    /// Keeps the leases held by a session that runs on `machine`, another
    /// machine, as that machine said them, or lets them go when there are
    /// none. Refused when the id names a holding of another machine's, or
    /// of this machine's while its session is not one `machine` carries
    /// here.
    fn put_leases(&self, session: &str, machine: &str, leases: &[Value]) -> io::Result<()>;

    /// Says this machine's process `pid` has ended, since the connection
    /// it held to say it was present closed. A session it owns is judged
    /// ended from now on, until `pid` is present again or no process of
    /// that id is left, so a Mac, where no process can be watched, need not
    /// wait for its next look.
    fn ended(&self, pid: i32);

    /// Says this machine's process `pid` is present, taking back an end
    /// said of it before.
    fn present(&self, pid: i32);

    /// What each other machine last said it is, by name.
    fn machines(&self) -> Vec<Machine>;

    /// Keeps what another machine, `machine.name`, says it is, in place of
    /// what it said before.
    fn put_machine(&self, machine: &Machine) -> io::Result<()>;

    /// The carries each machine's node last told of, by machine: this
    /// machine's own as its node put them, and each other's as its link
    /// heard them.
    fn told_carries(&self) -> BTreeMap<String, Vec<Value>>;

    /// Keeps `told`, the carries `machine`'s node ordered, in place of
    /// what it told before; none lets go of them.
    fn put_told_carries(&self, machine: &str, told: &[Value]) -> io::Result<()>;

    /// Starts telling of each change.
    fn changes(&self) -> io::Result<Changes>;
}

/// Lets go of every session `machine` carried here but those `kept`.
pub fn let_go(store: &impl Store, machine: &str, kept: impl Fn(&str) -> bool) {
    for presence in store.presences() {
        if presence.elsewhere.as_deref() == Some(machine) && !kept(&presence.session) {
            // Letting go is tidying: a presence left behind is let go when
            // the next link to that machine starts.
            let _ = store.remove_presence(&presence.session, machine);
        }
    }
}

/// Has `machine`, made for work and now let go, leave the realm `at`: it
/// is no longer one of the machines, as one whose `leaves_at` has passed
/// isn't, and every session it carried here is let go, since a machine
/// released is never dialled again. One that declared nothing has no
/// `leaves_at` to say it by, so it stays, out of reach, as any machine a
/// link no longer reaches does.
pub fn left(store: &impl Store, machine: &str, at: jiff::Timestamp) {
    if let Some(mut said) = store.machines().into_iter().find(|one| one.name == machine)
        && let Some(declared) = said.declared.as_mut()
    {
        declared.leaves_at = Some(declared.leaves_at.map_or(at, |leaves| leaves.min(at)));
        if let Err(error) = store.put_machine(&said) {
            eprintln!("couldn't say {machine} has left: {error}");
        }
    }
    let_go(store, machine, |_| false);
}

/// Marks every session `machine` carried here out of reach since `since`:
/// a link that drops says nothing of whether they still run, so they stay
/// until a link to that machine comes back and says which do.
pub fn out_of_reach(store: &impl Store, machine: &str, since: i64) {
    // What it said it is stays, as of when a link last carried it.
    if let Some(mut said) = store.machines().into_iter().find(|one| one.name == machine)
        && said.state != Readiness::Unreachable
    {
        said.state = Readiness::Unreachable;
        if let Err(error) = store.put_machine(&said) {
            eprintln!("couldn't mark {machine} out of reach: {error}");
        }
    }
    for presence in store.presences() {
        if presence.elsewhere.as_deref() == Some(machine)
            && let Err(error) = store.mark_unreachable(&presence.session, machine, since)
        {
            eprintln!("couldn't mark {} out of reach: {error}", presence.session);
        }
    }
}

/// Marks out of reach the sessions of every machine no link carries now,
/// as left behind by a link or node that ended without saying so, since
/// that machine was last linked.
pub fn out_of_reach_of_unclaimed(store: &impl Store) -> io::Result<()> {
    let mut machines: Vec<String> = store
        .presences()
        .into_iter()
        .filter_map(|p| p.elsewhere)
        .chain(store.machines().into_iter().map(|one| one.name))
        .collect();
    machines.sort_unstable();
    machines.dedup();
    for machine in machines {
        if let Some(_claim) = store.claim(&machine)? {
            let since = store
                .linked_until(&machine)
                .unwrap_or_else(|| jiff::Timestamp::now().as_millisecond());
            out_of_reach(store, &machine, since);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Changes waiting behind the next are taken with it, each once and
    /// in the order first seen, and the next waits for what comes after.
    #[tokio::test]
    async fn waiting_changes_are_told_once_each() {
        let (telling, told) = mpsc::unbounded_channel();
        let mut changes = Changes {
            changes: told,
            keep: Box::new(()),
        };
        for change in [
            Change::Presence,
            Change::Message("m1".to_owned()),
            Change::Presence,
            Change::Carries("wren".to_owned()),
            Change::Message("m1".to_owned()),
            Change::Presence,
        ] {
            telling.send(change).expect("the change is told");
        }
        assert_eq!(
            changes.gathered().await,
            Some(vec![
                Change::Presence,
                Change::Message("m1".to_owned()),
                Change::Carries("wren".to_owned()),
            ])
        );
        telling.send(Change::Presence).expect("the change is told");
        assert_eq!(changes.gathered().await, Some(vec![Change::Presence]));
        drop(telling);
        assert_eq!(changes.gathered().await, None);
    }

    /// No more than [`GATHERED_AT_MOST`] are taken at once, so the rest
    /// wait for the next.
    #[tokio::test]
    async fn a_flood_of_changes_is_taken_in_parts() {
        let (telling, told) = mpsc::unbounded_channel();
        let mut changes = Changes {
            changes: told,
            keep: Box::new(()),
        };
        for id in 0..=GATHERED_AT_MOST {
            telling
                .send(Change::Message(id.to_string()))
                .expect("the change is told");
        }
        let first = changes.gathered().await.expect("changes are told");
        assert_eq!(first.len(), GATHERED_AT_MOST);
        assert_eq!(
            changes.gathered().await,
            Some(vec![Change::Message(GATHERED_AT_MOST.to_string())])
        );
    }
}
