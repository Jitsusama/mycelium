//! What this machine's node asks of another machine, carried to a link that
//! machine dialled. Only one link joins two machines, and whichever dialled
//! first holds it. A link another machine dialled runs in a process SSH
//! started, not in the node, so it asks the node over the node's socket for
//! what the node would ask of that machine, and answers each as the other
//! end does. The node then reaches that machine whichever end dialled.
//!
//! A run isn't carried: its output would hold up every other ask of that
//! machine behind a reader that has stopped reading, so a command runs
//! only on a machine this one's node dialled.
//!
//! A lend is carried, since a machine borrows its model calls over the
//! link the lender dialled. Its answer comes back a line a piece, and the
//! node never waits on a lend's reader to read the next line: a reader
//! [`STALLED_AFTER`] pieces behind has its lend let go, as one that hangs
//! up does, so no lend holds up the other asks of that machine.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::{AbortHandle, JoinSet};

use super::starts::{Ask, Asked, Asks};
use crate::lend::{BACKLOG, Lend, Lent};
use crate::steer::Steer;
use crate::wire::{Reply, Request, Response, Start};

/// How long a link waits before asking its node again once it can't reach
/// it, as while the node restarts: short, since until it asks again the
/// node reads its machine ready with no link to ask it through, and a
/// missing socket costs nothing to try.
const ASKED_AGAIN_AFTER: Duration = Duration::from_millis(500);

/// How long a link waits for its node to answer that it carries the
/// link's asks, before it hangs up and asks again: a node that took the
/// call and never answers would otherwise hold the asks for good.
const ANSWERED_WITHIN: Duration = Duration::from_secs(5);

/// How many pieces of a lend's answer the node holds for a reader that
/// has stopped taking them, before it lets the lend go. A streamed answer
/// comes in many small pieces, a few bytes each, so a reader busy for a
/// moment must not count as one that stopped: at most 16 KiB each, these
/// hold 16 MiB at the very most.
const STALLED_AFTER: usize = 1024;

/// Something the node asks of the other machine, as it crosses the socket.
#[derive(Debug, Serialize, Deserialize)]
enum Carried {
    Start(Start),
    Reopen(Start),
    Steer {
        address: String,
        steer: Map<String, Value>,
        #[serde(default)]
        checked: bool,
    },
    Update,
    Carry {
        quest: String,
        to: String,
        #[serde(default)]
        checked: bool,
    },
    End {
        held: String,
        by: crate::hold::end::Asker,
    },
    Lend(Lend),
}

impl Carried {
    /// What the node asks, as it crosses the socket, with where a lend's
    /// answer goes; or why it can't cross.
    fn of(asked: Asked) -> Result<(Self, Option<mpsc::Sender<Lent>>), String> {
        let carried = match asked {
            Asked::Start(start) => Self::Start(start),
            Asked::Reopen(start) => Self::Reopen(start),
            Asked::Steer(steer) => Self::Steer {
                address: steer.address,
                steer: steer.body,
                checked: steer.checked,
            },
            Asked::Update => Self::Update,
            Asked::Carry { quest, to, checked } => Self::Carry { quest, to, checked },
            Asked::End { held, by } => Self::End { held, by },
            Asked::Lend(lend, answers) => return Ok((Self::Lend(lend), Some(answers))),
            Asked::Run(..) => {
                return Err("it dialled this machine, and a run goes only where this \
                            machine dialled"
                    .to_owned());
            }
        };
        Ok((carried, None))
    }

