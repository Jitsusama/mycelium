//! The quests' records and the scratch folders a link shares, watched for
//! changes, so a look walks only what changed rather than every file of
//! every quest.
//!
//! A record or folder that can't be watched, one newly named, and every one
//! after the watch fails or overflows, is walked anyway, so a watch that
//! misses something costs a walk, never a change.
//!
//! A quest's record and folders are watched in one batch as it's looked
//! at, and only those there to watch, so macOS's watcher, which starts its
//! stream again for each change, starts it once a quest newly looked at
//! rather than once a path, and not at all at a look that starts nothing
//! (see [`crate::watching`]).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
#[cfg(test)]
use std::time::Duration;

use notify::event::ModifyKind;
use notify::{EventKind, RecursiveMode, Watcher};

use super::tree::{Place, Shared, crosses};

/// A quest's scratch folder, by the quest's id and the folder's name.
pub type Folder = (String, String);

/// The quests' records and shared scratch folders being watched.
pub struct Watch {
    /// The quests' scratch folders, one a quest, by its id.
    scratch: Root,
    /// The quests' records, one a quest, by its id.
    records: Root,
    /// The watcher, unless one couldn't be made, when every record and
    /// folder is walked at every look.
    watcher: Option<Box<dyn Watcher + Send>>,
    /// The folders watched.
    watched: BTreeSet<Folder>,
    /// The records watched, by their quests' ids.
    records_watched: BTreeSet<String>,
    /// The folders the watcher may hold though they aren't watched: one
    /// heard gone or made anew, or one a batch failed to start. Each is
    /// given back when its quest is forgotten, unless watched again first.
    stale: BTreeSet<Folder>,
    /// The records it may hold though they aren't watched, likewise.
    records_stale: BTreeSet<String>,
    /// What the watcher heard, not yet taken.
    heard: mpsc::Receiver<Heard>,
    /// What was taken from `heard` early, by a wait, to be taken again.
    early: Vec<Heard>,
    /// The folders heard to change, not yet walked.
    changed: BTreeSet<Folder>,
    /// The records heard to change, not yet walked, by their quests' ids.
    records_changed: BTreeSet<String>,
}

/// A folder holding one folder a quest, as a watch names changes in it.
struct Root {
    /// The folder.
    path: PathBuf,
    /// `path` with every link in it resolved, once it is there. macOS's
    /// watcher names a change by this path, so a folder reached through a
    /// link, as `/var` is `/private/var` on a Mac, is matched by it.
    resolved: Option<PathBuf>,
}

impl Root {
    const fn new(path: PathBuf) -> Self {
        Self {
            path,
            resolved: None,
        }
    }

    /// Resolves its links, once it is there.
    fn resolve(&mut self) {
        if self.resolved.is_none() {
            self.resolved = fs::canonicalize(&self.path).ok();
        }
    }

    /// The names of `path` below it, by the path as given or as resolved.
    fn names_of<'a>(&self, path: &'a Path) -> Option<Vec<&'a str>> {
        let within = path
            .strip_prefix(&self.path)
            .ok()
            .or_else(|| path.strip_prefix(self.resolved.as_deref()?).ok())?;
        within.iter().map(|name| name.to_str()).collect()
    }
}

/// What the watcher heard.
enum Heard {
    /// Something changed at this path.
    At(PathBuf),
    /// The watch failed or overflowed, so anything may have changed.
    Anything,
}

impl Watch {
    /// A watch of the quests' scratch folders in `scratch` and their
    /// records in `records`, watching none until a look names them.
    #[must_use]
    pub fn new(scratch: PathBuf, records: PathBuf) -> Self {
        Self::watched_by(scratch, records, |hears| {
            let watcher: Box<dyn Watcher + Send> = Box::new(crate::watching::watcher(hears)?);
            Ok(watcher)
        })
    }

