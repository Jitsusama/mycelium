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
pub mod dial;
pub mod discovery;
pub mod membership;
pub mod refused;
pub mod starts;
pub mod steers;

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::carry::kept;
use crate::facts::{self, Machine, Readiness};
use crate::handover;
use crate::hold;
use crate::mesh;
use crate::own_health;
use crate::share::exchange::{Exchange, Places, Say};
use crate::store::{self, Change, Claim, Store};
use crate::wire::{Ran, Reply, Response, Run, Start};
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
    /// The asker of a run has taken a piece of its output, so one more
    /// may be sent.
    Taken {
        /// Which ask.
        ask: u64,
    },
    /// The asker of a run has let go of it, so it is stopped.
    Unrun {
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
/// only the quests it was given, and those it began, which this machine
/// held none of.
///
/// Any other end may be a worker too until it says what its machine is,
/// which it does first of all when both ends take facts, so nothing above
/// the quests shared crosses before then. An end that takes none can
/// declare nothing, so it is no worker.
fn exchange_with(places: Places, here: &Hello, there: &Hello, worker: bool) -> Exchange {
    let exchange = Exchange::new(places, here.machine.clone(), there.machine.clone());
    let mut exchange = if worker {
        exchange.for_worker()
    } else {
        exchange
    };
    exchange.withhold_above(here.facts && there.facts);
    exchange
}

/// Runs this end of a link until the other end leaves, then marks every
/// session it carried here out of reach. With `shares`, it carries the
/// files of the quests this machine shares too, and with `starting`, it
/// carries starts.
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
    let (mut frames, _reader) = read_frames(reading);

    let other = greet(
        &mut writing,
        &mut frames,
        (&store, membership),
        releases,
        hello_within,
    )
    .await?;
    pinned(&store, named.as_deref(), &other.machine)?;
    // A dial that reached the machine says how a command runs there, even
    // when a link that machine dialled holds it and carries the starts.
    if let Some(Asking::Kept(asks, there)) = starting.as_ref().and_then(|s| s.asking.as_ref()) {
        asks.know(&other.machine, there.clone());
    }
    let claim = claimed(&store, &other.machine)?;
    let reach = reach_start(&store, &other.machine);
    admitted(&store, &other.machine)?;
    let exchange = shares.map(|places| exchange_with(places, &membership.hello(), &other, worker));
    let (mut asked, _reaching) = asked_of(starting.as_ref(), &other.machine);
    let (answering, mut answered) = mpsc::unbounded_channel();
    let (runs, mut run_said) = Runs::new();
    let inboxes = starting.as_ref().map(|starting| starting.steers.clone());
    let mut end = End {
        store,
        here: membership.hello().machine,
        realm: membership.hello().realm,
        proven: membership.proves(),
        worker,
        shows: membership.hello().shows && other.shows && !releases && !other.releases && !worker,
        handover: Handover {
            releases: other.releases,
            ..Handover::default()
        },
        // Two ends that both take facts each say their machine every
        // minute, so either hearing nothing for longer knows the other
        // is gone.
        hearing: Hearing::from_now(membership.hello().facts && other.facts),
        following: inboxes.clone().map(Following::new),
        inboxes,
        held: starting.as_ref().map(|starting| starting.held.clone()),
        health: health_of(starting.as_ref()),
        declared: declared_of(starting.as_ref()),
        node: starting.map(|starting| starting.node),
        answering,
        asks: HashMap::new(),
        runs,
        next_ask: 0,
        other: other.machine.clone(),
        said: other,
        reach,
        has: HashSet::new(),
        sent: BTreeMap::new(),
        carried: HashSet::new(),
        exchange,
        _claim: claim,
    };

    end.open(&mut writing).await?;

    let (mut keeping, mut looking) = paces();
    let result = loop {
        let step = tokio::select! {
            frame = frames.recv() => match frame {
                Some(Ok(frame)) => end.take(frame, &mut writing).await,
                Some(Err(error)) => Err(error),
                None => break Ok(Ending::Left),
            },
            _ = keeping.tick() => end.keep(&mut writing).await,
            Some(ask) = next(&mut asked) => end.ask(ask, &mut writing).await,
            Some((ask, reply)) = answered.recv() => {
                // Work started here has reached this machine, however soon
                // it ends: a minute's keep may never see it running.
                end.handover.ran |= matches!(reply, Ok(Response::Started(_)));
                send(&mut writing, &Frame::Started { ask, reply }).await
            }
            Some(said) = run_said.recv() => end.run_said(said, &mut writing).await,
            _ = looking.tick() => end.look(&mut writing).await,
            () = own_health::moved(&mut end.health.told) => end.send_machine(&mut writing).await,
            change = changes.changes.recv() => match change {
                Some(change) => end.changed(change, &mut writing).await,
                None => break Ok(Ending::Left),
            },
        };
        if let Err(error) = step {
            break Err(error);
        }
        if end.handover.done {
            break Ok(Ending::HandedOver);
        }
    };
    end.leave(&result);
    result
}

