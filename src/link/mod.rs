//! A link: two machines' stores kept as one, over any byte stream.
//!
//! Both ends run the same code. Each says who it is ([`membership`]), then
//! which messages it holds; each then sends the other what it lacks, and
//! from there every new message as it lands, and each minute whatever the
//! other still lacks, in case its watch missed one. Messages are written
//! once by id, so holding the union of both ends is always right and
//! nothing needs agreeing. What is said to a machine's own group stays on that machine.
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
//! Each end also carries starts ([`starts`]): its node asks the other
//! machine to start a session held, and the other end hands it to its own
//! node, whose answer comes back. Only an end whose hello says it carries
//! them is asked.
//!
//! Each end also carries steers ([`steers`]): its node asks the other
//! machine to put what the person says into a session's inbox there, and
//! the other end writes it, then carries back each line the session adds
//! to the talk it opens. Only an end whose hello says it carries them is
//! asked.
//!
//! Each end reads in a task of its own, so two ends sending a week of
//! messages at once never wait on each other.
//!
//! Where a machine shares quests, its end also carries their files
//! ([`crate::share::exchange`]), looking for changes every second. An end
//! that shares none says nothing of them, so it links with an end too old
//! to know of sharing.
//!
//! An end whose machine leaves (`leaves_at`) tells an end that can release
//! it, once no session runs there any more, that it has handed over
//! ([`crate::handover::handed_over`]), having said all it holds; the link
//! then ends with [`Ending::HandedOver`] if the other end holds all of it,
//! for that end's node to release the machine.
//!
//! How an end is reached is [`dial`]'s, and which ends to reach is
//! [`discovery`]'s.

pub mod carried;
mod declared_heard;
pub mod dial;
pub mod discovery;
pub mod membership;
pub mod refused;
mod started;
pub mod starts;
pub mod steers;

use std::cell::Cell;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::carry::kept;
use crate::facts::{self, Machine, Readiness};
use crate::forage;
use crate::gate::Gates;
use crate::handover;
use crate::hold;
use crate::lend::lender::{self, Lender};
use crate::lend::{BACKLOG, Lend, Lent};
use crate::mesh;
use crate::own_health;
use crate::share::exchange::{Exchange, Places, Say};
use crate::stimuli;
use crate::store::{self, Change, Claim, Store};
use crate::wire::{Ran, Reply, Response, Run, Start, Started};
use dial::Connection;
use membership::{Hello, Membership};
use starts::{Ask, Asked, Asking, RunSaid, RunningHere, Starting};
use steers::{Following, Replies};

/// How far back a link reaches for messages at most: the mesh's own week,
/// so a link never hands back a message the other end has already let go.
const REACH: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How often this end says again that the sessions it carried here still
/// stand, so a copy nothing keeps saying so can be judged gone, and says
/// again what its machine is, whose room changes by the minute.
const KEEP_EVERY: Duration = Duration::from_secs(60);

/// How long a link waits on an end that says its machine each minute
/// before taking it as gone: three of its minutes, so a late one or two
/// don't end it.
const SILENT_AFTER: Duration = Duration::from_secs(3 * 60);

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
        /// The id a holder on this end's machine keeps it by, if one does.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        held: Option<String>,
        /// The cell this end's node's book says started it, if the book
        /// names one; an end older than this names none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_by: Option<String>,
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
    /// Start a session held on the other end's machine.
    Start {
        /// Which ask, so its answer finds it.
        ask: u64,
        /// What to start.
        start: Start,
    },
    /// Put a steer into a session's inbox on the other end's machine.
    Steer {
        /// Which ask, so its answer finds it.
        ask: u64,
        /// The session's address.
        address: String,
        /// What is said, in the harness's own shape.
        steer: Value,
        /// Whether this end's node saw who asked and found them the
        /// person's own; an end older than this never says so.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        checked: bool,
    },
    /// Update the other end's machine to its configuration's main.
    Update {
        /// Which ask, so its answer finds it.
        ask: u64,
    },
    /// Carry a quest's sessions off the other end's machine, as the
    /// person asked on this one.
    Carry {
        /// Which ask, so its answer finds it.
        ask: u64,
        /// The quest, by its id.
        quest: String,
        /// The machine it goes to.
        to: String,
        /// Whether this end's node saw the person's own login ask, which
        /// the other end carries nothing without.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        checked: bool,
    },
    /// End a session held on the other end's machine for good, as this
    /// end's node judged who asked.
    End {
        /// Which ask, so its answer finds it.
        ask: u64,
        /// The session's held id there.
        held: String,
        /// Who asked, as this end's node judged them.
        by: hold::end::Asker,
    },
    /// The talks this end follows and how much of each it has, so the
    /// other end carries on with those whose sessions run there.
    Follow {
        /// The talks.
        talks: Vec<crate::steer::Talk>,
    },
    /// Lines a session on this end's machine added to a talk.
    Replies(Replies),
    /// Run a command on the other end's machine, if this end's key proved
    /// which machine asks.
    Run {
        /// Which ask, so what it says finds it.
        ask: u64,
        /// What to run.
        run: Run,
    },
    /// What a run asked of this end's machine says as it goes, its end
    /// last.
    Ran {
        /// Which ask.
        ask: u64,
        /// What it says.
        ran: Ran,
    },
    /// The asker of a run or a lend has taken a piece of what it says, so
    /// one more may be sent.
    Taken {
        /// Which ask.
        ask: u64,
    },
    /// The asker of a run has let go of it, so it is stopped.
    Unrun {
        /// Which ask.
        ask: u64,
    },
    /// Make a model call with the other end's machine's credentials, if
    /// this end's machine is a worker of it.
    Lend {
        /// Which ask, so what it answers finds it.
        ask: u64,
        /// The call.
        lend: Lend,
    },
    /// What a lend asked of this end's machine answers as it comes, its
    /// end last.
    Lent {
        /// Which ask.
        ask: u64,
        /// A piece of its answer.
        lent: Lent,
    },
    /// The asker of a lend has let go of it, so it is let go.
    Unlend {
        /// Which ask.
        ask: u64,
    },
    /// The other end's machine's answer to an ask, a start's or a steer's.
    Started {
        /// Which ask.
        ask: u64,
        /// Its node's answer.
        reply: Reply,
    },
    /// This end's machine, which leaves, has handed over: no session runs
    /// there now, and all it holds of the quests it shares has been said.
    /// Sent only to an end that said it `releases`.
    HandedOver,
    /// The carries this end's machine's node ordered within the last
    /// day, newest first, as [`kept::told`] says each, so the person sees
    /// them from the other end's machine too. Sent only where both ends
    /// said they `shows`, and never on a link that `releases` a machine,
    /// whose lent machine's person is not the person's own.
    Carries {
        /// The carries, at most [`kept::MOST_TOLD`].
        carries: Vec<Value>,
    },
}

/// How a link ended without failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// The other end left.
    Left,
    /// The other end's machine handed over, and this end holds all it
    /// said, so the machine may be released.
    HandedOver,
}

/// The exchange of what `places` shares between this end, which said
/// `here`, and the other, which said `there`. A worker is sent and served
/// only the quests it was given, and nothing it says of any quest is
/// taken.
///
/// Any other end may be a worker too until it says what its machine is,
/// which it does first of all when both ends take facts, so nothing above
/// the quests shared crosses before then. An end that takes none can
/// declare nothing, so it is never sent a quest above, and this node logs
/// so unless the link is a worker's, which is never sent one whatever its
/// node.
///
/// A machine this end's way in `releases` was made for this machine, and
/// is reached over a provider's link of a few kilobytes a second shared
/// with the person's own work, so it is sent only the scratch of quests
/// given to it ([`Exchange::scratch_where_given`]).
fn exchange_with(
    places: Places,
    here: &Hello,
    there: &Hello,
    (worker, releases): (bool, bool),
) -> Exchange {
    let exchange = Exchange::new(places, here.machine.clone(), there.machine.clone());
    let exchange = if releases {
        exchange.scratch_where_given()
    } else {
        exchange
    };
    let mut exchange = if worker {
        exchange.for_worker()
    } else {
        exchange
    };
    exchange.withhold_above(true);
    if !there.facts && !worker {
        eprintln!(
            "{} takes no facts, so it is sent none of the quests above those shared; \
             update its node to carry them",
            there.machine
        );
    }
    exchange
}

/// Runs this end of a link until the other end leaves, then marks every
/// session it carried here out of reach. With `shares`, it carries the
/// files of the quests this machine shares too, and with `starting`, it
/// carries starts.
#[expect(
    clippy::too_many_lines,
    reason = "an end's state is built in one literal, each field beside where it comes from"
)]
pub async fn run(
    store: impl Store,
    membership: &impl Membership,
    connection: Connection,
    shares: Option<Places>,
    starting: Option<Starting>,
) -> io::Result<Ending> {
    let Connection {
        reading,
        mut writing,
        keep: _keep,
        releases,
        worker,
        named,
        hello_within,
        ..
    } = connection;
    let mut changes = store.changes()?;
    let arrived = Arc::new(AtomicI64::new(now()));
    let skipped_shares = Arc::new(AtomicBool::new(false));
    let (mut frames, mut share_frames, _reader) =
        read_frames(reading, Arc::clone(&arrived), Arc::clone(&skipped_shares));

    let other = greet(
        &mut writing,
        &mut frames,
        (&store, membership),
        releases,
        hello_within,
    )
    .await?;
    pinned(&store, named.as_deref(), &other.machine)?;
    know_how_it_runs(starting.as_ref(), &other.machine);
    let (claim, claimed_at) = claimed(&store, &other.machine).await?;
    let reach = reach_start(&store, &other.machine);
    // As it stood before admission, which marks a machine never linked
    // as of now.
    let began = admitted(&store, &other.machine)?;
    let (mut asked, _reaching) = asked_of(starting.as_ref(), &other.machine).await;
    let (answering, mut answered) = mpsc::unbounded_channel();
    let (runs, mut run_said) = Runs::new();
    let (starters, mut learned) = started::Starters::new();
    let inboxes = starting.as_ref().map(|starting| starting.steers.clone());
    let gates = gates_of(starting.as_ref());
    let places = shares.clone();
    let mut end = End {
        store,
        here: membership.hello().machine,
        realm: membership.hello().realm,
        proven: membership.proves(),
        worker,
        shows: membership.hello().shows && other.shows && !releases && !other.releases && !worker,
        handover: Handover::new(other.releases, skipped_shares),
        // Two ends that both take facts each say their machine every
        // minute, so either hearing nothing for longer knows the other
        // is gone.
        hearing: Hearing::from_now(membership.hello().facts && other.facts, began),
        arrived,
        following: inboxes.clone().map(Following::new),
        inboxes,
        parked: parked_of(starting.as_ref()),
        worker_wakes: VecDeque::new(),
        held: starting.as_ref().map(|starting| starting.held.clone()),
        health: health_of(starting.as_ref()),
        declared: declared_of(starting.as_ref()),
        heard_declared: declared_heard::DeclaredHeard::default(),
        lender: lender_of(starting.as_ref()),
        housed: if matches!(
            starting
                .as_ref()
                .and_then(|starting| starting.asking.as_ref()),
            Some(Asking::Dialled)
        ) {
            Housed::ApartFromItsNode
        } else {
            Housed::InItsNode
        },
        node: starting.map(|starting| starting.node),
        starters,
        answering,
        asks: HashMap::new(),
        runs,
        next_ask: 0,
        exchange: shares.map(|places| {
            let exchange =
                exchange_with(places, &membership.hello(), &other, (worker, releases));
            if worker {
                exchange.gating(gates.clone())
            } else {
                exchange
            }
        }),
        gates,
        other: other.machine.clone(),
        said: other,
        reach,
        has: HashSet::new(),
        sent: BTreeMap::new(),
        carried: HashSet::new(),
        places,
        acting: None,
        outgoing: VecDeque::new(),
        withheld: Vec::new(),
        owed: Owed::default(),
        _claim: claim,
    };

    // A link that fails as it opens leaves as one failing later does.
    if let Err(error) = end.open(&mut writing).await {
        return end.leave(Err(error));
    }

    let (mut keeping, mut looking) = paces();
    let mut queued = VecDeque::new();
    let (store, other_machine) = (end.store.clone(), end.other.clone());
    let carrying = async {
        loop {
            let step = tokio::select! {
                frame = pull(&mut queued, &mut frames) => match frame {
                    Some(Ok(frame)) => end.take(frame, &mut writing).await,
                    Some(Err(error)) => Err(error),
                    None => break Ok(Ending::Left),
                },
                _ = keeping.tick() => end.keep(&mut writing).await,
                Some(ask) = next(&mut asked) => end.ask(ask, &mut writing).await,
                Some((ask, reply)) = answered.recv() => {
                    end.answered(ask, reply, &mut writing).await
                }
                Some(said) = run_said.recv() => end.run_said(said, &mut writing).await,
                Some(found) = learned.recv() => end.learned(found, &mut writing).await,
                _ = looking.tick() => end.look(&mut writing).await,
                () = own_health::moved(&mut end.health.told) => {
                    end.send_machine(&mut writing).await
                }
                gathered = changes.gathered() => match gathered {
                    Some(gathered) => end.changed_all(gathered, &mut writing).await,
                    None => break Ok(Ending::Left),
                },
                // The exchange's work takes a turn like any other, so an
                // ask waits on one frame of it at most, never on all of
                // a quest's files. One share is taken at a time, and none
                // while what the last said is still being sent, so the
                // other end's are read no faster than they are answered.
                acted = acted(&mut end.acting) => end.acted(acted),
                () = std::future::ready(()), if !end.outgoing.is_empty() => {
                    end.send_shared(&mut writing).await
                }
                Some(frame) = share_frames.recv(), if end.acting.is_none() => {
                    end.take(frame, &mut writing).await
                }
            };
            if let Err(error) = step {
                break Err(error);
            }
            if end.handover.done {
                break Ok(Ending::HandedOver);
            }
        }
    };
    // Raced whole, so a link held in a write that never goes through
    // still lets its claim go to a newer one at once.
    let result = tokio::select! {
        result = carrying => result,
        () = superseded(&store, &other_machine, claimed_at) => Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            format!("a newer link with {other_machine} took over"),
        )),
    };
    end.leave(result)
}

/// When a link keeps what it said, a minute on since the start has just
/// said all of it, and when it looks at what it carries.
fn paces() -> (tokio::time::Interval, tokio::time::Interval) {
    let keeping = tokio::time::interval_at(tokio::time::Instant::now() + KEEP_EVERY, KEEP_EVERY);
    (keeping, tokio::time::interval(LOOK_EVERY))
}

/// What the exchange's act running in `acting` gave back, or never while
/// none runs. It is cancel safe, as `select!` needs: the act runs on
/// whether it is waited on or not.
async fn acted(
    acting: &mut Option<Acting>,
) -> Result<(Exchange, io::Result<Vec<Say>>), tokio::task::JoinError> {
    match acting {
        Some(acting) => acting.await,
        None => std::future::pending().await,
    }
}

/// The next of what `asked` holds, or never when there is none.
async fn next<T>(asked: &mut Option<mpsc::UnboundedReceiver<T>>) -> Option<T> {
    match asked {
        Some(asked) => asked.recv().await,
        None => std::future::pending().await,
    }
}

/// How many frames waiting behind a machine's facts are looked through
/// for newer facts at once, so a long queue still lets the link's other
/// work in between.
const FACTS_LOOKED_THROUGH: usize = 256;

/// The other end's next frame: the first of `queued`, else the next of
/// `frames`, where facts it brings are kept only if no newer facts wait
/// behind them ([`newest_facts`]). It is cancel safe, as `select!` needs:
/// nothing is taken from `frames` but in the wait for its next.
async fn pull(
    queued: &mut VecDeque<io::Result<Frame>>,
    frames: &mut mpsc::UnboundedReceiver<io::Result<Frame>>,
) -> Option<io::Result<Frame>> {
    if let Some(frame) = queued.pop_front() {
        return Some(frame);
    }
    let first = frames.recv().await?;
    Some(newest_facts(first, frames, queued))
}

/// `first`, or where it is a machine's facts and others wait behind it,
/// the newest facts waiting, taken in `first`'s place, with the other
/// frames waiting put in `queued` in the order they came and every older
/// facts left out. Facts are said whole every time, so older ones held up
/// behind newer ones say nothing the newest doesn't: a link whose node
/// fell behind, as one did by 49 minutes on 2026-11-21, catches up on
/// what its machine is now rather than on what it was. The newest come
/// first so no frame is judged on facts older than those it came behind.
fn newest_facts(
    first: io::Result<Frame>,
    frames: &mut mpsc::UnboundedReceiver<io::Result<Frame>>,
    queued: &mut VecDeque<io::Result<Frame>>,
) -> io::Result<Frame> {
    if !matches!(first, Ok(Frame::Machine(_))) {
        return first;
    }
    let mut waiting = vec![first];
    while waiting.len() < FACTS_LOOKED_THROUGH {
        match frames.try_recv() {
            Ok(frame) => {
                let failed = frame.is_err();
                waiting.push(frame);
                if failed {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let newest = waiting
        .iter()
        .rposition(|frame| matches!(frame, Ok(Frame::Machine(_))))
        .unwrap_or_else(|| unreachable!("the first is facts"));
    let next = waiting.remove(newest);
    queued.extend(
        waiting
            .into_iter()
            .filter(|frame| !matches!(frame, Ok(Frame::Machine(_)))),
    );
    next
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
/// sends a line that isn't one, keeping in `arrived` when the last came:
/// the other end is heard as its words arrive, not only once the link's
/// own work reaches them, which may be held minutes behind a quest it
/// shares. A frame after the hello of a kind this end doesn't know is
/// skipped ([`unread`]), said once a link for each of the first
/// [`SKIPS_SAID`] kinds, and a skipped share is kept in `skipped_shares`,
/// since it may have named a file this end then never hears of.
///
/// After the first frame, shares and the word that a machine has handed
/// over, which must follow its shares, come apart from the rest
/// ([`shared_lane`]), so an ask, an answer or a message is taken ahead
/// of the files a quest's first copy carries: on 25 November a start on
/// a new cloud machine waited 79 seconds behind them.
fn read_frames(
    reading: Box<dyn AsyncRead + Unpin + Send>,
    arrived: Arc<AtomicI64>,
    skipped_shares: Arc<AtomicBool>,
) -> (
    mpsc::UnboundedReceiver<io::Result<Frame>>,
    mpsc::UnboundedReceiver<Frame>,
    Reader,
) {
    let (sender, frames) = mpsc::unbounded_channel();
    let (sharing, shared) = mpsc::unbounded_channel();
    let task = tokio::spawn(async move {
        let mut lines = BufReader::new(reading).lines();
        // The machine the first frame named, once one has been read: the
        // hello's own word, before the link admits it.
        let mut greeted: Option<String> = None;
        let mut skipped = Skipped::default();
        loop {
            let line = lines.next_line().await;
            arrived.fetch_max(now(), Ordering::Relaxed);
            let frame = match line {
                Ok(Some(line)) => match serde_json::from_str(&line) {
                    Ok(frame) => Ok(frame),
                    // Before the first frame is read, nothing is skipped.
                    Err(error) => match greeted.as_deref().zip(unread(&line)) {
                        Some((machine, unread)) => {
                            if let Unread::Share(_) = unread {
                                // Set before the next frame is sent on, so
                                // the link reads it by then.
                                skipped_shares.store(true, Ordering::Relaxed);
                            }
                            if let Some(said) = say_skipped(machine, &unread, &mut skipped) {
                                eprintln!("{said}");
                            }
                            continue;
                        }
                        None => Err(io::Error::new(io::ErrorKind::InvalidData, error)),
                    },
                },
                Ok(None) => return,
                Err(error) => Err(error),
            };
            if greeted.is_none() {
                greeted = Some(match &frame {
                    Ok(Frame::Hello(hello)) => hello.machine.clone(),
                    _ => "the other end".to_owned(),
                });
                let failed = frame.is_err();
                if sender.send(frame).is_err() || failed {
                    return;
                }
                continue;
            }
            let sent = match frame {
                Ok(frame) if shared_lane(&frame) => sharing.send(frame).is_ok(),
                frame => {
                    let failed = frame.is_err();
                    sender.send(frame).is_ok() && !failed
                }
            };
            if !sent {
                return;
            }
        }
    });
    (frames, shared, Reader(task))
}

/// Whether `frame` is taken in the order shares are: a share, or the
/// word that the other machine has handed over, which is judged on every
/// file its shares named having been heard of ([`End::heard_handed_over`]).
fn shared_lane(frame: &Frame) -> bool {
    matches!(frame, Frame::Share(_) | Frame::HandedOver)
}

/// How many kinds of skipped frame a link says, each once: past them a
/// link says only that there are more, so an end sending ever new kinds
/// grows neither this end's memory nor its log.
const SKIPS_SAID: usize = 8;

/// How much of the other end's word, a kind or its machine's name, a
/// skip says and keeps.
const WORD_SHOWN: usize = 40;

/// The kinds of frame a link has skipped, each cut to [`WORD_SHOWN`], and
/// whether it has said that there are more than it keeps.
#[derive(Default)]
struct Skipped {
    kinds: HashSet<String>,
    more: bool,
}

/// What to log of `machine` sending `unread`, which this end skips: a line
/// the first time a link meets its kind, and one more past
/// [`SKIPS_SAID`] kinds, that there are others. The kind and the machine
/// are the other end's words, so they are cut short and quoted, their
/// marks escaped, and can't write a line of their own into the log.
fn say_skipped(machine: &str, unread: &Unread, skipped: &mut Skipped) -> Option<String> {
    let kind = cut(&unread.to_string());
    if skipped.more || skipped.kinds.contains(&kind) {
        return None;
    }
    let machine = cut(machine);
    if skipped.kinds.len() == SKIPS_SAID {
        skipped.more = true;
        return Some(format!(
            "{machine:?} sent more kinds of frame this node can't read, skipped unsaid"
        ));
    }
    let said = format!(
        "{machine:?} sent a frame of a kind this node doesn't know, {kind:?}, so it skips \
         each it sends; update this node to read them"
    );
    skipped.kinds.insert(kind);
    Some(said)
}

/// The first [`WORD_SHOWN`] characters of `word`.
fn cut(word: &str) -> String {
    word.chars().take(WORD_SHOWN).collect()
}

/// A frame this end can't read but the link can carry on past, by its
/// kind.
#[derive(Debug, PartialEq, Eq)]
enum Unread {
    /// A frame of a kind this end doesn't know.
    Frame(String),
    /// A share of a kind this end doesn't know.
    Share(String),
}

impl fmt::Display for Unread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(kind) => f.write_str(kind),
            Self::Share(kind) => write!(f, "Share {kind}"),
        }
    }
}

/// What `line` is, where it is a frame this end can't read but the link
/// can carry on past: a frame, from an end newer than this one, of a kind
/// this end doesn't know, or a share of a kind it doesn't (PROTOCOL.md, A
/// Link). None for a line that isn't a frame at all, and for a frame of a
/// kind this end knows whose body it can't read, which may be one an ask
/// waits on: either ends the link.
fn unread(line: &str) -> Option<Unread> {
    let frame = serde_json::from_str::<Variant<IgnoredAny>>(line).ok()?;
    if !kinds::<Frame>().contains(&frame.kind.as_str()) {
        return Some(Unread::Frame(frame.kind));
    }
    if frame.kind != "Share" {
        return None;
    }
    let share = serde_json::from_str::<Variant<Variant<IgnoredAny>>>(line)
        .ok()?
        .body?;
    (!kinds::<Say>().contains(&share.kind.as_str())).then_some(Unread::Share(share.kind))
}

/// A value as serde writes an enum's variant, read without the enum: its
/// kind, from a bare string or an object of exactly one key, and the body
/// an object holds. A key said twice is refused, where a map would keep
/// only the last, so a share of a known kind can't hide behind another.
struct Variant<B> {
    kind: String,
    body: Option<B>,
}

impl<'de, B: Deserialize<'de>> Deserialize<'de> for Variant<B> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(VariantVisitor(PhantomData))
    }
}

struct VariantVisitor<B>(PhantomData<B>);

impl<'de, B: Deserialize<'de>> serde::de::Visitor<'de> for VariantVisitor<B> {
    type Value = Variant<B>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an enum's variant")
    }

    fn visit_str<E: serde::de::Error>(self, kind: &str) -> Result<Self::Value, E> {
        Ok(Variant {
            kind: kind.to_owned(),
            body: None,
        })
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let Some((kind, body)) = map.next_entry::<String, B>()? else {
            return Err(serde::de::Error::custom("a variant names its kind"));
        };
        if map.next_key::<IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom("a variant names one kind"));
        }
        Ok(Variant {
            kind,
            body: Some(body),
        })
    }
}

/// The variants of enum `T` by the names it reads them by, as its own
/// derive hands them to a deserializer, so the list can't drift from the
/// enum.
fn kinds<T: for<'de> Deserialize<'de>>() -> &'static [&'static str] {
    let named = Cell::new(&[][..]);
    let _ = T::deserialize(Kinds(&named));
    named.get()
}

/// A deserializer that reads nothing, keeping the variants an enum asks
/// it for ([`kinds`]).
struct Kinds<'a>(&'a Cell<&'static [&'static str]>);

impl<'de> serde::Deserializer<'de> for Kinds<'_> {
    type Error = serde::de::value::Error;

    fn deserialize_any<V: serde::de::Visitor<'de>>(self, _: V) -> Result<V::Value, Self::Error> {
        Err(serde::de::Error::custom("only an enum's variants are read"))
    }

    fn deserialize_enum<V: serde::de::Visitor<'de>>(
        self,
        _: &'static str,
        variants: &'static [&'static str],
        _: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.set(variants);
        Err(serde::de::Error::custom("only its variants are read"))
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
        option unit unit_struct newtype_struct seq tuple tuple_struct map struct identifier
        ignored_any
    }
}

/// Tells the node's asks how a command runs on `machine`, as this dial
/// that reached it says, even when a link that machine dialled holds it
/// and carries the starts.
fn know_how_it_runs(starting: Option<&Starting>, machine: &str) {
    if let Some(Asking::Kept(asks, there)) = starting.and_then(|s| s.asking.as_ref()) {
        asks.know(machine, there.clone());
    }
}

/// How long a link `machine` dialled waits for its node to take the asks
/// of `machine` ([`asked_of`]): far past the moment a node at hand takes,
/// so only a node stalled for longer has its facts go on first.
const ASKS_TAKEN_WITHIN: Duration = Duration::from_secs(5);

/// The starts this machine's node asks of `machine`, taken for this link
/// until what it answers is dropped, when the link carries the node's
/// starts: in the node's own process for a link it keeps, and over its
/// socket for one `machine` dialled. None otherwise.
///
/// A link `machine` dialled waits, up to [`ASKS_TAKEN_WITHIN`], for its
/// node to take the asks before the link goes on to say `machine`'s
/// facts, since those mark it ready: a start placed there in between
/// would otherwise find no link reaching it. A node that can't be reached
/// ends the wait at once, and is asked again as [`carried::from_node`]
/// says, while the link carries on.
async fn asked_of(
    starting: Option<&Starting>,
    machine: &str,
) -> (Option<mpsc::UnboundedReceiver<Ask>>, Option<Box<dyn Send>>) {
    let Some(starting) = starting else {
        return (None, None);
    };
    match &starting.asking {
        Some(Asking::Kept(asks, there)) => {
            let (asked, reaching) = asks.reach(machine, there.clone());
            (Some(asked), Some(Box::new(reaching)))
        }
        Some(Asking::Dialled) => {
            let (asked, carrying, taken) = carried::from_node(starting.node.clone(), machine);
            match tokio::time::timeout(ASKS_TAKEN_WITHIN, taken).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => eprintln!(
                    "mycelium: the asking of the node for {machine} ended before the node took \
                     its asks; its facts go on, and a start there finds no link"
                ),
                Err(_) => eprintln!(
                    "mycelium: the node hasn't taken its asks of {machine} through this link \
                     yet; its facts go on, and a start there may find no link until it does"
                ),
            }
            (Some(asked), Some(Box::new(carrying)))
        }
        None => (None, None),
    }
}

