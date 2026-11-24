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

    /// Keeps the message `id`, unless it is already held; answers whether
    /// it was kept now, so what follows only a message's first arrival
    /// (a wake) never follows it again.
    fn put_message(&self, id: &str, body: &Value) -> io::Result<bool>;

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
    /// gains `seen`. Carrying it again clears the mark.
    fn mark_unreachable(
        &self,
        session: &str,
        machine: &str,
        since: i64,
        seen: i64,
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

/// Marks every session `machine` carried here out of reach since `since`,
/// as this running node saw at `seen`: a link that drops says nothing of
/// whether they still run, so they stay until a link to that machine comes
/// back and says which do.
pub fn out_of_reach(store: &impl Store, machine: &str, since: i64, seen: i64) {
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
/// that machine was last linked, and seen so from now, as this node runs:
/// it saw none of the time it was down. A mark an earlier run left keeps
/// what that run saw, so a node restarted again and again doesn't see a
/// machine gone afresh each time.
pub fn out_of_reach_of_unclaimed(store: &impl Store) -> io::Result<()> {
    let now = jiff::Timestamp::now().as_millisecond();
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
            let since = last_linked(store, &machine).unwrap_or(now);
            out_of_reach(store, &machine, since, now);
        }
    }
    Ok(())
}

/// What a node does with its marks out of reach as it stops at `at`: marks
/// the sessions of every machine no link carries, then puts away what this
/// run saw of each, so the time it is down isn't counted as seen. In that
/// order, since a mark put away and then swept would be seen from the stop
/// and keep that at the next start. The put-away claims nothing, so it runs
/// whatever the sweep returns, and a sweep that fails is said after it. A
/// node killed before it gets here leaves its marks as they stand.
pub fn stop_seeing_out_of_reach(store: &impl Store, at: i64) -> io::Result<()> {
    let swept = out_of_reach_of_unclaimed(store);
    put_away_out_of_reach(store, at);
    swept
}

