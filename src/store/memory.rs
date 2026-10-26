//! A store held in memory, for tests: what the mesh's folders hold, with
//! the machine's processes stood in for by whether each session lives.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;
use tokio::sync::mpsc;

use super::{Change, Changes, Presence, Store};

/// A store in memory, shared by its clones as the folders are.
#[derive(Clone, Default)]
pub struct Memory(Arc<Mutex<Held>>);

#[derive(Default)]
struct Held {
    messages: BTreeMap<String, Value>,
    presences: BTreeMap<String, Presence>,
    told: Vec<mpsc::UnboundedSender<Change>>,
}

impl Memory {
    /// Starts a session of this machine's, living or ended.
    pub fn start_here(&self, session: &str, body: Value, lives: bool) {
        let mut held = self.held();
        held.presences.insert(
            session.to_owned(),
            Presence {
                session: session.to_owned(),
                body,
                elsewhere: None,
                lives,
            },
        );
        held.tell(&Change::Presence);
    }

    /// Sends a message from this machine, as a session here would.
    pub fn send_here(&self, id: &str, body: Value) {
        let mut held = self.held();
        held.messages.insert(id.to_owned(), body);
        held.tell(&Change::Message(id.to_owned()));
    }

    fn held(&self) -> MutexGuard<'_, Held> {
        // A test that panicked holding the lock has failed already.
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Held {
    fn tell(&mut self, change: &Change) {
        self.told
            .retain(|sender| sender.send(change.clone()).is_ok());
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
        self.held().messages.get(id).cloned()
    }

    fn put_message(&self, id: &str, body: &Value) -> io::Result<()> {
        let mut held = self.held();
        if !held.messages.contains_key(id) {
            held.messages.insert(id.to_owned(), body.clone());
            held.tell(&Change::Message(id.to_owned()));
        }
        Ok(())
    }

    fn presences(&self) -> Vec<Presence> {
        self.held().presences.values().cloned().collect()
    }

    fn put_presence(&self, session: &str, machine: &str, body: Value) -> io::Result<()> {
        let mut held = self.held();
        held.presences.insert(
            session.to_owned(),
            Presence {
                session: session.to_owned(),
                body,
                elsewhere: Some(machine.to_owned()),
                lives: true,
            },
        );
        held.tell(&Change::Presence);
        Ok(())
    }

    fn remove_presence(&self, session: &str) -> io::Result<()> {
        let mut held = self.held();
        if held
            .presences
            .get(session)
            .is_some_and(|presence| presence.elsewhere.is_some())
        {
            held.presences.remove(session);
            held.tell(&Change::Presence);
        }
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
