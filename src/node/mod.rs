//! The node: one per machine and realm. It keeps its store linked with the
//! stores of the machines discovery finds, holds the sessions and messages
//! in memory, and serves the local socket every client reads.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinSet;

use crate::awake::{self, Awake};
use crate::daemon;
use crate::facts::{self, Readiness};
use crate::flow::{self, Flows};
use crate::group::{Pattern, Routes};
use crate::handover::{self, Sleeping, Told};
use crate::hold::{self, start, start::Starter};
use crate::hosted_attach;
use crate::link::dial::{Dial, Program};
use crate::link::refused::Refusals;
use crate::link::starts::{Asking, Asks, Starting};
use crate::link::{self, discovery::Discovery, membership::Hello, membership::Membership};
use crate::made::{self, Book, Made};
use crate::mesh;
use crate::offer::{self, Offer};
use crate::place;
use crate::placement::{self, Miss, Need, Placement, Work};
use crate::record::{Message, Session, State as SessionState};
use crate::room;
use crate::served::{self, Served};
use crate::share::exchange::Places;
use crate::state::State;
use crate::steer::{self, Steer};
use crate::store::{self, Change, Store};
use crate::wire::{Answer, Draft, Event, Offers, Reply, Request, Response, Start, Started};
use crate::within;

mod crossing;
mod standing;

/// How long a link waits before dialling again after it dropped, at first
/// and at most; each failure in a row doubles the wait.
const REDIAL_FIRST: Duration = Duration::from_secs(2);
const REDIAL_MOST: Duration = Duration::from_secs(60);

/// How long a link's way in may take to finish saying why it ended, once
/// the link is over.
const SAID_WITHIN: Duration = Duration::from_secs(5);

/// How often the node looks whether its machine leaves soon enough for its
/// sessions to be told to hand over (`handover`).
const LEAVING_EVERY: Duration = Duration::from_secs(60);

/// How many events a watch may fall behind by before the node lets it go;
/// a client that can't keep up connects again and is sent the whole state.
const WATCH_BACKLOG: usize = 4096;

/// How long a watch's client may take to read one event before the node
/// lets it go.
const WATCH_PATIENCE: Duration = Duration::from_secs(10);

/// A node's socket, bound and taken down when dropped.
pub struct Socket {
    listener: UnixListener,
    path: PathBuf,
}

impl Socket {
    /// The next client to connect.
    pub async fn accept(&self) -> io::Result<UnixStream> {
        self.listener.accept().await.map(|(stream, _)| stream)
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        // The socket is the node's alone, made at bind; if it's already
        // gone there is nothing to take down.
        let _ = fs::remove_file(&self.path);
    }
}

/// Binds the socket at `path` in a folder only this account can enter,
/// refusing a folder anyone else could, and refusing to take over a socket
/// a live node still serves or a path that isn't a socket.
pub fn bind(path: &Path) -> io::Result<Socket> {
    let refused = |why: String| io::Error::new(io::ErrorKind::PermissionDenied, why);
    if let Some(parent) = path.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        let folder = fs::metadata(parent)?;
        if folder.uid() != rustix::process::geteuid().as_raw() || folder.mode() & 0o077 != 0 {
            return Err(refused(format!(
                "{} is open to others, so the socket can't go there",
                parent.display()
            )));
        }
    }
    if let Ok(held) = fs::symlink_metadata(path) {
        if !held.file_type().is_socket() {
            return Err(refused(format!(
                "{} is not a socket, so it is left alone",
                path.display()
            )));
        }
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("a node already serves {}", path.display()),
            ));
        }
        fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(Socket {
        listener,
        path: path.to_owned(),
    })
}

/// What a node is given to run with besides its seams.
pub struct Given {
    /// The quests this machine shares, whose files its links carry.
    pub sharing: Option<Places>,
    /// How it starts the sessions it is asked to, if it starts any.
    pub starter: Option<Starter>,
    /// How it keeps the refusals of links whose way in turned it away.
    pub refusals: Refusals,
    /// This machine's sessions' steer inboxes.
    pub steers: PathBuf,
    /// How it keeps its machine awake while it holds sessions.
    pub awake: Awake,
    /// The socket its machine's updater takes asks on.
    pub updater: PathBuf,
    /// The files it serves in the daemon interface, as kinds of their own.
    pub serves: BTreeMap<String, served::Source>,
    /// The providers' programs asked what machines they could make.
    pub providers: Vec<String>,
    /// Where it keeps each machine it makes and lets go.
    pub book: Book,
    /// The realms its messages may go to and come from, and where it
    /// keeps each crossing.
    pub flows: Flows,
    /// Where the harness marks each session it parks here.
    pub parked: PathBuf,
}

/// How long a machine made has to link: a provider's machine is ready in
/// seconds, and its node in a few more.
const LINKED_WITHIN: Duration = Duration::from_secs(5 * 60);

/// The kinds the node holds itself in the daemon interface, which no
/// served file may take.
pub const HOLDS: [&str; 3] = ["session", "machine", "message"];

/// How many of each group's latest messages the daemon interface holds as
/// `message` records: a conversation's recent tail, as a person reads it,
/// rather than every message the node keeps, which a watch would rebuild
/// and compare at every message heard.
pub const MESSAGES_EACH: usize = 100;

/// A presence as a session, with what holds it here and the window it is
/// in. A hosted session is held too when a holder here joins it, as
/// Joel's own window does once its pi handed it to a host.
fn session_held(presence: &store::Presence, machine: &str, held: &[hold::Held]) -> Option<Session> {
    let mut session = mesh::session(presence, machine)?;
    let holding = mesh::held_by(presence, held).or_else(|| mesh::joined_by(presence, held));
    if let Some(one) = holding {
        session.held = Some(one.id.clone());
        // A held session is in whichever window joined it last and is
        // still joined, or in none.
        session.window = one.windows.last().cloned();
    }
    Some(session)
}

/// Tells each machine within this one, as its configuration declares
/// them, this one's power and room as `here` measures them.
fn tell_hosted(here: &facts::Machine) {
    let Some(hosts) = here.declared.as_ref().map(|declared| &declared.hosts) else {
        return;
    };
    let said = within::Said {
        at: jiff::Timestamp::now(),
        power: here.power,
        room: here.room.clone(),
    };
    for hosted in hosts {
        if let Err(error) = within::tell(&hosted.file, &said) {
            eprintln!(
                "{} wasn't told this machine's power and room: {error}",
                hosted.machine
            );
        }
    }
}

/// Runs a node serving `socket` until `stop` resolves, then ends its links
/// and lets go of the sessions they carried.
pub async fn run<S, M, D>(
    store: S,
    membership: M,
    discovery: D,
    given: Given,
    socket: Socket,
    stop: impl Future<Output = ()>,
) -> io::Result<()>
where
    S: Store,
    M: Membership + Clone,
    D: Discovery,
{
    store::out_of_reach_of_unclaimed(&store)?;
    let (to_dial, mut made) = mpsc::unbounded_channel();
    let (shared, kept) = Shared::of(store.clone(), membership.hello(), given, to_dial);
    let Kept {
        sharing,
        refusals,
        mut awake,
    } = kept;
    let mut changes = store.changes()?;
    let (_watching_held, mut held) = watch(&shared.held, "what is held");
    let (_watching_parks, mut parks) = watch(&shared.parked, "what is parked");
    shared.read_store();
    let mut links = JoinSet::new();
    let mut found = discovery.found();
    let mut leaving = tokio::time::interval(LEAVING_EVERY);
    leaving.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // What the machine runs on, read with each minute's measure rather
    // than as each session changes: a battery's charge is read from its
    // embedded controller, which blocks the node's one thread on every
    // read, and a charge moves too slowly for a minute to matter. Measured
    // before anything can ask, so the first reader of the machines finds
    // this one among them.
    let mut power = shared.keep_measured(&shared.measure());
    tokio::pin!(stop);
    let result = loop {
        tokio::select! {
            () = &mut stop => break Ok(()),
            accepted = socket.accept() => match accepted {
                Ok(stream) => {
                    tokio::spawn(serve(stream, shared.clone()));
                }
                Err(error) => break Err(error),
            },
            Some(dial) = found.recv() => {
                let dial = Arc::new(dial);
                let carries = (sharing.clone(), shared.starting(&dial, &socket.path), refusals.clone());
                links.spawn(keep_linked(dial, store.clone(), membership.clone(), carries));
            }
            Some((made, unwanted)) = made.recv() => {
                let dial = Arc::new(Program {
                    program: made.provider.clone(),
                    place: made.place.clone(),
                });
                let carries = (sharing.clone(), shared.starting(&dial, &socket.path), refusals.clone());
                let linked = keep_linked(dial.clone(), store.clone(), membership.clone(), carries);
                links.spawn(keep_made(dial, made, shared.book.clone(), unwanted, linked));
            }
            Some(()) = held.recv() => {
                shared.read_sessions();
                shared.keep_awake(&mut awake, power.as_ref());
            }
            // A park's marker says nothing of the session itself, so only
            // how it stands, which records read from the markers, moves.
            Some(()) = parks.recv() => shared.said.sessions.send_modify(|said| *said += 1),
            // A holder that ended without letting its session go says
            // nothing, so what is held is looked at each minute too.
            _ = leaving.tick() => {
                let here = shared.measure();
                power = shared.keep_measured(&here);
                tell_hosted(&here);
                shared.hand_over(&here);
                shared.keep_awake(&mut awake, power.as_ref());
                shared.cross_waiting();
            }
            change = changes.changes.recv() => match change {
                Some(Change::Message(id)) => shared.read_message(&id),
                Some(Change::Presence) => {
                    shared.read_sessions();
                    // A held session going idle, or busy, may let it sleep.
                    shared.keep_awake(&mut awake, power.as_ref());
                }
                Some(Change::Rescan) => {
                    shared.read_store();
                    shared.said.machines.send_modify(|said| *said += 1);
                }
                Some(Change::Machine(_)) => shared.said.machines.send_modify(|said| *said += 1),
                None => break Err(io::Error::other("the store stopped telling its changes")),
            },
        }
    };
    // A link stopped mid-step can't say its sessions are out of reach, so
    // once every one has stopped and given up its claim, the node says so
    // for them.
    links.shutdown().await;
    store::out_of_reach_of_unclaimed(&store)?;
    drop(socket);
    result
}

