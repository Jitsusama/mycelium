//! A store held in memory, for tests: what the mesh's folders hold, with
//! the machine's processes stood in for by whether each session lives. It
//! keeps the folders' rules: a copy never goes over a session not its
//! machine's, but for one of this machine's that has ended, and one claim
//! per machine at a time.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use serde_json::Value;
use tokio::sync::mpsc;

use super::{Change, Changes, Claim, Presence, Store};
use crate::facts::Machine;

/// A store in memory, shared by its clones as the folders are.
#[derive(Clone, Default)]
pub struct Memory(Arc<Mutex<Held>>);

#[derive(Default)]
struct Held {
    messages: BTreeMap<String, Value>,
    presences: BTreeMap<String, Presence>,
    /// The presences of sessions elsewhere let go, the latest for each
    /// session and machine kept for good, since memory holds no times to
    /// let them go by.
    gone: Vec<Presence>,
    holdings: BTreeMap<String, Value>,
    claimed: BTreeSet<String>,
    linked: BTreeMap<String, i64>,
    /// When a link with each machine was last let in.
    admitted: BTreeMap<String, i64>,
    /// When each machine's links ended, every one kept.
    links_ended: BTreeMap<String, Vec<i64>>,
    /// Why each machine's last link was refused, until one was admitted.
    links_refused: BTreeMap<String, String>,
    machines: BTreeMap<String, Machine>,
    /// The carries each machine's node told of.
    carries: BTreeMap<String, Vec<Value>>,
    /// Processes said to have ended, whose sessions are ended whatever
    /// the test said of them.
    ended: BTreeSet<i64>,
    told: Vec<mpsc::UnboundedSender<Change>>,
    /// How many times a message has been read.
    messages_read: usize,
    /// Whether reading a message waits, as a slow disk's open does, until
    /// the test lets it go on.
    gate: Option<Arc<Gate>>,
}

/// Shut while a test holds a store's reads, opened when it lets them go.
#[derive(Default)]
struct Gate {
    shut: Mutex<bool>,
    opened: Condvar,
}

/// A store's reads of messages held, each waiting until this is dropped.
pub struct ReadsHeld(Arc<Gate>);

impl Drop for ReadsHeld {
    fn drop(&mut self) {
        *self.0.shut.lock().unwrap_or_else(PoisonError::into_inner) = false;
        self.0.opened.notify_all();
    }
}

/// A claim on a memory store, given up when dropped.
struct Claimed {
    store: Memory,
    machine: String,
}

impl Drop for Claimed {
    fn drop(&mut self) {
        self.store.held().claimed.remove(&self.machine);
    }
}

impl Memory {
    /// Starts a session of this machine's, living or ended.
    pub fn start_here(&self, session: &str, body: Value, lives: bool) {
        let mut held = self.held();
        held.presences.insert(
            session.to_owned(),
            Presence {
                session: session.to_owned(),
                body: Arc::new(body),
                elsewhere: None,
                lives,
                unreachable_since: None,
            },
        );
        held.tell(&Change::Presence);
    }

    /// Has a session of this machine's hold leases, as `holding` says.
    pub fn hold_here(&self, session: &str, holding: Value) {
        let mut held = self.held();
        held.holdings.insert(session.to_owned(), holding);
        held.tell(&Change::Presence);
    }

    /// Sends a message from this machine, as a session here would.
    pub fn send_here(&self, id: &str, body: Value) {
        let mut held = self.held();
        held.messages.insert(id.to_owned(), body);
        held.tell(&Change::Message(id.to_owned()));
    }

    /// Holds every read of a message, as a disk too busy to open a file
    /// would, until what this answers is dropped. Nothing else waits.
    #[must_use]
    pub fn hold_reads(&self) -> ReadsHeld {
        let gate = Arc::new(Gate {
            shut: Mutex::new(true),
            opened: Condvar::new(),
        });
        self.held().gate = Some(Arc::clone(&gate));
        ReadsHeld(gate)
    }

    /// How many times a message has been read, as each is a file opened
    /// where the store is folders.
    pub fn messages_read(&self) -> usize {
        self.held().messages_read
    }

    /// Holds `body` as message `id` without telling anyone, as one a watch
    /// missed.
    pub fn miss(&self, id: &str, body: Value) {
        self.held().messages.insert(id.to_owned(), body);
    }

    /// Says a watch may have missed something, as one that overflowed
    /// does.
    pub fn overflow(&self) {
        self.held().tell(&Change::Rescan);
    }

    fn held(&self) -> MutexGuard<'_, Held> {
        // A test that panicked holding the lock has failed already.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Held {
    fn tell(&mut self, change: &Change) {
        self.told
            .retain(|sender| sender.send(change.clone()).is_ok());
    }

