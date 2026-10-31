//! The daemon interface, version 1, which the launcher and pi speak to
//! every daemon on a machine alike: one JSON object a line, several asks
//! at once on one connection, each answered with the `id` it was asked
//! with. A record is keyed `kind:<id>`, says its `kind` and when it was
//! last `heard`, and carries its fields as the node holds them. A watch
//! sends the whole records first, then only what changed, by key, and
//! nothing while nothing changes.
//!
//! A connection that opens with `hello` speaks this; any other speaks the
//! node's own protocol in `wire`, so its clients keep working unchanged.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt, Lines};
use tokio::sync::{mpsc, watch};

/// How many answers wait for a slow reader before a watch waits too; a
/// waiting watch then sends the latest, never what it missed.
const WAITING_ANSWERS: usize = 16;

/// What a daemon holds, by kind, which the interface serves.
pub trait Holds: Send + Sync + 'static {
    /// Each kind it holds, with the fields of its records that are live.
    fn kinds(&self) -> BTreeMap<String, Vec<&'static str>>;
    /// Every record of `kind` it holds now, by key, or none for a kind it
    /// doesn't hold.
    fn records(&self, kind: &str) -> Option<BTreeMap<String, Value>>;
    /// What changes each time `kind`'s records may have, or none for a
    /// kind it doesn't hold.
    fn told(&self, kind: &str) -> Option<watch::Receiver<u64>>;
    /// How it and each of its sources stand, as the interface's `health`
    /// answers beside its `id`.
    fn health(&self) -> Map<String, Value>;
}

/// The version of the interface this node speaks.
pub const VERSION: u64 = 1;

/// The shortest time between two sends of one watch, so a record changes
/// on the wire at most four times a second, and a reader slower than that
/// is given the latest rather than a backlog.
pub const BETWEEN_SENDS: std::time::Duration = std::time::Duration::from_millis(250);

/// What a client asks, by the `id` it is answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    /// The client's own id for it, echoed in every answer to it.
    pub id: Value,
    /// What it asks.
    pub what: What,
}

/// The asks the interface knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum What {
    /// Which version and kinds this daemon speaks.
    Hello,
    /// The records of these keys, of those held.
    Get(Vec<String>),
    /// Every record of a kind, at most `limit` of them.
    List {
        /// The kind.
        kind: String,
        /// At most how many, or all.
        limit: Option<usize>,
    },
    /// The records of a kind, or of these keys of it, then each change.
    Watch {
        /// The kind.
        kind: String,
        /// Only these keys, or all of the kind.
        keys: Option<Vec<String>>,
    },
    /// The end of the watch asked with this id.
    Unwatch,
    /// How the daemon and each of its sources stand.
    Health,
}

/// Why an ask can't be answered, by the interface's codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// An ask this daemon doesn't know.
    UnknownAsk(String),
    /// A kind this daemon doesn't hold.
    UnknownKind(String),
    /// A line that isn't an ask at all.
    NotAnAsk(String),
}

impl Refusal {
    /// The interface's code for it.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownAsk(_) => "unknown-ask",
            Self::UnknownKind(_) => "unknown-kind",
            Self::NotAnAsk(_) => "not-an-ask",
        }
    }

    /// What it says, for a person.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::UnknownAsk(ask) => format!("this node doesn't answer {ask:?}"),
            Self::UnknownKind(kind) => format!("this node holds no {kind:?}"),
            Self::NotAnAsk(why) => format!("not an ask: {why}"),
        }
    }
}