/// Keeps a link up for as long as the node runs, dialling again after each
/// drop, carrying the quests shared and starts. A link whose way in turned
/// this machine away waits until its refusal is withdrawn instead, logging
/// it once.
async fn keep_linked(
    dial: Arc<impl Dial>,
    store: impl Store,
    membership: impl Membership,
    (shares, starting, refusals): (Option<Places>, Starting, Refusals),
) {
    let words = dial.there(&[]);
    if refusals.stands(&words) {
        eprintln!(
            "{} turned this machine away before, so it isn't dialled {}",
            dial.describe(),
            until_withdrawn(&refusals, &words),
        );
        refusals.withdrawn(&words).await;
    }
    let mut wait = REDIAL_FIRST;
    loop {
        let started = tokio::time::Instant::now();
        let (ended, refused) = match dial.dial(&membership.hello().realm).await {
            Ok(mut connection) => {
                let refused = connection.refused.take();
                let starting = Some(starting.clone());
                let ended = link::run(
                    store.clone(),
                    &membership,
                    connection,
                    shares.clone(),
                    starting,
                )
                .await;
                (ended, said_why(refused).await)
            }
            Err(error) => (Err(error), None),
        };
        if let Some(why) = refused {
            eprintln!(
                "{} turned this machine away ({why}), so it isn't dialled again {}",
                dial.describe(),
                until_withdrawn(&refusals, &words),
            );
            if let Err(error) = refusals.remember(&words, &why) {
                eprintln!("its refusal isn't remembered past this node: {error}");
            }
            refusals.withdrawn(&words).await;
            wait = REDIAL_FIRST;
            continue;
        }
        if let Err(error) = refusals.forget(&words) {
            eprintln!("a refusal of {} wasn't forgotten: {error}", dial.describe());
        }
        match ended {
            Ok(link::Ending::HandedOver) => match dial.release().await {
                Some(Ok(())) => {
                    eprintln!(
                        "the machine over {} handed over and was released",
                        dial.describe()
                    );
                    return;
                }
                Some(Err(error)) => eprintln!(
                    "the machine over {} handed over, but wasn't released: {error}",
                    dial.describe()
                ),
                None => eprintln!(
                    "the machine over {} handed over, but nothing here releases it",
                    dial.describe()
                ),
            },
            Ok(link::Ending::Left) => eprintln!("the link over {} ended", dial.describe()),
            Err(error) => eprintln!("the link over {} failed: {error}", dial.describe()),
        }
        // A link that stood a while starts the waits over.
        if started.elapsed() > REDIAL_MOST {
            wait = REDIAL_FIRST;
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(REDIAL_MOST);
    }
}

/// Keeps a machine made from an offer `linked` until it is done with:
/// until its work has handed over and it was released, it isn't wanted
/// once made, or its time is up, when it is released. Keeps in the book
/// that it was let go, and why.
async fn keep_made(
    dial: Arc<Program>,
    made: Made,
    book: Book,
    unwanted: Arc<Notify>,
    linked: impl Future<Output = ()>,
) {
    let left = jiff::Timestamp::now().duration_until(made.until);
    // A link lets its machine go itself once its work has handed over.
    let release = tokio::select! {
        () = linked => None,
        () = unwanted.notified() => Some("nothing started on it once it was made"),
        () = tokio::time::sleep(Duration::try_from(left).unwrap_or_default()) => {
            Some("its time was up")
        }
    };
    let why = match release {
        None => "its work handed over".to_owned(),
        Some(why) => match dial.release().await {
            Some(Ok(())) => why.to_owned(),
            Some(Err(error)) => {
                format!("{why}, but it wasn't released, so it ends at its time: {error}")
            }
            None => format!("{why}, but nothing releases it, so it ends at its time"),
        },
    };
    eprintln!("{} was let go: {why}", made.machine);
    if let Err(error) = book.released(&made, &why) {
        eprintln!(
            "{} was let go, but the book doesn't say so: {error}",
            made.machine
        );
    }
}

/// How long a machine made of `offer` for `work` is to live: past the
/// work's end by the time its sessions are given to hand over, though no
/// longer than the offer lives; or as long as the offer lives, where the
/// work doesn't say.
///
/// # Errors
///
/// When neither says, since no machine is made without an end.
fn lasting(work: &Work, offer: &Offer) -> Result<Duration, String> {
    let wanted = work
        .lasts
        .and_then(|lasts| Duration::try_from(lasts + handover::HANDOVER).ok());
    let offered = offer.lasts.map(Duration::from_secs);
    match (wanted, offered) {
        (Some(wanted), Some(offered)) => Ok(wanted.min(offered)),
        (Some(one), None) | (None, Some(one)) => Ok(one),
        (None, None) => Err(format!(
            "{} from {} would be made with no end, as neither it nor the work says how long \
             it lives; say how long the work lasts",
            offer.shape, offer.provider
        )),
    }
}

/// Why nothing was made for work no machine of the realm takes, as
/// `misses` say: each offer's reasons, as `offered` says, and each
/// provider that couldn't say what it offers.
fn nothing_offered(
    misses: &[Miss],
    offered: &[Miss],
    unanswered: &BTreeMap<String, String>,
) -> String {
    let mut why = format!("nowhere: {}", placement::said(misses));
    if offered.is_empty() && unanswered.is_empty() {
        why.push_str("; and no provider offers a machine");
    }
    if !offered.is_empty() {
        why.push_str("; and no offer would do: ");
        why.push_str(&placement::said(offered));
    }
    for (program, unsaid) in unanswered {
        // Writing to a string can't fail.
        let _ = write!(why, "; {program} couldn't say what it offers: {unsaid}");
    }
    why
}

/// Why a way in that has ended turned this machine away, if it did.
async fn said_why(refused: Option<tokio::task::JoinHandle<Option<String>>>) -> Option<String> {
    // The link is over and its process gone, so what it said is all but
    // read; a way in that hangs on to its output is taken as not refusing.
    tokio::time::timeout(SAID_WITHIN, refused?)
        .await
        .ok()?
        .ok()?
}

/// What has a refused link dialled again.
fn until_withdrawn(refusals: &Refusals, words: &[String]) -> String {
    refusals.record(words).map_or_else(
        || "until the node starts again".to_owned(),
        |record| format!("until {} is removed", record.display()),
    )
}

/// What the node's loop keeps of what it was given, apart from what it
/// shares with every client.
struct Kept {
    sharing: Option<Places>,
    refusals: Refusals,
    awake: Awake,
}

/// What the node's loop and every client share.
struct Shared<S> {
    store: S,
    machine: String,
    realm: String,
    /// The realm's folder of held sessions on this machine.
    held: PathBuf,
    /// Where the harness marks each session it parks here.
    parked: PathBuf,
    /// The boot this machine runs in, which says which sessions ended
    /// this boot.
    boot: Arc<standing::Boot>,
    /// This machine's sessions' steer inboxes.
    steers: PathBuf,
    /// The socket its machine's updater takes asks on.
    updater: PathBuf,
    starter: Option<Arc<Starter>>,
    /// The links this node asks other machines' starts through.
    asks: Asks,
    inner: Arc<Mutex<Inner>>,
    /// Each time a kind the daemon interface serves may have changed.
    said: Arc<Said>,
    /// The files it serves as kinds of their own.
    served: Arc<Served>,
    /// The providers' programs asked what machines they could make.
    providers: Arc<[String]>,
    /// Where it keeps each machine it makes and lets go.
    book: Book,
    /// The node's loop, which links each machine made, and lets it go
    /// when told it isn't wanted.
    to_dial: mpsc::UnboundedSender<(Made, Arc<Notify>)>,
    /// The realms its messages may go to and come from.
    flows: Arc<Flows>,
    /// This machine as its last minute's measure found it, which the
    /// daemon interface says beside the others rather than measuring it
    /// again for each reader, since a measure reads every part.
    measured: Arc<Mutex<Option<facts::Machine>>>,
}

/// What changes each time the node's sessions or the realm's machines may
/// have, for the daemon interface's watches.
struct Said {
    sessions: watch::Sender<u64>,
    machines: watch::Sender<u64>,
    messages: watch::Sender<u64>,
}

impl Default for Said {
    fn default() -> Self {
        Self {
            sessions: watch::Sender::new(0),
            machines: watch::Sender::new(0),
            messages: watch::Sender::new(0),
        }
    }
}

impl<S: Clone> Clone for Shared<S> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            machine: self.machine.clone(),
            realm: self.realm.clone(),
            held: self.held.clone(),
            parked: self.parked.clone(),
            boot: self.boot.clone(),
            steers: self.steers.clone(),
            updater: self.updater.clone(),
            starter: self.starter.clone(),
            asks: self.asks.clone(),
            inner: self.inner.clone(),
            said: self.said.clone(),
            served: self.served.clone(),
            providers: self.providers.clone(),
            book: self.book.clone(),
            to_dial: self.to_dial.clone(),
            flows: self.flows.clone(),
            measured: self.measured.clone(),
        }
    }
}

/// The node in the daemon interface: the realm's sessions, wherever they
/// run, and its other machines, as `Sessions` and `Machines` answer, and
/// each group's latest messages, as `Messages` does.
impl<S: Store + Clone + Send + Sync + 'static> daemon::Holds for Shared<S> {
    fn kinds(&self) -> BTreeMap<String, Vec<&'static str>> {
        // A served file's fields are its program's, so none is marked live.
        let served = self
            .served
            .kinds()
            .map(|kind| (kind.to_owned(), Vec::new()));
        let [sessions, machines, messages] = HOLDS.map(str::to_owned);
        BTreeMap::from([
            (
                sessions,
                // A session's busy, worksOn and paused are its presence's.
                vec![
                    "state",
                    "step",
                    "title",
                    "wants",
                    "landings",
                    "working_on",
                    "presence",
                    "standing",
                    "told",
                ],
            ),
            (
                machines,
                vec![
                    "state",
                    "measured_at",
                    "power",
                    "room",
                    "reached",
                    "disk_band",
                ],
            ),
            // A message is said once and never changes.
            (messages, Vec::new()),
        ])
        .into_iter()
        .chain(served)
        .collect()
    }

    fn records(&self, kind: &str) -> Option<BTreeMap<String, serde_json::Value>> {
        match kind {
            "session" => {
                let parked = standing::parked(&self.parked);
                let sessions = self.lock().state.sessions();
                Some(
                    sessions
                        .into_iter()
                        .map(|session| self.session_record(kind, &session, &parked))
                        .collect(),
                )
            }
            "machine" => Some(
                self.known_machines()
                    .into_iter()
                    .map(|machine| {
                        let heard = millis_at(&machine.measured_at);
                        let name = machine.name.clone();
                        let mut fields = json_of(&machine);
                        if let Some(fields) = fields.as_object_mut() {
                            fields.insert("here".to_owned(), (name == self.machine).into());
                        }
                        let record = daemon::record(kind, &name, heard, fields);
                        (daemon::key(kind, &name), record)
                    })
                    .collect(),
            ),
            "message" => Some(
                self.lock()
                    .state
                    .latest_by_group(MESSAGES_EACH)
                    .into_iter()
                    .map(|message| {
                        let heard = millis_at(&message.at);
                        let id = message.id.clone();
                        let record = daemon::record(kind, &id, heard, json_of(&message));
                        (daemon::key(kind, &id), record)
                    })
                    .collect(),
            ),
            _ => self.served.records(kind),
        }
    }

    fn told(&self, kind: &str) -> Option<watch::Receiver<u64>> {
        match kind {
            "session" => Some(self.said.sessions.subscribe()),
            "machine" => Some(self.said.machines.subscribe()),
            "message" => Some(self.said.messages.subscribe()),
            _ => self.served.told(kind),
        }
    }

    fn health(&self) -> serde_json::Map<String, serde_json::Value> {
        let sources: Vec<serde_json::Value> = self
            .machines()
            .into_iter()
            .filter(|machine| machine.name != self.machine)
            .map(|machine| match machine.state {
                Readiness::Ready => {
                    serde_json::json!({"name": machine.name, "state": "answering"})
                }
                Readiness::Unreachable => serde_json::json!({
                    "name": machine.name,
                    "state": "refused",
                    "why": format!("no link reaches it; it last said itself at {}", machine.measured_at),
                }),
            })
            .chain(self.served.health())
            .collect();
        serde_json::Map::from_iter([
            ("version".to_owned(), daemon::VERSION.into()),
            ("domain".to_owned(), "node".into()),
            ("state".to_owned(), "answering".into()),
            ("sources".to_owned(), sources.into()),
        ])
    }
}

/// The node steering a session for a client of the daemon interface, as
/// it does for `Steer`, here or through the link reaching its machine.
impl<S: Store + Clone + Send + Sync + 'static> daemon::Steers for Shared<S> {
    async fn steer(&self, address: &str, said: &serde_json::Value) -> Result<String, String> {
        match Shared::steer(self, address, said).await? {
            Response::Steered { machine } => Ok(machine),
            other => Err(format!(
                "its machine answered {other:?}, not that it was steered"
            )),
        }
    }
}

/// `value` as JSON, or null for what can't be, which a node's own records
/// never are.
fn json_of(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_default()
}

/// An RFC 3339 time in milliseconds since the epoch, or now where it
/// doesn't read as one.
fn millis_at(time: &str) -> i64 {
    time.parse::<jiff::Timestamp>()
        .unwrap_or_else(|_| jiff::Timestamp::now())
        .as_millisecond()
}

#[derive(Default)]
struct Inner {
    state: State,
    watches: HashMap<u64, mpsc::Sender<Event>>,
    routes: Routes<u64>,
    next_watch: u64,
    /// The connection each present process holds now, by its id, so a
    /// connection another replaced says nothing when it closes.
    present: HashMap<i32, u64>,
    next_present: u64,
    /// The sessions told this machine leaves.
    told: Told,
    /// The sessions told this machine runs low on its battery.
    sleeping: Sleeping,
    /// The messages waiting to cross into another realm by a flow of this
    /// one's, by id, which no one here hears.
    waiting: BTreeMap<String, Message>,
    /// The ids of the messages handed across, or being handed, or refused
    /// for good, which aren't handed across again.
    crossed: HashSet<String>,
}

/// Watches a folder of records, of what is held or what is parked, or,
/// when it can't be watched, says so and leaves `what` it holds read only
/// with presence.
fn watch(
    folder: &Path,
    what: &str,
) -> (
    Option<notify::RecommendedWatcher>,
    mpsc::UnboundedReceiver<()>,
) {
    match hold::watch(folder) {
        Ok((watcher, changed)) => (Some(watcher), changed),
        Err(error) => {
            eprintln!("{what} isn't watched, so is read only with presence: {error}");
            (None, mpsc::unbounded_channel().1)
        }
    }
}

impl Inner {
    /// What a node of `realm` holds as it starts: nothing, but the crossings
    /// its log says were judged before it restarted, which aren't handed
    /// across again.
    fn after(flows: &Flows, realm: &str) -> Self {
        Self {
            crossed: flow::settled(&flows.log, realm),
            ..Self::default()
        }
    }
}

