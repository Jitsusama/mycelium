//! What this machine knows of a shared quest's files: each one's content
//! and version, as it last saw them, kept in mycelium's own folder.
//!
//! A machine adds one to its own count in a file's version when it sees the
//! file's content change on its disk, so the ledger is how it tells a
//! change it made from one it took: a file it took is entered as taken, and
//! a scan then finds nothing new in it. Each file's size and modification
//! time are kept too, so a scan hashes only what may have changed, once
//! they can be trusted: a file's clock ticks coarsely, so one changed
//! again within a tick of being hashed keeps its time, and a size and time
//! that recent are hashed again rather than kept, as git does.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustix::fs::FlockOperation;
use serde::{Deserialize, Serialize};

use super::clones::Clone;
use super::settle::State;
use super::tree::{Quest, Shared};
use super::version::Version;

/// One file as this machine last saw it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Its content's hash, or `None` once deleted.
    pub hash: Option<String>,
    /// Which machines' changes it has seen.
    pub version: Version,
    /// Its size and modification time on this disk when last hashed, so
    /// an unchanged file isn't hashed again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen: Option<Seen>,
}

/// A file's size and modification time on this disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    /// Its length in bytes.
    pub len: u64,
    /// When it was last modified, in nanoseconds since the epoch.
    pub modified: u128,
}

impl Entry {
    /// The state another machine is told of.
    #[must_use]
    pub fn state(&self) -> State {
        State {
            hash: self.hash.clone(),
            version: self.version.clone(),
        }
    }
}

/// What this machine knows of one shared quest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    /// Each file, by its key ([`key_of`]).
    pub files: BTreeMap<String, Entry>,
    /// The journey both ends last held, which a merge of two journeys
    /// written apart is taken against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journey: Option<String>,
    /// The folders of the quest's scratch that are shared, by name.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub scratch: BTreeSet<String>,
    /// The git clones in those folders, by their paths in the scratch, as
    /// this machine last found them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub clones: BTreeMap<String, Clone>,
    /// What each other machine last said it holds of the quest's files,
    /// or was seen to take, by machine and key: only those this machine
    /// holds too, so a file deleted drops out. A machine named here has
    /// said it shares the quest, even holding none of it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub theirs: BTreeMap<String, BTreeMap<String, State>>,
    /// Whether the quest is shared only as one above another shared quest,
    /// so it stops once no shared quest is under it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub above: bool,
}

/// A shared file's key in a ledger and on a link: its place and its path.
#[must_use]
pub fn key_of(shared: &Shared) -> String {
    let place = match shared.place {
        super::tree::Place::Record => "record",
        super::tree::Place::Scratch => "scratch",
    };
    format!("{place}/{}", shared.path)
}

/// The shared file a key names, if it names one.
#[must_use]
pub fn shared_of(key: &str) -> Option<Shared> {
    let (place, path) = key.split_once('/')?;
    let place = match place {
        "record" => super::tree::Place::Record,
        "scratch" => super::tree::Place::Scratch,
        _ => return None,
    };
    Some(Shared {
        place,
        path: path.to_owned(),
    })
}

/// The coarsest a filesystem's clock may tick, FAT's two seconds; Linux's
/// own tick its timer, a few milliseconds.
const GRAIN: Duration = Duration::from_secs(2);

/// Now, in nanoseconds since the epoch, as a file's time is kept.
#[must_use]
pub fn clock() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos())
}

/// `seen`, read at `read_at`, if it may stand for the content read then:
/// only once the file was last changed a clock's grain before, since a
/// change within the same tick leaves the same time.
#[must_use]
pub fn lasting(seen: Seen, read_at: u128) -> Option<Seen> {
    (seen.modified + GRAIN.as_nanos() < read_at).then_some(seen)
}

/// A file's size and modification time, if it is there.
pub fn seen(path: &Path) -> io::Result<Option<Seen>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(Seen {
            len: metadata.len(),
            modified: metadata
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos()),
        })),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// What a look at a shared file found, against what this machine last saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sight {
    /// As it was.
    Same,
    /// The same content, its size and time now standing, which the ledger
    /// keeps so the file isn't read again until they change.
    Settled,
    /// Its content changed, or it came or went.
    Changed,
}

