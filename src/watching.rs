//! A watch of folders that hears only what changes them.
//!
//! A Linux watch hears every open of a file in what it watches unless told
//! not to. The folders a node watches are read all day by every session on
//! the machine, so a work node heard about 3,000 opens a second and woke
//! for each, though it answers none (9 November). Asking the kernel for
//! only what writes, renames or removes keeps those from waking it at all.

use notify::{Config, EventHandler, EventKindMask, RecommendedWatcher, Watcher};

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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc;
    use std::time::Duration;

    use notify::{EventKind, RecursiveMode};

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
}