impl<S: Store> Shared<S> {
    /// What a node of `hello`'s machine and realm shares, from what it was
    /// `given`, telling `to_dial` of each machine it makes, with what its
    /// loop keeps apart.
    fn of(
        store: S,
        hello: Hello,
        given: Given,
        to_dial: mpsc::UnboundedSender<(Made, Arc<Notify>)>,
    ) -> (Self, Kept) {
        let Given {
            sharing,
            starter,
            refusals,
            steers,
            awake,
            updater,
            serves,
            providers,
            book,
            flows,
            parked,
        } = given;
        let shared = Self {
            served: Arc::new(Served::start(serves)),
            providers: providers.into(),
            book,
            to_dial,
            held: place::held(&hello.realm),
            parked,
            boot: Arc::new(standing::Boot::this()),
            steers,
            updater,
            store,
            inner: Arc::new(Mutex::new(Inner::after(&flows, &hello.realm))),
            machine: hello.machine,
            realm: hello.realm,
            starter: starter.map(Arc::new),
            asks: Asks::default(),
            said: Arc::default(),
            flows: Arc::new(flows),
            measured: Arc::default(),
        };
        let kept = Kept {
            sharing,
            refusals,
            awake,
        };
        (shared, kept)
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock leaves the state as it was before
        // the change that panicked, so carrying on with it is sound.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reads the messages the store holds that the node doesn't yet, and
    /// every session. A message is written once by its id and never
    /// changes, so one already held isn't read again: a rescan after a
    /// watch overflowed lists the messages rather than reading them all,
    /// whose every open a Linux watch hears, which would overflow it
    /// again. The node holds far more messages than the mesh keeps, its
    /// week, so none it let go is read again.
    fn read_store(&self) {
        let ids = self.store.message_ids(0);
        let unread: Vec<String> = {
            let inner = self.lock();
            ids.into_iter()
                .filter(|id| !inner.state.holds_message(id))
                .collect()
        };
        let (waiting, messages): (Vec<Message>, Vec<Message>) = unread
            .iter()
            .filter_map(|id| mesh::message(&self.store.message(id)?))
            .partition(|message| message.for_realm.is_some());
        self.lock().state.add_messages(messages);
        self.wait_to_cross(waiting);
        self.said.messages.send_modify(|said| *said += 1);
        self.read_sessions();
    }

    /// Judges every session again and sends what changed, with what is
    /// held on this machine, which the node also watches.
    fn read_sessions(&self) {
        let held = hold::held(&self.held);
        let sessions = self
            .store
            .presences()
            .iter()
            .filter_map(|presence| {
                let mut session = session_held(presence, &self.machine, &held)?;
                session.attach = self.attach(&session);
                Some(session)
            })
            .collect();
        let mut inner = self.lock();
        let events = inner.state.set_sessions(sessions);
        if !events.is_empty() {
            self.said.sessions.send_modify(|said| *said += 1);
        }
        for event in events {
            inner.send_all(&event);
        }
    }

    /// A session as the daemon interface holds it: its fields, whether it
    /// runs here, how it stands (`standing`) where it's shown at all, and
    /// the digest of what a shut launcher reads of it (`told`), given the
    /// sessions `parked` here.
    fn session_record(
        &self,
        kind: &str,
        session: &Session,
        parked: &HashSet<String>,
    ) -> (String, serde_json::Value) {
        let heard = millis_at(&session.at);
        let mut fields = json_of(session);
        if let Some(fields) = fields.as_object_mut() {
            let here = session.machine == self.machine;
            fields.insert("here".to_owned(), here.into());
            if let Some(stands) = standing::standing(session, here, parked, &self.boot) {
                fields.insert("standing".to_owned(), stands.into());
            }
        }
        let told = standing::told(&fields);
        if let Some(fields) = fields.as_object_mut() {
            fields.insert("told".to_owned(), told.into());
        }
        let record = daemon::record(kind, &session.id, heard, fields);
        (daemon::key(kind, &session.id), record)
    }

    /// Reads a message that landed and sends it to those who hear it.
    fn read_message(&self, id: &str) {
        let Some(message) = self.store.message(id).as_ref().and_then(mesh::message) else {
            return;
        };
        if message.for_realm.is_some() {
            self.wait_to_cross(vec![message]);
            return;
        }
        let mut inner = self.lock();
        if let Some(message) = inner.state.add_message(message) {
            inner.send_message(message);
            self.said.messages.send_modify(|said| *said += 1);
        }
    }

    /// Sends a message on the mesh; the store's change brings it back to
    /// every watch, as any session's message comes.
    fn send(&self, draft: &Draft) -> Result<Message, String> {
        if draft
            .realm
            .as_ref()
            .is_some_and(|realm| *realm != self.realm)
        {
            return Err("a message for another realm is sent across by a flow".to_owned());
        }
        if draft.groups.is_empty() {
            return Err("a message needs at least one group".to_owned());
        }
        store::folders::safe(&draft.from).map_err(|error| format!("from: {error}"))?;
        let (id, body) = mesh::written(draft, std::process::id());
        self.store
            .put_message(&id, &body)
            .map_err(|error| format!("the store refused the message: {error}"))?;
        mesh::message(&body).ok_or_else(|| "the message didn't read back".to_owned())
    }

    /// The words that join `session` from a terminal here, when it is held,
    /// or within a pi host, here or on a machine a link of this node's
    /// reaches. A hosted session here is viewed by the words its presence
    /// says, when it says any, so a window opened on it carries on as plain
    /// pi once its host releases it; one elsewhere by its host's attach,
    /// since those words name this machine's paths.
    fn attach(&self, session: &Session) -> Option<Vec<String>> {
        let words = match (&session.held, &session.host, &session.conversation) {
            (Some(held), _, _) => start::attach_here(&self.realm, held),
            (None, Some(host), Some(conversation)) => {
                let view =
                    mesh::viewed_by(&session.presence).filter(|_| session.machine == self.machine);
                if view.is_some() {
                    return view;
                }
                hosted_attach::attach_here(&self.realm, host, conversation)
            }
            _ => return None,
        };
        if session.machine == self.machine {
            return Some(words);
        }
        let there = self.asks.there(&session.machine)?;
        Some(there(&words))
    }

    /// This machine as it is now, as its realm hears it: a handful of small
    /// files read, or one sysctl on a Mac, quick enough to answer in place.
    fn measure(&self) -> facts::Machine {
        facts::measure_in(&self.machine, &self.realm, &facts::Sources::here())
    }

    /// Keeps this machine awake while what it holds is at work and it has
    /// the charge, as `power` last read, and lets it sleep once it isn't
    /// or hasn't.
    fn keep_awake(&self, awake: &mut Awake, power: Option<&facts::Power>) {
        let held = hold::held(&self.held);
        let sessions = self.lock().state.sessions();
        let now = jiff::Timestamp::now().as_second();
        let queued = place::queue()
            .map(|queue| room::queued_by(&queue, now))
            .unwrap_or_default();
        let holds = awake::at_work(&self.machine, &held, &sessions, &queued);
        awake.keep(awake::wanted(holds, power));
    }

    /// Tells each session running here that hasn't been told yet, once this
    /// machine leaves soon (`handover`), through its steer inbox.
    fn hand_over(&self, here: &facts::Machine) {
        let leaves = here
            .declared
            .as_ref()
            .and_then(|declared| declared.leaves_at);
        let due = {
            let inner = self.lock();
            inner.told.due(
                &self.machine,
                leaves,
                jiff::Timestamp::now(),
                &inner.state.sessions(),
            )
        };
        for steer in due {
            match steer.deliver(&self.steers) {
                Ok(_) => self.lock().told.told(&steer.address),
                Err(error) => {
                    eprintln!("{} wasn't told this machine leaves: {error}", steer.address);
                }
            }
        }
        self.tell_sleeping(here);
    }

    /// Tells each session running here that hasn't been told yet, while
    /// this machine runs low on its battery and so is let sleep
    /// (`awake::low`), through its steer inbox.
    fn tell_sleeping(&self, here: &facts::Machine) {
        let low = here.power.as_ref().is_some_and(awake::low);
        let due = {
            let mut inner = self.lock();
            if !low {
                inner.sleeping.charged();
            }
            inner.sleeping.due(
                &self.machine,
                low,
                jiff::Timestamp::now(),
                &inner.state.sessions(),
            )
        };
        for steer in due {
            match steer.deliver(&self.steers) {
                Ok(_) => self.lock().sleeping.told(&steer.address),
                Err(error) => {
                    eprintln!(
                        "{} wasn't told this machine runs low: {error}",
                        steer.address
                    );
                }
            }
        }
    }

    /// Keeps `here`, this machine's measure, for the daemon interface,
    /// tells its readers the machines changed, and answers what it runs on.
    fn keep_measured(&self, here: &facts::Machine) -> Option<facts::Power> {
        *self
            .measured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(here.clone());
        self.said.machines.send_modify(|said| *said += 1);
        here.power
    }

    /// Every machine of the realm as last known: this one as its minute's
    /// measure found it, once there has been one, then each other one as
    /// its links last carried it.
    fn known_machines(&self) -> Vec<facts::Machine> {
        let here = self
            .measured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        here.into_iter().chain(self.others()).collect()
    }

    /// Each other machine of the realm that hasn't left, as its links last
    /// carried it.
    fn others(&self) -> Vec<facts::Machine> {
        let now = jiff::Timestamp::now();
        self.store
            .machines()
            .into_iter()
            .filter(|one| {
                one.name != self.machine
                    && !one.declared.as_ref().is_some_and(|said| said.has_left(now))
            })
            .collect()
    }

    /// Every machine of the realm: this one measured now, then each other
    /// as its links last carried it, but for those that said they would
    /// leave and have.
    fn machines(&self) -> Vec<facts::Machine> {
        let mut all = vec![self.measure()];
        all.extend(self.others());
        all
    }

    fn answer(&self, request: Request) -> Reply {
        match request {
            Request::Sessions => Ok(Response::Sessions(self.lock().state.sessions())),
            Request::Machine => Ok(Response::Machine(Box::new(self.measure()))),
            Request::Machines => Ok(Response::Machines(self.machines())),
            Request::Place(work) => Ok(Response::Placement(placement::place(
                &work,
                &self.machines(),
            ))),
            Request::Messages { groups, limit } => {
                Ok(Response::Messages(self.lock().state.recent(&groups, limit)))
            }
            Request::Send(draft) => self.send(&draft).map(Response::Sent),
            Request::Flows => Ok(Response::Flows {
                to: self.flows.to.iter().cloned().collect(),
            }),
            Request::Cross(_) => Err("a crossing is answered by `take`".to_owned()),
            Request::Watch { .. } => Err("a watch is answered by `watch`".to_owned()),
            Request::Present => Err("being present is answered by `be_present`".to_owned()),
            Request::Start(_) => Err("a start is answered by `start`".to_owned()),
            Request::Offers => Err("offers are answered by `offers`".to_owned()),
            Request::Steer { .. } => Err("a steer is answered by `steer`".to_owned()),
            Request::Update { .. } => Err("an update is answered by `update`".to_owned()),
            Request::Carry { .. } => Err("carrying is answered by `carried`".to_owned()),
        }
    }

    /// What a link over `dial` carries starts with: this node's `node`
    /// socket, its held sessions and steer inboxes, and the way to ask its
    /// machine's node to start one there.
    fn starting<D: Dial + 'static>(&self, dial: &Arc<D>, node: &Path) -> Starting {
        Starting {
            node: node.to_owned(),
            held: self.held.clone(),
            asking: Some(Asking::Kept(self.asks.clone(), {
                let dial = dial.clone();
                Arc::new(move |words: &[String]| dial.there(words))
            })),
            steers: self.steers.clone(),
        }
    }

    /// What each provider's program offers, asked now, all at once.
    async fn offers(&self) -> Offers {
        let mut asking = JoinSet::new();
        for (order, program) in self.providers.iter().cloned().enumerate() {
            asking.spawn(async move {
                let said = offer::asked(&program).await;
                (order, program, said)
            });
        }
        let mut asked = asking.join_all().await;
        asked.sort_by_key(|(order, _, _)| *order);
        let mut offers = Offers::default();
        for (_, program, said) in asked {
            match said {
                Ok(each) => offers.offers.extend(each),
                Err(why) => {
                    offers.unanswered.insert(program, why);
                }
            }
        }
        offers
    }

    /// Updates `machine` to its configuration's main: this one through its
    /// updater, saying each step in the node's log, and another through
    /// the link reaching it. Answers how the update ended.
    async fn update(&self, machine: &str) -> Reply {
        if machine != self.machine {
            return self.asks.update(machine).await;
        }
        let ending = crate::update::ask(&self.updater, |step| {
            eprintln!("updating {machine}: {step}");
        })
        .await?;
        eprintln!("updated {machine}: {ending}");
        Ok(Response::Updated(ending))
    }

    /// Puts what the person says into the inbox of the session going by
    /// `address`, on the one machine it runs on: here, or through the link
    /// reaching that machine. A parked session counts as running there,
    /// since it reads its inbox as it wakes, and the person's words are
    /// left for it before the node there wakes it. A talk's file is opened here first,
    /// so the replies carried back land where the person's launcher reads
    /// them.
    async fn steer(&self, address: &str, said: &serde_json::Value) -> Reply {
        let steer = Steer::new(address, said)?;
        let address = steer.address.clone();
        let sessions: Vec<Session> = self
            .lock()
            .state
            .sessions()
            .into_iter()
            .filter(|session| session.address.eq_ignore_ascii_case(&address))
            .collect();
        let mut running: Vec<String> = sessions
            .iter()
            .filter(|session| {
                matches!(session.state, SessionState::Busy | SessionState::Idle)
                    || (session.state == SessionState::Ended
                        && session
                            .presence
                            .get("state")
                            .and_then(serde_json::Value::as_str)
                            == Some("parked"))
            })
            .map(|session| session.machine.clone())
            .collect();
        running.sort();
        running.dedup();
        let machine = match running.as_slice() {
            [one] => one.clone(),
            [] => {
                return Err(sessions
                    .iter()
                    .find(|session| session.state == SessionState::Unreachable)
                    .map_or_else(
                        || format!("no session goes by {address} now"),
                        |session| {
                            format!(
                                "{} is out of reach, so {address} can't be told now",
                                session.machine
                            )
                        },
                    ));
            }
            many => {
                return Err(format!(
                    "{address} runs on {}, so it isn't told twice",
                    many.join(" and ")
                ));
            }
        };
        if let Some(talk) = steer.talk() {
            steer::open_talk(&self.steers, &address, talk)
                .map_err(|error| format!("its talk didn't open here: {error}"))?;
        }
        if machine != self.machine {
            // The far end of a link puts a steer in the inbox there but
            // wakes nothing, so a wake carried would only be left unread.
            if steer.only_wakes() {
                return Err(format!(
                    "{address} runs on {machine}, and a wake reaches only here"
                ));
            }
            return self.asks.steer(&machine, steer).await;
        }
        self.steer_here(&steer, &sessions)?;
        Ok(Response::Steered { machine })
    }

