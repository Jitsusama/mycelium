//! A link: two machines' stores kept as one, over any byte stream.
//!
//! Both ends run the same code. Each says who it is ([`membership`]), then
//! which messages it holds; each then sends the other what it lacks, and
//! from there every new message as it lands. Messages are written once by
//! id, so holding the union of both ends is always right and nothing needs
//! agreeing. Each end also sends its own living sessions' presence, which
//! the other keeps until the session ends or the link drops, since only a
//! session's own machine can say whether it lives.
//!
//! How an end is reached is [`dial`]'s, and which ends to reach is
//! [`discovery`]'s.

pub mod dial;
pub mod discovery;
pub mod membership;

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::store::{Change, Store};
use dial::Connection;
use membership::{Hello, Membership};

/// How far back a link reaches for messages: the mesh's own week, so a link
/// never hands back a message the other end has already let go.
const REACH: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How often this end's sessions are judged again, since a session ending
/// changes its process, not its presence.
const LIVENESS_EVERY: Duration = Duration::from_secs(5);

/// One line on a link.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Frame {
    /// Who this end is; always first.
    Hello(Hello),
    /// The ids of every message this end holds within reach.
    Have {
        /// Their ids.
        messages: Vec<String>,
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
    },
    /// A session of this end's machine has ended.
    Ended {
        /// Its id.
        session: String,
    },
}

/// Runs this end of a link until the other end leaves, then lets go of the
/// presence of every session it carried here.
pub async fn run(
    store: impl Store,
    membership: &impl Membership,
    connection: Connection,
) -> io::Result<()> {
    let Connection {
        reading,
        mut writing,
        keep: _keep,
    } = connection;
    let mut changes = store.changes()?;
    let mut lines = BufReader::new(reading).lines();
    let mut end = End {
        store,
        other: None,
        has: HashSet::new(),
        sent: BTreeMap::new(),
    };

    send(&mut writing, &Frame::Hello(membership.hello())).await?;
    let first = lines.next_line().await?;
    let Some(Frame::Hello(other)) = first
        .as_deref()
        .map(serde_json::from_str::<Frame>)
        .transpose()?
    else {
        return Err(refusal("the other end didn't say hello first".to_owned()));
    };
    membership.admit(&other).map_err(refusal)?;
    end.other = Some(other.machine);
    // What an earlier link to that machine left behind, if it ended without
    // letting go, is stale: the other end sends its living sessions afresh.
    end.let_go();

    let messages = end.store.message_ids(reach_start());
    send(&mut writing, &Frame::Have { messages }).await?;
    end.send_presence(&mut writing).await?;

    let mut tick = tokio::time::interval(LIVENESS_EVERY);
    let result = loop {
        let step = tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => match serde_json::from_str(&line) {
                    Ok(frame) => end.take(frame, &mut writing).await,
                    Err(error) => Err(io::Error::new(io::ErrorKind::InvalidData, error)),
                },
                Ok(None) => break Ok(()),
                Err(error) => Err(error),
            },
            _ = tick.tick() => end.send_presence(&mut writing).await,
            change = changes.changes.recv() => match change {
                Some(Change::Message(id)) => end.send_messages(vec![id], &mut writing).await,
                Some(Change::Presence) => end.send_presence(&mut writing).await,
                None => break Ok(()),
            },
        };
        if let Err(error) = step {
            break Err(error);
        }
    };
    end.let_go();
    result
}

/// This end of a link, and what it knows of the other.
struct End<S> {
    store: S,
    other: Option<String>,
    has: HashSet<String>,
    sent: BTreeMap<String, Value>,
}

impl<S: Store> End<S> {
    async fn take(
        &mut self,
        frame: Frame,
        writing: &mut (impl AsyncWrite + Unpin),
    ) -> io::Result<()> {
        match frame {
            Frame::Hello(_) => return Err(refusal("the other end said hello twice".to_owned())),
            Frame::Have { messages } => {
                self.has.extend(messages);
                let ids = self.store.message_ids(reach_start());
                self.send_messages(ids, writing).await?;
            }
            Frame::Message { id, body } => {
                self.has.insert(id.clone());
                self.store.put_message(&id, &body)?;
            }
            Frame::Presence { session, body } => {
                let machine = self.other.as_deref().unwrap_or_default();
                self.store.put_presence(&session, machine, body)?;
            }
            Frame::Ended { session } => self.store.remove_presence(&session)?,
        }
        Ok(())
    }

    /// Sends the messages among `ids` the other end doesn't hold yet.
    async fn send_messages(
        &mut self,
        ids: Vec<String>,
        writing: &mut (impl AsyncWrite + Unpin),
    ) -> io::Result<()> {
        for id in ids {
            if self.has.contains(&id) {
                continue;
            }
            if let Some(body) = self.store.message(&id) {
                send(
                    writing,
                    &Frame::Message {
                        id: id.clone(),
                        body,
                    },
                )
                .await?;
                self.has.insert(id);
            }
        }
        Ok(())
    }

    /// Sends what changed in this machine's living sessions since last sent.
    async fn send_presence(&mut self, writing: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        let living: BTreeMap<String, Value> = self
            .store
            .presences()
            .into_iter()
            .filter(|presence| presence.elsewhere.is_none() && presence.lives)
            .map(|presence| (presence.session, presence.body))
            .collect();
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
        for (session, body) in living {
            if self.sent.get(&session) != Some(&body) {
                send(
                    writing,
                    &Frame::Presence {
                        session: session.clone(),
                        body: body.clone(),
                    },
                )
                .await?;
                self.sent.insert(session, body);
            }
        }
        Ok(())
    }

