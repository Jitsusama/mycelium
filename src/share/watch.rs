//! The scratch folders a link shares, watched for changes, so a look walks
//! only the folders that changed rather than every file in every one.
//!
//! A folder that can't be watched, one newly named, and every folder after
//! the watch fails or overflows, is walked anyway, so a watch that misses
//! something costs a walk, never a change.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
#[cfg(test)]
use std::time::Duration;

use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use super::tree::{Place, Shared, crosses};

/// A quest's scratch folder, by the quest's id and the folder's name.
pub type Folder = (String, String);

/// The shared scratch folders being watched.
pub struct Watch {
    /// The quests' scratch folders, one a quest, by its id.
    scratch: PathBuf,
    /// `scratch` with every link in it resolved, once it is there. macOS's
    /// watcher names a change by this path, so a scratch reached through a
    /// link, as `/var` is `/private/var` on a Mac, is matched by it.
    resolved: Option<PathBuf>,
    /// The watcher, unless one couldn't be made, when every folder is
    /// walked at every look.
    watcher: Option<RecommendedWatcher>,
    /// The folders watched.
    watched: BTreeSet<Folder>,
    /// What the watcher heard, not yet taken.
    heard: mpsc::Receiver<Heard>,
    /// What was taken from `heard` early, by a wait, to be taken again.
    early: Vec<Heard>,
    /// The folders heard to change, not yet walked.
    changed: BTreeSet<Folder>,
}

/// What the watcher heard.
enum Heard {
    /// Something changed at this path.
    At(PathBuf),
    /// The watch failed or overflowed, so anything may have changed.
    Anything,
}

impl Watch {
    /// A watch of the quests' scratch folders in `scratch`, watching none
    /// until [`Self::follow`] names them.
    #[must_use]
    pub fn new(scratch: PathBuf) -> Self {
        let (sender, heard) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let heard = match event {
                Err(_) => vec![Heard::Anything],
                Ok(event) if event.need_rescan() => vec![Heard::Anything],
                // A file opened or read holds nothing new; a look's own
                // reads would otherwise bring another look.
                Ok(event) if !alters(event.kind) => Vec::new(),
                Ok(event) => event.paths.into_iter().map(Heard::At).collect(),
            };
            for heard in heard {
                // Nobody hears once the link has gone; the watcher goes
                // with it.
                let _ = sender.send(heard);
            }
        })
        .inspect_err(|error| {
            eprintln!("shared scratch folders are walked at every look, unwatched: {error}");
        })
        .ok();
        Self {
            scratch,
            resolved: None,
            watcher,
            watched: BTreeSet::new(),
            heard,
            early: Vec::new(),
            changed: BTreeSet::new(),
        }
    }

    /// The folders among `folders`, those of quest `id` it shares, to walk
    /// at this look: those that changed since the last, and those not
    /// watched yet, which are watched from now on where they can be.
    pub fn to_walk(&mut self, id: &str, folders: &BTreeSet<String>) -> BTreeSet<String> {
        self.take_heard();
        let mut walk = BTreeSet::new();
        for folder in folders {
            let named = (id.to_owned(), folder.clone());
            if self.changed.remove(&named) {
                walk.insert(folder.clone());
            }
            if !self.watched.contains(&named) {
                if self.start(&named) {
                    self.watched.insert(named);
                }
                walk.insert(folder.clone());
            }
        }
        walk
    }

    /// Stops watching quest `id`'s folders, once it is shared no longer.
    pub fn forget(&mut self, id: &str) {
        let gone: Vec<Folder> = self
            .watched
            .iter()
            .filter(|(quest, _)| quest == id)
            .cloned()
            .collect();
        for folder in gone {
            if let Some(watcher) = self.watcher.as_mut() {
                // A folder already gone is no longer watched anyway.
                let _ = watcher.unwatch(&self.scratch.join(&folder.0).join(&folder.1));
            }
            self.watched.remove(&folder);
        }
        self.changed.retain(|(quest, _)| quest != id);
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

    /// Starts watching `folder`, answering whether it is watched.
    fn start(&mut self, (id, folder): &Folder) -> bool {
        let at = self.scratch.join(id).join(folder);
        // A folder that is a git clone crosses as its remote, and walking it
        // only finds its `.git`, where watching it whole could spend the
        // watches a machine has on a large clone's every folder.
        if at.join(".git").exists() {
            return false;
        }
        if self.resolved.is_none() {
            self.resolved = fs::canonicalize(&self.scratch).ok();
        }
        self.watcher.as_mut().is_some_and(|watcher| {
            // A folder not there yet is walked until it is, then watched.
            watcher.watch(&at, RecursiveMode::Recursive).is_ok()
        })
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
                Heard::Anything => self.changed.extend(self.watched.iter().cloned()),
                Heard::At(path) => {
                    let found = folder_of(&self.scratch, &path).or_else(|| {
                        let resolved = self.resolved.as_deref()?;
                        folder_of(resolved, &path)
                    });
                    if let Some((folder, whole)) = found {
                        if whole {
                            self.watched.remove(&folder);
                        }
                        self.changed.insert(folder);
                    }
                }
            }
        }
    }
}

/// How long a test waits for the watcher to hear a change.
#[cfg(test)]
pub const HEARD_WITHIN: Duration = Duration::from_secs(5);

/// The scratch folder a change at `path` is in, if it is one whose files
/// cross, and whether it was the folder itself that changed.
fn folder_of(scratch: &Path, path: &Path) -> Option<(Folder, bool)> {
    let within = path.strip_prefix(scratch).ok()?;
    let names: Option<Vec<&str>> = within.iter().map(|name| name.to_str()).collect();
    let names = names?;
    let [id, folder, rest @ ..] = names.as_slice() else {
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

    #[test]
    fn a_folder_is_walked_when_named_and_then_only_once_it_changes() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        for folder in ["notes", "bench"] {
            fs::create_dir_all(dir.path().join(QUEST).join(folder)).expect("made");
        }
        let mut watch = Watch::new(dir.path().to_owned());
        let both = shared(&["notes", "bench"]);
        assert_eq!(
            watch.to_walk(QUEST, &both),
            BTreeSet::from([named("bench"), named("notes")]),
            "each folder is walked once when first named"
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
    fn a_change_is_matched_to_its_folder_through_a_linked_scratch() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let real = dir.path().join("real");
        fs::create_dir_all(real.join(QUEST).join("notes")).expect("made");
        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(&real, &linked).expect("linked");
        let mut watch = Watch::new(linked.clone());
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
        let mut watch = Watch::new(dir.path().to_owned());
        let notes = shared(&["notes"]);
        watch.to_walk(QUEST, &notes);

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
        let mut watch = Watch::new(dir.path().to_owned());
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
}