    /// A watch of `scratch` and `records` by the watcher `make` makes,
    /// telling what it hears to `hears`.
    fn watched_by(
        scratch: PathBuf,
        records: PathBuf,
        make: impl FnOnce(Hears) -> notify::Result<Box<dyn Watcher + Send>>,
    ) -> Self {
        let (sender, heard) = mpsc::channel();
        let watcher = make(Hears(sender))
            .inspect_err(|error| {
                eprintln!("shared scratch folders are walked at every look, unwatched: {error}");
            })
            .ok();
        Self {
            scratch: Root::new(scratch),
            records: Root::new(records),
            watcher,
            watched: BTreeSet::new(),
            records_watched: BTreeSet::new(),
            stale: BTreeSet::new(),
            records_stale: BTreeSet::new(),
            heard,
            early: Vec::new(),
            changed: BTreeSet::new(),
            records_changed: BTreeSet::new(),
        }
    }

    /// What of quest `id` to walk at this look: the folders among
    /// `folders`, those it shares, that changed since the last or aren't
    /// watched yet, and whether its record did or isn't. Those not watched
    /// yet are watched from now on, in one batch, where they can be.
    pub fn to_look_at(&mut self, id: &str, folders: &BTreeSet<String>) -> (BTreeSet<String>, bool) {
        self.look(id, folders, true)
    }

    /// Whether quest `id`'s record is to be walked, as
    /// [`Self::to_look_at`] says, for a test of the record alone.
    #[cfg(test)]
    pub fn record_to_walk(&mut self, id: &str) -> bool {
        self.look(id, &BTreeSet::new(), true).1
    }

    /// The folders of quest `id` to walk, as [`Self::to_look_at`] says,
    /// for a test of its folders alone.
    #[cfg(test)]
    pub fn to_walk(&mut self, id: &str, folders: &BTreeSet<String>) -> BTreeSet<String> {
        self.look(id, folders, false).0
    }

    /// What of quest `id` to walk, its record too when `with_record`.
    fn look(
        &mut self,
        id: &str,
        folders: &BTreeSet<String>,
        with_record: bool,
    ) -> (BTreeSet<String>, bool) {
        self.take_heard();
        let mut walk = BTreeSet::new();
        let mut new = Vec::new();
        for folder in folders {
            let named = (id.to_owned(), folder.clone());
            if self.changed.remove(&named) {
                walk.insert(folder.clone());
            }
            if !self.watched.contains(&named) {
                new.push(named);
                walk.insert(folder.clone());
            }
        }
        let unwatched = with_record && !self.records_watched.contains(id);
        let record = with_record && (self.records_changed.remove(id) || unwatched);
        self.start(&new, unwatched.then_some(id));
        (walk, record)
    }

    /// Stops watching quest `id`'s record and folders, once it is shared
    /// no longer.
    pub fn forget(&mut self, id: &str) {
        let ours = |(quest, _): &Folder| quest == id;
        let mut gone: Vec<PathBuf> = self
            .watched
            .iter()
            .chain(&self.stale)
            .filter(|folder| ours(folder))
            .map(|folder| self.folder_path(folder))
            .collect();
        let held = self.records_watched.remove(id);
        if self.records_stale.remove(id) || held {
            gone.push(self.records.path.join(id));
        }
        if let Some(watcher) = self.watcher.as_mut() {
            // What's already gone is no longer watched anyway.
            crate::watching::watch_all(watcher.as_mut(), &gone, &[]);
        }
        self.watched.retain(|folder| !ours(folder));
        self.stale.retain(|folder| !ours(folder));
        self.changed.retain(|folder| !ours(folder));
        self.records_changed.remove(id);
    }

    /// Takes it that anything may have changed, as when the watch
    /// overflows, so the next look walks every record and folder watched,
    /// for a test asking what crosses rather than when a look walks.
    #[cfg(test)]
    pub fn hear_anything(&mut self) {
        self.early.push(Heard::Anything);
    }

    /// Waits up to `within` for the watcher to hear a change, for a test
    /// that must see one heard before it looks.
    #[cfg(test)]
    pub fn wait(&mut self, within: Duration) -> bool {
        match self.heard.recv_timeout(within) {
            Ok(heard) => {
                self.early.push(heard);
                true
            }
            Err(_) => false,
        }
    }

