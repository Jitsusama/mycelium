//! A machine's own health, as the health daemon on it says: one word for
//! the whole machine and one for each of its parts, carried in the record
//! its node measures, so every machine of the realm reads how every other
//! stands without asking each.
//!
//! The node is told which socket to ask (`--health-socket`) and asks it in
//! the daemon interface for its `summary` and `part` records, watching
//! both. Only the words are kept: never what a part says in a line, nor
//! when it was read, so the record moves only when a word does, and a
//! machine's health stays a few hundred bytes. A word outside the four
//! this knows reads `unread`, and a part whose name is odd, or past the
//! sixteenth, is dropped, which also holds for what a link carries in.
//!
//! While no daemon answers, or before one has said the whole, the machine
//! has no health at all, which every reader takes as unknown.
//!
//! The node keeps the health in a slot of its own, and each link it keeps
//! watches that slot. A link another machine dialled runs in a process of
//! its own, so it watches the node's slot over the node's socket
//! ([`from_node`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader,
};
use tokio::net::UnixStream;
use tokio::sync::watch;

use crate::wire::{Reply, Request, Response};

/// The words a health may be; any other reads as the last.
const WORDS: [&str; 4] = ["good", "watch", "bad", UNREAD];

/// What a health this doesn't know, or that couldn't be read, reads.
const UNREAD: &str = "unread";

/// The most parts a machine's health keeps: the daemon names four today,
/// and a few hundred bytes for all of them is what a record may carry.
const MOST_PARTS: usize = 16;

/// The longest name a part may go by.
const LONGEST_NAME: usize = 32;

/// The first wait before asking a daemon that went away again: long
/// enough that a daemon restarting isn't asked while it starts.
const FIRST_WAIT: Duration = Duration::from_secs(2);

/// The longest wait between asking a daemon that doesn't answer, so one
/// started later is heard within a minute without the node spinning on a
/// machine that runs none.
const LONGEST_WAIT: Duration = Duration::from_secs(60);

/// The longest line this reads from a daemon or a node. A daemon's part
/// record is a few hundred bytes and all of them a few kilobytes, so a
/// line past this is a daemon gone wrong, and the node lets it go rather
/// than holding whatever it sends.
const LONGEST_LINE: usize = 64 * 1024;

/// What a slot of a machine's health holds: none while it is unknown.
pub type Slot = watch::Sender<Option<Health>>;

/// What watches a slot.
pub type Told = watch::Receiver<Option<Health>>;

/// A machine's health, as its own daemon says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    /// The whole machine's word: `good`, `watch`, `bad` or `unread`.
    pub health: String,
    /// Each part's word, by its name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parts: BTreeMap<String, String>,
}

impl Health {
    /// This health with every word one this knows and only parts it
    /// keeps, as a health is wherever it came from.
    fn cleaned(self) -> Self {
        let mut parts = BTreeMap::new();
        for (name, word) in self.parts {
            keep_part(&mut parts, name, &word);
        }
        Self {
            health: word_of(&self.health),
            parts,
        }
    }
}

/// Reads a machine's health as a link carried it, cleaned as this
/// machine's own is: a word this build doesn't know reads `unread`, and
/// the parts it wouldn't keep are dropped.
///
/// # Errors
///
/// Fails only where the health isn't one at all.
pub fn cleaned<'de, D: Deserializer<'de>>(said: D) -> Result<Option<Health>, D::Error> {
    Ok(Option::<Health>::deserialize(said)?.map(Health::cleaned))
}

/// The word `said` is, as this reads it.
fn word_of(said: &str) -> String {
    if WORDS.contains(&said) { said } else { UNREAD }.to_owned()
}

/// Keeps part `name`'s `word` in `parts`, unless its name is odd or it
/// would be one past the most.
fn keep_part(parts: &mut BTreeMap<String, String>, name: String, word: &str) {
    let named = !name.is_empty()
        && name.len() <= LONGEST_NAME
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if named && (parts.len() < MOST_PARTS || parts.contains_key(&name)) {
        parts.insert(name, word_of(word));
    }
}

