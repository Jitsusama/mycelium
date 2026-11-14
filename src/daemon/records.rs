//! A record as the interface carries it, what changed between two sets of
//! them, and each kind's records built once for every watch of it.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Map, Value};

use super::Holds;

/// A kind's records as last built, by kind, with the count of its `told`
/// they were built at.
type Builds = HashMap<String, (u64, Arc<BTreeMap<String, Value>>)>;

/// Each kind's records as built for the latest change its `told` says,
/// shared by every watch of the kind. A node with a hundred sessions and
/// a dozen watches of them built each session's record a dozen times for
/// every change (12 November); built once, each watch only compares.
#[derive(Default)]
pub struct Built(Mutex<Builds>);

impl Built {
    /// `kind`'s records as `holds` holds them once its `told` has counted
    /// to `told`, built only by the first to ask at that count. A daemon
    /// changes what it holds before it counts, so records built at a count
    /// hold at least that change.
    pub fn records(
        &self,
        holds: &impl Holds,
        kind: &str,
        told: u64,
    ) -> Option<Arc<BTreeMap<String, Value>>> {
        // Held while building, so watches woken by one change wait for the
        // first's build rather than each making their own.
        let mut builds = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, records)) = builds.get(kind)
            && *at == told
        {
            return Some(Arc::clone(records));
        }
        let records = Arc::new(holds.records(kind)?);
        builds.insert(kind.to_owned(), (told, Arc::clone(&records)));
        Some(records)
    }
}

/// The interface's own fields of every record, which a record's fields
/// of the same names give way to.
const INTERFACE_FIELDS: [&str; 3] = ["key", "kind", "heard"];

/// A record of `kind`, keyed by `id` within it, last heard at `heard`
/// (milliseconds since the epoch), carrying `fields` as they are, beside
/// the interface's own. A value that isn't an object is carried whole as
/// `value`. A field its source named as the interface names one, such as
/// a job's own `kind`, is kept under `source` rather than lost, beside the
/// record's own `source` where it has one.
#[must_use]
pub fn record(kind: &str, id: &str, heard: i64, fields: Value) -> Value {
    let mut record = match fields {
        Value::Object(fields) => fields,
        other => Map::from_iter([("value".to_owned(), other)]),
    };
    let mut sourced: Map<String, Value> = INTERFACE_FIELDS
        .iter()
        .filter_map(|name| record.remove(*name).map(|own| ((*name).to_owned(), own)))
        .collect();
    if !sourced.is_empty() {
        if let Some(own) = record.remove("source") {
            sourced.insert("source".to_owned(), own);
        }
        record.insert("source".to_owned(), Value::Object(sourced));
    }
    record.insert("key".to_owned(), Value::String(key(kind, id)));
    record.insert("kind".to_owned(), Value::String(kind.to_owned()));
    record.insert("heard".to_owned(), Value::from(heard));
    Value::Object(record)
}

/// The key of `id` of `kind`.
#[must_use]
pub fn key(kind: &str, id: &str) -> String {
    format!("{kind}:{id}")
}

/// What changed from `before` to `after`, each a set of records by key:
/// each record new or changed, with its key and only the fields that
/// differ (a new one whole), and the keys gone. Nothing, when nothing
/// changed.
#[must_use]
pub fn changes(
    before: &BTreeMap<String, Value>,
    after: &BTreeMap<String, Value>,
) -> (Vec<Value>, Vec<String>) {
    let changed = after
        .iter()
        .filter_map(|(key, now)| match before.get(key) {
            None => Some(now.clone()),
            Some(was) if was == now => None,
            Some(was) => Some(differs(key, was, now)),
        })
        .collect();
    let gone = before
        .keys()
        .filter(|key| !after.contains_key(*key))
        .cloned()
        .collect();
    (changed, gone)
}

