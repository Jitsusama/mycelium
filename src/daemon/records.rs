//! A record as the interface carries it, what changed between two sets of
//! them, and each kind's records built once for every watch of it.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Serialize, Serializer};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use super::Holds;

/// A record held as the bytes it is sent as, and when it was heard. A
/// parsed record keeps a map for every object in it, five times its bytes
/// (16 November: ten thousand cells' records held 56.5 MB parsed), and is
/// mostly only compared and sent, which its bytes do as well. It is parsed
/// only where its fields are read: a change of it, or a list asking for
/// some of them.
///
/// Two records are the same record when their bytes are: a value's object
/// keeps its keys sorted, however it was built, so equal values always
/// serialize to equal bytes.
#[derive(Clone, Debug)]
pub struct Record {
    /// The record, serialized.
    bytes: Box<RawValue>,
    /// When it was heard, which a kind listed newest first sorts by.
    heard: i64,
}

impl Record {
    /// The record as its bytes.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.bytes.get()
    }

    /// The record parsed, to read its fields.
    #[must_use]
    pub fn value(&self) -> Value {
        serde_json::from_str(self.as_str()).unwrap_or(Value::Null)
    }

    /// When it was heard, or the epoch where it says not.
    #[must_use]
    pub fn heard(&self) -> i64 {
        self.heard
    }
}

impl From<Value> for Record {
    /// `record` serialized. A value always serializes, its keys being
    /// strings; were one ever not to, it is held as null.
    fn from(record: Value) -> Self {
        let heard = record.get("heard").and_then(Value::as_i64).unwrap_or(0);
        let bytes =
            serde_json::value::to_raw_value(&record).unwrap_or_else(|_| RawValue::NULL.to_owned());
        Self { bytes, heard }
    }
}

impl PartialEq for Record {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for Record {}

impl Serialize for Record {
    /// The record's bytes, written as they are.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.bytes.serialize(serializer)
    }
}

/// A kind's records by key, each shared, so one unchanged since the last
/// build is the very record every watch of it already holds.
pub type Shared = BTreeMap<String, Arc<Record>>;

/// What a watch is sent of a record new or changed: its key, the names of
/// the fields the change carries, and the change itself as the bytes it
/// is sent as, written from the record's own bytes, never parsed. Shared,
/// every watch sending it sends these very bytes.
#[derive(Debug)]
pub struct Change {
    /// The record's key.
    key: String,
    /// The fields the change carries, in order.
    carries: Vec<String>,
    /// The change, serialized.
    bytes: Box<RawValue>,
}

impl Change {
    /// The key of the record it changes.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Whether it carries `field`, as a record new carries all of its own.
    #[must_use]
    pub fn carries(&self, field: &str) -> bool {
        self.carries
            .binary_search_by(|carried| carried.as_str().cmp(field))
            .is_ok()
    }

    /// The change parsed, to read it.
    #[must_use]
    pub fn value(&self) -> Value {
        serde_json::from_str(self.bytes.get()).unwrap_or(Value::Null)
    }
}

impl Serialize for Change {
    /// The change's bytes, written as they are.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.bytes.serialize(serializer)
    }
}

/// What changed of each moved record, by its key.
type Moved = HashMap<String, Arc<Change>>;

/// A kind's records as built at one change, beside those of the build
/// before it, so each record moved between them is parsed and its change
/// worked out once, for every watch that sent the build before (KP0L6A,
/// 16 November): fifty watches of ten thousand sessions would otherwise
/// parse each moved session fifty times.
#[derive(Default)]
pub struct Build {
    /// The records.
    records: Arc<Shared>,
    /// The records of the build before, or none for a first build.
    before: Option<Arc<Shared>>,
    /// What changed of each record moved since `before`, by its key, for
    /// each set of fields asked deep, worked out the first time a watch
    /// asks.
    moved: Mutex<HashMap<Vec<String>, Arc<Moved>>>,
}

impl Build {
    /// `records`, built after `before`, or first where it is none.
    #[must_use]
    pub fn after(records: Shared, before: Option<Arc<Shared>>) -> Self {
        Self {
            records: Arc::new(records),
            before,
            moved: Mutex::default(),
        }
    }

