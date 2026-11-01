//! What this machine's node asks of another machine, carried to a link that
//! machine dialled. Only one link joins two machines, and whichever dialled
//! first holds it. A link another machine dialled runs in a process SSH
//! started, not in the node, so it asks the node over the node's socket for
//! what the node would ask of that machine, and answers each as the other
//! end does. The node then reaches that machine whichever end dialled.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;

use super::starts::{Ask, Asked, Asks};
use crate::steer::Steer;
use crate::wire::{Reply, Request, Response, Start};

/// How long a link waits before asking its node again once it can't reach
/// it, as while the node restarts.
const ASKED_AGAIN_AFTER: Duration = Duration::from_secs(5);

/// Something the node asks of the other machine, as it crosses the socket.
#[derive(Debug, Serialize, Deserialize)]
enum Carried {
    Start(Start),
    Steer {
        address: String,
        steer: Map<String, Value>,
    },
    Update,
}

impl From<Asked> for Carried {
    fn from(asked: Asked) -> Self {
        match asked {
            Asked::Start(start) => Self::Start(start),
            Asked::Steer(steer) => Self::Steer {
                address: steer.address,
                steer: steer.body,
            },
            Asked::Update => Self::Update,
        }
    }
}

impl From<Carried> for Asked {
    fn from(carried: Carried) -> Self {
        match carried {
            Carried::Start(start) => Self::Start(start),
            Carried::Steer { address, steer } => Self::Steer(Steer {
                address,
                body: steer,
            }),
            Carried::Update => Self::Update,
        }
    }
}

/// An ask the node hands the link, numbered so its answer can say which.
#[derive(Debug, Serialize, Deserialize)]
struct Handed {
    ask: u64,
    asked: Carried,
}

/// The link's answer to the ask of that number, as the other end gave it.
#[derive(Debug, Serialize, Deserialize)]
struct Answered {
    ask: u64,
    reply: Reply,
}

/// Hands what the node asks of `machine` to the link at the other end of a
/// connection, one line each, and gives each ask the answer the link sends
/// back, until the connection closes or another link reaching `machine`
/// takes its place. An ask still waiting then hears that the link dropped.
pub async fn to_link<R, W>(asks: &Asks, machine: &str, mut lines: Lines<R>, mut writing: W)
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if write(&mut writing, &Reply::Ok(Response::Carrying))
        .await
        .is_err()
    {
        return;
    }
    let (mut asked, _reaching) = asks.reach(machine, asks.way(machine));
    let mut unanswered = HashMap::new();
    let mut next = 0;
    loop {
        tokio::select! {
            ask = asked.recv() => {
                // None once another link reaching the machine has taken
                // this one's place.
                let Some(Ask { asked, answer }) = ask else { return };
                let handed = Handed { ask: next, asked: asked.into() };
                if write(&mut writing, &handed).await.is_err() {
                    return;
                }
                unanswered.insert(next, answer);
                next += 1;
            }
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { return };
                // A line that isn't an answer answers nothing.
                if let Ok(Answered { ask, reply }) = serde_json::from_str(&line)
                    && let Some(answer) = unanswered.remove(&ask)
                {
                    // The asker may have given up waiting; then nobody hears it.
                    let _ = answer.send(reply);
                }
            }
        }
    }
}

/// A link's asking of its node, until it is dropped.
pub(super) struct Carrying(tokio::task::JoinHandle<()>);

impl Drop for Carrying {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What this machine's node at `node` asks of `machine`, for a link
/// `machine` dialled: asked of the node over its socket for as long as
/// what this answers is held, and asked again whenever the node can't be
/// reached, as while it restarts.
pub(super) fn from_node(node: PathBuf, machine: &str) -> (mpsc::UnboundedReceiver<Ask>, Carrying) {
    let (asks, asked) = mpsc::unbounded_channel();
    let machine = machine.to_owned();
    let task = tokio::spawn(async move {
        let mut said = None;
        loop {
            // Each new reason is said once, so a node that stays away for
            // long fills no log.
            if let Err(error) = carry(&node, &machine, &asks).await {
                let why = error.to_string();
                if said.as_ref() != Some(&why) {
                    eprintln!(
                        "mycelium: this link can't carry its node's asks of {machine}: {why}"
                    );
                    said = Some(why);
                }
            }
            tokio::time::sleep(ASKED_AGAIN_AFTER).await;
        }
    });
    (asked, Carrying(task))
}

/// Asks the node at `node` for what it asks of `machine`, sending each to
/// the link through `asks` and each answer back, until the node hangs up.
async fn carry(node: &Path, machine: &str, asks: &mpsc::UnboundedSender<Ask>) -> io::Result<()> {
    let (reading, mut writing) = UnixStream::connect(node).await?.into_split();
    let mut lines = BufReader::new(reading).lines();
    let carry = Request::Carry {
        machine: machine.to_owned(),
    };
    write(&mut writing, &carry).await?;
    let first = lines
        .next_line()
        .await?
        .ok_or_else(|| io::Error::other("the node hung up"))?;
    match serde_json::from_str::<Reply>(&first) {
        Ok(Ok(Response::Carrying)) => {}
        Ok(Err(why)) => return Err(io::Error::other(why)),
        _ => return Err(io::Error::other(format!("the node answered {first}"))),
    }
    let mut answering = JoinSet::new();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()) };
                let Handed { ask, asked } = serde_json::from_str(&line)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                let (answer, answered) = oneshot::channel();
                if asks.send(Ask { asked: asked.into(), answer }).is_err() {
                    // The link has ended, and its asking ends with it.
                    return Ok(());
                }
                answering.spawn(async move {
                    let reply = answered
                        .await
                        .unwrap_or_else(|_| Err("the link dropped before it answered".to_owned()));
                    Answered { ask, reply }
                });
            }
            Some(answered) = answering.join_next() => {
                write(&mut writing, &answered.map_err(io::Error::other)?).await?;
            }
        }
    }
}

/// Writes `said` as one JSON line, in a single write so no reader sees part
/// of it.
async fn write(writing: &mut (impl AsyncWrite + Unpin), said: &impl Serialize) -> io::Result<()> {
    let mut line = serde_json::to_string(said).map_err(io::Error::other)?;
    line.push('\n');
    writing.write_all(line.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_runs_on_a_machine_as_the_dial_that_reached_it_says_or_else_over_ssh_by_name() {
        let asks = Asks::default();
        let words = ["mycelium".to_owned(), "attach".to_owned()];
        asks.know(
            "penelope",
            std::sync::Arc::new(|words: &[String]| {
                let mut there = vec!["into-penelope".to_owned()];
                there.extend_from_slice(words);
                there
            }),
        );
        assert_eq!(
            asks.way("penelope")(&words),
            ["into-penelope", "mycelium", "attach"],
            "a machine the node dialled is reached as its dial says"
        );
        assert_eq!(
            asks.way("optimus")(&words),
            ["ssh", "-t", "--", "optimus", "mycelium", "attach"],
            "one it never dialled is reached over SSH by its name"
        );
    }
}