    /// Lets go of the presence of every session the other end carried here.
    fn let_go(&self) {
        let Some(other) = &self.other else { return };
        for presence in self.store.presences() {
            if presence.elsewhere.as_ref() == Some(other) {
                // Letting go is tidying: a presence left behind is let go
                // as the next link to that machine starts.
                let _ = self.store.remove_presence(&presence.session);
            }
        }
    }
}

async fn send(writing: &mut (impl AsyncWrite + Unpin), frame: &Frame) -> io::Result<()> {
    let mut line = serde_json::to_vec(frame).map_err(io::Error::other)?;
    line.push(b'\n');
    writing.write_all(&line).await?;
    writing.flush().await
}

/// When the link's reach starts, in milliseconds since the epoch.
fn reach_start() -> i64 {
    let reach = i64::try_from(REACH.as_millis()).unwrap_or(i64::MAX);
    jiff::Timestamp::now()
        .as_millisecond()
        .saturating_sub(reach)
}

fn refusal(reason: String) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, reason)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::link::membership::Realm;
    use crate::store::memory::Memory;

    /// How long a test waits for a link to carry something before failing.
    const PATIENCE: Duration = Duration::from_secs(5);

    fn realm(machine: &str) -> Realm {
        Realm {
            name: "work".to_owned(),
            machine: machine.to_owned(),
        }
    }

    /// Two stores joined by a link over an in-memory pipe.
    fn linked(optimus: &Memory, equilibrius: &Memory) -> tokio::task::JoinHandle<io::Result<()>> {
        let (near, far) = tokio::io::duplex(1 << 16);
        let (near_reading, near_writing) = tokio::io::split(near);
        let (far_reading, far_writing) = tokio::io::split(far);
        let far_end = equilibrius.clone();
        tokio::spawn(async move {
            let connection = Connection {
                reading: Box::new(far_reading),
                writing: Box::new(far_writing),
                keep: Box::new(()),
            };
            run(far_end, &realm("equilibrius"), connection).await
        });
        let near_end = optimus.clone();
        tokio::spawn(async move {
            let connection = Connection {
                reading: Box::new(near_reading),
                writing: Box::new(near_writing),
                keep: Box::new(()),
            };
            run(near_end, &realm("optimus"), connection).await
        })
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

    fn recent(ms_ago: i64, from: &str) -> String {
        format!(
            "{}-{from}-1-0",
            jiff::Timestamp::now().as_millisecond() - ms_ago
        )
    }

    #[tokio::test]
    async fn each_end_gets_the_messages_it_lacked_and_each_new_one() {
        let optimus = Memory::default();
        let equilibrius = Memory::default();
        let before = recent(1000, "WCJQSG");
        let theirs = recent(900, "20KSI7");
        let stale = format!("{}-OLD-1-0", reach_start() - 1);
        optimus.send_here(&before, json!({"text": "from optimus"}));
        optimus.send_here(&stale, json!({"text": "let go a week ago"}));
        equilibrius.send_here(&theirs, json!({"text": "from the Mac"}));

        let _link = linked(&optimus, &equilibrius);
        until(&equilibrius, |store| store.message(&before).is_some()).await;
        until(&optimus, |store| store.message(&theirs).is_some()).await;

        let after = recent(0, "4UHC5Q");
        optimus.send_here(&after, json!({"text": "said while linked"}));
        until(&equilibrius, |store| store.message(&after).is_some()).await;
        assert_eq!(
            equilibrius.message(&stale),
            None,
            "a message past reach stays behind"
        );
    }

    #[tokio::test]
    async fn a_living_session_is_carried_and_let_go_when_it_ends_or_the_link_drops() {
        let optimus = Memory::default();
        let equilibrius = Memory::default();
        equilibrius.start_here("walgit", json!({"session": "walgit"}), true);
        equilibrius.start_here("gone", json!({"session": "gone"}), false);

        let link = linked(&optimus, &equilibrius);
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
        // end sees the link drop and lets go of what optimus carried.
        link.abort();
        until(&equilibrius, |store| {
            store.presences().iter().all(|p| p.elsewhere.is_none())
        })
        .await;
    }

    #[tokio::test]
    async fn another_realm_is_refused_before_anything_crosses() {
        let optimus = Memory::default();
        optimus.send_here(&recent(0, "WCJQSG"), json!({"text": "work only"}));
        let (near, far) = tokio::io::duplex(1 << 16);
        let (far_reading, far_writing) = tokio::io::split(far);
        let (near_reading, near_writing) = tokio::io::split(near);
        let penelope = Realm {
            name: "personal".to_owned(),
            machine: "penelope".to_owned(),
        };
        let personal = Memory::default();
        let far_end = personal.clone();
        let far_task = tokio::spawn(async move {
            let connection = Connection {
                reading: Box::new(far_reading),
                writing: Box::new(far_writing),
                keep: Box::new(()),
            };
            run(far_end, &penelope, connection).await
        });
        let connection = Connection {
            reading: Box::new(near_reading),
            writing: Box::new(near_writing),
            keep: Box::new(()),
        };
        let near = run(optimus, &realm("optimus"), connection).await;
        assert!(near.is_err(), "optimus refuses the personal realm");
        let far = far_task.await.expect("the far end runs to its end");
        assert!(far.is_err(), "the personal realm refuses optimus");
        assert!(personal.message_ids(0).is_empty(), "nothing crossed");
    }
}
