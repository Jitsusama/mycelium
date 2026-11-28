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
//!
//! A file deleted is kept as deleted only until every other machine sharing
//! the quest is known to hold it deleted too, then forgotten, so a scratch
//! whose files come and go doesn't leave a ledger of tombstones.
//!
//! Long because its tests, over half of it, walk how a ledger is kept, read
//! back and shared between holds, and how it forgets what all deleted.

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
    /// holds too, and a file this machine holds deleted only while that
    /// machine holds it deleted too, so a file deleted here drops out of
    /// what a machine holding it says. A machine named here has said it
    /// shares the quest, or asked for a file of it, even holding none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub theirs: BTreeMap<String, BTreeMap<String, State>>,
    /// Every version of the files deleted and forgotten, joined: a file
    /// seen here anew starts from it, so one written again after its
    /// deletion was forgotten is newer than that deletion, wherever it is
    /// still kept, and a deletion never outlasts a write made after it.
    #[serde(default, skip_serializing_if = "Version::is_unchanged")]
    pub forgot: Version,
    /// What each worker given the quest to read last said it holds of
    /// its files, by machine and key, only those it holds as this machine
    /// does: a worker that spoke and holds none yet is kept, holding
    /// nothing, so a share waiting for it waits for its files rather than
    /// reading it as a machine that never took the quest up.
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
        let forgot = &self.forgot;
        let entry = self.files.entry(key.to_owned()).or_insert_with(|| Entry {
            version: forgot.clone(),
            ..Entry::default()
        });
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
    /// that changed what the ledger holds of it. A file it holds deleted,
    /// as this machine and every other sharing the quest do, is forgotten
    /// ([`Self::deleted_everywhere`]).
    pub fn heard(&mut self, machine: &str, files: BTreeMap<String, State>) -> bool {
        let before = self.theirs.get(machine).cloned();
        let deleted: Vec<String> = files
            .iter()
            .filter(|(_, state)| state.hash.is_none())
            .map(|(key, _)| key.clone())
            .collect();
        self.theirs
            .entry(machine.to_owned())
            .or_default()
            .extend(files);
        self.bound_theirs();
        let forgot = self.forget_deleted(&deleted);
        forgot || self.theirs.get(machine) != before.as_ref()
    }

    /// Names `machine` as sharing the quest, holding nothing yet if it
    /// wasn't named, as one asking for a file of it does, so a deletion
    /// waits for it even where it drops before saying it took the file.
    /// Answers whether it wasn't named before.
    pub fn asked_by(&mut self, machine: &str) -> bool {
        if self.theirs.contains_key(machine) {
            return false;
        }
        self.theirs.insert(machine.to_owned(), BTreeMap::new());
        true
    }

    /// Enters that the other end holds file `key` deleted, at `theirs`,
    /// where this machine holds it deleted too or never held it, as
    /// settling leaves both. Answers the state to tell it back where the
    /// two differ, so each end learns the other holds it deleted: this
    /// machine's deletion joined with theirs, which it then holds, or
    /// theirs where it never held the file, which it keeps no entry of.
    pub fn deleted_there(&mut self, key: &str, theirs: &State) -> Option<State> {
        let Some(entry) = self.files.get_mut(key) else {
            return Some(State {
                hash: None,
                version: theirs.version.clone(),
            });
        };
        if entry.hash.is_some() || entry.version == theirs.version {
            return None;
        }
        entry.version = entry.version.joined(&theirs.version);
        Some(entry.state())
    }

    /// Whether this machine holds file `key` deleted and every other
    /// machine sharing the quest is known to hold it deleted at the same
    /// state or newer, so nothing is left to tell of it. A machine that
    /// held the file and hasn't said since that it holds it deleted, as
    /// one away when it was deleted, keeps it.
    #[must_use]
    pub fn deleted_everywhere(&self, key: &str) -> bool {
        let Some(ours) = self.files.get(key).filter(|entry| entry.hash.is_none()) else {
            return false;
        };
        self.theirs.values().all(|held| {
            held.get(key)
                .is_some_and(|theirs| theirs.hash.is_none() && theirs.version.holds(&ours.version))
        })
    }

    /// Forgets each of `keys` deleted everywhere, as
    /// [`Self::deleted_everywhere`] says, answering whether it forgot any.
    fn forget_deleted(&mut self, keys: &[String]) -> bool {
        let mut forgot = false;
        for key in keys {
            if self.deleted_everywhere(key) {
                self.forget(key);
                forgot = true;
            }
        }
        forgot
    }

    /// Forgets file `key`, keeping its versions here and theirs in
    /// [`Self::forgot`].
    fn forget(&mut self, key: &str) {
        if let Some(entry) = self.files.remove(key) {
            self.forgot = self.forgot.joined(&entry.version);
        }
        for held in self.theirs.values_mut() {
            if let Some(theirs) = held.remove(key) {
                self.forgot = self.forgot.joined(&theirs.version);
            }
        }
    }

    /// Lets go of `machines`, which the realm has let go of for good, as
    /// sharing the quest or reading it, then forgets each file deleted
    /// that every machine left holds deleted. Answers whether any was
    /// named.
    pub fn let_go(&mut self, machines: &BTreeSet<String>) -> bool {
        let named = self.theirs.len() + self.read.len();
        self.theirs.retain(|machine, _| !machines.contains(machine));
        self.read.retain(|machine, _| !machines.contains(machine));
        if self.theirs.len() + self.read.len() == named {
            return false;
        }
        let deleted: Vec<String> = self
            .files
            .iter()
            .filter(|(_, entry)| entry.hash.is_none())
            .map(|(key, _)| key.clone())
            .collect();
        self.forget_deleted(&deleted);
        true
    }

    /// Enters what `worker`, given the quest to read, said it holds of
    /// `files`, answering whether that changed what the ledger holds of
    /// it. Only a file it holds as this machine holds it, by its content's
    /// hash, is kept, and kept as this machine's own state: the worker
    /// names nothing this machine doesn't hold, and chooses nothing of
    /// what is written. Any other file it names is dropped, so what is
    /// kept of a worker is bounded by what this machine holds, while the
    /// worker itself is kept holding what is left, even nothing.
    pub fn read_by(&mut self, worker: &str, files: BTreeMap<String, State>) -> bool {
        let before = self.read.get(worker).cloned();
        let read = self.read.entry(worker.to_owned()).or_default();
        for (key, state) in files {
            match self.files.get(&key) {
                Some(ours) if ours.hash.is_some() && ours.hash == state.hash => {
                    read.insert(key, ours.state());
                }
                _ => {
                    read.remove(&key);
                }
            }
        }
        self.read.get(worker) != before.as_ref()
    }

    /// Keeps what the other machines hold, and the workers given the quest
    /// read, to the files this one holds, answering whether any dropped
    /// out. Of a file this one holds deleted, another machine's deletion
    /// is kept, so it is known when every machine holds it deleted.
    pub fn bound_theirs(&mut self) -> bool {
        let mut dropped = false;
        for held in self.theirs.values_mut() {
            let before = held.len();
            held.retain(|key, state| {
                self.files
                    .get(key)
                    .is_some_and(|entry| entry.hash.is_some() == state.hash.is_some())
            });
            dropped |= held.len() != before;
        }
        for held in self.read.values_mut() {
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
/// opened, so what it stands for is what is read. It is the copy kept as
/// read, shared rather than copied: on 26 November copying and dropping
/// whole ledgers, of up to 7.3 MB, took a third of a node's time, as each
/// link's look took every shared quest's once a second.
fn read_ledger(path: &Path) -> io::Result<Option<Arc<Ledger>>> {
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
        return Ok(Some(ledger));
    }
    let mut text = Vec::new();
    file.read_to_end(&mut text)?;
    let ledger: Ledger = serde_json::from_slice(&text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    #[cfg(test)]
    PARSED.with(|parsed| parsed.set(parsed.get() + 1));
    let ledger = Arc::new(ledger);
    read_before().insert(path.to_owned(), (stamp, Arc::clone(&ledger)));
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
        read_ledger(&path)?
            .map(|ledger| Ledger::clone(&ledger))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{} isn't there", path.display()),
                )
            })
    }

    /// What this machine knows of `quest` as last kept, an empty ledger
    /// where it has none, read as [`Self::read`] reads it and shared with
    /// the copy kept as read rather than copied, for a reader that
    /// changes nothing. What it holds may be changed by a holder the
    /// moment after, so a reader that would change the ledger holds it
    /// and reads it again.
    ///
    /// # Errors
    ///
    /// When `quest` isn't a quest's id, or its ledger can't be read.
    pub fn look(&self, quest: &str) -> io::Result<Arc<Ledger>> {
        if !is_quest(quest) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{quest:?} is not a quest's id"),
            ));
        }
        let path = self.folder.join(format!("{quest}.json"));
        Ok(read_ledger(&path)?.unwrap_or_default())
    }

    /// Lets go of `machines`, which the realm has let go of for good, in
    /// each ledger naming one, as [`Ledger::let_go`] does, answering the
    /// quests whose ledgers named one. A ledger naming none is only
    /// looked at, never held, so a round that finds nothing writes
    /// nothing.
    pub fn let_go(&self, machines: &BTreeSet<String>) -> io::Result<Vec<String>> {
        let names = |held: &BTreeMap<String, BTreeMap<String, State>>| {
            held.keys().any(|machine| machines.contains(machine))
        };
        let mut let_go = Vec::new();
        for quest in self.quests()? {
            let looked = self.look(&quest)?;
            if !names(&looked.theirs) && !names(&looked.read) {
                continue;
            }
            drop(looked);
            // Held anew, so nothing kept since the look is lost; one
            // stopped meanwhile holds an empty ledger, naming none, and is
            // never kept again.
            let mut held = self.hold(&quest)?;
            if held.ledger.let_go(machines) {
                held.keep()?;
                let_go.push(quest);
            }
        }
        Ok(let_go)
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
        let ledger = read_ledger(&path)?.map_or_else(Ledger::default, Arc::unwrap_or_clone);
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
    use super::super::version::Standing;
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

    #[test]
    fn a_look_shares_the_ledger_kept_and_sees_what_is_kept_next() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let ledgers = Ledgers::in_state(dir.path());
        let quest_id = "QEST-20261013-4UHC5Q";
        assert_eq!(
            *ledgers.look(quest_id).expect("looked"),
            Ledger::default(),
            "a quest unshared looks empty"
        );
        let mut held = ledgers.hold(quest_id).expect("held");
        held.ledger.journey = Some("# Journey\n".to_owned());
        held.keep().expect("kept");
        drop(held);
        let first = ledgers.look(quest_id).expect("looked");
        assert!(
            Arc::ptr_eq(&first, &ledgers.look(quest_id).expect("looked again")),
            "two looks share one copy"
        );
        let mut held = ledgers.hold(quest_id).expect("held");
        held.ledger.journey = Some("# Journey, on\n".to_owned());
        held.keep().expect("kept");
        drop(held);
        assert_eq!(
            ledgers.look(quest_id).expect("looked").journey.as_deref(),
            Some("# Journey, on\n"),
            "and a look after a keep sees it"
        );
        assert_eq!(
            first.journey.as_deref(),
            Some("# Journey\n"),
            "while an earlier look's stands"
        );
        assert!(
            ledgers.look("../etc").is_err(),
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

    /// A deletion as this machine holds it, and as another holds it.
    fn deleted(version: &Version) -> State {
        State {
            hash: None,
            version: version.clone(),
        }
    }

    /// Kestrel holds the README deleted, told to skiff and tern, which
    /// both held it.
    fn deleted_at_kestrel(dir: &Path) -> (Ledger, Version) {
        let quest = quest(dir);
        write(&quest, "README.md", "# Ours\n");
        let mut ledger = Ledger::default();
        ledger.scan(&quest, "kestrel").expect("scanned");
        let held = ledger.states();
        ledger.heard("skiff", held.clone());
        ledger.heard("tern", held);
        fs::remove_file(quest.record.join("README.md")).expect("removed");
        ledger.scan(&quest, "kestrel").expect("scanned");
        ledger.bound_theirs();
        let version = ledger.files["record/README.md"].version.clone();
        (ledger, version)
    }

    #[test]
    fn a_deletion_is_forgotten_only_once_every_machine_holds_it_deleted() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut ledger, version) = deleted_at_kestrel(dir.path());
        let key = "record/README.md";
        let said = |state: State| BTreeMap::from([(key.to_owned(), state)]);
        assert!(
            ledger.heard("skiff", said(deleted(&version))),
            "skiff holding it deleted is heard"
        );
        assert!(
            ledger.files.contains_key(key),
            "kept while tern, which held it, hasn't said it holds it deleted"
        );
        ledger.heard(
            "tern",
            said(deleted(&Version::default().changed_on("kestrel"))),
        );
        assert!(
            ledger.files.contains_key(key),
            "and kept while tern holds it deleted at an older state"
        );
        assert!(
            ledger.heard("tern", said(deleted(&version.changed_on("tern")))),
            "tern holding it deleted at a newer state is heard"
        );
        assert!(
            !ledger.files.contains_key(key),
            "and once every machine holds it deleted, it is forgotten"
        );
        assert!(
            ledger.theirs.values().all(|held| !held.contains_key(key)),
            "by every machine's too"
        );
    }

    #[test]
    fn a_file_written_again_after_its_deletion_was_forgotten_is_newer() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut ledger, version) = deleted_at_kestrel(dir.path());
        let key = "record/README.md";
        let gone = BTreeMap::from([(key.to_owned(), deleted(&version))]);
        ledger.heard("skiff", gone.clone());
        ledger.heard("tern", gone);
        assert!(!ledger.files.contains_key(key), "forgotten");
        let quest = quest(dir.path());
        write(&quest, "README.md", "# Again\n");
        ledger.scan(&quest, "kestrel").expect("scanned");
        assert_eq!(
            ledger.files[key].version.against(&version),
            Standing::Newer,
            "so the write outlasts the deletion wherever it is still kept"
        );
    }

    #[test]
    fn a_deletion_held_at_another_state_is_said_back_until_both_hold_one() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut ledger, version) = deleted_at_kestrel(dir.path());
        let key = "record/README.md";
        let theirs = version.changed_on("skiff");
        let joined = ledger.deleted_there(key, &deleted(&theirs));
        assert_eq!(
            joined,
            Some(deleted(&theirs)),
            "a newer deletion there is joined here, and said back"
        );
        assert_eq!(
            ledger.deleted_there(key, &deleted(&theirs)),
            None,
            "and once both hold one, nothing more is said"
        );
        assert_eq!(
            ledger.deleted_there("record/never.md", &deleted(&theirs)),
            Some(deleted(&theirs)),
            "a file never held here is said held deleted as there"
        );
        assert!(
            !ledger.files.contains_key("record/never.md"),
            "with no entry kept of it"
        );
    }

    #[test]
    fn a_machine_let_go_for_good_is_waited_on_no_longer() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut ledger, version) = deleted_at_kestrel(dir.path());
        let key = "record/README.md";
        ledger.heard(
            "skiff",
            BTreeMap::from([(key.to_owned(), deleted(&version))]),
        );
        ledger.read_by("wren", BTreeMap::new());
        assert!(
            !ledger.let_go(&BTreeSet::from(["osprey".to_owned()])),
            "a machine never named lets nothing go"
        );
        assert!(
            ledger.let_go(&BTreeSet::from(["tern".to_owned(), "wren".to_owned()])),
            "a machine named is let go"
        );
        assert_eq!(
            ledger.theirs.keys().collect::<Vec<_>>(),
            ["skiff"],
            "skiff stays"
        );
        assert!(ledger.read.is_empty(), "and a worker let go reads no more");
        assert!(
            !ledger.files.contains_key(key),
            "what only tern lacked is forgotten"
        );
    }

    /// A worker given the quest names only files this machine holds, as
    /// it holds them, and what is kept of each is this machine's own.
    #[test]
    fn a_worker_is_kept_to_the_files_held_here_as_held_here() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let quest = quest(dir.path());
        write(&quest, "README.md", "# Ours\n");
        write(&quest, "notes.md", "notes\n");
        let mut ledger = Ledger::default();
        ledger.scan(&quest, "kestrel").expect("scanned");
        let ours = ledger.states();
        let theirs = |hash: &str| State {
            hash: Some(hash.to_owned()),
            version: Version::default().changed_on("tern").changed_on("tern"),
        };
        let said = BTreeMap::from([
            (
                "record/README.md".to_owned(),
                ours["record/README.md"].clone(),
            ),
            ("record/notes.md".to_owned(), theirs("another")),
            ("record/elsewhere.md".to_owned(), theirs("any")),
        ]);
        assert!(
            ledger.read_by("tern", said.clone()),
            "what it holds is heard"
        );
        assert_eq!(
            ledger.read["tern"],
            BTreeMap::from([(
                "record/README.md".to_owned(),
                ours["record/README.md"].clone()
            )]),
            "only the file it holds as kestrel does, as kestrel holds it"
        );
        assert!(
            !ledger.read_by("tern", said),
            "saying it again changes nothing"
        );

        let lacking = BTreeMap::from([("record/notes.md".to_owned(), theirs("another"))]);
        assert!(
            ledger.read_by("wren", lacking),
            "a worker holding none of kestrel's files as kestrel does is heard"
        );
        assert_eq!(
            ledger.read.get("wren"),
            Some(&BTreeMap::new()),
            "and kept holding nothing, so a wait for it waits for its files"
        );
        assert!(
            ledger.read_by("osprey", BTreeMap::new()),
            "a worker saying it holds nothing yet is heard"
        );
        assert!(
            !ledger.read_by("osprey", BTreeMap::new()),
            "and saying it again changes nothing"
        );

        fs::remove_file(quest.record.join("README.md")).expect("removed");
        ledger.scan(&quest, "kestrel").expect("scanned");
        assert!(
            ledger.bound_theirs(),
            "what kestrel no longer holds drops out"
        );
        assert_eq!(
            ledger.read.get("tern"),
            Some(&BTreeMap::new()),
            "a worker keeps none of what kestrel no longer holds"
        );
    }
}