    /// Waits up to `within` for the watcher to hear of `path` itself,
    /// keeping what it hears before for the next look. What it hears comes
    /// in order, so once it has heard of `path`, everything it was to hear
    /// of earlier, such as macOS telling late of folders made just before
    /// the watch, has come too.
    #[cfg(test)]
    pub fn wait_for(&mut self, path: &Path, within: Duration) -> bool {
        let resolved = fs::canonicalize(path).ok();
        let deadline = std::time::Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let Ok(heard) = self.heard.recv_timeout(left) else {
                return false;
            };
            let found =
                matches!(&heard, Heard::At(at) if at == path || resolved.as_ref() == Some(at));
            self.early.push(heard);
            if found {
                return true;
            }
        }
    }

    /// Where `folder` is.
    fn folder_path(&self, (id, folder): &Folder) -> PathBuf {
        self.scratch.path.join(id).join(folder)
    }

    /// Starts watching each of `folders`, and quest `record`'s record if
    /// named, in one batch, where they can be.
    fn start(&mut self, folders: &[Folder], record: Option<&str>) {
        if !folders.is_empty() {
            self.scratch.resolve();
        }
        if record.is_some() {
            self.records.resolve();
        }
        // A folder that is a git clone crosses as its remote, and walking it
        // only finds its `.git`, where watching it whole could spend the
        // watches a machine has on a large clone's every folder. One not
        // there yet is walked until it is, then watched.
        let folders: Vec<&Folder> = folders
            .iter()
            .filter(|folder| {
                let at = self.folder_path(folder);
                at.exists() && !at.join(".git").exists()
            })
            .collect();
        let record = record.filter(|id| self.records.path.join(id).exists());
        // A path watched again replaces what the watcher held for it.
        let mut new: Vec<(PathBuf, RecursiveMode)> = folders
            .iter()
            .map(|folder| (self.folder_path(folder), RecursiveMode::Recursive))
            .collect();
        if let Some(id) = record {
            new.push((self.records.path.join(id), RecursiveMode::Recursive));
        }
        let Some(watcher) = self.watcher.as_mut() else {
            return;
        };
        let mut answers = crate::watching::watch_all(watcher.as_mut(), &[], &new).into_iter();
        for (folder, took) in folders.into_iter().zip(answers.by_ref()) {
            if took {
                self.stale.remove(folder);
                self.watched.insert(folder.clone());
            } else {
                self.stale.insert(folder.clone());
            }
        }
        if let (Some(id), Some(took)) = (record, answers.next()) {
            if took {
                self.records_stale.remove(id);
                self.records_watched.insert(id.to_owned());
            } else {
                self.records_stale.insert(id.to_owned());
            }
        }
    }

    /// Takes what the watcher heard into the folders changed. A folder
    /// gone, or made again, is no longer watched, and is watched again at
    /// its next walk.
    fn take_heard(&mut self) {
        let early = std::mem::take(&mut self.early);
        for heard in early
            .into_iter()
            .chain(std::iter::from_fn(|| self.heard.try_recv().ok()))
        {
            match heard {
                Heard::Anything => {
                    self.changed.extend(self.watched.iter().cloned());
                    self.records_changed
                        .extend(self.records_watched.iter().cloned());
                }
                Heard::At(path) => {
                    if let Some((folder, whole)) = self
                        .scratch
                        .names_of(&path)
                        .and_then(|names| folder_of(&names))
                    {
                        if whole && self.watched.remove(&folder) {
                            self.stale.insert(folder.clone());
                        }
                        self.changed.insert(folder);
                    } else if let Some((id, whole)) = self
                        .records
                        .names_of(&path)
                        .and_then(|names| record_of(&names))
                    {
                        if whole && self.records_watched.remove(&id) {
                            self.records_stale.insert(id.clone());
                        }
                        self.records_changed.insert(id);
                    }
                }
            }
        }
    }
}

/// What a watcher tells a [`Watch`].
struct Hears(mpsc::Sender<Heard>);

impl notify::EventHandler for Hears {
    fn handle_event(&mut self, event: notify::Result<notify::Event>) {
        let heard = match event {
            Err(_) => vec![Heard::Anything],
            Ok(event) if event.need_rescan() => vec![Heard::Anything],
            // A file opened or read holds nothing new; a look's own reads
            // would otherwise bring another look.
            Ok(event) if !alters(event.kind) => Vec::new(),
            Ok(event) => event.paths.into_iter().map(Heard::At).collect(),
        };
        for heard in heard {
            // Nobody hears once the link has gone; the watcher goes with
            // it.
            let _ = self.0.send(heard);
        }
    }
}

