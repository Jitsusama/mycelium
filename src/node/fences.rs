//! The strictest fence each session here was ever seen to run in, kept
//! across the node's restarts.
//!
//! What a session's process shows of its fence comes and goes: a
//! bubblewrap lives only while a command it fences runs, and a session
//! may switch its fence on after its pi began, which its starting
//! environment never says. So a fence missing from what a process shows
//! proves nothing, and the node keeps the strictest it has seen, which
//! no session can lower: a carry is judged by the strictest of what the
//! session claims, what its process shows now and what was seen before.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use crate::declared::Kind;

/// The strictest fence each session was seen in, as kept in its file.
pub struct Seen {
    file: PathBuf,
    /// What was seen, or why it can't be told, as when its file couldn't
    /// be read or kept.
    held: Mutex<Result<BTreeMap<String, Kind>, String>>,
}

impl Seen {
    /// What `file` says was seen, nothing where it doesn't exist yet.
    pub fn at(file: PathBuf) -> Self {
        let held = match fs::read(&file) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| unread(&file, &error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(unread(&file, &error)),
        };
        Self {
            file,
            held: Mutex::new(held),
        }
    }

    /// Raises each session's strictest fence to what `shown` says where
    /// that is stricter, keeping it in its file; whether any rose. Where
    /// it can't be kept, nothing is told from then on, so a session's
    /// strictest fence is never forgotten unawares.
    pub fn raise(&self, shown: &BTreeMap<String, Kind>) -> bool {
        let mut held = self.lock();
        let Ok(seen) = held.as_mut() else {
            return false;
        };
        let mut rose = false;
        for (session, kind) in shown {
            if *kind > Kind::None && seen.get(session).is_none_or(|was| kind > was) {
                seen.insert(session.clone(), *kind);
                rose = true;
            }
        }
        if rose && let Err(error) = keep(&self.file, seen) {
            *held = Err(format!(
                "{} can't be kept ({error}), so the strictest fence each session was seen in \
                 can't be told",
                self.file.display()
            ));
        }
        rose
    }

    /// The strictest fence `session` was seen in, unfenced where none was.
    ///
    /// # Errors
    ///
    /// Where what was seen can't be told, as when its file can't be read.
    pub fn strictest(&self, session: &str) -> Result<Kind, String> {
        self.lock()
            .as_ref()
            .map(|seen| seen.get(session).copied().unwrap_or(Kind::None))
            .map_err(Clone::clone)
    }

    fn lock(&self) -> MutexGuard<'_, Result<BTreeMap<String, Kind>, String>> {
        // A panic while holding it leaves what was seen as it was before
        // the change that panicked, so carrying on with it is sound.
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn unread(file: &std::path::Path, error: &dyn std::fmt::Display) -> String {
    format!(
        "{} can't be read ({error}), so the strictest fence each session was seen in can't \
         be told",
        file.display()
    )
}

/// Writes `seen` to `file` whole, beside it first and renamed into place,
/// so a reader never finds it half written.
fn keep(file: &std::path::Path, seen: &BTreeMap<String, Kind>) -> io::Result<()> {
    let beside = file.with_extension("json.new");
    fs::write(&beside, serde_json::to_vec(seen)?)?;
    fs::rename(&beside, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(fences: &[(&str, Kind)]) -> BTreeMap<String, Kind> {
        fences
            .iter()
            .map(|(session, kind)| ((*session).to_owned(), *kind))
            .collect()
    }

    #[test]
    fn a_session_is_judged_by_the_strictest_fence_it_was_seen_in() {
        let dir = tempfile::tempdir().expect("a folder");
        let seen = Seen::at(dir.path().join("fences.json"));
        assert_eq!(seen.strictest("s-one"), Ok(Kind::None), "none seen yet");
        assert!(seen.raise(&shown(&[("s-one", Kind::Strict), ("s-two", Kind::None)])));
        assert!(
            !seen.raise(&shown(&[("s-one", Kind::Fence), ("s-two", Kind::None)])),
            "a looser fence shown later lowers nothing"
        );
        assert_eq!(seen.strictest("s-one"), Ok(Kind::Strict));
        assert!(seen.raise(&shown(&[("s-two", Kind::Fence)])));
        assert_eq!(seen.strictest("s-two"), Ok(Kind::Fence));
    }

    #[test]
    fn what_was_seen_outlasts_the_node() {
        let dir = tempfile::tempdir().expect("a folder");
        let file = dir.path().join("fences.json");
        Seen::at(file.clone()).raise(&shown(&[("s-one", Kind::Fence)]));
        assert_eq!(
            Seen::at(file).strictest("s-one"),
            Ok(Kind::Fence),
            "a node started again remembers it"
        );
    }

    #[test]
    fn what_was_seen_that_cant_be_read_tells_nothing() {
        let dir = tempfile::tempdir().expect("a folder");
        let file = dir.path().join("fences.json");
        fs::write(&file, "{\"s-one\": \"str").expect("half written");
        let seen = Seen::at(file);
        assert!(
            seen.strictest("s-one")
                .is_err_and(|why| why.contains("can't be read")),
            "never taken for unfenced"
        );
        assert!(
            !seen.raise(&shown(&[("s-one", Kind::Strict)])),
            "nor written over"
        );
    }
}
