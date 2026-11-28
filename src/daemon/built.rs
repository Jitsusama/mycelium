//! Each kind's records built once for every watch of it, and at most once
//! for each [`BETWEEN_SENDS`], however its watches' wakings fall.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::time::Instant;

use super::records::{Build, shared};
use super::{BETWEEN_SENDS, Holds};

/// A kind's records as last built: the count of its `told` they were
/// built at, and when.
struct Last {
    /// The count its `told` had reached when they were built.
    told: u64,
    /// When they were built.
    at: Instant,
    /// The records.
    build: Arc<Build>,
}

/// The records a watch is given, and whether they may lack a change it was
/// told of.
#[derive(Default)]
pub struct Given {
    /// The records.
    pub build: Arc<Build>,
    /// Whether they were built at another count than the watch asked at,
    /// shared from a build made within [`BETWEEN_SENDS`] before, so may
    /// lack a change it was told of: it then asks again once that time
    /// passes, when a newer build may be made.
    pub behind: bool,
}

/// Each kind's records as built for the latest change its `told` says,
/// shared by every watch of the kind. A node with a hundred sessions and
/// a dozen watches of them built each session's record a dozen times for
/// every change (12 November); built once, each watch only compares. A
/// record that didn't change is kept as it was, so each watch finds it
/// unchanged by its address alone, and every watch holds one copy of it
/// rather than its own (14 November: 93 watches each held a copy of
/// every record, and compared every field of each on every change).
///
/// A kind is built at most once each [`BETWEEN_SENDS`]. While sessions
/// change steadily, watches of one kind wake at different moments and
/// read different counts, so a build for each count made one for each
/// watch: on 27 November a node's watches spent 54% of its one thread
/// building records, and a watch took 4 to 6 seconds to go live.
#[derive(Default)]
pub struct Built(Mutex<HashMap<String, Last>>);

impl Built {
    /// `kind`'s records as `holds` holds them once its `told` has counted
    /// to `told`. The build made at that count is shared by every watch
    /// asking at it; one made within [`BETWEEN_SENDS`] before is shared
    /// too, though made at another count, saying so, as it may lack the
    /// change asked at. A daemon changes what it holds before it counts,
    /// so records built at a count hold at least that change.
    pub fn records(&self, holds: &impl Holds, kind: &str, told: u64) -> Option<Given> {
        // Held while building, so watches woken by one change wait for the
        // first's build rather than each making their own.
        let mut builds = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        if let Some(last) = builds.get(kind) {
            let build = Arc::clone(&last.build);
            if last.told == told {
                return Some(Given {
                    build,
                    behind: false,
                });
            }
            if now < last.at + BETWEEN_SENDS {
                return Some(Given {
                    build,
                    behind: true,
                });
            }
        }
        let fresh = holds.records(kind)?;
        let before = builds.get(kind).map(|last| last.build.shared_records());
        let records = shared(before.as_deref(), fresh);
        let build = Arc::new(Build::after(records, before));
        let last = Last {
            told,
            at: now,
            build: Arc::clone(&build),
        };
        builds.insert(kind.to_owned(), last);
        Some(Given {
            build,
            behind: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU32, Ordering};

    use serde_json::{Map, Value, json};
    use tokio::sync::watch;

    use super::super::{Record, record};
    use super::*;

    /// A daemon holding one session, which counts how often its records
    /// are built.
    #[derive(Default)]
    struct Counting {
        builds: AtomicU32,
        step: Mutex<String>,
    }

    impl Counting {
        fn builds(&self) -> u32 {
            self.builds.load(Ordering::Relaxed)
        }

        fn step(&self, step: &str) {
            step.clone_into(&mut self.step.lock().expect("its step"));
        }
    }

    impl Holds for Counting {
        fn kinds(&self) -> BTreeMap<String, Vec<&'static str>> {
            BTreeMap::from([("session".to_owned(), vec!["step"])])
        }

        fn records(&self, kind: &str) -> Option<BTreeMap<String, Record>> {
            (kind == "session").then(|| {
                self.builds.fetch_add(1, Ordering::Relaxed);
                let step = self.step.lock().expect("its step").clone();
                BTreeMap::from([(
                    "session:a".to_owned(),
                    Record::from(record("session", "a", 1, json!({"step": step}))),
                )])
            })
        }

        fn told(&self, _: &str) -> Option<watch::Receiver<u64>> {
            None
        }

        fn health(&self) -> Map<String, Value> {
            Map::new()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn records_are_built_once_for_each_change_however_many_watch() {
        let holds = Counting::default();
        let built = Built::default();
        let first = built.records(&holds, "session", 1).expect("held");
        let again = built.records(&holds, "session", 1).expect("held");
        assert_eq!(
            holds.builds(),
            1,
            "a second watch at the same change shares the build"
        );
        assert!(
            Arc::ptr_eq(&first.build, &again.build),
            "the very same records"
        );
        assert!(!again.behind, "which hold the change asked at");
        holds.step("next");
        tokio::time::advance(BETWEEN_SENDS).await;
        let next = built.records(&holds, "session", 2).expect("held");
        assert_eq!(holds.builds(), 2, "the next change builds them again");
        assert_eq!(
            next.build.records()["session:a"].value()["step"],
            json!("next"),
            "as they stand after it"
        );
        assert!(
            !Arc::ptr_eq(
                &next.build.records()["session:a"],
                &first.build.records()["session:a"]
            ),
            "a record that changed is a new one"
        );
        tokio::time::advance(BETWEEN_SENDS).await;
        let after = built.records(&holds, "session", 3).expect("held");
        assert!(
            Arc::ptr_eq(
                &after.build.records()["session:a"],
                &next.build.records()["session:a"]
            ),
            "a record a change left as it was is the very one every watch holds"
        );
        assert!(
            built.records(&holds, "machine", 2).is_none(),
            "a kind it doesn't hold has none"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_build_is_shared_within_its_window_saying_it_may_be_behind() {
        let holds = Counting::default();
        let built = Built::default();
        built.records(&holds, "session", 1).expect("held");
        holds.step("wrote");
        tokio::time::advance(BETWEEN_SENDS / 2).await;
        let shared = built.records(&holds, "session", 2).expect("held");
        assert_eq!(
            holds.builds(),
            1,
            "a watch told of a change within the window shares the build before it"
        );
        assert_eq!(
            shared.build.records()["session:a"].value()["step"],
            json!(""),
            "which lacks the change"
        );
        assert!(shared.behind, "and says it may be behind");
        tokio::time::advance(BETWEEN_SENDS / 2).await;
        let newer = built.records(&holds, "session", 2).expect("held");
        assert_eq!(holds.builds(), 2, "once it passes, the change is built");
        assert_eq!(
            (
                newer.build.records()["session:a"].value()["step"].clone(),
                newer.behind
            ),
            (json!("wrote"), false),
            "and holds the change, owing nothing more"
        );
    }
}
