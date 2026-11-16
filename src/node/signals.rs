//! The node saying its receptors' climbs on their networks (`stimuli`):
//! as the receptor's own cell, `<source>@<machine>`, which only this node
//! speaks as. A client can't send as a name with `@` in it, a link never
//! carries one, and a message from a receptor's name is taken only when
//! this node said it this run, so one planted in the store is no signal.

use std::collections::BTreeMap;

use jiff::Timestamp;

use super::Shared;
use crate::mesh;
use crate::record::Message;
use crate::stimuli::{Signals, Table, speaks_as_receptor};
use crate::store::Store;
use crate::wire::Draft;

impl<S: Store + Clone + Send + Sync + 'static> Shared<S> {
    /// Says each receptor's climb the person's table names, as the
    /// receptors are judged again, until the node ends.
    pub(super) async fn signal_receptors(self) {
        let (table, why) = Table::read(self.stimuli.as_deref(), &self.runs.declarers);
        if let Some(why) = why {
            eprintln!("mycelium: {why}");
        }
        let mut told = self.said.receptors.subscribe();
        let mut signals = Signals::default();
        loop {
            let held: BTreeMap<_, _> = self
                .receptors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            for draft in signals.after(&held, &table, &self.machine, Timestamp::now()) {
                if let Err(why) = self.speak(&draft) {
                    eprintln!("mycelium: {} couldn't be said: {why}", draft.from);
                }
            }
            if told.changed().await.is_err() {
                return;
            }
        }
    }

    /// Writes one of its receptors' signals to the store, vouched for
    /// first, so the store's change brings it back as this node's own.
    fn speak(&self, draft: &Draft) -> std::io::Result<()> {
        let (id, body) = mesh::written(draft, std::process::id());
        self.spoken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.clone());
        self.store.put_message(&id, &body)
    }

    /// Whether `message` is one the node takes: any but one from a
    /// receptor's name, which it takes only when it said it this run.
    pub(super) fn takes(&self, message: &Message) -> bool {
        !speaks_as_receptor(&message.from)
            || self
                .spoken
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&message.id)
    }
}
