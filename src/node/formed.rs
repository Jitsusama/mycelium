//! The tree networks that have formed, kept across the node's restarts.
//!
//! A tree's network forms once two cells hold the tree at once, and stays
//! while one does, so a network two sessions shared isn't lost the moment
//! one of them ends. Once none holds it, it is kept 10 minutes more, so a
//! cell starting again soon after rejoins it, and then it fades. Each is
//! kept as a `formed:<network name>` record, saying when it formed (`at`)
//! and, while none holds it, when its last cell went (`left`), each in
//! milliseconds since the epoch, as ties keep theirs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::watch;

use crate::daemon;
pub use crate::networks::FORMED as KIND;

/// How long a network no cell holds is kept before it fades, in
/// milliseconds.
pub const FADES_AFTER: i64 = 10 * 60 * 1000;

/// One formed network, as kept in its file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Kept {
    /// When two cells first held its tree at once.
    at: i64,
    /// When its last cell went, while none holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    left: Option<i64>,
}

impl Kept {
    /// Whether it stands at `now`: held by a cell, or left less than
    /// [`FADES_AFTER`] before.
    fn stands(&self, now: i64) -> bool {
        self.left.is_none_or(|left| now - left < FADES_AFTER)
    }
}

/// The tree networks formed, by name, as kept in their file.
pub struct Formed {
    file: PathBuf,
    held: Mutex<BTreeMap<String, Kept>>,
    /// Told each time what has formed changes.
    said: watch::Sender<u64>,
}

impl Formed {
    /// What `realm`'s own file beside `file` says has formed, as
    /// [`Formed::at`] reads it: one realm's node never reads another's,
    /// though each keeps its state in the machine's one folder.
    pub fn beside(file: &Path, realm: &str) -> Self {
        Self::at(file.with_file_name(format!("formed-{realm}.json")))
    }

