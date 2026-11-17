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
    /// Whether what forms is written to `file`: not where what stood there
    /// couldn't be read or moved aside, so it is left for the next start.
    keeps: bool,
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
    /// cells share its tree. One that doesn't parse is moved aside first,
    /// so what it held can still be seen once it is written anew. One that
    /// can't be opened, or moved aside, is left where it is and never
    /// written, since a read that failed may pass at the next start: what
    /// forms is then kept only while the node runs.
    pub fn at(file: PathBuf) -> Self {
        let (held, keeps) = match fs::read(&file) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(held) => (held, true),
                Err(error) => (BTreeMap::new(), set_aside(&file, &error.to_string())),
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => (BTreeMap::new(), true),
            Err(error) => {
                eprintln!(
                    "{} can't be read ({error}); no network is formed from it, and it is \
                     left as it is, so what forms is kept only while the node runs",
                    file.display()
                );
                (BTreeMap::new(), false)
            }
        };
        Self {
            file,
            keeps,
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
        if changed
            && self.keeps
            && let Err(error) = keep(&self.file, &held)
        {
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

/// Moves `file`, which doesn't parse for `why`, aside beside it as
/// `<file>.bad.<millis>`, a name no earlier one holds, saying so, since the
/// next network to form or fade would write over it. Says whether it
/// moved, and so whether `file` may be written.
fn set_aside(file: &Path, why: &str) -> bool {
    let millis = jiff::Timestamp::now().as_millisecond();
    let named = |n: u32| {
        let mut aside = file.as_os_str().to_owned();
        aside.push(format!(".bad.{millis}"));
        if n > 0 {
            aside.push(format!("-{n}"));
        }
        PathBuf::from(aside)
    };
    // Two set aside in one millisecond would take a hundred node starts.
    let free = (0..100).map(named).find(|aside| !aside.exists());
    match free.map_or_else(
        || Err(io::Error::other("every name beside it is taken")),
        |aside| fs::rename(file, &aside).map(|()| aside),
    ) {
        Ok(aside) => {
            eprintln!(
                "{} can't be read ({why}); no network is formed from it, and it is kept as {}",
                file.display(),
                aside.display()
            );
            true
        }
        Err(error) => {
            eprintln!(
                "{} can't be read ({why}); no network is formed from it, and it can't be \
                 kept aside ({error}), so it is left as it is and what forms is kept only \
                 while the node runs",
                file.display()
            );
            false
        }
    }
}

/// Writes `held` to `file` whole, beside it first, flushed to the disk and
/// renamed into place, so a reader never finds it half written, even after
/// the machine stops at once.
fn keep(file: &Path, held: &BTreeMap<String, Kept>) -> io::Result<()> {
    let beside = file.with_extension("json.new");
    let mut written = fs::File::create(&beside)?;
    io::Write::write_all(&mut written, &serde_json::to_vec(held)?)?;
    written.sync_all()?;
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

    /// What each file set aside beside `dir`'s `formed.json` holds.
    fn set_aside(dir: &Path) -> Vec<Vec<u8>> {
        let mut aside: Vec<PathBuf> = fs::read_dir(dir)
            .expect("it reads")
            .map(|entry| entry.expect("an entry").path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("formed.json.bad"))
            })
            .collect();
        aside.sort();
        aside
            .iter()
            .map(|path| fs::read(path).expect("it reads"))
            .collect()
    }

    #[test]
    fn a_file_that_cant_be_read_is_kept_aside_before_anything_writes() {
        let dir = tempfile::tempdir().expect("a folder");
        let file = dir.path().join("formed.json");
        let torn = br#"{"tree.github:harbour/oars:jsama/tides":{"at":17"#;
        fs::write(&file, torn).expect("a torn file");
        let formed = Formed::at(file.clone());
        let start = 1_763_300_000_000;
        assert!(formed.names(start).is_empty(), "nothing is read from it");
        assert!(formed.saw(&holding(&[(TIDES, 2)]), start));
        assert_eq!(
            set_aside(dir.path()),
            vec![torn.to_vec()],
            "what was lost can still be seen once a network forms anew"
        );
        assert_eq!(
            Formed::at(file.clone()).names(start),
            BTreeSet::from([TIDES.to_owned()]),
            "and the file holds what formed since"
        );
        fs::write(&file, b"also torn").expect("a second torn file");
        Formed::at(file);
        assert_eq!(
            set_aside(dir.path()),
            vec![torn.to_vec(), b"also torn".to_vec()],
            "a second is kept beside the first, never over it"
        );
    }

    #[test]
    fn a_file_that_cant_be_opened_is_left_where_it_is_and_never_written() {
        let dir = tempfile::tempdir().expect("a folder");
        let file = dir.path().join("formed.json");
        fs::create_dir(&file).expect("a folder where the file goes");
        let formed = Formed::at(file.clone());
        let start = 1_763_300_000_000;
        assert!(formed.saw(&holding(&[(TIDES, 2)]), start));
        assert_eq!(
            formed.names(start),
            BTreeSet::from([TIDES.to_owned()]),
            "it forms while the node runs"
        );
        assert!(file.is_dir(), "what stood there is left as it was");
        assert!(set_aside(dir.path()).is_empty(), "and not moved aside");
        assert!(
            !dir.path().join("formed.json.new").exists(),
            "nor written beside, since a read that failed may pass next start"
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
