//! What a machine does with a shared file when it hears the other end's
//! version of it.
//!
//! The newer version wins whole, a deletion as much as a write. Two
//! written apart are settled here: a journey or a harness log is merged,
//! since both ends add to it all the time; a deletion gives way to a
//! write, so nothing either end wrote is lost; anything else keeps this
//! end's and puts the other's beside it, for whoever works on the quest
//! to settle, and is said to be.

use serde::{Deserialize, Serialize};

use super::tree::{Place, Shared};
use super::version::{Standing, Version};

/// One end's state of a shared file; by default, a file it never held.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// What it holds, by its content's hash, or `None` once deleted.
    pub hash: Option<String>,
    /// Which machines' changes it has seen.
    pub version: Version,
}

/// What to do with this end's file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settled {
    /// Nothing: this end has it, or has something newer the other end
    /// will take.
    Keep,
    /// Take the other end's whole: its content, or its deletion.
    Take,
    /// Merge the two by the file's shape, as [`Merge`] says.
    Merge(Merge),
    /// Keep this end's and put the other's beside it, named for its
    /// machine, and say so.
    Beside,
}

/// How a file written apart on two ends is merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Merge {
    /// A quest's journey, by entry.
    Journey,
    /// A quest's harness log, by line.
    Log,
}

/// What to do with this end's file, `ours`, on hearing `theirs`.
#[must_use]
pub fn settle(shared: &Shared, ours: &State, theirs: &State) -> Settled {
    if ours.hash == theirs.hash {
        return Settled::Keep;
    }
    match ours.version.against(&theirs.version) {
        Standing::Newer => Settled::Keep,
        Standing::Same | Standing::Older => Settled::Take,
        Standing::Apart => match (&ours.hash, &theirs.hash) {
            (None, Some(_)) => Settled::Take,
            (Some(_), None) => Settled::Keep,
            _ => merge_for(shared).map_or(Settled::Beside, Settled::Merge),
        },
    }
}

/// The name the other end's file is put under beside this end's: its own
/// with the machine it came from, which never crosses back.
#[must_use]
pub fn beside(path: &str, machine: &str) -> String {
    format!("{path}.from-{machine}")
}

fn merge_for(shared: &Shared) -> Option<Merge> {
    match (shared.place, shared.path.as_str()) {
        (Place::Record, "JOURNEY.md") => Some(Merge::Journey),
        (Place::Record, "harness.log") => Some(Merge::Log),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(path: &str) -> Shared {
        Shared {
            place: Place::Record,
            path: path.to_owned(),
        }
    }

    fn state(hash: Option<&str>, version: &Version) -> State {
        State {
            hash: hash.map(str::to_owned),
            version: version.clone(),
        }
    }

    /// Two versions of one file: changed once on optimus, then here
    /// ahead of there, and there ahead of here.
    fn versions() -> (Version, Version, Version) {
        let shared = Version::default().changed_on("optimus");
        let here = shared.changed_on("optimus");
        let there = shared.changed_on("equilibrius");
        (shared, here, there)
    }

    #[test]
    fn the_newer_version_wins_whole() {
        let (shared, here, _) = versions();
        let readme = record("README.md");
        assert_eq!(
            settle(
                &readme,
                &state(Some("a"), &shared),
                &state(Some("b"), &here)
            ),
            Settled::Take,
            "a newer version from the other end is taken"
        );
        assert_eq!(
            settle(
                &readme,
                &state(Some("b"), &here),
                &state(Some("a"), &shared)
            ),
            Settled::Keep,
            "an older version from the other end leaves ours"
        );
        assert_eq!(
            settle(&readme, &state(Some("a"), &shared), &state(None, &here)),
            Settled::Take,
            "a newer deletion is taken too"
        );
        assert_eq!(
            settle(
                &readme,
                &state(Some("a"), &here),
                &state(Some("a"), &shared)
            ),
            Settled::Keep,
            "the same content needs nothing whatever the versions"
        );
    }

    #[test]
    fn a_journey_or_log_written_apart_is_merged() {
        let (_, here, there) = versions();
        assert_eq!(
            settle(
                &record("JOURNEY.md"),
                &state(Some("a"), &here),
                &state(Some("b"), &there)
            ),
            Settled::Merge(Merge::Journey),
            "a journey written on both ends is merged by entry"
        );
        assert_eq!(
            settle(
                &record("harness.log"),
                &state(Some("a"), &here),
                &state(Some("b"), &there)
            ),
            Settled::Merge(Merge::Log),
            "a harness log written on both ends is merged by line"
        );
    }

    #[test]
    fn anything_else_written_apart_keeps_both() {
        let (_, here, there) = versions();
        let plan = record("plans/PLAN-20261026-IZH11J.md");
        assert_eq!(
            settle(&plan, &state(Some("a"), &here), &state(Some("b"), &there)),
            Settled::Beside,
            "a document written on both ends keeps ours with theirs beside it"
        );
        assert_eq!(
            settle(&plan, &state(None, &here), &state(Some("b"), &there)),
            Settled::Take,
            "a write on the other end outlasts a deletion here"
        );
        assert_eq!(
            settle(&plan, &state(Some("a"), &here), &state(None, &there)),
            Settled::Keep,
            "a write here outlasts a deletion there"
        );
        assert_eq!(
            beside("plans/PLAN-20261026-IZH11J.md", "equilibrius"),
            "plans/PLAN-20261026-IZH11J.md.from-equilibrius",
            "theirs is named for its machine"
        );
    }
}
