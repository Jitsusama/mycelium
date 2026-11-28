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
//! each path it newly watches or gives back alone, and each start
//! registers anew with the system's file-events daemon. Watching a path at
//! a time, nodes and test runs kept that daemon on a whole core (26
//! November). Batched by [`watch_all`], a watcher's stream starts once for
//! every path a change names.

use std::path::{Path, PathBuf};

use notify::{
    Config, EventHandler, EventKindMask, PathOp, RecommendedWatcher, RecursiveMode,
    WatchPathConfig, Watcher,
};

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

/// What became of a path a change asked a watcher to watch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// It is watched.
    Watched,
    /// The watcher holds it, but it isn't watched: the stream didn't
    /// start, or the watcher refused it yet holds it still, from before or
    /// from taking part of it, as Linux's does when it runs out of watches
    /// partway down a folder.
    Held,
    /// The watcher refused it and holds nothing of it.
    Refused,
}

/// Gives back each path in `gone` and watches each in `new`, as one
/// change to `watcher`, answering for each of `new` whether it is watched.
/// A path given back that wasn't watched is no matter. A change stops at
/// the first path that fails, so the rest are applied again after it, at
/// a start each; a caller leaves out what it knows isn't there. If the
/// stream can't start, none of `new` is watched. A change naming nothing
/// leaves the watcher be.
pub fn watch_all(
    watcher: &mut dyn Watcher,
    gone: &[PathBuf],
    new: &[(PathBuf, RecursiveMode)],
) -> Vec<bool> {
    answer_all(watcher, gone, new)
        .into_iter()
        .map(|answer| answer == Answer::Watched)
        .collect()
}

/// As [`watch_all`], but answering for each of `new` whether it is
/// watched, held unwatched or refused, for a caller that gives back only
/// what the watcher holds: a batch stops at a path given back that it
/// doesn't hold, as it does at one that fails to watch.
pub fn answer_all(
    watcher: &mut dyn Watcher,
    gone: &[PathBuf],
    new: &[(PathBuf, RecursiveMode)],
) -> Vec<Answer> {
    let mut answers = vec![Answer::Watched; new.len()];
    let mut change: Vec<PathOp> = gone
        .iter()
        .map(|path| PathOp::unwatch(path.clone()))
        .chain(
            new.iter()
                .map(|(path, mode)| PathOp::Watch(path.clone(), WatchPathConfig::new(*mode))),
        )
        .collect();
    while !change.is_empty() {
        let Err(failed) = watcher.update_paths(change) else {
            break;
        };
        match failed.origin {
            Some(PathOp::Watch(path, _)) => {
                // A refusal can leave a path held, so it is given back
                // unless the watcher says it holds nothing of it.
                let refused = if holds(watcher, &path) {
                    Answer::Held
                } else {
                    Answer::Refused
                };
                for (answer, (at, _)) in answers.iter_mut().zip(new) {
                    if *at == path {
                        *answer = refused;
                    }
                }
            }
            // A path not watched has nothing to give back.
            Some(PathOp::Unwatch(_)) => {}
            // Every path was applied, but the stream didn't start, so the
            // watcher holds those it didn't refuse.
            None => {
                for answer in &mut answers {
                    if *answer == Answer::Watched {
                        *answer = Answer::Held;
                    }
                }
                return answers;
            }
        }
        change = failed.remaining;
    }
    answers
}

/// Whether `watcher` holds `path`, or can't say what it holds.
fn holds(watcher: &dyn Watcher, path: &Path) -> bool {
    match watcher.watched_paths() {
        Ok(held) => held.iter().any(|(at, _)| at.as_path() == path),
        Err(_) => true,
    }
}

