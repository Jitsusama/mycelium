//! The node feeding its receptors' items to the cells whose quests claim
//! them (`feed`): each as a fyi from the receptor's own cell to that one
//! cell, which only this node speaks as, said once while the item stands.
//! Which cells claim what is the quests' record's word, read from their
//! READMEs, never a session's own presence, so no session can draw the
//! person's review notices to itself.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;

use super::Shared;
use crate::feed::{self, Claimant, Item};
use crate::mesh;
use crate::receptor::Source;
use crate::record::{Intent, State as SessionState};
use crate::stimuli::{self, Table};
use crate::store::Store;
use crate::wire::Draft;

/// The sources whose receptors keep items to feed: GitHub's alone, for
/// now, the reviews asked of the person.
const FED_BY: [Source; 1] = [Source::Github];

/// How often the node looks again without being told: a new cell, or a
/// quest's README claiming a change, shows within this.
const FEED_EVERY: Duration = Duration::from_secs(30);

impl<S: Store + Clone + Send + Sync + 'static> Shared<S> {
    /// Feeds each item of a receptor the person's table names to the
    /// live cells whose quests claim it, as the receptors or the quests
    /// change and every [`FEED_EVERY`], until the node ends. Nothing is
    /// fed where the node has no receptors' folder or the table names no
    /// source that feeds.
    pub(super) async fn feed_cells(self) {
        let Some(folder) = self.receptors_at.clone() else {
            return;
        };
        // What the table couldn't say is said once, by the signalling.
        let (table, _) = Table::read(self.stimuli.as_deref(), &self.runs.declarers);
        let sources: Vec<Source> = FED_BY
            .into_iter()
            .filter(|source| table.says(*source))
            .collect();
        if sources.is_empty() {
            return;
        }
        let mut receptors = self.said.receptors.subscribe();
        let mut forages = self.said.forages.subscribe();
        let mut every = tokio::time::interval(FEED_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                told = receptors.changed() => if told.is_err() { return },
                told = forages.changed() => if told.is_err() { return },
                _ = every.tick() => {}
            }
            self.feed_once(&folder, &sources).await;
        }
    }

    /// One pass: each source's items read afresh, fed to whoever claims
    /// them now, and the records' watchers told when anything changed.
    pub(super) async fn feed_once(&self, folder: &Path, sources: &[Source]) {
        let now = Timestamp::now();
        let items = read_items(folder.to_owned(), sources.to_vec(), self.realm.clone(), now).await;
        let claimants = self.claimants();
        let mut moved = false;
        for (source, items) in items {
            let fed = {
                let mut feeders = self
                    .feeders
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let feeder = feeders.entry(source).or_default();
                let shown = feeder.shown();
                let fed = feeder.feed(&items, &claimants, &self.realm, now);
                moved |= feeder.shown() != shown;
                fed
            };
            for each in fed {
                let Some(draft) = fed_draft(source, &self.machine, &each.cell, &each.item) else {
                    continue;
                };
                if let Err(why) = self.speak(&draft) {
                    eprintln!("mycelium: {} couldn't be said: {why}", draft.from);
                }
            }
        }
        if moved {
            self.said.fed.send_modify(|said| *said += 1);
        }
    }

    /// This machine's cells that work on a quest, each with what its
    /// quest's README claims, asking for each quest not yet read to be
    /// read. A cell whose quest isn't read yet claims nothing until it is.
    fn claimants(&self) -> Vec<Claimant> {
        let sessions: Vec<_> = self
            .lock()
            .state
            .sessions()
            .into_iter()
            .filter(|session| session.machine == self.machine)
            .collect();
        let mut unread = Vec::new();
        let claimants = {
            let mut read_quests = self
                .forages
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sessions
                .into_iter()
                .filter_map(|session| {
                    let quest = session.quest?;
                    let Some(read) = read_quests.quest(&quest) else {
                        unread.push(quest);
                        return None;
                    };
                    let claims = read.claims.clone();
                    Some(Claimant {
                        address: session.address,
                        session: session.id,
                        live: matches!(session.state, SessionState::Busy | SessionState::Idle),
                        realm: claims.realm,
                        claims: claims.changes,
                        sessions: claims.sessions,
                    })
                })
                .collect()
        };
        if let Some(to_read) = &self.to_read {
            for quest in unread {
                // Nothing reads quests once the node has stopped.
                let _ = to_read.send(quest);
            }
        }
        claimants
    }

    /// What each receptor fed lately, as the daemon interface holds it.
    pub(super) fn fed_records(&self) -> BTreeMap<String, serde_json::Value> {
        let now = Timestamp::now();
        self.feeders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .flat_map(|(source, feeder)| feeder.records(*source, &self.machine, now))
            .collect()
    }
}

/// Each of `sources`' items in `folder` for `realm`, read off the
/// runtime's threads, since a file read blocks.
async fn read_items(
    folder: PathBuf,
    sources: Vec<Source>,
    realm: String,
    now: Timestamp,
) -> Vec<(Source, Vec<Item>)> {
    tokio::task::spawn_blocking(move || {
        sources
            .into_iter()
            .map(|source| (source, feed::read(&folder, source, &realm, now)))
            .collect()
    })
    .await
    // A read that panicked feeds nothing this pass.
    .unwrap_or_default()
}

/// The fyi feeding `item` to the cell at `address`, from `source`'s
/// receptor of `machine`, in words built from the item's fields alone;
/// none for an address that isn't letters and digits alone, the one
/// shape a session takes a fed signal to (the harness's `ONE_CELL`).
fn fed_draft(source: Source, machine: &str, address: &str, item: &Item) -> Option<Draft> {
    if address.is_empty() || !address.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return None;
    }
    let group = mesh::group_for(&format!("@{address}"))?;
    Some(Draft {
        from: stimuli::cell(source, machine),
        groups: vec![group],
        intent: Some(Intent::Fyi),
        text: item.words(source),
        re: None,
        realm: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fed_item_goes_to_its_one_cell_from_the_receptor_alone() {
        let item = Item {
            repo: "jsama/tern".to_owned(),
            number: 12,
            reason: feed::Reason::ReviewAsked,
            since: "2026-11-18T08:00:00Z".parse().expect("a time"),
        };
        let draft = fed_draft(Source::Github, "kestrel", "7M6JKG", &item).expect("a draft");
        assert_eq!(draft.from, "github@kestrel");
        assert_eq!(
            draft
                .groups
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            vec!["peer.7m6jkg".to_owned()],
            "to that one cell alone"
        );
        assert_eq!(draft.intent, Some(Intent::Fyi));
        assert_eq!(draft.text, item.words(Source::Github));
        for address in ["7M6JKG/elsewhere", "7M6JKG.x", "", "7M6 JKG"] {
            assert!(
                fed_draft(Source::Github, "kestrel", address, &item).is_none(),
                "{address:?} is no one cell, so is fed nothing"
            );
        }
    }
}