    /// The records.
    #[must_use]
    pub fn records(&self) -> &Shared {
        &self.records
    }

    /// What changed from `sent` to this build's records, those `keys`
    /// names or all, asked `deep`, as [`changes`] says it. A record `sent`
    /// holds as the build before held it takes the change worked out once
    /// for every watch; only one sent from an older build, as a watch
    /// holding back what `when` doesn't name has, is parsed for this watch
    /// alone.
    #[must_use]
    pub fn changes(
        &self,
        sent: &Shared,
        keys: Option<&[String]>,
        deep: &[String],
    ) -> (Vec<Arc<Change>>, Vec<String>) {
        let after = self
            .records
            .iter()
            .filter(|(key, _)| keys.is_none_or(|keys| keys.contains(*key)));
        let Some(before) = &self.before else {
            return found(sent, after, &self.records, |key, was, now| {
                Arc::new(differs(key, was, now, deep))
            });
        };
        let moved = self.moved(before, deep);
        found(sent, after, &self.records, |key, was, now| {
            let as_before = match (was, before.get(key)) {
                (None, None) => true,
                (Some(was), Some(then)) => std::ptr::eq(was, Arc::as_ptr(then)) || *was == **then,
                _ => false,
            };
            match moved.get(key) {
                Some(change) if as_before => Arc::clone(change),
                _ => Arc::new(differs(key, was, now, deep)),
            }
        })
    }

    /// What changed of each record moved from `before` to this build's,
    /// asked `deep`, worked out by the first watch to ask while the rest
    /// wait for it.
    fn moved(&self, before: &Shared, deep: &[String]) -> Arc<Moved> {
        let mut moved = self.moved.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(said) = moved.get(deep) {
            return Arc::clone(said);
        }
        let said: Moved = self
            .records
            .iter()
            .filter_map(|(key, now)| match before.get(key) {
                Some(was) if Arc::ptr_eq(was, now) || was == now => None,
                was => Some((
                    key.clone(),
                    Arc::new(differs(key, was.map(AsRef::as_ref), now, deep)),
                )),
            })
            .collect();
        let said = Arc::new(said);
        moved.insert(deep.to_vec(), Arc::clone(&said));
        said
    }
}

/// A kind's records as last built, by kind, with the count of its `told`
/// they were built at.
type Builds = HashMap<String, (u64, Arc<Build>)>;

/// Each kind's records as built for the latest change its `told` says,
/// shared by every watch of the kind. A node with a hundred sessions and
/// a dozen watches of them built each session's record a dozen times for
/// every change (12 November); built once, each watch only compares. A
/// record that didn't change is kept as it was, so each watch finds it
/// unchanged by its address alone, and every watch holds one copy of it
/// rather than its own (14 November: 93 watches each held a copy of
/// every record, and compared every field of each on every change).
#[derive(Default)]
pub struct Built(Mutex<Builds>);

impl Built {
    /// `kind`'s records as `holds` holds them once its `told` has counted
    /// to `told`, built only by the first to ask at that count. A daemon
    /// changes what it holds before it counts, so records built at a count
    /// hold at least that change.
    pub fn records(&self, holds: &impl Holds, kind: &str, told: u64) -> Option<Arc<Build>> {
        // Held while building, so watches woken by one change wait for the
        // first's build rather than each making their own.
        let mut builds = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, build)) = builds.get(kind)
            && *at == told
        {
            return Some(Arc::clone(build));
        }
        let fresh = holds.records(kind)?;
        let last = builds
            .get(kind)
            .map(|(_, build)| Arc::clone(&build.records));
        let records = shared(last.as_deref(), fresh);
        let build = Arc::new(Build::after(records, last));
        builds.insert(kind.to_owned(), (told, Arc::clone(&build)));
        Some(build)
    }
}

