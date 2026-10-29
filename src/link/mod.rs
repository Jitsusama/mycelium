//! A link: two machines' stores kept as one, over any byte stream.
//!
//! Both ends run the same code. Each says who it is ([`membership`]), then
//! which messages it holds; each then sends the other what it lacks, and
//! from there every new message as it lands. Messages are written once by
//! id, so holding the union of both ends is always right and nothing needs
//! agreeing. What is said to a machine's own group stays on that machine.
//! Each end also sends its own living sessions' presence, which the other
//! keeps until the session ends or the link drops, since only a session's
//! own machine can say whether it lives; one link at a time carries a
//! machine's sessions into a store, by the store's claim.
//!
//! Each end also says what its machine is, as its node would measure it,
//! when the link starts and again each minute, and keeps what the other
//! end says of its own machine, which turns out of reach when the link
//! drops. Only an end whose hello says it knows of this hears it.
//!
//! Each end reads in a task of its own, so two ends sending a week of
//! messages at once never wait on each other.
//!
//! Where a machine shares quests, its end also carries their files
//! ([`crate::share::exchange`]), looking for changes every second. An end
//! that shares none says nothing of them, so it links with an end too old
//! to know of sharing.
//!
//! How an end is reached is [`dial`]'s, and which ends to reach is
//! [`discovery`]'s.

pub mod dial;
pub mod discovery;
pub mod membership;

use std::collections::BTreeMap;
use std::collections::HashSet;
use std::io;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::facts::{self, Machine, Readiness};
use crate::mesh;
use crate::place;
use crate::share::exchange::{Exchange, Places, Say};
use crate::store::{self, Change, Claim, Store};
use dial::Connection;
use membership::{Hello, Membership};

/// How far back a link reaches for messages at most: the mesh's own week,
/// so a link never hands back a message the other end has already let go.
const REACH: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How long the other end has to say hello before the link gives up.
const HELLO_WITHIN: Duration = Duration::from_secs(30);

/// How often this end says again that the sessions it carried here still
/// stand, so a copy nothing keeps saying so can be judged gone, and says
/// again what its machine is, whose room changes by the minute.
const KEEP_EVERY: Duration = Duration::from_secs(60);

/// How often this end looks for changes in the quests it shares.
const LOOK_EVERY: Duration = Duration::from_secs(1);

/// One line on a link.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Frame {
    /// Who this end is; always first.
    Hello(Hello),
    /// The ids of every message this end holds within reach, and of its
    /// machine's living sessions.
    Have {
        /// Their ids.
        messages: Vec<String>,
        /// The living sessions' ids; an end older than this names none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        living: Option<Vec<String>>,
    },
    /// A message, as its sender wrote it.
    Message {
        /// Its id.
        id: String,
        /// The message.
        body: Value,
    },
    /// A living session of this end's machine, as its presence says.
    Presence {
        /// Its id.
        session: String,
        /// The presence.
        body: Value,
        /// The change leases it holds that the other machine can weigh; an
        /// end older than this names none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        leases: Option<Vec<Value>>,
    },
    /// A session of this end's machine has ended.
    Ended {
        /// Its id.
        session: String,
    },
    /// Something said about shared quests.
    Share(Say),
    /// What this end's machine is, as measured as it was sent.
    Machine(Box<Machine>),
}

