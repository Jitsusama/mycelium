//! A record as the interface carries it, a kind's records as one build
//! holds them, and what changed between two sets of them.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Serialize, Serializer};
use serde_json::value::RawValue;
use serde_json::{Map, Value};

use super::split::{self, Field, Piece};

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

    /// The records, shared, for the build after this one to keep each
    /// that didn't change as the very one this holds.
    pub(super) fn shared_records(&self) -> Arc<Shared> {
        Arc::clone(&self.records)
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
        was.and_then(|was| split::fields(was.as_str())),
        split::fields(now.as_str()),
    ) else {
        return Change::whole(key, now);
    };
    let mut changed = moved(&was, &now_fields, |field, before, after| {
        if deep.iter().any(|named| named == field)
            && let (Some(before), Some(after)) = (split::fields(before), split::fields(after))
        {
            let inside = moved(&before, &after, |_, _, inside| Cow::Borrowed(inside));
            return Cow::Owned(split::object(&inside));
        }
        Cow::Borrowed(after)
    });
    let keyed = Piece {
        name: "key",
        written: "key",
        value: Cow::Owned(quoted(key)),
    };
    match changed.binary_search_by(|piece| piece.name.cmp("key")) {
        Ok(at) => changed[at] = keyed,
        Err(at) => changed.insert(at, keyed),
    }
    Change {
        key: key.to_owned(),
        carries: changed.iter().map(|piece| piece.name.to_owned()).collect(),
        bytes: RawValue::from_string(split::object(&changed))
            .unwrap_or_else(|_| RawValue::NULL.to_owned()),
    }
}

impl Change {
    /// `now` whole, as a record new is sent, carrying every field it has:
    /// read from its split, or from a parse where it can't be split, so a
    /// watch held back to some fields isn't held back from it.
    pub(super) fn whole(key: &str, now: &Record) -> Self {
        let split = split::fields(now.as_str()).map(|fields| {
            let names = fields.into_iter().map(|field| field.name.into_owned());
            names.collect()
        });
        let parsed = || {
            let fields: BTreeMap<String, &RawValue> = serde_json::from_str(now.as_str()).ok()?;
            Some(fields.into_keys().collect())
        };
        Self {
            key: key.to_owned(),
            carries: split.or_else(parsed).unwrap_or_default(),
            bytes: now.bytes.clone(),
        }
    }
}

/// The fields of `now` that differ from `was`'s, each as `said` says it
/// from its name and both values, and each `was` had that `now` lacks, as
/// null, in the order of their names. Both hold their fields in that
/// order, so they are walked side by side.
fn moved<'f, 'v>(
    was: &'f [Field<'v>],
    now: &'f [Field<'v>],
    said: impl Fn(&str, &'v str, &'v str) -> Cow<'v, str>,
) -> Vec<Piece<'f, 'v>> {
    let mut changed = Vec::new();
    let (mut before, mut after) = (0, 0);
    loop {
        let order = match (was.get(before), now.get(after)) {
            (None, None) => return changed,
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (Some(then), Some(new)) => then.name.cmp(&new.name),
        };
        match order {
            Ordering::Less => {
                changed.push(Piece::of(&was[before], Cow::Borrowed("null")));
                before += 1;
            }
            Ordering::Greater => {
                let new = &now[after];
                changed.push(Piece::of(new, Cow::Borrowed(new.value)));
                after += 1;
            }
            Ordering::Equal => {
                let (then, new) = (&was[before], &now[after]);
                if then.value != new.value {
                    changed.push(Piece::of(new, said(&new.name, then.value, new.value)));
                }
                before += 1;
                after += 1;
            }
        }
    }
}

/// `text` as a JSON string.
fn quoted(text: &str) -> String {
    Value::from(text).to_string()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

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

    #[test]
    fn a_field_named_with_escapes_is_asked_deep_and_compared_by_its_name() {
        let one = |at: u32| -> Shared {
            let record = json!({
                "key": "session:\"a\"",
                "say \"hi\"": {"tab\there": at, "same\\": "\u{1F980}", "z": 1},
                "say": "kept",
            });
            Shared::from([("session:\"a\"".to_owned(), Arc::new(Record::from(record)))])
        };
        assert_eq!(
            said(changes(&one(1), &one(2), &["say \"hi\"".to_owned()])),
            (
                vec![json!({"key": "session:\"a\"", "say \"hi\"": {"tab\there": 2}})],
                Vec::new()
            ),
            "a name is matched as it reads, not as it is escaped"
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

    #[test]
    fn a_record_the_split_cant_read_is_sent_whole_carrying_every_field() {
        // Held bytes are always serde_json's, keys sorted; these are not,
        // as they would be were serde_json's preserve_order turned on.
        let held = |json: &str| Record {
            bytes: RawValue::from_string(json.to_owned()).expect("JSON"),
            heard: 0,
        };
        let was = held(r#"{"key":"session:a","step":"read","presence":{"b":1,"a":1}}"#);
        let now = held(r#"{"key":"session:a","step":"wrote","presence":{"b":2,"a":1}}"#);
        let change = differs("session:a", Some(&was), &now, &["presence".to_owned()]);
        assert_eq!(change.value(), now.value(), "sent whole, not read wrong");
        for field in ["key", "presence", "step"] {
            assert!(
                change.carries(field),
                "it carries {field}, so a watch held back to it is sent the change"
            );
        }
    }
}