/// `fresh` shared, each record equal to the one `last` holds under its key
/// kept as that one, the rest new.
#[must_use]
pub fn shared(last: Option<&Shared>, fresh: BTreeMap<String, Record>) -> Shared {
    fresh
        .into_iter()
        .map(|(key, now)| {
            let record = match last.and_then(|last| last.get(&key)) {
                Some(was) if **was == now => Arc::clone(was),
                _ => Arc::new(now),
            };
            (key, record)
        })
        .collect()
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
/// changed. A record still the very one it was is unchanged without its
/// fields being compared. A field `deep` names whose value is an object
/// both before and after says only what differs inside it, in the same
/// way, rather than all of it.
#[must_use]
pub fn changes(
    before: &Shared,
    after: &Shared,
    deep: &[String],
) -> (Vec<Arc<Change>>, Vec<String>) {
    found(before, after.iter(), after, |key, was, now| {
        Arc::new(differs(key, was, now, deep))
    })
}

/// The records of `after` new or changed since `before`, each as `said`
/// says it from its key and both records, and the keys of `before` that
/// `all` no longer holds. A record still the very one it was, or of the
/// same bytes, is unchanged without being parsed.
fn found<'a>(
    before: &Shared,
    after: impl Iterator<Item = (&'a String, &'a Arc<Record>)>,
    all: &Shared,
    mut said: impl FnMut(&str, Option<&Record>, &Record) -> Arc<Change>,
) -> (Vec<Arc<Change>>, Vec<String>) {
    let changed = after
        .filter_map(|(key, now)| match before.get(key) {
            Some(was) if Arc::ptr_eq(was, now) || was == now => None,
            was => Some(said(key, was.map(AsRef::as_ref), now)),
        })
        .collect();
    let gone = before
        .keys()
        .filter(|key| !all.contains_key(*key))
        .cloned()
        .collect();
    (changed, gone)
}

/// What a watch is sent of `now`, keyed `key`: all of it, where it is new
/// or no object, else its fields that differ from `was`'s, with its key,
/// and each field `was` had that `now` lacks, as null. A field `deep`
/// names, an object both before and after, is said the same way, one
/// level in. Each field is compared as its bytes, as a record is, and
/// the change is written from the bytes of those that moved, none of them
/// parsed: a cell's trees, most of its bytes, are only split from the
/// rest and copied.
fn differs(key: &str, was: Option<&Record>, now: &Record, deep: &[String]) -> Change {
    let (Some(was), Some(now_fields)) = (
        was.and_then(|was| fields(was.as_str())),
        fields(now.as_str()),
    ) else {
        return Change::whole(key, now);
    };
    let mut changed = moved(&was, &now_fields, |field, before, after| {
        if deep.iter().any(|named| named == field)
            && let (Some(before), Some(after)) = (fields(before.get()), fields(after.get()))
        {
            let inside = moved(&before, &after, |_, _, inside| Cow::Borrowed(inside.get()));
            return Cow::Owned(object(&inside));
        }
        Cow::Borrowed(after.get())
    });
    changed.insert("key", Cow::Owned(quoted(key)));
    Change {
        key: key.to_owned(),
        carries: changed.keys().map(|field| (*field).to_owned()).collect(),
        bytes: RawValue::from_string(object(&changed))
            .unwrap_or_else(|_| RawValue::NULL.to_owned()),
    }
}

impl Change {
    /// `now` whole, as a record new is sent, carrying every field it has.
    pub(super) fn whole(key: &str, now: &Record) -> Self {
        Self {
            key: key.to_owned(),
            carries: fields(now.as_str())
                .map(|fields| fields.into_keys().collect())
                .unwrap_or_default(),
            bytes: now.bytes.clone(),
        }
    }
}

/// An object's fields, each as its bytes, or none where `json` is no
/// object.
type Fields<'a> = BTreeMap<String, &'a RawValue>;

/// `json`'s fields as its bytes, where it is an object.
fn fields(json: &str) -> Option<Fields<'_>> {
    serde_json::from_str(json).ok()
}

/// A change's fields by name, each as the bytes it is sent as, in the
/// order a value's object keeps them.
type Pieces<'f, 'v> = BTreeMap<&'f str, Cow<'v, str>>;

/// The fields of `now` that differ from `was`'s, each as `said` says it
/// from both, and each `was` had that `now` lacks, as null.
fn moved<'f, 'v>(
    was: &'f Fields<'v>,
    now: &'f Fields<'v>,
    said: impl Fn(&str, &'v RawValue, &'v RawValue) -> Cow<'v, str>,
) -> Pieces<'f, 'v> {
    let mut changed: Pieces<'f, 'v> = now
        .iter()
        .filter_map(|(field, value)| match was.get(field) {
            Some(before) if before.get() == value.get() => None,
            Some(before) => Some((field.as_str(), said(field, before, value))),
            None => Some((field.as_str(), Cow::Borrowed(value.get()))),
        })
        .collect();
    for field in was.keys().filter(|field| !now.contains_key(*field)) {
        changed.insert(field.as_str(), Cow::Borrowed("null"));
    }
    changed
}