    /// What the link asks of the other machine, with where a lend's answer
    /// comes as the link hands it on.
    fn asked(self) -> (Asked, Option<mpsc::Receiver<Lent>>) {
        let asked = match self {
            Self::Start(start) => Asked::Start(start),
            Self::Reopen(start) => Asked::Reopen(start),
            Self::Steer {
                address,
                steer,
                checked,
            } => Asked::Steer(Steer {
                address,
                body: steer,
                checked,
            }),
            Self::Update => Asked::Update,
            Self::Carry { quest, to, checked } => Asked::Carry { quest, to, checked },
            Self::End { held, by } => Asked::End { held, by },
            Self::Lend(lend) => {
                let (sending, answers) = mpsc::channel(BACKLOG);
                return (Asked::Lend(lend, sending), Some(answers));
            }
        };
        (asked, None)
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

/// A line the node writes to the link.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum ToLink {
    Handed(Handed),
    /// Let the lend of this ask go, as its reader hung up or stalled.
    Unlend {
        unlend: u64,
    },
}

/// A line the link writes to the node.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum FromLink {
    Answered(Answered),
    /// A piece of the answer to the lend of this ask.
    Lent {
        ask: u64,
        lent: Lent,
    },
}

/// Hands what the node asks of `machine` to the link at the other end of a
/// connection, one line each, and gives each ask the answer the link sends
/// back, until the connection closes or another link reaching `machine`
/// takes its place. An ask still waiting then hears that the link dropped,
/// and a lend still answering ends short.
///
/// A link that doesn't say it carries a reopening (`reopens`) is older
/// than reopenings and couldn't read one, so each is answered here
/// instead, rather than handed to it.
pub async fn to_link<R, W>(
    asks: &Asks,
    (machine, reopens): (&str, bool),
    mut lines: Lines<R>,
    mut writing: W,
) where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // Reached before the link hears it is carried, so a link that waits
    // for that before saying its machine's facts never has the node read
    // the machine ready with no link to ask it through.
    let (mut asked, _reaching) = asks.reach(machine, asks.way(machine));
    if write(&mut writing, &Reply::Ok(Response::Carrying))
        .await
        .is_err()
    {
        return;
    }
    let mut unanswered = HashMap::new();
    // Where each lend's pieces go, while it answers.
    let mut lending = HashMap::new();
    let (gone, mut let_go) = mpsc::unbounded_channel();
    let mut next = 0;
    loop {
        tokio::select! {
            ask = asked.recv() => {
                // None once another link reaching the machine has taken
                // this one's place.
                let Some(Ask { asked, answer }) = ask else { return };
                let (asked, lent) = match Carried::of(asked) {
                    Ok((Carried::Reopen(_), _)) if !reopens => {
                        // The asker may have given up waiting.
                        let _ = answer.send(Err(format!(
                            "{machine}'s link here is too old to carry a reopening"
                        )));
                        continue;
                    }
                    Ok(carried) => carried,
                    Err(why) => {
                        // The asker may have given up waiting.
                        let _ = answer.send(Err(format!("{machine} isn't asked that: {why}")));
                        continue;
                    }
                };
                let handed = ToLink::Handed(Handed { ask: next, asked });
                if write(&mut writing, &handed).await.is_err() {
                    return;
                }
                unanswered.insert(next, answer);
                if let Some(lent) = lent {
                    lending.insert(next, handing_on(next, lent, gone.clone()));
                }
                next += 1;
            }
            Some(ask) = let_go.recv() => {
                // A lend that ended meanwhile has nothing to let go.
                if lending.remove(&ask).is_some()
                    && write(&mut writing, &ToLink::Unlend { unlend: ask }).await.is_err()
                {
                    return;
                }
            }
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { return };
                // A line that is neither answers nothing.
                match serde_json::from_str(&line) {
                    Ok(FromLink::Answered(Answered { ask, reply })) => {
                        // A lend refused there lends nothing to hand on.
                        if reply.is_err() {
                            lending.remove(&ask);
                        }
                        if let Some(answer) = unanswered.remove(&ask) {
                            // The asker may have given up waiting; then
                            // nobody hears it.
                            let _ = answer.send(reply);
                        }
                    }
                    Ok(FromLink::Lent { ask, lent }) => {
                        let Some(handing) = lending.get(&ask) else { continue };
                        let ends = matches!(lent, Lent::Done | Lent::Refused { .. });
                        if handing.try_send(lent).is_ok() {
                            if ends {
                                lending.remove(&ask);
                            }
                        } else {
                            // A reader that fell behind, or hung up, has
                            // its lend let go there; what it was handed
                            // ends short.
                            lending.remove(&ask);
                            if write(&mut writing, &ToLink::Unlend { unlend: ask })
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                    Err(_) => {}
                }
            }
        }
    }
}