/// What a watch has heard so far: the whole's word once it is said, and
/// each part's.
#[derive(Debug, Default)]
struct Heard {
    whole: Option<String>,
    parts: BTreeMap<String, String>,
}

impl Heard {
    /// Takes in one line the daemon wrote: the records a watch begins
    /// with, or those it changed and the keys it let go.
    fn take(&mut self, line: &Value) {
        let records = ["records", "changed"]
            .into_iter()
            .filter_map(|field| line.get(field)?.as_array())
            .flatten();
        for record in records {
            let health = record.get("health").and_then(Value::as_str);
            match (record.get("kind").and_then(Value::as_str), health) {
                (Some("summary"), Some(word)) => self.whole = Some(word_of(word)),
                (Some("part"), Some(word)) => {
                    if let Some(name) = record.get("part").and_then(Value::as_str) {
                        keep_part(&mut self.parts, name.to_owned(), word);
                    }
                }
                _ => {}
            }
        }
        let gone = line
            .get("gone")
            .and_then(Value::as_array)
            .into_iter()
            .flatten();
        for key in gone.filter_map(Value::as_str) {
            if key == "summary" {
                self.whole = None;
            } else if let Some(name) = key.strip_prefix("part:") {
                self.parts.remove(name);
            }
        }
    }

    /// The health heard, once the whole has been said.
    fn health(&self) -> Option<Health> {
        Some(Health {
            health: self.whole.clone()?,
            parts: self.parts.clone(),
        })
    }
}

/// Keeps the health the daemon on `socket` says in `slot`, for as long as
/// the node runs: none while it doesn't answer, and asked again after a
/// wait that doubles each time it still doesn't say the whole.
pub async fn keep(socket: PathBuf, slot: Slot) {
    let mut wait = FIRST_WAIT;
    loop {
        let said = listen(&socket, &slot).await;
        slot.send_if_modified(|kept| kept.take().is_some());
        wait = after(wait, said);
        tokio::time::sleep(wait).await;
    }
}

/// How long to wait before asking again, having waited `waited` last:
/// the first wait once the daemon had said the whole, since a daemon that
/// did is one restarting, and twice the last otherwise, up to the
/// longest, so a daemon that answers but never says the whole isn't asked
/// every two seconds.
fn after(waited: Duration, said_whole: bool) -> Duration {
    if said_whole {
        FIRST_WAIT
    } else {
        (waited * 2).min(LONGEST_WAIT)
    }
}

/// Watches the daemon on `socket` until it goes away or sends a line past
/// the longest, writing `slot` each time a word moves, and says whether
/// it ever said the whole.
async fn listen(socket: &Path, slot: &Slot) -> bool {
    let Ok(stream) = UnixStream::connect(socket).await else {
        return false;
    };
    let (reading, mut writing) = stream.into_split();
    let asks = [
        json!({"id": 0, "ask": "hello"}),
        json!({"id": 1, "ask": "watch", "kind": "summary"}),
        json!({"id": 2, "ask": "watch", "kind": "part"}),
    ];
    for ask in asks {
        if writing
            .write_all(format!("{ask}\n").as_bytes())
            .await
            .is_err()
        {
            return false;
        }
    }
    let (mut reading, mut heard, mut said) = (BufReader::new(reading), Heard::default(), false);
    while let Some(line) = next_line(&mut reading).await {
        let Ok(line) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        heard.take(&line);
        let health = heard.health();
        said |= health.is_some();
        slot.send_if_modified(|kept| keep_moved(kept, health));
    }
    said
}