/// `pieces` as one object's bytes, as a value's object would write them.
fn object(pieces: &Pieces<'_, '_>) -> String {
    let mut written = String::from("{");
    for (at, (field, bytes)) in pieces.iter().enumerate() {
        if at > 0 {
            written.push(',');
        }
        written.push_str(&quoted(field));
        written.push(':');
        written.push_str(bytes);
    }
    written.push('}');
    written
}

/// `text` as a JSON string.
fn quoted(text: &str) -> String {
    Value::from(text).to_string()
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

    /// Changes read back as values, each first shown to be the very bytes
    /// its value would write.
    fn said((changed, gone): (Vec<Arc<Change>>, Vec<String>)) -> (Vec<Value>, Vec<String>) {
        let changed = changed
            .iter()
            .map(|change| {
                let value = change.value();
                assert_eq!(
                    serde_json::to_string(change).expect("a change serializes"),
                    value.to_string(),
                    "a change is written as its value would be"
                );
                value
            })
            .collect();
        (changed, gone)
    }

    #[test]
    fn a_change_says_only_what_differs_and_what_is_gone() {
        let records = |all: &[Value]| -> Shared {
            all.iter()
                .map(|one| {
                    (
                        one["key"].as_str().expect("a key").to_owned(),
                        Arc::new(Record::from(one.clone())),
                    )
                })
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
            said(changes(&before, &after, &[])),
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
            said(changes(&after, &after, &[])),
            (Vec::new(), Vec::new()),
            "nothing is said while nothing changes"
        );
        let odd = |at: u32| records(&[json!({"key": "session:\"é\"", "tab\tbed": at})]);
        assert_eq!(
            said(changes(&odd(1), &odd(2), &[])),
            (
                vec![json!({"key": "session:\"é\"", "tab\tbed": 2})],
                Vec::new()
            ),
            "a key and a field that need escaping are written escaped"
        );
    }

    #[test]
    fn a_field_asked_deep_says_only_what_moved_inside_it() {
        let one = |record: Value| -> Shared {
            Shared::from([("session:kestrel".to_owned(), Arc::new(Record::from(record)))])
        };
        let before = one(json!({
            "key": "session:kestrel", "step": "read", "standing": "running",
            "presence": {"at": 1, "cwd": "~/src/skiff", "owner": "kestrel",
                "landings": [{"what": "Hear the Tide"}]},
        }));
        let after = one(json!({
            "key": "session:kestrel", "step": "edit", "standing": 3,
            "presence": {"at": 2, "cwd": "~/src/skiff",
                "landings": [{"what": "Hear the Tide"}], "busy": true},
        }));
        let deep = ["presence".to_owned(), "standing".to_owned()];
        assert_eq!(
            said(changes(&before, &after, &deep)),
            (
                vec![json!({
                    "key": "session:kestrel", "step": "edit", "standing": 3,
                    "presence": {"at": 2, "owner": null, "busy": true},
                })],
                Vec::new(),
            ),
            "inside a field asked deep, only what moved is said, one dropped as null, \
             and a field asked deep that isn't an object both times comes whole"
        );
        assert_eq!(
            said(changes(&before, &after, &[]))
                .0
                .first()
                .map(|record| record["presence"].clone()),
            Some(after["session:kestrel"].value()["presence"].clone()),
            "a field not asked deep comes whole, as before"
        );
        let nulled = one(json!({
            "key": "session:kestrel", "step": "read", "standing": "running",
            "presence": {"at": 1, "cwd": null, "owner": "kestrel",
                "landings": [{"what": "Hear the Tide"}]},
        }));
        assert_eq!(
            said(changes(&before, &nulled, &deep)).0,
            vec![json!({"key": "session:kestrel", "presence": {"cwd": null}})],
            "a field inside one asked deep that becomes null is said as null, as one dropped \
             is, so a reader takes null for either"
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

        fn records(&self, kind: &str) -> Option<BTreeMap<String, Record>> {
            (kind == "session").then(|| {
                self.builds
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            next.records()["session:a"].value()["step"],
            json!("next"),
            "as they stand after it"
        );
        assert!(
            !Arc::ptr_eq(&next.records()["session:a"], &first.records()["session:a"]),
            "a record that changed is a new one"
        );
        let after = built.records(&holds, "session", 3).expect("held");
        assert!(
            Arc::ptr_eq(&after.records()["session:a"], &next.records()["session:a"]),
            "a record a change left as it was is the very one every watch holds"
        );
        assert!(
            built.records(&holds, "machine", 2).is_none(),
            "a kind it doesn't hold has none"
        );
    }

    #[test]
    fn records_are_the_same_exactly_when_their_bytes_are() {
        let mut built_one_way = Map::new();
        built_one_way.insert("title".to_owned(), json!("Hear the Tide"));
        built_one_way.insert("usage".to_owned(), json!({"cost": 1.25, "input": 3}));
        built_one_way.insert("at".to_owned(), json!(7));
        let mut usage = Map::new();
        usage.insert("input".to_owned(), json!(3));
        usage.insert("cost".to_owned(), json!(1.25));
        let mut built_another = Map::new();
        built_another.insert("at".to_owned(), json!(7));
        built_another.insert("usage".to_owned(), Value::Object(usage));
        built_another.insert("title".to_owned(), json!("Hear the Tide"));
        let pairs = [
            (Value::Object(built_one_way), Value::Object(built_another)),
            (json!({"cost": 0.1 + 0.2}), json!({"cost": 0.1 + 0.2})),
            (json!({"cost": 0.3}), json!({"cost": 0.1 + 0.2})),
            (json!({"cost": 1.0}), json!({"cost": 1})),
            (json!({"cost": 1e300}), json!({"cost": 1e300})),
            (json!({"at": 7}), json!({"at": 8})),
            (json!({"at": [1, 2]}), json!({"at": [2, 1]})),
            (json!({"at": null}), json!({})),
        ];
        for (one, other) in pairs {
            let same = one == other;
            assert_eq!(
                Record::from(one.clone()) == Record::from(other.clone()),
                same,
                "{one} and {other} are the same record exactly when they are the same value"
            );
            assert_eq!(
                Record::from(one.clone()).value(),
                one,
                "a record parses back to the value it was made of"
            );
        }
    }

    #[test]
    fn a_change_is_worked_out_once_for_every_watch_that_sent_the_build_before() {
        let one = |step: &str, at: u32| -> Shared {
            Shared::from([(
                "session:skiff".to_owned(),
                Arc::new(Record::from(json!({
                    "key": "session:skiff", "step": step,
                    "presence": {"at": at, "step": step},
                }))),
            )])
        };
        let older = one("read", 1);
        let before = Arc::new(one("edit", 2));
        let mut after = one("test", 3);
        after.insert(
            "session:tern".to_owned(),
            Arc::new(Record::from(json!({"key": "session:tern", "step": "read"}))),
        );
        let build = Build::after(after.clone(), Some(Arc::clone(&before)));
        let deep = ["presence".to_owned()];
        for sent in [&*before, &older, &Shared::new()] {
            for asked in [&deep[..], &[]] {
                let bytes = |(changed, gone): (Vec<Arc<Change>>, Vec<String>)| {
                    serde_json::to_string(&(changed, gone)).expect("changes serialize")
                };
                assert_eq!(
                    bytes(build.changes(sent, None, asked)),
                    bytes(changes(sent, &after, asked)),
                    "a watch is sent what it would have worked out alone, asked deep {asked:?}"
                );
            }
        }
        assert_eq!(
            said(build.changes(&before, Some(&["session:tern".to_owned()]), &deep)),
            (
                vec![json!({"key": "session:tern", "step": "read"})],
                Vec::new()
            ),
            "a watch of some keys is sent only theirs"
        );
        assert_eq!(
            build
                .moved
                .lock()
                .expect("its changes")
                .get(&deep[..])
                .map(|moved| moved.len()),
            Some(2),
            "each set asked deep works out each moved record once, the new one included"
        );
    }
}
