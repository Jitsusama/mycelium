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
pub(crate) mod folder;
pub mod folders;
#[cfg(test)]
pub mod memory;

use std::collections::{BTreeMap, BTreeSet};
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

    /// Keeps the message `id`, unless it is already held; answers whether
    /// it was kept now, so what follows only a message's first arrival
    /// (a wake) never follows it again.
    fn put_message(&self, id: &str, body: &Value) -> io::Result<bool>;

    /// Every presence held, this machine's and others'.
    fn presences(&self) -> Vec<Presence>;

    /// Claims the right to carry `machine`'s sessions here, or `None` while
    /// another link holds it.
    fn claim(&self, machine: &str) -> io::Result<Option<Claim>>;

    /// Asks the link holding `machine`'s claim to let it go, for a newer
    /// link with it, as of now, or a millisecond past the last ask where
    /// that is as late, so each ask's mark is later than the one before.
    fn supersede(&self, machine: &str) -> io::Result<()>;

    /// The mark of the last ask for `machine`'s claim, when a newer link
    /// asked for it in milliseconds since the epoch, if one ever did.
    fn superseded(&self, machine: &str) -> Option<i64>;

    /// Until when this store was last linked with `machine`, in
    /// milliseconds since the epoch, if it ever was.
    fn linked_until(&self, machine: &str) -> Option<i64>;

    /// Records that this store is linked with `machine` as of `at`.
    fn mark_linked(&self, machine: &str, at: i64) -> io::Result<()>;

    /// When a link with `machine` was last let in, in milliseconds since
    /// the epoch, if one ever was.
    fn admitted_at(&self, machine: &str) -> Option<i64>;

    /// Records that a link with `machine` was let in at `at`.
    fn mark_admitted(&self, machine: &str, at: i64) -> io::Result<()>;

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
    /// out of reach since `since`, as this running node first saw at
    /// `seen`, unless this run has marked it so already. A mark an earlier
    /// run or node left keeps its `since` and what earlier runs saw, and
    /// gains `seen`, never before its `since`, as nothing is seen gone
    /// before it went. With no `seen`, as a link running apart from the
    /// node marks it, only `since` is kept. Carrying it again clears the
    /// mark.
    fn mark_unreachable(
        &self,
        session: &str,
        machine: &str,
        since: i64,
        seen: Option<i64>,
    ) -> io::Result<()>;

    /// Puts away what this run has seen of a session `machine` carries out
    /// of reach, as the node stops at `at`: the time from its `seen` to
    /// `at` joins what earlier runs saw, and it keeps no `seen`, so the time
    /// the node is down isn't seen and the next start gives it a new one. A
    /// session this run hasn't marked is left as it is.
    fn put_away_unreachable(&self, session: &str, machine: &str, at: i64) -> io::Result<()>;

    /// Lets go of the presence of a session that runs on `machine`, and of
    /// the leases it holds, keeping it among those [`Self::gone`]; this
    /// machine's own sessions, and other machines', are never touched.
    fn remove_presence(&self, session: &str, machine: &str) -> io::Result<()>;

    /// Keeps `body` as the presence of `session`, a session of this
    /// machine's that the node itself speaks for, as it does for one
    /// present through its worker socket: its owner is the node's own
    /// process, so it stands while the node runs and is judged ended once
    /// the node is gone. Refused where the id names another machine's
    /// session, or one of this machine's another process holds, living or
    /// parked.
    fn put_own_presence(&self, session: &str, body: &Value) -> io::Result<()>;

    /// Lets go of the presence of `session` where the node's own process
    /// owns it, as [`Self::put_own_presence`] wrote it; any other is left.
    fn remove_own_presence(&self, session: &str) -> io::Result<()>;

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

    /// What each other machine last said it is, by name. One is a worker
    /// only where this store last heard so from its node's link since it
    /// opened, by the link's admission ([`Store::mark_worker`]) or its
    /// last put of it, whatever its record says: the records outlive the
    /// node, and a session may write them.
    fn machines(&self) -> Vec<Machine>;

    /// Keeps what another machine, `machine.name`, says it is, in place of
    /// what it said before.
    fn put_machine(&self, machine: &Machine) -> io::Result<()>;

    /// Keeps whether a link let in with `machine` reached it as a
    /// `worker`, as its way in alone says, so it reads so from now until a
    /// put of it says otherwise, whether or not it has said what it is.
    /// A mark that moves is told as a change of that machine, as a put is,
    /// so a watch of the machines sees it.
    fn mark_worker(&self, machine: &str, worker: bool);

    /// Whether this store last heard from its node's link, since it
    /// opened, that `machine` is a worker ([`Store::machines`]), whether
    /// or not a record of it can be read: a session may take a record away
    /// as well as write one.
    fn is_worker(&self, machine: &str) -> bool;

    /// Keeps that this store's node said `machine` left at `at`
    /// ([`left`]), the earliest such kept. Kept in memory alone, as a
    /// worker's mark is, so a leaving only the node said stands against
    /// what the machine's link says after, never one a session wrote in
    /// its record.
    fn mark_left(&self, machine: &str, at: jiff::Timestamp);

    /// When this store's node last said, since the store opened, that
    /// `machine` left ([`Store::mark_left`]).
    fn left_at(&self, machine: &str) -> Option<jiff::Timestamp>;

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
/// link no longer reaches does. Where no link holds the machine's claim,
/// it holds it while it puts back what it read, as the start's sweep
/// does, so no link is let in meanwhile; one a link holds is that link's,
/// let in already. Either way, whether it is a worker is put back as its
/// last link said just before.
pub fn left(store: &impl Store, machine: &str, at: jiff::Timestamp) {
    let _claim = store.claim(machine).unwrap_or_else(|error| {
        eprintln!("couldn't claim {machine} while saying it has left: {error}");
        None
    });
    if let Some(mut said) = store.machines().into_iter().find(|one| one.name == machine)
        && let Some(declared) = said.declared.as_mut()
    {
        declared.leaves_at = Some(declared.leaves_at.map_or(at, |leaves| leaves.min(at)));
        said.worker = store.is_worker(machine);
        if let Err(error) = store.put_machine(&said) {
            eprintln!("couldn't say {machine} has left: {error}");
        }
    }
    store.mark_left(machine, at);
    let_go(store, machine, |_| false);
}