/// A watcher for tests that counts how often its stream would start, as
/// macOS's does: once for each change that alters what it watches, a path
/// at a time or a batch at once. Like notify's, it keeps one watch a path,
/// a path watched again unchanged alters nothing, and a batch stops at
/// the first path that fails: one not there, or one given back unwatched.
/// A path that fails alters nothing, as notify 9.0.0-rc.7's macOS watcher
/// refuses it before holding it, and refuses to give back a path it
/// doesn't hold before stopping anything. Made to, it holds a path it
/// refuses, as that version's Linux watcher keeps a folder and those under
/// it it took before it ran out of watches.
///
/// That watcher starts a batch's stream again only when a root's path,
/// device or inode changes. It starts a single watch's again only when no
/// folder it watches recursively holds the path and the path's file or
/// mode changed, and a single unwatch's only when no such folder holds it.
/// This counts a start for any path watched anew or another way and any
/// path given back, held inside a recursive root or not, in a batch or
/// alone, so it counts a start at least as often as macOS does. It lists
/// what it holds, as notify's own watchers do, and may be made to refuse a
/// path, as a system out of watches would, to fail to start its stream
/// once a batch is applied, or to fail to list what it holds.
#[cfg(test)]
pub mod counting {
    use std::collections::BTreeMap;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use notify::{
        Config, EventHandler, PathOp, RecursiveMode, UpdatePathsError, Watcher, WatcherKind,
    };

    /// The watcher, sharing its count of starts with whoever made it.
    pub struct Counting {
        starts: Arc<AtomicUsize>,
        /// Each path watched, by the file it was when watched and how.
        watched: BTreeMap<PathBuf, ((u64, u64), RecursiveMode)>,
        /// A path it refuses to watch, if any.
        refused: Option<PathBuf>,
        /// Whether it takes the path it refuses before refusing it.
        partway: bool,
        /// Whether its stream fails to start after a batch alters what it
        /// holds.
        stalled: bool,
        /// Whether it fails to list what it holds.
        unlisted: bool,
    }

    impl Counting {
        /// A counting watcher and its count.
        #[must_use]
        pub fn made() -> (Self, Arc<AtomicUsize>) {
            let starts = Arc::new(AtomicUsize::new(0));
            let watcher = Self {
                starts: Arc::clone(&starts),
                watched: BTreeMap::new(),
                refused: None,
                partway: false,
                stalled: false,
                unlisted: false,
            };
            (watcher, starts)
        }

        /// A counting watcher that refuses to watch `path`, and its count.
        #[must_use]
        pub fn refusing(path: PathBuf) -> (Self, Arc<AtomicUsize>) {
            let (mut watcher, starts) = Self::made();
            watcher.refused = Some(path);
            (watcher, starts)
        }

        /// A counting watcher that takes `path` and then refuses it, as
        /// Linux's does running out of watches partway down a folder, and
        /// its count.
        #[must_use]
        pub fn refusing_partway(path: PathBuf) -> (Self, Arc<AtomicUsize>) {
            let (mut watcher, starts) = Self::refusing(path);
            watcher.partway = true;
            (watcher, starts)
        }

        /// Refuses `path` from now on, holding it still if it holds it.
        pub fn refuse(&mut self, path: PathBuf) {
            self.refused = Some(path);
        }

        /// Fails from now on to start its stream after a batch alters what
        /// it holds, holding the batch's paths still, as macOS's does when
        /// its file-events stream won't start.
        pub fn stall(&mut self) {
            self.stalled = true;
        }

        /// Fails from now on to list what it holds.
        pub fn unlist(&mut self) {
            self.unlisted = true;
        }

        /// Applies `op`, answering whether it altered what's watched.
        fn apply(&mut self, op: &PathOp) -> notify::Result<bool> {
            match op {
                PathOp::Watch(path, config) if self.refused.as_ref() == Some(path) => {
                    if self.partway {
                        self.take(path, config.recursive_mode())?;
                    }
                    Err(notify::Error::generic("the watch was refused").add_path(path.clone()))
                }
                PathOp::Watch(path, config) => self.take(path, config.recursive_mode()),
                PathOp::Unwatch(path) => self
                    .watched
                    .remove(path)
                    .map(|_| true)
                    .ok_or_else(notify::Error::watch_not_found),
            }
        }

        /// Holds `path` watched `mode`, answering whether that altered
        /// what's watched.
        fn take(&mut self, path: &Path, mode: RecursiveMode) -> notify::Result<bool> {
            let file = path
                .metadata()
                .map_err(|_| notify::Error::path_not_found().add_path(path.to_owned()))?;
            let now = ((file.dev(), file.ino()), mode);
            let was = self.watched.insert(path.to_owned(), now);
            Ok(was != Some(now))
        }

