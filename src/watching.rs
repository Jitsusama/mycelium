//! A watch of folders that hears only what changes them, and takes and
//! gives back its paths in batches.
//!
//! A Linux watch hears every open of a file in what it watches unless told
//! not to. The folders a node watches are read all day by every session on
//! the machine, so a work node heard about 3,000 opens a second and woke
//! for each, though it answers none (9 November). Asking the kernel for
//! only what writes, renames or removes keeps those from waking it at all.
//!
//! macOS's watcher stops its file-events stream and starts it again for
//! every path watched or given back alone, and each start registers anew
//! with the system's file-events daemon. Watching a path at a time, nodes
//! and test runs kept that daemon on a whole core (26 November). Batched
//! by [`watch_all`], a watcher's stream starts once for every path a
//! change names.

use std::path::PathBuf;

use notify::{Config, EventHandler, EventKindMask, RecommendedWatcher, RecursiveMode, Watcher};

/// What a watch hears: a file made, removed, written or renamed. Not its
/// opens, closes or reads, nor new permissions or times, which change
/// nothing a node reads from it.
pub const CHANGES: EventKindMask = EventKindMask::CREATE
    .union(EventKindMask::REMOVE)
    .union(EventKindMask::MODIFY_DATA)
    .union(EventKindMask::MODIFY_NAME);

/// A watcher telling `handler` what changes in the folders it is then
/// asked to watch, and nothing else.
///
/// # Errors
///
/// When the system's watch can't be made.
pub fn watcher<F: EventHandler>(handler: F) -> notify::Result<RecommendedWatcher> {
    RecommendedWatcher::new(handler, Config::default().with_event_kinds(CHANGES))
}

/// Gives back each path in `gone` and watches each in `new`, as one
/// change to `watcher`, answering for each of `new` whether it is watched.
/// A path given back that wasn't watched is no matter. If the change
/// can't be applied, none of `new` is watched. A change naming nothing
/// leaves the watcher be.
pub fn watch_all(
    watcher: &mut dyn Watcher,
    gone: &[PathBuf],
    new: &[(PathBuf, RecursiveMode)],
) -> Vec<bool> {
    if gone.is_empty() && new.is_empty() {
        return Vec::new();
    }
    let mut paths = watcher.paths_mut();
    for path in gone {
        // A path not watched has nothing to give back.
        let _ = paths.remove(path);
    }
    let watched: Vec<bool> = new
        .iter()
        .map(|(path, mode)| paths.add(path, *mode).is_ok())
        .collect();
    if paths.commit().is_err() {
        return vec![false; new.len()];
    }
    watched
}

/// A watcher for tests that counts how often its stream would start: once
/// for each batch committed, and once for each path watched or given back
/// alone, as macOS's does.
#[cfg(test)]
pub mod counting {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use notify::{Config, EventHandler, PathsMut, RecursiveMode, Watcher, WatcherKind};

    /// The watcher, sharing its count of starts with whoever made it.
    pub struct Counting(Arc<AtomicUsize>);

    impl Counting {
        /// A counting watcher and its count.
        #[must_use]
        pub fn made() -> (Self, Arc<AtomicUsize>) {
            let starts = Arc::new(AtomicUsize::new(0));
            (Self(Arc::clone(&starts)), starts)
        }
    }

    /// Whether `path` is there to watch, as every watcher asks first.
    fn there(path: &Path) -> notify::Result<()> {
        if path.exists() {
            Ok(())
        } else {
            Err(notify::Error::path_not_found().add_path(path.to_owned()))
        }
    }

    /// A batch, starting the stream once when committed.
    struct Batch<'a>(&'a AtomicUsize);

    impl PathsMut for Batch<'_> {
        fn add(&mut self, path: &Path, _mode: RecursiveMode) -> notify::Result<()> {
            there(path)
        }

        fn remove(&mut self, _path: &Path) -> notify::Result<()> {
            Ok(())
        }

        fn commit(self: Box<Self>) -> notify::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    impl Watcher for Counting {
        fn new<F: EventHandler>(_handler: F, _config: Config) -> notify::Result<Self> {
            Ok(Self::made().0)
        }

        fn watch(&mut self, path: &Path, _mode: RecursiveMode) -> notify::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            there(path)
        }

        fn unwatch(&mut self, _path: &Path) -> notify::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn paths_mut<'me>(&'me mut self) -> Box<dyn PathsMut + 'me> {
            Box::new(Batch(&self.0))
        }

        fn kind() -> WatcherKind {
            WatcherKind::NullWatcher
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    use std::time::Duration;

    use notify::{EventKind, RecursiveMode};

    use super::counting::Counting;
    use super::*;

    /// How long a test waits for an event it is owed.
    const PATIENCE: Duration = Duration::from_secs(30);

    #[test]
    fn a_watch_hears_a_file_written_and_never_one_read() {
        let dir = tempfile::tempdir().expect("a folder");
        let (tell, heard) = mpsc::channel();
        let mut watch = watcher(move |event: notify::Result<notify::Event>| {
            let _ = tell.send(event.expect("an event").kind);
        })
        .expect("a watcher");
        watch
            .watch(dir.path(), RecursiveMode::NonRecursive)
            .expect("the folder is watched");
        let file = dir.path().join("one.json");
        fs::write(&file, "{}").expect("a file written");
        let kind = heard.recv_timeout(PATIENCE).expect("the write is heard");
        assert!(!matches!(kind, EventKind::Access(_)), "heard {kind:?}");
        // Drain what the first write said, so only what follows is read.
        while heard.recv_timeout(Duration::ZERO).is_ok() {}

        for _ in 0..20 {
            fs::read(&file).expect("the file read");
        }
        // A second file is written after the reads, so once it is heard
        // everything the reads could have said has been said before it.
        fs::write(dir.path().join("two.json"), "{}").expect("a second file written");
        let mut kinds = Vec::new();
        while let Ok(kind) = heard.recv_timeout(PATIENCE) {
            let created = matches!(kind, EventKind::Create(_));
            kinds.push(kind);
            if created {
                break;
            }
        }
        assert!(
            kinds
                .iter()
                .any(|kind| matches!(kind, EventKind::Create(_))),
            "the second file was never heard: {kinds:?}"
        );
        assert!(
            !kinds
                .iter()
                .any(|kind| matches!(kind, EventKind::Access(_))),
            "the reads were heard: {kinds:?}"
        );
    }

    #[test]
    fn every_path_a_change_names_starts_the_stream_once() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let new: Vec<(PathBuf, RecursiveMode)> = ["a", "b", "c"]
            .iter()
            .map(|name| {
                let path = dir.path().join(name);
                fs::create_dir(&path).expect("made");
                (path, RecursiveMode::NonRecursive)
            })
            .collect();
        let (mut watcher, starts) = Counting::made();
        let gone = [dir.path().join("old")];
        assert_eq!(watch_all(&mut watcher, &gone, &new), [true; 3]);
        assert_eq!(starts.load(Ordering::SeqCst), 1, "one start for all four");
        assert!(
            watch_all(&mut watcher, &[], &[]).is_empty(),
            "and none for a change naming nothing"
        );
        assert_eq!(starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_path_not_there_is_answered_unwatched_beside_those_that_are() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut watcher, _) = Counting::made();
        let new = [
            (dir.path().to_owned(), RecursiveMode::Recursive),
            (dir.path().join("not-yet"), RecursiveMode::Recursive),
        ];
        assert_eq!(watch_all(&mut watcher, &[], &new), [true, false]);
    }
}