/// The next line `reading` holds, without its end, or none once it ends,
/// fails, or sends a line past the longest, which ends the reading too.
async fn next_line(reading: &mut (impl AsyncBufRead + Unpin)) -> Option<Vec<u8>> {
    let mut line = Vec::new();
    let limit = u64::try_from(LONGEST_LINE + 1).unwrap_or(u64::MAX);
    reading
        .take(limit)
        .read_until(b'\n', &mut line)
        .await
        .ok()?;
    // A line cut short by the end, as one past the longest is, ends it.
    (line.pop() == Some(b'\n')).then_some(line)
}

/// Watches the health the node on `node` keeps, as a link another
/// machine dialled does, since it runs apart from the node: none while
/// the node doesn't answer, or is one too old to say it, and asked again
/// after a wait that doubles each time it still doesn't, until what
/// watches is dropped.
#[must_use]
pub fn from_node(node: PathBuf) -> (Told, Watching) {
    let (slot, told) = watch::channel(None);
    let task = tokio::spawn(async move {
        let mut wait = FIRST_WAIT;
        loop {
            let said = watch_node(&node, &slot).await;
            slot.send_if_modified(|kept| kept.take().is_some());
            wait = after(wait, said);
            tokio::time::sleep(wait).await;
        }
    });
    (told, Watching(task))
}

/// The watch [`from_node`] keeps, which stops when this is dropped.
pub struct Watching(tokio::task::JoinHandle<()>);

impl Drop for Watching {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Waits until `told` moves, or forever where there is none or it has
/// closed, which it then lets go, so a closed one is never waited on
/// again.
pub async fn moved(told: &mut Option<Told>) {
    if let Some(watching) = told.as_mut()
        && watching.changed().await.is_ok()
    {
        return;
    }
    *told = None;
    std::future::pending::<()>().await;
}

/// Watches the health the node on `node` keeps until it hangs up,
/// writing `slot` each time it moves, and says whether the node said any.
async fn watch_node(node: &Path, slot: &Slot) -> bool {
    let Ok(stream) = UnixStream::connect(node).await else {
        return false;
    };
    let (reading, mut writing) = stream.into_split();
    let Ok(ask) = serde_json::to_string(&Request::Health) else {
        return false;
    };
    if writing
        .write_all(format!("{ask}\n").as_bytes())
        .await
        .is_err()
    {
        return false;
    }
    let (mut reading, mut said) = (BufReader::new(reading), false);
    while let Some(line) = next_line(&mut reading).await {
        // A node too old to know the ask refuses it, which reads as none.
        let Ok(Ok(Response::Health(health))) = serde_json::from_slice::<Reply>(&line) else {
            return said;
        };
        said = true;
        let health = health.map(Health::cleaned);
        slot.send_if_modified(|kept| keep_moved(kept, health));
    }
    said
}

/// Says the health in `slot` to a node's client on `writing`: what it is
/// now, then each time it moves, until the client hangs up or says
/// anything more.
pub async fn tell(
    mut told: Told,
    mut lines: tokio::io::Lines<impl AsyncBufRead + Unpin>,
    writing: &mut (impl tokio::io::AsyncWrite + Unpin),
) {
    loop {
        let health = told.borrow_and_update().clone();
        let Ok(line) = serde_json::to_string(&Reply::Ok(Response::Health(health))) else {
            return;
        };
        if writing
            .write_all(format!("{line}\n").as_bytes())
            .await
            .is_err()
        {
            return;
        }
        tokio::select! {
            moved = told.changed() => if moved.is_err() { return },
            // A client says nothing more; anything it does send, or its
            // hanging up, ends the telling.
            _ = lines.next_line() => return,
        }
    }
}

/// Keeps `health` in `kept` and says whether that moved it, so a watcher
/// hears only a word moving.
fn keep_moved(kept: &mut Option<Health>, health: Option<Health>) -> bool {
    let moved = *kept != health;
    if moved {
        *kept = health;
    }
    moved
}

#[cfg(test)]
mod tests {
    use tokio::net::UnixListener;
    use tokio::net::unix::OwnedWriteHalf;