        /// Starts the stream again if `altered`.
        fn start(&self, altered: bool) {
            if altered {
                self.starts.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    impl Watcher for Counting {
        fn new<F: EventHandler>(_handler: F, _config: Config) -> notify::Result<Self> {
            Ok(Self::made().0)
        }

        fn watch(&mut self, path: &Path, mode: RecursiveMode) -> notify::Result<()> {
            let op = match mode {
                RecursiveMode::Recursive => PathOp::watch_recursive(path),
                RecursiveMode::NonRecursive => PathOp::watch_non_recursive(path),
            };
            let altered = self.apply(&op)?;
            self.start(altered);
            Ok(())
        }

        fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
            let altered = self.apply(&PathOp::unwatch(path))?;
            self.start(altered);
            Ok(())
        }

        fn update_paths(&mut self, ops: Vec<PathOp>) -> Result<(), UpdatePathsError> {
            let mut altered = false;
            let mut ops = ops.into_iter();
            while let Some(op) = ops.next() {
                match self.apply(&op) {
                    Ok(this) => altered |= this,
                    Err(source) => {
                        self.start(altered);
                        return Err(UpdatePathsError {
                            source,
                            origin: Some(op),
                            remaining: ops.collect(),
                        });
                    }
                }
            }
            self.start(altered);
            if altered && self.stalled {
                return Err(UpdatePathsError {
                    source: notify::Error::generic("the stream didn't start"),
                    origin: None,
                    remaining: Vec::new(),
                });
            }
            Ok(())
        }

        fn watched_paths(&self) -> notify::Result<Vec<(PathBuf, RecursiveMode)>> {
            if self.unlisted {
                return Err(notify::Error::generic("its paths can't be listed"));
            }
            Ok(self
                .watched
                .iter()
                .map(|(path, (_, mode))| (path.clone(), *mode))
                .collect())
        }

        fn kind() -> WatcherKind {
            WatcherKind::NullWatcher
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
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

    #[test]
    fn the_paths_after_one_not_there_are_watched_still() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        for folder in [&a, &b] {
            fs::create_dir(folder).expect("made");
        }
        let (mut watcher, starts) = Counting::made();
        let new = [
            (a, RecursiveMode::NonRecursive),
            (dir.path().join("not-yet"), RecursiveMode::NonRecursive),
            (b.clone(), RecursiveMode::NonRecursive),
        ];
        assert_eq!(watch_all(&mut watcher, &[], &new), [true, false, true]);
        assert_eq!(
            starts.load(Ordering::SeqCst),
            2,
            "once before the path not there, once for the rest after it"
        );
        let again = [(b, RecursiveMode::NonRecursive)];
        assert_eq!(watch_all(&mut watcher, &[], &again), [true]);
        assert_eq!(
            starts.load(Ordering::SeqCst),
            2,
            "the path after it was held, so watching it again alters nothing"
        );
    }

    #[test]
    fn a_path_refused_is_answered_refused_and_left_unheld() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        for folder in [&a, &b] {
            fs::create_dir(folder).expect("made");
        }
        let (mut watcher, _) = Counting::refusing(b.clone());
        let new = [
            (a.clone(), RecursiveMode::NonRecursive),
            (b, RecursiveMode::NonRecursive),
        ];
        assert_eq!(
            answer_all(&mut watcher, &[], &new),
            [Answer::Watched, Answer::Refused]
        );
        assert_eq!(
            watcher.watched_paths().expect("its paths listed"),
            [(a, RecursiveMode::NonRecursive)],
            "the fake holds only the path it took"
        );
    }

    #[test]
    fn a_path_refused_partway_is_answered_held() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        for folder in [&a, &b] {
            fs::create_dir(folder).expect("made");
        }
        // As Linux's watcher, out of watches partway down `b`, keeps what
        // it took of it, so a forget must give it back.
        let (mut watcher, _) = Counting::refusing_partway(b.clone());
        let new = [(a, RecursiveMode::Recursive), (b, RecursiveMode::Recursive)];
        assert_eq!(
            answer_all(&mut watcher, &[], &new),
            [Answer::Watched, Answer::Held],
            "a path refused but held was answered as holding nothing"
        );
    }