/// The ask a line holds, or why it isn't one, with the `id` it gave if
/// it gave one, so even a refusal is answered to it.
///
/// # Errors
///
/// A line that isn't a JSON object with an `ask`, or asks what this
/// node doesn't know.
pub fn read(line: &str) -> Result<Ask, (Value, Refusal)> {
    let value: Value = serde_json::from_str(line)
        .map_err(|error| (Value::Null, Refusal::NotAnAsk(error.to_string())))?;
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let refused = |refusal| (id.clone(), refusal);
    let Some(ask) = value.get("ask").and_then(Value::as_str) else {
        return Err(refused(Refusal::NotAnAsk("it names no `ask`".to_owned())));
    };
    let kind = || {
        value
            .get("kind")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| refused(Refusal::NotAnAsk(format!("{ask:?} names no `kind`"))))
    };
    let keys = |name| {
        value.get(name).and_then(Value::as_array).map(|keys| {
            keys.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
    };
    let what = match ask {
        "hello" => What::Hello,
        "get" => What::Get(keys("keys").unwrap_or_default()),
        "list" => What::List {
            kind: kind()?,
            limit: value
                .get("limit")
                .and_then(Value::as_u64)
                .and_then(|limit| usize::try_from(limit).ok()),
        },
        "watch" => What::Watch {
            kind: kind()?,
            keys: keys("keys"),
        },
        "unwatch" => What::Unwatch,
        "health" => What::Health,
        other => return Err(refused(Refusal::UnknownAsk(other.to_owned()))),
    };
    Ok(Ask { id, what })
}

/// The answer to `id` that it was refused, and why.
#[must_use]
pub fn refused(id: &Value, refusal: &Refusal) -> Value {
    json!({"id": id, "error": {"code": refusal.code(), "message": refusal.message()}})
}

/// A record of `kind`, keyed by `id` within it, last heard at `heard`
/// (milliseconds since the epoch), carrying `fields` as they are, beside
/// the interface's own. A value that isn't an object is carried whole as
/// `value`.
#[must_use]
pub fn record(kind: &str, id: &str, heard: i64, fields: Value) -> Value {
    let mut record = match fields {
        Value::Object(fields) => fields,
        other => Map::from_iter([("value".to_owned(), other)]),
    };
    record.insert("key".to_owned(), Value::String(key(kind, id)));
    record.insert("kind".to_owned(), Value::String(kind.to_owned()));
    record.insert("heard".to_owned(), Value::from(heard));
    Value::Object(record)
}

/// Answers a client that opened with `hello`, its first ask, and every
/// ask after it on `lines`, writing to `writing`, until it hangs up.
pub async fn speak<H, R, W>(holds: Arc<H>, first: Ask, mut lines: Lines<R>, mut writing: W)
where
    H: Holds,
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (answering, mut answers) = mpsc::channel::<Value>(WAITING_ANSWERS);
    let mut watches: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    let mut asked = Some(Ok(first));
    loop {
        let now = match asked.take() {
            Some(Ok(ask)) => answer(&holds, ask, &answering, &mut watches),
            Some(Err((id, refusal))) => Some(refused(&id, &refusal)),
            None => None,
        };
        if let Some(now) = now
            && say(&mut writing, &now).await.is_err()
        {
            break;
        }
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => asked = Some(read(&line)),
                _ => break,
            },
            Some(pushed) = answers.recv() => if say(&mut writing, &pushed).await.is_err() {
                break;
            },
        }
    }
    for (_, watch) in watches {
        watch.abort();
    }
}

/// Writes `answer` as a line.
async fn say(writing: &mut (impl AsyncWrite + Unpin), answer: &Value) -> std::io::Result<()> {
    let mut line = answer.to_string().into_bytes();
    line.push(b'\n');
    writing.write_all(&line).await
}

/// The answer to `ask`, now, or none for a watch, which answers through
/// `answering` for as long as it lasts, or an unwatch, which answers not
/// at all.
fn answer<H: Holds>(
    holds: &Arc<H>,
    ask: Ask,
    answering: &mpsc::Sender<Value>,
    watches: &mut HashMap<String, tokio::task::JoinHandle<()>>,
) -> Option<Value> {
    let begun = std::time::Instant::now();
    let Ask { id, what } = ask;
    let answered = match what {
        What::Hello => Ok(hello(holds.as_ref(), &id)),
        What::Health => {
            let mut health = holds.health();
            health.insert("id".to_owned(), id.clone());
            Ok(Value::Object(health))
        }
        What::Get(keys) => Ok(json!({"id": id, "records": got(holds.as_ref(), &keys)})),
        What::List { kind, limit } => holds
            .records(&kind)
            .map(|records| {
                let records: Vec<Value> = records
                    .into_values()
                    .take(limit.unwrap_or(usize::MAX))
                    .collect();
                json!({"id": id, "records": records})
            })
            .ok_or(Refusal::UnknownKind(kind)),
        What::Watch { kind, keys } => match holds.told(&kind) {
            Some(told) => {
                let watch = tokio::spawn(keep_watching(
                    Arc::clone(holds),
                    id.clone(),
                    kind,
                    keys,
                    told,
                    answering.clone(),
                ));
                if let Some(earlier) = watches.insert(id.to_string(), watch) {
                    earlier.abort();
                }
                return None;
            }
            None => Err(Refusal::UnknownKind(kind)),
        },
        What::Unwatch => {
            if let Some(watch) = watches.remove(&id.to_string()) {
                watch.abort();
            }
            return None;
        }
    };
    let mut answer = answered.unwrap_or_else(|refusal| refused(&id, &refusal));
    if let Some(fields) = answer.as_object_mut() {
        fields.insert("took".to_owned(), took(begun));
    }
    Some(answer)
}