/// Runs this end of a link until the other end leaves, then marks every
/// session it carried here out of reach. With `shares`, it carries the
/// files of the quests this machine shares too.
pub async fn run(
    store: impl Store,
    membership: &impl Membership,
    connection: Connection,
    shares: Option<Places>,
) -> io::Result<()> {
    let Connection {
        reading,
        mut writing,
        keep: _keep,
    } = connection;
    let mut changes = store.changes()?;
    let (mut frames, _reader) = read_frames(reading);

    send(&mut writing, &Frame::Hello(membership.hello())).await?;
    let other = hello(&mut frames).await?;
    membership.admit(&other).map_err(refusal)?;
    let Some(claim) = store.claim(&other.machine)? else {
        return Err(refusal(format!(
            "a link to {} already carries it here",
            other.machine
        )));
    };
    let reach = reach_start(&store, &other.machine);
    store.mark_linked(&other.machine, now())?;
    let exchange = shares
        .map(|places| Exchange::new(places, membership.hello().machine, other.machine.clone()));
    let mut end = End {
        store,
        here: membership.hello().machine,
        facts: other.facts,
        other: other.machine,
        reach,
        has: HashSet::new(),
        sent: BTreeMap::new(),
        carried: HashSet::new(),
        exchange,
        _claim: claim,
    };

    let messages = end.store.message_ids(end.reach);
    let living = Some(end.living().into_keys().collect());
    send(&mut writing, &Frame::Have { messages, living }).await?;
    end.send_machine(&mut writing).await?;
    end.send_presence(&mut writing).await?;

    // The start has just said all of it, so the first keep is a minute on.
    let mut keeping =
        tokio::time::interval_at(tokio::time::Instant::now() + KEEP_EVERY, KEEP_EVERY);
    let mut looking = tokio::time::interval(LOOK_EVERY);
    let result = loop {
        let step = tokio::select! {
            frame = frames.recv() => match frame {
                Some(Ok(frame)) => end.take(frame, &mut writing).await,
                Some(Err(error)) => Err(error),
                None => break Ok(()),
            },
            _ = keeping.tick() => end.keep(&mut writing).await,
            _ = looking.tick() => end.share(Exchange::look, &mut writing).await,
            change = changes.changes.recv() => match change {
                Some(Change::Message(id)) => end.send_messages(vec![id], &mut writing).await,
                Some(Change::Presence) => end.send_presence(&mut writing).await,
                Some(Change::Rescan) => end.catch_up(&mut writing).await,
                None => break Ok(()),
            },
        };
        if let Err(error) = step {
            break Err(error);
        }
    };
    // Had the link died outright, the last minute's mark would stand.
    let left = now();
    let _ = end.store.mark_linked(&end.other, left);
    store::out_of_reach(&end.store, &end.other, left);
    result
}

/// The task reading a link's frames, stopped when this end of the link is,
/// so the stream it reads is let go with the rest of the link.
struct Reader(tokio::task::JoinHandle<()>);

impl Drop for Reader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Reads the other end's frames in a task of their own, until it leaves or
/// sends a line that isn't one.
fn read_frames(
    reading: Box<dyn AsyncRead + Unpin + Send>,
) -> (mpsc::UnboundedReceiver<io::Result<Frame>>, Reader) {
    let (sender, frames) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let mut lines = BufReader::new(reading).lines();
        loop {
            let frame = match lines.next_line().await {
                Ok(Some(line)) => serde_json::from_str(&line)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
                Ok(None) => return,
                Err(error) => Err(error),
            };
            let failed = frame.is_err();
            if sender.send(frame).is_err() || failed {
                return;
            }
        }
    });
    (frames, Reader(task))
}

/// The other end's hello, which must come first and soon.
async fn hello(frames: &mut mpsc::UnboundedReceiver<io::Result<Frame>>) -> io::Result<Hello> {
    let first = tokio::time::timeout(HELLO_WITHIN, frames.recv())
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                format!("the other end said nothing within {HELLO_WITHIN:?}"),
            )
        })?;
    match first {
        Some(Ok(Frame::Hello(other))) => Ok(other),
        Some(Ok(_)) => Err(refusal("the other end didn't say hello first".to_owned())),
        Some(Err(error)) => Err(error),
        // Why is the dial's to say, as SSH does on its own error output: a
        // machine it can't find, a key refused, or no mycelium there.
        None => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the other end closed before saying hello",
        )),
    }
}

/// This end of a link, and what it knows of the other.
struct End<S> {
    store: S,
    /// This end's machine.
    here: String,
    /// Whether the other end hears what this end's machine is.
    facts: bool,
    other: String,
    reach: i64,
    has: HashSet<String>,
    /// Each living session's presence and leases, as last sent.
    sent: BTreeMap<String, (Value, Vec<Value>)>,
    carried: HashSet<String>,
    exchange: Option<Exchange>,
    _claim: Claim,
}

