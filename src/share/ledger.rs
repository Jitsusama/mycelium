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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{self, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
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
    /// What each worker given the quest to read last said it holds of
    /// its files, by machine and key, only those this machine holds too.
    /// Kept apart from `theirs`, which takes a machine's writes and lets
    /// its runs into this one's trees: a worker's word only tells a share
    /// waiting for it that the quest arrived ([`super::reached::reach`]).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub read: BTreeMap<String, BTreeMap<String, State>>,
    /// Whether the quest is shared only as one above another shared quest,
    /// so it stops once no shared quest is under it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub above: bool,
    /// Whether this machine's own record of the quest, there before it was
    /// taken up from another machine, still waits to move aside until that
    /// machine's copy first comes. Until then it is neither read nor said,
    /// so it is never taken for a change made here, and a link that drops
    /// first leaves it where it was.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub aside: bool,
    /// The machine whose copy the record waits for, while it does, so a
    /// share refused meanwhile can name it. Kept apart from `theirs`,
    /// which lets a machine's runs into this one's trees, since nothing
    /// of its copy has come.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waits_for: Option<String>,
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

    /// Enters what `worker`, given the quest to read, said it holds of
    /// `files`, kept to the files this machine holds, answering whether
    /// that changed what the ledger holds of it. A file it says is gone,
    /// or one this machine doesn't hold, is dropped from what it holds.
    pub fn read_by(&mut self, worker: &str, files: BTreeMap<String, State>) -> bool {
        let before = self.read.get(worker).cloned();
        let read = self.read.entry(worker.to_owned()).or_default();
        for (key, state) in files {
            let ours = self
                .files
                .get(&key)
                .is_some_and(|entry| entry.hash.is_some());
            if ours && state.hash.is_some() {
                read.insert(key, state);
            } else {
                read.remove(&key);
            }
        }
        self.read.get(worker) != before.as_ref()
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
    _lock: fs::File,
}

impl Held {
    /// Writes the ledger back, by rename so no reader sees half of it, and
    /// keeps it as read, so the next hold need not parse what it wrote.
    pub fn keep(&self) -> io::Result<()> {
        #[cfg(test)]
        KEPT.with(|kept| kept.set(kept.get() + 1));
        let staged = self.path.with_extension("json.mycelium.tmp");
        let text = serde_json::to_vec_pretty(&self.ledger).map_err(io::Error::other)?;
        fs::write(&staged, text)?;
        fs::rename(staged, &self.path)?;
        // Read after the rename, which moves a file's change time, and
        // while the ledger is held, so no other writer comes between. The
        // ledger is written by then, so a stamp that can't be read only
        // leaves the next hold to parse it.
        match fs::metadata(&self.path) {
            Ok(metadata) => read_before().insert(
                self.path.clone(),
                (Stamp::of(&metadata), Arc::new(self.ledger.clone())),
            ),
            Err(_) => read_before().remove(&self.path),
        };
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    /// How many ledgers this thread has kept, so a test can count writes.
    static KEPT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many ledgers this thread has kept.
#[cfg(test)]
#[must_use]
pub fn kept_here() -> usize {
    KEPT.with(std::cell::Cell::get)
}

/// What tells one written ledger from another without reading it: the
/// file's inode, size and change time. Every ledger is written whole to a
/// new file and renamed into place, by this process or any other, so a
/// write gives the path an inode it didn't have just before. Only a
/// filesystem that hands a freed inode straight back, such as ext4, could
/// show two writes as one: another process writing the ledger twice
/// within the tick of the clock it was last read or kept in, the second
/// given back the inode the first freed and of the length read. This
/// process keeps what it writes itself as read, so its own writes never
/// count among the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    len: u64,
    changed: (i64, i64),
}

impl Stamp {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            len: metadata.len(),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

/// The ledgers this process last read or wrote, by path, each with the
/// stamp of the file it came from. Each link looks at every shared quest
/// once a second, and parsing all of them each time read 28.7 MB of JSON a
/// second per link on a machine sharing 71 quests, with nothing changed.
/// Each is shared, so the lock is held only to find it, and copied
/// outside it, where looks on several links don't wait on each other.
static READ_BEFORE: LazyLock<Mutex<ReadBefore>> = LazyLock::new(Mutex::default);

/// Each ledger read before, by its path, with the stamp of its file.
type ReadBefore = HashMap<PathBuf, (Stamp, Arc<Ledger>)>;

/// The ledgers read before, whatever a holder that panicked left: each
/// entry is written whole, so none is ever half changed.
fn read_before() -> std::sync::MutexGuard<'static, ReadBefore> {
    READ_BEFORE.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
thread_local! {
    /// How many ledgers this thread has parsed, so a test can tell one
    /// read anew from one served as read before.
    static PARSED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The ledger at `path`, parsed only when its file isn't the one read
/// before, or `None` where there is none. The stamp is read from the file
/// opened, so what it stands for is what is read.
fn read_ledger(path: &Path) -> io::Result<Option<Ledger>> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            read_before().remove(path);
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let stamp = Stamp::of(&file.metadata()?);
    let before = read_before()
        .get(path)
        .filter(|(before, _)| *before == stamp)
        .map(|(_, ledger)| Arc::clone(ledger));
    if let Some(ledger) = before {
        return Ok(Some(Ledger::clone(&ledger)));
    }
    let mut text = Vec::new();
    file.read_to_end(&mut text)?;
    let ledger: Ledger = serde_json::from_slice(&text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    #[cfg(test)]
    PARSED.with(|parsed| parsed.set(parsed.get() + 1));
    read_before().insert(path.to_owned(), (stamp, Arc::new(ledger.clone())));
    Ok(Some(ledger))
}

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
        let path = self.folder.join(format!("{quest}.json"));
        read_ledger(&path)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} isn't there", path.display()),
            )
        })
    }

    /// Whether this machine shares `quest`.
    #[must_use]
    pub fn shares(&self, quest: &str) -> bool {
        self.folder.join(format!("{quest}.json")).exists()
    }

    /// Stops sharing `quest`, leaving its files as they are.
    pub fn stop(&self, quest: &str) -> io::Result<()> {
        let held = self.hold(quest)?;
        // Kept from here on only while it is shared, so a node up for
        // weeks holds no ledger of every quest it ever shared.
        read_before().remove(&held.path);
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
        let ledger = read_ledger(&path)?.unwrap_or_default();
        Ok(Held {
            ledger,
            path,
            _lock: lock,
        })
    }
}

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

    /// A ledger read before is read again once another process writes it,
    /// by rename as every writer does or in place, and once it is gone.
    #[test]
    fn a_ledger_written_elsewhere_is_read_anew() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let ledgers = Ledgers::in_state(dir.path());
        let quest_id = "QEST-20261013-4UHC5Q";
        let mut held = ledgers.hold(quest_id).expect("held");
        held.ledger.journey = Some("here".to_owned());
        held.keep().expect("kept");
        drop(held);
        let parsed = || PARSED.with(std::cell::Cell::get);
        let before = parsed();
        let journey = |ledgers: &Ledgers| ledgers.read(quest_id).ok().and_then(|l| l.journey);
        assert_eq!(journey(&ledgers).as_deref(), Some("here"), "read as kept");
        drop(ledgers.hold(quest_id).expect("held again"));
        assert_eq!(
            parsed(),
            before,
            "what this process kept is read and held again without parsing it"
        );

        // Another process writes it whole and renames it into place.
        let path = dir.path().join("shared").join(format!("{quest_id}.json"));
        let staged = dir.path().join("staged.json");
        fs::write(&staged, r#"{"files":{},"journey":"elsewhere"}"#).expect("staged");
        fs::rename(&staged, &path).expect("renamed into place");
        assert_eq!(
            journey(&ledgers).as_deref(),
            Some("elsewhere"),
            "a rename is read"
        );
        assert_eq!(
            ledgers
                .hold(quest_id)
                .expect("held")
                .ledger
                .journey
                .as_deref(),
            Some("elsewhere"),
            "and held"
        );
        assert_eq!(parsed(), before + 1, "parsed once, then read as before");

        // Then in place, at another length, keeping its inode.
        fs::write(&path, r#"{"files":{},"journey":"in place"}"#).expect("written");
        assert_eq!(
            journey(&ledgers).as_deref(),
            Some("in place"),
            "an edit in place is read"
        );

        fs::remove_file(&path).expect("removed");
        assert!(ledgers.read(quest_id).is_err(), "a ledger gone is none");
        assert_eq!(
            ledgers.hold(quest_id).expect("held").ledger,
            Ledger::default(),
            "and is held anew"
        );
    }

    /// A quest no longer shared is no longer kept as read, so it costs the
    /// node nothing, and is shared again as new.
    #[test]
    fn a_ledger_stopped_is_let_go() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let ledgers = Ledgers::in_state(dir.path());
        let quest_id = "QEST-20261013-4UHC5Q";
        let mut held = ledgers.hold(quest_id).expect("held");
        held.ledger.journey = Some("here".to_owned());
        held.keep().expect("kept");
        drop(held);
        let path = dir.path().join("shared").join(format!("{quest_id}.json"));
        assert!(read_before().contains_key(&path), "kept as read");

        ledgers.stop(quest_id).expect("stopped");
        assert!(!read_before().contains_key(&path), "let go once stopped");
        assert_eq!(
            ledgers.hold(quest_id).expect("held").ledger,
            Ledger::default(),
            "and shared again as new"
        );
    }
}