/// The answer to `hello`: this node's version, and the kinds it holds with
/// each kind's live fields.
fn hello(holds: &impl Holds, id: &Value) -> Value {
    let kinds: Map<String, Value> = holds
        .kinds()
        .into_iter()
        .map(|(kind, live)| (kind, json!({"live": live})))
        .collect();
    json!({"id": id, "version": VERSION, "domain": "node", "state": "answering", "kinds": kinds})
}

/// The records of `keys` held, in the order asked.
fn got(holds: &impl Holds, keys: &[String]) -> Vec<Value> {
    let mut by_kind: BTreeMap<&str, BTreeMap<String, Value>> = BTreeMap::new();
    keys.iter()
        .filter_map(|key| {
            let (kind, _) = key.split_once(':')?;
            if !by_kind.contains_key(kind) {
                by_kind.insert(kind, holds.records(kind)?);
            }
            by_kind.get(kind)?.get(key).cloned()
        })
        .collect()
}

/// Sends `kind`'s records, or those of `keys`, whole, then what changes
/// each time `told` says they may have, no sooner than [`BETWEEN_SENDS`]
/// after the last, and nothing while nothing changes.
async fn keep_watching<H: Holds>(
    holds: Arc<H>,
    id: Value,
    kind: String,
    keys: Option<Vec<String>>,
    mut told: watch::Receiver<u64>,
    answering: mpsc::Sender<Value>,
) {
    let held = |holds: &H| {
        let mut records = holds.records(&kind).unwrap_or_default();
        if let Some(keys) = &keys {
            records.retain(|key, _| keys.contains(key));
        }
        records
    };
    told.mark_unchanged();
    let mut sent = held(&holds);
    let whole: Vec<Value> = sent.values().cloned().collect();
    if answering
        .send(json!({"id": id, "records": whole}))
        .await
        .is_err()
    {
        return;
    }
    while told.changed().await.is_ok() {
        tokio::time::sleep(BETWEEN_SENDS).await;
        told.mark_unchanged();
        let now = held(&holds);
        let (changed, gone) = changes(&sent, &now);
        sent = now;
        if changed.is_empty() && gone.is_empty() {
            continue;
        }
        if answering
            .send(json!({"id": id, "changed": changed, "gone": gone}))
            .await
            .is_err()
        {
            return;
        }
    }
}

/// How long since `begun`, in milliseconds, as an answer's `took`.
fn took(begun: std::time::Instant) -> Value {
    Value::from(begun.elapsed().as_secs_f64() * 1000.0)
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
    use super::*;

    #[test]
    fn an_ask_is_read_with_its_id_and_a_refusal_is_answered_to_it() {
        assert_eq!(
            read(r#"{"id":4,"ask":"watch","kind":"session","keys":["session:a"]}"#),
            Ok(Ask {
                id: json!(4),
                what: What::Watch {
                    kind: "session".to_owned(),
                    keys: Some(vec!["session:a".to_owned()]),
                },
            }),
            "a watch names its kind and keys"
        );
        let Err((id, refusal)) = read(r#"{"id":"x","ask":"juggle"}"#) else {
            panic!("an unknown ask is refused");
        };
        assert_eq!(
            refused(&id, &refusal),
            json!({"id": "x", "error": {
                "code": "unknown-ask",
                "message": "this node doesn't answer \"juggle\""
            }}),
            "with the interface's code, to the id it gave"
        );
        assert_eq!(
            read(r#"{"id":5,"ask":"list"}"#).map_err(|(_, refusal)| refusal.code()),
            Err("not-an-ask"),
            "a list names its kind"
        );
    }

    #[test]
    fn a_record_carries_its_fields_beside_its_key_kind_and_when_heard() {
        assert_eq!(
            record("session", "abc", 7, json!({"title": "work", "kind": "pi"})),
            json!({"key": "session:abc", "kind": "session", "heard": 7, "title": "work"}),
            "the interface's fields are its own"
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
}