/// Where this machine's declaration is read from: the node's file, for a
/// link it keeps, so both say the machine alike; the node itself, for a
/// link another machine dialled; the account's own file for a link with no
/// node behind it, which says only facts.
fn declared_of(starting: Option<&Starting>) -> Declaring {
    match starting {
        None => Declaring::File(crate::place::declared(), None, None),
        Some(Starting {
            declared: Some(file),
            kit,
            saturation,
            ..
        }) => Declaring::File(file.clone(), kit.clone(), saturation.clone()),
        Some(Starting {
            declared: None,
            node,
            ..
        }) => Declaring::Node(node.clone()),
    }
}

/// How long an end of a link waits for its node to say what it measures
/// this machine to be. The node answers from a few small files, so one
/// slower than this is stuck, and the end says what it measures itself.
const MEASURED_WITHIN: Duration = Duration::from_secs(5);

/// What the node on `node` measures this machine to be, or none where no
/// node answers there in time, as while it restarts.
async fn measured_by(node: &std::path::Path) -> Option<Machine> {
    let asked = async {
        let (reading, mut writing) = tokio::net::UnixStream::connect(node)
            .await
            .ok()?
            .into_split();
        let mut ask = serde_json::to_vec(&crate::wire::Request::Machine).ok()?;
        ask.push(b'\n');
        writing.write_all(&ask).await.ok()?;
        let line = BufReader::new(reading).lines().next_line().await.ok()??;
        match serde_json::from_str::<Reply>(&line).ok()? {
            Ok(Response::Machine(machine)) => Some(*machine),
            _ => None,
        }
    };
    tokio::time::timeout(MEASURED_WITHIN, asked)
        .await
        .ok()
        .flatten()
}

/// Where an end of a link reads what its machine declares.
enum Declaring {
    /// From this file, with the realm's kit as the node was given it, if
    /// it was given one, and the node's judgement of saturation, if a
    /// node keeps the link.
    File(
        PathBuf,
        Option<hold::start::Kit>,
        Option<Arc<crate::placement::Saturation>>,
    ),
    /// From the node on this socket, asked for what it measures this
    /// machine to be, as a link another machine dialled does, since it
    /// runs apart from the node and is started by whatever SSH runs.
    Node(PathBuf),
}

/// Where this machine's parked sessions' markers are, and when it booted,
/// for an end that carries starts. None otherwise.
fn parked_of(starting: Option<&Starting>) -> Option<(PathBuf, Option<i64>)> {
    starting.map(|starting| (starting.parked.clone(), crate::node::standing::began()))
}

/// This machine's own health as a link says it, when the link carries
/// what this machine is: the node's own, for a link it keeps, or watched
/// over its socket, for one another machine dialled, which runs apart.
/// None otherwise.
fn health_of(starting: Option<&Starting>) -> OwnHealth {
    let (told, watching) =
        match starting.map(|starting| (starting, &starting.health, &starting.asking)) {
            Some((_, Some(health), _)) => (Some(health.clone()), None),
            Some((starting, None, Some(Asking::Dialled))) => {
                let (health, watching) = own_health::from_node(starting.node.clone());
                (Some(health), Some(watching))
            }
            _ => (None, None),
        };
    OwnHealth {
        told,
        _watching: watching,
    }
}

/// What this machine lends the other end, as `starting` says; none where
/// the link carries no starts, so every lend it asks is refused.
fn lender_of(starting: Option<&Starting>) -> Option<Arc<Lender>> {
    starting.and_then(|starting| starting.lender.clone())
}

/// The gates this machine answers its workers, if its node names any.
fn gates_of(starting: Option<&Starting>) -> Option<Arc<Gates>> {
    starting.and_then(|starting| starting.gates.clone())
}

/// Holds the other end to the name its way in pinned, `named`, if it pins
/// one, pinning the first it says where none was yet, so a machine dialled
/// as one can't say hello as another; one that does is refused, and why is
/// kept in `store`.
fn pinned(store: &impl Store, named: Option<&OnceLock<String>>, said: &str) -> io::Result<()> {
    let Some(named) = named else {
        return Ok(());
    };
    let pinned = named.get_or_init(|| said.to_owned());
    if pinned == said {
        return Ok(());
    }
    let why = format!("{pinned} said hello as {said}, which its way in doesn't reach");
    if let Err(error) = store.refuse_link(pinned, &why) {
        eprintln!("why {pinned} was refused isn't kept: {error}");
    }
    Err(refusal(why))
}

/// How long a newer link waits for the link holding its machine's claim
/// to let it go: that one looks each [`LOOK_EVERY`], and lets go what it
/// carried in under a second.
const TAKEN_OVER_WITHIN: Duration = Duration::from_secs(10);

/// This link's hold on `machine`, and when it took it. Where another link
/// holds it, that one is asked to let it go ([`Store::supersede`]), since a
/// newer link from the same machine means the older one's other end is
/// gone though it hasn't heard so: a node restarted on 25 November had its
/// new links to a cloud machine refused there while the old one's end
/// went on for minutes. Refused when it isn't let go in time.
async fn claimed(store: &impl Store, machine: &str) -> io::Result<(Claim, i64)> {
    if let Some(claim) = store.claim(machine)? {
        return Ok((claim, now()));
    }
    store.supersede(machine)?;
    let taken = tokio::time::timeout(TAKEN_OVER_WITHIN, async {
        loop {
            tokio::time::sleep(LOOK_EVERY / 4).await;
            if let Some(claim) = store.claim(machine)? {
                return Ok::<_, io::Error>((claim, now()));
            }
        }
    });
    taken.await.unwrap_or_else(|_| {
        Err(refusal(format!(
            "a link to {machine} already carries it here, and didn't let it go \
             within {TAKEN_OVER_WITHIN:?}"
        )))
    })
}

/// Waits until a newer link with `machine` asks for the claim this link
/// took at `claimed_at`, looking each [`LOOK_EVERY`].
async fn superseded(store: &impl Store, machine: &str, claimed_at: i64) {
    let mut looking = tokio::time::interval(LOOK_EVERY);
    loop {
        looking.tick().await;
        if store
            .superseded(machine)
            .is_some_and(|asked| asked > claimed_at)
        {
            return;
        }
    }
}

/// Keeps that this store is linked with `machine`, as of now where it
/// never was, and forgets why a link with it was last refused. A mark
/// already kept stays where it is, since the link's first keep, or its
/// end, keeps it where the link began until its catch-up has stood
/// ([`Hearing::linked_until`]), and a node killed before either leaves
/// it there too, so the next link still reaches back that far. When it
/// was let in is kept apart, as when the machine was last linked
/// ([`store::last_linked`]). Answers the mark as it stood before, none
/// for a machine never linked.
fn admitted(store: &impl Store, machine: &str) -> io::Result<Option<i64>> {
    let before = store.linked_until(machine);
    let at = now();
    if before.is_none() {
        store.mark_linked(machine, at)?;
    }
    store.mark_admitted(machine, at)?;
    if let Err(error) = store.link_admitted(machine) {
        eprintln!("{machine} linked, but its last refusal stands: {error}");
    }
    Ok(before)
}

/// Says this end's hello, saying whether its way in `releases` the other
/// end's machine, and hears and admits the other end's, waiting on it no
/// longer than `within`, if that says how long. Why one is refused is kept
/// in `store`, for health to say while no link reaches its machine.
async fn greet(
    writing: &mut (impl AsyncWrite + Unpin),
    frames: &mut mpsc::UnboundedReceiver<io::Result<Frame>>,
    (store, membership): (&impl Store, &impl Membership),
    releases: bool,
    within: Option<Duration>,
) -> io::Result<Hello> {
    let ours = Hello {
        releases,
        ..membership.hello()
    };
    if let Err(error) = send(writing, &Frame::Hello(ours)).await {
        // An end whose hello is already here was reached, though it
        // closed before taking ours, so what its way in says next may be
        // the far machine's own words.
        return Err(match frames.try_recv() {
            Ok(Ok(Frame::Hello(_))) => error,
            _ => unheard_write(error),
        });
    }
    let other = hello(frames, within).await?;
    if let Err(why) = membership.admit(&other) {
        if let Err(error) = store.refuse_link(&other.machine, &why) {
            eprintln!("why {} was refused isn't kept: {error}", other.machine);
        }
        return Err(refusal(why));
    }
    Ok(other)
}

/// What an end that said `said` in its hello is too old to do of what is
/// `asked`, if it is: it carries only what it said it carries, and a
/// reopening only where its node judges one too.
fn cannot_carry(asked: &Asked, said: &Hello) -> Option<&'static str> {
    let (carries, cannot) = match asked {
        Asked::Start(_) => (said.starts, "start sessions"),
        Asked::Reopen(_) => (said.starts && said.reopens, "judge a reopening"),
        Asked::Steer(_) => (said.steers, "carry steers"),
        Asked::Update => (said.updates, "be asked to update"),
        Asked::Run(..) => (said.runs, "run commands"),
        Asked::Lend(..) => (said.lends, "lend model calls"),
        Asked::Carry { .. } => (said.carries, "carry sessions"),
        Asked::End { .. } => (said.ends, "end held sessions"),
    };
    (!carries).then_some(cannot)
}

/// The other end's hello, which must come first, and within `within` if
/// it says how long.
async fn hello(
    frames: &mut mpsc::UnboundedReceiver<io::Result<Frame>>,
    within: Option<Duration>,
) -> io::Result<Hello> {
    let first = match within {
        Some(within) => tokio::time::timeout(within, frames.recv())
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("the other end said nothing within {within:?}"),
                )
            })?,
        None => frames.recv().await,
    };
    match first {
        Some(Ok(Frame::Hello(other))) => Ok(other),
        Some(Ok(_)) => Err(refusal("the other end didn't say hello first".to_owned())),
        Some(Err(error)) => Err(error),
        // Why is the dial's to say, as SSH does on its own error output: a
        // machine it can't find, a key refused, or no mycelium there.
        None => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            ClosedUnheard("the other end closed before saying hello".to_owned()),
        )),
    }
}

/// The other end closed before it said hello, so no link came up, in
/// the words that say how.
#[derive(Debug)]
struct ClosedUnheard(String);

impl fmt::Display for ClosedUnheard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ClosedUnheard {}

/// Our hello, written to an end that closed before it was taken, which
/// is a way in that ended before any link came up, as a provider's that
/// says its machine is gone and exits may.
fn unheard_write(error: io::Error) -> io::Error {
    if matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
    ) {
        let words = format!("the other end closed before our hello was taken: {error}");
        io::Error::new(error.kind(), ClosedUnheard(words))
    } else {
        error
    }
}

/// Whether a link ended in `error` because the other end closed before
/// it said hello, so no link ever came up. Only then is what the way in
/// said its own, as a provider says before it dials, rather than what a
/// machine it reached said once there.
#[must_use]
pub fn closed_unheard(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(<dyn std::error::Error + Send + Sync>::is::<ClosedUnheard>)
}

/// A living session's presence, the leases it holds, the id its holder
/// keeps it by and who started it, as its node names them, as last sent:
/// the presence shared with the store's reading of it, so one unchanged is
/// compared by pointer and never copied, and the starter compared too, so
/// a presence goes again once its node names one.
type Sent = (Arc<Value>, Vec<Value>, Option<String>, Option<String>);

/// Where an end runs, which says whether its node sees the link drop.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Housed {
    /// In its node, as a link the node keeps, so the node sees the drop as
    /// it is.
    InItsNode,
    /// Apart from its node, as a link another machine dialled, in a
    /// process SSH started, which may run on while the node is stopped.
    ApartFromItsNode,
}

/// This end of a link, and what it knows of the other.
struct End<S> {
    store: S,
    /// This end's machine.
    here: String,
    /// The realm the link is in, which hears what this end's machine
    /// declares of it alone.
    realm: String,
    /// Whether the other end is proven to be its machine, so may ask this
    /// one to update.
    proven: bool,
    /// Whether the other end is a worker, which asks nothing of this
    /// machine and speaks only for its own sessions ([`Connection::worker`]).
    worker: bool,
    /// Whether the two ends tell each other of the carries their nodes
    /// ordered: both said they do, neither releases the other, and the
    /// other isn't a worker, since quest ids and reasons aren't work.
    shows: bool,
    /// Where either machine's handover stands.
    handover: Handover,
    /// When this end last heard the other, and whether it should have.
    hearing: Hearing,
    /// When the other end's last frame arrived, as the task reading them
    /// keeps it ([`read_frames`]).
    arrived: Arc<AtomicI64>,
    /// This machine's sessions' steer inboxes, if this end carries steers.
    inboxes: Option<PathBuf>,
    /// Where the harness keeps this machine's parked sessions' markers,
    /// for an end its node keeps or a machine dialled, so a message
    /// carried in wakes the parked sessions it gives a turn
    /// ([`crate::wake`]); and when this machine booted, before which a
    /// marker names a holder that is gone.
    parked: Option<(PathBuf, Option<i64>)>,
    /// When, in milliseconds since the epoch, each session this end woke in
    /// the last minute was woken, kept only for a worker's link, which
    /// wakes at most [`crate::wake::WORKER_WAKES`] a minute.
    worker_wakes: VecDeque<i64>,
    /// The talks this end carries back to the other.
    following: Option<Following>,
    /// Where this end runs, in its node or apart from it.
    housed: Housed,
    /// This machine's node's socket, which starts asked of it go to.
    node: Option<PathBuf>,
    /// What answers the lends a worker asks of this machine, if it lends.
    lender: Option<Arc<Lender>>,
    /// The gates this machine answers for the workers it names, which
    /// are the one start a worker may ask of it ([`crate::gate`]).
    gates: Option<Arc<Gates>>,
    /// Who started this machine's held sessions, as its node's book says,
    /// said beside their presences.
    starters: started::Starters,
    /// Where the answers to starts asked of this machine go, to be sent.
    answering: mpsc::UnboundedSender<(u64, Reply)>,
    /// Starts asked of the other machine and not yet answered.
    asks: HashMap<u64, oneshot::Sender<Reply>>,
    /// The runs and lends this end carries, either way.
    runs: Runs,
    next_ask: u64,
    other: String,
    /// What the other end said of itself in its hello: what it hears of
    /// and carries.
    said: Hello,
    reach: i64,
    has: HashSet<String>,
    /// Each living session's presence and leases, as last sent.
    sent: BTreeMap<String, Sent>,
    /// The realm's folder of held sessions on this machine, if this end
    /// says which hold its sessions.
    held: Option<PathBuf>,
    carried: HashSet<String>,
    exchange: Option<Exchange>,
    /// This machine's own health, as its node keeps it, which a word
    /// moving has said again at once rather than with the next minute's
    /// keep.
    health: OwnHealth,
    /// Where what this machine's configuration declares is read from.
    declared: Declaring,
    /// What the other machine last declared, kept through a short silence
    /// about it.
    heard_declared: declared_heard::DeclaredHeard,
    /// Where this machine keeps the quests it shares, if it shares any,
    /// kept apart from the exchange, which an act may hold for minutes.
    places: Option<Places>,
    /// The exchange's act on the disk, while one runs away from the link's
    /// loop ([`End::share`]).
    acting: Option<Acting>,
    /// What the exchange's acts said, waiting to be sent one at a time
    /// between the link's other work ([`End::send_shared`]).
    outgoing: VecDeque<Frame>,
    /// Whether the exchange holds what the other machine says above its
    /// quests, each as it was judged while an act held the exchange.
    withheld: Vec<bool>,
    /// What waits on the act running now.
    owed: Owed,
    _claim: Claim,
}

/// An exchange's act on the disk, running away from the link's loop: the
/// exchange it took, given back with what it says.
type Acting = tokio::task::JoinHandle<(Exchange, io::Result<Vec<Say>>)>;

/// What a link owes once the act running now gives the exchange back.
#[derive(Default)]
struct Owed {
    /// A look, due while the act held the exchange.
    look: bool,
    /// The word that this machine has handed over, sent after what the
    /// act's look says.
    handed_over: bool,
}

/// This machine's own health as an end of a link says it.
struct OwnHealth {
    /// The health, if this end says it.
    told: Option<own_health::Told>,
    /// The watch of the node's health, for an end that runs apart from it.
    _watching: Option<own_health::Watching>,
}

/// The runs and lends an end carries: those it asked of the other
/// machine, and those the other end asked of this one. Both number their
/// asks from one count, so a [`Frame::Taken`] names one or the other.
struct Runs {
    /// Where what each run asked of the other machine says goes, by ask.
    asked: HashMap<u64, mpsc::UnboundedSender<Ran>>,
    /// Each run the other end asked of this machine, by its ask.
    here: HashMap<u64, RunningHere>,
    /// Where what each lend asked of the other machine answers goes, by
    /// ask.
    borrowed: HashMap<u64, mpsc::UnboundedSender<Lent>>,
    /// Each lend the other end asked of this machine, by its ask.
    lending: HashMap<u64, RunningHere>,
    /// Where runs and lends tell the link's loop what to send.
    saying: mpsc::UnboundedSender<RunSaid>,
}

impl Runs {
    /// None yet, and where what runs tell the link's loop comes out.
    fn new() -> (Self, mpsc::UnboundedReceiver<RunSaid>) {
        let (saying, said) = mpsc::unbounded_channel();
        let runs = Self {
            asked: HashMap::new(),
            here: HashMap::new(),
            borrowed: HashMap::new(),
            lending: HashMap::new(),
            saying,
        };
        (runs, said)
    }

    /// Hands what the run asked as `ask` of the other machine said on to
    /// its asker, letting go of it once it ends.
    fn ran(&mut self, ask: u64, ran: Ran) {
        let ends = ran.ends();
        // A run its asker let go of has nobody to hear it.
        if let Some(asker) = self.asked.get(&ask) {
            let _ = asker.send(ran);
        }
        if ends {
            self.asked.remove(&ask);
        }
    }

    /// Hands what the lend asked as `ask` of the other machine answered on
    /// to its asker, letting go of it once it ends.
    fn lent(&mut self, ask: u64, lent: Lent) {
        let ends = lent.ends();
        // A lend its asker let go of has nobody to hear it.
        if let Some(asker) = self.borrowed.get(&ask) {
            let _ = asker.send(lent);
        }
        if ends {
            self.borrowed.remove(&ask);
        }
    }

    /// Lets the run or lend asked as `ask` of this machine send one more
    /// piece, widening its window by one the other end took; a lend's never
    /// past [`BACKLOG`], so a worker saying it took what it was never sent
    /// holds back no more of the lender's answer.
    fn taken(&self, ask: u64) {
        if let Some(running) = self.here.get(&ask) {
            running.window.add_permits(1);
        } else if let Some(lending) = self.lending.get(&ask)
            && lending.window.available_permits() < BACKLOG
        {
            lending.window.add_permits(1);
        }
    }
}

/// Where a machine's handover stands, at one end of a link.
#[derive(Debug, Default)]
struct Handover {
    /// Whether the other end can release this end's machine, and so is
    /// told when it has handed over.
    releases: bool,
    /// When this end's machine leaves, as it last declared.
    leaves: Option<jiff::Timestamp>,
    /// Whether work, a session or a held program, has run on this end's
    /// machine since the link came.
    ran: bool,
    /// Whether the other end's machine has handed over, with this end
    /// holding all it said.
    done: bool,
    /// Whether this end skipped a share of a kind it doesn't know, which
    /// may have named a file it then never heard of.
    skipped_shares: Arc<AtomicBool>,
}

impl Handover {
    /// A handover not begun, at an end whose other end can release it
    /// where `releases` says, and whose reading marks `skipped_shares`.
    fn new(releases: bool, skipped_shares: Arc<AtomicBool>) -> Self {
        Self {
            releases,
            skipped_shares,
            ..Self::default()
        }
    }
}

impl<S: Store> End<S> {
    /// Marks the link left as it ends, however it ended, and its machine
    /// out of reach, and gives back how it ended.
    fn leave(&self, result: io::Result<Ending>) -> io::Result<Ending> {
        let left = now();
        // Had the link died outright, the last minute's mark would stand.
        let _ = self
            .store
            .mark_linked(&self.other, self.hearing.linked_until());
        // A handover is the link's work done, so only a drop counts as one.
        if !matches!(result, Ok(Ending::HandedOver)) {
            let _ = self.store.mark_ended(&self.other, left);
        }
        // A link the node keeps drops as the node runs, so it sees the
        // drop as it is. One running apart may drop while the node is
        // stopped, so it marks when the link went and leaves the seeing
        // to the node: its next start, or its easing, which times a mark
        // it hasn't seen from the drop.
        let seen = (self.housed == Housed::InItsNode).then_some(left);
        store::out_of_reach(&self.store, &self.other, left, seen);
        result
    }

    /// Says what opens a link, once the hellos are said: the Have, then
    /// this end's machine, the carries its node ordered, its sessions and
    /// the talks it follows.
    async fn open(&mut self, writing: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        let messages = self.store.message_ids(self.reach);
        let living = Some(self.living().into_keys().collect());
        send(writing, &Frame::Have { messages, living }).await?;
        self.send_machine(writing).await?;
        self.send_carries(writing).await?;
        self.send_presence(writing).await?;
        self.send_follow(writing).await
    }

    /// Tells the other end of each of what changed in the store, in turn,
    /// until one can't be told.
    async fn changed_all(
        &mut self,
        gathered: Vec<Change>,
        writing: &mut (impl AsyncWrite + Unpin),
    ) -> io::Result<()> {
        for change in gathered {
            self.changed(change, writing).await?;
        }
        Ok(())
    }

    /// Tells the other end of what changed in the store that is this
    /// end's to tell.
    async fn changed(
        &mut self,
        change: Change,
        writing: &mut (impl AsyncWrite + Unpin),
    ) -> io::Result<()> {
        match change {
            Change::Message(id) => self.send_messages(vec![id], writing).await,
            Change::Presence => self.send_presence(writing).await,
            Change::Rescan => self.catch_up(writing).await,
            // A machine's facts and carries cross only from that machine
            // itself, and this end measures its own facts as it sends them.
            Change::Carries(machine) if machine == self.here => self.send_carries(writing).await,
            Change::Machine(_) | Change::Carries(_) => Ok(()),
        }
    }

    /// Tells the other end of the carries this end's machine's node
    /// ordered, if the two ends tell each other so.
    async fn send_carries(&mut self, writing: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        if !self.shows {
            return Ok(());
        }
        let carries = self
            .store
            .told_carries()
            .remove(&self.here)
            .unwrap_or_default();
        send(writing, &Frame::Carries { carries }).await
    }

    /// Keeps the carries the other end's machine's node ordered, which
    /// only that end tells of: refused where the two ends never agreed to
    /// tell them, or where they are more than a machine may tell at once.
    fn keep_carries(&self, carries: &[Value]) {
        let record = format!("{}'s carries", self.other);
        if !self.shows {
            self.kept(
                &record,
                Err(refusal(format!(
                    "{} told of its carries where this link doesn't show them",
                    self.other
                ))),
            );
            return;
        }
        if let Err(why) = kept::fits(carries) {
            self.kept(&record, Err(refusal(why)));
            return;
        }
        self.kept(&record, self.store.put_told_carries(&self.other, carries));
    }

    /// Keeps a message the other end sent, unless its body names another
    /// id or it speaks as a receptor, which only its own machine's node
    /// does.
    fn hear_message(&mut self, id: &str, body: &Value) {
        self.has.insert(id.to_owned());
        let as_receptor = body
            .get("from")
            .and_then(Value::as_str)
            .is_some_and(stimuli::speaks_as_receptor);
        let refused = if as_receptor {
            "only its own machine's node speaks as a receptor"
        } else if body.get("id").and_then(Value::as_str) == Some(id) {
            let kept = self.store.put_message(id, body);
            // A message carried again, as after a link drops, woke its
            // sessions when it first came.
            if matches!(kept, Ok(true)) {
                self.wake_parked(id, body);
            }
            return self.kept(id, kept.map(drop));
        } else {
            "its body names another id"
        };
        self.kept(id, Err(io::Error::new(io::ErrorKind::InvalidData, refused)));
    }

    /// Wakes each session parked on this machine that the message `id`,
    /// carried in saying `body`, gives a turn, as its sender would have
    /// on its own machine: no sender here sees this machine's markers. A
    /// worker's link wakes only for a session's own, and only so many a
    /// minute ([`crate::wake`]).
    fn wake_parked(&mut self, id: &str, body: &Value) {
        let Some((park, booted)) = self.parked.clone() else {
            return;
        };
        let at = now();
        while self
            .worker_wakes
            .front()
            .is_some_and(|woke| at - woke >= 60_000)
        {
            self.worker_wakes.pop_front();
        }
        let room = self
            .worker
            .then(|| crate::wake::WORKER_WAKES.saturating_sub(self.worker_wakes.len()));
        let presences = self.store.presences();
        let (woken, left) = crate::wake::wake_for(
            &park,
            booted,
            id,
            body,
            &presences,
            |earlier| self.store.message(earlier),
            room,
        );
        for session in woken {
            if self.worker {
                self.worker_wakes.push_back(at);
            }
            eprintln!("woke {session}, parked, for {id} from {}", self.other);
        }
        if left > 0 {
            eprintln!(
                "left {left} parked for {id} from {}, past a worker's wakes a minute",
                self.other
            );
        }
    }