impl<S: Store> End<S> {
    async fn take(
        &mut self,
        frame: Frame,
        writing: &mut (impl AsyncWrite + Unpin),
    ) -> io::Result<()> {
        match frame {
            Frame::Hello(_) => return Err(refusal("the other end said hello twice".to_owned())),
            Frame::Have { messages, living } => {
                // What an earlier link to that machine left out of reach is
                // settled now: those still living come again as presences,
                // and the rest have ended. An end that names none can't
                // say, so all go, and the living come again.
                let living: HashSet<String> = living.into_iter().flatten().collect();
                store::let_go(&self.store, &self.other, |session| living.contains(session));
                self.has.extend(messages);
                self.catch_up(writing).await?;
            }
            Frame::Message { id, body } => {
                self.has.insert(id.clone());
                if body.get("id").and_then(Value::as_str) == Some(id.as_str()) {
                    self.kept(&id, self.store.put_message(&id, &body));
                } else {
                    self.kept(
                        &id,
                        Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "its body names another id",
                        )),
                    );
                }
            }
            Frame::Presence {
                session,
                body,
                leases,
            } => {
                let put = self.store.put_presence(&session, &self.other, &body);
                if put.is_ok() {
                    self.carried.insert(session.clone());
                    if let Some(leases) = leases {
                        self.kept(
                            &session,
                            self.store.put_leases(&session, &self.other, &leases),
                        );
                    }
                }
                self.kept(&session, put);
            }
            Frame::Ended { session } => {
                self.carried.remove(&session);
                self.store.remove_presence(&session, &self.other)?;
            }
            Frame::Share(say) => {
                self.share(move |exchange| exchange.hear(say), writing)
                    .await?;
            }
            Frame::Machine(said) => self.keep_machine(*said),
        }
        Ok(())
    }

    /// Keeps what the other end says its machine is, which only it can
    /// say: a machine named otherwise is refused, and it is ready while
    /// this link carries it, whatever it claims.
    fn keep_machine(&self, mut said: Machine) {
        if said.name != self.other {
            self.kept(
                &said.name,
                Err(refusal(format!("{} spoke for another machine", self.other))),
            );
            return;
        }
        said.state = Readiness::Ready;
        self.kept(&said.name, self.store.put_machine(&said));
    }

    /// Says what this end's machine is, measured now, to an end that hears
    /// it. Measuring reads a few files, or runs `sysctl` on a Mac, so it
    /// runs away from the link's own work.
    async fn send_machine(&mut self, writing: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        if !self.facts {
            return Ok(());
        }
        let here = self.here.clone();
        let machine = tokio::task::spawn_blocking(move || {
            let home = std::env::home_dir().unwrap_or_else(std::env::temp_dir);
            facts::measure(&here, &home, place::queue().as_deref())
        })
        .await
        .map_err(io::Error::other)?;
        send(writing, &Frame::Machine(Box::new(machine))).await
    }

    /// Says again that what this link carried here stands, and what this
    /// end's machine is now.
    async fn keep(&mut self, writing: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        self.keep_carried()?;
        self.send_machine(writing).await
    }

    /// Has the exchange act on the disk, away from the link's own work,
    /// and sends what it says. A quest that fails to share is told of and
    /// tried again at the next look, rather than ending the link, which
    /// would only meet it again at every redial.
    async fn share(
        &mut self,
        act: impl FnOnce(&mut Exchange) -> io::Result<Vec<Say>> + Send + 'static,
        writing: &mut (impl AsyncWrite + Unpin),
    ) -> io::Result<()> {
        let Some(mut exchange) = self.exchange.take() else {
            return Ok(());
        };
        let (mut exchange, says) = tokio::task::spawn_blocking(move || {
            let says = act(&mut exchange);
            (exchange, says)
        })
        .await
        .map_err(io::Error::other)?;
        // Written here, a finding reaches this machine's sessions and, as
        // any message does, the other's.
        for draft in exchange.findings() {
            let (id, body) = mesh::written(&draft, std::process::id());
            self.kept(&id, self.store.put_message(&id, &body));
        }
        self.exchange = Some(exchange);
        match says {
            Ok(says) => {
                for say in says {
                    send(writing, &Frame::Share(say)).await?;
                }
            }
            Err(error) => eprintln!("sharing quests with {}: {error}", self.other),
        }
        Ok(())
    }

    /// Tells of a record the store refused. One bad record is the other
    /// end's to fix, so the link carries on with the rest rather than
    /// ending, which would only meet the same record at every redial.
    fn kept(&self, record: &str, put: io::Result<()>) {
        if let Err(error) = put {
            eprintln!("refused {record} from {}: {error}", self.other);
        }
    }

    /// Sends every message within reach the other end lacks.
    async fn catch_up(&mut self, writing: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        let ids = self.store.message_ids(self.reach);
        self.send_messages(ids, writing).await?;
        self.send_presence(writing).await
    }

    /// Sends the messages among `ids` the other end doesn't hold yet,
    /// except those that stay on this machine.
    async fn send_messages(
        &mut self,
        ids: Vec<String>,
        writing: &mut (impl AsyncWrite + Unpin),
    ) -> io::Result<()> {
        for id in ids {
            if self.has.contains(&id) {
                continue;
            }
            let Some(body) = self.store.message(&id) else {
                continue;
            };
            self.has.insert(id.clone());
            if !mesh::stays_here(&body, |earlier| self.store.message(earlier)) {
                send(writing, &Frame::Message { id, body }).await?;
            }
        }
        Ok(())
    }

    /// Sends what changed in this machine's living sessions since last sent.
    async fn send_presence(&mut self, writing: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        let living = self.living();
        let ended: Vec<String> = self
            .sent
            .keys()
            .filter(|session| !living.contains_key(*session))
            .cloned()
            .collect();
        for session in ended {
            self.sent.remove(&session);
            send(writing, &Frame::Ended { session }).await?;
        }
        let holdings = self.store.holdings();
        for (session, body) in living {
            let said = (body, mesh::carried_leases(&holdings, &session));
            if self.sent.get(&session) != Some(&said) {
                send(
                    writing,
                    &Frame::Presence {
                        session: session.clone(),
                        body: said.0.clone(),
                        leases: Some(said.1.clone()),
                    },
                )
                .await?;
                self.sent.insert(session, said);
            }
        }
        Ok(())
    }

    /// This machine's living sessions, as their presences say.
    fn living(&self) -> BTreeMap<String, Value> {
        self.store
            .presences()
            .into_iter()
            .filter(|presence| presence.elsewhere.is_none() && presence.lives)
            .map(|presence| (presence.session, presence.body))
            .collect()
    }

    /// Says again that every session this link carried here still stands,
    /// and marks the link as standing now.
    fn keep_carried(&self) -> io::Result<()> {
        for session in &self.carried {
            if let Err(error) = self.store.keep_presence(session, &self.other) {
                eprintln!("couldn't keep {session} from {}: {error}", self.other);
            }
        }
        self.store.mark_linked(&self.other, now())
    }
}