/// Marks every session `machine` carried here out of reach since `since`,
/// as this running node saw at `seen`, if it runs to see it: a link that
/// drops says nothing of whether they still run, so they stay until a link
/// to that machine comes back and says which do.
pub fn out_of_reach(store: &impl Store, machine: &str, since: i64, seen: Option<i64>) {
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
            && let Err(error) = store.mark_unreachable(&presence.session, machine, since, seen)
        {
            eprintln!("couldn't mark {} out of reach: {error}", presence.session);
        }
    }
}

/// When this store last stood linked with `machine`, if it ever did: the
/// link's mark ([`Store::linked_until`]), or when a link was last let in
/// where that is later. A new link keeps the mark where the last left
/// it, maybe days back, until its catch-up has stood, so the next link
/// reaches as far; a node stopped or killed before then would otherwise
/// read the machine as out of reach since those days.
pub fn last_linked(store: &impl Store, machine: &str) -> Option<i64> {
    let until = store.linked_until(machine);
    let admitted = store.admitted_at(machine);
    until.max(admitted)
}

/// Marks out of reach the sessions of every machine no link carries now,
/// as left behind by a link or node that ended without saying so, since
/// that machine was last linked, and seen so from `seen`, when this run of
/// the node began, or from the drop where that is later: it saw none of
/// the time it was down. A mark an earlier run left keeps what that run
/// saw, so a node restarted again and again doesn't see a machine gone
/// afresh each time. A machine whose claim fails is said and passed over
/// for the rest, and the first failure returned after them, naming its
/// machine; one a link claims is left to it, and said where it has marks,
/// since those then keep no sight of this run's.
pub fn out_of_reach_of_unclaimed(store: &impl Store, seen: i64) -> io::Result<()> {
    let now = jiff::Timestamp::now().as_millisecond();
    let mut machines: Vec<String> = store
        .presences()
        .into_iter()
        .filter_map(|p| p.elsewhere)
        .chain(store.machines().into_iter().map(|one| one.name))
        .collect();
    machines.sort_unstable();
    machines.dedup();
    let marked: BTreeSet<String> = store
        .presences()
        .into_iter()
        .filter(|presence| presence.unreachable_since.is_some())
        .filter_map(|presence| presence.elsewhere)
        .collect();
    let mut failed = None;
    for machine in machines {
        match store.claim(&machine) {
            Ok(Some(_claim)) => {
                let since = last_linked(store, &machine).unwrap_or(now);
                out_of_reach(store, &machine, since, Some(seen));
            }
            Ok(None) if marked.contains(&machine) => {
                eprintln!("left {machine}'s marks out of reach to the link that claims it");
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("couldn't claim {machine} to mark it out of reach: {error}");
                failed.get_or_insert_with(|| {
                    io::Error::new(error.kind(), format!("couldn't claim {machine}: {error}"))
                });
            }
        }
    }
    failed.map_or(Ok(()), Err)
}

/// What a node that `began` this run does with its marks out of reach as
/// it stops at `at`: marks the sessions of every machine no link carries,
/// seen from when it began or the drop, whichever is later, as a link
/// running apart from the node leaves its drops unseen, then puts away
/// what this run saw of each, so the time it is down isn't counted as
/// seen. In that order, since a mark put away and then swept would be
/// seen at the next start from this run. The put-away runs whatever the
/// sweep returns, and a sweep that fails is said after it. A node killed
/// before it gets here leaves its marks as they stand.
pub fn stop_seeing_out_of_reach(store: &impl Store, began: i64, at: i64) -> io::Result<()> {
    let swept = out_of_reach_of_unclaimed(store, began);
    put_away_out_of_reach(store, at);
    swept
}

/// Puts away what this run has seen of each session out of reach, as the
/// node stops at `at`, under its machine's claim, as every mark is made.
/// A machine a link running apart from the node claims is being carried,
/// so its marks are that link's to clear as it carries each session, and
/// are left; one that can't be claimed is left too. Either is said, since
/// a mark left so keeps this run's sight, and the time the node is down
/// then counts as seen.
fn put_away_out_of_reach(store: &impl Store, at: i64) {
    let mut marked: Vec<(String, String)> = store
        .presences()
        .into_iter()
        .filter(|presence| presence.unreachable_since.is_some())
        .filter_map(|presence| Some((presence.elsewhere?, presence.session)))
        .collect();
    marked.sort_unstable();
    for each in marked.chunk_by(|one, other| one.0 == other.0) {
        let machine = &each[0].0;
        match store.claim(machine) {
            Ok(Some(_claim)) => {
                for (_, session) in each {
                    if let Err(error) = store.put_away_unreachable(session, machine, at) {
                        eprintln!("couldn't put away {session}'s time out of reach: {error}");
                    }
                }
            }
            Ok(None) => eprintln!(
                "left {} of {machine}'s sessions out of reach to the link that claims it",
                each.len()
            ),
            Err(error) => {
                eprintln!("couldn't claim {machine} to put its time out of reach away: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests;