    #[test]
    fn a_path_held_already_and_then_refused_is_answered_held() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut watcher, _) = Counting::made();
        let new = [(dir.path().to_owned(), RecursiveMode::Recursive)];
        assert_eq!(answer_all(&mut watcher, &[], &new), [Answer::Watched]);
        watcher.refuse(dir.path().to_owned());
        assert_eq!(
            answer_all(&mut watcher, &[], &new),
            [Answer::Held],
            "a path the watcher still holds was answered as holding nothing"
        );
    }

    #[test]
    fn paths_a_batch_applied_but_whose_stream_didn_t_start_are_answered_held() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        for folder in [&a, &b] {
            fs::create_dir(folder).expect("made");
        }
        // `b` is refused first, so the rest of the batch is applied again
        // after it, and that stream is the one that doesn't start.
        let (mut watcher, _) = Counting::refusing(b.clone());
        watcher.stall();
        let new = [
            (b, RecursiveMode::NonRecursive),
            (a.clone(), RecursiveMode::NonRecursive),
        ];
        assert_eq!(
            answer_all(&mut watcher, &[], &new),
            [Answer::Refused, Answer::Held],
            "a path held by a stream that didn't start was answered watched, or the \
             refused one held"
        );
        assert_eq!(
            watcher.watched_paths().expect("its paths listed"),
            [(a, RecursiveMode::NonRecursive)],
            "the fake holds the path it took though its stream didn't start"
        );
    }

    #[test]
    fn a_path_refused_by_a_watcher_that_can_t_list_its_paths_is_answered_held() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        for folder in [&a, &b] {
            fs::create_dir(folder).expect("made");
        }
        // It holds nothing of `b`, but can't say so, so `b` may be held.
        let (mut watcher, _) = Counting::refusing(b.clone());
        watcher.unlist();
        let new = [
            (a, RecursiveMode::NonRecursive),
            (b, RecursiveMode::NonRecursive),
        ];
        assert_eq!(
            answer_all(&mut watcher, &[], &new),
            [Answer::Watched, Answer::Held],
            "a path refused by a watcher that can't say what it holds was answered \
             as holding nothing"
        );
    }

    #[test]
    fn the_counting_watcher_counts_a_path_watched_another_way_as_a_start() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut watcher, starts) = Counting::made();
        watcher
            .watch(dir.path(), RecursiveMode::NonRecursive)
            .expect("watched");
        watcher
            .watch(dir.path(), RecursiveMode::Recursive)
            .expect("watched again");
        assert_eq!(
            starts.load(Ordering::SeqCst),
            2,
            "a single watch in another mode starts macOS's stream again, so it counts"
        );
        assert_eq!(
            watcher.watched_paths().expect("its paths listed"),
            [(dir.path().to_owned(), RecursiveMode::Recursive)],
            "held the new way"
        );
    }

    #[test]
    fn the_system_s_watcher_answers_a_path_not_there_beside_the_rest() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let inside = dir.path().join("inside");
        fs::create_dir(&inside).expect("made");
        let mut watch = watcher(|_: notify::Result<notify::Event>| {}).expect("a watcher");
        let new = [
            (dir.path().to_owned(), RecursiveMode::NonRecursive),
            (dir.path().join("not-yet"), RecursiveMode::NonRecursive),
            (inside.clone(), RecursiveMode::NonRecursive),
        ];
        assert_eq!(watch_all(&mut watch, &[], &new), [true, false, true]);
        // Its own list, as it may hold a path by another name.
        let real = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        let mut held: Vec<PathBuf> = watch
            .watched_paths()
            .expect("its paths listed")
            .iter()
            .map(|(path, _)| real(path))
            .collect();
        held.sort();
        assert_eq!(
            held,
            [real(dir.path()), real(&inside)],
            "the path after the one not there is watched too"
        );
    }
}