    use super::*;

    /// How long a test waits for the node to hear a change.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// A stand-in for the health daemon: it takes one connection, waits
    /// for both watches, and hands back what writes to it.
    async fn daemon(listener: &UnixListener) -> OwnedWriteHalf {
        let (stream, _) = listener.accept().await.expect("the node connects");
        let (reading, writing) = stream.into_split();
        let mut lines = BufReader::new(reading).lines();
        let mut watched = Vec::new();
        while watched.len() < 2 {
            let line = lines.next_line().await.expect("a line").expect("an ask");
            let ask: Value = serde_json::from_str(&line).expect("an ask is JSON");
            if ask["ask"] == "watch" {
                watched.push(ask["kind"].as_str().expect("a kind").to_owned());
            }
        }
        assert_eq!(
            watched,
            ["summary", "part"],
            "it watches the whole, then the parts"
        );
        writing
    }

    /// Writes `line` to the node as one line.
    async fn say(writing: &mut OwnedWriteHalf, line: &Value) {
        writing
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("the node reads");
    }

    /// A summary record whose whole is `health`, as the daemon writes it.
    fn summary(health: &str) -> Value {
        json!({"key": "summary", "kind": "summary", "health": health, "machine": "kestrel",
               "parts": ["machines", "network"], "read_at": "2026-11-16T02:00:00Z"})
    }

    /// A part record for `part`, whose word is `health`, saying `says`.
    fn part(part: &str, health: &str, says: &str) -> Value {
        json!({"key": format!("part:{part}"), "kind": "part", "part": part,
               "health": health, "says": says})
    }

    /// Waits until `slot` holds what `wanted` says.
    async fn until(
        told: &mut watch::Receiver<Option<Health>>,
        wanted: impl FnMut(&Option<Health>) -> bool,
    ) -> Option<Health> {
        tokio::time::timeout(PATIENCE, told.wait_for(wanted))
            .await
            .expect("heard in time")
            .expect("still kept")
            .clone()
    }

    /// A slot of a test's own, so no test hears another's daemon.
    fn slot() -> Slot {
        watch::Sender::new(None)
    }

    /// A health whose whole is `word`, with one part, `code`, as `code`.
    fn health(word: &str, code: &str) -> Health {
        Health {
            health: word.to_owned(),
            parts: BTreeMap::from([("code".to_owned(), code.to_owned())]),
        }
    }

    #[tokio::test]
    async fn a_line_past_the_longest_lets_the_daemon_go() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("system.sock");
        let listener = UnixListener::bind(&socket).expect("the stand-in listens");
        let slot = slot();
        let mut told = slot.subscribe();
        tokio::spawn(keep(socket, slot));
        let mut writing = daemon(&listener).await;
        say(
            &mut writing,
            &json!({"id": 1, "records": [summary("good")]}),
        )
        .await;
        until(&mut told, Option::is_some).await;
        let long = json!({"changed": [part("code", "good", &"x".repeat(LONGEST_LINE))]});
        say(&mut writing, &long).await;
        assert_eq!(
            until(&mut told, Option::is_none).await,
            None,
            "a line past the longest ends the watch, and with it the health"
        );
    }

    #[test]
    fn the_wait_starts_again_only_once_the_whole_was_said() {
        assert_eq!(after(LONGEST_WAIT, true), FIRST_WAIT);
        assert_eq!(
            after(FIRST_WAIT, false),
            FIRST_WAIT * 2,
            "a daemon that answered but never said the whole waits longer"
        );
        assert_eq!(after(LONGEST_WAIT, false), LONGEST_WAIT);
    }