    fn holds_for(&self, session: &str, machine: &str) -> bool {
        self.holdings
            .get(session)
            .and_then(|holding| holding.get("machine"))
            .and_then(Value::as_str)
            == Some(machine)
    }

    fn carries(&self, session: &str, machine: &str) -> bool {
        self.presences
            .get(session)
            .is_some_and(|presence| presence.elsewhere.as_deref() == Some(machine))
    }

    /// Whether `session` is this machine's and has ended here, as the
    /// folders judge it, so another machine may speak for it.
    fn ended_here(&self, session: &str) -> bool {
        self.presences.get(session).is_some_and(|presence| {
            let owner = presence.body.pointer("/owner/pid").and_then(Value::as_i64);
            presence.elsewhere.is_none()
                && (!presence.lives || owner.is_some_and(|pid| self.ended.contains(&pid)))
        })
    }
}

impl Store for Memory {
    fn message_ids(&self, since: i64) -> Vec<String> {
        self.held()
            .messages
            .keys()
            .filter(|id| {
                id.split('-')
                    .next()
                    .and_then(|at| at.parse::<i64>().ok())
                    .is_some_and(|at| at >= since)
            })
            .cloned()
            .collect()
    }

    fn message(&self, id: &str) -> Option<Value> {
        // Waited on with the store let go, so only reads wait.
        let gate = self.held().gate.clone();
        if let Some(gate) = gate {
            let shut = gate.shut.lock().unwrap_or_else(PoisonError::into_inner);
            drop(
                gate.opened
                    .wait_while(shut, |shut| *shut)
                    .unwrap_or_else(PoisonError::into_inner),
            );
        }
        let mut held = self.held();
        held.messages_read += 1;
        held.messages.get(id).cloned()
    }

    fn put_message(&self, id: &str, body: &Value) -> io::Result<bool> {
        super::folders::safe(id)?;
        let mut held = self.held();
        if held.messages.contains_key(id) {
            return Ok(false);
        }
        held.messages.insert(id.to_owned(), body.clone());
        held.tell(&Change::Message(id.to_owned()));
        Ok(true)
    }

    fn presences(&self) -> Vec<Presence> {
        let held = self.held();
        held.presences
            .values()
            .cloned()
            .map(|mut presence| {
                let owner = presence.body.pointer("/owner/pid").and_then(Value::as_i64);
                if presence.elsewhere.is_none()
                    && owner.is_some_and(|pid| held.ended.contains(&pid))
                {
                    presence.lives = false;
                }
                presence
            })
            .collect()
    }

    fn ended(&self, pid: i32) {
        let mut held = self.held();
        held.ended.insert(pid.into());
        held.tell(&Change::Presence);
    }

    fn present(&self, pid: i32) {
        let mut held = self.held();
        if held.ended.remove(&pid.into()) {
            held.tell(&Change::Presence);
        }
    }

    fn claim(&self, machine: &str) -> io::Result<Option<Claim>> {
        if !self.held().claimed.insert(machine.to_owned()) {
            return Ok(None);
        }
        Ok(Some(Box::new(Claimed {
            store: self.clone(),
            machine: machine.to_owned(),
        })))
    }

    fn linked_until(&self, machine: &str) -> Option<i64> {
        self.held().linked.get(machine).copied()
    }

    fn mark_linked(&self, machine: &str, at: i64) -> io::Result<()> {
        self.held().linked.insert(machine.to_owned(), at);
        Ok(())
    }

    fn admitted_at(&self, machine: &str) -> Option<i64> {
        self.held().admitted.get(machine).copied()
    }

    fn mark_admitted(&self, machine: &str, at: i64) -> io::Result<()> {
        self.held().admitted.insert(machine.to_owned(), at);
        Ok(())
    }

    fn mark_ended(&self, machine: &str, at: i64) -> io::Result<()> {
        self.held()
            .links_ended
            .entry(machine.to_owned())
            .or_default()
            .push(at);
        Ok(())
    }

    fn endings(&self, machine: &str, since: i64) -> usize {
        self.held()
            .links_ended
            .get(machine)
            .map_or(0, |ended| ended.iter().filter(|at| **at >= since).count())
    }

    fn refuse_link(&self, machine: &str, why: &str) -> io::Result<()> {
        self.held()
            .links_refused
            .insert(machine.to_owned(), why.to_owned());
        Ok(())
    }

    fn link_refused(&self, machine: &str) -> Option<String> {
        self.held().links_refused.get(machine).cloned()
    }

