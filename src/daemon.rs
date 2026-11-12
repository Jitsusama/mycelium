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
use std::sync::{Arc, Mutex, PoisonError};

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
    /// Where the records its watches read are kept once built, so every
    /// watch of a kind shares one build for each change; none builds them
    /// for each watch.
    fn built(&self) -> Option<&Built> {
        None
    }
}

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

/// What a daemon does for its clients beyond reading: what the person says
/// put to one of its sessions.
pub trait Steers: Send + Sync + 'static {
    /// Puts `said` into the inbox of the session going by `address`, on
    /// whichever machine runs it, answering which machine that is, or why
    /// it can't be told.
    fn steer(
        &self,
        address: &str,
        said: &Value,
    ) -> impl Future<Output = Result<String, String>> + Send;

    /// Updates `machine` to its configuration's main, here or through the
    /// link reaching it, answering how its updater said it ended, or why
    /// it couldn't be asked.
    fn update(&self, machine: &str) -> impl Future<Output = Result<Value, String>> + Send;
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
        /// Only a change moving one of these fields, or bringing or taking
        /// a record, is sent, with all that moved since the last send; or
        /// every change.
        when: Option<Vec<String>>,
    },
    /// The end of the watch asked with this id.
    Unwatch,
    /// What the person says, put to the session going by `address`.
    Steer {
        /// The session's address.
        address: String,
        /// What is said, in the harness's own shape.
        said: Value,
    },
    /// How the daemon and each of its sources stand.
    Health,
    /// An update of `machine` to its configuration's main.
    Update {
        /// The machine's name.
        machine: String,
    },
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
    /// An ask understood, which the daemon couldn't do, and why.
    Refused(String),
}

impl Refusal {
    /// The interface's code for it.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownAsk(_) => "unknown-ask",
            Self::UnknownKind(_) => "unknown-kind",
            Self::NotAnAsk(_) => "not-an-ask",
            Self::Refused(_) => "refused",
        }
    }

    /// What it says, for a person.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::UnknownAsk(ask) => format!("this node doesn't answer {ask:?}"),
            Self::UnknownKind(kind) => format!("this node holds no {kind:?}"),
            Self::NotAnAsk(why) => format!("not an ask: {why}"),
            Self::Refused(why) => why.clone(),
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
            when: keys("when"),
        },
        "unwatch" => What::Unwatch,
        "health" => What::Health,
        "steer" => What::Steer {
            address: value
                .get("address")
                .and_then(Value::as_str)
                .filter(|address| !address.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    refused(Refusal::NotAnAsk("a steer names no `address`".to_owned()))
                })?,
            said: value
                .get("steer")
                .filter(|said| said.is_object())
                .cloned()
                .ok_or_else(|| refused(Refusal::NotAnAsk("a steer says nothing".to_owned())))?,
        },
        "update" => What::Update {
            machine: value
                .get("machine")
                .and_then(Value::as_str)
                .filter(|machine| !machine.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    refused(Refusal::NotAnAsk("an update names no `machine`".to_owned()))
                })?,
        },
        other => return Err(refused(Refusal::UnknownAsk(other.to_owned()))),
    };
    Ok(Ask { id, what })
}

/// The answer to `id` that it was refused, and why.
#[must_use]
pub fn refused(id: &Value, refusal: &Refusal) -> Value {
    json!({"id": id, "error": {"code": refusal.code(), "message": refusal.message()}})
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

/// Answers a client that opened with `hello`, its first ask, and every
/// ask after it on `lines`, writing to `writing`, until it hangs up.
pub async fn speak<H, R, W>(holds: Arc<H>, first: Ask, mut lines: Lines<R>, mut writing: W)
where
    H: Holds + Steers,
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (answering, mut answers) = mpsc::channel::<Value>(WAITING_ANSWERS);
    let mut watches: HashMap<String, Watching> = HashMap::new();
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
        watch.task.abort();
    }
}

/// A watch a client asked for, running, with what it watches, so asking
/// again under its id for the same records changes only `when` it sends.
struct Watching {
    /// The kind it watches.
    kind: String,
    /// The keys it watches, or all of the kind.
    keys: Option<Vec<String>>,
    /// The fields whose moving it sends for, told to it as they change.
    when: watch::Sender<Option<Vec<String>>>,
    /// What sends it.
    task: tokio::task::JoinHandle<()>,
}