    async fn take(
        &mut self,
        frame: Frame,
        writing: &mut (impl AsyncWrite + Unpin),
    ) -> io::Result<()> {
        self.hearing.heard = now();
        if self.worker
            && let Some(refused) = self.refused_of_worker(&frame)
        {
            return match refused {
                Refused::Answered(answer) => send(writing, &answer).await,
                Refused::Dropped(why) => {
                    eprintln!("{why}, so it isn't taken");
                    Ok(())
                }
            };
        }
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
            Frame::Message { id, body } if self.worker => {
                self.hear_message(&id, &said_on_worker(body, &self.other));
            }
            Frame::Message { id, body } => self.hear_message(&id, &body),
            Frame::Presence {
                session,
                body,
                leases,
                held,
                started_by,
            } => self.hear_presence(&session, body, leases, held, started_by),
            Frame::Ended { session } => {
                self.carried.remove(&session);
                self.store.remove_presence(&session, &self.other)?;
            }
            // Taken only while no act runs, so it is never dropped as busy.
            Frame::Share(say) => {
                self.share(move |exchange| exchange.hear(say));
            }
            Frame::Machine(said) => self.keep_machine(*said),
            Frame::Carries { carries } => self.keep_carries(&carries),
            Frame::Start { ask, start } if self.worker => self.answer_gate(ask, &start),
            Frame::Start { ask, start } => self.answer(ask, start),
            Frame::Steer {
                ask,
                address,
                steer,
                checked,
            } => {
                let reply = self.steered(&address, &steer, checked);
                send(writing, &Frame::Started { ask, reply }).await?;
            }
            Frame::Update { ask } => self.update(ask),
            Frame::Run { ask, run } => self.run_here(ask, run),
            Frame::Ran { ask, ran } => self.runs.ran(ask, ran),
            Frame::Taken { ask } => self.runs.taken(ask),
            // Dropping it stops it.
            Frame::Unrun { ask } => drop(self.runs.here.remove(&ask)),
            Frame::Lend { ask, lend } => self.lend_here(ask, lend),
            Frame::Lent { ask, lent } => self.runs.lent(ask, lent),
            // Dropping it lets the lender's answer go.
            Frame::Unlend { ask } => drop(self.runs.lending.remove(&ask)),
            Frame::Carry {
                ask,
                quest,
                to,
                checked,
            } => self.carry(ask, (quest, to), checked),
            Frame::End { ask, held, by } => self.end(ask, held, &by),
            Frame::Follow { talks } => {
                if let Some(following) = self.following.as_mut() {
                    following.follow(talks);
                }
            }
            Frame::Replies(replies) => {
                if let Some(inboxes) = &self.inboxes {
                    self.kept(&replies.id, steers::keep(inboxes, &replies));
                }
            }
            Frame::HandedOver => self.heard_handed_over(),
            Frame::Started { ask, reply } => {
                // An ask its asker has given up on is answered to nobody.
                if let Some(answer) = self.asks.remove(&ask) {
                    let _ = answer.send(reply);
                }
            }
        }
        Ok(())
    }

    /// Why a worker may not say `frame`, if it may not: it is asked and
    /// asks nothing back, and speaks only for its own sessions
    /// ([`Connection::worker`]). Every frame is named, so one added later
    /// is weighed here rather than let through.
    fn refused_of_worker(&self, frame: &Frame) -> Option<Refused> {
        let other = &self.other;
        match frame {
            // The one start a worker may ask, a gate, is judged as it's
            // answered ([`Self::answer_gate`]).
            Frame::Start { .. } if self.gates.is_some() => None,
            Frame::Start { ask, .. } => Some(self.refused(*ask, "starts nothing for")),
            Frame::Steer { ask, .. } => Some(self.refused(*ask, "takes no steers from")),
            Frame::Update { ask } => Some(self.refused(*ask, "updates nothing for")),
            Frame::Carry { ask, .. } => Some(self.refused(*ask, "carries nothing for")),
            Frame::End { ask, .. } => Some(self.refused(*ask, "ends nothing for")),
            Frame::Run { ask, .. } => Some(Refused::Answered(Box::new(Frame::Ran {
                ask: *ask,
                ran: Ran::Failed(format!("{} runs nothing for a worker", self.here)),
            }))),
            // Following a talk here would read what this machine's
            // sessions say in it.
            Frame::Follow { .. } => Some(Refused::Dropped(format!(
                "{other}, a worker, asked to follow talks here"
            ))),
            Frame::Presence { session, body, .. } if !speaks_for(body, other) => {
                Some(Refused::Dropped(format!(
                    "{other} spoke for {session}, a session of another machine's, which a worker can't"
                )))
            }
            Frame::Presence { session, body, .. } => self.taken_name(session, body).map(|name| {
                Refused::Dropped(format!(
                    "{other} spoke for {session} as {name}, which a session living elsewhere \
                     answers to, so a worker can't"
                ))
            }),
            Frame::Replies(replies) if !self.carries(&replies.address) => {
                Some(Refused::Dropped(format!(
                    "{other} replied in a talk of {}, which isn't a session of its own",
                    replies.address
                )))
            }
            Frame::Message { id, body } => self.message_refused(id, body),
            // Quest ids and reasons aren't work.
            Frame::Carries { .. } => Some(Refused::Dropped(format!(
                "{other}, a worker, told of carries, which aren't work"
            ))),
            Frame::Hello(_)
            | Frame::Have { .. }
            | Frame::Ended { .. }
            | Frame::Share(_)
            | Frame::Machine(_)
            | Frame::Replies(_)
            | Frame::HandedOver
            | Frame::Started { .. }
            // What a run this machine asked of the worker says, and how a
            // run the worker asked here, which is refused, is let go.
            | Frame::Ran { .. }
            | Frame::Taken { .. }
            | Frame::Unrun { .. }
            // A worker's lend is answered by this machine's lender, which
            // knows the worker by this link, never by what the lend says;
            // a lend this machine asked of the worker answers nothing.
            | Frame::Lend { .. }
            | Frame::Lent { .. }
            | Frame::Unlend { .. } => None,
        }
    }

    /// The answer refusing a worker's `ask`: this machine `does` nothing
    /// for it, such as "starts nothing for".
    fn refused(&self, ask: u64, does: &str) -> Refused {
        Refused::Answered(Box::new(Frame::Started {
            ask,
            reply: Err(format!("{} {does} a worker", self.here)),
        }))
    }

    /// Why a worker's message `id` isn't taken, if it isn't: one this
    /// machine already holds isn't written over, nothing that steers this
    /// machine's sessions is taken ([`mesh::steers`]), and only a session
    /// the worker holds a presence for now speaks through it, so it can put
    /// no words in the mouth of a session another machine holds, nor of
    /// one between presences, nor say what no session said.
    fn message_refused(&self, id: &str, body: &Value) -> Option<Refused> {
        let other = &self.other;
        if self.store.message(id).is_some() {
            return Some(Refused::Dropped(format!(
                "{other} said {id} again, which a worker can't write over"
            )));
        }
        if let Some(steers) = mesh::steers(body, |earlier| self.store.message(earlier)) {
            return Some(Refused::Dropped(format!(
                "{other} said {id}, {steers}, which a worker can't say here"
            )));
        }
        let Some(from) = body.get("from").and_then(Value::as_str) else {
            return Some(Refused::Dropped(format!(
                "{other} said {id} from no session, which a worker can't"
            )));
        };
        // A sender is named by an address its presence lists, as the
        // mesh writes it, or by its id.
        let (mut its_own, mut taken) = (false, false);
        for presence in self.store.presences() {
            if !answers_to(&presence, from) {
                continue;
            }
            if presence.elsewhere.as_deref() == Some(other.as_str()) {
                its_own = true;
            } else if stands(&presence) {
                taken = true;
            }
        }
        if taken {
            return Some(Refused::Dropped(format!(
                "{other} said {id} as {from}, which a session living elsewhere answers to, \
                 so a worker can't"
            )));
        }
        (!its_own).then(|| {
            Refused::Dropped(format!(
                "{other} said {id} as {from}, which holds no presence through it, so a worker can't"
            ))
        })
    }

    /// The name a worker's presence of `session`, saying `body`, shares
    /// with a session living or parked here or through another link, if it
    /// shares one: its id or any address it answers to. A worker writes its
    /// presence as it likes, so one taking a living session's name would
    /// let it send as that session, and only the label on its words would
    /// tell them apart; a parked one is woken by what is sent it, so its
    /// name is as taken.
    fn taken_name(&self, session: &str, body: &Value) -> Option<String> {
        let its = names(session, body);
        self.store
            .presences()
            .into_iter()
            .filter(|presence| {
                stands(presence) && presence.elsewhere.as_deref() != Some(self.other.as_str())
            })
            .flat_map(|presence| names(&presence.session, &presence.body))
            .find(|name| its.iter().any(|said| said.eq_ignore_ascii_case(name)))
    }

    /// Whether `address` is a session of the other end's own that this
    /// link carries here.
    fn carries(&self, address: &str) -> bool {
        self.carried
            .iter()
            .any(|session| session.eq_ignore_ascii_case(address))
    }

    /// Asks the other machine to start what this machine's node placed
    /// there, or answers why it can't be asked ([`cannot_carry`]).
    async fn ask(&mut self, ask: Ask, writing: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        if let Some(cannot) = cannot_carry(&ask.asked, &self.said) {
            // The asker may have given up; then nobody hears it.
            let _ = ask.answer.send(Err(format!(
                "{}'s mycelium is too old to {cannot}",
                self.other
            )));
            return Ok(());
        }
        let id = self.next_ask;
        self.next_ask += 1;
        if let Asked::Run(run, asker) = ask.asked {
            self.runs
                .asked
                .insert(id, self.follow(id, asker, RunSaid::Unrun(id)));
            // The asker may have given up; then the run is let go.
            let _ = ask.answer.send(Ok(Response::Running {
                machine: self.other.clone(),
            }));
            return send(writing, &Frame::Run { ask: id, run }).await;
        }
        if let Asked::Lend(lend, asker) = ask.asked {
            self.runs
                .borrowed
                .insert(id, self.follow(id, asker, RunSaid::Unlend(id)));
            // A lend is under way as a run is; the asker may have given up.
            let _ = ask.answer.send(Ok(Response::Running {
                machine: self.other.clone(),
            }));
            return send(writing, &Frame::Lend { ask: id, lend }).await;
        }
        self.asks.insert(id, ask.answer);
        let frame = match ask.asked {
            Asked::Start(start) | Asked::Reopen(start) => Frame::Start { ask: id, start },
            Asked::Steer(steer) => Frame::Steer {
                ask: id,
                address: steer.address,
                steer: Value::Object(steer.body),
                checked: steer.checked,
            },
            Asked::Update => Frame::Update { ask: id },
            Asked::Carry { quest, to, checked } => Frame::Carry {
                ask: id,
                quest,
                to,
                checked,
            },
            Asked::End { held, by } => Frame::End { ask: id, held, by },
            Asked::Run(..) => unreachable!("a run is sent above"),
            Asked::Lend(..) => unreachable!("a lend is sent above"),
        };
        send(writing, &frame).await
    }

    /// Hands what the run or lend asked of the other machine as `ask`
    /// says on to `asker`, telling the other end each time a piece is
    /// taken, so it sends one more, and once `asker` lets go, with
    /// `let_go`, so it stops. Answers where the link puts what it says.
    fn follow<T: Piece>(
        &self,
        ask: u64,
        asker: mpsc::Sender<T>,
        let_go: RunSaid,
    ) -> mpsc::UnboundedSender<T> {
        let (said, mut saying) = mpsc::unbounded_channel::<T>();
        let (loop_, other) = (self.runs.saying.clone(), self.other.clone());
        tokio::spawn(async move {
            loop {
                let piece = tokio::select! {
                    piece = saying.recv() => piece.or_else(|| T::dropped(&other)),
                    () = asker.closed() => break,
                };
                // Dropping `asker` tells it the link went.
                let Some(piece) = piece else { return };
                let ends = piece.last();
                if asker.send(piece).await.is_err() {
                    break;
                }
                if ends {
                    return;
                }
                // The link outlives what it carries, or nobody needs this.
                let _ = loop_.send(RunSaid::Taken(ask));
            }
            let _ = loop_.send(let_go);
        });
        said
    }

    /// Hands a lend the other end asks of this machine to its lender, only
    /// when the other end is a worker of this machine and this machine
    /// lends. The lender knows the worker by this link's other end.
    fn lend_here(&mut self, ask: u64, lend: Lend) {
        let lender = if self.worker {
            self.lender
                .clone()
                .ok_or_else(|| format!("{} lends nothing", self.here))
        } else {
            Err(format!(
                "{} lends only to its workers, and {} isn't one",
                self.here, self.other
            ))
        };
        let lender = match lender {
            Ok(lender) => lender,
            Err(why) => {
                eprintln!("{} asked {} to lend, refused: {why}", self.other, self.here);
                let refused = Lent::Refused { status: 403, why };
                // The link outlives this call, so its loop is there to send it.
                let _ = self.runs.saying.send(RunSaid::Lent(ask, refused));
                return;
            }
        };
        let window = Arc::new(tokio::sync::Semaphore::new(BACKLOG));
        let (stop, stopped) = oneshot::channel();
        self.runs.lending.insert(
            ask,
            RunningHere {
                window: window.clone(),
                _stop: stop,
            },
        );
        let answers = lender::answer(&lender, &self.other, lend);
        tokio::spawn(hand_lent(
            ask,
            answers,
            self.runs.saying.clone(),
            (window, stopped),
        ));
    }

    /// Hands a run the other end asks of this machine to its node, only
    /// when the other end is proven to be its machine, as for an update.
    fn run_here(&mut self, ask: u64, run: Run) {
        let run = arrived(run, &self.other);
        let node = if self.proven {
            self.node
                .clone()
                .ok_or_else(|| format!("{} runs nothing over this link", self.here))
        } else {
            Err(format!(
                "{} runs only what is asked over a link its key proved, which this one isn't",
                self.here
            ))
        };
        let node = match node {
            Ok(node) => node,
            Err(why) => {
                eprintln!("{} asked {} to run, refused: {why}", self.other, self.here);
                // The link outlives this call, so its loop is there to send it.
                let _ = self.runs.saying.send(RunSaid::Ran(ask, Ran::Failed(why)));
                return;
            }
        };
        let window = Arc::new(tokio::sync::Semaphore::new(starts::RUN_WINDOW));
        let (stop, stopped) = oneshot::channel();
        self.runs.here.insert(
            ask,
            RunningHere {
                window: window.clone(),
                _stop: stop,
            },
        );
        self.handover.ran = true;
        tokio::spawn(starts::run(
            (node, self.here.clone()),
            run,
            (ask, self.runs.saying.clone()),
            window,
            stopped,
        ));
    }

    /// Sends the other end the answer to a start it asked of this machine.
    async fn answered(
        &mut self,
        ask: u64,
        reply: Reply,
        writing: &mut (impl AsyncWrite + Unpin),
    ) -> io::Result<()> {
        // Work started here has reached this machine, however soon it
        // ends: a minute's keep may never see it running.
        self.handover.ran |= matches!(reply, Ok(Response::Started(_)));
        send(writing, &Frame::Started { ask, reply }).await
    }

    /// Sends the other end what a run said.
    async fn run_said(
        &mut self,
        said: RunSaid,
        writing: &mut (impl AsyncWrite + Unpin),
    ) -> io::Result<()> {
        let frame = match said {
            RunSaid::Ran(ask, ran) => {
                if ran.ends() {
                    self.runs.here.remove(&ask);
                }
                Frame::Ran { ask, ran }
            }
            RunSaid::Taken(ask) => Frame::Taken { ask },
            RunSaid::Unrun(ask) => {
                self.runs.asked.remove(&ask);
                Frame::Unrun { ask }
            }
            RunSaid::Lent(ask, lent) => {
                if lent.ends() {
                    self.runs.lending.remove(&ask);
                }
                Frame::Lent { ask, lent }
            }
            RunSaid::Unlend(ask) => {
                self.runs.borrowed.remove(&ask);
                Frame::Unlend { ask }
            }
        };
        send(writing, &frame).await
    }

    /// Keeps the presence the other end sent of `session`, with the leases
    /// it holds and the id the link holds it by.
    fn hear_presence(
        &mut self,
        session: &str,
        mut body: Value,
        leases: Option<Vec<Value>>,
        held: Option<String>,
        started_by: Option<String>,
    ) {
        // A starter means something only for a hold, and a worker's node is
        // whoever runs on the machine lent, so its word is no book's.
        let started_by =
            started_by.filter(|by| held.is_some() && !self.worker && forage::is_address(by));
        // The held id and the starter are the link's to say, never the
        // presence's.
        if let Some(fields) = body.as_object_mut() {
            fields.remove(mesh::HELD_AS);
            fields.remove(mesh::STARTED_BY_AS);
            if let Some(held) = held {
                fields.insert(mesh::HELD_AS.to_owned(), Value::String(held));
            }
            // Only an address is a cell's, so nothing longer is kept.
            if let Some(started_by) = started_by {
                fields.insert(mesh::STARTED_BY_AS.to_owned(), Value::String(started_by));
            }
        }
        let put = self.store.put_presence(session, &self.other, &body);
        if put.is_ok() {
            self.carried.insert(session.to_owned());
            if let Some(leases) = leases {
                self.kept(
                    session,
                    self.store.put_leases(session, &self.other, &leases),
                );
            }
        }
        self.kept(session, put);
    }

    /// Tells an end that carries steers which talks this end follows, so
    /// it carries on with those whose sessions run on its machine.
    async fn send_follow(&mut self, writing: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        let Some(inboxes) = self.inboxes.clone().filter(|_| self.said.steers) else {
            return Ok(());
        };
        let mut talks = tokio::task::spawn_blocking(move || crate::steer::open_talks(&inboxes))
            .await
            .map_err(io::Error::other)?;
        // A worker hears only of its own sessions' talks, never which talks
        // this machine's sessions have open.
        if self.worker {
            talks.retain(|talk| self.carries(&talk.address));
        }
        if talks.is_empty() {
            return Ok(());
        }
        send(writing, &Frame::Follow { talks }).await
    }

    /// Looks for changes in the quests shared and the talks followed,
    /// away from the link's own work, and sends what it finds.
    async fn look(&mut self, writing: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        if !self.share(Exchange::look) && self.acting.is_some() {
            self.owed.look = true;
        }
        let Some(mut following) = self.following.take() else {
            return Ok(());
        };
        let (following, carried) = tokio::task::spawn_blocking(move || {
            let carried = following.look();
            (following, carried)
        })
        .await
        .map_err(io::Error::other)?;
        self.following = Some(following);
        for replies in carried {
            send(writing, &Frame::Replies(replies)).await?;
        }
        Ok(())
    }

    /// Hands a start the other end asks of this machine to its node, away
    /// from the link's own work, since starting waits on the holder. Its
    /// mark as the person's carry passes only where the other end is
    /// proven to be its machine, since the node reads the carry's cell as
    /// that machine's word.
    fn answer(&self, ask: u64, mut start: Start) {
        start.carries &= self.proven;
        let answering = self.answering.clone();
        let Some(node) = self.node.clone() else {
            let refused = Err(format!("{} starts nothing over this link", self.here));
            // The link outlives this call, so its loop is there to send it.
            let _ = answering.send((ask, refused));
            return;
        };
        let (here, from) = (self.here.clone(), self.other.clone());
        tokio::spawn(async move {
            let reply = starts::answer(node, here, from, start).await;
            // A link that dropped meanwhile has nobody to answer.
            let _ = answering.send((ask, reply));
        });
    }

    /// Answers the one start a worker may ask, a gate this machine names
    /// it for ([`Gates::admit`]) of a quest given to it: run here by the
    /// gate's own path, with its quest and commit alone, and answered as
    /// started, since the
    /// gate's result crosses back as a file of the quest. How the run
    /// ended is said here, never to the worker. Any other start is
    /// refused, and why is said both here and to the worker.
    fn answer_gate(&self, ask: u64, start: &Start) {
        let worker = self.other.clone();
        let admitted = self
            .gates
            .as_ref()
            .ok_or(crate::gate::Refused::Unlisted)
            .and_then(|gates| gates.admit(&worker, start))
            .and_then(|answer| {
                let given = self
                    .exchange
                    .as_ref()
                    .is_some_and(|exchange| exchange.gives(answer.quest()));
                // Refused, the answer's place is freed as it drops.
                if given {
                    Ok(answer)
                } else {
                    Err(crate::gate::Refused::NotGiven)
                }
            });
        let reply = match admitted {
            Err(why) => {
                eprintln!("gate: refused a start from {worker}, a worker: {why}");
                Err(format!(
                    "{} starts nothing for a worker but a gate it answers: {why}",
                    self.here
                ))
            }
            Ok(answer) => match answer.command().spawn() {
                Err(error) => {
                    eprintln!("gate: couldn't start {worker}'s gate: {error}");
                    Err(format!("{} couldn't start the gate", self.here))
                }
                Ok(running) => {
                    eprintln!(
                        "gate: answering {worker}'s gate of {} in {}",
                        answer.commit(),
                        answer.quest()
                    );
                    let started = Started {
                        machine: self.here.clone(),
                        held: format!("gate-{}", &answer.commit()[..12]),
                        folder: PathBuf::new(),
                        attach: Vec::new(),
                    };
                    // Its place is held until it ends, at the latest at
                    // its limit.
                    tokio::spawn(async move {
                        let ended =
                            crate::gate::ended(running, crate::gate::LIMIT, crate::gate::GRACE)
                                .await;
                        eprintln!(
                            "gate: {}'s gate of {} in {} ended: {ended}",
                            answer.worker(),
                            answer.commit(),
                            answer.quest(),
                        );
                    });
                    Ok(Response::Started(started))
                }
            },
        };
        // The link outlives this call, so its loop is there to send it.
        let _ = self.answering.send((ask, reply));
    }

    /// Delivers a steer the other end carries to the session it names here,
    /// saying it reached this machine, where this end carries steers.
    fn steered(&mut self, address: &str, steer: &Value, checked: bool) -> Result<Response, String> {
        let here = self.here.clone();
        match self.following.as_mut() {
            Some(following) => {
                following
                    .deliver(address, steer, checked)
                    .map(|()| Response::Steered {
                        machine: here,
                        by: None,
                        carries_on: false,
                        carries_with: false,
                    })
            }
            None => Err(format!("{here} carries no steers over this link")),
        }
    }

    /// Hands an update the other end asks of this machine to its node, away
    /// from the link's own work, since it takes minutes: only when the
    /// other end is proven to be its machine, so nobody else may ask.
    fn update(&self, ask: u64) {
        let answering = self.answering.clone();
        let refused = if self.proven {
            self.node
                .clone()
                .ok_or_else(|| format!("{} updates over no link", self.here))
        } else {
            Err(format!(
                "{} updates only when asked over a link its key proved, which this one isn't",
                self.here
            ))
        };
        let node = match refused {
            Ok(node) => node,
            Err(why) => {
                eprintln!(
                    "{} asked {} to update, refused: {why}",
                    self.other, self.here
                );
                // The link outlives this call, so its loop is there to send it.
                let _ = answering.send((ask, Err(why)));
                return;
            }
        };
        eprintln!("{} asked {} to update", self.other, self.here);
        let here = self.here.clone();
        tokio::spawn(async move {
            let reply = starts::update(node, here).await;
            // A link that dropped meanwhile has nobody to answer.
            let _ = answering.send((ask, reply));
        });
    }

    /// Carries `quest`'s sessions off this machine to `to` as the other
    /// end asks for the person, away from the link's own work, since it
    /// waits minutes for them to end: only when the other end is proven
    /// to be its machine and its node `checked` the person asked, so no
    /// agent there carries a session here, and never as anyone but the
    /// person.
    fn carry(&self, ask: u64, (quest, to): (String, String), checked: bool) {
        let answering = self.answering.clone();
        let hands = if !self.proven {
            Err(format!(
                "{} carries a session only when asked over a link its key proved, which \
                 this one isn't",
                self.here
            ))
        } else if !checked {
            Err(format!(
                "{} didn't say it saw the person ask, so {} carries none of {quest}'s \
                 sessions for it",
                self.other, self.here
            ))
        } else {
            self.node
                .clone()
                .zip(self.places.clone())
                .ok_or_else(|| format!("{} carries no sessions over this link", self.here))
        };
        let (node, places) = match hands {
            Ok(hands) => hands,
            Err(why) => {
                eprintln!(
                    "{} asked {} to carry {quest} to {to}, refused: {why}",
                    self.other, self.here
                );
                // The link outlives this call, so its loop is there to send it.
                let _ = answering.send((ask, Err(why)));
                return;
            }
        };
        eprintln!(
            "{} asked {} to carry {quest} to {to}, for the person",
            self.other, self.here
        );
        let here = self.here.clone();
        tokio::spawn(async move {
            let reply = starts::carry(node, places, here, (quest, to)).await;
            // A link that dropped meanwhile has nobody to answer.
            let _ = answering.send((ask, reply));
        });
    }

    /// Ends the session held here as `held` for good, as the other end
    /// asks for `by`, away from the link's own work, since the session has
    /// a grace to save what it holds: only when the other end is proven
    /// to be its machine, as for a carry, and only for the person.
    fn end(&self, ask: u64, held: String, by: &hold::end::Asker) {
        let answering = self.answering.clone();
        let folder = if self.proven {
            self.held
                .clone()
                .ok_or_else(|| format!("{} ends no held sessions over this link", self.here))
        } else {
            Err(format!(
                "{} ends a held session only when asked over a link its key proved, which \
                 this one isn't",
                self.here
            ))
        };
        // A session's end is proved only where both run, by that node's
        // record of orchestrations, so across a link only the person's is.
        let may = folder.and_then(|folder| match by {
            hold::end::Asker::Person => Ok(folder),
            hold::end::Asker::Session(address) => Err(format!(
                "{} ends a held session over a link only for the person, not for {address}, \
                 whose orchestration is proved only where both run",
                self.here
            )),
        });
        let folder = match may {
            Ok(folder) => folder,
            Err(why) => {
                eprintln!(
                    "{} asked {} to end {held} for {by}, refused: {why}",
                    self.other, self.here
                );
                // The link outlives this call, so its loop is there to send it.
                let _ = answering.send((ask, Err(why)));
                return;
            }
        };
        eprintln!("{} asked {} to end {held}, for {by}", self.other, self.here);
        let here = self.here.clone();
        tokio::spawn(async move {
            let id = held.clone();
            let ended =
                tokio::task::spawn_blocking(move || hold::end::end(&folder, &id, hold::end::GRACE))
                    .await;
            let reply = match ended {
                Ok(Ok(ending)) => Ok(Response::Ended {
                    machine: here,
                    held,
                    ending,
                }),
                Ok(Err(error)) => Err(format!("{held} didn't end on {here}: {error}")),
                Err(error) => Err(format!("its end stopped: {error}")),
            };
            // A link that dropped meanwhile has nobody to answer.
            let _ = answering.send((ask, reply));
        });
    }

    /// Keeps what the other end says its machine is, which only it can
    /// say: a machine named otherwise is refused, and it is ready while
    /// this link carries it, whatever it claims. A worker is withheld the
    /// quests above those shared with it, and so is a machine that declares
    /// nothing of itself, as a node does for a while as it restarts, even
    /// while what it last declared stands in for nothing.
    fn keep_machine(&mut self, mut said: Machine) {
        if said.name != self.other {
            self.kept(
                &said.name,
                Err(refusal(format!("{} spoke for another machine", self.other))),
            );
            return;
        }
        said.state = Readiness::Ready;
        // Whether it is a worker is this end's to say: a worker saying it
        // isn't one, or a peer saying it is, changes nothing.
        said.worker = self.worker;
        // A node restarting, or too slow to answer its end, has it say its
        // machine with nothing declared; what the machine last declared
        // stands for a while, so a carry isn't refused for ground the
        // machine still stands on. A node that answered declaring nothing
        // is taken at its word at once.
        let said_nothing = std::mem::take(&mut said.declares_nothing);
        said.declared = self.heard_declared.heard(
            &self.other,
            (said.declared.take(), said_nothing),
            tokio::time::Instant::now(),
        );
        let withheld = declared_heard::withheld_above(
            said.declared.as_ref(),
            self.heard_declared.standing_in(),
        );
        match self.exchange.as_mut() {
            Some(exchange) => exchange.withhold_above(withheld),
            None if self.acting.is_some() => self.withheld.push(withheld),
            None => {}
        }
        self.kept(&said.name, self.store.put_machine(&said));
    }

    /// Says what this end's machine is, measured now, to an end that hears
    /// it. Measuring reads a few files, or runs `sysctl` on a Mac, so it
    /// runs away from the link's own work.
    async fn send_machine(&mut self, writing: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        if !self.said.facts {
            return Ok(());
        }
        let (here, realm) = (self.here.clone(), self.realm.clone());
        let (declared, kit, saturation) = match &self.declared {
            Declaring::File(file, kit, saturation) => {
                (Some(file.clone()), kit.clone(), saturation.clone())
            }
            Declaring::Node(node) => match measured_by(node).await {
                // The node answered, so a machine declaring nothing truly
                // does, and the far end mustn't keep what it declared.
                Some(mut machine) => {
                    machine.declares_nothing = machine.declared.is_none();
                    return self.say_machine(machine, writing).await;
                }
                // Nothing the account's files declare is said for it:
                // anything running as the account could have written them.
                None => (None, None, None),
            },
        };
        // A file read here answers at once, so a machine whose file declares
        // nothing, or can't be read, truly declares nothing; only a node too
        // slow to answer leaves the far end keeping what it last declared.
        let read_its_file = declared.is_some();
        let sources = facts::Sources {
            declared,
            ..facts::Sources::here()
        };
        let machine = tokio::task::spawn_blocking(move || {
            let mut machine = facts::measure_in(&here, &realm, &sources);
            machine.declares_nothing = read_its_file && machine.declared.is_none();
            if let Some(band) = &saturation {
                band.say(machine.room.as_mut());
            }
            machine.kit = kit.as_ref().and_then(hold::start::Kit::said);
            machine
        })
        .await
        .map_err(io::Error::other)?;
        self.say_machine(machine, writing).await
    }

    /// Says `machine`, this end's own, with the health this end watches.
    async fn say_machine(
        &mut self,
        mut machine: Machine,
        writing: &mut (impl AsyncWrite + Unpin),
    ) -> io::Result<()> {
        machine.health = self
            .health
            .told
            .as_ref()
            .and_then(|health| health.borrow().clone());
        if let Some(held) = self.held.as_deref() {
            machine.holds = Some(hold::running(held));
        }
        self.handover.leaves = machine
            .declared
            .as_ref()
            .and_then(|declared| declared.leaves_at);
        send(writing, &Frame::Machine(Box::new(machine))).await
    }

    /// Offers the other end whatever it still lacks, then says again that
    /// what this link carried here stands, and what this end's machine is
    /// now, and whether it has handed over.
    async fn keep(&mut self, writing: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        self.hearing.arrived(self.arrived.load(Ordering::Relaxed));
        if self.hearing.gone(now()) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{} has said nothing for {SILENT_AFTER:?}", self.other),
            ));
        }
        // A message is sent as the store's watch tells of it, and a watch
        // can miss one and say nothing: a Mac's messages stopped crossing
        // for 38 minutes on 22 November while its link stood. The mark
        // below is where the next link begins its offer, so what was
        // missed is offered first, or it would never cross.
        self.catch_up(writing).await?;
        self.keep_carried()?;
        // A starter the node named none for is asked about again here too,
        // so one booked late still crosses while no presence changes.
        self.ask_starters(&self.living());
        self.send_machine(writing).await?;
        self.hand_over(writing).await
    }

    /// Asks this machine's node, away from the link's loop, who started
    /// those of the `living` sessions held here that are due an ask.
    fn ask_starters(&mut self, living: &BTreeMap<String, (Arc<Value>, Option<String>)>) {
        if let Some(node) = self.node.as_deref() {
            let held: Vec<&str> = living
                .values()
                .filter_map(|(_, held)| held.as_deref())
                .collect();
            self.starters.ask(node, &held);
        }
    }

    /// Keeps what an ask of this machine's node found of who started its
    /// held sessions, telling once of each way it couldn't say until it
    /// says again, and sends the presences whose starter it named.
    async fn learned(
        &mut self,
        found: started::Learned,
        writing: &mut (impl AsyncWrite + Unpin),
    ) -> io::Result<()> {
        match self.starters.learned(found, tokio::time::Instant::now()) {
            Some(started::Told::Unasked(why)) => eprintln!(
                "who started {}'s held sessions goes unsaid to {} until its node says: {why}",
                self.here, self.other
            ),
            Some(started::Told::Again) => eprintln!(
                "who started {}'s held sessions is said to {} again, as its node says",
                self.here, self.other
            ),
            None => {}
        }
        self.send_presence(writing).await
    }

    /// Tells an end that can release this machine that it has handed over
    /// (`handover::handed_over`), once it has, saying first all it holds
    /// of the quests it shares.
    async fn hand_over(&mut self, writing: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        if !self.handover.releases {
            return Ok(());
        }
        let held_as: Vec<Option<String>> = self.living().into_values().map(|(_, id)| id).collect();
        let held = self.held.as_deref().map(hold::held).unwrap_or_default();
        let running = handover::running(&held_as, &held);
        self.handover.ran |= running > 0;
        let Handover { leaves, ran, .. } = self.handover;
        // While an act holds the exchange, the next keep tries again.
        if !handover::handed_over(leaves, jiff::Timestamp::now(), ran, running)
            || self.acting.is_some()
        {
            return Ok(());
        }
        if self.share(Exchange::look) {
            self.owed.handed_over = true;
            return Ok(());
        }
        send(writing, &Frame::HandedOver).await
    }

    /// Takes the other end's word that its machine has handed over, if this
    /// end holds all it said: frames cross in the order they are said, so
    /// every file it holds has been heard of, and asked for if wanted.
    fn heard_handed_over(&mut self) {
        let lacking = self.exchange.as_ref().map_or(0, Exchange::waiting);
        if self.handover.skipped_shares.load(Ordering::Relaxed) {
            eprintln!(
                "{} has handed over, but sent shares of a kind this node can't read, so it can't \
                 tell its files all reached this machine; update this node to read them",
                self.other
            );
        } else if lacking == 0 {
            self.handover.done = true;
        } else {
            eprintln!(
                "{} has handed over, but {lacking} of its files haven't reached this machine yet",
                self.other
            );
        }
    }

    /// Has the exchange act on the disk, away from the link's loop, which
    /// goes on taking asks, answers and messages, and keeping the link,
    /// while it runs: acts over many large ledgers take minutes on a busy
    /// machine. What it says is sent once it's done ([`Self::acted`]).
    /// False, with nothing done, where the link shares nothing or another
    /// act holds the exchange.
    fn share(
        &mut self,
        act: impl FnOnce(&mut Exchange) -> io::Result<Vec<Say>> + Send + 'static,
    ) -> bool {
        let Some(mut exchange) = self.exchange.take() else {
            return false;
        };
        self.acting = Some(tokio::task::spawn_blocking(move || {
            let says = act(&mut exchange);
            (exchange, says)
        }));
        true
    }

    /// Takes back the exchange an act `acted` with, and queues what it
    /// says, then what waited on it. A quest that fails to share is told
    /// of and tried again at the next look, rather than ending the link,
    /// which would only meet it again at every redial.
    fn acted(
        &mut self,
        acted: Result<(Exchange, io::Result<Vec<Say>>), tokio::task::JoinError>,
    ) -> io::Result<()> {
        self.acting = None;
        let (mut exchange, says) = acted.map_err(io::Error::other)?;
        for withheld in self.withheld.drain(..) {
            exchange.withhold_above(withheld);
        }
        // Written here, a finding reaches this machine's sessions and, as
        // any message does, the other's.
        for draft in exchange.findings() {
            let (id, body) = mesh::written(&draft, std::process::id());
            self.kept(&id, self.store.put_message(&id, &body).map(drop));
        }
        self.exchange = Some(exchange);
        match says {
            Ok(says) => self.outgoing.extend(says.into_iter().map(Frame::Share)),
            Err(error) => eprintln!("sharing quests with {}: {error}", self.other),
        }
        if std::mem::take(&mut self.owed.handed_over) {
            self.outgoing.push_back(Frame::HandedOver);
        }
        if std::mem::take(&mut self.owed.look) {
            self.share(Exchange::look);
        }
        Ok(())
    }

    /// Sends the first of what the exchange's acts said.
    async fn send_shared(&mut self, writing: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        match self.outgoing.pop_front() {
            Some(frame) => send(writing, &frame).await,
            None => Ok(()),
        }
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
        self.hearing.caught_up.get_or_insert_with(now);
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
        self.handover.ran |= !living.is_empty();
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
        self.ask_starters(&living);
        let holdings = self.store.holdings();
        for (session, (body, held)) in living {
            let started_by = held
                .as_deref()
                .and_then(|held| self.starters.of(held))
                .map(str::to_owned);
            let said = (
                body,
                mesh::carried_leases(&holdings, &session),
                held,
                started_by,
            );
            if self.sent.get(&session) != Some(&said) {
                // What a session says of itself under the link's fields
                // never goes, so no hearer, however old, keeps it as the
                // link's word.
                let mut body = (*said.0).clone();
                if let Some(fields) = body.as_object_mut() {
                    fields.remove(mesh::HELD_AS);
                    fields.remove(mesh::STARTED_BY_AS);
                }
                send(
                    writing,
                    &Frame::Presence {
                        session: session.clone(),
                        body,
                        leases: Some(said.1.clone()),
                        held: said.2.clone(),
                        started_by: said.3.clone(),
                    },
                )
                .await?;
                self.sent.insert(session, said);
            }
        }
        Ok(())
    }

    /// This machine's living sessions, as their presences say, with the id
    /// a holder here keeps each by, if one does. To an end that keeps a
    /// parked session as parked, those parked this boot count too: each
    /// is started again for what is sent to it, so its address stands on
    /// every machine, not only this one, while it waits.
    fn living(&self) -> BTreeMap<String, (Arc<Value>, Option<String>)> {
        let held = self.held.as_deref().map(hold::held).unwrap_or_default();
        let boot = self.said.parks.then(store::folders::boot_id);
        self.store
            .presences()
            .into_iter()
            .filter(|presence| {
                presence.elsewhere.is_none()
                    && (presence.lives
                        || boot
                            .as_ref()
                            .is_some_and(|boot| parked_in(&presence.body, boot.as_deref())))
            })
            .map(|presence| {
                let id = mesh::held_by(&presence, &held).map(|one| one.id.clone());
                (presence.session, (presence.body, id))
            })
            .collect()
    }

    /// Says again that every session this link carried here still stands,
    /// and marks the link as standing until the other end was last heard.
    fn keep_carried(&self) -> io::Result<()> {
        for session in &self.carried {
            if let Err(error) = self.store.keep_presence(session, &self.other) {
                eprintln!("couldn't keep {session} from {}: {error}", self.other);
            }
        }
        self.store
            .mark_linked(&self.other, self.hearing.linked_until())
    }
}