    /// What `file` says has formed, nothing where it doesn't exist yet or
    /// can't be read, since a network lost only shows again once two
    /// cells share its tree.
    pub fn at(file: PathBuf) -> Self {
        let held = match fs::read(&file) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                eprintln!(
                    "{} can't be read ({error}); no network is formed",
                    file.display()
                );
                BTreeMap::new()
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => {
                eprintln!(
                    "{} can't be read ({error}); no network is formed",
                    file.display()
                );
                BTreeMap::new()
            }
        };
        Self {
            file,
            held: Mutex::new(held),
            said: watch::Sender::new(0),
        }
    }

    /// A watch told each time a network forms, is left, is held again or
    /// fades.
    pub fn told(&self) -> watch::Receiver<u64> {
        self.said.subscribe()
    }

    /// The names of the networks formed and not faded at `now`.
    pub fn names(&self, now: i64) -> BTreeSet<String> {
        self.lock()
            .iter()
            .filter(|(_, kept)| kept.stands(now))
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Takes in how many cells hold each tree network made at `now`, by
    /// name: one two hold forms, one none holds is left, and one left
    /// [`FADES_AFTER`] ago fades. Keeps it in its file where anything
    /// changed, and says whether it did.
    pub fn saw(&self, holding: &BTreeMap<String, usize>, now: i64) -> bool {
        let mut held = self.lock();
        // One faded stays gone, though a cell holds its tree again.
        let before = held.len();
        held.retain(|_, kept| kept.stands(now));
        let mut changed = held.len() != before;
        for (name, cells) in holding {
            if *cells > 1 && !held.contains_key(name) {
                held.insert(
                    name.clone(),
                    Kept {
                        at: now,
                        left: None,
                    },
                );
                changed = true;
            }
        }
        held.retain(|name, kept| {
            if holding.get(name).is_some_and(|cells| *cells > 0) {
                changed |= kept.left.take().is_some();
                return true;
            }
            let left = *kept.left.get_or_insert_with(|| {
                changed = true;
                now
            });
            let stays = now - left < FADES_AFTER;
            changed |= !stays;
            stays
        });
        if changed && let Err(error) = keep(&self.file, &held) {
            eprintln!(
                "{} can't be kept ({error}); a restart forgets what formed or faded since",
                self.file.display()
            );
        }
        if changed {
            self.said.send_modify(|said| *said += 1);
        }
        changed
    }

    /// Each network formed and not faded at `now` as the daemon interface
    /// holds it, heard when it formed, or when its last cell went.
    pub fn records(&self, now: i64) -> BTreeMap<String, daemon::Record> {
        self.lock()
            .iter()
            .filter(|(_, kept)| kept.stands(now))
            .map(|(name, kept)| {
                let fields = json!({"name": name, "at": kept.at, "left": kept.left});
                let heard = kept.left.unwrap_or(kept.at);
                (
                    daemon::key(KIND, name),
                    daemon::Record::from(daemon::record(KIND, name, heard, fields)),
                )
            })
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Kept>> {
        // A panic while holding it leaves what formed as it was before the
        // change that panicked, so carrying on with it is sound.
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Writes `held` to `file` whole, beside it first and renamed into place,
/// so a reader never finds it half written.
fn keep(file: &Path, held: &BTreeMap<String, Kept>) -> io::Result<()> {
    let beside = file.with_extension("json.new");
    fs::write(&beside, serde_json::to_vec(held)?)?;
    fs::rename(&beside, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIDES: &str = "tree.github:harbour/oars:jsama/tides";

    fn holding(cells: &[(&str, usize)]) -> BTreeMap<String, usize> {
        cells
            .iter()
            .map(|(name, cells)| ((*name).to_owned(), *cells))
            .collect()
    }

    fn formed(dir: &Path) -> Formed {
        Formed::at(dir.join("formed.json"))
    }

    #[test]
    fn a_tree_forms_once_two_cells_hold_it_and_stays_across_a_restart_while_one_does() {
        let dir = tempfile::tempdir().expect("a folder");
        let start = 1_763_300_000_000;
        assert!(!formed(dir.path()).saw(&holding(&[(TIDES, 1)]), start));
        assert!(
            formed(dir.path()).names(start).is_empty(),
            "one cell forms none"
        );
        assert!(formed(dir.path()).saw(&holding(&[(TIDES, 2)]), start + 1));
        let again = formed(dir.path());
        assert_eq!(
            again.names(start + 1),
            BTreeSet::from([TIDES.to_owned()]),
            "two formed it, and a node started again remembers it"
        );
        assert!(
            !again.saw(&holding(&[(TIDES, 1)]), start + 2),
            "one keeps it"
        );
        let fields = again.records(start + 2)[&format!("formed:{TIDES}")].value();
        assert_eq!(
            (&fields["at"], &fields["left"], &fields["heard"]),
            (
                &json!(start + 1),
                &serde_json::Value::Null,
                &json!(start + 1)
            ),
            "kept as when it formed: {fields}"
        );
    }

    #[test]
    fn a_tree_none_holds_fades_ten_minutes_after_its_last_cell_went() {
        let dir = tempfile::tempdir().expect("a folder");
        let kept = formed(dir.path());
        let start = 1_763_300_000_000;
        kept.saw(&holding(&[(TIDES, 2)]), start);
        assert!(kept.saw(&BTreeMap::new(), start + 1), "its last cell went");
        let fields = kept.records(start + 1)[&format!("formed:{TIDES}")].value();
        assert_eq!(fields["left"], json!(start + 1), "{fields}");
        kept.saw(&BTreeMap::new(), start + FADES_AFTER);
        assert_eq!(
            kept.names(start + FADES_AFTER),
            BTreeSet::from([TIDES.to_owned()]),
            "within its ten minutes it stays"
        );
        kept.saw(&holding(&[(TIDES, 1)]), start + FADES_AFTER);
        kept.saw(&BTreeMap::new(), start + 2 * FADES_AFTER);
        assert_eq!(
            kept.names(start + 2 * FADES_AFTER),
            BTreeSet::from([TIDES.to_owned()]),
            "a cell holding it again started its ten minutes afresh"
        );
        assert!(kept.saw(&BTreeMap::new(), start + 3 * FADES_AFTER + 1));
        assert!(
            kept.names(start + 3 * FADES_AFTER + 1).is_empty(),
            "past them it fades"
        );
        assert!(
            formed(dir.path())
                .names(start + 3 * FADES_AFTER + 1)
                .is_empty(),
            "and a node started again forgets it too"
        );
    }

    #[test]
    fn a_faded_tree_is_served_by_none_and_one_cell_back_forms_it_not() {
        let dir = tempfile::tempdir().expect("a folder");
        let kept = formed(dir.path());
        let start = 1_763_300_000_000;
        kept.saw(&holding(&[(TIDES, 2)]), start);
        kept.saw(&BTreeMap::new(), start + 1);
        let past = start + 1 + FADES_AFTER;
        assert!(
            kept.names(past).is_empty() && kept.records(past).is_empty(),
            "past its ten minutes it is served by none, though nothing looked since"
        );
        kept.saw(&holding(&[(TIDES, 1)]), past);
        assert!(
            kept.names(past).is_empty(),
            "one cell back forms nothing alone once it faded"
        );
        kept.saw(&holding(&[(TIDES, 2)]), past + 1);
        let formed = kept.records(past + 1);
        assert_eq!(
            formed
                .values()
                .map(|record| record.value()["at"].as_i64())
                .collect::<Vec<_>>(),
            vec![Some(past + 1)],
            "two cells back form it afresh, from when they did"
        );
    }

    #[test]
    fn a_watch_of_formed_networks_is_told_as_one_forms_is_left_and_fades() {
        let dir = tempfile::tempdir().expect("a folder");
        let kept = formed(dir.path());
        let mut told = kept.told();
        let start = 1_763_300_000_000;
        kept.saw(&holding(&[(TIDES, 2)]), start);
        assert!(told.has_changed().expect("told"), "told it formed");
        told.mark_unchanged();
        kept.saw(&holding(&[(TIDES, 2)]), start + 1);
        assert!(
            !told.has_changed().expect("told"),
            "not told while nothing changed"
        );
        kept.saw(&BTreeMap::new(), start + 2);
        assert!(told.has_changed().expect("told"), "told it was left");
        told.mark_unchanged();
        kept.saw(&BTreeMap::new(), start + 2 + FADES_AFTER);
        assert!(
            told.has_changed().expect("told"),
            "told it faded, with no cell moving"
        );
    }
}