/// Hands the pieces the link lends for `ask` on to `lent` as its reader
/// takes them, telling `gone` once that reader hangs up. Answers where
/// the node puts each piece, which holds [`STALLED_AFTER`] at most, so a
/// reader that stops reading never holds up the node's other asks.
fn handing_on(
    ask: u64,
    lent: mpsc::Sender<Lent>,
    gone: mpsc::UnboundedSender<u64>,
) -> mpsc::Sender<Lent> {
    let (handing, mut held) = mpsc::channel(STALLED_AFTER);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                piece = held.recv() => {
                    // None once the lend ended, or was let go.
                    let Some(piece) = piece else { return };
                    if lent.send(piece).await.is_err() {
                        break;
                    }
                }
                () = lent.closed() => break,
            }
        }
        // The connection may have closed; then there is nothing to let go.
        let _ = gone.send(ask);
    });
    handing
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
/// reached, as while it restarts. The last it answers hears once the node
/// first takes the asks, or the first asking fails: a node that can't be
/// reached places no start to miss them.
pub(super) fn from_node(
    node: PathBuf,
    machine: &str,
) -> (
    mpsc::UnboundedReceiver<Ask>,
    Carrying,
    oneshot::Receiver<()>,
) {
    let (asks, asked) = mpsc::unbounded_channel();
    let (taken, first_taken) = oneshot::channel();
    let machine = machine.to_owned();
    let task = tokio::spawn(async move {
        let mut said = None;
        let mut taken = Some(taken);
        loop {
            // Each new reason is said once, so a node that stays away for
            // long fills no log.
            let carried = carry(&node, &machine, &asks, &mut taken).await;
            if let Some(taken) = taken.take() {
                let _ = taken.send(());
            }
            if let Err(error) = carried {
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
    (asked, Carrying(task), first_taken)
}

/// Asks the node at `node` for what it asks of `machine`, sending each to
/// the link through `asks` and each answer back, until the node hangs up,
/// telling `taken`, if it is still there, once the node takes them.
async fn carry(
    node: &Path,
    machine: &str,
    asks: &mpsc::UnboundedSender<Ask>,
    taken: &mut Option<oneshot::Sender<()>>,
) -> io::Result<()> {
    let (reading, mut writing) = UnixStream::connect(node).await?.into_split();
    let mut lines = BufReader::new(reading).lines();
    let carry = Request::Carry {
        machine: machine.to_owned(),
        reopens: true,
    };
    write(&mut writing, &carry).await?;
    let first = tokio::time::timeout(ANSWERED_WITHIN, lines.next_line())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the node didn't answer"))??
        .ok_or_else(|| io::Error::other("the node hung up"))?;
    match serde_json::from_str::<Reply>(&first) {
        Ok(Ok(Response::Carrying)) => {
            if let Some(taken) = taken.take() {
                // A link that no longer waits for it has nothing to hear.
                let _ = taken.send(());
            }
        }
        Ok(Err(why)) => return Err(io::Error::other(why)),
        _ => return Err(io::Error::other(format!("the node answered {first}"))),
    }
    // The node's lines are read apart from this end's writing, so a write
    // the node is slow to take never stops what the node writes being
    // read: a lend handed over while another's pieces stream back would
    // otherwise leave both ends writing to a full socket, for ever.
    let (heard, mut hearing) = mpsc::unbounded_channel();
    let mut listening = JoinSet::new();
    listening.spawn(async move {
        loop {
            let line = lines.next_line().await;
            let ended = !matches!(line, Ok(Some(_)));
            if heard.send(line).is_err() || ended {
                return;
            }
        }
    });
    let mut answering = JoinSet::new();
    // Each lend's reading, which ends as the node hangs up.
    let mut reading = JoinSet::new();
    let mut lends: HashMap<u64, AbortHandle> = HashMap::new();
    let (lending, mut pieces) = mpsc::channel(BACKLOG);
    loop {
        tokio::select! {
            line = hearing.recv() => {
                let Some(Some(line)) = line.transpose()? else { return Ok(()) };
                let said = serde_json::from_str(&line)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                let Handed { ask, asked } = match said {
                    ToLink::Handed(handed) => handed,
                    ToLink::Unlend { unlend } => {
                        // Dropping its reader lets the lend go there.
                        if let Some(reader) = lends.remove(&unlend) {
                            reader.abort();
                        }
                        continue;
                    }
                };
                let (asked, answers) = asked.asked();
                let (answer, answered) = oneshot::channel();
                if asks.send(Ask { asked, answer }).is_err() {
                    // The link has ended, and its asking ends with it.
                    return Ok(());
                }
                if let Some(answers) = answers {
                    let reader = reading.spawn(read_lend(ask, answers, lending.clone()));
                    lends.insert(ask, reader);
                }
                answering.spawn(async move {
                    let reply = answered
                        .await
                        .unwrap_or_else(|_| Err("the link dropped before it answered".to_owned()));
                    Answered { ask, reply }
                });
            }
            Some(answered) = answering.join_next() => {
                let answered = FromLink::Answered(answered.map_err(io::Error::other)?);
                write(&mut writing, &answered).await?;
            }
            Some((ask, piece)) = pieces.recv() => match piece {
                Some(lent) => write(&mut writing, &FromLink::Lent { ask, lent }).await?,
                None => {
                    lends.remove(&ask);
                }
            },
            // A lend's reading that ended, or was let go, has nothing more.
            Some(_) = reading.join_next() => {}
        }
    }
}

/// Hands each piece the link lends for `ask` to `lending`, a line each
/// for the node, waiting while the node's connection is behind, then that
/// the lend ended.
async fn read_lend(
    ask: u64,
    mut answers: mpsc::Receiver<Lent>,
    lending: mpsc::Sender<(u64, Option<Lent>)>,
) {
    while let Some(piece) = answers.recv().await {
        if lending.send((ask, Some(piece))).await.is_err() {
            return;
        }
    }
    // The connection may have closed; then nobody needs telling.
    let _ = lending.send((ask, None)).await;
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
    use std::pin::Pin;
    use std::sync::{Arc, OnceLock};
    use std::task::{Context, Poll};

    use tokio::io::{DuplexStream, ReadHalf, WriteHalf};

    use super::*;

    /// A node's writing to a link, noting at its first write whether the
    /// node then reached tern through the link.
    struct Watched {
        asks: Asks,
        writing: WriteHalf<DuplexStream>,
        reached: Arc<OnceLock<bool>>,
    }

    impl AsyncWrite for Watched {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            let reached = self.asks.there("tern").is_some();
            self.reached.get_or_init(|| reached);
            Pin::new(&mut self.writing).poll_write(cx, bytes)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.writing).poll_flush(cx)
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.writing).poll_shutdown(cx)
        }
    }

    /// A bare reopening of `s-gone`, asked of no machine in particular.
    fn reopening() -> Start {
        Start {
            work: crate::placement::Work {
                needs: Vec::new(),
                prefers: Vec::new(),
                tolerates: Vec::new(),
                may_wait: false,
                lasts: None,
                fence: None,
            },
            folder: None,
            strict: false,
            command: vec!["pi".to_owned(), "--session".to_owned(), "s-gone".to_owned()],
            may_cost: false,
            carries: false,
            once: None,
        }
    }

    // Our council on the reopen rule (2026-11-19): a link process older
    // than reopenings couldn't read one, so its node answers it instead.
    #[tokio::test]
    async fn a_link_that_doesnt_say_it_carries_a_reopening_is_handed_none() {
        let said = serde_json::to_string(&Request::Carry {
            machine: "kestrel".to_owned(),
            reopens: true,
        })
        .expect("a carry is said")
        .replace(",\"reopens\":true", "");
        assert!(
            matches!(
                serde_json::from_str::<Request>(&said),
                Ok(Request::Carry { reopens: false, .. })
            ),
            "a link that doesn't say it carries one carries none: {said}"
        );
        let asks = Asks::default();
        let (node_end, link_end) = tokio::io::duplex(4096);
        let (reading, writing) = tokio::io::split(node_end);
        let carrying = tokio::spawn({
            let asks = asks.clone();
            async move {
                to_link(
                    &asks,
                    ("kestrel", false),
                    BufReader::new(reading).lines(),
                    writing,
                )
                .await;
            }
        });
        let mut link = BufReader::new(link_end).lines();
        assert!(
            link.next_line().await.is_ok_and(|said| said.is_some()),
            "the link is told it carries"
        );
        assert_eq!(
            asks.reopen("kestrel", reopening()).await,
            Ok(Err(
                "kestrel's link here is too old to carry a reopening".to_owned()
            )),
            "a reopening is answered here, never handed on"
        );
        let started = tokio::spawn({
            let asks = asks.clone();
            async move { asks.ask("kestrel", reopening()).await }
        });
        let handed = link
            .next_line()
            .await
            .expect("the link reads on")
            .expect("a start is handed on");
        assert!(
            handed.contains("\"Start\"") && !handed.contains("\"Reopen\""),
            "any other start is still handed on: {handed}"
        );
        started.abort();
        carrying.abort();
    }

    /// Longer than any of these waits should take, so a test fails rather
    /// than hangs.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// A model call, as the node asks it.
    fn lend() -> Lend {
        Lend {
            budget: None,
            method: "POST".to_owned(),
            path: "/apis/anthropic/v1/messages".to_owned(),
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: "e30=".to_owned(),
        }
    }

    /// The link's end of a connection a node carries its asks of tern
    /// across, once the node said it carries them.
    struct Link {
        lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
        writing: WriteHalf<DuplexStream>,
    }

    impl Link {
        async fn carried_by(asks: &Asks) -> Self {
            let (node, link) = tokio::io::duplex(64 * 1024);
            let (reading, writing) = tokio::io::split(node);
            let node_asks = asks.clone();
            let reached = Arc::new(OnceLock::new());
            let watched = Watched {
                asks: asks.clone(),
                writing,
                reached: Arc::clone(&reached),
            };
            tokio::spawn(async move {
                to_link(
                    &node_asks,
                    ("tern", true),
                    BufReader::new(reading).lines(),
                    watched,
                )
                .await;
            });
            let (reading, writing) = tokio::io::split(link);
            let mut link = Self {
                lines: BufReader::new(reading).lines(),
                writing,
            };
            let first = link.heard().await;
            assert!(first.contains("Carrying"), "the node carries: {first}");
            assert_eq!(
                reached.get(),
                Some(&true),
                "the node reaches tern through the link before it says it carries"
            );
            link
        }

        async fn heard(&mut self) -> String {
            tokio::time::timeout(PATIENCE, self.lines.next_line())
                .await
                .expect("the node says it in time")
                .expect("the node's connection holds")
                .expect("the node stays")
        }

        async fn say(&mut self, said: &FromLink) {
            write(&mut self.writing, said)
                .await
                .expect("the link says it");
        }

        /// Takes the lend the node hands it, answering that it is lent.
        async fn lent(&mut self) -> u64 {
            let line = self.heard().await;
            let Ok(ToLink::Handed(Handed {
                ask,
                asked: Carried::Lend(asked),
            })) = serde_json::from_str(&line)
            else {
                panic!("the node handed what isn't a lend: {line}");
            };
            assert_eq!(asked, lend(), "the lend crosses as it was asked");
            let reply = Ok(Response::Carrying);
            self.say(&FromLink::Answered(Answered { ask, reply })).await;
            ask
        }
    }

    /// Asks tern for a lend through `asks`, answering what it lends.
    async fn borrowed(asks: &Asks, link: &mut Link) -> (u64, mpsc::Receiver<Lent>) {
        let asking = {
            let asks = asks.clone();
            tokio::spawn(async move { asks.lend("tern", lend()).await })
        };
        let ask = link.lent().await;
        let answers = asking.await.expect("the asker runs").expect("tern lends");
        (ask, answers)
    }

    #[tokio::test]
    async fn a_lend_crosses_to_a_dialled_link_and_its_answer_comes_back_in_order() {
        let asks = Asks::default();
        let mut link = Link::carried_by(&asks).await;
        let (ask, mut answers) = borrowed(&asks, &mut link).await;
        let pieces = [
            Lent::Head {
                status: 200,
                headers: vec![],
            },
            Lent::Body("b25l".to_owned()),
            Lent::Body("dHdv".to_owned()),
            Lent::Done,
        ];
        for lent in pieces.clone() {
            link.say(&FromLink::Lent { ask, lent }).await;
        }
        let mut heard = Vec::new();
        while let Some(lent) = tokio::time::timeout(PATIENCE, answers.recv())
            .await
            .expect("the lend answers in time")
        {
            heard.push(lent);
        }
        assert_eq!(
            heard, pieces,
            "each piece arrives once, in order, and then it ends"
        );
    }

    #[tokio::test]
    async fn a_lend_whose_reader_hangs_up_is_let_go_there() {
        let asks = Asks::default();
        let mut link = Link::carried_by(&asks).await;
        let (ask, answers) = borrowed(&asks, &mut link).await;
        drop(answers);
        let said = link.heard().await;
        assert!(
            matches!(serde_json::from_str(&said), Ok(ToLink::Unlend { unlend }) if unlend == ask),
            "the node lets the lend go: {said}"
        );
    }

    #[tokio::test]
    async fn a_lend_whose_reader_stalls_is_let_go_without_holding_up_the_rest() {
        let asks = Asks::default();
        let mut link = Link::carried_by(&asks).await;
        let (ask, mut answers) = borrowed(&asks, &mut link).await;
        let first = Lent::Head {
            status: 200,
            headers: vec![],
        };
        link.say(&FromLink::Lent { ask, lent: first }).await;
        // More than the reader's backlog and the node's hold together.
        let sent = BACKLOG + STALLED_AFTER + 8;
        for _ in 0..sent {
            let lent = Lent::Body("cGllY2U=".to_owned());
            link.say(&FromLink::Lent { ask, lent }).await;
        }
        link.say(&FromLink::Lent {
            ask,
            lent: Lent::Done,
        })
        .await;
        let said = link.heard().await;
        assert!(
            matches!(serde_json::from_str(&said), Ok(ToLink::Unlend { unlend }) if unlend == ask),
            "the node lets the stalled lend go: {said}"
        );
        let (other, _answers) = borrowed(&asks, &mut link).await;
        assert_ne!(other, ask, "another lend still crosses");
        let mut heard = Vec::new();
        while let Some(lent) = tokio::time::timeout(PATIENCE, answers.recv())
            .await
            .expect("what was held arrives in time")
        {
            heard.push(lent);
        }
        assert!(
            heard.len() < sent && !heard.contains(&Lent::Done),
            "the stalled lend ends short, without its end: {} pieces",
            heard.len()
        );
    }

    #[tokio::test]
    async fn a_dialled_link_hands_its_node_a_lend_and_lets_it_go_when_told() {
        let folder = tempfile::tempdir().expect("a folder");
        let path = folder.path().join("node.sock");
        let listener = tokio::net::UnixListener::bind(&path).expect("the node binds");
        let (asks, mut asked) = mpsc::unbounded_channel();
        let (taken, first_taken) = oneshot::channel();
        let linking =
            tokio::spawn(async move { carry(&path, "tern", &asks, &mut Some(taken)).await });
        let (stream, _) = listener.accept().await.expect("the link asks its node");
        let (reading, mut writing) = stream.into_split();
        let mut node = BufReader::new(reading).lines();
        let first = node.next_line().await.expect("it asks").expect("a line");
        assert!(first.contains("Carry"), "the link asks to carry: {first}");
        write(&mut writing, &Reply::Ok(Response::Carrying))
            .await
            .expect("the node answers");
        tokio::time::timeout(PATIENCE, first_taken)
            .await
            .expect("the link hears in time")
            .expect("the link hears its node takes its asks");
        let handed = ToLink::Handed(Handed {
            ask: 7,
            asked: Carried::Lend(lend()),
        });
        write(&mut writing, &handed)
            .await
            .expect("the node hands it");
        let Some(Ask {
            asked: Asked::Lend(asked, lent),
            answer,
        }) = tokio::time::timeout(PATIENCE, asked.recv())
            .await
            .expect("the link is asked in time")
        else {
            panic!("the link was asked what isn't a lend");
        };
        assert_eq!(asked, lend(), "the lend crosses as it was asked");
        answer
            .send(Ok(Response::Carrying))
            .expect("the link answers");
        lent.send(Lent::Body("b25l".to_owned()))
            .await
            .expect("the link lends");
        let mut heard = Vec::new();
        for _ in 0..2 {
            let line = tokio::time::timeout(PATIENCE, node.next_line())
                .await
                .expect("the link says it in time")
                .expect("its connection holds")
                .expect("a line");
            heard.push(serde_json::from_str::<FromLink>(&line).expect("a line from a link"));
        }
        assert!(
            heard.iter().any(|said| matches!(
                said,
                FromLink::Answered(Answered {
                    ask: 7,
                    reply: Ok(_)
                })
            )),
            "the node hears the lend was asked: {heard:?}"
        );
        assert!(
            heard.iter().any(|said| matches!(
                said,
                FromLink::Lent {
                    ask: 7,
                    lent: Lent::Body(_)
                }
            )),
            "the node hears the lend's piece: {heard:?}"
        );
        write(&mut writing, &ToLink::Unlend { unlend: 7 })
            .await
            .expect("the node lets it go");
        tokio::time::timeout(PATIENCE, lent.closed())
            .await
            .expect("the lend is let go in time");
        linking.abort();
    }

    /// A node that takes the link's call and never answers is hung up on,
    /// so the link asks again rather than holding its asks for good.
    #[tokio::test(start_paused = true)]
    async fn a_link_hangs_up_on_a_node_that_never_answers() {
        let folder = tempfile::tempdir().expect("a folder");
        let path = folder.path().join("node.sock");
        let listener = tokio::net::UnixListener::bind(&path).expect("the node binds");
        let (asks, _asked) = mpsc::unbounded_channel();
        let (taken, _first_taken) = oneshot::channel();
        let linking =
            tokio::spawn(async move { carry(&path, "tern", &asks, &mut Some(taken)).await });
        let (stream, _) = listener.accept().await.expect("the link asks its node");
        // Held, so the link never reads the node as gone.
        let (reading, _writing) = stream.into_split();
        let mut node = BufReader::new(reading).lines();
        let first = node.next_line().await.expect("it asks").expect("a line");
        assert!(first.contains("Carry"), "the link asks to carry: {first}");
        let error = linking
            .await
            .expect("the asking runs")
            .expect_err("a node that never answers is hung up on");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
    }

    #[test]
    fn a_command_runs_on_a_machine_as_the_dial_that_reached_it_says_or_else_over_ssh_by_name() {
        let asks = Asks::default();
        let words = ["mycelium".to_owned(), "attach".to_owned()];
        asks.know(
            "tern",
            Arc::new(|_: &str, words: &[String]| {
                let mut there = vec!["into-tern".to_owned()];
                there.extend_from_slice(words);
                there
            }),
        );
        assert_eq!(
            asks.way("tern")("tern", &words),
            ["into-tern", "mycelium", "attach"],
            "a machine the node dialled is reached as its dial says"
        );
        assert_eq!(
            asks.way("skiff")("skiff", &words),
            [
                "mycelium", "attach", "--on", "skiff", "--", "ssh", "-t", "--", "skiff",
                "mycelium", "attach"
            ],
            "one it never dialled is reached over SSH by its name, joined again as it drops"
        );
    }
}