    fn link_admitted(&self, machine: &str) -> io::Result<()> {
        self.held().links_refused.remove(machine);
        Ok(())
    }

    fn put_presence(&self, session: &str, machine: &str, body: &Value) -> io::Result<()> {
        let mut held = self.held();
        if body.get("session").and_then(Value::as_str) != Some(session) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the presence names another session",
            ));
        }
        if held.presences.contains_key(session)
            && !held.carries(session, machine)
            && !held.ended_here(session)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "not that machine's session",
            ));
        }
        // As the folders do: only the session's own machine can judge its
        // process, and a link carrying it has reached that machine.
        let mut body = body.clone();
        if let Some(fields) = body.as_object_mut() {
            fields.remove("owner");
            fields.remove("unreachableSince");
            fields.remove("unreachableSeen");
        }
        held.presences.insert(
            session.to_owned(),
            Presence {
                session: session.to_owned(),
                body: Arc::new(body),
                elsewhere: Some(machine.to_owned()),
                lives: true,
                unreachable_since: None,
            },
        );
        held.tell(&Change::Presence);
        Ok(())
    }

    fn keep_presence(&self, _session: &str, _machine: &str) -> io::Result<()> {
        // Memory holds no times, so there is nothing to say again.
        Ok(())
    }

    fn mark_unreachable(
        &self,
        session: &str,
        machine: &str,
        since: i64,
        seen: i64,
    ) -> io::Result<()> {
        let mut held = self.held();
        if held.carries(session, machine)
            && let Some(presence) = held.presences.get_mut(session)
            && presence.body.get("unreachableSeen").is_none()
        {
            let since = *presence.unreachable_since.get_or_insert(since);
            // As the folders mark it, in the presence too, where easing
            // reads how long a lane has been out of reach.
            if let Some(fields) = Arc::make_mut(&mut presence.body).as_object_mut() {
                fields.insert("unreachableSince".to_owned(), Value::from(since));
                fields.insert("unreachableSeen".to_owned(), Value::from(seen));
            }
            held.tell(&Change::Presence);
        }
        Ok(())
    }

    fn remove_presence(&self, session: &str, machine: &str) -> io::Result<()> {
        let mut held = self.held();
        if held.carries(session, machine) {
            if let Some(presence) = held.presences.remove(session) {
                // The latest of each session a machine ran, as Folders
                // keeps it.
                held.gone.retain(|one| {
                    one.session != session || one.elsewhere.as_deref() != Some(machine)
                });
                held.gone.push(Presence {
                    lives: false,
                    unreachable_since: None,
                    ..presence
                });
            }
            if held.holds_for(session, machine) {
                held.holdings.remove(session);
            }
            held.tell(&Change::Presence);
        }
        Ok(())
    }

    fn gone(&self) -> Vec<Presence> {
        self.held().gone.clone()
    }

    fn holdings(&self) -> Vec<Value> {
        self.held().holdings.values().cloned().collect()
    }

    fn put_leases(&self, session: &str, machine: &str, leases: &[Value]) -> io::Result<()> {
        let mut held = self.held();
        let own = held
            .holdings
            .get(session)
            .is_some_and(|holding| holding.get("machine").is_none());
        if held.holdings.contains_key(session)
            && !held.holds_for(session, machine)
            && !(own && held.carries(session, machine))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "not that machine's holding",
            ));
        }
        if leases.is_empty() {
            held.holdings.remove(session);
        } else {
            held.holdings.insert(
                session.to_owned(),
                serde_json::json!({"session": session, "leases": leases, "machine": machine}),
            );
        }
        held.tell(&Change::Presence);
        Ok(())
    }

    fn machines(&self) -> Vec<Machine> {
        self.held().machines.values().cloned().collect()
    }

    fn put_machine(&self, machine: &Machine) -> io::Result<()> {
        let mut held = self.held();
        held.machines.insert(machine.name.clone(), machine.clone());
        held.tell(&Change::Machine(machine.name.clone()));
        Ok(())
    }

    fn told_carries(&self) -> BTreeMap<String, Vec<Value>> {
        self.held().carries.clone()
    }

    fn put_told_carries(&self, machine: &str, told: &[Value]) -> io::Result<()> {
        let mut held = self.held();
        if told.is_empty() {
            held.carries.remove(machine);
        } else {
            held.carries.insert(machine.to_owned(), told.to_vec());
        }
        held.tell(&Change::Carries(machine.to_owned()));
        Ok(())
    }

    fn changes(&self) -> io::Result<Changes> {
        let (sender, changes) = mpsc::unbounded_channel();
        self.held().told.push(sender);
        Ok(Changes {
            changes,
            keep: Box::new(()),
        })
    }
}