    #[tokio::test]
    async fn a_dialled_link_hears_the_nodes_health_as_it_moves() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let node = dir.path().join("node.sock");
        let listener = UnixListener::bind(&node).expect("the stand-in node listens");
        let kept = slot();
        let theirs = kept.subscribe();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("the link connects");
            let (reading, mut writing) = stream.into_split();
            let mut lines = BufReader::new(reading).lines();
            let asked = lines.next_line().await.expect("a line").expect("an ask");
            assert_eq!(
                serde_json::from_str::<Request>(&asked).expect("a request"),
                Request::Health
            );
            tell(theirs, lines, &mut writing).await;
        });
        let (mut told, _watching) = from_node(node);
        kept.send_replace(Some(health("watch", "watch")));
        let heard = until(&mut told, Option::is_some).await;
        assert_eq!(heard, Some(health("watch", "watch")));
        kept.send_replace(Some(health("bad", "bad")));
        let heard = until(&mut told, |heard| {
            heard.as_ref().is_some_and(|heard| heard.health == "bad")
        })
        .await;
        assert_eq!(heard, Some(health("bad", "bad")), "a move is heard");
        kept.send_replace(None);
        assert_eq!(until(&mut told, Option::is_none).await, None);
    }

    #[tokio::test]
    async fn a_node_too_old_to_say_its_health_leaves_none() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let node = dir.path().join("node.sock");
        let listener = UnixListener::bind(&node).expect("the stand-in node listens");
        let (told, _watching) = from_node(node);
        let (stream, _) = listener.accept().await.expect("the link connects");
        let (reading, mut writing) = stream.into_split();
        let mut lines = BufReader::new(reading).lines();
        lines.next_line().await.expect("a line").expect("an ask");
        say(
            &mut writing,
            &json!({"Err": "not a request: unknown variant `Health`"}),
        )
        .await;
        // The link hangs up on a refusal, so the stand-in hears the end.
        assert_eq!(
            tokio::time::timeout(PATIENCE, lines.next_line())
                .await
                .expect("heard in time")
                .expect("read"),
            None
        );
        assert_eq!(*told.borrow(), None);
    }

    #[tokio::test]
    async fn a_closed_slot_is_never_waited_on_again() {
        let slot = slot();
        let mut told = Some(slot.subscribe());
        drop(slot);
        let waited = tokio::time::timeout(Duration::from_millis(50), moved(&mut told)).await;
        assert!(waited.is_err(), "a closed slot waits forever");
        assert!(told.is_none(), "and is let go");
    }

    #[tokio::test]
    async fn the_whole_and_each_part_are_kept_as_the_daemon_says_them() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("system.sock");
        let listener = UnixListener::bind(&socket).expect("the stand-in listens");
        let slot = slot();
        let mut told = slot.subscribe();
        tokio::spawn(keep(socket, slot));
        let mut writing = daemon(&listener).await;
        say(
            &mut writing,
            &json!({"id": 1, "records": [summary("watch")]}),
        )
        .await;
        say(
            &mut writing,
            &json!({"id": 2, "records": [part("machines", "watch", "1 of 2 answer"),
                                          part("network", "good", "all up")]}),
        )
        .await;
        let health = until(&mut told, |health| {
            health
                .as_ref()
                .is_some_and(|health| health.parts.len() == 2)
        })
        .await;
        assert_eq!(
            health,
            Some(Health {
                health: "watch".to_owned(),
                parts: BTreeMap::from([
                    ("machines".to_owned(), "watch".to_owned()),
                    ("network".to_owned(), "good".to_owned()),
                ]),
            })
        );
        say(
            &mut writing,
            &json!({"changed": [part("machines", "good", "2 of 2 answer")]}),
        )
        .await;
        say(&mut writing, &json!({"changed": [summary("good")]})).await;
        let health = until(&mut told, |health| {
            health
                .as_ref()
                .is_some_and(|health| health.health == "good")
        })
        .await
        .expect("a health");
        assert_eq!(health.parts["machines"], "good", "a part's change is heard");
    }

    #[test]
    fn a_part_saying_something_new_in_the_same_word_moves_nothing() {
        let mut heard = Heard::default();
        heard.take(&json!({"records": [summary("good"), part("code", "good", "3 trees")]}));
        let mut kept = None;
        assert!(
            keep_moved(&mut kept, heard.health()),
            "the first health moves it"
        );
        let mut read_again = summary("good");
        read_again["read_at"] = json!("2026-11-16T02:01:00Z");
        for line in [
            json!({"changed": [part("code", "good", "4 trees")]}),
            json!({"changed": [read_again]}),
        ] {
            heard.take(&line);
            assert!(!keep_moved(&mut kept, heard.health()), "{line} moved it");
        }
        heard.take(&json!({"changed": [part("code", "watch", "a tree is dirty")]}));
        assert!(
            keep_moved(&mut kept, heard.health()),
            "a word moving moves it"
        );
        assert_eq!(kept.expect("a health").parts["code"], "watch");
    }

    #[tokio::test]
    async fn a_daemon_that_goes_away_leaves_no_health() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("system.sock");
        let listener = UnixListener::bind(&socket).expect("the stand-in listens");
        let slot = slot();
        let mut told = slot.subscribe();
        tokio::spawn(keep(socket, slot));
        let mut writing = daemon(&listener).await;
        say(&mut writing, &json!({"id": 1, "records": [summary("bad")]})).await;
        until(&mut told, Option::is_some).await;
        drop(writing);
        drop(listener);
        assert_eq!(until(&mut told, Option::is_none).await, None);
    }

    #[test]
    fn a_word_it_doesnt_know_reads_unread_and_odd_parts_are_dropped() {
        let mut heard = Heard::default();
        let mut parts: Vec<Value> = (0..20)
            .map(|count| part(&format!("p{count:02}"), "good", ""))
            .collect();
        parts.push(part("Upper", "good", ""));
        parts.push(part(&"long".repeat(9), "good", ""));
        parts.push(part("odd.name", "good", ""));
        heard.take(&json!({"records": parts}));
        heard.take(&json!({"records": [summary("not read")]}));
        heard.take(&json!({"changed": [part("p00", "on fire", "")]}));
        let health = heard.health().expect("the whole is said");
        assert_eq!(health.health, "unread");
        assert_eq!(health.parts.len(), MOST_PARTS, "past the most are dropped");
        assert_eq!(health.parts["p00"], "unread");
        assert!(health.parts.keys().all(|name| name.starts_with('p')));
        assert!(
            serde_json::to_string(&health).expect("a health").len() < 1024,
            "the most a health holds is under a kilobyte"
        );
    }

    #[test]
    fn a_part_let_go_is_forgotten_and_no_whole_is_no_health() {
        let mut heard = Heard::default();
        heard.take(&json!({"records": [part("network", "good", "")]}));
        assert_eq!(heard.health(), None, "parts alone are no health");
        heard.take(&json!({"records": [summary("good")]}));
        heard.take(&json!({"gone": ["part:network"]}));
        assert_eq!(heard.health().expect("a health").parts, BTreeMap::new());
        heard.take(&json!({"gone": ["summary"]}));
        assert_eq!(heard.health(), None);
    }

    #[test]
    fn what_a_link_carries_is_cleaned_as_this_machines_own_is() {
        #[derive(Deserialize)]
        struct Carried {
            #[serde(default, deserialize_with = "cleaned")]
            health: Option<Health>,
        }
        let carried: Carried = serde_json::from_value(json!({
            "health": {"health": "fine", "parts": {"network": "bad", "Odd Name": "good"}}
        }))
        .expect("a health");
        assert_eq!(
            carried.health,
            Some(Health {
                health: "unread".to_owned(),
                parts: BTreeMap::from([("network".to_owned(), "bad".to_owned())]),
            })
        );
        let none: Carried = serde_json::from_value(json!({})).expect("no health");
        assert_eq!(none.health, None);
    }
}