async fn send(writing: &mut (impl AsyncWrite + Unpin), frame: &Frame) -> io::Result<()> {
    let mut line = serde_json::to_vec(frame).map_err(io::Error::other)?;
    line.push(b'\n');
    writing.write_all(&line).await?;
    writing.flush().await
}

/// How far back a link to `machine` reaches for messages, in milliseconds
/// since the epoch: to when the two were last linked, a minute early for a
/// mark a link killed outright left late, and never past the mesh's week.
/// Messages from before two machines were ever linked never cross, since
/// every session hearing them on the other machine would take a week of
/// talk it was never part of as new.
fn reach_start(store: &impl Store, machine: &str) -> i64 {
    let now = now();
    let week = now.saturating_sub(millis(REACH));
    store
        .linked_until(machine)
        .map_or(now, |until| until.saturating_sub(millis(KEEP_EVERY)))
        .max(week)
}

fn now() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn refusal(reason: String) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, reason)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::{DuplexStream, ReadHalf, WriteHalf};

    use super::*;
    use crate::link::membership::{Realm, VERSION};
    use crate::store::memory::Memory;

    /// How long a test waits for a link to carry something before failing.
    const PATIENCE: Duration = Duration::from_secs(5);

    fn realm(machine: &str) -> Realm {
        Realm {
            name: "work".to_owned(),
            machine: machine.to_owned(),
        }
    }

    fn connection(reading: ReadHalf<DuplexStream>, writing: WriteHalf<DuplexStream>) -> Connection {
        Connection {
            reading: Box::new(reading),
            writing: Box::new(writing),
            keep: Box::new(()),
        }
    }

    /// Runs `machine`'s end of a link over `pipe`.
    fn end(
        store: &Memory,
        machine: &'static str,
        pipe: DuplexStream,
    ) -> tokio::task::JoinHandle<io::Result<()>> {
        sharing_end(store, machine, pipe, None)
    }

    /// Runs `machine`'s end of a link over `pipe`, sharing the quests kept
    /// in `shares`.
    fn sharing_end(
        store: &Memory,
        machine: &'static str,
        pipe: DuplexStream,
        shares: Option<Places>,
    ) -> tokio::task::JoinHandle<io::Result<()>> {
        let store = store.clone();
        let (reading, writing) = tokio::io::split(pipe);
        tokio::spawn(async move {
            run(store, &realm(machine), connection(reading, writing), shares).await
        })
    }

    /// Waits until the file at `path` holds `text`. Nothing tells of a
    /// write to a quest's folder, so it looks again every few milliseconds
    /// until it does, or gives up.
    async fn until_file(path: &std::path::Path, text: &str) {
        let waited = tokio::time::timeout(PATIENCE, async {
            while std::fs::read_to_string(path).ok().as_deref() != Some(text) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            waited.is_ok(),
            "{} didn't come to hold {text:?} within {PATIENCE:?}",
            path.display()
        );
    }

    /// Marks the two stores as last linked `ms_ago`, as if they had been
    /// apart since.
    fn apart_since(optimus: &Memory, equilibrius: &Memory, ms_ago: i64) {
        let until = now() - ms_ago;
        optimus
            .mark_linked("equilibrius", until)
            .expect("a memory store marks");
        equilibrius
            .mark_linked("optimus", until)
            .expect("a memory store marks");
    }

    /// Two stores joined by a link over an in-memory pipe of `buffer` bytes,
    /// optimus's end first.
    fn linked(
        optimus: &Memory,
        equilibrius: &Memory,
        buffer: usize,
    ) -> (
        tokio::task::JoinHandle<io::Result<()>>,
        tokio::task::JoinHandle<io::Result<()>>,
    ) {
        let (near, far) = tokio::io::duplex(buffer);
        (
            end(optimus, "optimus", near),
            end(equilibrius, "equilibrius", far),
        )
    }

    /// Waits on `store`'s changes until `holds` is true of it.
    async fn until(store: &Memory, holds: impl Fn(&Memory) -> bool) {
        let mut changes = store.changes().expect("a memory store tells its changes");
        let waited = tokio::time::timeout(PATIENCE, async {
            while !holds(store) {
                changes.changes.recv().await;
            }
        })
        .await;
        assert!(
            waited.is_ok(),
            "the link didn't carry it within {PATIENCE:?}"
        );
    }

    fn recent(ms_ago: i64, from: &str, n: usize) -> String {
        format!(
            "{}-{from}-1-{n}",
            jiff::Timestamp::now().as_millisecond() - ms_ago
        )
    }

    /// A message `id` as the mesh writes it, to `to`.
    fn said(store: &Memory, id: &str, to: &str) {
        store.send_here(
            id,
            json!({"id": id, "to": [to], "text": format!("message {id}")}),
        );
    }

    #[tokio::test]
    async fn each_end_gets_the_messages_it_lacked_and_each_new_one() {
        let optimus = Memory::default();
        let equilibrius = Memory::default();
        let before = recent(1000, "WCJQSG", 0);
        let theirs = recent(900, "20KSI7", 0);
        let stale = format!("{}-OLD-1-0", now() - millis(REACH) - 1000);
        said(&optimus, &before, "@20KSI7");
        said(&optimus, &stale, "@20KSI7");
        said(&equilibrius, &theirs, "@WCJQSG");

        // Apart longer than the mesh keeps anything, so the week is the reach.
        apart_since(&optimus, &equilibrius, 2 * millis(REACH));
        let _link = linked(&optimus, &equilibrius, 1 << 16);
        until(&equilibrius, |store| store.message(&before).is_some()).await;
        until(&optimus, |store| store.message(&theirs).is_some()).await;

        let after = recent(0, "4UHC5Q", 0);
        said(&optimus, &after, "#quest/4uhc5q");
        until(&equilibrius, |store| store.message(&after).is_some()).await;
        assert_eq!(
            equilibrius.message(&stale),
            None,
            "a message past reach stays behind"
        );
    }

    #[tokio::test]
    async fn what_is_said_to_the_machine_stays_on_it() {
        let optimus = Memory::default();
        let equilibrius = Memory::default();
        let hold = recent(30, "16V6F4", 0);
        let ended = recent(20, "16V6F4", 1);
        let next = recent(10, "WCJQSG", 0);
        said(&optimus, &hold, "#all");
        let answer = json!({"id": ended, "to": ["@16V6F4"], "re": hold, "text": "parked"});
        optimus.send_here(&ended, answer);
        said(&optimus, &next, "@20KSI7");

        apart_since(&optimus, &equilibrius, 60_000);
        let _link = linked(&optimus, &equilibrius, 1 << 16);
        until(&equilibrius, |store| store.message(&next).is_some()).await;
        assert_eq!(
            equilibrius.message(&hold),
            None,
            "a hold to the machine stays on it"
        );
        assert_eq!(equilibrius.message(&ended), None, "and so do its answers");
    }

    #[tokio::test]
    async fn two_ends_with_a_week_each_catch_up_through_a_narrow_pipe() {
        let optimus = Memory::default();
        let equilibrius = Memory::default();
        let many = 2000;
        for n in 0..many {
            said(&optimus, &recent(5000, "WCJQSG", n), "@20KSI7");
            said(&equilibrius, &recent(5000, "20KSI7", n), "@WCJQSG");
        }
        // A pipe narrower than one message, so neither end can write its
        // backlog without the other reading.
        apart_since(&optimus, &equilibrius, 60_000);
        let _link = linked(&optimus, &equilibrius, 64);
        until(&optimus, |store| store.message_ids(0).len() == 2 * many).await;
        until(&equilibrius, |store| store.message_ids(0).len() == 2 * many).await;
    }

    #[tokio::test]
    async fn a_first_link_carries_only_what_is_said_from_then_on() {
        let optimus = Memory::default();
        let equilibrius = Memory::default();
        let before = recent(1000, "WCJQSG", 0);
        said(&optimus, &before, "#repo/walgit-rs");

        optimus.start_here("launcher", json!({"session": "launcher"}), true);
        let _link = linked(&optimus, &equilibrius, 1 << 16);
        // The launcher arriving says the link stands.
        until(&equilibrius, |store| !store.presences().is_empty()).await;
        let after = recent(0, "WCJQSG", 1);
        said(&optimus, &after, "#repo/walgit-rs");
        until(&equilibrius, |store| store.message(&after).is_some()).await;
        assert_eq!(
            equilibrius.message(&before),
            None,
            "talk from before the machines linked stays"
        );
    }

    #[tokio::test]
    async fn a_living_session_is_carried_let_go_when_it_ends_and_unreachable_once_the_link_drops() {
        let optimus = Memory::default();
        let equilibrius = Memory::default();
        equilibrius.start_here("walgit", json!({"session": "walgit"}), true);
        equilibrius.start_here("gone", json!({"session": "gone"}), false);

        let (near, _far) = linked(&optimus, &equilibrius, 1 << 16);
        until(&optimus, |store| !store.presences().is_empty()).await;
        let carried = optimus.presences();
        assert_eq!(
            carried.len(),
            1,
            "only the living session is carried: {carried:?}"
        );
        assert_eq!(carried[0].elsewhere.as_deref(), Some("equilibrius"));

        equilibrius.start_here("walgit", json!({"session": "walgit"}), false);
        until(&optimus, |store| store.presences().is_empty()).await;

        optimus.start_here("launcher", json!({"session": "launcher"}), true);
        until(&equilibrius, |store| {
            store.presences().iter().any(|p| p.elsewhere.is_some())
        })
        .await;
        // Stopping optimus's end closes its half of the pipe, so the Mac's
        // end sees the link drop, and keeps what optimus carried out of
        // reach rather than gone.
        near.abort();
        until(&equilibrius, |store| {
            store
                .presences()
                .iter()
                .any(|p| p.session == "launcher" && p.unreachable_since.is_some())
        })
        .await;
    }

    #[tokio::test]
    async fn each_end_says_what_its_machine_is_which_stays_out_of_reach_once_the_link_drops() {
        let optimus = Memory::default();
        let equilibrius = Memory::default();
        equilibrius.start_here("walgit", json!({"session": "walgit"}), true);

        let (_near, far) = linked(&optimus, &equilibrius, 1 << 16);
        // A machine says what it is before its sessions, so once one is
        // carried, so is what it said.
        until(&optimus, |store| !store.presences().is_empty()).await;
        let said = optimus.machines();
        assert_eq!(
            said.iter()
                .map(|one| (one.name.as_str(), one.state))
                .collect::<Vec<_>>(),
            [("equilibrius", Readiness::Ready)],
            "the far machine is kept as ready while linked"
        );
        assert_eq!(
            said[0].arch,
            std::env::consts::ARCH,
            "as it measured itself"
        );

        far.abort();
        until(&optimus, |store| {
            store
                .presences()
                .iter()
                .all(|p| p.unreachable_since.is_some())
        })
        .await;
        assert_eq!(
            optimus
                .machines()
                .iter()
                .map(|one| (one.name.as_str(), one.state))
                .collect::<Vec<_>>(),
            [("equilibrius", Readiness::Unreachable)],
            "what it said stays, as of when the link last carried it"
        );
    }

    #[tokio::test]
    async fn an_older_end_hears_nothing_of_machines_and_none_speaks_for_another() {
        let optimus = Memory::default();
        optimus.start_here("launcher", json!({"session": "launcher"}), true);
        let (near, far) = tokio::io::duplex(1 << 16);
        let _optimus_end = end(&optimus, "optimus", near);
        let (far_reading, mut far_writing) = tokio::io::split(far);

        let older = Hello {
            facts: false,
            ..realm("equilibrius").hello()
        };
        send(&mut far_writing, &Frame::Hello(older))
            .await
            .expect("the far end says hello");
        let mut lines = BufReader::new(far_reading).lines();
        let mut heard = Vec::new();
        let waited = tokio::time::timeout(PATIENCE, async {
            while let Some(line) = lines.next_line().await.expect("the pipe reads") {
                let frame = serde_json::from_str::<Frame>(&line).expect("a frame");
                let presence = matches!(frame, Frame::Presence { .. });
                heard.push(frame);
                if presence {
                    return;
                }
            }
        })
        .await;
        assert!(waited.is_ok(), "optimus's session never crossed");
        assert!(
            !heard.iter().any(|frame| matches!(frame, Frame::Machine(_))),
            "an end that doesn't say it knows of machines hears none: {heard:?}"
        );

        let mut penelope = facts::measure("penelope", std::path::Path::new("/"), None);
        penelope.state = Readiness::Ready;
        for frame in [
            Frame::Have {
                messages: Vec::new(),
                living: Some(vec!["walgit".to_owned()]),
            },
            Frame::Machine(Box::new(penelope)),
            Frame::Presence {
                session: "walgit".to_owned(),
                body: json!({"session": "walgit"}),
                leases: None,
            },
        ] {
            send(&mut far_writing, &frame)
                .await
                .expect("the far end speaks");
        }
        until(&optimus, |store| {
            store.presences().iter().any(|p| p.session == "walgit")
        })
        .await;
        assert!(
            optimus.machines().is_empty(),
            "equilibrius can't say what penelope is"
        );
    }

    #[tokio::test]
    async fn a_sessions_changes_are_carried_with_it_and_go_when_it_ends() {
        let optimus = Memory::default();
        let equilibrius = Memory::default();
        let change = |subject: &str| {
            json!({"kind": "change", "subject": subject, "since": "s",
                   "until": "2999-01-01T00:00:00.000Z"})
        };
        let leases_of = |store: &Memory| -> Vec<Value> {
            store
                .holdings()
                .iter()
                .filter(|holding| holding["machine"] == "equilibrius")
                .flat_map(|holding| holding["leases"].as_array().cloned().unwrap_or_default())
                .collect()
        };
        equilibrius.start_here("walgit", json!({"session": "walgit"}), true);
        equilibrius.hold_here(
            "walgit",
            json!({"session": "walgit", "leases": [change("core.nix:flake.nix")]}),
        );

        let _link = linked(&optimus, &equilibrius, 1 << 16);
        until(&optimus, |store| leases_of(store).len() == 1).await;
        assert_eq!(
            leases_of(&optimus)[0]["subject"],
            "core.nix:flake.nix",
            "the Mac session's change is held here, marked as the Mac's"
        );

        equilibrius.hold_here("walgit", json!({"session": "walgit", "leases": []}));
        until(&optimus, |store| leases_of(store).is_empty()).await;

        equilibrius.hold_here(
            "walgit",
            json!({"session": "walgit", "leases": [change("mycelium:src/mesh.rs")]}),
        );
        until(&optimus, |store| leases_of(store).len() == 1).await;
        equilibrius.start_here("walgit", json!({"session": "walgit"}), false);
        until(&optimus, |store| store.holdings().is_empty()).await;
    }

    #[tokio::test]
    async fn a_link_coming_back_says_which_sessions_out_of_reach_still_run() {
        let optimus = Memory::default();
        let equilibrius = Memory::default();
        optimus.start_here("launcher", json!({"session": "launcher"}), true);
        optimus.start_here("done", json!({"session": "done"}), true);
        let (near, far) = linked(&optimus, &equilibrius, 1 << 16);
        until(&equilibrius, |store| store.presences().len() == 2).await;
        near.abort();
        far.await
            .expect("the Mac's end runs to its end")
            .expect("a dropped link ends quietly");
        assert!(
            equilibrius
                .presences()
                .iter()
                .all(|p| p.unreachable_since.is_some()),
            "both are out of reach: {:?}",
            equilibrius.presences()
        );

        optimus.start_here("done", json!({"session": "done"}), false);
        let _back = linked(&optimus, &equilibrius, 1 << 16);
        until(&equilibrius, |store| {
            matches!(
                store.presences().as_slice(),
                [one] if one.session == "launcher" && one.unreachable_since.is_none()
            )
        })
        .await;
    }

    #[tokio::test]
    async fn an_end_that_names_no_living_sessions_has_every_one_out_of_reach_let_go() {
        let optimus = Memory::default();
        optimus
            .put_presence("walgit", "equilibrius", &json!({"session": "walgit"}))
            .expect("a copy is kept");
        store::out_of_reach(&optimus, "equilibrius", now());
        let (near, far) = tokio::io::duplex(1 << 16);
        let _optimus_end = end(&optimus, "optimus", near);
        let (_far_reading, mut far_writing) = tokio::io::split(far);
        // As an end older than naming its living sessions says it.
        for line in [
            serde_json::to_string(&Frame::Hello(realm("equilibrius").hello())).expect("a frame"),
            r#"{"Have":{"messages":[]}}"#.to_owned(),
        ] {
            far_writing
                .write_all(format!("{line}\n").as_bytes())
                .await
                .expect("the far end sends");
        }
        until(&optimus, |store| store.presences().is_empty()).await;
    }

    #[tokio::test]
    async fn a_second_link_to_a_machine_already_linked_is_refused() {
        let optimus = Memory::default();
        let equilibrius = Memory::default();
        equilibrius.start_here("walgit", json!({"session": "walgit"}), true);
        let _first = linked(&optimus, &equilibrius, 1 << 16);
        until(&optimus, |store| !store.presences().is_empty()).await;

        let (near, far) = linked(&optimus, &equilibrius, 1 << 16);
        let refused = near.await.expect("the second link's end runs to its end");
        assert!(
            refused.is_err(),
            "optimus already carries the Mac by the first link"
        );
        drop(far);
        assert_eq!(
            optimus.presences().len(),
            1,
            "the first link's sessions stay"
        );
    }

    #[tokio::test]
    async fn another_realm_is_refused_before_anything_crosses() {
        let optimus = Memory::default();
        said(&optimus, &recent(0, "WCJQSG", 0), "@20KSI7");
        optimus.start_here("launcher", json!({"session": "launcher"}), true);
        let (near, far) = tokio::io::duplex(1 << 16);
        let optimus_end = end(&optimus, "optimus", near);

        let (far_reading, mut far_writing) = tokio::io::split(far);
        let personal = Hello {
            machine: "penelope".to_owned(),
            realm: "personal".to_owned(),
            version: VERSION,
            facts: true,
        };
        send(&mut far_writing, &Frame::Hello(personal))
            .await
            .expect("the far end says hello");
        let refused = optimus_end.await.expect("optimus's end runs to its end");
        assert!(refused.is_err(), "optimus refuses the personal realm");

        let mut heard = Vec::new();
        let mut lines = BufReader::new(far_reading).lines();
        while let Some(line) = lines.next_line().await.expect("the pipe reads") {
            heard.push(serde_json::from_str::<Frame>(&line).expect("a frame"));
        }
        assert!(
            matches!(heard.as_slice(), [Frame::Hello(_)]),
            "only optimus's hello crossed: {heard:?}"
        );
    }

    #[tokio::test]
    async fn a_record_the_store_refuses_is_skipped_and_the_link_carries_on() {
        let optimus = Memory::default();
        let (near, far) = tokio::io::duplex(1 << 16);
        let _optimus_end = end(&optimus, "optimus", near);
        let (_far_reading, mut far_writing) = tokio::io::split(far);

        let good = recent(0, "20KSI7", 0);
        let frames = [
            Frame::Hello(realm("equilibrius").hello()),
            Frame::Message {
                id: "../escape".to_owned(),
                body: json!({"id": "../escape"}),
            },
            Frame::Message {
                id: recent(0, "20KSI7", 1),
                body: json!({"id": "another"}),
            },
            Frame::Presence {
                session: "launcher".to_owned(),
                body: json!({"session": "not-launcher"}),
                leases: None,
            },
            Frame::Message {
                id: good.clone(),
                body: json!({"id": good, "to": ["@WCJQSG"]}),
            },
        ];
        for frame in &frames {
            send(&mut far_writing, frame)
                .await
                .expect("the far end sends");
        }
        until(&optimus, |store| store.message(&good).is_some()).await;
        assert_eq!(
            optimus.message_ids(0),
            [good],
            "only the good message was kept"
        );
        assert!(
            optimus.presences().is_empty(),
            "the presence naming another session was refused"
        );
    }

    #[tokio::test]
    async fn a_shared_quests_record_is_carried_both_ways() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let places = |machine: &str| Places {
            quests: dir.path().join(machine).join("quests"),
            scratch: dir.path().join(machine).join("scratch"),
            home: dir.path().join(machine).join("home"),
            state: dir.path().join(machine).join("state"),
        };
        let (here, there) = (places("optimus"), places("equilibrius"));
        let quest = "QEST-20261013-4UHC5Q";
        let ours = here.quests.join(quest).join("README.md");
        let theirs = there.quests.join(quest).join("README.md");
        std::fs::create_dir_all(here.quests.join(quest)).expect("the quest made");
        std::fs::write(&ours, "# Shared\n").expect("its README written");
        here.ledgers()
            .hold(quest)
            .expect("its ledger held")
            .keep()
            .expect("the quest shared");

        let (optimus, equilibrius) = (Memory::default(), Memory::default());
        let (near, far) = tokio::io::duplex(1 << 16);
        let _near = sharing_end(&optimus, "optimus", near, Some(here));
        let _far = sharing_end(&equilibrius, "equilibrius", far, Some(there));

        until_file(&theirs, "# Shared\n").await;
        std::fs::write(&theirs, "# Changed there\n").expect("changed on the far end");
        until_file(&ours, "# Changed there\n").await;
    }
}