impl Sight {
    /// Whether the ledger holds something new of the file, to be kept.
    #[must_use]
    pub fn is_new(self) -> bool {
        self != Self::Same
    }
}

/// What a look at every file of a quest found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// The key of each file whose content changed, or that came or went.
    pub changed: Vec<String>,
    /// Whether the ledger holds something new of any file, to be kept.
    pub is_new: bool,
}

impl Ledger {
    /// Looks at every file of `quest` against what this machine last saw,
    /// adding one to `machine`'s count for each whose content changed,
    /// came or went. Only a file `quest` carries is found gone, so a quest
    /// walking some of its folders finds nothing gone from the rest.
    pub fn scan(&mut self, quest: &Quest, machine: &str) -> io::Result<Scan> {
        let mut scan = Scan::default();
        let mut present = HashSet::new();
        quest.each_file(&mut |shared| {
            let key = key_of(&shared);
            present.insert(key.clone());
            if let Some(now) = quest.seen_of(&shared)? {
                let sight = self.saw(quest, &shared, &key, now, machine)?;
                scan.is_new |= sight.is_new();
                if sight == Sight::Changed {
                    scan.changed.push(key);
                }
            }
            Ok(())
        })?;
        for (key, entry) in &mut self.files {
            if !present.contains(key)
                && shared_of(key)
                    .is_some_and(|shared| quest.carries(&shared) && quest.may_be_removed(&shared))
                && gone(entry, machine)
            {
                scan.changed.push(key.clone());
                scan.is_new = true;
            }
        }
        Ok(scan)
    }

    /// Looks at one file of `quest` against what this machine last saw, as
    /// [`Self::scan`] does.
    pub fn scan_one(&mut self, quest: &Quest, shared: &Shared, machine: &str) -> io::Result<Sight> {
        if !quest.carries(shared) {
            return Ok(Sight::Same);
        }
        let key = key_of(shared);
        match quest.seen_of(shared)? {
            Some(now) => self.saw(quest, shared, &key, now, machine),
            None if quest.may_be_removed(shared)
                && self
                    .files
                    .get_mut(&key)
                    .is_some_and(|entry| gone(entry, machine)) =>
            {
                Ok(Sight::Changed)
            }
            None => Ok(Sight::Same),
        }
    }

    /// Enters file `key`, seen on disk as `now`, hashing it only if it may
    /// have changed. A size and time that stand are entered beside its
    /// hash, so a file read once isn't read again until they change.
    fn saw(
        &mut self,
        quest: &Quest,
        shared: &Shared,
        key: &str,
        now: Seen,
        machine: &str,
    ) -> io::Result<Sight> {
        let entry = self.files.entry(key.to_owned()).or_default();
        if entry.seen == Some(now) && entry.hash.is_some() {
            return Ok(Sight::Same);
        }
        let read_at = clock();
        let hash = quest.content_of(shared)?.hash;
        let changed = entry.hash.as_deref() != Some(hash.as_str());
        if changed {
            entry.hash = Some(hash);
            entry.version = entry.version.changed_on(machine);
        }
        let seen = lasting(now, read_at);
        let settled = entry.seen != seen;
        entry.seen = seen;
        Ok(if changed {
            Sight::Changed
        } else if settled {
            Sight::Settled
        } else {
            Sight::Same
        })
    }

    /// Enters what `machine` said it holds of `files`, answering whether
    /// that changed what the ledger holds of it.
    pub fn heard(&mut self, machine: &str, files: BTreeMap<String, State>) -> bool {
        let before = self.theirs.get(machine).cloned();
        self.theirs
            .entry(machine.to_owned())
            .or_default()
            .extend(files);
        self.bound_theirs();
        self.theirs.get(machine) != before.as_ref()
    }

    /// Keeps what the other machines hold to the files this one holds,
    /// answering whether any dropped out.
    pub fn bound_theirs(&mut self) -> bool {
        let mut dropped = false;
        for held in self.theirs.values_mut() {
            let before = held.len();
            held.retain(|key, state| {
                state.hash.is_some()
                    && self
                        .files
                        .get(key)
                        .is_some_and(|entry| entry.hash.is_some())
            });
            dropped |= held.len() != before;
        }
        dropped
    }