/// Whether `presence` stands for a session: one living, or one parked this
/// boot, here or, as its machine says, on another.
fn stands(presence: &store::Presence) -> bool {
    presence.lives || parked_in(&presence.body, store::folders::boot_id().as_deref())
}

/// Whether a presence saying `body` was parked in this machine's `boot`,
/// for its holder to start again. A park of an earlier boot has no holder
/// left to start it; where the machine names no boot, or the presence
/// none, any park is taken as this boot's, as a carry takes it.
fn parked_in(body: &Value, boot: Option<&str>) -> bool {
    body.get("state").and_then(Value::as_str) == Some("parked")
        && match (boot, body.pointer("/owner/bootId").and_then(Value::as_str)) {
            (Some(boot), Some(began)) => boot == began,
            _ => true,
        }
}

/// Writes `frame` as a line, ending the link when the other end takes none
/// of it for [`SILENT_AFTER`]. A write that never goes through would hold
/// the link past every keep that judges silence: a cloud machine's end of
/// a link went on holding its claim on a node restarted at 18:48 on 25
/// November, so the node's new link was refused there for minutes.
async fn send(writing: &mut (impl AsyncWrite + Unpin), frame: &Frame) -> io::Result<()> {
    let mut line = serde_json::to_vec(frame).map_err(io::Error::other)?;
    line.push(b'\n');
    let written = async {
        writing.write_all(&line).await?;
        writing.flush().await
    };
    tokio::time::timeout(SILENT_AFTER, written)
        .await
        .unwrap_or_else(|_| {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("the other end took nothing for {SILENT_AFTER:?}"),
            ))
        })
}

/// How far back a link to `machine` reaches for messages, in milliseconds
/// since the epoch: to when this end last heard `machine` over a link
/// ([`Hearing::linked_until`]), a minute early for what was in flight when
/// it was last heard, and never past the mesh's week.
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

/// When this end last heard the other end, and whether the other says
/// its machine each minute, so that hearing nothing for longer means it
/// is gone.
struct Hearing {
    /// Whether the other end says its machine each minute.
    watched: bool,
    /// When this end last heard the other say anything, in milliseconds
    /// of the wall clock.
    heard: i64,
    /// The mark the link began from, which its reach was read from, if
    /// the machines were linked before.
    began: Option<i64>,
    /// When this end first sent the other what it lacked of its reach.
    caught_up: Option<i64>,
}

impl Hearing {
    /// Hearing from now, `watched` or not, for a link begun from the mark
    /// `began`.
    fn from_now(watched: bool, began: Option<i64>) -> Self {
        Self {
            watched,
            heard: now(),
            began,
            caught_up: None,
        }
    }

    /// Takes the other end as heard at `at`, when one of its frames
    /// arrived, if that is later than this end last took one.
    fn arrived(&mut self, at: i64) {
        self.heard = self.heard.max(at);
    }

    /// Whether the other end is gone by `now`.
    fn gone(&self, now: i64) -> bool {
        self.watched && silent(self.heard, now)
    }

    /// Until when the link is known to have stood, to mark as the next
    /// link's reach for messages: when the other end was last heard, not
    /// now. A link whose other end went silent stands for minutes before
    /// it is judged gone, and what this end sent in them was written into
    /// a connection that carried nothing. Marked now, the next link would
    /// reach back only to a minute before the end. On 22 November,
    /// messages sent a minute before a Mac left the network didn't cross
    /// once it came back, which, read from the source, this explains.
    /// Marked at the last word heard, they are offered again; what the
    /// other end already holds it mostly names in its Have, and a message
    /// that comes twice is kept once.
    ///
    /// The mark moves past the one the link began from only once the
    /// other end has been heard a keep after this end caught it up, so a
    /// link that drops again before its catch-up could cross leaves the
    /// next one reaching as far back as it did. Hearing the other end is
    /// no proof that what this end wrote arrived, only that the link stood
    /// a minute past it, which the minute's slack in the reach covers.
    fn linked_until(&self) -> i64 {
        let stood = self
            .caught_up
            .is_some_and(|at| self.heard >= at.saturating_add(millis(KEEP_EVERY)));
        match self.began {
            Some(began) if !stood => began.min(self.heard),
            _ => self.heard,
        }
    }
}

/// Whether an end last heard at `heard` has said nothing too long by
/// `now`, both in milliseconds of the wall clock. The wall clock, since it
/// runs on while a machine sleeps: a link thawed after its machine slept
/// learns at its first keep that the other end is long gone, and ends, so
/// the machine takes the new link its dialler has been trying.
fn silent(heard: i64, now: i64) -> bool {
    now.saturating_sub(heard) >= millis(SILENT_AFTER)
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// What a session `session`, whose presence says `body`, answers to: the
/// addresses its presence lists and its own id.
fn names(session: &str, body: &Value) -> Vec<String> {
    let mut names = forage::addresses(body.get("addresses").unwrap_or(&Value::Null));
    names.push(session.to_owned());
    names
}

/// Whether `presence`'s session answers to `name`, as [`names`] says.
fn answers_to(presence: &store::Presence, name: &str) -> bool {
    names(&presence.session, &presence.body)
        .iter()
        .any(|its| its.eq_ignore_ascii_case(name))
}

/// Whether a presence a worker said speaks only for a session of its own
/// machine, `worker`: one that names no machine, or names that one.
fn speaks_for(body: &Value, worker: &str) -> bool {
    body.get(store::folders::MACHINE)
        .is_none_or(|machine| machine.as_str() == Some(worker))
}

/// A worker's message `body` as this machine keeps it: labelled by the
/// link's own name for the worker, `worker`, never by anything the
/// worker says, so every session reading it reads it as a worker's word.
/// Its text opens with where it was said, and `worker` names the machine.
fn said_on_worker(mut body: Value, worker: &str) -> Value {
    if let Some(said) = body.as_object_mut() {
        if let Some(Value::String(text)) = said.get_mut("text") {
            *text = format!("[said on {worker}, a worker machine] {text}");
        }
        said.insert("worker".to_owned(), Value::String(worker.to_owned()));
    }
    body
}

/// Why a worker's frame isn't taken.
enum Refused {
    /// An ask, answered with why.
    Answered(Box<Frame>),
    /// Anything else, let go and said here.
    Dropped(String),
}

fn refusal(reason: String) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, reason)
}

/// A piece of what a run or a lend asked of the other machine says, as
/// the link hands it on.
trait Piece: Send + 'static {
    /// Whether nothing follows it.
    fn last(&self) -> bool;
    /// What its asker hears where the link to `other` drops first, or
    /// none where its asker is better left to see what it was handed end
    /// and judge for itself what that means.
    fn dropped(other: &str) -> Option<Self>
    where
        Self: Sized;
}

impl Piece for Ran {
    fn last(&self) -> bool {
        self.ends()
    }

    fn dropped(other: &str) -> Option<Self> {
        Some(Self::Failed(format!(
            "the link to {other} dropped before the run ended"
        )))
    }
}

impl Piece for Lent {
    fn last(&self) -> bool {
        self.ends()
    }

    /// None, so the node asking sees the lend end with nothing said and
    /// may ask it again, where the lender lets it go with its link, once
    /// nothing of it has reached pi.
    fn dropped(_: &str) -> Option<Self> {
        None
    }
}

/// Sends the other end each piece this machine's lender `answers` for its
/// lend `ask`, through the link's loop at `saying`, one more each time the
/// other end takes one, as its `window` allows. The other end letting go,
/// which drops `stopped`'s sender, lets the lender's answer go.
async fn hand_lent(
    ask: u64,
    mut answers: mpsc::Receiver<Lent>,
    saying: mpsc::UnboundedSender<RunSaid>,
    (window, mut stopped): (Arc<tokio::sync::Semaphore>, oneshot::Receiver<()>),
) {
    loop {
        let permit = tokio::select! {
            permit = window.acquire() => permit,
            _ = &mut stopped => return,
        };
        // The window is never closed, so this only guards.
        let Ok(permit) = permit else { return };
        let lent = tokio::select! {
            lent = answers.recv() => lent,
            _ = &mut stopped => return,
        };
        // The lender always ends with its last piece, so one gone sooner
        // failed, and its asker is told so rather than left waiting.
        let lent = lent.unwrap_or_else(|| Lent::Refused {
            status: 502,
            why: "the lender ended before its answer did".to_owned(),
        });
        permit.forget();
        let ends = lent.ends();
        if saying.send(RunSaid::Lent(ask, lent)).is_err() || ends {
            return;
        }
    }
}