/// `now`'s fields that differ from `was`'s, with its key, and each field
/// `was` had that `now` lacks, as null.
fn differs(key: &str, was: &Value, now: &Value) -> Value {
    let (Some(was), Some(now)) = (was.as_object(), now.as_object()) else {
        return now.clone();
    };
    let mut changed: Map<String, Value> = now
        .iter()
        .filter(|(field, value)| was.get(*field) != Some(*value))
        .map(|(field, value)| (field.clone(), value.clone()))
        .collect();
    for field in was.keys().filter(|field| !now.contains_key(*field)) {
        changed.insert(field.clone(), Value::Null);
    }
    changed.insert("key".to_owned(), Value::String(key.to_owned()));
    Value::Object(changed)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::sync::watch;

    use super::*;

    #[test]
    fn a_record_carries_its_fields_beside_its_key_kind_and_when_heard() {
        assert_eq!(
            record("session", "abc", 7, json!({"title": "work"})),
            json!({"key": "session:abc", "kind": "session", "heard": 7, "title": "work"}),
            "a record's fields sit beside the interface's"
        );
    }

    #[test]
    fn a_field_named_as_the_interface_names_one_is_kept_under_its_source() {
        assert_eq!(
            record(
                "job",
                "j1",
                7,
                json!({"kind": "vm-test", "heard": 3, "what": "looks"})
            ),
            json!({
                "key": "job:j1", "kind": "job", "heard": 7, "what": "looks",
                "source": {"kind": "vm-test", "heard": 3}
            }),
            "a job's own kind isn't lost under the interface's"
        );
    }

    #[test]
    fn a_records_own_source_is_kept_beside_what_is_moved_under_it() {
        assert_eq!(
            record(
                "waiting",
                "w1",
                7,
                json!({"kind": "try", "source": "sessions"})
            ),
            json!({
                "key": "waiting:w1", "kind": "waiting", "heard": 7,
                "source": {"kind": "try", "source": "sessions"}
            }),
            "a record's own source isn't lost to the fields moved under it"
        );
    }

    #[test]
    fn a_change_says_only_what_differs_and_what_is_gone() {
        let records = |all: &[Value]| -> BTreeMap<String, Value> {
            all.iter()
                .map(|one| (one["key"].as_str().expect("a key").to_owned(), one.clone()))
                .collect()
        };
        let before = records(&[
            json!({"key": "session:a", "state": "idle", "title": "one", "step": "x"}),
            json!({"key": "session:b", "state": "busy"}),
        ]);
        let after = records(&[
            json!({"key": "session:a", "state": "busy", "title": "one"}),
            json!({"key": "session:c", "state": "idle"}),
        ]);
        assert_eq!(
            changes(&before, &after),
            (
                vec![
                    json!({"key": "session:a", "state": "busy", "step": null}),
                    json!({"key": "session:c", "state": "idle"}),
                ],
                vec!["session:b".to_owned()],
            ),
            "a changed record names its key and what moved, a new one comes whole"
        );
        assert_eq!(
            changes(&after, &after),
            (Vec::new(), Vec::new()),
            "nothing is said while nothing changes"
        );
    }

    /// A daemon holding one session, which counts how often its records
    /// are built.
    #[derive(Default)]
    struct Counting {
        builds: std::sync::atomic::AtomicU32,
        step: Mutex<String>,
    }

    impl Holds for Counting {
        fn kinds(&self) -> BTreeMap<String, Vec<&'static str>> {
            BTreeMap::from([("session".to_owned(), vec!["step"])])
        }

        fn records(&self, kind: &str) -> Option<BTreeMap<String, Value>> {
            (kind == "session").then(|| {
                self.builds
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let step = self.step.lock().expect("its step").clone();
                BTreeMap::from([(
                    "session:a".to_owned(),
                    record("session", "a", 1, json!({"step": step})),
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

    #[test]
    fn records_are_built_once_for_each_change_however_many_watch() {
        let holds = Counting::default();
        let built = Built::default();
        let builds = || holds.builds.load(std::sync::atomic::Ordering::Relaxed);
        let first = built.records(&holds, "session", 1).expect("held");
        let again = built.records(&holds, "session", 1).expect("held");
        assert_eq!(
            builds(),
            1,
            "a second watch at the same change shares the build"
        );
        assert!(Arc::ptr_eq(&first, &again), "the very same records");
        *holds.step.lock().expect("its step") = "next".to_owned();
        let next = built.records(&holds, "session", 2).expect("held");
        assert_eq!(builds(), 2, "the next change builds them again");
        assert_eq!(
            next["session:a"]["step"],
            json!("next"),
            "as they stand after it"
        );
        assert_eq!(
            built.records(&holds, "machine", 2),
            None,
            "a kind it doesn't hold has none"
        );
    }
}