/// When a link keeps what it said, a minute on since the start has just
/// said all of it, and when it looks at what it carries.
fn paces() -> (tokio::time::Interval, tokio::time::Interval) {
    let keeping = tokio::time::interval_at(tokio::time::Instant::now() + KEEP_EVERY, KEEP_EVERY);
    (keeping, tokio::time::interval(LOOK_EVERY))
}

/// The next of what `asked` holds, or never when there is none.
async fn next<T>(asked: &mut Option<mpsc::UnboundedReceiver<T>>) -> Option<T> {
    match asked {
        Some(asked) => asked.recv().await,
        None => std::future::pending().await,
    }
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

/// The starts this machine's node asks of `machine`, taken for this link
/// until what it answers is dropped, when the link carries the node's
/// starts: in the node's own process for a link it keeps, and over its
/// socket for one `machine` dialled. None otherwise.
fn asked_of(
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
            let (asked, carrying) = carried::from_node(starting.node.clone(), machine);
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
        None => Declaring::File(crate::place::declared()),
        Some(Starting {
            declared: Some(file),
            ..
        }) => Declaring::File(file.clone()),
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
    /// From this file.
    File(PathBuf),
    /// From the node on this socket, asked for what it measures this
    /// machine to be, as a link another machine dialled does, since it
    /// runs apart from the node and is started by whatever SSH runs.
    Node(PathBuf),
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

/// Keeps that this store is linked with `machine` as of now, and forgets
/// why a link with it was last refused.
/// This link's hold on `machine`, refused when another link carries it.
fn claimed(store: &impl Store, machine: &str) -> io::Result<Claim> {
    store
        .claim(machine)?
        .ok_or_else(|| refusal(format!("a link to {machine} already carries it here")))
}

fn admitted(store: &impl Store, machine: &str) -> io::Result<()> {
    store.mark_linked(machine, now())?;
    if let Err(error) = store.link_admitted(machine) {
        eprintln!("{machine} linked, but its last refusal stands: {error}");
    }
    Ok(())
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
    send(writing, &Frame::Hello(ours)).await?;
    let other = hello(frames, within).await?;
    if let Err(why) = membership.admit(&other) {
        if let Err(error) = store.refuse_link(&other.machine, &why) {
            eprintln!("why {} was refused isn't kept: {error}", other.machine);
        }
        return Err(refusal(why));
    }
    Ok(other)
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
            "the other end closed before saying hello",
        )),
    }
}

/// A living session's presence, the leases it holds and the id its holder
/// keeps it by, as last sent: the presence shared with the store's reading
/// of it, so one unchanged is compared by pointer and never copied.
type Sent = (Arc<Value>, Vec<Value>, Option<String>);

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
    /// This machine's sessions' steer inboxes, if this end carries steers.
    inboxes: Option<PathBuf>,
    /// The talks this end carries back to the other.
    following: Option<Following>,
    /// This machine's node's socket, which starts asked of it go to.
    node: Option<PathBuf>,
    /// Where the answers to starts asked of this machine go, to be sent.
    answering: mpsc::UnboundedSender<(u64, Reply)>,
    /// Starts asked of the other machine and not yet answered.
    asks: HashMap<u64, oneshot::Sender<Reply>>,
    /// The runs this end carries, either way.
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
    _claim: Claim,
}