    /// Puts `steer` in its session's inbox here, unless it only wakes,
    /// then wakes the session where it is parked this boot: a wake that
    /// can't is refused, but a steer told and not woken is read at the
    /// session's next wake, whatever wakes it.
    fn steer_here(&self, steer: &Steer, sessions: &[Session]) -> Result<(), String> {
        let address = &steer.address;
        if !steer.only_wakes() {
            steer
                .deliver(&self.steers)
                .map_err(|error| format!("its inbox refused it: {error}"))?;
        }
        let boot = store::folders::boot_id();
        let waking = sessions
            .iter()
            .filter(|session| session.machine == self.machine)
            .find_map(|session| steer::wake_for(session, boot.as_deref(), &self.held));
        match (waking, steer.only_wakes()) {
            (Some(file), true) => steer::wake(&file, address)
                .map_err(|error| format!("{address} didn't wake: {error}")),
            (None, true) => Err(format!(
                "{address} isn't parked with a holder to wake it this boot"
            )),
            (Some(file), false) => {
                if let Err(error) = steer::wake(&file, address) {
                    eprintln!("{address} was told, but not woken: {error}");
                }
                Ok(())
            }
            (None, false) => Ok(()),
        }
    }

    /// Counts `pid` present through a new connection, which replaces any
    /// it held before, answering which connection that is.
    fn present(&self, pid: i32) -> u64 {
        let connection = {
            let mut inner = self.lock();
            inner.next_present += 1;
            let connection = inner.next_present;
            inner.present.insert(pid, connection);
            connection
        };
        self.store.present(pid);
        connection
    }

    /// Says `pid` has ended as `connection` closed, unless a later
    /// connection of its own has taken that one's place.
    fn gone(&self, pid: i32, connection: u64) {
        {
            let mut inner = self.lock();
            if inner.present.get(&pid) != Some(&connection) {
                return;
            }
            inner.present.remove(&pid);
        }
        self.store.ended(pid);
    }

    /// Starts a program held on the machine placement chooses, answering
    /// where and how to join it, or why it didn't start. Where none of the
    /// realm's machines takes it and it may cost money, on a machine made
    /// for it.
    async fn start(&self, asked: Start) -> Reply {
        if asked.command.is_empty() {
            return Err("nothing to start: name a program".to_owned());
        }
        match placement::place(&asked.work, &self.machines()) {
            Placement::Placed { machine, .. } => self.start_on(machine, asked).await,
            Placement::Pending { misses } | Placement::Refused { misses } if asked.may_cost => {
                self.start_on_made(asked, &misses).await
            }
            Placement::Pending { misses } => Err(format!(
                "nowhere now, though a machine out of reach would do: {}",
                placement::said(&misses)
            )),
            Placement::Refused { misses } => Err(format!("nowhere: {}", placement::said(&misses))),
        }
    }

    /// Makes a machine for `asked`, which `misses` say no machine of the
    /// realm takes, from the cheapest offer that would, and starts it there
    /// once it links. A machine made that nothing starts on is let go.
    async fn start_on_made(&self, asked: Start, misses: &[Miss]) -> Reply {
        let offers = self.offers().await;
        let offer = placement::to_make(&asked.work, &offers.offers)
            .map_err(|offered| nothing_offered(misses, &offered, &offers.unanswered))?;
        let made = made::make(offer, lasting(&asked.work, offer)?).await?;
        if let Err(error) = self.book.made(&made) {
            eprintln!(
                "{} was made, but the book doesn't say so: {error}",
                made.machine
            );
        }
        let unwanted = Arc::new(Notify::new());
        // The loop runs for as long as the node answers anything.
        let _ = self.to_dial.send((made.clone(), unwanted.clone()));
        let started = self.start_once_linked(asked, &made).await;
        let used = || {
            let inner = self.lock();
            inner
                .state
                .sessions()
                .iter()
                .any(|one| one.machine == made.machine)
        };
        if started.is_err() && !used() {
            unwanted.notify_one();
        }
        started
    }

    /// Starts `asked` on `made` once it has linked and its own facts, not
    /// the offer's, say it takes the work.
    async fn start_once_linked(&self, asked: Start, made: &Made) -> Reply {
        let mut changed = self.said.machines.subscribe();
        let ready = |all: &[facts::Machine]| {
            all.iter()
                .any(|one| one.name == made.machine && one.state == Readiness::Ready)
        };
        let linked = async {
            while !ready(&self.machines()) {
                // The node holds the sender for as long as it answers.
                let _ = changed.changed().await;
            }
        };
        tokio::time::timeout(LINKED_WITHIN, linked)
            .await
            .map_err(|_| {
                format!(
                    "{} was made, but didn't link within {}s, so it is let go",
                    made.machine,
                    LINKED_WITHIN.as_secs()
                )
            })?;
        let mut there = asked.work.clone();
        there.needs.push(Need::Machine(made.machine.clone()));
        match placement::place(&there, &self.machines()) {
            Placement::Placed { machine, .. } => self.start_on(machine, asked).await,
            Placement::Pending { misses } | Placement::Refused { misses } => {
                let its: Vec<Miss> = misses
                    .into_iter()
                    .filter(|miss| miss.machine == made.machine)
                    .collect();
                Err(format!(
                    "{} was made, but isn't what was offered, so it is let go: {}",
                    made.machine,
                    placement::said(&its)
                ))
            }
        }
    }

    /// Starts `asked` on `machine`: this one by its own starter, another
    /// through the link reaching it.
    async fn start_on(&self, machine: String, asked: Start) -> Reply {
        if machine != self.machine {
            return self.asks.ask(&machine, asked).await;
        }
        let Some(starter) = self.starter.clone() else {
            return Err(format!("{machine}'s node starts no sessions"));
        };
        let Some(home) = std::env::home_dir() else {
            return Err(format!("{machine} has no home to start it in"));
        };
        let folder = start::begins_in(asked.folder.as_deref(), &home);
        let realm = self.realm.clone();
        let command = asked.command;
        let begins = folder.clone();
        // Through the login shell, each start of it: the holder has only
        // the node's environment, where a woken `pi` wouldn't be found.
        let held =
            tokio::task::spawn_blocking(move || starter.start(&realm, &begins, &command, true))
                .await
                .map_err(|error| format!("its start stopped: {error}"))?
                .map_err(|error| format!("it didn't start: {error}"))?;
        Ok(Response::Started(Started {
            folder,
            attach: start::attach_here(&self.realm, &held),
            machine,
            held,
        }))
    }

    /// Starts a watch: its first events are the whole state, taken under
    /// the same lock as every later change, so none is missed or doubled.
    fn watch(&self, groups: Option<&Pattern>, recent: usize) -> (u64, mpsc::Receiver<Event>) {
        let mut inner = self.lock();
        let (sender, receiver) = mpsc::channel(WATCH_BACKLOG);
        let first = [
            Event::SessionsChanged {
                sessions: inner.state.sessions(),
            },
            Event::MessagesChanged {
                messages: groups.map_or_else(Vec::new, |groups| inner.state.recent(groups, recent)),
            },
        ];
        for event in first {
            // The channel is new and holds more than two, so this can't fail.
            let _ = sender.try_send(event);
        }
        let id = inner.next_watch;
        inner.next_watch += 1;
        inner.watches.insert(id, sender);
        if let Some(groups) = groups {
            inner.routes.insert(groups, id);
        }
        (id, receiver)
    }

    fn unwatch(&self, id: u64) {
        let mut inner = self.lock();
        inner.watches.remove(&id);
        inner.routes.remove(&id);
    }
}

impl Inner {
    fn send_message(&mut self, message: Message) {
        let heard = self.routes.listeners(&message.groups);
        let event = Event::Message { message };
        for id in heard {
            self.send(id, &event);
        }
    }

    fn send_all(&mut self, event: &Event) {
        let ids: Vec<u64> = self.watches.keys().copied().collect();
        for id in ids {
            self.send(id, event);
        }
    }

    /// Sends `event` to one watch, letting it go if it has fallen too far
    /// behind or left.
    fn send(&mut self, id: u64, event: &Event) {
        let Some(sender) = self.watches.get(&id) else {
            return;
        };
        if sender.try_send(event.clone()).is_err() {
            self.watches.remove(&id);
            self.routes.remove(&id);
        }
    }
}

/// Serves one client: requests answered a line each, or a watch for as long
/// as it stays.
async fn serve<S: Store>(stream: UnixStream, shared: Shared<S>) {
    // The kernel says which process is at the other end, so no client can
    // say it is present for another.
    let process = stream.peer_cred().ok().and_then(|cred| cred.pid());
    let (reading, mut writing) = stream.into_split();
    let mut lines = BufReader::new(reading).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        // A client opening with the daemon interface's hello speaks it
        // from then on; any other speaks the node's own protocol.
        if let Ok(
            hello @ daemon::Ask {
                what: daemon::What::Hello,
                ..
            },
        ) = daemon::read(&line)
        {
            daemon::speak(Arc::new(shared), hello, lines, writing).await;
            return;
        }
        let request = serde_json::from_str::<Request>(&line)
            .map_err(|error| format!("not a request: {error}"));
        if let Ok(Request::Present) = request {
            be_present(&shared, process, lines, writing).await;
            return;
        }
        if let Ok(Request::Carry { machine }) = request {
            link::carried::to_link(&shared.asks, &machine, lines, writing).await;
            return;
        }
        if let Ok(Request::Watch { groups, recent }) = request {
            if write(&mut writing, &Reply::Ok(Response::Watching))
                .await
                .is_ok()
            {
                let (id, mut events) = shared.watch(groups.as_ref(), recent);
                loop {
                    tokio::select! {
                        event = events.recv() => {
                            let Some(event) = event else { break };
                            let written = tokio::time::timeout(WATCH_PATIENCE, write(&mut writing, &event)).await;
                            if !matches!(written, Ok(Ok(()))) {
                                break;
                            }
                        }
                        // A watch's client says nothing more; anything it
                        // does send, or its hanging up, ends the watch.
                        _ = lines.next_line() => break,
                    }
                }
                shared.unwatch(id);
            }
            return;
        }
        let reply: Answer = match request {
            Ok(Request::Start(asked)) => shared.start(asked).await.into(),
            Ok(Request::Steer { address, steer }) => shared.steer(&address, &steer).await.into(),
            Ok(Request::Update { machine }) => shared.update(&machine).await.into(),
            Ok(Request::Offers) => Answer::Ok(Response::Offers(shared.offers().await)),
            Ok(Request::Send(draft))
                if draft
                    .realm
                    .as_ref()
                    .is_some_and(|realm| *realm != shared.realm) =>
            {
                shared.send_across(draft).await
            }
            Ok(Request::Cross(crossing)) => shared.take(crossing),
            asked => asked.and_then(|request| shared.answer(request)).into(),
        };
        if write(&mut writing, &reply).await.is_err() {
            return;
        }
    }
}

/// Counts `process` present until it hangs up, then says it has ended. A
/// process the kernel won't name is refused, since its end couldn't be told.
async fn be_present<S: Store>(
    shared: &Shared<S>,
    process: Option<i32>,
    mut lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    mut writing: tokio::net::unix::OwnedWriteHalf,
) {
    let Some(pid) = process else {
        let refused = Reply::Err(
            "the node can't tell which process is at the other end, so can't count it present"
                .to_owned(),
        );
        // A client that hung up already hears nothing either way.
        let _ = write(&mut writing, &refused).await;
        return;
    };
    if write(&mut writing, &Reply::Ok(Response::Present))
        .await
        .is_err()
    {
        return;
    }
    let connection = shared.present(pid);
    // A present process says nothing more; its hanging up, or the socket
    // failing, is its end.
    while let Ok(Some(_)) = lines.next_line().await {}
    shared.gone(pid, connection);
}