/// Puts away what this run has seen of each session out of reach, as the
/// node stops at `at`.
fn put_away_out_of_reach(store: &impl Store, at: i64) {
    for presence in store.presences() {
        if let Some(machine) = presence.elsewhere.as_deref()
            && presence.unreachable_since.is_some()
            && let Err(error) = store.put_away_unreachable(&presence.session, machine, at)
        {
            eprintln!(
                "couldn't put away {}'s time out of reach: {error}",
                presence.session
            );
        }
    }
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

    /// A machine whose last link was let in after its mark, as a new one
    /// holding the mark back until its catch-up stands is, and that no
    /// link carries now, as after a node killed in that link's first
    /// minute, is out of reach since that link was let in, not since the
    /// days-old mark.
    #[test]
    fn a_machine_no_link_carries_is_out_of_reach_since_it_was_last_let_in() {
        let store = memory::Memory::default();
        let (days_ago, let_in) = (1_793_046_604_647, 1_793_305_804_647);
        store
            .put_presence("s1", "kestrel", &serde_json::json!({"session": "s1"}))
            .expect("carried");
        store.mark_linked("kestrel", days_ago).expect("marked");
        store.mark_admitted("kestrel", let_in).expect("let in");

        out_of_reach_of_unclaimed(&store).expect("swept");
        let since = store
            .presences()
            .into_iter()
            .find(|one| one.session == "s1")
            .and_then(|one| one.unreachable_since);
        assert_eq!(since, Some(let_in), "out of reach since it was let in");
    }

    /// Easing times a lane out of reach by the `unreachableSince` and
    /// `unreachableSeen` in its presence, so the store in memory marks them
    /// there, as the folders do, once and no later, and a link carrying the
    /// session again clears them, even where the body it carries names
    /// times or an owner of its own.
    #[test]
    fn a_session_out_of_reach_says_since_when_in_its_presence() {
        let store = memory::Memory::default();
        let said = serde_json::json!({"session": "s1"});
        store.put_presence("s1", "kestrel", &said).expect("carried");
        let body = |store: &memory::Memory, field: &str| {
            store
                .presences()
                .into_iter()
                .find(|one| one.session == "s1")
                .map(|one| one.body[field].clone())
        };
        out_of_reach(&store, "kestrel", 7, 8);
        out_of_reach(&store, "kestrel", 9, 10);
        assert_eq!(
            (
                body(&store, "unreachableSince"),
                body(&store, "unreachableSeen")
            ),
            (Some(serde_json::json!(7)), Some(serde_json::json!(8))),
            "since the first drop, as first seen"
        );
        let carried = serde_json::json!({
            "session": "s1",
            "unreachableSince": 3,
            "unreachableSeen": 4,
            "owner": {"pid": 1},
        });
        store
            .put_presence("s1", "kestrel", &carried)
            .expect("carried again");
        assert_eq!(
            ["unreachableSince", "unreachableSeen", "owner"].map(|field| body(&store, field)),
            [Some(Value::Null), Some(Value::Null), Some(Value::Null)],
            "a link carrying it again says it is in reach, and only its own machine \
             judges its process"
        );
    }

    /// A node killed and started again finds the marks its last run left,
    /// which say when that run first saw each machine gone, and keeps them;
    /// a session it marks at this start is seen gone from this start, though
    /// out of reach since its machine was last linked.
    #[test]
    fn a_restart_keeps_when_a_machine_was_first_seen_gone() {
        let store = memory::Memory::default();
        for session in ["s1", "s2"] {
            store
                .put_presence(session, "kestrel", &serde_json::json!({"session": session}))
                .expect("carried");
        }
        store
            .mark_unreachable("s1", "kestrel", 7, 8)
            .expect("marked");
        store
            .mark_linked("kestrel", 7)
            .expect("linked until it dropped");

        let before = jiff::Timestamp::now().as_millisecond();
        out_of_reach_of_unclaimed(&store).expect("swept");
        let after = jiff::Timestamp::now().as_millisecond();
        let seen = |session: &str| {
            store
                .presences()
                .into_iter()
                .find(|one| one.session == session)
                .map(|one| one.body["unreachableSeen"].clone())
        };
        assert_eq!(
            seen("s1"),
            Some(serde_json::json!(8)),
            "a mark this node's last run saw keeps when it saw it"
        );
        let started = seen("s2")
            .as_ref()
            .and_then(Value::as_i64)
            .expect("a session marked at this start says when it was seen");
        assert!(
            (before..=after).contains(&started),
            "seen gone from this start, not before it"
        );

        store
            .mark_unreachable("s1", "kestrel", 3, 4)
            .expect("marked");
        assert_eq!(
            seen("s1"),
            Some(serde_json::json!(8)),
            "and never seen again"
        );
    }

    /// A node stopped puts away what it saw of each machine out of reach,
    /// after sweeping the machines no link carries, so the hours it is down
    /// aren't seen: its next start sees the machine gone afresh from then,
    /// with what it saw before stopping kept. Swept after the put-away, a
    /// mark would be seen from the stop and keep that at the next start.
    #[test]
    fn a_stopped_node_sees_none_of_its_time_down() {
        let store = memory::Memory::default();
        for session in ["s1", "s2"] {
            store
                .put_presence(session, "kestrel", &serde_json::json!({"session": session}))
                .expect("carried");
        }
        let field = |session: &str, name: &str| {
            store
                .presences()
                .into_iter()
                .find(|one| one.session == session)
                .map(|one| one.body[name].clone())
        };
        let before = jiff::Timestamp::now().as_millisecond();
        // s1 dropped a minute ago as this node ran; s2 no link left marked.
        store
            .mark_unreachable("s1", "kestrel", 7, before - 60_000)
            .expect("marked");

        // The stop is said a minute on, so what the sweep saw is a minute.
        stop_seeing_out_of_reach(&store, before + 60_000).expect("stopped");
        let after = jiff::Timestamp::now().as_millisecond();
        assert_eq!(
            (
                field("s1", "unreachableSeen"),
                field("s1", "unreachableSeenFor")
            ),
            (Some(Value::Null), Some(serde_json::json!(120_000))),
            "seen from its drop to the stop, and no longer seen as the node is down"
        );
        let swept = field("s2", "unreachableSeenFor")
            .as_ref()
            .and_then(Value::as_i64)
            .expect("the stop's sweep marks it, and the put-away puts that away");
        assert!(
            (60_000 - (after - before)..=60_000).contains(&swept),
            "seen from the stop's sweep to the stop"
        );
        assert_eq!(
            field("s2", "unreachableSeen"),
            Some(Value::Null),
            "and no longer seen"
        );
        store
            .put_away_unreachable("s1", "kestrel", before + 600_000)
            .expect("put away again");
        assert_eq!(
            field("s1", "unreachableSeenFor"),
            Some(serde_json::json!(120_000)),
            "a mark with nothing seen this run has nothing more to put away"
        );

        let before = jiff::Timestamp::now().as_millisecond();
        out_of_reach_of_unclaimed(&store).expect("swept at the next start");
        let after = jiff::Timestamp::now().as_millisecond();
        let seen = field("s1", "unreachableSeen")
            .as_ref()
            .and_then(Value::as_i64)
            .expect("seen again from the start");
        assert!(
            (before..=after).contains(&seen),
            "seen gone from this start, not from the stop"
        );
        assert_eq!(
            (
                field("s1", "unreachableSince"),
                field("s1", "unreachableSeenFor")
            ),
            (Some(serde_json::json!(7)), Some(serde_json::json!(120_000))),
            "still out of reach since 7, with what was seen before the stop"
        );
    }
}
