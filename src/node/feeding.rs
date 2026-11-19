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
use crate::receptor::{self, Source};
use crate::record::{Intent, State as SessionState};
use crate::stimuli::{self, Table};
use crate::store::Store;
use crate::wire::Draft;

/// The sources whose receptors keep items to feed: GitHub's, the reviews
/// asked of the person, and calls', the calls a summary is written of.
const FED_BY: [Source; 2] = [Source::Github, Source::Calls];

/// The sources fed from: those that keep items, of those the person's
/// table `says`, so a source the table leaves out feeds no cell either.
fn feeding_sources(says: impl Fn(Source) -> bool) -> Vec<Source> {
    FED_BY.into_iter().filter(|source| says(*source)).collect()
}

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
        let sources = feeding_sources(|source| table.says(source));
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
        let mut feeders = self
            .feeders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (source, items) in items {
            let feeder = feeders.entry(source).or_default();
            // Each feeding is said before it's kept, so one that couldn't
            // be said is neither served nor marked fed, and is tried again.
            let feeding = feeder.feed_saying(&items, &claimants, &self.realm, now, |each| {
                let Some(draft) = fed_draft(source, &self.machine, &each.cell, &each.item) else {
                    return false;
                };
                self.speak(&draft)
                    .map_err(|why| eprintln!("mycelium: {} couldn't be said: {why}", draft.from))
                    .is_ok()
            });
            moved |= feeding.moved;
        }
        // A source left out above, its receptor off or its file unsound,
        // still lets its feedings go once they're ten minutes old.
        for feeder in feeders.values_mut() {
            moved |= feeder.fade(now);
        }
        drop(feeders);
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
                        changes: claims.changes,
                        events: claims.events,
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
///
/// A source whose receptor reads off, or keeps no record, is left out, so
/// what a stopped receptor last named isn't fed to cells that come later;
/// so is one whose items file is there but unsound, so its feeding stands
/// as it was rather than starting over once the file reads again.
async fn read_items(
    folder: PathBuf,
    sources: Vec<Source>,
    realm: String,
    now: Timestamp,
) -> Vec<(Source, Vec<Item>)> {
    tokio::task::spawn_blocking(move || {
        let receptors = receptor::judged(&folder, &realm, now, &receptor::Here);
        sources
            .into_iter()
            .filter(|source| {
                receptors
                    .get(source)
                    .is_some_and(|record| record.state != receptor::State::Off)
            })
            .filter_map(|source| Some((source, feed::read(&folder, source, &realm, now)?)))
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

/// A record of `source`'s receptor in `folder`, kept by this process and
/// reading, so the node reads it as running.
#[cfg(test)]
pub(super) fn reading(folder: &Path, source: Source, realm: &str) {
    let now = Timestamp::now();
    let pid = std::process::id();
    let record = receptor::Record {
        source,
        realm: realm.to_owned(),
        state: receptor::State::Reading,
        since: now,
        read_at: now,
        every: 300,
        pid,
        started: receptor::started_at(pid),
        counts: receptor::Counts {
            hour: 1,
            today: 1,
            addressed: 1,
        },
    };
    receptor::write(folder, realm, record, &receptor::Here).expect("its record is kept");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_source_the_table_says_is_fed_from() {
        assert_eq!(feeding_sources(|_| true), [Source::Github, Source::Calls]);
        assert_eq!(
            feeding_sources(|source| source != Source::Github),
            [Source::Calls],
            "a table leaving GitHub out still fed from it"
        );
        assert!(
            feeding_sources(|source| !matches!(source, Source::Github | Source::Calls)).is_empty(),
            "a table leaving both out feeds"
        );
        assert!(
            feeding_sources(|_| false).is_empty(),
            "a silent table feeds nothing"
        );
    }

    #[tokio::test]
    async fn only_a_running_receptors_sound_items_are_read() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let folder = dir.path().join("receptors");
        let now = Timestamp::now();
        let asked = [(
            feed::Named::Change("jsama/tern".to_owned(), 12),
            feed::Reason::ReviewAsked,
        )];
        feed::write(&folder, Source::Github, "work", &asked, now).expect("kept");
        let read = || read_items(folder.clone(), vec![Source::Github], "work".to_owned(), now);
        assert!(
            read().await.is_empty(),
            "a receptor with no record feeds nothing"
        );

        reading(&folder, Source::Github, "work");
        let items = read().await;
        assert_eq!(
            items
                .iter()
                .map(|(_, items)| items.len())
                .collect::<Vec<_>>(),
            [1],
            "a running receptor's item is read"
        );

        // The same record, its process since ended, reads off.
        let mut ended = std::process::Command::new("true")
            .spawn()
            .expect("a process runs");
        ended.wait().expect("it ended");
        let record = receptor::file(&folder, Source::Github);
        let mut gone: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&record).expect("kept")).expect("json");
        gone["pid"] = ended.id().into();
        std::fs::write(&record, gone.to_string()).expect("planted");
        assert!(
            read().await.is_empty(),
            "a receptor that reads off still fed what it last named"
        );
        reading(&folder, Source::Github, "work");

        std::fs::write(feed::file(&folder, Source::Github), b"{ not whole").expect("planted");
        assert!(
            read().await.is_empty(),
            "an unsound file is left out, not read as none"
        );
        std::fs::remove_file(feed::file(&folder, Source::Github)).expect("gone");
        assert_eq!(
            read().await,
            [(Source::Github, Vec::new())],
            "a file gone holds none"
        );
    }

    #[test]
    fn a_fed_item_goes_to_its_one_cell_from_the_receptor_alone() {
        let item = Item::new(
            feed::Named::Change("jsama/tern".to_owned(), 12),
            feed::Reason::ReviewAsked,
            "2026-11-18T08:00:00Z".parse().expect("a time"),
        );
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