async fn write(
    writing: &mut tokio::net::unix::OwnedWriteHalf,
    value: &impl serde::Serialize,
) -> io::Result<()> {
    let mut line = serde_json::to_vec(value).map_err(io::Error::other)?;
    line.push(b'\n');
    writing.write_all(&line).await
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::client::Client;
    use crate::group::Group;
    use crate::link::dial::{Connection, Ssh};
    use crate::link::discovery::Listed;
    use crate::link::membership::{Proven, Realm};
    use crate::record;
    use crate::record::Intent;
    use crate::store::memory::Memory;

    /// How long the test waits on the node before failing. Each wait ends
    /// the moment what it waits for happens, so this only bounds a hang:
    /// five seconds failed a dial and a start on a Mac building its system
    /// beside the tests, so it's the link tests' thirty.
    const PATIENCE: Duration = Duration::from_secs(30);

    /// A holder in `dir` that says it holds at once, writing down in the
    /// file it answers beside it what it was given to hold. A child process
    /// puts it in place, since another test's fork holding the file open
    /// for writing would keep it from running.
    fn stand_in_holder(dir: &Path) -> (PathBuf, PathBuf) {
        let given = dir.join("given");
        let text = dir.join("holder.sh");
        let holder = dir.join("holder");
        fs::write(
            &text,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\necho held\n",
                given.display()
            ),
        )
        .expect("the stand-in holder is written");
        let put = std::process::Command::new("install")
            .args(["-m", "755"])
            .arg(&text)
            .arg(&holder)
            .status()
            .expect("install runs");
        assert!(put.success(), "the stand-in holder is put in place");
        (holder, given)
    }

    /// Holds session `0e50` on equilibrius, whose store is `store`, as its
    /// holder there keeps it by `5e551011`, answering the held folder.
    fn hold_a_session(dir: &Path, store: &Memory) -> PathBuf {
        let held = dir.join("equilibrius.held");
        fs::create_dir(&held).expect("the held folder is made");
        let keeps = hold::Held {
            id: "5e551011".to_owned(),
            command: vec!["pi".to_owned()],
            folder: dir.to_owned(),
            holder: std::process::id(),
            pid: 4242,
            parked: false,
            since: "2026-10-29T06:00:00Z".to_owned(),
            windows: Vec::new(),
        };
        let record = serde_json::to_vec(&keeps).expect("a record");
        fs::write(held.join("5e551011.json"), record).expect("the record is written");
        fs::write(held.join("5e551011.sock"), "").expect("its socket stands");
        let presence = json!({
            "session": "0e50",
            "addresses": ["SAK9FZ"],
            "at": "2026-10-29T06:00:00Z",
            "owner": {"pid": 4242},
        });
        store.start_here("0e50", presence, true);
        held
    }

    /// Session `id` once the node at `socket` lists it.
    async fn listed(socket: PathBuf, id: &'static str) -> Session {
        let listed = tokio::task::spawn_blocking(move || {
            let mut client = Client::connect(&socket).expect("the client connects");
            // Nothing tells a client when the link has carried a session,
            // so it asks again every few milliseconds until it has.
            loop {
                let Ok(Ok(Response::Sessions(all))) = client.ask(&Request::Sessions) else {
                    panic!("the node lists its sessions");
                };
                if let Some(one) = all.into_iter().find(|one| one.id == id) {
                    return one;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        tokio::time::timeout(PATIENCE, listed)
            .await
            .expect("the node lists it in time")
            .expect("the client asks")
    }

    /// What a node given only its sessions' `steers` and its `starter` runs
    /// with: nothing shared or served, no updater and no providers.
    fn bare(steers: PathBuf, starter: Option<Starter>) -> Given {
        Given {
            sharing: None,
            starter,
            refusals: Refusals::default(),
            steers,
            awake: Awake::by(None),
            updater: "/nowhere".into(),
            serves: BTreeMap::new(),
            providers: Vec::new(),
            book: Book::at("/nowhere/made.jsonl".into()),
            flows: Flows::default(),
            parked: "/nowhere/parked".into(),
        }
    }

    fn work(needs: Vec<Need>) -> Work {
        Work {
            needs,
            prefers: Vec::new(),
            tolerates: Vec::new(),
            may_wait: false,
            lasts: None,
        }
    }

    /// Runs a node of `machine` in the work realm on `socket`, linked over
    /// `dials`, until `stop` is dropped, its steer inboxes beside the
    /// socket.
    fn node<D: Dial + Clone>(
        store: &Memory,
        machine: &str,
        dials: Vec<D>,
        starter: Option<Starter>,
        socket: &Path,
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<io::Result<()>>,
    ) {
        let membership = Realm {
            name: "work".to_owned(),
            machine: machine.to_owned(),
        };
        let bound = bind(socket).expect("the socket binds");
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let store = store.clone();
        let steers = inboxes(socket);
        let updater = updater_of(socket);
        let serves = BTreeMap::from([("quest".to_owned(), quests_of(socket))]);
        let providers = providers_of(socket);
        let book = Book::at(socket.with_file_name("made.jsonl"));
        let parked = parks_of(socket);
        let node = tokio::spawn(async move {
            let given = Given {
                sharing: None,
                starter,
                refusals: Refusals::default(),
                steers,
                awake: Awake::by(None),
                updater,
                serves,
                providers,
                book,
                flows: Flows::default(),
                parked,
            };
            let stopped = async {
                // A dropped sender stops the node as surely as a sent stop.
                let _ = stopped.await;
            };
            run(store, membership, Listed(dials), given, bound, stopped).await
        });
        (stop, node)
    }

    /// A dial to equilibrius over a pipe, whose far end runs equilibrius's
    /// end of the link on `store`, handing starts to its node at `socket`.
    #[derive(Clone)]
    struct Piped {
        store: Memory,
        socket: PathBuf,
        held: PathBuf,
        /// Whether equilibrius's end knows the far end is optimus, as a
        /// pinned key proves it.
        proven: bool,
        /// Whether equilibrius's end carries what its node asks of
        /// optimus, as the end of a link the other machine dialled does.
        asks_back: bool,
    }

    impl Dial for Piped {
        fn describe(&self) -> String {
            "a pipe to equilibrius".to_owned()
        }

        fn dial(&self, _realm: &str) -> impl Future<Output = io::Result<Connection>> + Send {
            let (near, far) = tokio::io::duplex(1 << 16);
            let store = self.store.clone();
            let node = self.socket.clone();
            let held_here = self.held.clone();
            let steers = inboxes(&self.socket);
            let proven = self.proven;
            let asking = self.asks_back.then_some(Asking::Dialled);
            tokio::spawn(async move {
                let realm = Realm {
                    name: "work".to_owned(),
                    machine: "equilibrius".to_owned(),
                };
                let starting = Starting {
                    node,
                    held: held_here,
                    asking,
                    steers,
                };
                // The test judges the link by what crosses it.
                let _ = if proven {
                    let by_key = Proven {
                        by: realm,
                        far: "optimus".to_owned(),
                    };
                    link::run(store, &by_key, piped(far), None, Some(starting)).await
                } else {
                    link::run(store, &realm, piped(far), None, Some(starting)).await
                };
            });
            std::future::ready(Ok(piped(near)))
        }

        fn there(&self, words: &[String]) -> Vec<String> {
            let mut there = vec!["into-equilibrius".to_owned()];
            there.extend_from_slice(words);
            there
        }
    }

    /// What the node at `socket` answers `request`.
    async fn ask(socket: PathBuf, request: Request) -> Reply {
        let asked = tokio::task::spawn_blocking(move || {
            let mut client = Client::connect(&socket).expect("the client connects");
            client.ask(&request).expect("the node answers")
        });
        tokio::time::timeout(PATIENCE, asked)
            .await
            .expect("the node answers in time")
            .expect("the client asks")
    }

    /// Steers the session held on equilibrius from the node at
    /// `near_socket`, and has its talk answered there, as its session would,
    /// until the answer is carried back to `near_steers`.
    async fn steers_the_session_on_equilibrius(
        near_socket: &Path,
        near_steers: &Path,
        far_steers: &Path,
    ) {
        let asked = Request::Steer {
            address: "sak9fz".to_owned(),
            steer: json!({"text": "how goes it?", "from": "joel", "id": "t1"}),
        };
        assert_eq!(
            ask(near_socket.to_owned(), asked).await,
            Ok(Response::Steered {
                machine: "equilibrius".to_owned()
            }),
            "a session on another machine is steered there"
        );
        let inbox: Vec<_> = fs::read_dir(far_steers.join("SAK9FZ"))
            .expect("equilibrius's inbox for it is made")
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|it| it == "json"))
            .collect();
        assert_eq!(inbox.len(), 1, "the steer is in its inbox there, once");
        let replied = "{\"stage\":\"replied\",\"text\":\"well\"}\n";
        let far_talk = steer::replies(far_steers, "SAK9FZ", "t1").expect("a talk");
        // The harness makes its talks' folder as it writes the first event.
        fs::create_dir_all(far_talk.parent().expect("a folder")).expect("made");
        fs::write(&far_talk, replied).expect("the session answers");
        let near_talk = steer::replies(near_steers, "SAK9FZ", "t1").expect("a talk");
        // Nothing tells the test when the link has carried the answer, so
        // it looks again every few milliseconds until it has.
        tokio::time::timeout(PATIENCE, async {
            while fs::read_to_string(&near_talk).unwrap_or_default() != replied {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the talk's answer comes back to where it was asked");
        let nobody = Request::Steer {
            address: "NOPE".to_owned(),
            steer: json!({"text": "hello"}),
        };
        assert_eq!(
            ask(near_socket.to_owned(), nobody).await,
            Err("no session goes by NOPE now".to_owned()),
            "a steer for nobody is refused"
        );
    }

    /// The steer inboxes of the test machine whose node's socket is
    /// `socket`.
    fn inboxes(socket: &Path) -> PathBuf {
        socket.with_file_name("steer")
    }

    /// Where the harness marks the sessions it parks for a node on
    /// `socket`, beside it.
    fn parks_of(socket: &Path) -> PathBuf {
        socket.with_file_name("parked")
    }

    /// Where the updater of a node on `socket` is asked, beside it.
    fn updater_of(socket: &Path) -> PathBuf {
        socket.with_file_name("update.sock")
    }

    /// The providers of a node on `socket`, beside it: one a test may put
    /// in place, and one never there.
    fn providers_of(socket: &Path) -> Vec<String> {
        ["provider", "absent"]
            .map(|name| socket.with_file_name(name).display().to_string())
            .into()
    }

    fn piped(pipe: tokio::io::DuplexStream) -> Connection {
        let (reading, writing) = tokio::io::split(pipe);
        Connection {
            reading: Box::new(reading),
            writing: Box::new(writing),
            keep: Box::new(()),
            refused: None,
            releases: false,
            hello_within: Some(link::dial::HELLO_WITHIN),
        }
    }

    /// A dial whose way in always turns this machine away, telling
    /// `dialled` each time it is dialled.
    struct Refusing {
        dialled: mpsc::UnboundedSender<tokio::time::Instant>,
    }

    impl Dial for Refusing {
        fn describe(&self) -> String {
            "a way in that refuses".to_owned()
        }

        fn dial(&self, _realm: &str) -> impl Future<Output = io::Result<Connection>> + Send {
            let _ = self.dialled.send(tokio::time::Instant::now());
            // The other end hangs up before saying hello, as SSH does.
            let (near, _far) = tokio::io::duplex(64);
            let mut connection = piped(near);
            connection.refused = Some(tokio::spawn(async {
                Some("joel@equilibrius: Permission denied (publickey).".to_owned())
            }));
            std::future::ready(Ok(connection))
        }

        fn there(&self, words: &[String]) -> Vec<String> {
            let mut there = vec!["into-equilibrius".to_owned()];
            there.extend_from_slice(words);
            there
        }
    }

    #[tokio::test]
    async fn a_link_that_turned_this_machine_away_isnt_dialled_again_until_that_is_withdrawn() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (dialled, mut dials) = mpsc::unbounded_channel();
        let refusals = Refusals {
            folder: Some(dir.path().join("refused")),
            look_every: Duration::from_millis(50),
        };
        let starting = Starting {
            node: dir.path().join("work.sock"),
            held: dir.path().join("work.held"),
            asking: None,
            steers: dir.path().join("steer"),
        };
        let membership = Realm {
            name: "work".to_owned(),
            machine: "optimus".to_owned(),
        };
        let dial = Arc::new(Refusing { dialled });
        let words = dial.there(&[]);
        let keeping = tokio::spawn(keep_linked(
            dial,
            Memory::default(),
            membership,
            (None, starting, refusals.clone()),
        ));

        dials.recv().await.expect("the link is dialled");
        // Past the first redial after a drop and many looks: that no dial
        // comes can only be shown over a span, not waited on as an event.
        assert!(
            tokio::time::timeout(REDIAL_FIRST * 2, dials.recv())
                .await
                .is_err(),
            "a refused link isn't dialled again while its refusal stands"
        );
        assert!(
            refusals.stands(&words),
            "the refusal is remembered for a node started again"
        );

        let record = refusals.record(&words).expect("the refusal is kept");
        fs::remove_file(record).expect("the refusal is withdrawn");
        tokio::time::timeout(PATIENCE, dials.recv())
            .await
            .expect("the link is dialled again once its refusal is withdrawn")
            .expect("the link is kept");
        keeping.abort();
    }

    #[test]
    fn a_machine_is_made_to_outlive_its_work_and_never_without_an_end() {
        let offer = |lasts| Offer {
            provider: "lender".to_owned(),
            shape: "small".to_owned(),
            facts: offer::Shape::default(),
            costs: offer::Costs {
                hourly: "0.003".to_owned(),
                currency: "USD".to_owned(),
            },
            lasts,
        };
        let mut work = work(Vec::new());
        work.lasts = Some(jiff::SignedDuration::from_mins(30));
        assert_eq!(
            lasting(&work, &offer(Some(7200))),
            Ok(Duration::from_secs(40 * 60)),
            "past the work's end by the time its sessions have to hand over"
        );
        assert_eq!(
            lasting(&work, &offer(Some(2100))),
            Ok(Duration::from_secs(2100)),
            "but no longer than the offer lives"
        );
        work.lasts = None;
        assert_eq!(
            lasting(&work, &offer(Some(7200))),
            Ok(Duration::from_secs(7200)),
            "as long as the offer lives, where the work doesn't say"
        );
        assert!(
            lasting(&work, &offer(None)).is_err(),
            "and never with no end, where neither says"
        );
    }

    #[tokio::test]
    async fn a_node_says_what_its_providers_offer_and_why_one_couldnt() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("optimus").join("work.sock");
        // The node makes its socket's folder, open to no one else; its
        // providers are asked only when offers are.
        let (_stop, _node) = node(
            &Memory::default(),
            "optimus",
            Vec::<Piped>::new(),
            None,
            &socket,
        );
        let text = socket.with_file_name("provider.sh");
        fs::write(
            &text,
            "#!/bin/sh\necho '{\"shape\": \"scratch\", \"costs\": {\"hourly\": \"0.12\", \"currency\": \"USD\"}}'\n",
        )
        .expect("the provider is written");
        // Put in place by a child, so no test's fork holds it open.
        let put = std::process::Command::new("install")
            .args(["-m", "755"])
            .arg(&text)
            .arg(socket.with_file_name("provider"))
            .status()
            .expect("install runs");
        assert!(put.success(), "the provider is put in place");

        let Ok(Response::Offers(offers)) = ask(socket.clone(), Request::Offers).await else {
            panic!("the node says what is offered");
        };
        assert_eq!(
            offers
                .offers
                .iter()
                .map(|offer| (offer.provider.clone(), offer.shape.as_str()))
                .collect::<Vec<_>>(),
            [(
                socket.with_file_name("provider").display().to_string(),
                "scratch"
            )],
            "each shape a provider offers, by the program that offers it"
        );
        let absent = socket.with_file_name("absent").display().to_string();
        assert!(
            offers
                .unanswered
                .get(&absent)
                .is_some_and(|why| why.contains("couldn't run")),
            "a program that couldn't say is named with why, not taken as offering nothing: {:?}",
            offers.unanswered
        );
    }

    #[tokio::test]
    async fn a_machine_that_has_left_is_no_longer_one_of_the_realms() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("optimus").join("work.sock");
        let store = Memory::default();
        let nowhere = facts::Sources {
            home: "/".into(),
            queue: None,
            disk_guard: None,
            declared: "/nowhere".into(),
            reached: "/nowhere".into(),
            updated: "/nowhere".into(),
        };
        let lent = |name: &str, leaves: jiff::Timestamp| facts::Machine {
            declared: Some(crate::declared::Declared {
                reach: Vec::new(),
                probes: BTreeMap::new(),
                outlives: Vec::new(),
                costs_money: true,
                taints: Vec::new(),
                cores: None,
                agents: BTreeMap::new(),
                leaves_at: Some(leaves),
                within: None,
                hosts: Vec::new(),
            }),
            ..facts::measure(name, &nowhere)
        };
        let now = jiff::Timestamp::now();
        let minute = jiff::SignedDuration::from_mins(1);
        store
            .put_machine(&lent("gone", now - minute))
            .expect("what it said is kept");
        store
            .put_machine(&lent("staying", now + minute * 60))
            .expect("what it said is kept");
        let (_stop, _node) = node(&store, "optimus", Vec::<Piped>::new(), None, &socket);
        let names = tokio::task::spawn_blocking(move || {
            let mut client = Client::connect(&socket).expect("the client connects");
            let Ok(Ok(Response::Machines(all))) = client.ask(&Request::Machines) else {
                panic!("the node lists its machines");
            };
            all.into_iter().map(|one| one.name).collect::<Vec<_>>()
        })
        .await
        .expect("the machines are asked for");
        assert_eq!(
            names,
            ["optimus", "staying"],
            "a machine past the time it said it leaves is no longer listed"
        );
    }

    /// A client of the node at a socket, speaking the daemon interface.
    struct Speaking {
        lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
        writing: tokio::net::unix::OwnedWriteHalf,
    }

    impl Speaking {
        async fn to(socket: &Path) -> Self {
            let (reading, writing) = UnixStream::connect(socket)
                .await
                .expect("the client connects")
                .into_split();
            let lines = BufReader::new(reading).lines();
            Self { lines, writing }
        }

        async fn ask(&mut self, ask: Value) {
            let mut line = ask.to_string();
            line.push('\n');
            self.writing
                .write_all(line.as_bytes())
                .await
                .expect("the node hears");
        }

        async fn next(&mut self) -> Value {
            let line = tokio::time::timeout(PATIENCE, self.lines.next_line())
                .await
                .expect("the node answers in time")
                .expect("the node is read")
                .expect("the node answers");
            serde_json::from_str(&line).expect("an answer is JSON")
        }
    }

    /// What equilibrius says of itself, as measured at `at`.
    fn equilibrius_at(at: &str) -> facts::Machine {
        let nowhere = facts::Sources {
            home: "/".into(),
            queue: None,
            disk_guard: None,
            declared: "/nowhere".into(),
            reached: "/nowhere".into(),
            updated: "/nowhere".into(),
        };
        facts::Machine {
            measured_at: at.to_owned(),
            ..facts::measure("equilibrius", &nowhere)
        }
    }

    #[tokio::test]
    async fn a_client_opening_with_hello_is_answered_in_the_daemon_interface() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("optimus").join("work.sock");
        let store = Memory::default();
        store
            .put_machine(&equilibrius_at("2026-10-31T04:00:00Z"))
            .expect("what it said is kept");
        let (_stop, _node) = node(&store, "optimus", Vec::<Piped>::new(), None, &socket);
        let mut client = Speaking::to(&socket).await;

        client.ask(json!({"id": 1, "ask": "hello"})).await;
        let hello = client.next().await;
        assert_eq!(
            (&hello["id"], &hello["version"], &hello["domain"]),
            (&json!(1), &json!(1), &json!("node")),
            "hello says which version it speaks: {hello}"
        );
        assert!(
            hello["kinds"]["session"]["live"].is_array() && hello["kinds"]["machine"].is_object(),
            "hello names the kinds it holds: {hello}"
        );

        client
            .ask(json!({"id": 2, "ask": "list", "kind": "weather"}))
            .await;
        let refused = client.next().await;
        assert_eq!(
            (&refused["id"], &refused["error"]["code"]),
            (&json!(2), &json!("unknown-kind")),
            "a kind it doesn't hold is refused: {refused}"
        );

        client
            .ask(json!({"id": 3, "ask": "watch", "kind": "machine"}))
            .await;
        let whole = client.next().await;
        let keys: Vec<&Value> = whole["records"]
            .as_array()
            .expect("the records come whole first")
            .iter()
            .map(|record| &record["key"])
            .collect();
        assert_eq!(
            keys,
            [&json!("machine:equilibrius"), &json!("machine:optimus")],
            "this machine among the others, by key"
        );
        let here: Vec<&Value> = whole["records"]
            .as_array()
            .expect("the records")
            .iter()
            .map(|record| &record["here"])
            .collect();
        assert_eq!(
            here,
            [&json!(false), &json!(true)],
            "each saying whether it is this one"
        );
        store
            .put_machine(&equilibrius_at("2026-10-31T04:01:00Z"))
            .expect("what it said is kept");
        // This machine's own minute's measure may be told first.
        let mut changed = client.next().await;
        while changed["changed"][0]["key"] == json!("machine:optimus") {
            changed = client.next().await;
        }
        assert_eq!(
            (
                &changed["id"],
                &changed["changed"][0]["key"],
                &changed["changed"][0]["measured_at"]
            ),
            (
                &json!(3),
                &json!("machine:equilibrius"),
                &json!("2026-10-31T04:01:00Z")
            ),
            "a machine saying itself again is sent as what changed: {changed}"
        );
    }

    /// The quests a node on `socket` serves, from a file beside it.
    fn quests_of(socket: &Path) -> served::Source {
        served::Source {
            file: Some(socket.with_file_name("quests.json")),
            folder: None,
            talks: None,
            records: Some("quests".to_owned()),
            whole: None,
            key: Some("id".to_owned()),
            by: None,
        }
    }

    #[tokio::test]
    async fn a_file_the_node_serves_is_a_kind_its_clients_watch() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("optimus").join("work.sock");
        let (_stop, _node) = node(
            &Memory::default(),
            "optimus",
            Vec::<Piped>::new(),
            None,
            &socket,
        );
        let mut client = Speaking::to(&socket).await;

        client.ask(json!({"id": 1, "ask": "hello"})).await;
        let hello = client.next().await;
        assert_eq!(
            hello["kinds"]["quest"],
            json!({"live": []}),
            "hello names the kind served, with no field its own to call live: {hello}"
        );
        client.ask(json!({"id": 2, "ask": "health"})).await;
        let health = client.next().await;
        assert!(
            health["sources"].as_array().is_some_and(|sources| sources
                .iter()
                .any(|source| { source["name"] == "quest" && source["state"] == "answering" })),
            "a file not written yet holds nothing, which is no trouble: {health}"
        );

        client
            .ask(json!({"id": 3, "ask": "watch", "kind": "quest"}))
            .await;
        assert_eq!(client.next().await["records"], json!([]), "none yet");
        let file = socket.with_file_name("quests.json");
        let beside = socket.with_file_name(".quests.json.new");
        fs::write(
            &beside,
            json!({"quests": [{"id": "QEST-A", "title": "a"}]}).to_string(),
        )
        .expect("written");
        fs::rename(&beside, &file).expect("renamed into place");
        let changed = client.next().await;
        assert_eq!(
            (
                &changed["id"],
                &changed["changed"][0]["key"],
                &changed["changed"][0]["title"]
            ),
            (&json!(3), &json!("quest:QEST-A"), &json!("a")),
            "the file's record is sent as it arrives: {changed}"
        );
    }

    #[tokio::test]
    async fn a_watch_of_messages_is_sent_each_message_heard() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("optimus").join("work.sock");
        let store = Memory::default();
        let (_stop, _node) = node(&store, "optimus", Vec::<Piped>::new(), None, &socket);
        let said = |text: &str| {
            let draft = Draft {
                from: "joel".to_owned(),
                groups: vec![Group::new("peer.4uhc5q").expect("a valid group")],
                intent: None,
                text: text.to_owned(),
                re: None,
                realm: None,
            };
            mesh::written(&draft, 1)
        };
        let (first, body) = said("first");
        store.send_here(&first, body);
        let mut client = Speaking::to(&socket).await;

        client.ask(json!({"id": 1, "ask": "hello"})).await;
        let hello = client.next().await;
        assert_eq!(
            hello["kinds"]["message"],
            json!({"live": []}),
            "hello names messages, none of whose fields change: {hello}"
        );
        client
            .ask(json!({"id": 2, "ask": "watch", "kind": "message"}))
            .await;
        let records = client.next().await["records"].clone();
        assert_eq!(
            (&records[0]["key"], &records[0]["text"]),
            (&json!(format!("message:{first}")), &json!("first")),
            "the messages already heard come first: {records}"
        );
        let (second, body) = said("second");
        store.send_here(&second, body);
        let changed = client.next().await;
        assert_eq!(
            (
                &changed["id"],
                &changed["changed"][0]["key"],
                &changed["changed"][0]["text"]
            ),
            (
                &json!(2),
                &json!(format!("message:{second}")),
                &json!("second")
            ),
            "a message heard is sent as it arrives: {changed}"
        );
    }

    #[tokio::test]
    async fn a_steer_asked_in_the_daemon_interface_reaches_its_sessions_inbox() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("optimus").join("work.sock");
        let store = Memory::default();
        let presence = json!({
            "session": "here", "addresses": ["HERE12"], "groups": [],
            "busy": false, "state": "listening", "at": jiff::Timestamp::now().to_string(),
        });
        store.start_here("here", presence, true);
        // One parked has no process, and reads its inbox as it wakes.
        let parked = json!({
            "session": "napper", "addresses": ["NAPPER"], "groups": [],
            "busy": false, "state": "parked", "at": jiff::Timestamp::now().to_string(),
        });
        store.start_here("napper", parked, false);
        let (_stop, _node) = node(&store, "optimus", Vec::<Piped>::new(), None, &socket);
        let mut client = Speaking::to(&socket).await;
        client.ask(json!({"id": 1, "ask": "hello"})).await;
        client.next().await;

        let said = json!({"act": "pause", "from": "joel", "at": "2026-11-07T18:00:00Z"});
        client
            .ask(json!({"id": 2, "ask": "steer", "address": "here12", "steer": said}))
            .await;
        let steered = client.next().await;
        assert_eq!(
            (&steered["id"], &steered["steered"]["machine"]),
            (&json!(2), &json!("optimus")),
            "a session here is steered here: {steered}"
        );
        let inbox: Vec<Value> = fs::read_dir(inboxes(&socket).join("HERE12"))
            .expect("its inbox is made")
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|it| it == "json"))
            .map(|entry| {
                serde_json::from_str(&fs::read_to_string(entry.path()).expect("it reads"))
                    .expect("it is JSON")
            })
            .collect();
        assert_eq!(inbox, [said], "what was said is in its inbox, once");

        let talk = json!({"text": "what now?", "id": "talk-1", "from": "joel"});
        client
            .ask(json!({"id": 4, "ask": "steer", "address": "NAPPER", "steer": talk}))
            .await;
        let left = client.next().await;
        assert_eq!(
            (&left["id"], &left["steered"]["machine"]),
            (&json!(4), &json!("optimus")),
            "a session parked here is left what is said, to read as it wakes: {left}"
        );

        client
            .ask(json!({"id": 5, "ask": "steer", "address": "NAPPER", "steer": {"act": "wake"}}))
            .await;
        assert_eq!(
            client.next().await["error"]["message"],
            json!("NAPPER isn't parked with a holder to wake it this boot"),
            "a wake with no holder's file to write is refused, saying why"
        );
        let talked: Vec<PathBuf> = fs::read_dir(inboxes(&socket).join("NAPPER"))
            .expect("its inbox is made")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|it| it == "json"))
            .collect();
        assert_eq!(
            talked.len(),
            1,
            "a wake leaves nothing in its inbox: {talked:?}"
        );

        client
            .ask(json!({"id": 3, "ask": "steer", "address": "NOPE", "steer": {"act": "stop"}}))
            .await;
        assert_eq!(
            client.next().await["error"],
            json!({"code": "refused", "message": "no session goes by NOPE now"}),
            "a steer for nobody is refused, saying why"
        );
    }

    #[tokio::test]
    async fn a_watch_of_sessions_is_sent_each_session_that_arrives() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("optimus").join("work.sock");
        let store = Memory::default();
        let (_stop, _node) = node(&store, "optimus", Vec::<Piped>::new(), None, &socket);
        let mut client = Speaking::to(&socket).await;
        client.ask(json!({"id": 1, "ask": "hello"})).await;
        client.next().await;

        client
            .ask(json!({"id": 4, "ask": "watch", "kind": "session"}))
            .await;
        assert_eq!(
            client.next().await,
            json!({"id": 4, "records": []}),
            "no session runs yet"
        );
        let presence = json!({
            "session": "there", "addresses": ["THERE1"], "groups": [],
            "busy": true, "state": "busy", "at": jiff::Timestamp::now().to_string(),
        });
        store
            .put_presence("there", "equilibrius", &presence)
            .expect("the presence is kept");
        let arrived = client.next().await;
        assert_eq!(
            (
                &arrived["id"],
                &arrived["changed"][0]["key"],
                &arrived["changed"][0]["here"]
            ),
            (&json!(4), &json!("session:there"), &json!(false)),
            "a session arriving is sent, saying it runs elsewhere: {arrived}"
        );
    }

    #[tokio::test]
    async fn a_watch_asked_when_told_moves_holds_back_what_only_an_open_launcher_shows() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("optimus").join("work.sock");
        let store = Memory::default();
        let (_stop, _node) = node(&store, "optimus", Vec::<Piped>::new(), None, &socket);
        let mut client = Speaking::to(&socket).await;
        client.ask(json!({"id": 1, "ask": "hello"})).await;
        client.next().await;
        let at = jiff::Timestamp::now().to_string();
        let presence = |session: &str, busy: bool, title: &str| {
            json!({
                "session": session, "addresses": [session.to_uppercase()], "groups": [],
                "busy": busy, "state": if busy { "busy" } else { "idle" },
                "at": at, "title": title,
            })
        };
        let put_as = |session, busy, title| {
            store
                .put_presence(session, "equilibrius", &presence(session, busy, title))
                .expect("the presence is kept");
        };
        let put = |busy, title| put_as("there", busy, title);

        // A shut launcher's watch, as the shell asks for one.
        let shut = json!({"id": 4, "ask": "watch", "kind": "session", "when": ["told"]});
        client.ask(shut).await;
        assert_eq!(
            client.next().await["records"],
            json!([]),
            "no session runs yet"
        );
        put(true, "one");
        let came = client.next().await;
        assert_eq!(
            came["changed"][0]["key"],
            json!("session:there"),
            "a session coming is sent: {came}"
        );
        put_as("beside", true, "other");
        client.next().await;

        put(false, "one");
        put_as("beside", false, "other");
        put(false, "two");
        let retitled = client.next().await;
        assert_eq!(
            (
                retitled["changed"].as_array().map(Vec::len),
                &retitled["changed"][0]["presence"]["busy"],
                &retitled["changed"][0]["presence"]["title"]
            ),
            (Some(1), &json!(false), &json!("two")),
            "its working ending wasn't sent alone, but with its retitling, \
             and the other's step stayed held back: {retitled}"
        );

        put(true, "two");
        // The launcher opening asks for every change, under the same id.
        client
            .ask(json!({"id": 4, "ask": "watch", "kind": "session"}))
            .await;
        let opened = client.next().await;
        let busy = |key: &str| {
            opened["changed"]
                .as_array()
                .and_then(|changed| changed.iter().find(|record| record["key"] == key))
                .map(|record| record["presence"]["busy"].clone())
        };
        assert_eq!(
            (
                opened.get("records"),
                busy("session:there"),
                busy("session:beside")
            ),
            (None, Some(json!(true)), Some(json!(false))),
            "asked again, it sends all it held back at once, not everything whole: {opened}"
        );
    }

    #[tokio::test]
    async fn a_watch_of_sessions_says_how_each_stands_and_hears_a_park_marked() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("optimus").join("work.sock");
        let store = Memory::default();
        let now = jiff::Timestamp::now().to_string();
        let boot = store::folders::boot_id();
        let presence = |session: &str, state: &str| {
            json!({
                "session": session, "addresses": [session.to_uppercase()], "groups": [],
                "busy": false, "state": state, "at": now, "owner": {"bootId": boot},
            })
        };
        store.start_here("runner", presence("runner", "listening"), true);
        // Parked, its process ended, though the harness hasn't marked it.
        store.start_here("napper", presence("napper", "parked"), false);
        let (_stop, _node) = node(&store, "optimus", Vec::<Piped>::new(), None, &socket);
        let mut client = Speaking::to(&socket).await;
        client.ask(json!({"id": 1, "ask": "hello"})).await;
        client.next().await;

        client
            .ask(json!({"id": 2, "ask": "watch", "kind": "session"}))
            .await;
        let records = client.next().await;
        let stands: Vec<(&Value, &Value)> = records["records"]
            .as_array()
            .expect("its records")
            .iter()
            .map(|record| (&record["key"], &record["standing"]))
            .collect();
        assert_eq!(
            stands,
            [
                (&json!("session:napper"), &json!("closed")),
                (&json!("session:runner"), &json!("running")),
            ],
            "one running here runs, and one ended this boot unmarked closed: {records}"
        );
        assert!(
            records["records"][0]["told"].as_str().is_some(),
            "each with the digest of what a shut launcher reads: {records}"
        );

        fs::write(parks_of(&socket).join("napper.json"), "{}").expect("a marker");
        let parked = client.next().await;
        assert_eq!(
            (
                &parked["changed"][0]["key"],
                &parked["changed"][0]["standing"]
            ),
            (&json!("session:napper"), &json!("parked")),
            "its park marked, it's parked: {parked}"
        );
        assert!(
            parked["changed"][0]["told"].as_str().is_some(),
            "which a shut launcher reads: {parked}"
        );
    }

    #[tokio::test]
    async fn another_machines_node_starts_joins_and_steers_its_sessions_for_this_one() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (holder, given) = stand_in_holder(dir.path());
        let far_socket = dir.path().join("equilibrius").join("work.sock");
        let near_socket = dir.path().join("optimus").join("work.sock");
        let near_steers = inboxes(&near_socket);
        let far_steers = inboxes(&far_socket);
        let far_store = Memory::default();
        let holds = Starter {
            program: holder,
            scoped: false,
        };
        let nobody = Vec::<Piped>::new();
        let far = node(&far_store, "equilibrius", nobody, Some(holds), &far_socket);
        let near_store = Memory::default();
        let held = hold_a_session(dir.path(), &far_store);
        let dial = Piped {
            store: far_store,
            socket: far_socket,
            held,
            proven: false,
            asks_back: false,
        };
        let near = node(&near_store, "optimus", vec![dial], None, &near_socket);
        // Nothing tells the test when the link has said what equilibrius
        // is, so it looks again every few milliseconds until it has.
        tokio::time::timeout(PATIENCE, async {
            while !near_store
                .machines()
                .iter()
                .any(|one| one.name == "equilibrius")
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the link says what equilibrius is");
        let carried = listed(near_socket.clone(), "0e50").await;
        assert_eq!(carried.held.as_deref(), Some("5e551011"));
        assert_eq!(
            carried.attach,
            Some(
                [
                    "into-equilibrius",
                    "mycelium",
                    "--realm",
                    "work",
                    "attach",
                    "5e551011"
                ]
                .map(str::to_owned)
                .to_vec()
            ),
            "a session held elsewhere is joined through the dial that reaches it"
        );

        let there = Need::Machine("equilibrius".to_owned());
        let asked = Request::Start(Start {
            work: work(vec![there]),
            folder: Some(dir.path().to_owned()),
            command: vec!["pi".to_owned()],
            may_cost: false,
        });
        let started = ask(near_socket.clone(), asked).await;
        let Ok(Response::Started(started)) = started else {
            panic!("equilibrius's node starts it: {started:?}");
        };
        assert_eq!(started.machine, "equilibrius");
        assert_eq!(
            started.attach,
            [
                "into-equilibrius",
                "mycelium",
                "--realm",
                "work",
                "attach",
                &started.held
            ],
            "its words join it through the dial that reaches equilibrius"
        );
        let given = fs::read_to_string(&given).expect("equilibrius's holder was started");
        assert!(
            given.lines().any(|word| word == started.held),
            "under the id its node answered: {given}"
        );

        steers_the_session_on_equilibrius(&near_socket, &near_steers, &far_steers).await;

        for (stop, node) in [near, far] {
            drop(stop);
            let stopped = node.await.expect("the node runs to its end");
            assert!(stopped.is_ok(), "the node stops cleanly: {stopped:?}");
        }
    }

    #[test]
    fn a_hosted_session_is_in_the_window_its_join_holds() {
        let presence = store::Presence {
            session: "01a1".to_owned(),
            body: json!({
                "session": "01a1",
                "addresses": ["4UHC5Q"],
                "at": "2026-11-11T02:00:00Z",
                "owner": {"pid": 4242},
                "hosted": {"host": "work-attended", "conversation": "01a1"},
            }),
            elsewhere: None,
            lives: true,
            unreachable_since: None,
        };
        let window = record::Window {
            terminal: "kitty".to_owned(),
            id: "7".to_owned(),
            within: None,
            app_id: Some("pi-session.7".to_owned()),
        };
        let hold = |id: &str, command: &[&str]| hold::Held {
            id: id.to_owned(),
            command: command.iter().map(|word| (*word).to_owned()).collect(),
            folder: "/home/jitsusama".into(),
            holder: 1,
            pid: 77,
            parked: false,
            since: "2026-11-11T02:00:00Z".to_owned(),
            windows: vec![window.clone()],
        };
        let join = ["node", "host-join.ts", "--session", "01a1", "--", "pi"];
        let joined =
            session_held(&presence, "optimus", &[hold("0d1e", &join)]).expect("a session here");
        assert_eq!(
            joined.held.as_deref(),
            Some("0d1e"),
            "held by its join's holder"
        );
        assert_eq!(
            joined.window.and_then(|one| one.app_id).as_deref(),
            Some("pi-session.7"),
            "in the window its join holds"
        );
        let fell_back = ["pi", "--session", "/home/jitsusama/.pi/01a1.jsonl"];
        let alone = session_held(&presence, "optimus", &[hold("0d1e", &fell_back)])
            .expect("a session here");
        assert_eq!(
            (alone.held, alone.window),
            (None, None),
            "a join fallen back to plain pi on its log holds a session of its own, not this"
        );
    }

    #[tokio::test]
    async fn a_conversation_a_pi_host_holds_is_joined_through_its_host() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("optimus").join("work.sock");
        let store = Memory::default();
        let presence = json!({
            "session": "01a1",
            "addresses": ["4UHC5Q"],
            "at": "2026-11-09T19:00:00Z",
            "owner": {"pid": 4242},
            "hosted": {"host": "work-attended", "conversation": "c-01a1"},
        });
        store.start_here("01a1", presence, true);
        let (stop, node) = node(&store, "optimus", Vec::<Piped>::new(), None, &socket);
        let hosted = listed(socket.clone(), "01a1").await;
        assert_eq!(
            hosted.attach,
            Some(
                [
                    "mycelium",
                    "--realm",
                    "work",
                    "attach",
                    "--host",
                    "work-attended",
                    "--session",
                    "c-01a1"
                ]
                .map(str::to_owned)
                .to_vec()
            ),
            "a hosted conversation is joined through its host, by its conversation"
        );
        let view = [
            "node",
            "host-join.ts",
            "--view",
            "--session",
            "0e50",
            "--",
            "pi",
        ];
        store.start_here(
            "0e50",
            json!({
                "session": "0e50",
                "addresses": ["7M6JKG"],
                "at": "2026-11-09T19:00:00Z",
                "owner": {"pid": 4242},
                "hosted": {"host": "work-attended", "conversation": "0e50", "view": view},
            }),
            true,
        );
        assert_eq!(
            listed(socket.clone(), "0e50").await.attach,
            Some(view.map(str::to_owned).to_vec()),
            "one whose presence names its view is joined by those words"
        );
        drop(stop);
        let stopped = node.await.expect("the node runs to its end");
        assert!(stopped.is_ok(), "the node stops cleanly: {stopped:?}");
    }

    #[tokio::test]
    async fn a_machine_starts_work_on_the_one_that_dialled_it_through_that_link() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (holder, given) = stand_in_holder(dir.path());
        let far_socket = dir.path().join("equilibrius").join("work.sock");
        let near_socket = dir.path().join("optimus").join("work.sock");
        let far_store = Memory::default();
        let far = node(
            &far_store,
            "equilibrius",
            Vec::<Piped>::new(),
            None,
            &far_socket,
        );
        let holds = Starter {
            program: holder,
            scoped: false,
        };
        let dial = Piped {
            store: far_store.clone(),
            socket: far_socket.clone(),
            held: dir.path().join("held"),
            proven: false,
            asks_back: true,
        };
        let near_store = Memory::default();
        let near = node(
            &near_store,
            "optimus",
            vec![dial],
            Some(holds),
            &near_socket,
        );
        // Nothing tells the test when equilibrius's node has heard of
        // optimus, or when its link has taken up what the node asks of it,
        // so it asks again every few milliseconds until a start goes
        // through.
        let there = Need::Machine("optimus".to_owned());
        let asked = Request::Start(Start {
            work: work(vec![there]),
            folder: Some(dir.path().to_owned()),
            command: vec!["pi".to_owned()],
            may_cost: false,
        });
        let started = tokio::time::timeout(PATIENCE, async {
            loop {
                if let Ok(Response::Started(started)) = ask(far_socket.clone(), asked.clone()).await
                {
                    break started;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("equilibrius starts work on optimus over the link optimus dialled");
        assert_eq!(started.machine, "optimus");
        assert_eq!(
            started.attach,
            [
                "ssh",
                "-t",
                "--",
                "optimus",
                "mycelium",
                "--realm",
                "work",
                "attach",
                &started.held
            ],
            "equilibrius never dialled optimus, so its words join it over SSH by name"
        );
        let given = fs::read_to_string(&given).expect("optimus's holder was started");
        assert!(
            given.lines().any(|word| word == started.held),
            "under the id optimus's node answered: {given}"
        );

        for (stop, node) in [near, far] {
            drop(stop);
            let stopped = node.await.expect("the node runs to its end");
            assert!(stopped.is_ok(), "the node stops cleanly: {stopped:?}");
        }
    }

    /// A node running in a test: what stops it, and what it ends with.
    type Running = (
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<io::Result<()>>,
    );

    /// An updater at `socket`, in a folder that is already there, answering
    /// each ask with a step and then `ending`.
    fn stand_in_updater(socket: &Path, ending: Value) {
        let listener = UnixListener::bind(socket).expect("listening");
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let line = format!("{}\n{ending}\n", json!({"step": "fetching"}));
                let (reading, mut writing) = stream.into_split();
                let mut asked = BufReader::new(reading).lines();
                if asked.next_line().await.ok().flatten().as_deref() == Some("update") {
                    let _ = writing.write_all(line.as_bytes()).await;
                }
            }
        });
    }

    /// Runs equilibrius's node and optimus's, linked, equilibrius's end
    /// `proven` or not, once optimus has heard what equilibrius is.
    async fn linked_to_equilibrius(dir: &Path, proven: bool) -> (PathBuf, [Running; 2]) {
        let far_socket = dir.join("equilibrius").join("work.sock");
        let near_socket = dir.join("optimus").join("work.sock");
        let far_store = Memory::default();
        let far = node(
            &far_store,
            "equilibrius",
            Vec::<Piped>::new(),
            None,
            &far_socket,
        );
        let near_store = Memory::default();
        let dial = Piped {
            store: far_store,
            socket: far_socket,
            held: dir.join("held"),
            proven,
            asks_back: false,
        };
        let near = node(&near_store, "optimus", vec![dial], None, &near_socket);
        // Nothing tells the test when the link has said what equilibrius
        // is, so it looks again every few milliseconds until it has.
        tokio::time::timeout(PATIENCE, async {
            while !near_store
                .machines()
                .iter()
                .any(|one| one.name == "equilibrius")
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the link says what equilibrius is");
        (near_socket, [near, far])
    }

    #[tokio::test]
    async fn a_machine_updates_when_asked_over_a_link_its_key_proved_and_only_then() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let done = json!({"step": "done", "commit": "c0ffee", "applied": "now",
            "restart": null, "restarted": []});
        let (near_socket, nodes) = linked_to_equilibrius(dir.path(), true).await;
        // equilibrius's node has made its folder by now, as only it may.
        stand_in_updater(
            &updater_of(&dir.path().join("equilibrius").join("work.sock")),
            done.clone(),
        );
        let update = |machine: &str| Request::Update {
            machine: machine.to_owned(),
        };
        let updated = ask(near_socket.clone(), update("equilibrius")).await;
        assert_eq!(
            updated,
            Ok(Response::Updated(done)),
            "its node asks its updater, and the ending comes back"
        );
        let here = ask(near_socket, update("optimus")).await;
        assert!(
            here.as_ref().is_err_and(
                |why| why.starts_with("no updater answers at") && why.contains("update.sock")
            ),
            "a machine with no updater says where it looked: {here:?}"
        );
        stop(nodes).await;

        let dir = tempfile::tempdir().expect("a temporary folder");
        let (near_socket, nodes) = linked_to_equilibrius(dir.path(), false).await;
        let refused = ask(near_socket, update("equilibrius")).await;
        assert_eq!(
            refused,
            Err("equilibrius updates only when asked over a link its key proved, which this one isn't".to_owned()),
            "a link nothing proved can't ask"
        );
        stop(nodes).await;
    }

    /// Stops each of `nodes`, checking it stops cleanly.
    async fn stop(nodes: [Running; 2]) {
        for (stop, node) in nodes {
            drop(stop);
            let stopped = node.await.expect("the node runs to its end");
            assert!(stopped.is_ok(), "the node stops cleanly: {stopped:?}");
        }
    }

    #[tokio::test]
    async fn a_watch_that_overflowed_reads_only_the_messages_it_missed() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("mycelium").join("work.sock");
        let store = Memory::default();
        let (stop, node) = node::<Ssh>(&store, "optimus", Vec::new(), None, &socket);
        let said = |text: &str, pid| {
            let draft = Draft {
                from: "joel".to_owned(),
                groups: vec![Group::new("peer.4uhc5q").expect("a valid group")],
                intent: None,
                text: text.to_owned(),
                re: None,
                realm: None,
            };
            mesh::written(&draft, pid)
        };
        let (heard, body) = said("first", 1);
        store.send_here(&heard, body);
        let texts = |wanted: usize| {
            let socket = socket.clone();
            let listed = tokio::task::spawn_blocking(move || {
                let mut client = Client::connect(&socket).expect("the client connects");
                let asked = Request::Messages {
                    groups: Pattern::new("peer.*").expect("a valid pattern"),
                    limit: 10,
                };
                // Nothing tells a client when the node has read the
                // store, so it asks again every few milliseconds until it
                // holds as many as are wanted.
                loop {
                    let Ok(Ok(Response::Messages(all))) = client.ask(&asked) else {
                        panic!("the node lists its messages");
                    };
                    if all.len() >= wanted {
                        return all.into_iter().map(|one| one.text).collect::<Vec<_>>();
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
            async move {
                tokio::time::timeout(PATIENCE, listed)
                    .await
                    .expect("the node reads them in time")
                    .expect("the client asks")
            }
        };
        assert_eq!(texts(1).await, ["first"], "a message told of is read");

        let read = store.messages_read();
        let (missed, body) = said("missed", 2);
        store.miss(&missed, body);
        store.overflow();
        assert_eq!(
            texts(2).await,
            ["first", "missed"],
            "a rescan reads the message it missed"
        );
        assert_eq!(
            store.messages_read() - read,
            1,
            "and not again the one it holds, whose opens would overflow a watch again"
        );

        drop(stop);
        let stopped = node.await.expect("the node runs to its end");
        assert!(stopped.is_ok(), "the node stops cleanly: {stopped:?}");
    }

    #[tokio::test]
    async fn a_present_process_hanging_up_ends_its_session_at_once() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("mycelium").join("work.sock");
        let store = Memory::default();
        // The session is this test's own process's, as the kernel names the
        // process at the other end of each connection the test makes.
        let presence = json!({
            "session": "01a1",
            "addresses": ["4UHC5Q"],
            "at": "2026-10-29T06:00:00Z",
            "owner": {"pid": std::process::id()},
        });
        store.start_here("01a1", presence, true);
        let (stop, node) = node::<Ssh>(&store, "optimus", Vec::new(), None, &socket);

        let asked = tokio::task::spawn_blocking(move || {
            let stands = |client: &mut Client| {
                let Ok(Ok(Response::Sessions(all))) = client.ask(&Request::Sessions) else {
                    panic!("the node lists its sessions");
                };
                all.into_iter()
                    .find(|one| one.id == "01a1")
                    .map(|one| one.state)
            };
            let mut present = Client::connect(&socket).expect("the process connects");
            let said = present.ask(&Request::Present).expect("the node answers");
            assert_eq!(
                said,
                Ok(Response::Present),
                "the process is counted present"
            );
            let mut other = Client::connect(&socket).expect("another client connects");
            assert_eq!(stands(&mut other), Some(record::State::Idle), "it runs");
            drop(other);
            let mut asking = Client::connect(&socket).expect("a client connects");
            assert_eq!(
                stands(&mut asking),
                Some(record::State::Idle),
                "a connection that never said it was present ends nothing as it closes"
            );

            drop(present);
            // Nothing tells a client when the node has heard the hang-up,
            // so it asks again every few milliseconds until it has.
            while stands(&mut asking) == Some(record::State::Idle) {
                std::thread::sleep(Duration::from_millis(10));
            }
            stands(&mut asking)
        });
        let stood = tokio::time::timeout(PATIENCE, asked)
            .await
            .expect("the node hears the hang-up in time")
            .expect("the client asks");
        assert_eq!(
            stood,
            Some(record::State::Ended),
            "the session ended though its process runs on, since it hung up"
        );

        drop(stop);
        let stopped = node.await.expect("the node runs to its end");
        assert!(stopped.is_ok(), "the node stops cleanly: {stopped:?}");
    }

    #[tokio::test]
    async fn a_watch_gets_the_whole_state_then_a_message_a_client_sent() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("mycelium").join("work.sock");
        let store = Memory::default();
        let presence =
            json!({"session": "01a1", "addresses": ["4UHC5Q"], "at": "2026-10-26T20:00:00Z"});
        store.start_here("01a1", presence, true);
        let membership = Realm {
            name: "work".to_owned(),
            machine: "optimus".to_owned(),
        };
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let bound = bind(&socket).expect("the socket binds");
        let steers = dir.path().join("steer");
        let node = tokio::spawn({
            let store = store.clone();
            async move {
                let stopped = async {
                    // A dropped sender stops the node as surely as a sent stop.
                    let _ = stopped.await;
                };
                let given = bare(steers, None);
                run(
                    store,
                    membership,
                    Listed::<Ssh>(Vec::new()),
                    given,
                    bound,
                    stopped,
                )
                .await
            }
        });
        let asked = tokio::task::spawn_blocking(move || {
            let mut watch = Client::connect(&socket).expect("the watch connects");
            let watching = watch.ask(&Request::Watch {
                groups: Some(Pattern::new("peer.*").expect("a valid pattern")),
                recent: 10,
            });
            assert_eq!(watching.expect("the node answers"), Ok(Response::Watching));
            let Some(Event::SessionsChanged { sessions }) =
                watch.next_event().expect("the state comes")
            else {
                panic!("a watch starts with every session");
            };
            assert_eq!(
                sessions
                    .iter()
                    .map(|s| s.address.as_str())
                    .collect::<Vec<_>>(),
                ["4UHC5Q"]
            );
            assert_eq!(
                watch.next_event().expect("the state comes"),
                Some(Event::MessagesChanged {
                    messages: Vec::new()
                })
            );

            let mut sender = Client::connect(&socket).expect("the sender connects");
            let draft = Draft {
                from: "joel".to_owned(),
                groups: vec![Group::new("peer.4uhc5q").expect("a valid group")],
                intent: Some(Intent::Question),
                text: "how far along?".to_owned(),
                re: None,
                realm: None,
            };
            let sent = sender.ask(&Request::Send(draft));
            let Ok(Response::Sent(sent)) = sent.expect("the node answers") else {
                panic!("the node sends the message");
            };
            assert_eq!(
                watch.next_event().expect("the message comes"),
                Some(Event::Message {
                    message: sent.clone()
                })
            );
            sent.id
        });
        let id = tokio::time::timeout(PATIENCE, asked)
            .await
            .expect("the node answers in time")
            .expect("the client's checks pass");
        assert!(
            store.message(&id).is_some(),
            "what was sent is in the mesh's store"
        );

        drop(stop);
        let stopped = node.await.expect("the node runs to its end");
        assert!(stopped.is_ok(), "the node stops cleanly: {stopped:?}");
        assert!(
            !dir.path().join("mycelium").join("work.sock").exists(),
            "the socket is taken down"
        );
    }

    #[tokio::test]
    async fn a_node_starts_what_it_is_asked_where_placement_says_or_says_why_not() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("mycelium").join("work.sock");
        let (holder, given) = stand_in_holder(dir.path());
        let membership = Realm {
            name: "work".to_owned(),
            machine: "optimus".to_owned(),
        };
        let bound = bind(&socket).expect("the socket binds");
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let steers = dir.path().join("steer");
        let node = tokio::spawn(async move {
            let starter = Starter {
                program: holder,
                scoped: false,
            };
            let given = bare(steers, Some(starter));
            let stopped = async {
                // A dropped sender stops the node as surely as a sent stop.
                let _ = stopped.await;
            };
            let nobody = Listed::<Ssh>(Vec::new());
            run(Memory::default(), membership, nobody, given, bound, stopped).await
        });

        let folder = dir.path().to_owned();
        let answers = tokio::task::spawn_blocking(move || {
            let mut client = Client::connect(&socket).expect("the client connects");
            let mut start = |needs, folder| {
                let command = vec!["pi".to_owned(), "--continue".to_owned()];
                let asked = Request::Start(Start {
                    work: work(needs),
                    folder,
                    command,
                    may_cost: false,
                });
                client.ask(&asked).expect("the node answers")
            };
            let elsewhere = Need::Machine("equilibrius".to_owned());
            (
                start(vec![elsewhere], Some(folder)),
                start(Vec::new(), None),
            )
        });
        let (refused, started) = tokio::time::timeout(PATIENCE, answers)
            .await
            .expect("the node answers in time")
            .expect("the client asks");
        assert_eq!(
            refused,
            Err("nowhere: optimus isn't equilibrius".to_owned()),
            "a start nowhere can take says why, as the launcher shows it"
        );
        let Ok(Response::Started(started)) = started else {
            panic!("a start any machine can take starts here: {started:?}");
        };
        assert_eq!(started.machine, "optimus");
        assert_eq!(
            started.attach,
            ["mycelium", "--realm", "work", "attach", &started.held],
            "it says the words that join it"
        );
        let given = fs::read_to_string(&given).expect("the holder was started");
        let given: Vec<&str> = given.lines().collect();
        assert_eq!(
            given[..5],
            ["--realm", "work", "holder", "--id", &started.held],
            "the holder holds it under the id the node answers"
        );
        let home = std::env::home_dir().expect("the tests run with a home");
        assert_eq!(started.folder, home, "it says the folder it began in");
        assert_eq!(
            given[5..7],
            ["--folder", &home.display().to_string()],
            "a start naming no folder begins in its machine's home"
        );
        // Which words a login shell takes, and a Mac's yielding before
        // them, is start's own test; here, only that the node asks for them.
        assert_eq!(
            given[7..],
            ["--login", "--", "pi", "--continue"],
            "each start of the program runs through the login shell"
        );

        drop(stop);
        let stopped = node.await.expect("the node runs to its end");
        assert!(stopped.is_ok(), "the node stops cleanly: {stopped:?}");
    }
}