    /// Every file's state, as the other end is told it.
    #[must_use]
    pub fn states(&self) -> BTreeMap<String, State> {
        self.files
            .iter()
            .map(|(key, entry)| (key.clone(), entry.state()))
            .collect()
    }
}

/// Enters a file as gone from this machine's disk, unless it was already,
/// answering whether it was there before.
fn gone(entry: &mut Entry, machine: &str) -> bool {
    if entry.hash.is_none() {
        return false;
    }
    entry.hash = None;
    entry.seen = None;
    entry.version = entry.version.changed_on(machine);
    true
}

/// Where this machine keeps its ledgers, one a shared quest.
#[derive(Debug, Clone)]
pub struct Ledgers {
    folder: PathBuf,
}

/// A ledger held for changing: nobody else on this machine reads or
/// writes it until it is let go, and it is written back with [`Held::keep`].
pub struct Held {
    /// The ledger.
    pub ledger: Ledger,
    path: PathBuf,
    /// Where the README's version as the quest ended was kept, when this
    /// ledger was begun from it, so keeping the ledger forgets it.
    seeded: Option<PathBuf>,
    _lock: fs::File,
}

impl Held {
    /// Writes the ledger back, by rename so no reader sees half of it.
    pub fn keep(&self) -> io::Result<()> {
        let staged = self.path.with_extension("json.mycelium.tmp");
        let text = serde_json::to_vec_pretty(&self.ledger).map_err(io::Error::other)?;
        fs::write(&staged, text)?;
        fs::rename(staged, &self.path)?;
        match self.seeded.as_ref().map(fs::remove_file) {
            Some(Err(error)) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

/// The README's key in a ledger.
const README: &str = "record/README.md";

impl Ledgers {
    /// The ledgers in mycelium's folder `state`.
    #[must_use]
    pub fn in_state(state: &Path) -> Self {
        Self {
            folder: state.join("shared"),
        }
    }

    /// Every quest this machine shares.
    pub fn quests(&self) -> io::Result<Vec<String>> {
        let entries = match fs::read_dir(&self.folder) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut quests = Vec::new();
        for entry in entries {
            let name = entry?.file_name();
            if let Some(quest) = name.to_str().and_then(|name| name.strip_suffix(".json"))
                && is_quest(quest)
            {
                quests.push(quest.to_owned());
            }
        }
        quests.sort();
        Ok(quests)
    }

    /// What this machine knows of `quest` as last kept, read without
    /// holding it, since [`Held::keep`] writes it whole by rename.
    ///
    /// # Errors
    ///
    /// When `quest` isn't a quest's id, or its ledger isn't there or
    /// can't be read.
    pub fn read(&self, quest: &str) -> io::Result<Ledger> {
        if !is_quest(quest) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{quest:?} is not a quest's id"),
            ));
        }
        let text = fs::read(self.folder.join(format!("{quest}.json")))?;
        serde_json::from_slice(&text)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    /// Whether this machine shares `quest`.
    #[must_use]
    pub fn shares(&self, quest: &str) -> bool {
        self.folder.join(format!("{quest}.json")).exists()
    }

    /// Stops sharing `quest`, leaving its files as they are, and keeps
    /// the version its README stood at, so a README said of it later is
    /// known for one changed since, as one reopened is, or for one that
    /// never heard it end.
    pub fn stop(&self, quest: &str) -> io::Result<()> {
        let held = self.hold(quest)?;
        if let Some(readme) = held.ledger.files.get(README)
            && readme.hash.is_some()
        {
            let at = self.ended_at(quest);
            fs::create_dir_all(self.folder.join(ENDED))?;
            let staged = at.with_extension("json.mycelium.tmp");
            let text = serde_json::to_vec(&readme.version).map_err(io::Error::other)?;
            fs::write(&staged, text)?;
            fs::rename(staged, at)?;
        }
        match fs::remove_file(&held.path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Holds `quest`'s ledger for changing, a new one if it has none,
    /// waiting for anyone else on this machine holding it.
    pub fn hold(&self, quest: &str) -> io::Result<Held> {
        if !is_quest(quest) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{quest:?} is not a quest's id"),
            ));
        }
        fs::create_dir_all(&self.folder)?;
        // Opened only to read, and made only when it isn't there: a lock
        // holds nothing, and opening it to write on each hold, as each
        // link's look does once a second for every shared quest, wrote the
        // folder 44 times a second with nothing changed.
        let at = self.folder.join(format!("{quest}.lock"));
        let lock = match fs::File::open(&at) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::File::create(&at)?,
            opened => opened?,
        };
        rustix::fs::flock(&lock, FlockOperation::LockExclusive)?;
        let path = self.folder.join(format!("{quest}.json"));
        let mut seeded = None;
        let ledger = match fs::read(&path) {
            Ok(text) => serde_json::from_slice(&text)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // Shared anew after it ended here, its README goes on from
                // the version it ended at, so a change made since follows
                // it wherever that end was heard. It is entered as held
                // nowhere yet, so a scan takes up what is on the disk and
                // a README missing here is never said as deleted.
                let mut ledger = Ledger::default();
                if let Some(version) = self.ended(quest)? {
                    ledger.files.insert(
                        README.to_owned(),
                        Entry {
                            hash: None,
                            version,
                            seen: None,
                        },
                    );
                    seeded = Some(self.ended_at(quest));
                }
                ledger
            }
            Err(error) => return Err(error),
        };
        Ok(Held {
            ledger,
            path,
            seeded,
            _lock: lock,
        })
    }

    /// The version `quest`'s README stood at when this machine last
    /// stopped sharing it, if it kept one: none for a quest it never
    /// stopped, or stopped before it kept them.
    pub fn ended(&self, quest: &str) -> io::Result<Option<Version>> {
        if !is_quest(quest) {
            return Ok(None);
        }
        match fs::read(self.ended_at(quest)) {
            Ok(text) => serde_json::from_slice(&text)
                .map(Some)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Where the version of `quest`'s README as it ended here is kept,
    /// in a folder of its own, which [`Self::quests`] passes over.
    fn ended_at(&self, quest: &str) -> PathBuf {
        self.folder.join(ENDED).join(format!("{quest}.json"))
    }
}

/// The folder beside the ledgers where each stopped quest's README
/// version is kept.
const ENDED: &str = "ended";

/// Whether `id` is a quest's: `QEST-`, a date and six letters or digits.
#[must_use]
pub fn is_quest(id: &str) -> bool {
    let parts: Vec<&str> = id.split('-').collect();
    matches!(
        parts.as_slice(),
        ["QEST", date, short]
            if date.len() == 8
                && date.bytes().all(|byte| byte.is_ascii_digit())
                && short.len() == 6
                && short.bytes().all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quest(dir: &Path) -> Quest {
        Quest::new(dir.join("record"), dir.join("home"))
    }

    fn write(quest: &Quest, path: &str, text: &str) {
        let path = quest.record.join(path);
        fs::create_dir_all(path.parent().expect("a file has a folder")).expect("its folder made");
        fs::write(path, text).expect("the file written");
    }

    #[test]
    fn a_change_within_a_tick_of_a_scan_is_seen() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let quest = quest(dir.path());
        let mut ledger = Ledger::default();
        let tick = SystemTime::now();
        let written = |text: &str| {
            write(&quest, "README.md", text);
            // Both writes in one tick of the file's clock.
            fs::File::options()
                .write(true)
                .open(quest.record.join("README.md"))
                .and_then(|file| file.set_modified(tick))
                .expect("its time set");
        };
        written("one");
        ledger.scan(&quest, "skiff").expect("scanned");
        written("two");
        assert_eq!(
            ledger.scan(&quest, "skiff").expect("scanned").changed,
            ["record/README.md"],
            "the same size and time just after a hash are hashed again"
        );
    }

    #[test]
    fn a_size_and_time_stand_once_a_grain_has_passed() {
        let seen = Seen {
            len: 3,
            modified: 10 * GRAIN.as_nanos(),
        };
        assert_eq!(
            lasting(seen, seen.modified + GRAIN.as_nanos() / 2),
            None,
            "within a grain, it may have changed since"
        );
        assert_eq!(
            lasting(seen, seen.modified + 2 * GRAIN.as_nanos()),
            Some(seen),
            "long changed before it was read, it stands"
        );
    }

    #[test]
    fn a_scan_counts_each_change_once() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let quest = quest(dir.path());
        let mut ledger = Ledger::default();
        write(&quest, "README.md", "one");
        assert_eq!(
            ledger.scan(&quest, "skiff").expect("scanned").changed,
            ["record/README.md"],
            "a new file is a change"
        );
        assert!(
            ledger
                .scan(&quest, "skiff")
                .expect("scanned")
                .changed
                .is_empty(),
            "an unchanged file is none"
        );
        write(&quest, "README.md", "two");
        ledger.scan(&quest, "skiff").expect("scanned");
        fs::remove_file(quest.record.join("README.md")).expect("removed");
        ledger.scan(&quest, "skiff").expect("scanned");
        let entry = &ledger.files["record/README.md"];
        assert_eq!(entry.hash, None, "a removed file is entered as deleted");
        assert_eq!(
            entry.version,
            Version::default()
                .changed_on("skiff")
                .changed_on("skiff")
                .changed_on("skiff"),
            "made, changed and removed: three changes"
        );
    }

    #[test]
    fn a_ledger_is_kept_between_holds() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let ledgers = Ledgers::in_state(dir.path());
        let quest_id = "QEST-20261013-4UHC5Q";
        assert!(!ledgers.shares(quest_id), "nothing shared yet");
        let mut held = ledgers.hold(quest_id).expect("held");
        held.ledger.journey = Some("# Journey\n".to_owned());
        held.keep().expect("kept");
        drop(held);
        assert_eq!(
            ledgers.quests().expect("listed"),
            [quest_id],
            "the quest is shared"
        );
        assert_eq!(
            ledgers
                .hold(quest_id)
                .expect("held again")
                .ledger
                .journey
                .as_deref(),
            Some("# Journey\n"),
            "what was kept is read back"
        );
        assert!(
            ledgers.hold("../etc").is_err(),
            "only a quest's id names a ledger"
        );
    }

    // Round 5 of the ended-record fix (2026-11-20): a README said of a
    // quest after it stopped here is weighed against the one it ended
    // with, so the stop keeps that one's version.
    #[test]
    fn a_quest_stopped_here_begins_its_next_ledger_from_the_readme_it_ended_with() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let ledgers = Ledgers::in_state(dir.path());
        let quest_id = "QEST-20261013-4UHC5Q";
        let ended = Version::default().changed_on("skiff").changed_on("kestrel");
        let mut held = ledgers.hold(quest_id).expect("held");
        held.ledger.files.insert(
            README.to_owned(),
            Entry {
                hash: Some("concluded".to_owned()),
                version: ended.clone(),
                seen: None,
            },
        );
        held.keep().expect("kept");
        drop(held);
        assert_eq!(
            ledgers.ended(quest_id).expect("read"),
            None,
            "none kept while shared"
        );
        ledgers.stop(quest_id).expect("stopped");
        assert!(!ledgers.shares(quest_id), "it is shared no longer");
        assert_eq!(
            ledgers.ended(quest_id).expect("read"),
            Some(ended.clone()),
            "its README's version is kept"
        );
        assert!(
            ledgers.quests().expect("listed").is_empty(),
            "and what is kept is no ledger"
        );
        let held = ledgers.hold(quest_id).expect("held anew");
        assert_eq!(
            held.ledger.files.get(README),
            Some(&Entry {
                hash: None,
                version: ended,
                seen: None,
            }),
            "shared anew, its README goes on from that version, held nowhere yet"
        );
        held.keep().expect("kept");
        drop(held);
        assert_eq!(
            ledgers.ended(quest_id).expect("read"),
            None,
            "and once its ledger is kept, the version is forgotten"
        );
    }
}