/// `run`, asked over the link from `from`, as this end hands it to its
/// node: one that doesn't say who asks is taken as a strictly fenced
/// agent's, and the machine it came from is the one at the other end,
/// whatever it says.
fn arrived(mut run: Run, from: &str) -> Run {
    run.by
        .get_or_insert(crate::wire::By::Agent(crate::declared::Kind::Strict));
    run.from = Some(from.to_owned());
    run
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::{DuplexStream, ReadHalf, WriteHalf};

    use super::*;
    use crate::declared::Kind;
    use crate::link::membership::{Proven, Realm, VERSION};
    use crate::steer;
    use crate::store::memory::Memory;
    use crate::wire::By;

    #[test]
    fn a_run_from_another_machine_that_doesnt_say_who_asks_runs_strictly_fenced() {
        let said: Run = serde_json::from_value(json!({
            "work": {"needs": [], "prefers": [], "tolerates": [], "may_wait": false},
            "command": ["true"],
            "from": "tern"
        }))
        .expect("a run as an older node says it");
        let run = arrived(said.clone(), "skiff");
        assert_eq!(run.by, Some(By::Agent(Kind::Strict)));
        assert_eq!(
            run.from.as_deref(),
            Some("skiff"),
            "it came from the machine at the other end"
        );
        let person = arrived(
            Run {
                by: Some(By::Person),
                ..said
            },
            "skiff",
        );
        assert_eq!(
            person.by,
            Some(By::Person),
            "as the asking machine judged it"
        );
    }

    /// How long a test waits for a link to carry something before failing.
    /// Each waits on what it waits for, so this only bounds how long a
    /// failure takes to say so; a narrow pipe's backlog, carried a few
    /// bytes a write in a debug build, took past 5 s on a loaded machine.
    const PATIENCE: Duration = Duration::from_secs(30);

    fn realm(machine: &str) -> Realm {
        Realm {
            name: "work".to_owned(),
            machine: machine.to_owned(),
        }
    }

    /// Skiff's end of a link run over a pipe, on `store`, its connection
    /// as `shaped` makes it, sharing `shares`, and the far end's halves of
    /// the pipe.
    fn skiff_over(
        store: Memory,
        shaped: impl FnOnce(Connection) -> Connection + Send + 'static,
        shares: Option<Places>,
    ) -> (
        tokio::task::JoinHandle<io::Result<Ending>>,
        ReadHalf<DuplexStream>,
        WriteHalf<DuplexStream>,
    ) {
        let (near, far) = tokio::io::duplex(1 << 16);
        let (reading, writing) = tokio::io::split(near);
        let skiff_end = tokio::spawn(async move {
            run(
                store,
                &realm("skiff"),
                shaped(connection(reading, writing)),
                shares,
                None,
            )
            .await
        });
        let (far_reading, far_writing) = tokio::io::split(far);
        (skiff_end, far_reading, far_writing)
    }

    fn connection(reading: ReadHalf<DuplexStream>, writing: WriteHalf<DuplexStream>) -> Connection {
        Connection {
            reading: Box::new(reading),
            writing: Box::new(writing),
            keep: Box::new(()),
            said: None,
            releases: false,
            worker: false,
            hello_within: Some(dial::HELLO_WITHIN),
            named: None,
        }
    }

    /// The other end's hello, said `after` this long, as a provider's
    /// program says it once it has made the machine; the end's sender is
    /// kept until then, so the wait is never ended by its closing.
    fn said_after(after: Duration) -> mpsc::UnboundedReceiver<io::Result<Frame>> {
        let (sender, frames) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            let _ = sender.send(Ok(Frame::Hello(realm("kestrel").hello())));
            // Held, so the hello is read before the end seems to close.
            tokio::time::sleep(after).await;
        });
        frames
    }

    // KP0L6A (2026-11-21): a node fell 49 minutes behind the facts a
    // Mac sent it, so a link takes only the newest of those
    // waiting, first, and every other frame in the order it came.
    // Our council on #359: none is judged on facts older than it came
    // behind.
    #[tokio::test]
    async fn a_link_behind_takes_only_the_newest_facts() {
        let (sender, mut frames) = mpsc::unbounded_channel();
        // Measured once, since each measuring reads the clock and the disk.
        let tern = facts::measure("tern", &facts::Sources::nowhere());
        let facts = |cores: u32| {
            let mut machine = tern.clone();
            machine.cpu.cores = Some(cores);
            Ok(Frame::Machine(Box::new(machine)))
        };
        let ended = |session: &str| {
            Ok(Frame::Ended {
                session: session.to_owned(),
            })
        };
        for frame in [facts(1), ended("a"), facts(2), facts(3), ended("b")] {
            sender.send(frame).expect("sent");
        }
        let mut queued = VecDeque::new();
        let mut heard = Vec::new();
        for _ in 0..3 {
            heard.push(
                pull(&mut queued, &mut frames)
                    .await
                    .expect("a frame")
                    .expect("read"),
            );
        }
        assert_eq!(
            heard,
            [facts(3), ended("a"), ended("b")]
                .map(|frame: io::Result<Frame>| frame.expect("a frame")),
            "the newest facts come first, the older are left out, and the rest keep their order"
        );
        sender.send(facts(4)).expect("sent");
        assert_eq!(
            pull(&mut queued, &mut frames)
                .await
                .expect("a frame")
                .expect("read"),
            facts(4).expect("a frame"),
            "facts nothing newer waits behind are taken as they come"
        );
    }

    #[tokio::test]
    async fn an_end_that_closes_before_hello_is_told_from_one_that_said_otherwise() {
        let (_, mut closed) = mpsc::unbounded_channel::<io::Result<Frame>>();
        let unheard = hello(&mut closed, None)
            .await
            .expect_err("nothing was said");
        assert!(
            closed_unheard(&unheard),
            "an end that closed before saying anything brought no link up: {unheard}"
        );
        let (said, mut frames) = mpsc::unbounded_channel();
        said.send(Err(io::Error::other("a frame that can't be read")))
            .expect("sent");
        let unreadable = hello(&mut frames, None).await.expect_err("no hello");
        assert!(
            !closed_unheard(&unreadable),
            "an end that said something said more than its way in: {unreadable}"
        );
    }

    #[test]
    fn only_a_hello_written_to_a_closed_end_is_unheard() {
        for kind in [io::ErrorKind::BrokenPipe, io::ErrorKind::ConnectionReset] {
            let unheard = unheard_write(io::Error::from(kind));
            assert!(
                closed_unheard(&unheard) && unheard.kind() == kind,
                "an end that closed before our hello was taken brought no link up, \
                 and says how it closed: {unheard}"
            );
        }
        let full = unheard_write(io::Error::from(io::ErrorKind::StorageFull));
        assert!(
            !closed_unheard(&full),
            "a hello this end couldn't write isn't the other end's to say: {full}"
        );
    }

    #[tokio::test]
    async fn an_end_that_said_hello_and_closed_before_taking_ours_was_heard() {
        let membership = realm("skiff");
        let greeted = |said: Option<Hello>| {
            let membership = membership.clone();
            async move {
                let (mut near, far) = tokio::io::duplex(64);
                drop(far);
                let (saying, mut frames) = mpsc::unbounded_channel();
                if let Some(said) = said {
                    saying.send(Ok(Frame::Hello(said))).expect("sent");
                }
                drop(saying);
                let store = Memory::default();
                greet(&mut near, &mut frames, (&store, &membership), false, None)
                    .await
                    .expect_err("the other end closed")
            }
        };

        let unheard = greeted(None).await;
        assert!(
            closed_unheard(&unheard),
            "an end that closed having said nothing brought no link up: {unheard}"
        );
        let heard = greeted(Some(realm("tern").hello())).await;
        assert!(
            !closed_unheard(&heard),
            "an end whose hello came was reached, so its way in's words aren't taken: {heard}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_hello_is_waited_on_as_long_as_the_way_in_says() {
        let setting_up = Duration::from_secs(10 * 60);
        let heard = hello(&mut said_after(setting_up), None).await;
        assert_eq!(
            heard.expect("heard").machine,
            "kestrel",
            "a way in that ends itself when it can't is waited on however long"
        );
        let given_up = hello(&mut said_after(setting_up), Some(dial::HELLO_WITHIN)).await;
        assert_eq!(
            given_up
                .map(|hello| hello.machine)
                .map_err(|error| error.kind()),
            Err(io::ErrorKind::TimedOut),
            "and one that answers at once only for its while"
        );
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
            run(
                store,
                &realm(machine),
                connection(reading, writing),
                shares,
                None,
            )
            .await
            .map(|_| ())
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
    fn apart_since(skiff: &Memory, kestrel: &Memory, ms_ago: i64) {
        let until = now() - ms_ago;
        skiff
            .mark_linked("kestrel", until)
            .expect("a memory store marks");
        kestrel
            .mark_linked("skiff", until)
            .expect("a memory store marks");
    }

    /// Two stores joined by a link over an in-memory pipe of `buffer` bytes,
    /// skiff's end first.
    fn linked(
        skiff: &Memory,
        kestrel: &Memory,
        buffer: usize,
    ) -> (
        tokio::task::JoinHandle<io::Result<()>>,
        tokio::task::JoinHandle<io::Result<()>>,
    ) {
        let (near, far) = tokio::io::duplex(buffer);
        (end(skiff, "skiff", near), end(kestrel, "kestrel", far))
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
        let skiff = Memory::default();
        let kestrel = Memory::default();
        let before = recent(1000, "WCJQSG", 0);
        let theirs = recent(900, "20KSI7", 0);
        let stale = format!("{}-OLD-1-0", now() - millis(REACH) - 1000);
        said(&skiff, &before, "@20KSI7");
        said(&skiff, &stale, "@20KSI7");
        said(&kestrel, &theirs, "@WCJQSG");

        // Apart longer than the mesh keeps anything, so the week is the reach.
        apart_since(&skiff, &kestrel, 2 * millis(REACH));
        let _link = linked(&skiff, &kestrel, 1 << 16);
        until(&kestrel, |store| store.message(&before).is_some()).await;
        until(&skiff, |store| store.message(&theirs).is_some()).await;

        let after = recent(0, "4UHC5Q", 0);
        said(&skiff, &after, "#quest/4uhc5q");
        until(&kestrel, |store| store.message(&after).is_some()).await;
        assert_eq!(
            kestrel.message(&stale),
            None,
            "a message past reach stays behind"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_message_no_watch_told_of_crosses_at_the_next_keep() {
        let skiff = Memory::default();
        let kestrel = Memory::default();
        apart_since(&skiff, &kestrel, 60_000);
        let _link = linked(&skiff, &kestrel, 1 << 16);
        let heard = recent(0, "WCJQSG", 0);
        said(&skiff, &heard, "@20KSI7");
        until(&kestrel, |store| store.message(&heard).is_some()).await;

        let missed = recent(0, "E1EV6B", 0);
        let body = json!({"id": missed, "to": ["@CIJMZQ"], "text": "missed"});
        skiff.miss(&missed, body);
        let mut changes = kestrel.changes().expect("a memory store tells its changes");
        let waited = tokio::time::timeout(2 * KEEP_EVERY, async {
            while kestrel.message(&missed).is_none() {
                changes.changes.recv().await;
            }
        })
        .await;
        assert!(
            waited.is_ok(),
            "a message the watch never told of didn't cross at the next keep"
        );
    }

    #[tokio::test]
    async fn what_is_said_to_the_machine_stays_on_it() {
        let skiff = Memory::default();
        let kestrel = Memory::default();
        let hold = recent(30, "16V6F4", 0);
        let ended = recent(20, "16V6F4", 1);
        let next = recent(10, "WCJQSG", 0);
        said(&skiff, &hold, "#all");
        let answer = json!({"id": ended, "to": ["@16V6F4"], "re": hold, "text": "parked"});
        skiff.send_here(&ended, answer);
        said(&skiff, &next, "@20KSI7");

        apart_since(&skiff, &kestrel, 60_000);
        let _link = linked(&skiff, &kestrel, 1 << 16);
        until(&kestrel, |store| store.message(&next).is_some()).await;
        assert_eq!(
            kestrel.message(&hold),
            None,
            "a hold to the machine stays on it"
        );
        assert_eq!(kestrel.message(&ended), None, "and so do its answers");
    }

    #[tokio::test]
    async fn two_ends_with_a_week_each_catch_up_through_a_narrow_pipe() {
        let skiff = Memory::default();
        let kestrel = Memory::default();
        let many = 2000;
        for n in 0..many {
            said(&skiff, &recent(5000, "WCJQSG", n), "@20KSI7");
            said(&kestrel, &recent(5000, "20KSI7", n), "@WCJQSG");
        }
        // A pipe narrower than one message, so neither end can write its
        // backlog without the other reading.
        apart_since(&skiff, &kestrel, 60_000);
        let _link = linked(&skiff, &kestrel, 64);
        until(&skiff, |store| store.message_ids(0).len() == 2 * many).await;
        until(&kestrel, |store| store.message_ids(0).len() == 2 * many).await;
    }

    #[tokio::test]
    async fn a_first_link_carries_only_what_is_said_from_then_on() {
        let skiff = Memory::default();
        let kestrel = Memory::default();
        let before = recent(1000, "WCJQSG", 0);
        said(&skiff, &before, "#repo/walgit-rs");

        skiff.start_here("launcher", json!({"session": "launcher"}), true);
        let _link = linked(&skiff, &kestrel, 1 << 16);
        // The launcher arriving says the link stands.
        until(&kestrel, |store| !store.presences().is_empty()).await;
        let after = recent(0, "WCJQSG", 1);
        said(&skiff, &after, "#repo/walgit-rs");
        until(&kestrel, |store| store.message(&after).is_some()).await;
        assert_eq!(
            kestrel.message(&before),
            None,
            "talk from before the machines linked stays"
        );
    }

    #[tokio::test]
    async fn a_living_session_is_carried_let_go_when_it_ends_and_unreachable_once_the_link_drops() {
        let skiff = Memory::default();
        let kestrel = Memory::default();
        kestrel.start_here("walgit", json!({"session": "walgit"}), true);
        kestrel.start_here("gone", json!({"session": "gone"}), false);

        let (near, _far) = linked(&skiff, &kestrel, 1 << 16);
        until(&skiff, |store| !store.presences().is_empty()).await;
        let carried = skiff.presences();
        assert_eq!(
            carried.len(),
            1,
            "only the living session is carried: {carried:?}"
        );
        assert_eq!(carried[0].elsewhere.as_deref(), Some("kestrel"));

        kestrel.start_here("walgit", json!({"session": "walgit"}), false);
        until(&skiff, |store| store.presences().is_empty()).await;

        skiff.start_here("launcher", json!({"session": "launcher"}), true);
        until(&kestrel, |store| {
            store.presences().iter().any(|p| p.elsewhere.is_some())
        })
        .await;
        // Stopping skiff's end closes its half of the pipe, so the Mac's
        // end sees the link drop, and keeps what skiff carried out of
        // reach rather than gone.
        near.abort();
        until(&kestrel, |store| {
            store
                .presences()
                .iter()
                .any(|p| p.session == "launcher" && p.unreachable_since.is_some())
        })
        .await;
    }

    #[tokio::test]
    async fn a_session_carried_to_the_other_machine_stands_where_it_runs_on_both() {
        let skiff = Memory::default();
        let kestrel = Memory::default();
        // Carried from skiff, it ended there and runs on the Mac.
        skiff.start_here("moved", json!({"session": "moved"}), false);
        kestrel.start_here("moved", json!({"session": "moved"}), true);
        skiff.hold_here("moved", json!({"session": "moved", "leases": []}));
        kestrel.hold_here(
            "moved",
            json!({"session": "moved", "leases": [{"kind": "change", "subject": "a"}]}),
        );

        let _link = linked(&skiff, &kestrel, 1 << 16);
        until(&skiff, |store| {
            store
                .presences()
                .iter()
                .any(|p| p.elsewhere.as_deref() == Some("kestrel") && p.lives)
        })
        .await;
        until(&skiff, |store| {
            store
                .holdings()
                .iter()
                .any(|holding| holding["machine"] == "kestrel")
        })
        .await;
        let mac = kestrel.presences();
        assert!(
            matches!(mac.as_slice(), [one] if one.elsewhere.is_none() && one.lives),
            "the Mac keeps its own, its end on skiff never carried back: {mac:?}"
        );
    }

    #[tokio::test]
    async fn each_end_says_what_its_machine_is_which_stays_out_of_reach_once_the_link_drops() {
        let skiff = Memory::default();
        let kestrel = Memory::default();
        kestrel.start_here("walgit", json!({"session": "walgit"}), true);

        let (_near, far) = linked(&skiff, &kestrel, 1 << 16);
        // A machine says what it is before its sessions, so once one is
        // carried, so is what it said.
        until(&skiff, |store| !store.presences().is_empty()).await;
        let said = skiff.machines();
        assert_eq!(
            said.iter()
                .map(|one| (one.name.as_str(), one.state))
                .collect::<Vec<_>>(),
            [("kestrel", Readiness::Ready)],
            "the far machine is kept as ready while linked"
        );
        assert_eq!(
            said[0].arch,
            std::env::consts::ARCH,
            "as it measured itself"
        );

        far.abort();
        until(&skiff, |store| {
            store
                .presences()
                .iter()
                .all(|p| p.unreachable_since.is_some())
        })
        .await;
        assert_eq!(
            skiff
                .machines()
                .iter()
                .map(|one| (one.name.as_str(), one.state))
                .collect::<Vec<_>>(),
            [("kestrel", Readiness::Unreachable)],
            "what it said stays, as of when the link last carried it"
        );
        let carried = skiff.presences();
        assert!(
            !carried.is_empty()
                && carried
                    .iter()
                    .all(|p| p.body["unreachableSeen"] == json!(p.unreachable_since)),
            "an end not apart from its node sees the drop as it is: {carried:?}"
        );
        assert_eq!(skiff.endings("kestrel", 0), 1, "and the drop is counted");
    }

    #[tokio::test]
    async fn a_word_of_its_machines_own_health_moving_is_said_at_once() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let skiff = Memory::default();
        let kestrel = Memory::default();
        let slot = own_health::Slot::new(None);
        let (near, far) = tokio::io::duplex(1 << 16);
        let _near = end(&skiff, "skiff", near);
        let starting = Starting {
            node: dir.path().join("node.sock"),
            held: dir.path().join("held"),
            asking: None,
            steers: dir.path().join("steer"),
            parked: dir.path().join("parked"),
            health: Some(slot.subscribe()),
            declared: Some(dir.path().join("declared.json")),
            kit: None,
            saturation: None,
            lender: None,
            gates: None,
        };
        let (reading, writing) = tokio::io::split(far);
        let kestrel_end = kestrel.clone();
        let _far = tokio::spawn(async move {
            run(
                kestrel_end,
                &realm("kestrel"),
                connection(reading, writing),
                None,
                Some(starting),
            )
            .await
            .map(|_| ())
        });
        until(&skiff, |store| !store.machines().is_empty()).await;
        assert_eq!(skiff.machines()[0].health, None, "unknown until said");

        let health = own_health::Health {
            health: "watch".to_owned(),
            parts: BTreeMap::from([("code".to_owned(), "watch".to_owned())]),
        };
        slot.send_replace(Some(health.clone()));
        // Far sooner than the minute's keep, which would say it anyway.
        until(&skiff, |store| {
            store.machines()[0].health.as_ref() == Some(&health)
        })
        .await;
    }

    #[tokio::test]
    async fn a_kept_end_names_the_realms_kit_its_node_was_given() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let kit = dir.path().join("kit");
        std::fs::create_dir_all(kit.join("bin")).expect("the kit's bin is made");
        std::fs::write(kit.join("bin/gcloud"), "").expect("the kit holds gcloud");
        let carried = kit_heard(dir.path(), hold::start::Kit::Carried(kit.clone())).await;
        assert_eq!(
            carried.tools,
            ["gcloud"],
            "its tools, where its node carries it"
        );
        let why = "its bin may be written by others".to_owned();
        let refused = kit_heard(
            dir.path(),
            hold::start::Kit::Refused {
                given: kit.clone(),
                why: why.clone(),
            },
        )
        .await;
        assert_eq!(
            refused,
            facts::Kit {
                path: kit.display().to_string(),
                tools: Vec::new(),
                refused: Some(why),
            },
            "and none, with why, where its node refused it"
        );
    }

    #[tokio::test]
    async fn a_kept_end_says_its_nodes_verdict_in_the_room_it_measures() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        // Its host tells it a room, at a pressure each reading sets and
        // mostly with no verdict, as a host older than the band does, so
        // the room it measures is known here.
        let host_says = dir.path().join("osprey.json");
        std::fs::write(
            dir.path().join("declared.json"),
            serde_json::json!({
                "reach": [], "outlives": [], "costs_money": false, "taints": [],
                "within": {"machine": "osprey", "file": host_says}
            })
            .to_string(),
        )
        .expect("declared");
        let telling = |cpu: f64, saturated: Option<bool>| {
            let now = jiff::Timestamp::now();
            let room = crate::room::Room {
                said_at: now.to_string(),
                memory_available: 8 << 30,
                pressure: crate::room::Pressure {
                    cpu,
                    memory: 0.0,
                    io: 0.0,
                },
                takes: crate::room::Takes {
                    builds: 1,
                    vm_tests: 1,
                },
                over: Some(Vec::new()),
                saturated,
            };
            let said = crate::within::Said {
                at: now,
                power: None,
                room: Some(room),
            };
            crate::within::tell(&host_says, &said).expect("told");
        };
        telling(crate::placement::SATURATES_AT + 5.0, None);
        let band = Arc::new(crate::placement::Saturation::default());
        assert_eq!(
            saturated_heard(dir.path(), Some(band.clone())).await,
            Some(true),
            "a link its node keeps says the node's verdict"
        );
        assert_eq!(
            saturated_heard(dir.path(), None).await,
            None,
            "where it holds no band, it says none"
        );
        telling(
            f64::midpoint(
                crate::placement::UNSATURATES_UNDER,
                crate::placement::SATURATES_AT,
            ),
            None,
        );
        assert_eq!(
            saturated_heard(dir.path(), Some(band.clone())).await,
            Some(true),
            "inside the band, the link holds the verdict the band last judged"
        );
        assert_eq!(
            saturated_heard(
                dir.path(),
                Some(Arc::new(crate::placement::Saturation::default()))
            )
            .await,
            Some(false),
            "and a band that never saturated holds it free there"
        );
        telling(crate::placement::SATURATES_AT + 5.0, Some(false));
        assert_eq!(
            saturated_heard(dir.path(), Some(band)).await,
            Some(false),
            "a host that says its own verdict is heard, not the band's"
        );
    }

    /// The verdict skiff hears in kestrel's room over a link kestrel keeps,
    /// holding `saturation`.
    async fn saturated_heard(
        dir: &std::path::Path,
        saturation: Option<Arc<crate::placement::Saturation>>,
    ) -> Option<bool> {
        let skiff = Memory::default();
        let kestrel = Memory::default();
        let (near, far) = tokio::io::duplex(1 << 16);
        let _near = end(&skiff, "skiff", near);
        let starting = Starting {
            node: dir.join("node.sock"),
            held: dir.join("held"),
            asking: None,
            steers: dir.join("steer"),
            parked: dir.join("parked"),
            health: None,
            declared: Some(dir.join("declared.json")),
            kit: None,
            saturation,
            lender: None,
            gates: None,
        };
        let (reading, writing) = tokio::io::split(far);
        let kestrel_end = kestrel.clone();
        let _far = tokio::spawn(async move {
            run(
                kestrel_end,
                &realm("kestrel"),
                connection(reading, writing),
                None,
                Some(starting),
            )
            .await
            .map(|_| ())
        });

        until(&skiff, |store| {
            store
                .machines()
                .first()
                .is_some_and(|kestrel| kestrel.room.is_some())
        })
        .await;
        skiff.machines()[0].room.as_ref().expect("a room").saturated
    }

    /// The kit skiff hears of kestrel over a link kestrel keeps, its node
    /// having taken `kit` as it did.
    async fn kit_heard(dir: &std::path::Path, kit: hold::start::Kit) -> facts::Kit {
        let skiff = Memory::default();
        let kestrel = Memory::default();
        let (near, far) = tokio::io::duplex(1 << 16);
        let _near = end(&skiff, "skiff", near);
        let starting = Starting {
            node: dir.join("node.sock"),
            held: dir.join("held"),
            asking: None,
            steers: dir.join("steer"),
            parked: dir.join("parked"),
            health: None,
            declared: Some(dir.join("declared.json")),
            kit: Some(kit),
            saturation: None,
            lender: None,
            gates: None,
        };
        let (reading, writing) = tokio::io::split(far);
        let kestrel_end = kestrel.clone();
        let _far = tokio::spawn(async move {
            run(
                kestrel_end,
                &realm("kestrel"),
                connection(reading, writing),
                None,
                Some(starting),
            )
            .await
            .map(|_| ())
        });

        until(&skiff, |store| {
            store
                .machines()
                .first()
                .is_some_and(|kestrel| kestrel.kit.is_some())
        })
        .await;
        skiff.machines()[0].kit.clone().expect("a kit is heard")
    }

    #[tokio::test]
    async fn a_dialled_end_says_what_its_node_measures_its_machine_to_be() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let declared = dir.path().join("declared.json");
        std::fs::write(
            &declared,
            r#"{"reach": [], "outlives": [], "costs_money": false,
                "taints": [{"key": "node-says", "effect": "never", "why": "its node's file"}]}"#,
        )
        .expect("the node's declaration is written");
        let node = dir.path().join("node.sock");
        let _node = measuring_node(&node, &declared);
        let skiff = dialled_by_kestrel(dir.path(), node);

        until(&skiff, |store| {
            store.machines().first().is_some_and(|kestrel| {
                kestrel
                    .declared
                    .as_ref()
                    .is_some_and(|said| said.taints.iter().any(|taint| taint.key == "node-says"))
            })
        })
        .await;
    }

    #[tokio::test]
    async fn a_dialled_end_whose_node_doesnt_answer_says_only_what_it_measures() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let skiff = dialled_by_kestrel(dir.path(), dir.path().join("node.sock"));

        until(&skiff, |store| !store.machines().is_empty()).await;
        assert!(
            skiff.machines()[0].declared.is_none(),
            "with no node to ask, nothing kestrel declares is said"
        );
    }

    /// A link skiff dialled takes kestrel's node's asks of skiff only once
    /// the node says it carries them, and `run` says kestrel's facts only
    /// after, so the node never reads skiff ready, and places a start
    /// there, with no link to ask it through.
    #[tokio::test]
    async fn a_dialled_link_takes_its_asks_once_its_node_carries_them() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let path = dir.path().join("node.sock");
        let listener = tokio::net::UnixListener::bind(&path).expect("the node binds");
        let starting = dialled_starting(dir.path(), path);
        let asking = asked_of(Some(&starting), "skiff");
        tokio::pin!(asking);
        assert!(
            still_waiting(&mut asking).await,
            "the asks aren't taken before the node is called"
        );
        let (stream, _) = tokio::time::timeout(PATIENCE, listener.accept())
            .await
            .expect("the link asks its node in time")
            .expect("the node takes the link's call");
        let (reading, mut writing) = stream.into_split();
        let mut node = BufReader::new(reading).lines();
        let first = node.next_line().await.expect("it asks").expect("a line");
        assert!(first.contains("Carry"), "the link asks to carry: {first}");
        assert!(
            still_waiting(&mut asking).await,
            "the asks aren't taken before the node says it carries them"
        );
        let answered = tokio::time::Instant::now();
        let carrying = serde_json::to_string(&Reply::Ok(Response::Carrying)).expect("a reply");
        writing
            .write_all(format!("{carrying}\n").as_bytes())
            .await
            .expect("the node answers");
        let (asked, carrying) = tokio::time::timeout(PATIENCE, &mut asking)
            .await
            .expect("the asks are taken in time");
        assert!(
            answered.elapsed() < ASKS_TAKEN_WITHIN,
            "taken as the node says it carries them, not at the end of the wait"
        );
        assert!(
            asked.is_some() && carrying.is_some(),
            "the link carries the asks"
        );
    }

    /// Whether `asking` is still waiting, polled once without waiting on it.
    async fn still_waiting(asking: &mut std::pin::Pin<&mut impl Future>) -> bool {
        let polled = tokio::time::timeout(Duration::ZERO, asking).await;
        polled.is_err()
    }

    /// A link whose node isn't there to call goes on at once, rather than
    /// waiting out [`ASKS_TAKEN_WITHIN`] for a node that can't take them.
    #[tokio::test]
    async fn a_dialled_link_whose_node_isnt_there_goes_on_at_once() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let starting = dialled_starting(dir.path(), dir.path().join("node.sock"));
        let began = tokio::time::Instant::now();
        let asking = asked_of(Some(&starting), "skiff");
        let (asked, carrying) = tokio::time::timeout(PATIENCE, asking)
            .await
            .expect("the link goes on in time");
        assert!(
            began.elapsed() < ASKS_TAKEN_WITHIN,
            "no node to call ends the wait at once"
        );
        assert!(
            asked.is_some() && carrying.is_some(),
            "the link still carries the asks, and asks its node again"
        );
    }

    // Our council, on #408's fix-forward (2026-11-24): a link another
    // machine dialled runs apart from the node and may drop while it is
    // stopped, so its drop must not be marked as seen.
    #[tokio::test]
    async fn a_dialled_end_marks_its_drop_unseen_for_its_node_to_see() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (skiff, kestrel) = (Memory::default(), Memory::default());
        skiff.start_here("walgit", json!({"session": "walgit"}), true);
        let (near, far) = tokio::io::duplex(1 << 16);
        let near_end = end(&skiff, "skiff", near);
        let starting = dialled_starting(dir.path(), dir.path().join("node.sock"));
        let (reading, writing) = tokio::io::split(far);
        let kestrel_end = kestrel.clone();
        let _far = tokio::spawn(async move {
            run(
                kestrel_end,
                &realm("kestrel"),
                connection(reading, writing),
                None,
                Some(starting),
            )
            .await
            .map(|_| ())
        });
        until(&kestrel, |store| !store.presences().is_empty()).await;

        near_end.abort();
        until(&kestrel, |store| {
            store
                .presences()
                .iter()
                .all(|p| p.unreachable_since.is_some())
        })
        .await;
        let carried = kestrel.presences();
        assert!(
            !carried.is_empty()
                && carried
                    .iter()
                    .all(|p| p.body.get("unreachableSeen").is_none()),
            "marked when the link went, and left for the node to see: {carried:?}"
        );
    }

    /// How kestrel's end starts on a link skiff dialled, apart from
    /// kestrel's node on `node`, as SSH starts it.
    fn dialled_starting(dir: &std::path::Path, node: PathBuf) -> Starting {
        Starting {
            node,
            held: dir.join("held"),
            asking: Some(Asking::Dialled),
            steers: dir.join("steer"),
            parked: dir.join("parked"),
            health: None,
            declared: None,
            kit: None,
            saturation: None,
            lender: None,
            gates: None,
        }
    }

    /// Skiff's store, hearing kestrel's end of a link skiff dialled, which
    /// runs apart from kestrel's node on `node`, as SSH starts it.
    fn dialled_by_kestrel(dir: &std::path::Path, node: PathBuf) -> Memory {
        let skiff = Memory::default();
        let (near, far) = tokio::io::duplex(1 << 16);
        let near_end = end(&skiff, "skiff", near);
        let starting = dialled_starting(dir, node);
        let (reading, writing) = tokio::io::split(far);
        tokio::spawn(async move {
            let _near_end = near_end;
            run(
                Memory::default(),
                &realm("kestrel"),
                connection(reading, writing),
                None,
                Some(starting),
            )
            .await
            .map(|_| ())
        });
        skiff
    }

    /// A node on `socket` that answers only what it measures its machine
    /// to be, reading what that declares from `declared`, and hangs up on
    /// any other ask.
    fn measuring_node(
        socket: &std::path::Path,
        declared: &std::path::Path,
    ) -> tokio::task::JoinHandle<()> {
        let listener = tokio::net::UnixListener::bind(socket).expect("the node listens");
        let declared = declared.to_owned();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (reading, mut writing) = stream.into_split();
                let Ok(Some(line)) = BufReader::new(reading).lines().next_line().await else {
                    continue;
                };
                if serde_json::from_str::<crate::wire::Request>(&line).ok()
                    != Some(crate::wire::Request::Machine)
                {
                    continue;
                }
                let sources = facts::Sources {
                    declared: Some(declared.clone()),
                    ..facts::Sources::nowhere()
                };
                let measured = facts::measure_in("kestrel", "work", &sources);
                let reply: Reply = Ok(Response::Machine(Box::new(measured)));
                let mut said = serde_json::to_vec(&reply).expect("a reply");
                said.push(b'\n');
                writing.write_all(&said).await.expect("the node answers");
            }
        })
    }

    // Our council on the reopen rule (2026-11-19): an end too old to
    // judge a reopening takes one as any start, on looser ground.
    #[test]
    fn a_reopening_is_asked_only_of_an_end_whose_node_judges_one() {
        let start = || Start {
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
        };
        let newer = realm("kestrel").hello();
        let older = Hello {
            reopens: false,
            ..realm("kestrel").hello()
        };
        assert_eq!(cannot_carry(&Asked::Reopen(start()), &newer), None);
        assert_eq!(
            cannot_carry(&Asked::Reopen(start()), &older),
            Some("judge a reopening"),
            "an end that doesn't say it judges one is asked none"
        );
        assert_eq!(
            cannot_carry(&Asked::Start(start()), &older),
            None,
            "it is still asked any other start"
        );
        let startless = Hello {
            starts: false,
            ..realm("kestrel").hello()
        };
        assert_eq!(
            cannot_carry(&Asked::Reopen(start()), &startless),
            Some("judge a reopening"),
            "an end whose node starts no sessions is asked no reopening, whatever it judges"
        );
        assert!(
            serde_json::from_value::<Hello>(
                json!({"machine": "tern", "realm": "work", "version": VERSION})
            )
            .is_ok_and(|said| !said.reopens),
            "an end whose hello doesn't say it judges none"
        );
    }

    /// The presence of `session`, held unless `held` says not, as a far
    /// end says it with `started_by`, the presence itself naming a starter
    /// of its own.
    fn started(session: &str, held: bool, started_by: Option<&str>) -> Frame {
        Frame::Presence {
            session: session.to_owned(),
            body: json!({"session": session, mesh::STARTED_BY_AS: "FORGED"}),
            leases: None,
            held: held.then(|| format!("held-{session}")),
            started_by: started_by.map(str::to_owned),
        }
    }

    /// The starter `store` keeps beside the presence of `session`.
    fn starter_kept(store: &Memory, session: &str) -> Value {
        store
            .presences()
            .into_iter()
            .find(|presence| presence.session == session)
            .map(|presence| presence.body[mesh::STARTED_BY_AS].clone())
            .expect("its presence is kept")
    }

    // A session's starter is the far end's to say beside its presence, as
    // its node's book names it, so a presence naming one of its own is
    // never believed, and the far end's word is kept only as an address,
    // and only for a hold.
    #[tokio::test]
    async fn a_starter_is_kept_as_the_far_end_says_it_never_as_the_presence_does() {
        let skiff = Memory::default();
        let (near, far) = tokio::io::duplex(1 << 16);
        let _skiff_end = end(&skiff, "skiff", near);
        let (_far_reading, mut far_writing) = tokio::io::split(far);
        let longest = "A".repeat(32);
        let longer = "A".repeat(33);
        says(
            &mut far_writing,
            &[
                Frame::Hello(realm("kestrel").hello()),
                Frame::Have {
                    messages: Vec::new(),
                    living: Some(
                        [
                            "booked", "unbooked", "spaced", "longest", "longer", "unheld",
                        ]
                        .map(str::to_owned)
                        .to_vec(),
                    ),
                },
                started("booked", true, Some("OAR001")),
                started("unbooked", true, None),
                started("spaced", true, Some("not an address")),
                started("longest", true, Some(&longest)),
                started("longer", true, Some(&longer)),
                started("unheld", false, Some("OAR001")),
            ],
        )
        .await;
        until(&skiff, |store| store.presences().len() == 6).await;
        let said = |session: &str| starter_kept(&skiff, session);
        assert_eq!(said("booked"), json!("OAR001"), "the far end's starter");
        assert_eq!(said("unbooked"), Value::Null, "none, not the presence's");
        assert_eq!(said("spaced"), Value::Null, "none, as it is no address");
        assert_eq!(
            said("longest"),
            json!(longest),
            "an address as long as one goes"
        );
        assert_eq!(
            said("longer"),
            Value::Null,
            "none, as it is longer than an address"
        );
        assert_eq!(
            said("unheld"),
            Value::Null,
            "none for a session nothing holds"
        );
    }

    #[tokio::test]
    async fn a_worker_names_no_starter() {
        let skiff = Memory::default();
        let (_skiff_end, _far_reading, mut far_writing) = skiff_over(
            skiff.clone(),
            |connection| Connection {
                worker: true,
                ..connection
            },
            None,
        );
        says(
            &mut far_writing,
            &[
                Frame::Hello(realm("tern").hello()),
                Frame::Have {
                    messages: Vec::new(),
                    living: Some(vec!["lent".to_owned()]),
                },
                started("lent", true, Some("OAR001")),
            ],
        )
        .await;
        until(&skiff, |store| !store.presences().is_empty()).await;
        assert_eq!(
            starter_kept(&skiff, "lent"),
            Value::Null,
            "whoever runs on a lent machine runs its node, so its book is nobody's word"
        );
    }

    /// What a stand-in node says of who started a held session, as the
    /// test lets it.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Naming {
        /// It takes the ask and waits to be let answer.
        Waiting,
        /// It names no starter, as before its book's line is written.
        Nobody,
        /// It names OAR001.
        Named,
    }

    /// A node on `node` saying of who started `5e551011` as `naming` says,
    /// and telling `holding` of each such ask as it takes it. The end asks
    /// its node other things too, as what it measures the machine to be,
    /// each over a connection of its own, so the node takes every one and
    /// refuses all but who started what.
    fn naming_node(
        node: &std::path::Path,
        naming: tokio::sync::watch::Receiver<Naming>,
        holding: mpsc::UnboundedSender<()>,
    ) {
        let listener = tokio::net::UnixListener::bind(node).expect("the node listens");
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let mut naming = naming.clone();
                let holding = holding.clone();
                tokio::spawn(async move {
                    let (reading, mut writing) = stream.into_split();
                    let Ok(Some(line)) = BufReader::new(reading).lines().next_line().await else {
                        return;
                    };
                    let asked = serde_json::from_str::<crate::wire::Request>(&line);
                    let reply: Reply = if matches!(asked, Ok(crate::wire::Request::StartedBy(_))) {
                        // The test hears of it whether or not it waits.
                        let _ = holding.send(());
                        let said = naming
                            .wait_for(|naming| *naming != Naming::Waiting)
                            .await
                            .map_or(Naming::Nobody, |naming| *naming);
                        Ok(Response::StartedBy(if said == Naming::Named {
                            BTreeMap::from([("5e551011".to_owned(), "OAR001".to_owned())])
                        } else {
                            BTreeMap::new()
                        }))
                    } else {
                        Err(format!("not a request this node answers: {line}"))
                    };
                    let mut said = serde_json::to_vec(&reply).expect("a reply");
                    said.push(b'\n');
                    let _ = writing.write_all(&said).await;
                });
            }
        });
    }

    /// Skiff's store, and both ends of a link between it and kestrel, whose
    /// node on `node` holds `0e50` as `5e551011`.
    fn holding_kestrel(
        dir: &std::path::Path,
        node: PathBuf,
    ) -> (Memory, [tokio::task::JoinHandle<io::Result<()>>; 2]) {
        let kestrel = Memory::default();
        let held = dir.join("held");
        std::fs::create_dir(&held).expect("the held folder");
        let holds = hold::Held {
            id: "5e551011".to_owned(),
            command: vec!["pi".to_owned()],
            folder: dir.to_owned(),
            holder: std::process::id(),
            pid: 4242,
            parked: false,
            since: "2026-11-22T06:00:00Z".to_owned(),
            windows: Vec::new(),
        };
        std::fs::write(
            held.join("5e551011.json"),
            serde_json::to_vec(&holds).expect("a record"),
        )
        .expect("the hold is kept");
        std::fs::write(held.join("5e551011.sock"), "").expect("its socket stands");
        kestrel.start_here(
            "0e50",
            json!({"session": "0e50", "owner": {"pid": 4242}}),
            true,
        );
        let skiff = Memory::default();
        let (near, far) = tokio::io::duplex(1 << 16);
        let near = end(&skiff, "skiff", near);
        let starting = Starting {
            node,
            held,
            asking: None,
            steers: dir.join("steer"),
            parked: dir.join("parked"),
            health: None,
            declared: None,
            kit: None,
            saturation: None,
            lender: None,
            gates: None,
        };
        let (reading, writing) = tokio::io::split(far);
        let far = tokio::spawn(async move {
            run(
                kestrel,
                &realm("kestrel"),
                connection(reading, writing),
                None,
                Some(starting),
            )
            .await
            .map(|_| ())
        });
        (skiff, [near, far])
    }

    /// Whether `store` keeps OAR001 as the starter of `0e50`.
    fn named(store: &Memory) -> bool {
        store
            .presences()
            .iter()
            .any(|presence| presence.body[mesh::STARTED_BY_AS] == "OAR001")
    }

    // A node slow to say who started a held session holds back nothing on
    // the link: its presence goes while the ask is out, and again with the
    // starter once the node names one. On a paused clock, time moves only
    // while nothing can run, so a link holding the presence back for the
    // ask would wait out the whole of its answering time, and one that
    // doesn't waits none.
    #[tokio::test(start_paused = true)]
    async fn a_held_sessions_presence_goes_before_its_node_names_its_starter_and_again_after() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let node = dir.path().join("node.sock");
        let (naming, named_as) = tokio::sync::watch::channel(Naming::Waiting);
        let (holding, mut held) = mpsc::unbounded_channel();
        naming_node(&node, named_as, holding);
        let (skiff, _ends) = holding_kestrel(dir.path(), node);

        held.recv().await.expect("the node hears the ask");
        let asked_at = tokio::time::Instant::now();
        until(&skiff, |store| !store.presences().is_empty()).await;
        assert!(
            asked_at.elapsed() < started::ANSWERED_WITHIN,
            "the presence goes while the ask is out, not once it is given up on"
        );
        assert_eq!(
            starter_kept(&skiff, "0e50"),
            Value::Null,
            "and before the node has said"
        );
        naming.send(Naming::Named).expect("the node is let answer");
        until(&skiff, named).await;
    }

    // A starter booked after the node's first answer is said by the next
    // minute's keep, though nothing of the presence changes to ask again.
    #[tokio::test(start_paused = true)]
    async fn a_starter_booked_after_the_first_ask_is_said_by_the_next_keep() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let node = dir.path().join("node.sock");
        let (naming, named_as) = tokio::sync::watch::channel(Naming::Nobody);
        let (holding, mut held) = mpsc::unbounded_channel();
        naming_node(&node, named_as, holding);
        let (skiff, _ends) = holding_kestrel(dir.path(), node);

        held.recv().await.expect("the node hears the first ask");
        until(&skiff, |store| !store.presences().is_empty()).await;
        assert_eq!(
            starter_kept(&skiff, "0e50"),
            Value::Null,
            "the book names none yet"
        );
        naming
            .send(Naming::Named)
            .expect("the book's line is written");
        let mut changes = skiff.changes().expect("a memory store tells its changes");
        // Two keeps, so an ask a keep's answer outran is asked again.
        tokio::time::timeout(2 * KEEP_EVERY + PATIENCE, async {
            while !named(&skiff) {
                changes.changes.recv().await;
            }
        })
        .await
        .expect("the next keep asks again and says the starter");
    }

    #[tokio::test]
    async fn an_older_end_hears_nothing_of_machines_and_none_speaks_for_another() {
        let skiff = Memory::default();
        skiff.start_here("launcher", json!({"session": "launcher"}), true);
        let (near, far) = tokio::io::duplex(1 << 16);
        let _skiff_end = end(&skiff, "skiff", near);
        let (far_reading, mut far_writing) = tokio::io::split(far);

        let older = Hello {
            facts: false,
            starts: false,
            steers: false,
            ..realm("kestrel").hello()
        };
        send(&mut far_writing, &Frame::Hello(older))
            .await
            .expect("the far end says hello");
        let mut lines = BufReader::new(far_reading).lines();
        let heard = heard_until(&mut lines, |frame| matches!(frame, Frame::Presence { .. })).await;
        assert!(
            !heard.iter().any(|frame| matches!(frame, Frame::Machine(_))),
            "an end that doesn't say it knows of machines hears none: {heard:?}"
        );

        let mut tern = facts::measure("tern", &facts::Sources::nowhere());
        tern.state = Readiness::Ready;
        says(
            &mut far_writing,
            &[
                Frame::Have {
                    messages: Vec::new(),
                    living: Some(vec!["walgit".to_owned()]),
                },
                Frame::Machine(Box::new(tern)),
                present("walgit"),
            ],
        )
        .await;
        until(&skiff, |store| {
            store.presences().iter().any(|p| p.session == "walgit")
        })
        .await;
        assert!(
            skiff.machines().is_empty(),
            "kestrel can't say what tern is"
        );
    }

    /// What skiff's end says of its sessions to an end saying `parks` in
    /// its hello, while one runs, one is parked this boot, one was parked
    /// in an earlier boot and one has ended: each presence it sends, up to
    /// the running one, which sorts last.
    async fn presences_said(parks: bool) -> Vec<String> {
        let skiff = Memory::default();
        let boot = store::folders::boot_id().unwrap_or_default();
        let parked = |session: &str, boot: &str| {
            json!({"session": session, "state": "parked",
                   "owner": {"pid": 1, "bootId": boot}})
        };
        skiff.start_here("parked", parked("parked", &boot), false);
        skiff.start_here("rebooted", parked("rebooted", "an-earlier-boot"), false);
        skiff.start_here("ended", json!({"session": "ended", "state": "idle"}), false);
        skiff.start_here("running", json!({"session": "running"}), true);
        let (near, far) = tokio::io::duplex(1 << 16);
        let _skiff_end = end(&skiff, "skiff", near);
        let (far_reading, mut far_writing) = tokio::io::split(far);
        let hello = Hello {
            parks,
            ..realm("tern").hello()
        };
        says(
            &mut far_writing,
            &[
                Frame::Hello(hello),
                Frame::Have {
                    messages: Vec::new(),
                    living: Some(Vec::new()),
                },
            ],
        )
        .await;
        let mut lines = BufReader::new(far_reading).lines();
        heard_until(
            &mut lines,
            |frame| matches!(frame, Frame::Presence { session, .. } if session == "running"),
        )
        .await
        .into_iter()
        .filter_map(|frame| match frame {
            Frame::Presence { session, .. } => Some(session),
            _ => None,
        })
        .collect()
    }

    #[tokio::test]
    async fn a_session_parked_this_boot_stands_for_an_end_that_keeps_it_parked() {
        assert_eq!(
            presences_said(true).await,
            ["parked", "running"],
            "a session parked to be woken for what is sent it is still there to send to, \
             though one parked before the machine restarted, or ended, is not"
        );
        assert_eq!(
            presences_said(false).await,
            ["running"],
            "an older end, which would take a parked session as running, hears only those that run"
        );
    }

    /// The carries skiff's node ordered, as it tells them.
    fn skiffs_carries(why: &str) -> Vec<Value> {
        vec![json!({
            "at": "2026-11-16T08:00:00Z", "by": "WCJQSG", "session": "s-harbour",
            "from": "skiff", "to": "tern", "why": why, "left": false,
        })]
    }

    #[tokio::test]
    async fn a_nodes_carries_cross_to_each_machine_it_links_and_no_further() {
        let (skiff, kestrel) = (Memory::default(), Memory::default());
        for (machine, why) in [
            ("skiff", "it needs the camera"),
            ("tern", "heard from tern"),
        ] {
            skiff
                .put_told_carries(machine, &skiffs_carries(why))
                .expect("the carries are kept");
        }
        let (near, far) = tokio::io::duplex(1 << 16);
        let _near = end(&skiff, "skiff", near);
        let _far = end(&kestrel, "kestrel", far);
        until(&kestrel, |store| {
            store.told_carries().get("skiff") == Some(&skiffs_carries("it needs the camera"))
        })
        .await;

        skiff
            .put_told_carries("skiff", &skiffs_carries("it needs the lens"))
            .expect("the carries are kept");
        until(&kestrel, |store| {
            store.told_carries().get("skiff") == Some(&skiffs_carries("it needs the lens"))
        })
        .await;
        skiff
            .put_told_carries("skiff", &[])
            .expect("the carries are let go");
        until(&kestrel, |store| store.told_carries().is_empty()).await;
        assert_eq!(
            skiff.told_carries().keys().collect::<Vec<_>>(),
            ["tern"],
            "tern's, which skiff only heard, never went on, and kestrel told of none back"
        );
    }

    /// A link no carries cross: whether skiff's end releases, the far end
    /// releases and the far end is a worker; whether the far end says it
    /// shows them; what it tells; and what the link is.
    type Unshown = ((bool, bool, bool), bool, Vec<Value>, &'static str);

    /// The links no carries cross.
    fn unshown_links() -> Vec<Unshown> {
        let many: Vec<Value> = (0..=kept::MOST_TOLD)
            .flat_map(|_| skiffs_carries("one too many"))
            .collect();
        vec![
            (
                (false, false, false),
                false,
                skiffs_carries("older"),
                "an older end",
            ),
            (
                (true, false, false),
                true,
                skiffs_carries("lent"),
                "a link releasing a lent machine",
            ),
            (
                (false, true, false),
                true,
                skiffs_carries("lent"),
                "a lent machine's link",
            ),
            (
                (false, false, true),
                true,
                skiffs_carries("a worker"),
                "a worker's link",
            ),
            (
                (false, false, false),
                true,
                many,
                "an end telling of too many",
            ),
        ]
    }

    #[tokio::test]
    async fn no_carries_cross_unless_both_ends_show_them_and_neither_is_lent_or_a_worker() {
        for ((releases, far_releases, worker), shows, told, said) in unshown_links() {
            let skiff = Memory::default();
            skiff.start_here("launcher", json!({"session": "launcher"}), true);
            skiff
                .put_told_carries("skiff", &skiffs_carries("it needs the camera"))
                .expect("the carries are kept");
            let (near, far) = tokio::io::duplex(1 << 20);
            let store = skiff.clone();
            let _skiff_end = tokio::spawn(async move {
                let (reading, writing) = tokio::io::split(near);
                let connection = Connection {
                    releases,
                    worker,
                    ..connection(reading, writing)
                };
                run(store, &realm("skiff"), connection, None, None).await
            });
            let (far_reading, mut far_writing) = tokio::io::split(far);
            let hello = Hello {
                shows,
                releases: far_releases,
                ..realm("kestrel").hello()
            };
            send(&mut far_writing, &Frame::Hello(hello))
                .await
                .expect("the far end says hello");
            let mut lines = BufReader::new(far_reading).lines();
            let heard =
                heard_until(&mut lines, |frame| matches!(frame, Frame::Presence { .. })).await;
            let crossed = heard
                .iter()
                .any(|frame| matches!(frame, Frame::Carries { .. }));
            assert_eq!(crossed, said.ends_with("too many"), "{said}: {heard:?}");

            says(
                &mut far_writing,
                &[
                    Frame::Have {
                        messages: Vec::new(),
                        living: Some(vec!["walgit".to_owned()]),
                    },
                    Frame::Carries { carries: told },
                    present("walgit"),
                ],
            )
            .await;
            until(&skiff, |store| {
                store.presences().iter().any(|p| p.session == "walgit")
            })
            .await;
            assert_eq!(
                skiff.told_carries().keys().collect::<Vec<_>>(),
                ["skiff"],
                "{said}: what kestrel told is refused, and the link carries on"
            );
        }
    }

    #[tokio::test]
    async fn a_sessions_changes_are_carried_with_it_and_go_when_it_ends() {
        let skiff = Memory::default();
        let kestrel = Memory::default();
        let change = |subject: &str| {
            json!({"kind": "change", "subject": subject, "since": "s",
                   "until": "2999-01-01T00:00:00.000Z"})
        };
        let leases_of = |store: &Memory| -> Vec<Value> {
            store
                .holdings()
                .iter()
                .filter(|holding| holding["machine"] == "kestrel")
                .flat_map(|holding| holding["leases"].as_array().cloned().unwrap_or_default())
                .collect()
        };
        kestrel.start_here("walgit", json!({"session": "walgit"}), true);
        kestrel.hold_here(
            "walgit",
            json!({"session": "walgit", "leases": [change("core.nix:flake.nix")]}),
        );

        let _link = linked(&skiff, &kestrel, 1 << 16);
        until(&skiff, |store| leases_of(store).len() == 1).await;
        assert_eq!(
            leases_of(&skiff)[0]["subject"],
            "core.nix:flake.nix",
            "the Mac session's change is held here, marked as the Mac's"
        );

        kestrel.hold_here("walgit", json!({"session": "walgit", "leases": []}));
        until(&skiff, |store| leases_of(store).is_empty()).await;

        kestrel.hold_here(
            "walgit",
            json!({"session": "walgit", "leases": [change("mycelium:src/mesh.rs")]}),
        );
        until(&skiff, |store| leases_of(store).len() == 1).await;
        kestrel.start_here("walgit", json!({"session": "walgit"}), false);
        until(&skiff, |store| store.holdings().is_empty()).await;
    }

    #[tokio::test]
    async fn what_a_session_landed_crosses_with_its_presence() {
        let skiff = Memory::default();
        let kestrel = Memory::default();
        let landings = json!([{"at": "2026-11-01T13:58:00Z", "ref": "shop/world#2351158",
            "what": "a provider makes machines", "person": "the swarm borrows a machine"}]);
        kestrel.start_here(
            "walgit",
            json!({"session": "walgit", "addresses": ["NTQHL6"],
                   "at": "2026-11-01T14:00:00Z", "landings": landings}),
            true,
        );

        let _link = linked(&skiff, &kestrel, 1 << 16);
        until(&skiff, |store| !store.presences().is_empty()).await;
        let carried =
            mesh::session(&skiff.presences()[0], "skiff").expect("the Mac session reads here");
        assert_eq!(
            serde_json::to_value(&carried.landings).expect("landings write"),
            landings,
            "the Mac session's landings read here as it said them"
        );
    }

    #[tokio::test]
    async fn a_link_coming_back_says_which_sessions_out_of_reach_still_run() {
        let skiff = Memory::default();
        let kestrel = Memory::default();
        skiff.start_here("launcher", json!({"session": "launcher"}), true);
        skiff.start_here("done", json!({"session": "done"}), true);
        let (near, far) = linked(&skiff, &kestrel, 1 << 16);
        until(&kestrel, |store| store.presences().len() == 2).await;
        near.abort();
        far.await
            .expect("the Mac's end runs to its end")
            .expect("a dropped link ends quietly");
        assert!(
            kestrel
                .presences()
                .iter()
                .all(|p| p.unreachable_since.is_some()),
            "both are out of reach: {:?}",
            kestrel.presences()
        );

        skiff.start_here("done", json!({"session": "done"}), false);
        let _back = linked(&skiff, &kestrel, 1 << 16);
        until(&kestrel, |store| {
            matches!(
                store.presences().as_slice(),
                [one] if one.session == "launcher" && one.unreachable_since.is_none()
            )
        })
        .await;
    }

    #[tokio::test]
    async fn an_end_that_names_no_living_sessions_has_every_one_out_of_reach_let_go() {
        let skiff = Memory::default();
        skiff
            .put_presence("walgit", "kestrel", &json!({"session": "walgit"}))
            .expect("a copy is kept");
        store::out_of_reach(&skiff, "kestrel", now(), Some(now()));
        let (near, far) = tokio::io::duplex(1 << 16);
        let _skiff_end = end(&skiff, "skiff", near);
        let (_far_reading, mut far_writing) = tokio::io::split(far);
        // As an end older than naming its living sessions says it.
        for line in [
            serde_json::to_string(&Frame::Hello(realm("kestrel").hello())).expect("a frame"),
            r#"{"Have":{"messages":[]}}"#.to_owned(),
        ] {
            far_writing
                .write_all(format!("{line}\n").as_bytes())
                .await
                .expect("the far end sends");
        }
        until(&skiff, |store| store.presences().is_empty()).await;
    }

    #[tokio::test]
    async fn a_newer_link_to_a_machine_already_linked_takes_over_from_the_older() {
        let skiff = Memory::default();
        let kestrel = Memory::default();
        kestrel.start_here("walgit", json!({"session": "walgit"}), true);
        let (first, _first_far) = linked(&skiff, &kestrel, 1 << 16);
        until(&skiff, |store| !store.presences().is_empty()).await;

        let _second = linked(&skiff, &kestrel, 1 << 16);
        let ended = first.await.expect("the first link's end runs to its end");
        // Kestrel's older end is taken over too, and closes its stream,
        // so skiff's may hear it leave before it sees the newer claim.
        assert!(
            ended.as_ref().map_or_else(
                |why| why
                    .to_string()
                    .contains("a newer link with kestrel took over"),
                |()| true
            ),
            "the older end lets its claim go, as one whose other end is gone: {ended:?}"
        );
        until(&skiff, |store| {
            matches!(
                store.presences().as_slice(),
                [one] if one.session == "walgit" && one.unreachable_since.is_none()
            )
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_write_the_other_end_never_takes_ends_the_link() {
        let (mut near, _far) = tokio::io::duplex(8);
        let sent = send(&mut near, &Frame::Hello(realm("kestrel").hello())).await;
        assert!(
            sent.as_ref()
                .is_err_and(|why| why.kind() == io::ErrorKind::TimedOut),
            "a link held in a write ends as one silent would: {sent:?}"
        );
    }

    #[tokio::test]
    async fn another_realm_is_refused_before_anything_crosses() {
        let skiff = Memory::default();
        said(&skiff, &recent(0, "WCJQSG", 0), "@20KSI7");
        skiff.start_here("launcher", json!({"session": "launcher"}), true);
        let (near, far) = tokio::io::duplex(1 << 16);
        let skiff_end = end(&skiff, "skiff", near);

        let (far_reading, mut far_writing) = tokio::io::split(far);
        let personal = Hello {
            machine: "tern".to_owned(),
            realm: "personal".to_owned(),
            version: VERSION,
            facts: true,
            starts: true,
            steers: true,
            releases: false,
            updates: true,
            runs: true,
            carries: true,
            shows: true,
            ends: true,
            reopens: true,
            lends: true,
            parks: true,
        };
        send(&mut far_writing, &Frame::Hello(personal))
            .await
            .expect("the far end says hello");
        let refused = skiff_end.await.expect("skiff's end runs to its end");
        assert!(refused.is_err(), "skiff refuses the personal realm");

        let mut heard = Vec::new();
        let mut lines = BufReader::new(far_reading).lines();
        while let Some(line) = lines.next_line().await.expect("the pipe reads") {
            heard.push(serde_json::from_str::<Frame>(&line).expect("a frame"));
        }
        assert!(
            matches!(heard.as_slice(), [Frame::Hello(_)]),
            "only skiff's hello crossed: {heard:?}"
        );
    }

    #[tokio::test]
    async fn a_carry_is_taken_only_as_the_asking_node_saw_the_person_ask_it() {
        let (near, far) = tokio::io::duplex(1 << 16);
        let (reading, writing) = tokio::io::split(near);
        let proven = Proven {
            by: realm("skiff"),
            far: "kestrel".to_owned(),
        };
        let skiff = Memory::default();
        let skiff_end = tokio::spawn(async move {
            run(skiff, &proven, connection(reading, writing), None, None).await
        });
        let (far_reading, mut far_writing) = tokio::io::split(far);
        send(&mut far_writing, &Frame::Hello(realm("kestrel").hello()))
            .await
            .expect("the far end says hello");
        let mut lines = BufReader::new(far_reading).lines();
        let mut answer = async |ask: u64, checked: bool| {
            let carry = Frame::Carry {
                ask,
                quest: "QEST-20261115-HARBOR".to_owned(),
                to: "kestrel".to_owned(),
                checked,
            };
            send(&mut far_writing, &carry).await.expect("it asks");
            loop {
                let line = lines.next_line().await.expect("the pipe reads");
                let line = line.expect("skiff answers before it ends");
                if let Frame::Started {
                    ask: answered,
                    reply,
                } = serde_json::from_str(&line).expect("a frame")
                    && answered == ask
                {
                    return reply;
                }
            }
        };
        assert_eq!(
            tokio::time::timeout(PATIENCE, answer(1, false))
                .await
                .expect("in time"),
            Err(
                "kestrel didn't say it saw the person ask, so skiff carries none of \
                 QEST-20261115-HARBOR's sessions for it"
                    .to_owned()
            ),
            "a carry its asking node didn't vouch for as the person's is refused"
        );
        assert_eq!(
            tokio::time::timeout(PATIENCE, answer(2, true))
                .await
                .expect("in time"),
            Err("skiff carries no sessions over this link".to_owned()),
            "one it vouched for goes on to what this end can carry"
        );
        drop(far_writing);
        skiff_end.abort();
    }

    /// A node on `socket` that passes each request it is asked on to what
    /// it returns, and answers each that it starts nothing.
    fn keeping_node(
        socket: &std::path::Path,
    ) -> (
        tokio::task::JoinHandle<()>,
        mpsc::UnboundedReceiver<crate::wire::Request>,
    ) {
        let listener = tokio::net::UnixListener::bind(socket).expect("the node listens");
        let (asked, heard) = mpsc::unbounded_channel();
        let node = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (reading, mut writing) = stream.into_split();
                let Ok(Some(line)) = BufReader::new(reading).lines().next_line().await else {
                    continue;
                };
                if let Ok(request) = serde_json::from_str(&line) {
                    let _ = asked.send(request);
                }
                let reply: Reply = Err("it starts nothing".to_owned());
                let mut said = serde_json::to_vec(&reply).expect("a reply");
                said.push(b'\n');
                writing.write_all(&said).await.expect("the node answers");
            }
        });
        (node, heard)
    }

    /// The start kestrel asks skiff's end of a link, marked as the
    /// person's carry, as skiff's node is handed it, the end judging
    /// kestrel by `membership`.
    async fn a_carry_as_handed_on(membership: impl Membership + 'static) -> Start {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("node.sock");
        let (_node, mut heard) = keeping_node(&socket);
        let (near, far) = tokio::io::duplex(1 << 16);
        let (reading, writing) = tokio::io::split(near);
        let starting = Starting {
            node: socket,
            held: dir.path().join("held"),
            asking: None,
            steers: dir.path().join("steer"),
            parked: dir.path().join("parked"),
            health: None,
            declared: None,
            kit: None,
            saturation: None,
            lender: None,
            gates: None,
        };
        let skiff_end = tokio::spawn(async move {
            run(
                Memory::default(),
                &membership,
                connection(reading, writing),
                None,
                Some(starting),
            )
            .await
            .map(|_| ())
        });
        let (_far_reading, mut far_writing) = tokio::io::split(far);
        let start: Start =
            serde_json::from_value(json!({"work": {}, "command": ["pi"], "carries": true}))
                .expect("a start");
        says(
            &mut far_writing,
            &[
                Frame::Hello(realm("kestrel").hello()),
                Frame::Start { ask: 1, start },
            ],
        )
        .await;
        // The end asks its node what this machine is too, which isn't
        // what this is about.
        let relayed = async {
            loop {
                match heard.recv().await {
                    Some(crate::wire::Request::Relayed { from, start }) => break (from, start),
                    Some(_) => {}
                    None => panic!("its node is asked to start it"),
                }
            }
        };
        let (from, start) = tokio::time::timeout(PATIENCE, relayed)
            .await
            .expect("its node is asked in time");
        skiff_end.abort();
        assert_eq!(from, "kestrel", "from the machine that asked");
        start
    }

    #[tokio::test]
    async fn a_carrys_mark_passes_only_over_a_link_proven_to_be_its_machine() {
        let proven = Proven {
            by: realm("skiff"),
            far: "kestrel".to_owned(),
        };
        assert!(
            a_carry_as_handed_on(proven).await.carries,
            "a proven end passes the mark on"
        );
        assert!(
            !a_carry_as_handed_on(realm("skiff")).await.carries,
            "one whose other end is unproven clears it"
        );
    }

    #[tokio::test]
    async fn a_machine_refused_for_its_version_is_kept_as_why_until_it_links() {
        let skiff = Memory::default();
        let (near, far) = tokio::io::duplex(1 << 16);
        let skiff_end = end(&skiff, "skiff", near);
        let (_far_reading, mut far_writing) = tokio::io::split(far);
        let newer = Hello {
            version: VERSION + 1,
            ..realm("kestrel").hello()
        };
        send(&mut far_writing, &Frame::Hello(newer))
            .await
            .expect("the far end says hello");
        let refused = skiff_end.await.expect("skiff's end runs to its end");
        let why = refused
            .expect_err("skiff refuses another version")
            .to_string();
        assert_eq!(
            skiff.link_refused("kestrel"),
            Some(why),
            "the store keeps why, as the link said it"
        );

        let kestrel = Memory::default();
        kestrel.start_here("walgit", json!({"session": "walgit"}), true);
        let _linked = linked(&skiff, &kestrel, 1 << 16);
        // Its sessions cross only once the link is admitted.
        until(&skiff, |store| !store.presences().is_empty()).await;
        assert_eq!(
            skiff.link_refused("kestrel"),
            None,
            "a link admitted ends the refusal"
        );
    }

    #[tokio::test]
    async fn a_record_the_store_refuses_is_skipped_and_the_link_carries_on() {
        let skiff = Memory::default();
        let (near, far) = tokio::io::duplex(1 << 16);
        let _skiff_end = end(&skiff, "skiff", near);
        let (_far_reading, mut far_writing) = tokio::io::split(far);

        let good = recent(0, "20KSI7", 0);
        let frames = [
            Frame::Hello(realm("kestrel").hello()),
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
                held: None,
                started_by: None,
            },
            Frame::Message {
                id: good.clone(),
                body: json!({"id": good, "to": ["@WCJQSG"]}),
            },
        ];
        says(&mut far_writing, &frames).await;
        until(&skiff, |store| store.message(&good).is_some()).await;
        assert_eq!(
            skiff.message_ids(0),
            [good],
            "only the good message was kept"
        );
        assert!(
            skiff.presences().is_empty(),
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
        let (here, there) = (places("skiff"), places("kestrel"));
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

        let (skiff, kestrel) = (Memory::default(), Memory::default());
        let (near, far) = tokio::io::duplex(1 << 16);
        let _near = sharing_end(&skiff, "skiff", near, Some(here));
        let _far = sharing_end(&kestrel, "kestrel", far, Some(there));

        until_file(&theirs, "# Shared\n").await;
        std::fs::write(&theirs, "# Changed there\n").expect("changed on the far end");
        until_file(&ours, "# Changed there\n").await;
    }

    #[test]
    fn what_was_sent_into_a_silent_link_is_within_the_next_links_reach() {
        let store = Memory::default();
        let minute = millis(KEEP_EVERY);
        let began = now() - 30 * minute;
        let hearing = Hearing {
            watched: true,
            heard: now() - 3 * minute,
            began: Some(began),
            caught_up: Some(began + minute),
        };
        store
            .mark_linked("kestrel", hearing.linked_until())
            .expect("a memory store marks");
        let reach = reach_start(&store, "kestrel");
        assert_eq!(
            reach,
            hearing.heard - minute,
            "a message sent after the other end went quiet, or the minute before, which may \
             not have crossed, is offered again, and no earlier"
        );

        let dropped_again = Hearing {
            heard: now(),
            began: Some(hearing.linked_until()),
            caught_up: Some(now()),
            ..hearing
        };
        assert_eq!(
            dropped_again.linked_until(),
            hearing.heard,
            "a link dropped before its catch-up stood a keep leaves the mark it began from"
        );
        let never_caught_up = Hearing {
            caught_up: None,
            ..dropped_again
        };
        assert_eq!(
            never_caught_up.linked_until(),
            hearing.heard,
            "and so does one that never caught up"
        );
        let first = Hearing {
            began: None,
            ..never_caught_up
        };
        assert_eq!(
            first.linked_until(),
            first.heard,
            "the first link two machines ever had reaches from when it was last heard"
        );
    }

    #[test]
    fn a_link_admitted_keeps_the_mark_its_reach_comes_from() {
        let store = Memory::default();
        admitted(&store, "kestrel").expect("a memory store admits");
        let first = store
            .linked_until("kestrel")
            .expect("a first link is marked");
        assert!(
            (0..millis(KEEP_EVERY)).contains(&(now() - first)),
            "a machine never linked is marked as of now"
        );
        let days_ago = now() - 2 * millis(REACH) / 7;
        store
            .mark_linked("kestrel", days_ago)
            .expect("a memory store marks");
        admitted(&store, "kestrel").expect("a memory store admits");
        assert_eq!(
            store.linked_until("kestrel"),
            Some(days_ago),
            "a link admitted leaves the mark it reaches from, so a node killed before its \
             first keep loses nothing"
        );
    }

    #[test]
    fn an_end_silent_three_minutes_is_gone_however_long_its_machine_slept() {
        let heard = 1_793_250_000_000;
        let minute = millis(KEEP_EVERY);
        assert!(
            !silent(heard, heard + 3 * minute - 1),
            "a minute or two late isn't gone"
        );
        assert!(
            silent(heard, heard + 3 * minute),
            "three minutes with nothing said is"
        );
        assert!(
            silent(heard, heard + 45 * minute),
            "and so is an end last heard before its machine slept"
        );
    }

    /// An end whose frames kept arriving while this end's work was held
    /// behind a long share isn't gone, though none of them was taken yet;
    /// one whose last frame arrived three minutes ago is.
    #[test]
    fn an_end_whose_frames_arrived_while_the_link_was_busy_is_not_gone() {
        let minute = millis(KEEP_EVERY);
        let mut hearing = Hearing::from_now(true, None);
        let taken = hearing.heard;
        hearing.arrived(taken + 3 * minute);
        assert!(
            !hearing.gone(taken + 4 * minute),
            "a frame arrived a minute ago"
        );
        hearing.arrived(taken);
        assert_eq!(
            hearing.heard,
            taken + 3 * minute,
            "an older arrival takes nothing back"
        );
        assert!(
            hearing.gone(taken + 6 * minute),
            "nothing has arrived for three minutes"
        );
    }

    /// The task reading frames keeps when each arrived, before the link's
    /// own work takes it.
    #[tokio::test]
    async fn the_reader_keeps_when_each_frame_arrived() {
        let (far, near) = tokio::io::duplex(1024);
        let (_, mut writing) = tokio::io::split(far);
        let arrived = Arc::new(AtomicI64::new(0));
        let (mut frames, _shared, _reader) =
            read_frames(Box::new(near), Arc::clone(&arrived), Arc::default());
        let before = now();
        writing
            .write_all(b"{\"Ended\":{\"session\":\"s1\"}}\n")
            .await
            .expect("written");
        let frame = frames.recv().await.expect("a frame");
        assert!(frame.is_ok(), "{frame:?}");
        assert!(
            arrived.load(Ordering::Relaxed) >= before,
            "its arrival is kept"
        );
    }

    /// After the first frame, shares and the word that the other machine
    /// has handed over come on a lane of their own, in the order they were
    /// said, so an ask behind a quest's files is taken without waiting on
    /// them; a share said first is read as any first frame is.
    #[tokio::test]
    async fn the_reader_reads_shares_apart_from_what_asks_and_answers() {
        let (far, near) = tokio::io::duplex(8192);
        let (_, mut writing) = tokio::io::split(far);
        let (mut frames, mut share_frames, _reader) =
            read_frames(Box::new(near), Arc::default(), Arc::default());
        let hello = Frame::Hello(realm("tern").hello());
        let lines = [
            want_readme("q0"),
            hello,
            want_readme("q1"),
            ended("s1"),
            want_readme("q2"),
            Frame::HandedOver,
            ended("s2"),
        ]
        .iter()
        .map(|frame| serde_json::to_string(frame).expect("a frame") + "\n")
        .collect::<String>();
        writing.write_all(lines.as_bytes()).await.expect("written");
        drop(writing);
        let mut heard = Vec::new();
        while let Some(frame) = frames.recv().await {
            heard.push(frame.expect("a frame"));
        }
        assert!(matches!(heard[0], Frame::Share(_)), "{heard:?}");
        assert!(matches!(heard[1], Frame::Hello(_)), "{heard:?}");
        assert_eq!(heard[2..], [ended("s1"), ended("s2")]);
        let mut shares = Vec::new();
        while let Some(frame) = share_frames.recv().await {
            shares.push(frame);
        }
        assert_eq!(
            shares,
            [want_readme("q1"), want_readme("q2"), Frame::HandedOver],
            "shares keep their order, and the handover comes after them"
        );
    }

    /// What `read_frames` gives for `lines`, each frame or the kind of error
    /// that ended the reading.
    async fn read_lines(lines: &[&str]) -> Vec<Result<Frame, io::ErrorKind>> {
        read_lines_skipping(lines).await.0
    }

    /// What `read_frames` gives for `lines`, as [`read_lines`], and whether
    /// it skipped a share.
    async fn read_lines_skipping(lines: &[&str]) -> (Vec<Result<Frame, io::ErrorKind>>, bool) {
        let (far, near) = tokio::io::duplex(8192);
        let (_, mut writing) = tokio::io::split(far);
        let skipped_shares = Arc::new(AtomicBool::new(false));
        let (mut frames, _shared, _reader) = read_frames(
            Box::new(near),
            Arc::new(AtomicI64::new(0)),
            Arc::clone(&skipped_shares),
        );
        writing
            .write_all(format!("{}\n", lines.join("\n")).as_bytes())
            .await
            .expect("written");
        drop(writing);
        let mut heard = Vec::new();
        while let Some(frame) = frames.recv().await {
            heard.push(frame.map_err(|error| error.kind()));
        }
        (heard, skipped_shares.load(Ordering::Relaxed))
    }

    fn ended(session: &str) -> Frame {
        Frame::Ended {
            session: session.to_owned(),
        }
    }

    /// A frame of a kind this end knows whose body it can't read ends the
    /// reading, as it did before kinds were skipped: it may be the answer an
    /// ask waits on with no time limit, or a piece whose window slot would
    /// never come back, and only the link's drop answers those. So does a
    /// second hello this end can't read, JSON of no frame's shape, and a
    /// kind said twice, which a map would fold into the last.
    #[tokio::test]
    async fn the_reader_ends_at_a_known_kind_it_cant_read() {
        for bad in [
            "{\"Ended\":{\"session\":7}}",
            "{\"Started\":{\"reply\":{\"Ok\":\"Zzz\"}}}",
            "{\"Hello\":{}}",
            "{\"Share\":{\"Holds\":{\"quest\":1}}}",
            "{\"Share\":7}",
            "\"Share\"",
            "[\"Ended\",\"s2\"]",
            "{\"Ended\":{\"session\":\"s2\"},\"Left\":null}",
            "{\"Share\":{\"Want\":\"bad\"},\"Share\":{\"Zzz\":1}}",
            "{\"Share\":{\"Want\":\"bad\",\"Zzz\":1}}",
            "{\"Zzz\":1,\"Zzz\":2}",
        ] {
            let ended_by = [
                "{\"Ended\":{\"session\":\"s1\"}}",
                bad,
                "{\"Ended\":{\"session\":\"s3\"}}",
            ];
            assert_eq!(
                read_lines(&ended_by).await,
                [Ok(ended("s1")), Err(io::ErrorKind::InvalidData)],
                "{bad} ends the reading, and nothing after it is read"
            );
        }
    }

    /// The kinds the reader checks a frame against are the enums' own, so
    /// a variant added later is known without a list to keep.
    #[test]
    fn the_kinds_known_are_the_enums_own() {
        let frames = kinds::<Frame>();
        for kind in ["Hello", "Share", "Ended", "Started", "HandedOver"] {
            assert!(frames.contains(&kind), "{kind} is a frame: {frames:?}");
        }
        assert!(kinds::<Say>().contains(&"Holds"), "Holds is a share");
        assert!(!frames.contains(&"Holds"), "and not a frame");
    }

    /// Each skipped kind is said once a link, and past [`SKIPS_SAID`] kinds
    /// only once more that there are others, so the log and the kinds kept
    /// stay bounded however many an end sends, an empty kind among them.
    #[test]
    fn the_kinds_skipped_are_said_a_bounded_number_of_times() {
        let mut skipped = Skipped::default();
        let mut said = Vec::new();
        for kind in std::iter::once(String::new()).chain((0..40).map(|kind| format!("G{kind}"))) {
            let unread = Unread::Frame(kind);
            said.extend(say_skipped("tern", &unread, &mut skipped));
            said.extend(say_skipped("tern", &unread, &mut skipped));
        }
        assert_eq!(
            said.len(),
            SKIPS_SAID + 1,
            "the first kinds and the word for more: {said:?}"
        );
        assert!(said[SKIPS_SAID].contains("more kinds"), "{said:?}");
        assert_eq!(
            skipped.kinds.len(),
            SKIPS_SAID,
            "only the kinds said are kept"
        );
        assert!(skipped.kinds.contains("") && !skipped.kinds.contains("G39"));
    }

    /// A skip says the other end's words cut short and quoted, so a long or
    /// marked kind or machine can't fill the log or write a line of its own,
    /// and keeps only what it said.
    #[test]
    fn a_skip_says_the_other_ends_words_cut_and_quoted() {
        let mut skipped = Skipped::default();
        let long = format!("{}\nforged line", "k".repeat(60));
        let said = say_skipped(&"m".repeat(60), &Unread::Share(long), &mut skipped)
            .expect("a new kind is said");
        assert!(
            said.starts_with(&format!("{:?} sent", "m".repeat(WORD_SHOWN))),
            "{said}"
        );
        assert!(!said.contains('\n') && !said.contains("forged"), "{said}");
        let kept = skipped.kinds.iter().next().expect("one kind kept");
        assert_eq!(kept.chars().count(), WORD_SHOWN, "{kept}");
    }

    /// After the hello, a frame of a kind this end doesn't know, or a share
    /// of a kind it doesn't, is skipped and the frames after it are read,
    /// as an end newer than this one may send them; a line that isn't JSON
    /// still ends the reading, as does any unread first line.
    #[tokio::test]
    async fn the_reader_skips_a_frame_it_cant_read_after_the_hello() {
        let hello = serde_json::to_string(&Frame::Hello(realm("tern").hello())).expect("a hello");
        let lines = [
            hello.as_str(),
            "{\"Gossip\":{\"about\":\"what is next\"}}",
            "\"Wave\"",
            "{\"\":1}",
            "{\"Ended\":{\"session\":\"s2\"}}",
            "not a frame",
            "{\"Ended\":{\"session\":\"s3\"}}",
        ];
        let (heard, skipped_shares) = read_lines_skipping(&lines).await;
        assert!(matches!(heard[0], Ok(Frame::Hello(_))), "{heard:?}");
        assert_eq!(
            heard[1..],
            [Ok(ended("s2")), Err(io::ErrorKind::InvalidData)],
            "the three it can't read are skipped, and reading ends at the line that isn't one"
        );
        assert!(!skipped_shares, "none of them was a share");
        let shares = [
            "{\"Ended\":{\"session\":\"s1\"}}",
            "{\"Share\":{\"Murmur\":{\"quest\":\"q\"}}}",
            "{\"Share\":\"Hum\"}",
            "{\"Ended\":{\"session\":\"s2\"}}",
        ];
        assert_eq!(
            read_lines_skipping(&shares).await,
            (vec![Ok(ended("s1")), Ok(ended("s2"))], true),
            "shares of kinds it doesn't know are skipped, and kept as skipped"
        );
        assert_eq!(
            read_lines(&["{\"Gossip\":{}}", "{\"Ended\":{\"session\":\"s1\"}}"]).await,
            [Err(io::ErrorKind::InvalidData)],
            "a first line it can't read ends it, and nothing after it is read"
        );
    }

    #[tokio::test]
    async fn a_link_that_can_release_ends_once_the_other_machine_hands_over() {
        let (ended, skiff) = handed_over_after(&[]).await;
        assert_eq!(
            ended.expect("the link doesn't fail"),
            Ending::HandedOver,
            "holding all the other machine said, it may be released"
        );
        assert_eq!(skiff.endings("lent", 0), 0, "a handover is no drop");
    }

    /// A share this end skipped may have named a file it then never heard
    /// of, so a handover after one isn't taken as holding all the other
    /// machine said.
    #[tokio::test]
    async fn a_handover_after_a_skipped_share_doesnt_release() {
        let (ended, _) = handed_over_after(&["{\"Share\":{\"Murmur\":{\"quest\":\"q\"}}}"]).await;
        assert!(
            !matches!(ended, Ok(Ending::HandedOver)),
            "the link ends as the other end leaves, not as handed over: {ended:?}"
        );
    }

    /// How a link that can release ends when the other machine says its
    /// hello and what it has, then `before`, then that it has handed over,
    /// and leaves; and the store at this end.
    async fn handed_over_after(before: &[&str]) -> (io::Result<Ending>, Memory) {
        let skiff = Memory::default();
        let store = skiff.clone();
        let (skiff_end, far_reading, mut far_writing) = skiff_over(
            store,
            |connection| Connection {
                releases: true,
                ..connection
            },
            None,
        );
        let mut lines = BufReader::new(far_reading).lines();
        let first = lines
            .next_line()
            .await
            .expect("the pipe reads")
            .expect("skiff says hello");
        let Ok(Frame::Hello(hello)) = serde_json::from_str::<Frame>(&first) else {
            panic!("skiff's first line is its hello: {first}");
        };
        assert!(hello.releases, "an end that can release says so");

        says(
            &mut far_writing,
            &[
                Frame::Hello(realm("lent").hello()),
                Frame::Have {
                    messages: Vec::new(),
                    living: Some(Vec::new()),
                },
            ],
        )
        .await;
        for line in before {
            far_writing
                .write_all(format!("{line}\n").as_bytes())
                .await
                .expect("written");
        }
        says(&mut far_writing, &[Frame::HandedOver]).await;
        // Its reading half still holds the pipe open, so only a shutdown
        // says it has left.
        far_writing.shutdown().await.expect("shut");
        drop(far_writing);
        let ended = tokio::time::timeout(PATIENCE, skiff_end)
            .await
            .expect("skiff's end ends")
            .expect("skiff's end runs");
        (ended, skiff)
    }

    /// What a worker, tern, says to the machine that dialled it: its hello,
    /// an ask for a file of `quest`, a presence of its own and one naming
    /// the dialler's, the message `message`, then a start, a steer, an
    /// update, a run, a carry and an end the person asks, as 1 to 6.
    fn a_workers_frames(quest: &str, message: &str) -> Vec<Frame> {
        let start: Start =
            serde_json::from_value(json!({"work": {}, "command": ["pi"]})).expect("a start");
        let presence = |session: &str, body: Value| Frame::Presence {
            session: session.to_owned(),
            body,
            leases: None,
            held: None,
            started_by: None,
        };
        vec![
            Frame::Hello(realm("tern").hello()),
            Frame::Have {
                messages: Vec::new(),
                living: Some(Vec::new()),
            },
            Frame::Share(Say::Want {
                quest: quest.to_owned(),
                file: "README.md".to_owned(),
                at: None,
            }),
            presence("sak9fz", json!({"session": "sak9fz"})),
            presence("walgit", json!({"session": "walgit", "machine": "skiff"})),
            Frame::Message {
                id: message.to_owned(),
                body: json!({"id": message, "from": "SAK9FZ", "to": ["@WCJQSG"],
                             "text": "done"}),
            },
            Frame::Start { ask: 1, start },
            Frame::Steer {
                ask: 2,
                address: "wcjqsg".to_owned(),
                steer: json!({"text": "stop", "from": "joel", "id": "t1"}),
                checked: true,
            },
            Frame::Update { ask: 3 },
            Frame::Run {
                ask: 4,
                run: serde_json::from_value(json!({"work": {}, "command": ["true"]}))
                    .expect("a run"),
            },
            Frame::Carry {
                ask: 5,
                quest: quest.to_owned(),
                to: "tern".to_owned(),
                checked: true,
            },
            Frame::End {
                ask: 6,
                held: "01J".to_owned(),
                by: hold::end::Asker::Person,
            },
        ]
    }

    #[tokio::test]
    async fn a_worker_is_asked_and_asks_nothing_back() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let here = Places {
            quests: dir.path().join("quests"),
            scratch: dir.path().join("scratch"),
            home: dir.path().join("home"),
            state: dir.path().join("state"),
        };
        let quest = "QEST-20261013-4UHC5Q";
        std::fs::create_dir_all(here.quests.join(quest)).expect("the quest made");
        std::fs::write(here.quests.join(quest).join("README.md"), "# Ours\n")
            .expect("its README written");
        here.ledgers()
            .hold(quest)
            .expect("its ledger held")
            .keep()
            .expect("the quest shared");

        let skiff = Memory::default();
        let store = skiff.clone();
        let (_skiff_end, far_reading, mut far_writing) = skiff_over(
            store,
            |connection| Connection {
                worker: true,
                ..connection
            },
            Some(here),
        );
        let mut lines = BufReader::new(far_reading).lines();

        let message = recent(0, "SAK9FZ", 0);
        says(&mut far_writing, &a_workers_frames(quest, &message)).await;

        let mut answers = BTreeMap::new();
        let mut ran = None;
        let mut said = Vec::new();
        tokio::time::timeout(PATIENCE, async {
            while answers.len() < 5 || ran.is_none() {
                let line = lines
                    .next_line()
                    .await
                    .expect("the pipe reads")
                    .expect("skiff answers");
                match serde_json::from_str::<Frame>(&line).expect("a frame") {
                    Frame::Started { ask, reply } => {
                        answers.insert(ask, reply);
                    }
                    Frame::Ran { ask, ran: said } => ran = Some((ask, said)),
                    other => said.push(other),
                }
            }
        })
        .await
        .expect("skiff answers each ask");

        assert_eq!(
            answers,
            BTreeMap::from([
                (1, Err("skiff starts nothing for a worker".to_owned())),
                (2, Err("skiff takes no steers from a worker".to_owned())),
                (3, Err("skiff updates nothing for a worker".to_owned())),
                (5, Err("skiff carries nothing for a worker".to_owned())),
                (6, Err("skiff ends nothing for a worker".to_owned())),
            ]),
            "a worker can't start, steer, update, carry or end anything on the machine that \
             dialled it, even said as the person"
        );
        assert_eq!(
            ran,
            Some((4, Ran::Failed("skiff runs nothing for a worker".to_owned()))),
            "nor run anything there"
        );
        assert!(
            !said.iter().any(|frame| matches!(frame, Frame::Share(_))
                && serde_json::to_string(frame).is_ok_and(|json| json.contains(quest))),
            "nor is a quest skiff shares and didn't give it said or sent to it: {said:?}"
        );
        until(&skiff, |store| {
            store.message(&message).is_some()
                && store.presences().iter().any(|p| p.session == "sak9fz")
        })
        .await;
        assert!(
            !skiff.presences().iter().any(|p| p.session == "walgit"),
            "its messages and its own sessions cross, but not a presence naming another machine"
        );
    }

    #[tokio::test]
    async fn a_peer_link_cant_say_a_message_as_a_receptor() {
        let skiff = Memory::default();
        let store = skiff.clone();
        let (near, far) = tokio::io::duplex(1 << 16);
        let (reading, writing) = tokio::io::split(near);
        let _skiff_end = tokio::spawn(async move {
            run(
                store,
                &realm("skiff"),
                connection(reading, writing),
                None,
                None,
            )
            .await
        });
        let (_far_reading, mut far_writing) = tokio::io::split(far);

        let (forged, theirs, ours) = (
            recent(0, "github@tern", 0),
            recent(0, "github@skiff", 1),
            recent(0, "OSPR01", 2),
        );
        let message = |id: &str, from: &str| Frame::Message {
            id: id.to_owned(),
            body: json!({"id": id, "from": from, "to": ["#outside/github"],
                         "intent": "fyi", "text": "github: 9 asked of you, up from 1; 9 today"}),
        };
        says(
            &mut far_writing,
            &[
                Frame::Hello(realm("tern").hello()),
                Frame::Have {
                    messages: Vec::new(),
                    living: Some(Vec::new()),
                },
                message(&forged, "github@tern"),
                message(&theirs, "github@skiff"),
                message(&ours, "OSPR01"),
            ],
        )
        .await;

        until(&skiff, |store| store.message(&ours).is_some()).await;
        assert!(
            skiff.message(&forged).is_none() && skiff.message(&theirs).is_none(),
            "a link said a message as a receptor, its own machine's or this one's"
        );
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "each of a worker's messages is refused or carried for its own reason"
    )]
    async fn a_worker_puts_no_words_in_another_machines_session_nor_rewrites_a_message() {
        let skiff = Memory::default();
        skiff
            .put_presence("ospr01", "osprey", &json!({"session": "ospr01"}))
            .expect("osprey's session kept");
        let held = recent(0, "WCJQSG", 0);
        skiff.send_here(
            &held,
            json!({"id": held, "to": ["@SAK9FZ"], "text": "ours"}),
        );
        let store = skiff.clone();
        let (_skiff_end, _far_reading, mut far_writing) = skiff_over(
            store,
            |connection| Connection {
                worker: true,
                ..connection
            },
            None,
        );

        let (posing, own) = (recent(0, "OSPR01", 1), recent(0, "SAK9FZ", 2));
        let (between, nobodys) = (recent(0, "WCJQSG", 3), recent(0, "SAK9FZ", 4));
        let message = |id: &str, from: &str, text: &str| Frame::Message {
            id: id.to_owned(),
            body: json!({"id": id, "from": from, "to": ["@WCJQSG"], "text": text}),
        };
        // What its own session says to steer skiff's sessions, each with
        // the fields of `said`.
        let machines = recent(0, "WCJQSG", 5);
        skiff.send_here(
            &machines,
            json!({"id": machines, "to": ["#machine"], "intent": "hold", "text": "a reboot"}),
        );
        let steering = |id: &str, said: Value| {
            let mut body = json!({"id": id, "from": "SAK9FZ", "to": ["@WCJQSG"], "text": "go"});
            if let (Some(body), Value::Object(said)) = (body.as_object_mut(), said) {
                body.extend(said);
            }
            Frame::Message {
                id: id.to_owned(),
                body,
            }
        };
        let (holding, pivoting) = (recent(0, "SAK9FZ", 6), recent(0, "SAK9FZ", 7));
        let (ending, to_all) = (recent(0, "SAK9FZ", 8), recent(0, "SAK9FZ", 9));
        let answering = recent(0, "SAK9FZ", 10);
        says(
            &mut far_writing,
            &[
                Frame::Hello(realm("tern").hello()),
                Frame::Have {
                    messages: Vec::new(),
                    living: Some(Vec::new()),
                },
                present("sak9fz"),
                message(&posing, "OSPR01", "osprey says so"),
                message(&between, "WCJQSG", "skiff's own, between presences"),
                Frame::Message {
                    id: nobodys.clone(),
                    body: json!({"id": nobodys, "to": ["@WCJQSG"], "text": "nobody"}),
                },
                message(&held, "SAK9FZ", "theirs"),
                steering(&holding, json!({"intent": "hold"})),
                steering(&pivoting, json!({"intent": "pivot"})),
                steering(
                    &ending,
                    json!({"intent": "fyi", "re": held, "endsHold": true}),
                ),
                steering(&to_all, json!({"intent": "fyi", "to": ["#machine"]})),
                steering(&answering, json!({"intent": "fyi", "re": machines})),
                message(&own, "SAK9FZ", "done"),
            ],
        )
        .await;

        until(&skiff, |store| store.message(&own).is_some()).await;
        assert!(
            skiff.message(&posing).is_none(),
            "a message from a session osprey holds isn't the worker's to say"
        );
        assert!(
            skiff.message(&between).is_none() && skiff.message(&nobodys).is_none(),
            "nor one from a session it holds no presence for, nor one from no session"
        );
        for (id, what) in [
            (&holding, "a hold"),
            (&pivoting, "a pivot"),
            (&ending, "the end of a hold"),
            (&to_all, "anything to the whole machine"),
            (&answering, "nor an answer to what was"),
        ] {
            assert!(
                skiff.message(id).is_none(),
                "its own session can't steer skiff's: {what}"
            );
        }
        let kept = skiff.message(&own).expect("its own session's message kept");
        assert_eq!(
            (kept["text"].as_str(), kept["worker"].as_str()),
            (Some("[said on tern, a worker machine] done"), Some("tern")),
            "a worker's own message is kept labelled as the worker's word, by the link's name"
        );
        assert_eq!(
            skiff
                .message(&held)
                .and_then(|body| body["text"].as_str().map(str::to_owned)),
            Some("ours".to_owned()),
            "nor is a message skiff holds written over"
        );
    }

    #[tokio::test]
    async fn a_worker_takes_no_name_a_session_living_elsewhere_answers_to() {
        let skiff = Memory::default();
        skiff.start_here(
            "wcjq01",
            json!({"session": "wcjq01", "addresses": ["WCJQSG"]}),
            true,
        );
        skiff
            .put_presence(
                "ospr01",
                "osprey",
                &json!({"session": "ospr01", "addresses": ["OSPREY"]}),
            )
            .expect("osprey's session kept");
        let store = skiff.clone();
        let (_skiff_end, _far_reading, mut far_writing) = skiff_over(
            store,
            |connection| Connection {
                worker: true,
                ..connection
            },
            None,
        );
        let planted = |session: &str, address: &str| Frame::Presence {
            session: session.to_owned(),
            body: json!({"session": session, "addresses": [address]}),
            leases: None,
            held: None,
            started_by: None,
        };
        let message = |id: &str, from: &str| Frame::Message {
            id: id.to_owned(),
            body: json!({"id": id, "from": from, "to": ["@SAK9FZ"], "text": "go"}),
        };
        let (as_here, as_osprey) = (recent(0, "WCJQSG", 1), recent(0, "OSPREY", 2));
        let own = recent(0, "SAK9FZ", 3);
        says(
            &mut far_writing,
            &[
                Frame::Hello(realm("tern").hello()),
                Frame::Have {
                    messages: Vec::new(),
                    living: Some(Vec::new()),
                },
                planted("posing", "WCJQSG"),
                planted("posed", "osprey"),
                planted("early", "HARBR1"),
                planted("sak9fz", "SAK9FZ"),
                message(&as_here, "WCJQSG"),
                message(&as_osprey, "OSPREY"),
                message(&own, "SAK9FZ"),
            ],
        )
        .await;
        until(&skiff, |store| store.message(&own).is_some()).await;

        let tern_holds = |store: &Memory| {
            let mut held: Vec<String> = store
                .presences()
                .into_iter()
                .filter(|presence| presence.elsewhere.as_deref() == Some("tern"))
                .map(|presence| presence.session)
                .collect();
            held.sort();
            held
        };
        assert_eq!(
            tern_holds(&skiff),
            ["early", "sak9fz"],
            "a presence answering to a session living here or on osprey isn't kept"
        );
        assert!(
            skiff.message(&as_here).is_none() && skiff.message(&as_osprey).is_none(),
            "so nothing is said as either"
        );

        // A session that comes to live here after the worker took its name
        // first is still the only one heard as it.
        skiff.start_here(
            "harb01",
            json!({"session": "harb01", "addresses": ["HARBR1"]}),
            true,
        );
        let (as_harbour, after) = (recent(0, "HARBR1", 4), recent(0, "SAK9FZ", 5));
        says(
            &mut far_writing,
            &[message(&as_harbour, "HARBR1"), message(&after, "SAK9FZ")],
        )
        .await;
        until(&skiff, |store| store.message(&after).is_some()).await;
        assert!(
            skiff.message(&as_harbour).is_none(),
            "a name taken before its session lived here speaks for the worker no longer"
        );
    }

    #[tokio::test]
    async fn whether_a_machine_is_a_worker_is_the_links_word_never_the_machines() {
        for (worker, says_it_is) in [(true, false), (false, true)] {
            let skiff = Memory::default();
            let (_skiff_end, _far_reading, mut far_writing) = skiff_over(
                skiff.clone(),
                move |connection| Connection {
                    worker,
                    ..connection
                },
                None,
            );
            let mut tern = tern_undeclared();
            tern.worker = says_it_is;
            says(
                &mut far_writing,
                &[
                    Frame::Hello(realm("tern").hello()),
                    Frame::Have {
                        messages: Vec::new(),
                        living: Some(Vec::new()),
                    },
                    Frame::Machine(Box::new(tern)),
                ],
            )
            .await;
            until(&skiff, |store| {
                store
                    .machines()
                    .iter()
                    .any(|machine| machine.name == "tern")
            })
            .await;
            let kept = skiff
                .machines()
                .into_iter()
                .find(|machine| machine.name == "tern");
            assert_eq!(
                kept.map(|machine| machine.worker),
                Some(worker),
                "tern said it was a worker: {says_it_is}, but the link's word is kept"
            );
        }
    }

    /// Starts as skiff's are carried, with its sessions' inboxes in
    /// `dir`, and no node to start them.
    fn steering(dir: &std::path::Path) -> Starting {
        Starting {
            node: dir.join("node.sock"),
            held: dir.join("held"),
            asking: None,
            steers: dir.join("inboxes"),
            parked: dir.join("parked"),
            health: None,
            declared: Some(dir.join("declared.json")),
            kit: None,
            saturation: None,
            lender: None,
            gates: None,
        }
    }

    // On 2026-11-23 a question carried in from another machine sat ten
    // minutes in a parked peer's folder, since only a sender wakes a
    // parked peer, through its own machine's markers.
    #[tokio::test]
    async fn a_message_carried_in_wakes_the_parked_session_it_gives_a_turn() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let starting = steering(dir.path());
        let parked = starting.parked.clone();
        let wakes = dir.path().join("wakes");
        std::fs::create_dir_all(&parked).expect("the parked folder");
        std::fs::create_dir_all(&wakes).expect("the wakes' folder");
        for (session, address) in [("s1", "SAK9FZ"), ("s2", "OSPREY")] {
            std::fs::write(
                parked.join(format!("{session}.json")),
                json!({"session": session, "addresses": [address],
                       "wake": wakes.join(format!("{session}.wake"))})
                .to_string(),
            )
            .expect("a marker");
        }
        let skiff = Memory::default();
        let store = skiff.clone();
        let (near, far) = tokio::io::duplex(1 << 16);
        let (reading, writing) = tokio::io::split(near);
        tokio::spawn(async move {
            run(
                store,
                &realm("skiff"),
                connection(reading, writing),
                None,
                Some(starting),
            )
            .await
        });
        let (_far_reading, mut far_writing) = tokio::io::split(far);
        let (asked, news) = (recent(0, "WCJQSG", 1), recent(0, "WCJQSG", 2));
        let message = |id: &str, intent: &str| Frame::Message {
            id: id.to_owned(),
            body: json!({"id": id, "from": "WCJQSG", "to": ["@SAK9FZ", "@OSPREY"],
                         "intent": intent, "text": "go"}),
        };
        says(
            &mut far_writing,
            &[
                Frame::Hello(realm("tern").hello()),
                Frame::Have {
                    messages: Vec::new(),
                    living: Some(Vec::new()),
                },
                message(&asked, "question"),
                message(&news, "fyi"),
            ],
        )
        .await;
        until(&skiff, |store| store.message(&news).is_some()).await;

        let said: Value = serde_json::from_slice(
            &std::fs::read(wakes.join("s1.wake")).expect("the question woke the session asked"),
        )
        .expect("json");
        assert_eq!(said["messageId"].as_str(), Some(asked.as_str()));
        let said: Value =
            serde_json::from_slice(&std::fs::read(wakes.join("s2.wake")).expect("and the other"))
                .expect("json");
        assert_eq!(
            said["messageId"].as_str(),
            Some(asked.as_str()),
            "the news after it woke neither again"
        );

        std::fs::remove_file(wakes.join("s1.wake")).expect("its wake taken, as its holder does");
        let later = recent(0, "WCJQSG", 3);
        says(
            &mut far_writing,
            &[message(&asked, "question"), message(&later, "fyi")],
        )
        .await;
        until(&skiff, |store| store.message(&later).is_some()).await;
        assert!(
            !wakes.join("s1.wake").exists(),
            "the question carried again wakes nobody twice"
        );
    }

    /// Skiff's end of a link a worker dialled, carrying steers into the
    /// inboxes in `dir`, and the worker's end of it.
    fn a_worker_steering(
        skiff: &Memory,
        dir: &std::path::Path,
    ) -> (ReadHalf<DuplexStream>, WriteHalf<DuplexStream>) {
        let store = skiff.clone();
        let starting = steering(dir);
        let (near, far) = tokio::io::duplex(1 << 16);
        let (reading, writing) = tokio::io::split(near);
        tokio::spawn(async move {
            run(
                store,
                &realm("skiff"),
                Connection {
                    worker: true,
                    ..connection(reading, writing)
                },
                None,
                Some(starting),
            )
            .await
        });
        tokio::io::split(far)
    }

    /// What skiff answers a lend tern asks of it over a link, tern dialled
    /// as its worker or not, skiff lending through `lender` or not.
    async fn lent_to_tern(worker: bool, lender: Option<Lender>) -> Lent {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let starting = Starting {
            lender: lender.map(Arc::new),
            ..steering(dir.path())
        };
        let (near, far) = tokio::io::duplex(1 << 16);
        let (reading, writing) = tokio::io::split(near);
        tokio::spawn(async move {
            run(
                Memory::default(),
                &realm("skiff"),
                Connection {
                    worker,
                    ..connection(reading, writing)
                },
                None,
                Some(starting),
            )
            .await
        });
        let (far_reading, mut far_writing) = tokio::io::split(far);
        let lend = Lend {
            budget: None,
            method: "POST".to_owned(),
            path: "/apis/anthropic/v1/messages".to_owned(),
            headers: Vec::new(),
            body: String::new(),
        };
        says(
            &mut far_writing,
            &[
                Frame::Hello(realm("tern").hello()),
                Frame::Have {
                    messages: Vec::new(),
                    living: Some(Vec::new()),
                },
                Frame::Lend { ask: 7, lend },
            ],
        )
        .await;
        let mut lines = BufReader::new(far_reading).lines();
        tokio::time::timeout(PATIENCE, async {
            loop {
                let line = lines
                    .next_line()
                    .await
                    .expect("the pipe reads")
                    .expect("skiff answers");
                if let Frame::Lent { ask: 7, lent } =
                    serde_json::from_str::<Frame>(&line).expect("a frame")
                {
                    return lent;
                }
            }
        })
        .await
        .expect("skiff answers the lend")
    }

    /// What skiff answers each start tern, its worker, asks of it over a
    /// link, skiff answering the gates of the workers `listed` names, by
    /// a program that runs and fails at once, and having given tern the
    /// quest `QEST-20261124-SKIFF1`.
    async fn starts_of_tern(listed: Option<&str>, starts: Vec<Start>) -> Vec<Reply> {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let here = Places {
            quests: dir.path().join("quests"),
            scratch: dir.path().join("scratch"),
            home: dir.path().join("home"),
            state: dir.path().join("state"),
        };
        let quest = "QEST-20261124-SKIFF1";
        std::fs::create_dir_all(here.quests.join(quest)).expect("the quest made");
        std::fs::write(here.quests.join(quest).join("README.md"), "# Given\n")
            .expect("its README written");
        here.ledgers()
            .hold(quest)
            .expect("its ledger held")
            .keep()
            .expect("the quest shared");
        here.give("tern", quest).expect("the quest given");
        let gates = listed.map(|listed| {
            let file = dir.path().join("answers-for");
            std::fs::write(&file, listed).expect("the list written");
            // The shell, asked to run a script named `answer` that isn't
            // there, starts and ends at once.
            Arc::new(Gates::new(PathBuf::from("/bin/sh"), file).expect("the gates"))
        });
        let starting = Starting {
            gates,
            ..steering(dir.path())
        };
        let (near, far) = tokio::io::duplex(1 << 16);
        let (reading, writing) = tokio::io::split(near);
        tokio::spawn(async move {
            run(
                Memory::default(),
                &realm("skiff"),
                Connection {
                    worker: true,
                    ..connection(reading, writing)
                },
                Some(here),
                Some(starting),
            )
            .await
        });
        let (far_reading, mut far_writing) = tokio::io::split(far);
        let asked = starts.len();
        let mut frames = vec![
            Frame::Hello(realm("tern").hello()),
            Frame::Have {
                messages: Vec::new(),
                living: Some(Vec::new()),
            },
        ];
        frames.extend(
            (1..)
                .zip(starts)
                .map(|(ask, start)| Frame::Start { ask, start }),
        );
        says(&mut far_writing, &frames).await;
        let mut lines = BufReader::new(far_reading).lines();
        let mut answers = BTreeMap::new();
        tokio::time::timeout(PATIENCE, async {
            while answers.len() < asked {
                let line = lines
                    .next_line()
                    .await
                    .expect("the pipe reads")
                    .expect("skiff answers");
                if let Frame::Started { ask, reply } =
                    serde_json::from_str::<Frame>(&line).expect("a frame")
                {
                    answers.insert(ask, reply);
                }
            }
        })
        .await
        .expect("skiff answers each start");
        answers.into_values().collect()
    }

    fn gate_start(quest: &str, commit: &str) -> Start {
        serde_json::from_value(json!({
            "work": {},
            "command": ["nix-offload", "answer", quest, commit],
        }))
        .expect("a start")
    }

    #[tokio::test]
    async fn a_worker_s_gate_is_started_only_where_this_machine_answers_it() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let given = || gate_start("QEST-20261124-SKIFF1", commit);
        let answers = starts_of_tern(Some("tern\n"), vec![given()]).await;
        let [Ok(Response::Started(started))] = answers.as_slice() else {
            panic!("a listed worker's gate of a quest given it is started: {answers:?}");
        };
        assert_eq!(
            (started.machine.as_str(), started.held.as_str()),
            ("skiff", "gate-0123456789ab"),
            "named for its commit, on skiff"
        );

        assert_eq!(
            starts_of_tern(Some("wren\n"), vec![given()]).await,
            vec![Err(
                "skiff starts nothing for a worker but a gate it answers: this machine answers \
                 no gate for it (unlisted)"
                    .to_owned()
            )],
            "a worker the list doesn't name is refused"
        );
        assert_eq!(
            starts_of_tern(None, vec![given()]).await,
            vec![Err("skiff starts nothing for a worker".to_owned())],
            "as is every worker where skiff answers none"
        );
        let pi: Start =
            serde_json::from_value(json!({"work": {}, "command": ["pi"]})).expect("a start");
        let answers = starts_of_tern(
            Some("tern\n"),
            vec![pi, gate_start("QEST-20261124-TERN22", commit)],
        )
        .await;
        assert!(
            answers.iter().all(Result::is_err),
            "anything but a gate, or a gate of a quest never given, is refused: {answers:?}"
        );
    }

    #[tokio::test]
    async fn a_lend_is_answered_only_for_a_worker_where_this_machine_lends() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let unread = || {
            Lender::new(
                dir.path().join("lends.json"),
                Vec::new(),
                dir.path().join("lend"),
                None,
            )
        };
        assert_eq!(
            lent_to_tern(false, Some(unread())).await,
            Lent::Refused {
                status: 403,
                why: "skiff lends only to its workers, and tern isn't one".to_owned(),
            },
            "a machine that isn't skiff's worker borrows nothing"
        );
        assert_eq!(
            lent_to_tern(true, None).await,
            Lent::Refused {
                status: 403,
                why: "skiff lends nothing".to_owned(),
            },
            "nor does a worker where skiff lends nothing"
        );
        let lent = lent_to_tern(true, Some(unread())).await;
        assert!(
            matches!(lent, Lent::Refused { status: 503, .. }),
            "a worker's lend reaches skiff's lender, which can't read what it may lend: {lent:?}"
        );
    }

    #[tokio::test]
    async fn a_workers_replies_reach_only_its_own_sessions_talks() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let inboxes = dir.path().join("inboxes");
        for address in ["SAK9FZ", "OSPR01"] {
            steer::open_talk(&inboxes, address, "t1").expect("its talk opened");
        }
        let skiff = Memory::default();
        skiff
            .put_presence("ospr01", "osprey", &json!({"session": "ospr01"}))
            .expect("osprey's session kept");
        let (_far_reading, mut far_writing) = a_worker_steering(&skiff, dir.path());

        let replies = |address: &str, lines: &str| {
            Frame::Replies(Replies {
                address: address.to_owned(),
                id: "t1".to_owned(),
                from: 0,
                lines: lines.to_owned(),
            })
        };
        says(
            &mut far_writing,
            &[
                Frame::Hello(realm("tern").hello()),
                Frame::Have {
                    messages: Vec::new(),
                    living: Some(Vec::new()),
                },
                present("sak9fz"),
                replies("OSPR01", "{\"posing\":true}\n"),
                replies("SAK9FZ", "{\"done\":true}\n"),
            ],
        )
        .await;

        let talk = |address: &str| steer::replies(&inboxes, address, "t1").expect("a talk");
        until_file(&talk("SAK9FZ"), "{\"done\":true}\n").await;
        assert_eq!(
            std::fs::read_to_string(talk("OSPR01")).expect("osprey's talk reads"),
            "",
            "a worker's replies in a talk of a session osprey holds aren't kept"
        );
    }

    #[tokio::test]
    async fn a_worker_following_this_machines_talks_hears_nothing_of_them() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let inboxes = dir.path().join("inboxes");
        steer::open_talk(&inboxes, "WCJQSG", "t1").expect("skiff's talk opened");
        let talk = steer::replies(&inboxes, "WCJQSG", "t1").expect("a talk");
        std::fs::write(&talk, "{\"said\":\"ours\"}\n").expect("skiff's session says something");
        let skiff = Memory::default();
        let (far_reading, mut far_writing) = a_worker_steering(&skiff, dir.path());

        says(
            &mut far_writing,
            &[
                Frame::Hello(realm("tern").hello()),
                Frame::Follow {
                    talks: vec![steer::Talk {
                        address: "WCJQSG".to_owned(),
                        id: "t1".to_owned(),
                        has: 0,
                    }],
                },
            ],
        )
        .await;

        // Skiff looks at what it follows every second, so three looks pass.
        let mut heard = Vec::new();
        let mut lines = BufReader::new(far_reading).lines();
        let _ = tokio::time::timeout(3 * LOOK_EVERY, async {
            while let Ok(Some(line)) = lines.next_line().await {
                heard.push(serde_json::from_str::<Frame>(&line).expect("a frame"));
            }
        })
        .await;
        assert!(
            !heard.iter().any(|frame| matches!(frame, Frame::Replies(_))),
            "a worker hears nothing of a talk of skiff's own: {heard:?}"
        );
    }

    #[test]
    fn a_machine_dialled_as_one_is_refused_saying_hello_as_another() {
        let store = Memory::default();
        let named = OnceLock::from("kestrel".to_owned());
        let refused =
            pinned(&store, Some(&named), "osprey").expect_err("osprey isn't who was dialled");
        assert_eq!(
            refused.kind(),
            io::ErrorKind::PermissionDenied,
            "it is refused: {refused}"
        );
        assert!(
            store
                .link_refused("kestrel")
                .is_some_and(|why| why.contains("osprey")),
            "and why is kept under the machine dialled"
        );
        pinned(&store, Some(&named), "kestrel").expect("kestrel may say its own name");
    }

    #[test]
    fn a_redialled_machine_keeps_the_name_it_first_said() {
        let store = Memory::default();
        let named = OnceLock::new();
        pinned(&store, Some(&named), "tern").expect("the first hello is pinned");
        assert!(
            pinned(&store, Some(&named), "skiff").is_err(),
            "a redial saying another name is refused"
        );
        assert_eq!(
            named.get().map(String::as_str),
            Some("tern"),
            "tern stays pinned"
        );
    }

    #[tokio::test]
    async fn a_machine_handing_over_with_a_file_on_its_way_is_kept_until_it_comes() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let places = |machine: &str| Places {
            quests: dir.path().join(machine).join("quests"),
            scratch: dir.path().join(machine).join("scratch"),
            home: dir.path().join(machine).join("home"),
            state: dir.path().join(machine).join("state"),
        };
        let (home, lent) = (places("skiff"), places("lent"));
        let quest = "QEST-20261013-4UHC5Q";
        std::fs::create_dir_all(lent.quests.join(quest)).expect("the quest made");
        std::fs::write(lent.quests.join(quest).join("README.md"), "# Shared\n")
            .expect("its README written");
        lent.ledgers()
            .hold(quest)
            .expect("its ledger held")
            .keep()
            .expect("the quest shared");
        let mut exchange = Exchange::new(lent, "lent".to_owned(), "skiff".to_owned());

        let skiff = Memory::default();
        let store = skiff.clone();
        let (skiff_end, far_reading, mut far_writing) = skiff_over(
            store,
            |connection| Connection {
                releases: true,
                ..connection
            },
            Some(home),
        );
        let mut lines = BufReader::new(far_reading).lines();

        // The lent machine offers its README and hands over at once, then
        // says a message, which skiff keeps only once it has heard all
        // before it.
        let said = recent(0, "20KSI7", 0);
        let mut frames = vec![
            Frame::Hello(realm("lent").hello()),
            Frame::Have {
                messages: Vec::new(),
                living: Some(Vec::new()),
            },
        ];
        frames.extend(
            exchange
                .look()
                .expect("the lent machine looks")
                .into_iter()
                .map(Frame::Share),
        );
        frames.push(Frame::HandedOver);
        frames.push(Frame::Message {
            id: said.clone(),
            body: json!({"id": said, "to": ["@WCJQSG"]}),
        });
        says(&mut far_writing, &frames).await;
        until(&skiff, |store| store.message(&said).is_some()).await;
        assert!(
            !skiff_end.is_finished(),
            "skiff keeps the link while the README it asked for is on its way"
        );

        says(
            &mut far_writing,
            &files_asked_for(&mut lines, &mut exchange)
                .await
                .into_iter()
                .map(Frame::Share)
                .chain([Frame::HandedOver])
                .collect::<Vec<_>>(),
        )
        .await;
        let ended = tokio::time::timeout(PATIENCE, skiff_end)
            .await
            .expect("skiff's end ends")
            .expect("skiff's end runs");
        assert_eq!(
            ended.expect("the link doesn't fail"),
            Ending::HandedOver,
            "once the README came, the lent machine may be released"
        );
    }

    /// The presence of `session`, saying nothing but its name.
    fn present(session: &str) -> Frame {
        Frame::Presence {
            session: session.to_owned(),
            body: json!({"session": session}),
            leases: None,
            held: None,
            started_by: None,
        }
    }

    /// Says each of `frames` over `writing`, as the far end of a link.
    async fn says(writing: &mut WriteHalf<DuplexStream>, frames: &[Frame]) {
        for frame in frames {
            send(writing, frame).await.expect("the far end speaks");
        }
    }

    /// The frames the other end says over `lines`, up to and with the first
    /// `last` takes.
    async fn heard_until(
        lines: &mut tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
        last: impl Fn(&Frame) -> bool,
    ) -> Vec<Frame> {
        let mut heard = Vec::new();
        let waited = tokio::time::timeout(PATIENCE, async {
            while let Some(line) = lines.next_line().await.expect("the pipe reads") {
                let frame = serde_json::from_str::<Frame>(&line).expect("a frame");
                let done = last(&frame);
                heard.push(frame);
                if done {
                    return;
                }
            }
        })
        .await;
        assert!(waited.is_ok(), "the other end never said it: {heard:?}");
        heard
    }

    /// The quest above, and the one under it.
    const TOP: &str = "QEST-20261007-WCJQSG";
    const CHILD: &str = "QEST-20261115-5XF89V";

    /// `TOP` and `CHILD` under it, kept in `dir`, with `CHILD` shared and
    /// both given to the machine named, when one is.
    fn top_and_child(dir: &std::path::Path, given: Option<&str>) -> Places {
        let home = Places {
            quests: dir.join("quests"),
            scratch: dir.join("scratch"),
            home: dir.join("home"),
            state: dir.join("state"),
        };
        for (quest, readme) in [
            (TOP, "---\nparent: null\n---\n# Top\n".to_owned()),
            (CHILD, format!("---\nparent: {TOP}\n---\n# Child\n")),
        ] {
            std::fs::create_dir_all(home.quests.join(quest)).expect("the quest made");
            std::fs::write(home.quests.join(quest).join("README.md"), readme)
                .expect("its README written");
            if let Some(machine) = given {
                home.give(machine, quest).expect("given");
            }
        }
        home.offer(CHILD, &[]).expect("shared");
        home
    }

    /// skiff's end of a link sharing `home`'s quests, a worker's link when
    /// `worker`, run over a pipe; what is given back is the other end.
    fn skiff_sharing(
        home: Places,
        worker: bool,
    ) -> (
        tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
        WriteHalf<DuplexStream>,
    ) {
        let (near, far) = tokio::io::duplex(1 << 16);
        let (reading, writing) = tokio::io::split(near);
        let _skiff_end = tokio::spawn(async move {
            run(
                Memory::default(),
                &realm("skiff"),
                Connection {
                    worker,
                    ..connection(reading, writing)
                },
                Some(home),
                None,
            )
            .await
        });
        let (far_reading, far_writing) = tokio::io::split(far);
        (BufReader::new(far_reading).lines(), far_writing)
    }

    /// tern as it measures itself, ready, declaring nothing of itself.
    fn tern_undeclared() -> Machine {
        let mut tern = facts::measure("tern", &facts::Sources::nowhere());
        tern.state = Readiness::Ready;
        tern
    }

    /// A declaration of no reach, outliving nothing and tainted by nothing,
    /// which makes its machine a worker when it `costs_money`.
    fn declares(costs_money: bool) -> crate::declared::Declared {
        serde_json::from_value(json!({
            "reach": [], "outlives": [], "costs_money": costs_money, "taints": []
        }))
        .expect("a declaration")
    }

    /// Asks for `quest`'s README from its start.
    fn want_readme(quest: &str) -> Frame {
        Frame::Share(Say::Want {
            quest: quest.to_owned(),
            file: "record/README.md".to_owned(),
            at: Some(0),
        })
    }

    /// Whether `frame` carries a file of `quest`.
    fn carries_of(frame: &Frame, quest: &str) -> bool {
        matches!(frame, Frame::Share(Say::File { quest: of, .. }) if of == quest)
    }

    /// Whether `frame` names `quest` among those shared.
    fn names(frame: &Frame, quest: &str) -> bool {
        matches!(frame, Frame::Share(Say::Shares { quests, .. })
            if quests.iter().any(|named| named == quest))
    }

    /// Whether `frame` carries the parent or names it as shared.
    fn of_the_parent(frame: &Frame) -> bool {
        carries_of(frame, TOP) || names(frame, TOP)
    }

    /// What skiff sends over `lines`, up to the child's carries, once
    /// the far end has said `opening` and asked for both READMEs.
    async fn sent_once_both_asked(
        lines: &mut tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
        far_writing: &mut WriteHalf<DuplexStream>,
        opening: Vec<Frame>,
    ) -> Vec<Frame> {
        let mut said = opening;
        said.extend([want_readme(TOP), want_readme(CHILD)]);
        says(far_writing, &said).await;
        heard_until(lines, |frame| carries_of(frame, CHILD)).await
    }

    #[tokio::test]
    async fn a_parent_reaches_the_other_end_only_once_its_machine_declares_no_worker() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut lines, mut far_writing) = skiff_sharing(top_and_child(dir.path(), None), false);
        let shares = |frame: &Frame| matches!(frame, Frame::Share(Say::Shares { .. }));
        says(
            &mut far_writing,
            &[
                Frame::Hello(realm("tern").hello()),
                Frame::Have {
                    messages: Vec::new(),
                    living: Some(Vec::new()),
                },
            ],
        )
        .await;
        let first = heard_until(&mut lines, shares).await;
        assert!(
            first
                .last()
                .is_some_and(|frame| names(frame, CHILD) && !names(frame, TOP)),
            "before tern says what it is, skiff shares the quest, never its parent: {first:?}"
        );

        let mut tern = tern_undeclared();
        let asks = [want_readme(TOP), want_readme(CHILD)];
        let child_sent = |frame: &Frame| carries_of(frame, CHILD);
        says(&mut far_writing, &[Frame::Machine(Box::new(tern.clone()))]).await;
        says(&mut far_writing, &asks).await;
        let sent = heard_until(&mut lines, child_sent).await;
        assert!(
            !sent.iter().any(of_the_parent),
            "tern declares nothing of itself, as a restarting node does, so it is sent \
             nothing of the parent: {sent:?}"
        );

        // The parent asked for last, nothing asked is still to come once
        // it is heard.
        tern.declared = Some(declares(false));
        says(&mut far_writing, &[Frame::Machine(Box::new(tern.clone()))]).await;
        says(&mut far_writing, &[want_readme(CHILD), want_readme(TOP)]).await;
        heard_until(&mut lines, |frame| carries_of(frame, TOP)).await;

        tern.declared = Some(declares(true));
        says(&mut far_writing, &[Frame::Machine(Box::new(tern))]).await;
        says(&mut far_writing, &asks).await;
        let sent = heard_until(&mut lines, child_sent).await;
        assert!(
            !sent.iter().any(|frame| carries_of(frame, TOP)),
            "once tern says it costs money, it is sent nothing of the parent: {sent:?}"
        );
    }

    #[tokio::test]
    async fn a_worker_is_never_sent_a_parent_whatever_its_machine_says() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let home = top_and_child(dir.path(), Some("tern"));
        let (mut lines, mut far_writing) = skiff_sharing(home, true);
        let mut tern = tern_undeclared();
        tern.declared = Some(declares(false));
        let sent = sent_once_both_asked(
            &mut lines,
            &mut far_writing,
            vec![
                Frame::Hello(realm("tern").hello()),
                Frame::Have {
                    messages: Vec::new(),
                    living: Some(Vec::new()),
                },
                Frame::Machine(Box::new(tern)),
            ],
        )
        .await;
        assert!(
            !sent.iter().any(of_the_parent),
            "tern, a worker, declares itself no worker, and still hears nothing of the \
             parent, though it was given it: {sent:?}"
        );
    }

    #[tokio::test]
    async fn an_end_that_takes_no_facts_is_never_sent_a_parent() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut lines, mut far_writing) = skiff_sharing(top_and_child(dir.path(), None), false);
        let sent = sent_once_both_asked(
            &mut lines,
            &mut far_writing,
            vec![
                Frame::Hello(Hello {
                    facts: false,
                    ..realm("tern").hello()
                }),
                Frame::Have {
                    messages: Vec::new(),
                    living: Some(Vec::new()),
                },
            ],
        )
        .await;
        assert!(
            !sent.iter().any(of_the_parent),
            "tern takes no facts, so it can't say it is no worker, and hears nothing of \
             the parent: {sent:?}"
        );
    }

    /// The files `exchange` gives for what the other end asks of it over
    /// `lines`, once it has asked for any.
    async fn files_asked_for(
        lines: &mut tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
        exchange: &mut Exchange,
    ) -> Vec<Say> {
        let mut files = Vec::new();
        let asked = tokio::time::timeout(PATIENCE, async {
            while files.is_empty() {
                let line = lines
                    .next_line()
                    .await
                    .expect("the pipe reads")
                    .expect("the other end speaks");
                if let Ok(Frame::Share(say)) = serde_json::from_str::<Frame>(&line) {
                    let answers = exchange.hear(say).expect("the exchange hears");
                    files.extend(
                        answers
                            .into_iter()
                            .filter(|answer| matches!(answer, Say::File { .. })),
                    );
                }
            }
        })
        .await;
        assert!(asked.is_ok(), "the other end asked for a file");
        files
    }
}