/// Writes `answer` as a line.
async fn say(writing: &mut (impl AsyncWrite + Unpin), answer: &Value) -> std::io::Result<()> {
    let mut line = answer.to_string().into_bytes();
    line.push(b'\n');
    writing.write_all(&line).await
}

/// The answer to `ask`, now, or none for a watch, which answers through
/// `answering` for as long as it lasts, a steer, which answers through it
/// once its session's machine has it, or an unwatch, which answers not at
/// all.
fn answer<H: Holds + Steers>(
    holds: &Arc<H>,
    ask: Ask,
    answering: &mpsc::Sender<Value>,
    watches: &mut HashMap<String, Watching>,
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
        What::Watch { kind, keys, when } => {
            // Asked again for the same records, as a launcher opening asks
            // for every change where shut it asked for fewer, it sends what
            // it held back at once rather than everything whole.
            if let Some(running) = watches.get(&id.to_string())
                && running.kind == kind
                && running.keys == keys
                && !running.task.is_finished()
            {
                running.when.send_replace(when);
                return None;
            }
            match holds.told(&kind) {
                Some(told) => {
                    let (sends, sent) = watch::channel(when);
                    let task = tokio::spawn(keep_watching(
                        Arc::clone(holds),
                        id.clone(),
                        (kind.clone(), keys.clone()),
                        told,
                        sent,
                        answering.clone(),
                    ));
                    let running = Watching {
                        kind,
                        keys,
                        when: sends,
                        task,
                    };
                    if let Some(earlier) = watches.insert(id.to_string(), running) {
                        earlier.task.abort();
                    }
                    return None;
                }
                None => Err(Refusal::UnknownKind(kind)),
            }
        }
        What::Unwatch => {
            if let Some(watch) = watches.remove(&id.to_string()) {
                watch.task.abort();
            }
            return None;
        }
        What::Steer { address, said } => {
            // A session on another machine is told over the link, which
            // may take a while, so the connection's other asks go on.
            let holds = Arc::clone(holds);
            later(answering, id, begun, async move {
                let machine = holds.steer(&address, &said).await?;
                Ok(("steered", json!({"machine": machine})))
            });
            return None;
        }
        What::Update { machine } => {
            // An update takes minutes, so the connection's other asks go on.
            let holds = Arc::clone(holds);
            later(answering, id, begun, async move {
                Ok(("updated", holds.update(&machine).await?))
            });
            return None;
        }
    };
    let mut answer = answered.unwrap_or_else(|refusal| refused(&id, &refusal));
    if let Some(fields) = answer.as_object_mut() {
        fields.insert("took".to_owned(), took(begun));
    }
    Some(answer)
}

