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
//!
//! Here is what a daemon holds and does, and the answering of one
//! connection; what is asked is read in `ask`, a record's shape and its
//! changes are `records`, and a running watch is `watching`.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt, Lines};
use tokio::sync::{mpsc, watch};

mod ask;
mod records;
mod watching;

pub use ask::{Ask, Refusal, What, read, refused};
pub use records::{Built, changes, key, record};
use watching::{Watching, keep_watching};

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

/// How long since `begun`, in milliseconds, as an answer's `took`.
fn took(begun: std::time::Instant) -> Value {
    Value::from(begun.elapsed().as_secs_f64() * 1000.0)
}