/// How long a test waits for the watcher to hear a change.
#[cfg(test)]
pub const HEARD_WITHIN: Duration = Duration::from_secs(5);

/// The scratch folder a change at the path `names` gives below the scratch
/// is in, if it is one whose files cross, and whether it was the folder
/// itself that changed.
fn folder_of(names: &[&str]) -> Option<(Folder, bool)> {
    let [id, folder, rest @ ..] = names else {
        return None;
    };
    let whole = rest.is_empty();
    // A change in what never crosses, such as a build in a shared folder,
    // needs no walk.
    if !whole
        && !crosses(&Shared {
            place: Place::Scratch,
            path: names[1..].join("/"),
        })
    {
        return None;
    }
    Some((((*id).to_owned(), (*folder).to_owned()), whole))
}

/// The quest whose record a change at the path `names` gives below the
/// records is in, and whether it was the record itself that changed. Any
/// change counts, as a record changes seldom and a folder made in it may
/// hold files written before the watch reaches it.
fn record_of(names: &[&str]) -> Option<(String, bool)> {
    let [id, rest @ ..] = names else {
        return None;
    };
    Some(((*id).to_owned(), rest.is_empty()))
}

/// Whether a watch's event may have changed what a file holds: made,
/// written, renamed or removed, rather than opened, read or given new
/// times.
const fn alters(kind: EventKind) -> bool {
    !matches!(
        kind,
        EventKind::Access(_) | EventKind::Modify(ModifyKind::Metadata(_))
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const QUEST: &str = "QEST-20261013-4UHC5Q";

    fn shared(folders: &[&str]) -> BTreeSet<String> {
        folders.iter().map(|folder| (*folder).to_owned()).collect()
    }

    fn named(folder: &str) -> String {
        folder.to_owned()
    }

    /// Takes what the watcher has yet to tell of before the test, such as
    /// macOS telling late of the folders made just before it watched:
    /// it makes the folder `mark`, one change of one event, waits to hear
    /// of it, and so of all before it, and takes every folder changed.
    fn settle(watch: &mut Watch, mark: &Path, folders: &BTreeSet<String>) {
        fs::create_dir(mark).expect("the mark is made");
        assert!(
            watch.wait_for(mark, HEARD_WITHIN),
            "the watcher hears the mark"
        );
        watch.to_walk(QUEST, folders);
    }

    #[test]
    fn a_folder_is_walked_when_named_and_then_only_once_it_changes() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        for folder in ["notes", "bench"] {
            fs::create_dir_all(dir.path().join(QUEST).join(folder)).expect("made");
        }
        let mut watch = Watch::new(dir.path().to_owned(), dir.path().join("records"));
        let both = shared(&["notes", "bench"]);
        assert_eq!(
            watch.to_walk(QUEST, &both),
            BTreeSet::from([named("bench"), named("notes")]),
            "each folder is walked once when first named"
        );
        settle(
            &mut watch,
            &dir.path().join(QUEST).join("notes/settled"),
            &both,
        );
        assert!(
            watch.to_walk(QUEST, &both).is_empty(),
            "and not again while nothing changes"
        );

        fs::write(dir.path().join(QUEST).join("notes/plan.md"), "plan\n").expect("written");
        assert!(watch.wait(HEARD_WITHIN), "the watcher hears the write");
        assert_eq!(
            watch.to_walk(QUEST, &both),
            BTreeSet::from([named("notes")]),
            "only the folder that changed is walked"
        );
    }

    #[test]
    fn a_record_is_walked_when_first_looked_at_and_then_only_once_it_changes() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let records = dir.path().join("records");
        fs::create_dir_all(records.join(QUEST)).expect("made");
        let mut watch = Watch::new(dir.path().join("scratch"), records.clone());
        assert!(
            watch.record_to_walk(QUEST),
            "a record is walked when first looked at"
        );
        // One change of one event, so nothing is heard after it.
        let mark = records.join(QUEST).join("settled");
        fs::create_dir(&mark).expect("the mark is made");
        assert!(
            watch.wait_for(&mark, HEARD_WITHIN),
            "the watcher hears the mark"
        );
        assert!(
            watch.record_to_walk(QUEST),
            "a record that changed is walked"
        );
        assert!(
            !watch.record_to_walk(QUEST),
            "and not again while nothing changes"
        );

        fs::write(records.join(QUEST).join("README.md"), "# Shared\n").expect("written");
        assert!(watch.wait(HEARD_WITHIN), "the watcher hears the write");
        assert!(
            watch.record_to_walk(QUEST),
            "a change anywhere in the record has it walked"
        );
    }

    #[test]
    fn a_change_is_matched_to_its_folder_through_a_linked_scratch() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let real = dir.path().join("real");
        fs::create_dir_all(real.join(QUEST).join("notes")).expect("made");
        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(&real, &linked).expect("linked");
        let mut watch = Watch::new(linked.clone(), dir.path().join("records"));
        let notes = shared(&["notes"]);
        watch.to_walk(QUEST, &notes);

        fs::write(linked.join(QUEST).join("notes/plan.md"), "plan\n").expect("written");
        assert!(watch.wait(HEARD_WITHIN), "the watcher hears the write");
        assert_eq!(
            watch.to_walk(QUEST, &notes),
            BTreeSet::from([named("notes")]),
            "a change named by the resolved path is still its folder's"
        );
    }

    #[test]
    fn a_change_to_what_never_crosses_needs_no_walk() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        fs::create_dir_all(dir.path().join(QUEST).join("notes/target")).expect("made");
        let mut watch = Watch::new(dir.path().to_owned(), dir.path().join("records"));
        let notes = shared(&["notes"]);
        watch.to_walk(QUEST, &notes);
        settle(
            &mut watch,
            &dir.path().join(QUEST).join("notes/target/settled"),
            &notes,
        );

        fs::write(dir.path().join(QUEST).join("notes/target/big"), "built").expect("written");
        assert!(watch.wait(HEARD_WITHIN), "the watcher hears the build");
        assert!(
            watch.to_walk(QUEST, &notes).is_empty(),
            "but a build's output never crosses, so it needs no walk"
        );
    }

    #[test]
    fn a_folder_not_there_yet_is_walked_until_it_can_be_watched() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let mut watch = Watch::new(dir.path().to_owned(), dir.path().join("records"));
        let notes = shared(&["notes"]);
        for look in 0..2 {
            assert_eq!(
                watch.to_walk(QUEST, &notes),
                BTreeSet::from([named("notes")]),
                "walked at look {look}, since there is nothing to watch"
            );
        }
        fs::create_dir_all(dir.path().join(QUEST).join("notes")).expect("made");
        assert_eq!(
            watch.to_walk(QUEST, &notes),
            BTreeSet::from([named("notes")]),
            "walked once more as it comes"
        );
        assert!(
            watch.to_walk(QUEST, &notes).is_empty(),
            "and then watched instead"
        );
    }

    #[test]
    fn a_quest_newly_looked_at_starts_the_stream_once_and_a_look_after_none() {
        use std::sync::atomic::Ordering;

        use crate::watching::counting::Counting;

        let dir = tempfile::tempdir().expect("a temporary folder");
        let (scratch, records) = (dir.path().join("scratch"), dir.path().join("records"));
        for folder in ["notes", "bench", "logs"] {
            fs::create_dir_all(scratch.join(QUEST).join(folder)).expect("made");
        }
        fs::create_dir_all(records.join(QUEST)).expect("made");
        let (counting, starts) = Counting::made();
        let mut watch = Watch::watched_by(scratch.clone(), records, |_| {
            let watcher: Box<dyn Watcher + Send> = Box::new(counting);
            Ok(watcher)
        });
        let all = shared(&["notes", "bench", "logs", "not-yet"]);
        let (walk, record) = watch.to_look_at(QUEST, &all);
        assert_eq!((walk.len(), record), (4, true), "{walk:?}");
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "three folders and a record start it once"
        );
        for look in 0..3 {
            assert_eq!(
                watch.to_look_at(QUEST, &all),
                (BTreeSet::from([named("not-yet")]), false),
                "look {look}"
            );
        }
        assert_eq!(
            starts.load(Ordering::SeqCst),
            1,
            "a folder not there yet is walked, never started again and again"
        );

        // A folder made anew, as another file while the old one lingers,
        // is watched again.
        let notes = scratch.join(QUEST).join("notes");
        let old = dir.path().join("old-notes");
        fs::rename(&notes, &old).expect("moved");
        fs::create_dir(&notes).expect("made anew");
        fs::remove_dir(&old).expect("removed");
        watch.early.push(Heard::At(notes));
        let (walk, _) = watch.to_look_at(QUEST, &all);
        assert_eq!(walk, BTreeSet::from([named("not-yet"), named("notes")]));
        assert_eq!(starts.load(Ordering::SeqCst), 2, "once, for the new notes");
        assert!(watch.stale.is_empty(), "{:?}", watch.stale);

        watch.forget(QUEST);
        assert_eq!(starts.load(Ordering::SeqCst), 3, "and given back at once");
        let held = watch
            .watcher
            .as_ref()
            .expect("a watcher")
            .watched_paths()
            .expect("its paths listed");
        assert!(held.is_empty(), "forget kept some paths watched: {held:?}");
        let (walk, record) = watch.to_look_at(QUEST, &all);
        assert_eq!((walk.len(), record), (4, true), "{walk:?}");
        assert_eq!(
            starts.load(Ordering::SeqCst),
            4,
            "a look after watches them anew"
        );
    }

    #[test]
    fn a_quest_forgotten_while_a_folder_and_its_record_are_stale_gives_all_back_at_once() {
        use std::sync::atomic::Ordering;

        use crate::watching::counting::Counting;

        let dir = tempfile::tempdir().expect("a temporary folder");
        let (scratch, records) = (dir.path().join("scratch"), dir.path().join("records"));
        for folder in ["notes", "bench"] {
            fs::create_dir_all(scratch.join(QUEST).join(folder)).expect("made");
        }
        fs::create_dir_all(records.join(QUEST)).expect("made");
        let (counting, starts) = Counting::made();
        let mut watch = Watch::watched_by(scratch.clone(), records.clone(), |_| {
            let watcher: Box<dyn Watcher + Send> = Box::new(counting);
            Ok(watcher)
        });
        watch.to_look_at(QUEST, &shared(&["notes", "bench"]));

        // Heard of whole, the folder and the record go stale, held by the
        // watcher still; a look naming neither watches neither again.
        watch
            .early
            .push(Heard::At(scratch.join(QUEST).join("notes")));
        watch.early.push(Heard::At(records.join(QUEST)));
        watch.to_walk(QUEST, &BTreeSet::new());
        assert_eq!(
            (watch.stale.len(), watch.records_stale.len()),
            (1, 1),
            "the folder and the record went stale: {:?} {:?}",
            watch.stale,
            watch.records_stale
        );

        let held = watch
            .watcher
            .as_ref()
            .expect("a watcher")
            .watched_paths()
            .expect("its paths listed");
        assert_eq!(
            held.into_iter()
                .map(|(path, _)| path)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                scratch.join(QUEST).join("bench"),
                scratch.join(QUEST).join("notes"),
                records.join(QUEST),
            ]),
            "stale or not, the watcher holds both folders and the record"
        );

        let before = starts.load(Ordering::SeqCst);
        watch.forget(QUEST);
        let held = watch
            .watcher
            .as_ref()
            .expect("a watcher")
            .watched_paths()
            .expect("its paths listed");
        assert!(
            held.is_empty(),
            "forget kept a stale path watched: {held:?}"
        );
        assert_eq!(
            starts.load(Ordering::SeqCst) - before,
            1,
            "forget gave its three paths back in more than one batch"
        );
        assert!(
            watch.stale.is_empty() && watch.records_stale.is_empty(),
            "forget kept the quest stale: {:?} {:?}",
            watch.stale,
            watch.records_stale
        );
    }
}