/// Answers `id` through `answering` once `answered` ends, under the field
/// it names or as refused, so an ask that waits on another machine or a
/// program holds up none of the connection's others.
fn later(
    answering: &mpsc::Sender<Value>,
    id: Value,
    begun: std::time::Instant,
    answered: impl Future<Output = Result<(&'static str, Value), String>> + Send + 'static,
) {
    let answering = answering.clone();
    tokio::spawn(async move {
        let mut answer = match answered.await {
            Ok((field, said)) => json!({"id": id, field: said}),
            Err(why) => refused(&id, &Refusal::Refused(why)),
        };
        if let Some(fields) = answer.as_object_mut() {
            fields.insert("took".to_owned(), took(begun));
        }
        let _ = answering.send(answer).await;
    });
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
/// after the last, and nothing while nothing changes. While `when` names
/// fields, a record's change moving none of them is held back, and sent
/// with that record's next that does, or at once when `when` is changed
/// to let it through.
async fn keep_watching<H: Holds>(
    holds: Arc<H>,
    id: Value,
    (kind, keys): (String, Option<Vec<String>>),
    mut told: watch::Receiver<u64>,
    mut when: watch::Receiver<Option<Vec<String>>>,
    answering: mpsc::Sender<Value>,
) {
    let held = |holds: &H, told: u64| {
        let records = match holds.built() {
            Some(built) => built.records(holds, &kind, told),
            None => holds.records(&kind).map(Arc::new),
        }
        .unwrap_or_default();
        match &keys {
            Some(keys) => Arc::new(
                records
                    .iter()
                    .filter(|(key, _)| keys.contains(*key))
                    .map(|(key, record)| (key.clone(), record.clone()))
                    .collect(),
            ),
            None => records,
        }
    };
    let mut sent = BTreeMap::clone(&held(&holds, *told.borrow_and_update()));
    let whole: Vec<Value> = sent.values().cloned().collect();
    if answering
        .send(json!({"id": id, "records": whole}))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            heard = told.changed() => {
                if heard.is_err() {
                    return;
                }
                tokio::time::sleep(BETWEEN_SENDS).await;
            }
            asked = when.changed() => {
                if asked.is_err() {
                    return;
                }
            }
        }
        let now = held(&holds, *told.borrow_and_update());
        let (changed, gone) = changes(&sent, &now);
        let changed = sends(when.borrow().as_deref(), &sent, changed);
        if changed.is_empty() && gone.is_empty() {
            continue;
        }
        for record in &changed {
            if let Some((key, record)) = record
                .get("key")
                .and_then(Value::as_str)
                .and_then(|key| now.get_key_value(key))
            {
                sent.insert(key.clone(), record.clone());
            }
        }
        for key in &gone {
            sent.remove(key);
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

/// Of a change's records, those a watch sends: every one while `when`
/// names nothing; else each record come, and each moving a field `when`
/// names, such as a session's `told`, which moves only with what a shut
/// launcher reads. The rest are held back, so one session's `told`
/// moving sends that session alone, not every other's step beside it.
fn sends(
    when: Option<&[String]>,
    sent: &BTreeMap<String, Value>,
    changed: Vec<Value>,
) -> Vec<Value> {
    let Some(when) = when else {
        return changed;
    };
    changed
        .into_iter()
        .filter(|record| {
            record
                .get("key")
                .and_then(Value::as_str)
                .is_none_or(|key| !sent.contains_key(key))
                || when.iter().any(|field| record.get(field).is_some())
        })
        .collect()
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
                    when: None,
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
    fn a_steer_is_read_with_whom_it_is_for_and_what_it_says() {
        assert_eq!(
            read(r#"{"id":6,"ask":"steer","address":"AB12CD","steer":{"act":"pause"}}"#),
            Ok(Ask {
                id: json!(6),
                what: What::Steer {
                    address: "AB12CD".to_owned(),
                    said: json!({"act": "pause"}),
                },
            }),
            "a steer names its session's address and what is said"
        );
        for nothing in [
            r#"{"id":7,"ask":"steer","steer":{"act":"pause"}}"#,
            r#"{"id":7,"ask":"steer","address":"","steer":{"act":"pause"}}"#,
            r#"{"id":7,"ask":"steer","address":"AB12CD"}"#,
            r#"{"id":7,"ask":"steer","address":"AB12CD","steer":"pause"}"#,
        ] {
            assert_eq!(
                read(nothing).map_err(|(id, refusal)| (id, refusal.code())),
                Err((json!(7), "not-an-ask")),
                "a steer for nobody, or saying nothing, isn't one: {nothing}"
            );
        }
    }

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

    #[test]
    fn a_watch_asked_when_a_field_moves_sends_only_the_records_moving_it() {
        let sent: BTreeMap<String, Value> = BTreeMap::from([
            ("session:a".to_owned(), json!({"key": "session:a"})),
            ("session:c".to_owned(), json!({"key": "session:c"})),
        ]);
        let told = ["told".to_owned()];
        let stepped = || vec![json!({"key": "session:a", "step": "y"})];
        assert_eq!(
            sends(None, &sent, stepped()),
            stepped(),
            "asked when nothing in particular, every change is sent"
        );
        assert_eq!(
            sends(Some(&told), &sent, stepped()),
            Vec::<Value>::new(),
            "a step alone moves nothing a shut launcher reads"
        );
        assert_eq!(
            sends(
                Some(&told),
                &sent,
                vec![
                    json!({"key": "session:a", "step": "y", "told": "2"}),
                    json!({"key": "session:b", "step": "y"}),
                    json!({"key": "session:c", "step": "z"}),
                ]
            ),
            vec![
                json!({"key": "session:a", "step": "y", "told": "2"}),
                json!({"key": "session:b", "step": "y"}),
            ],
            "one moving its digest is sent, and one come, but not another's step"
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