/// This machine's own health as an end of a link says it.
struct OwnHealth {
    /// The health, if this end says it.
    told: Option<own_health::Told>,
    /// The watch of the node's health, for an end that runs apart from it.
    _watching: Option<own_health::Watching>,
}

/// The runs an end carries: those it asked of the other machine, and those
/// the other end asked of this one.
struct Runs {
    /// Where what each run asked of the other machine says goes, by ask.
    asked: HashMap<u64, mpsc::UnboundedSender<Ran>>,
    /// Each run the other end asked of this machine, by its ask.
    here: HashMap<u64, RunningHere>,
    /// Where runs tell the link's loop what to send.
    saying: mpsc::UnboundedSender<RunSaid>,
}

impl Runs {
    /// None yet, and where what runs tell the link's loop comes out.
    fn new() -> (Self, mpsc::UnboundedReceiver<RunSaid>) {
        let (saying, said) = mpsc::unbounded_channel();
        let runs = Self {
            asked: HashMap::new(),
            here: HashMap::new(),
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

    /// Lets the run asked as `ask` of this machine send one more piece.
    fn taken(&self, ask: u64) {
        if let Some(running) = self.here.get(&ask) {
            running.window.add_permits(1);
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
}

impl<S: Store> End<S> {
    /// Marks the link left as it ends, however it ended, and its machine
    /// out of reach.
    fn leave(&self, result: &io::Result<Ending>) {
        // Had the link died outright, the last minute's mark would stand.
        let left = now();
        let _ = self.store.mark_linked(&self.other, left);
        // A handover is the link's work done, so only a drop counts as one.
        if !matches!(result, Ok(Ending::HandedOver)) {
            let _ = self.store.mark_ended(&self.other, left);
        }
        store::out_of_reach(&self.store, &self.other, left);
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
                held,
            } => self.hear_presence(&session, body, leases, held),
            Frame::Ended { session } => {
                self.carried.remove(&session);
                self.store.remove_presence(&session, &self.other)?;
            }
            Frame::Share(say) => {
                self.share(move |exchange| exchange.hear(say), writing)
                    .await?;
            }
            Frame::Machine(said) => self.keep_machine(*said),
            Frame::Carries { carries } => self.keep_carries(&carries),
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
            Frame::Carry {
                ask,
                quest,
                to,
                checked,
            } => self.carry(ask, (quest, to), checked),
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
            Frame::Start { ask, .. } => Some(self.refused(*ask, "starts nothing for")),
            Frame::Steer { ask, .. } => Some(self.refused(*ask, "takes no steers from")),
            Frame::Update { ask } => Some(self.refused(*ask, "updates nothing for")),
            Frame::Carry { ask, .. } => Some(self.refused(*ask, "carries nothing for")),
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
            | Frame::Presence { .. }
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
            | Frame::Unrun { .. } => None,
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
    /// machine already holds isn't written over, and one from a session
    /// another machine holds would put words in that session's mouth.
    fn message_refused(&self, id: &str, body: &Value) -> Option<Refused> {
        let other = &self.other;
        if self.store.message(id).is_some() {
            return Some(Refused::Dropped(format!(
                "{other} said {id} again, which a worker can't write over"
            )));
        }
        let from = body.get("from").and_then(Value::as_str)?;
        let someone_elses = self.store.presences().iter().any(|presence| {
            presence.session.eq_ignore_ascii_case(from)
                && presence.elsewhere.as_deref() != Some(other.as_str())
        });
        someone_elses.then(|| {
            Refused::Dropped(format!(
                "{other} said {id} as {from}, a session of another machine's, which a worker can't"
            ))
        })
    }

    /// Whether `address` is a session of the other end's own that this
    /// link carries here.
    fn carries(&self, address: &str) -> bool {
        self.carried
            .iter()
            .any(|session| session.eq_ignore_ascii_case(address))
    }

    /// Asks the other machine to start what this machine's node placed
    /// there, or answers why it can't be asked.
    async fn ask(&mut self, ask: Ask, writing: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        let carried = match &ask.asked {
            Asked::Start(_) => self.said.starts.then_some(()).ok_or("start sessions"),
            Asked::Steer(_) => self.said.steers.then_some(()).ok_or("carry steers"),
            Asked::Update => self.said.updates.then_some(()).ok_or("be asked to update"),
            Asked::Run(..) => self.said.runs.then_some(()).ok_or("run commands"),
            Asked::Carry { .. } => self.said.carries.then_some(()).ok_or("carry sessions"),
        };
        if let Err(cannot) = carried {
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
            self.runs.asked.insert(id, self.follow_run(id, asker));
            // The asker may have given up; then the run is let go.
            let _ = ask.answer.send(Ok(Response::Running {
                machine: self.other.clone(),
            }));
            return send(writing, &Frame::Run { ask: id, run }).await;
        }
        self.asks.insert(id, ask.answer);
        let frame = match ask.asked {
            Asked::Start(start) => Frame::Start { ask: id, start },
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
            Asked::Run(..) => unreachable!("a run is sent above"),
        };
        send(writing, &frame).await
    }

    /// Hands what the run asked of the other machine as `ask` says on to
    /// `asker`, telling the other end each time a piece is taken, so it
    /// sends one more, and once `asker` lets go, so it stops the run.
    /// Answers where the link puts what the run says.
    fn follow_run(&self, ask: u64, asker: mpsc::Sender<Ran>) -> mpsc::UnboundedSender<Ran> {
        let (said, mut saying) = mpsc::unbounded_channel::<Ran>();
        let (loop_, other) = (self.runs.saying.clone(), self.other.clone());
        tokio::spawn(async move {
            loop {
                let ran = tokio::select! {
                    ran = saying.recv() => ran.unwrap_or_else(|| {
                        Ran::Failed(format!("the link to {other} dropped before the run ended"))
                    }),
                    () = asker.closed() => break,
                };
                let ends = ran.ends();
                if asker.send(ran).await.is_err() {
                    break;
                }
                if ends {
                    return;
                }
                // The link outlives what it carries, or nobody needs this.
                let _ = loop_.send(RunSaid::Taken(ask));
            }
            let _ = loop_.send(RunSaid::Unrun(ask));
        });
        said
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
    ) {
        // The held id is the link's to say, never the presence's.
        if let Some(fields) = body.as_object_mut() {
            fields.remove(mesh::HELD_AS);
            if let Some(held) = held {
                fields.insert(mesh::HELD_AS.to_owned(), Value::String(held));
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
        self.share(Exchange::look, writing).await?;
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
    /// from the link's own work, since starting waits on the holder.
    fn answer(&self, ask: u64, start: Start) {
        let answering = self.answering.clone();
        let Some(node) = self.node.clone() else {
            let refused = Err(format!("{} starts nothing over this link", self.here));
            // The link outlives this call, so its loop is there to send it.
            let _ = answering.send((ask, refused));
            return;
        };
        let here = self.here.clone();
        tokio::spawn(async move {
            let reply = starts::answer(node, here, start).await;
            // A link that dropped meanwhile has nobody to answer.
            let _ = answering.send((ask, reply));
        });
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
                .zip(
                    self.exchange
                        .as_ref()
                        .map(|exchange| exchange.places().clone()),
                )
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

    /// Keeps what the other end says its machine is, which only it can
    /// say: a machine named otherwise is refused, and it is ready while
    /// this link carries it, whatever it claims. A worker is withheld the
    /// quests above those shared with it, and so is a machine that declares
    /// nothing of itself, as a node does for a while as it restarts.
    fn keep_machine(&mut self, mut said: Machine) {
        if said.name != self.other {
            self.kept(
                &said.name,
                Err(refusal(format!("{} spoke for another machine", self.other))),
            );
            return;
        }
        said.state = Readiness::Ready;
        if let Some(exchange) = self.exchange.as_mut() {
            exchange.withhold_above(
                said.declared
                    .as_ref()
                    .is_none_or(crate::declared::Declared::is_worker),
            );
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
        let declared = match &self.declared {
            Declaring::File(file) => Some(file.clone()),
            Declaring::Node(node) => match measured_by(node).await {
                Some(machine) => return self.say_machine(machine, writing).await,
                // Nothing the account's files declare is said for it:
                // anything running as the account could have written them.
                None => None,
            },
        };
        let sources = facts::Sources {
            declared,
            ..facts::Sources::here()
        };
        let machine =
            tokio::task::spawn_blocking(move || facts::measure_in(&here, &realm, &sources))
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
        self.handover.leaves = machine
            .declared
            .as_ref()
            .and_then(|declared| declared.leaves_at);
        send(writing, &Frame::Machine(Box::new(machine))).await
    }

    /// Says again that what this link carried here stands, and what this
    /// end's machine is now, and whether it has handed over.
    async fn keep(&mut self, writing: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        if self.hearing.gone(now()) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{} has said nothing for {SILENT_AFTER:?}", self.other),
            ));
        }
        self.keep_carried()?;
        self.send_machine(writing).await?;
        self.hand_over(writing).await
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
        if !handover::handed_over(leaves, jiff::Timestamp::now(), ran, running) {
            return Ok(());
        }
        self.share(Exchange::look, writing).await?;
        send(writing, &Frame::HandedOver).await
    }

    /// Takes the other end's word that its machine has handed over, if this
    /// end holds all it said: frames cross in the order they are said, so
    /// every file it holds has been heard of, and asked for if wanted.
    fn heard_handed_over(&mut self) {
        let lacking = self.exchange.as_ref().map_or(0, Exchange::waiting);
        if lacking == 0 {
            self.handover.done = true;
        } else {
            eprintln!(
                "{} has handed over, but {lacking} of its files haven't reached this machine yet",
                self.other
            );
        }
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
        let holdings = self.store.holdings();
        for (session, (body, held)) in living {
            let said = (body, mesh::carried_leases(&holdings, &session), held);
            if self.sent.get(&session) != Some(&said) {
                send(
                    writing,
                    &Frame::Presence {
                        session: session.clone(),
                        body: (*said.0).clone(),
                        leases: Some(said.1.clone()),
                        held: said.2.clone(),
                    },
                )
                .await?;
                self.sent.insert(session, said);
            }
        }
        Ok(())
    }

    /// This machine's living sessions, as their presences say, with the id
    /// a holder here keeps each by, if one does.
    fn living(&self) -> BTreeMap<String, (Arc<Value>, Option<String>)> {
        let held = self.held.as_deref().map(hold::held).unwrap_or_default();
        self.store
            .presences()
            .into_iter()
            .filter(|presence| presence.elsewhere.is_none() && presence.lives)
            .map(|presence| {
                let id = mesh::held_by(&presence, &held).map(|one| one.id.clone());
                (presence.session, (presence.body, id))
            })
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

/// When this end last heard the other end, and whether the other says
/// its machine each minute, so that hearing nothing for longer means it
/// is gone.
struct Hearing {
    /// Whether the other end says its machine each minute.
    watched: bool,
    /// When this end last heard the other say anything, in milliseconds
    /// of the wall clock.
    heard: i64,
}

impl Hearing {
    /// Hearing from now, `watched` or not.
    fn from_now(watched: bool) -> Self {
        Self {
            watched,
            heard: now(),
        }
    }

    /// Whether the other end is gone by `now`.
    fn gone(&self, now: i64) -> bool {
        self.watched && silent(self.heard, now)
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

/// Whether a presence a worker said speaks only for a session of its own
/// machine, `worker`: one that names no machine, or names that one.
fn speaks_for(body: &Value, worker: &str) -> bool {
    body.get(store::folders::MACHINE)
        .is_none_or(|machine| machine.as_str() == Some(worker))
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

    fn connection(reading: ReadHalf<DuplexStream>, writing: WriteHalf<DuplexStream>) -> Connection {
        Connection {
            reading: Box::new(reading),
            writing: Box::new(writing),
            keep: Box::new(()),
            refused: None,
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
            health: Some(slot.subscribe()),
            declared: Some(dir.path().join("declared.json")),
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

    /// Skiff's store, hearing kestrel's end of a link skiff dialled, which
    /// runs apart from kestrel's node on `node`, as SSH starts it.
    fn dialled_by_kestrel(dir: &std::path::Path, node: PathBuf) -> Memory {
        let skiff = Memory::default();
        let (near, far) = tokio::io::duplex(1 << 16);
        let near_end = end(&skiff, "skiff", near);
        let starting = Starting {
            node,
            held: dir.join("held"),
            asking: Some(Asking::Dialled),
            steers: dir.join("steer"),
            health: None,
            declared: None,
        };
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
                    home: "/".into(),
                    queue: None,
                    disk_guard: None,
                    declared: Some(declared.clone()),
                    reached: "/nowhere".into(),
                    updated: "/nowhere".into(),
                };
                let measured = facts::measure_in("kestrel", "work", &sources);
                let reply: Reply = Ok(Response::Machine(Box::new(measured)));
                let mut said = serde_json::to_vec(&reply).expect("a reply");
                said.push(b'\n');
                writing.write_all(&said).await.expect("the node answers");
            }
        })
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
        assert!(waited.is_ok(), "skiff's session never crossed");
        assert!(
            !heard.iter().any(|frame| matches!(frame, Frame::Machine(_))),
            "an end that doesn't say it knows of machines hears none: {heard:?}"
        );

        let nowhere = facts::Sources {
            home: "/".into(),
            queue: None,
            disk_guard: None,
            declared: None,
            reached: "/nowhere".into(),
            updated: "/nowhere".into(),
        };
        let mut tern = facts::measure("tern", &nowhere);
        tern.state = Readiness::Ready;
        for frame in [
            Frame::Have {
                messages: Vec::new(),
                living: Some(vec!["walgit".to_owned()]),
            },
            Frame::Machine(Box::new(tern)),
            Frame::Presence {
                session: "walgit".to_owned(),
                body: json!({"session": "walgit"}),
                leases: None,
                held: None,
            },
        ] {
            send(&mut far_writing, &frame)
                .await
                .expect("the far end speaks");
        }
        until(&skiff, |store| {
            store.presences().iter().any(|p| p.session == "walgit")
        })
        .await;
        assert!(
            skiff.machines().is_empty(),
            "kestrel can't say what tern is"
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
            assert!(waited.is_ok(), "{said}: skiff's session never crossed");
            let crossed = heard
                .iter()
                .any(|frame| matches!(frame, Frame::Carries { .. }));
            assert_eq!(crossed, said.ends_with("too many"), "{said}: {heard:?}");

            for frame in [
                Frame::Have {
                    messages: Vec::new(),
                    living: Some(vec!["walgit".to_owned()]),
                },
                Frame::Carries { carries: told },
                Frame::Presence {
                    session: "walgit".to_owned(),
                    body: json!({"session": "walgit"}),
                    leases: None,
                    held: None,
                },
            ] {
                send(&mut far_writing, &frame)
                    .await
                    .expect("the far end speaks");
            }
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
        store::out_of_reach(&skiff, "kestrel", now());
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
    async fn a_second_link_to_a_machine_already_linked_is_refused() {
        let skiff = Memory::default();
        let kestrel = Memory::default();
        kestrel.start_here("walgit", json!({"session": "walgit"}), true);
        let _first = linked(&skiff, &kestrel, 1 << 16);
        until(&skiff, |store| !store.presences().is_empty()).await;

        let (near, far) = linked(&skiff, &kestrel, 1 << 16);
        let refused = near.await.expect("the second link's end runs to its end");
        assert!(
            refused.is_err(),
            "skiff already carries the Mac by the first link"
        );
        drop(far);
        assert_eq!(skiff.presences().len(), 1, "the first link's sessions stay");
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

    #[tokio::test]
    async fn a_link_that_can_release_ends_once_the_other_machine_hands_over() {
        let skiff = Memory::default();
        let store = skiff.clone();
        let (near, far) = tokio::io::duplex(1 << 16);
        let (reading, writing) = tokio::io::split(near);
        let skiff_end = tokio::spawn(async move {
            run(
                store,
                &realm("skiff"),
                Connection {
                    releases: true,
                    ..connection(reading, writing)
                },
                None,
                None,
            )
            .await
        });
        let (far_reading, mut far_writing) = tokio::io::split(far);
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

        for frame in [
            Frame::Hello(realm("lent").hello()),
            Frame::Have {
                messages: Vec::new(),
                living: Some(Vec::new()),
            },
            Frame::HandedOver,
        ] {
            send(&mut far_writing, &frame)
                .await
                .expect("the far end speaks");
        }
        let ended = tokio::time::timeout(PATIENCE, skiff_end)
            .await
            .expect("skiff's end ends")
            .expect("skiff's end runs");

        assert_eq!(
            ended.expect("the link doesn't fail"),
            Ending::HandedOver,
            "holding all the other machine said, it may be released"
        );
        assert_eq!(skiff.endings("lent", 0), 0, "a handover is no drop");
    }

    /// What a worker, tern, says to the machine that dialled it: its hello,
    /// an ask for a file of `quest`, a presence of its own and one naming
    /// the dialler's, the message `message`, then a start, a steer, an
    /// update, a run and a carry, asked as 1 to 5.
    fn a_workers_frames(quest: &str, message: &str) -> Vec<Frame> {
        let start: Start =
            serde_json::from_value(json!({"work": {}, "command": ["pi"]})).expect("a start");
        let presence = |session: &str, body: Value| Frame::Presence {
            session: session.to_owned(),
            body,
            leases: None,
            held: None,
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
                body: json!({"id": message, "to": ["@WCJQSG"], "text": "done"}),
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
        let (near, far) = tokio::io::duplex(1 << 16);
        let (reading, writing) = tokio::io::split(near);
        let _skiff_end = tokio::spawn(async move {
            run(
                store,
                &realm("skiff"),
                Connection {
                    worker: true,
                    ..connection(reading, writing)
                },
                Some(here),
                None,
            )
            .await
        });
        let (far_reading, mut far_writing) = tokio::io::split(far);
        let mut lines = BufReader::new(far_reading).lines();

        let message = recent(0, "SAK9FZ", 0);
        for frame in a_workers_frames(quest, &message) {
            send(&mut far_writing, &frame)
                .await
                .expect("the worker speaks");
        }

        let mut answers = BTreeMap::new();
        let mut ran = None;
        let mut said = Vec::new();
        tokio::time::timeout(PATIENCE, async {
            while answers.len() < 4 || ran.is_none() {
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
            ]),
            "a worker can't start, steer, update or carry anything on the machine that dialled it"
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
        let (near, far) = tokio::io::duplex(1 << 16);
        let (reading, writing) = tokio::io::split(near);
        let _skiff_end = tokio::spawn(async move {
            run(
                store,
                &realm("skiff"),
                Connection {
                    worker: true,
                    ..connection(reading, writing)
                },
                None,
                None,
            )
            .await
        });
        let (_far_reading, mut far_writing) = tokio::io::split(far);

        let (posing, own) = (recent(0, "OSPR01", 1), recent(0, "SAK9FZ", 2));
        let message = |id: &str, from: &str, text: &str| Frame::Message {
            id: id.to_owned(),
            body: json!({"id": id, "from": from, "to": ["@WCJQSG"], "text": text}),
        };
        for frame in [
            Frame::Hello(realm("tern").hello()),
            Frame::Have {
                messages: Vec::new(),
                living: Some(Vec::new()),
            },
            Frame::Presence {
                session: "sak9fz".to_owned(),
                body: json!({"session": "sak9fz"}),
                leases: None,
                held: None,
            },
            message(&posing, "OSPR01", "osprey says so"),
            message(&held, "SAK9FZ", "theirs"),
            message(&own, "SAK9FZ", "done"),
        ] {
            send(&mut far_writing, &frame)
                .await
                .expect("the worker speaks");
        }

        until(&skiff, |store| store.message(&own).is_some()).await;
        assert!(
            skiff.message(&posing).is_none(),
            "a message from a session osprey holds isn't the worker's to say"
        );
        assert_eq!(
            skiff
                .message(&held)
                .and_then(|body| body["text"].as_str().map(str::to_owned)),
            Some("ours".to_owned()),
            "nor is a message skiff holds written over"
        );
    }

    /// Starts as skiff's are carried, with its sessions' inboxes in
    /// `dir`, and no node to start them.
    fn steering(dir: &std::path::Path) -> Starting {
        Starting {
            node: dir.join("node.sock"),
            held: dir.join("held"),
            asking: None,
            steers: dir.join("inboxes"),
            health: None,
            declared: Some(dir.join("declared.json")),
        }
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
        for frame in [
            Frame::Hello(realm("tern").hello()),
            Frame::Have {
                messages: Vec::new(),
                living: Some(Vec::new()),
            },
            Frame::Presence {
                session: "sak9fz".to_owned(),
                body: json!({"session": "sak9fz"}),
                leases: None,
                held: None,
            },
            replies("OSPR01", "{\"posing\":true}\n"),
            replies("SAK9FZ", "{\"done\":true}\n"),
        ] {
            send(&mut far_writing, &frame)
                .await
                .expect("the worker speaks");
        }

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

        for frame in [
            Frame::Hello(realm("tern").hello()),
            Frame::Follow {
                talks: vec![steer::Talk {
                    address: "WCJQSG".to_owned(),
                    id: "t1".to_owned(),
                    has: 0,
                }],
            },
        ] {
            send(&mut far_writing, &frame)
                .await
                .expect("the worker speaks");
        }

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
        let (near, far) = tokio::io::duplex(1 << 16);
        let (reading, writing) = tokio::io::split(near);
        let skiff_end = tokio::spawn(async move {
            run(
                store,
                &realm("skiff"),
                Connection {
                    releases: true,
                    ..connection(reading, writing)
                },
                Some(home),
                None,
            )
            .await
        });
        let (far_reading, mut far_writing) = tokio::io::split(far);
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
        for frame in &frames {
            send(&mut far_writing, frame)
                .await
                .expect("the lent machine speaks");
        }
        until(&skiff, |store| store.message(&said).is_some()).await;
        assert!(
            !skiff_end.is_finished(),
            "skiff keeps the link while the README it asked for is on its way"
        );

        for frame in files_asked_for(&mut lines, &mut exchange)
            .await
            .into_iter()
            .map(Frame::Share)
            .chain([Frame::HandedOver])
        {
            send(&mut far_writing, &frame)
                .await
                .expect("the lent machine speaks");
        }
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
        let nowhere = facts::Sources {
            home: "/".into(),
            queue: None,
            disk_guard: None,
            declared: "/nowhere".into(),
            reached: "/nowhere".into(),
            updated: "/nowhere".into(),
        };
        let mut tern = facts::measure("tern", &nowhere);
        tern.state = Readiness::Ready;
        tern.declared = None;
        tern
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
            !sent
                .iter()
                .any(|frame| carries_of(frame, TOP) || names(frame, TOP)),
            "tern declares nothing of itself, as a restarting node does, so it is sent \
             nothing of the parent: {sent:?}"
        );

        let declares = |costs_money: bool| {
            serde_json::from_value(json!({
                "reach": [], "outlives": [], "costs_money": costs_money, "taints": []
            }))
            .expect("a declaration")
        };
        tern.declared = Some(declares(false));
        says(&mut far_writing, &[Frame::Machine(Box::new(tern.clone()))]).await;
        heard_until(&mut lines, |frame| names(frame, TOP)).await;

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
        says(
            &mut far_writing,
            &[
                Frame::Hello(realm("tern").hello()),
                Frame::Have {
                    messages: Vec::new(),
                    living: Some(Vec::new()),
                },
                Frame::Machine(Box::new(tern_undeclared())),
                want_readme(TOP),
                want_readme(CHILD),
            ],
        )
        .await;
        let sent = heard_until(&mut lines, |frame| carries_of(frame, CHILD)).await;
        assert!(
            !sent
                .iter()
                .any(|frame| carries_of(frame, TOP) || names(frame, TOP)),
            "tern, a worker, declares nothing of itself, and still hears nothing of the \
             parent, though it was given it: {sent:?}"
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
