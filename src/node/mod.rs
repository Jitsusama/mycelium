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
use crate::builders::{self, Builders};
use crate::carry::easing::{self, Eases, Easing};
use crate::carry::settle::{self, Settler, Settles};
use crate::carry::{chain, ease, ground, kept, ordered, through};
use crate::daemon;
use crate::facts::{self, Readiness};
use crate::flow::{self, Flows};
use crate::forage;
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
use crate::murmur::{self, Murmurs};
use crate::offer::{self, Offer};
use crate::own_health;
use crate::place;
use crate::placement::{self, Miss, Need, Placement, Work};
use crate::placements;
use crate::receptor;
use crate::record::{Message, Session, State as SessionState};
use crate::room;
use crate::run;
use crate::served::{self, Served};
use crate::share::exchange::Places;
use crate::state::State;
use crate::steer::{self, Steer};
use crate::store::{self, Change, Store};
use crate::wire::{
    Answer, By, Draft, Event, Offers, Ran, Reply, Request, Response, Run, Start, Started,
};
use crate::within;
use crate::{unanswered, words};
use turns::{Asked, Turns};

mod asker;
pub use asker::Processes;
mod cell;
pub use cell::records as cell_records;
mod crossing;
mod ended;
mod fences;
mod formed;
pub use formed::Formed;
mod glyph;
mod network;
pub use network::{Holding, Standing, records as network_records};
mod signals;
mod standing;
mod team;
mod tie;
pub use tie::{KIND as TIE_KIND, Talk, aliases as tie_aliases, records as tie_records, talk};
mod turns;

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

/// How often at most the machine's processes are looked through for the
/// fences its sessions show, as their presences change: each look reads
/// every process's status, and presences change many times a second.
const FENCES_SEEN_EVERY: Duration = Duration::from_secs(2);

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
///
/// The socket is bound and made private under a name of its own beside
/// `path`, then linked into place, so `path` appears already listening
/// and private: a client waiting for it to appear was once refused, as
/// binding makes the file before it listens (seen failing on 14
/// November). Linking, unlike renaming, refuses a path another node took
/// meanwhile. Hidden names left by binds whose process died before
/// linking in are swept first.
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
        sweep_staged(parent);
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
    let staged = staged(path);
    let _ = fs::remove_file(&staged);
    let listener = UnixListener::bind(&staged)?;
    let placed = link_in(&staged, path);
    let _ = fs::remove_file(&staged);
    placed.map(|()| Socket {
        listener,
        path: path.to_owned(),
    })
}

/// Makes the socket bound at `staged` private and links it in at `path`,
/// refusing a `path` another node took since this one looked.
fn link_in(staged: &Path, path: &Path) -> io::Result<()> {
    fs::set_permissions(staged, fs::Permissions::from_mode(0o600))?;
    fs::hard_link(staged, path).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("a node already serves {}", path.display()),
            )
        } else {
            error
        }
    })
}

/// Where the socket for `path` is bound before it is linked into place:
/// beside it, hidden, and named for this process and its count of binds,
/// so two binds never share one.
fn staged(path: &Path) -> PathBuf {
    static BINDS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let count = BINDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = format!(".{}-{count}", std::process::id());
    path.with_file_name(name)
}

/// Removes the sockets in `folder` that binds left under their hidden
/// names when their process died between binding and linking in, so
/// none gather there. A name whose process still runs, or might, stays.
fn sweep_staged(folder: &Path) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(staged_by) else {
            continue;
        };
        if pid != std::process::id()
            && gone(pid)
            && entry.file_type().is_ok_and(|kind| kind.is_socket())
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// The process that bound under `name`, when `name` is one `staged` gives.
fn staged_by(name: &str) -> Option<u32> {
    let (pid, count) = name.strip_prefix('.')?.split_once('-')?;
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(pid) || !digits(count) {
        return None;
    }
    pid.parse().ok()
}

/// Whether no process holds the id `pid`: only the kernel's "no such
/// process" says so, so a zombie, or one the signal may not reach, stays.
fn gone(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|pid| rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH))
}

/// Whether `session` has ended parked, which reads its inbox as it wakes.
fn parked_presence(session: &Session) -> bool {
    session.state == SessionState::Ended
        && session
            .presence
            .get("state")
            .and_then(serde_json::Value::as_str)
            == Some("parked")
}

/// Waits until `told` says its records changed, or forever where there is
/// none or it can say no more, so a loop selecting on it never spins.
async fn changed(told: &mut Option<watch::Receiver<u64>>) {
    if let Some(told) = told
        && told.changed().await.is_ok()
    {
        return;
    }
    std::future::pending::<()>().await;
}

/// How long the node sees a session ended before telling it
/// ([`ended::SETTLED_AFTER`]).
#[cfg(not(test))]
const ENDS_SETTLE: i64 = ended::SETTLED_AFTER;

/// None in the node's tests, which see an end told as it is first seen
/// rather than ten minutes on; `ended`'s own tests run the ten minutes.
#[cfg(test)]
const ENDS_SETTLE: i64 = 0;

/// Since when a carry is shown: a day ago.
fn shown_since() -> jiff::Timestamp {
    let now = jiff::Timestamp::now();
    now.checked_sub(kept::SHOWN_FOR).unwrap_or(now)
}

/// The groups the mesh put each session on `machine` in, running or
/// parked, by its id, as its presence gives them, so an orchestrator's
/// chain is believed only where the mesh agrees with the quests' record.
fn grouped(sessions: &[Session], machine: &str) -> BTreeMap<String, Vec<String>> {
    sessions
        .iter()
        .filter(|session| session.machine == machine)
        .filter(|session| {
            matches!(session.state, SessionState::Busy | SessionState::Idle)
                || parked_presence(session)
        })
        .filter_map(|session| {
            let id = session.presence.get("session")?.as_str()?.to_owned();
            let groups = session
                .presence
                .get("groups")?
                .as_array()?
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect();
            Some((id, groups))
        })
        .collect()
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
    /// The quests' waiting files, which its cells' forages say.
    pub waiting: PathBuf,
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
    /// The realm's folder of held sessions on this machine.
    pub held: PathBuf,
    /// Where the machine tells who runs its processes, which says whether
    /// a client asking to steer as the person is an agent's.
    pub processes: Processes,
    /// Where it keeps each carry an orchestrator ordered, a line each.
    pub carries: PathBuf,
    /// Where it keeps each command it runs, and who may say how a fenced
    /// one runs.
    pub runs: run::Given,
    /// Where pi's hosts each keep their record of the sessions they hold,
    /// which proves who owns a session a host holds.
    pub hosts: PathBuf,
    /// Where it keeps who orchestrates which quests, as each orchestrator
    /// told it (`carry::chain`).
    pub orchestrations: PathBuf,
    /// Where pi keeps each session's log, whose first line says which
    /// session it continues.
    pub logs: PathBuf,
    /// Where it keeps what the person says to sessions through it.
    pub words: words::Book,
    /// Where this machine's configuration declares it, whose fences say
    /// what ground a carry ordered from here may land on.
    pub declared: PathBuf,
    /// The folder its receptors each say how they stand in, if it serves
    /// any (`receptor`).
    pub receptors: Option<PathBuf>,
    /// The person's table of which receptors' climbs are said, as a
    /// sealed file (`stimuli`); unnamed, every source is silent.
    pub stimuli: Option<PathBuf>,
    /// The machines files naming the builders this machine sends Nix
    /// builds to, and how each is heard.
    pub builders: builders::Given,
    /// Where it keeps each placement it makes, if it keeps them
    /// (`placements`).
    pub placements: Option<placements::Log>,
    /// The socket its machine's own health daemon answers on, if it asks
    /// one (`own_health`).
    pub health: Option<PathBuf>,
    /// The quests' scratch folders, whose webs its cells murmur in, if it
    /// serves their murmurs (`murmur`).
    pub scratch: Option<PathBuf>,
    /// Where it keeps what it would do to ease its machine, and how
    /// often it looks, if it looks (`carry::easing`).
    pub eases: Option<Eases>,
    /// How it lets go of the trees a carry it ordered left here, once the
    /// carry has arrived, if it does (`carry::settle`).
    pub settles: Option<Settles>,
}

/// How long a machine made has to link: a provider's machine is ready in
/// seconds, and its node in a few more.
const LINKED_WITHIN: Duration = Duration::from_secs(5 * 60);

/// The kinds the node holds itself in the daemon interface, which no
/// served file may take.
pub const HOLDS: [&str; 17] = [
    "session",
    "machine",
    "message",
    team::KIND,
    network::KIND,
    cell::KIND,
    unanswered::KIND,
    words::KIND,
    EVENT,
    receptor::KIND,
    BUILDER,
    kept::KIND,
    forage::KIND,
    murmur::KIND,
    tie::KIND,
    formed::KIND,
    ended::KIND,
];

/// The kind of record that is one thing a session did, as its presence's
/// `events` says it.
pub const EVENT: &str = "event";

/// The kind of a builder's records.
pub const BUILDER: &str = "builder";

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
    redial_standing(&given.book, &given.providers, &store, &to_dial);
    let eases = given.eases.clone();
    // Asked before any task is spawned, so a store that can't tell its
    // changes leaves nothing running.
    let mut changes = store.changes()?;
    let (shared, kept) = Shared::of(
        store.clone(),
        (membership.hello(), socket.path.clone()),
        given,
        to_dial,
    );
    let seeing_fences = tokio::spawn(shared.clone().see_fences());
    let signalling = tokio::spawn(shared.clone().signal_receptors());
    let easing = eases.map(|eases| tokio::spawn(shared.clone().ease(eases)));
    let Kept {
        sharing,
        refusals,
        mut awake,
        receptors: keeping_receptors,
    } = kept;
    let (_watching_held, mut held) = watch(&shared.held, "what is held");
    let (_watching_parks, mut parks) = watch(&shared.parked, "what is parked");
    // The quests' record says whose sessions' ends are told, and to whom.
    let mut quests_told = shared.served.told("quest");
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
    // A word of this machine's own health moving is said at once rather
    // than with the next minute's measure.
    let mut health = shared.health.subscribe();
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
                    // The provider said what it made, so the machine's
                    // own word for its name isn't taken.
                    named: Arc::new(std::sync::OnceLock::from(made.machine.clone())),
                });
                let carries = (sharing.clone(), shared.starting(&dial, &socket.path), refusals.clone());
                let linked = keep_linked(dial.clone(), store.clone(), membership.clone(), carries);
                let keeping = Keeping { book: shared.book.clone(), store: store.clone() };
                links.spawn(keep_made(dial, made, keeping, unwanted, linked));
            }
            Some(()) = held.recv() => {
                shared.read_sessions();
                shared.keep_awake(&mut awake, power.as_ref());
            }
            // A park's marker says nothing of the session itself, so only
            // how it stands, which records read from the markers, moves.
            Some(()) = parks.recv() => shared.said.sessions_moved(),
            // The slot lives as long as the node, so it never closes.
            Ok(()) = health.changed() => shared.keep_health(health.borrow_and_update().clone()),
            // A holder that ended without letting its session go says
            // nothing, so what is held is looked at each minute too.
            _ = leaving.tick() => power = shared.each_minute(&mut awake),
            () = changed(&mut quests_told) => shared.said.ended_moved(),
            change = changes.changes.recv() => match change {
                Some(Change::Message(id)) => shared.read_message(&id),
                Some(Change::Presence) => {
                    shared.read_sessions();
                    // A session seen arriving where it was carried may
                    // let the trees it left here go.
                    shared.settle_moved();
                    // A held session going idle, or busy, may let it sleep.
                    shared.keep_awake(&mut awake, power.as_ref());
                }
                Some(Change::Rescan) => {
                    shared.read_store();
                    shared.said.machines_moved();
                }
                Some(Change::Machine(_)) => shared.said.machines_moved(),
                Some(Change::Carries(_)) => shared.said.sessions_moved(),
                None => break Err(io::Error::other("the store stopped telling its changes")),
            },
        }
    };
    // A link stopped mid-step can't say its sessions are out of reach, so
    // once every one has stopped and given up its claim, the node says so
    // for them.
    let stopping = std::time::Instant::now();
    seeing_fences.abort();
    signalling.abort();
    // Neither the easing nor the keeping of receptors outlives the node.
    for task in [&easing, &keeping_receptors].into_iter().flatten() {
        task.abort();
    }
    links.shutdown().await;
    let ended = stopping.elapsed();
    let letting_go = std::time::Instant::now();
    store::out_of_reach_of_unclaimed(&store)?;
    drop(socket);
    // A stop's time is the service manager's to bound, so each is said.
    eprintln!(
        "stopped: its links ended in {} ms, and what they carried was let go in {} ms more",
        ended.as_millis(),
        letting_go.elapsed().as_millis(),
    );
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

/// Where a machine made from an offer is kept: the book of what was made
/// and let go, and the store of the realm's machines it leaves once let
/// go.
struct Keeping<S> {
    book: Book,
    store: S,
}

/// Tells `to_dial` of each machine `book` says this node made before it
/// started and hasn't let go, so it links each, as it would one it made
/// now. Nothing ever finds one of these unwanted, so it is let go at its
/// time or once its work hands over. One whose time passed while the node
/// was down was ended by its provider, so it is booked as let go and
/// leaves the realm's machines. Its provider is the one of `providers`
/// named the same, or else the same program built again into the Nix
/// store, as an upgrade moves its path; one whose provider the node no
/// longer asks is left to end at its time, since the node runs only the
/// programs it was given.
fn redial_standing(
    book: &Book,
    providers: &[String],
    store: &impl Store,
    to_dial: &mpsc::UnboundedSender<(Made, Arc<Notify>)>,
) {
    let standing = match book.standing() {
        Ok(standing) => standing,
        Err(error) => {
            eprintln!("the book of machines made can't be read, so none is linked again: {error}");
            return;
        }
    };
    if standing.unread > 0 {
        eprintln!(
            "{} lines of the book of machines made can't be read, so what they kept isn't linked again",
            standing.unread
        );
    }
    if standing.unnamed > 0 {
        eprintln!(
            "{} machines were kept in the book before makes named their realm, so none is linked again; each ends at its time",
            standing.unnamed
        );
    }
    let now = jiff::Timestamp::now();
    for mut made in standing.made {
        if made.until <= now {
            let why = "its time was up while its node was down";
            eprintln!("{} was let go: {why}", made.machine);
            store::left(store, &made.machine, now);
            if let Err(error) = book.released(&made, why) {
                eprintln!(
                    "{} was let go, but the book doesn't say so: {error}",
                    made.machine
                );
            }
        } else if let Some(asked) = still_asked(providers, &made.provider) {
            eprintln!(
                "{} was made before this node started, so it is linked again",
                made.machine
            );
            made.provider.clone_from(asked);
            // The loop that reads it hasn't started, so it holds the sender.
            // Nothing is told on this Notify: no start waits on a machine
            // made before the node did, so none finds it unwanted, and it is
            // let go at its time or once its work hands over.
            let _ = to_dial.send((made, Arc::new(Notify::new())));
        } else {
            eprintln!(
                "{} was made by {}, which this node no longer asks, so it ends at its time",
                made.machine, made.provider
            );
        }
    }
}

/// The one of `providers` that is `provider`, or else the only one that is
/// it built again into the Nix store: the same path past the store's hash.
fn still_asked<'a>(providers: &'a [String], provider: &str) -> Option<&'a String> {
    if let Some(same) = providers.iter().find(|one| *one == provider) {
        return Some(same);
    }
    let built = past_the_hash(provider)?;
    let mut again = providers
        .iter()
        .filter(|one| past_the_hash(one) == Some(built));
    match (again.next(), again.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// What follows the hash in a path in the Nix store, such as
/// `lend-1.0/bin/lend`.
fn past_the_hash(path: &str) -> Option<&str> {
    let (hash, rest) = path.strip_prefix("/nix/store/")?.split_at_checked(32)?;
    let rest = rest.strip_prefix('-')?;
    hash.bytes()
        .all(|byte| byte.is_ascii_alphanumeric())
        .then_some(rest)
}

/// Keeps a machine made from an offer `linked` until it is done with:
/// until its work has handed over and it was released, it isn't wanted
/// once made, or its time is up, when it is released. Keeps in the book
/// that it was let go, and why, and has it leave the realm's machines at
/// once, so nothing lists a machine that is gone.
async fn keep_made(
    dial: Arc<Program>,
    made: Made,
    Keeping { book, store }: Keeping<impl Store>,
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
    store::left(&store, &made.machine, jiff::Timestamp::now());
    if let Err(error) = book.released(&made, &why) {
        eprintln!(
            "{} was let go, but the book doesn't say so: {error}",
            made.machine
        );
    }
}

/// Who a provider is told asks for a machine ([`made::ASKER`]), and why
/// it is told [`made::UNKNOWN`] where it is.
struct Asker {
    told: String,
    unknown_because: Option<String>,
}

/// `said` as a provider is told a mesh address, in capitals as the node
/// reads addresses: letters and digits, at most [`ADDRESS_MOST`], and
/// never [`made::PERSON`] or [`made::UNKNOWN`] whatever its case; else
/// nothing.
fn address_told(said: &str) -> Option<String> {
    let address = said.to_ascii_uppercase();
    let reads = !address.is_empty()
        && address.len() <= ADDRESS_MOST
        && address
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        && !address.eq_ignore_ascii_case(made::PERSON)
        && !address.eq_ignore_ascii_case(made::UNKNOWN);
    reads.then_some(address)
}

/// The longest a mesh address is read as, which a quest's six letters and
/// a session's eight hex digits fit.
const ADDRESS_MOST: usize = 16;

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
    /// What keeps the receptors, where the node has a folder of them,
    /// ended as the node stops.
    receptors: Option<tokio::task::JoinHandle<()>>,
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
    /// Where the machine tells who runs its processes, which says who asks
    /// to steer.
    processes: Arc<Processes>,
    /// The process at the other end of the connection served, as the
    /// kernel says, or none for the node's own loop and one it won't name.
    client: Option<i32>,
    /// Where it keeps each carry an orchestrator ordered.
    carries: PathBuf,
    /// What lets go of the trees a carry ordered from here left, once it
    /// has arrived, if anything does.
    settles: Option<Arc<Settler>>,
    /// The quests' scratch folders, where each machine's sessions hand
    /// over as they leave or refuse, or none where it shares no quests.
    scratch: Option<PathBuf>,
    /// This machine's hostname, read once at start, which names its
    /// sessions' handover folder as the harness names it.
    hostname: Arc<str>,
    /// The refusals in that handover that couldn't be read, each said on
    /// the node's stderr once.
    unreadable: Arc<through::Unreadable>,
    /// Where it keeps each command it runs, and who may say how a fenced
    /// one runs.
    runs: Arc<run::Given>,
    /// Where pi's hosts keep their records of the sessions they hold.
    hosts: PathBuf,
    /// Where it keeps who orchestrates which quests.
    orchestrations: PathBuf,
    /// Where pi keeps each session's log.
    logs: PathBuf,
    /// What the person said to sessions through it.
    words: Arc<words::Book>,
    /// What their talks said when last read.
    talks: Arc<words::Talks>,
    /// Where this machine's configuration declares it.
    declared: PathBuf,
    /// Where it keeps each placement it makes, if it does.
    placements: Option<Arc<placements::Log>>,
    /// The strictest fence each session here was seen to run in.
    fences: Arc<fences::Seen>,
    /// The tree networks formed, kept across restarts.
    formed: Arc<Formed>,
    /// This node's own socket, which a carry the person asks of it runs
    /// its steps through, as `mycelium carry` does.
    node: PathBuf,
    /// Where this machine keeps quests, which a carry the person asks of
    /// it shares, if it shares any.
    shares: Option<Arc<Places>>,
    /// Held while the record of orchestrations is read and kept again, so
    /// two claims at once never undo each other.
    claiming: Arc<Mutex<()>>,
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
    /// The builders this machine sends Nix builds to, kept heard.
    builders: Arc<Builders>,
    /// What this machine's cells murmured in the quests' webs, kept read.
    murmurs: Arc<Murmurs>,
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
    /// This machine's own health, as its health daemon last said it, which
    /// each measure and each link it keeps carries (`own_health`).
    health: own_health::Slot,
    /// The records its watches read, built once for each change and shared
    /// by every connection's.
    built: Arc<daemon::Built>,
    /// Its receptors, as last judged.
    receptors: receptor::Held,
    /// The person's table of which receptors' climbs are said
    /// (`stimuli`), if one is named.
    stimuli: Option<PathBuf>,
    /// The ids of the signals this node said as its receptors this run,
    /// the only messages from a receptor's name it takes (`signals`).
    spoken: Arc<Mutex<HashSet<String>>>,
    /// The ids of messages from a receptor's name it didn't say, refused
    /// once and never opened again when the store is read whole. It is
    /// kept for the node's life and never trimmed: it grows by one id for
    /// each such file the store has held this run, which is this node's
    /// own signals from earlier runs, still in the store until it lets
    /// them go, and whatever something writing the account's own files
    /// planted. Nothing else can add one, and it empties when the node
    /// starts again.
    refused: Arc<Mutex<HashSet<String>>>,
    /// Its cells' forages, as last read from the quest store.
    forages: forage::Held,
    /// Asks for a quest a cell works on to be read, when it hasn't been;
    /// none when the node has no quest store.
    to_read: Option<mpsc::UnboundedSender<String>>,
}

/// What changes each time the node's sessions or the realm's machines may
/// have, for the daemon interface's watches.
struct Said {
    sessions: watch::Sender<u64>,
    machines: watch::Sender<u64>,
    messages: watch::Sender<u64>,
    receptors: watch::Sender<u64>,
    forages: watch::Sender<u64>,
    /// Told as any of the sessions, the machines or the receptors are,
    /// whose records the cells are made of.
    cells: watch::Sender<u64>,
    /// Told as the messages are, whose ties they make, and each minute,
    /// as a message leaves a tie's hour or its day.
    ties: watch::Sender<u64>,
    /// Told as the sessions or the quests are, whose ends it tells, and
    /// each minute, as an end settles or leaves its day.
    ended: watch::Sender<u64>,
}

impl Default for Said {
    fn default() -> Self {
        Self {
            sessions: watch::Sender::new(0),
            machines: watch::Sender::new(0),
            messages: watch::Sender::new(0),
            receptors: watch::Sender::new(0),
            forages: watch::Sender::new(0),
            cells: watch::Sender::new(0),
            ties: watch::Sender::new(0),
            ended: watch::Sender::new(0),
        }
    }
}

impl Said {
    /// Tells the watches of the sessions, and of the cells, that the
    /// sessions may have changed.
    fn sessions_moved(&self) {
        self.sessions.send_modify(|said| *said += 1);
        self.cells.send_modify(|said| *said += 1);
        self.ended_moved();
    }

    /// Tells the watches of the ended sessions that what they are made of
    /// may have changed.
    fn ended_moved(&self) {
        self.ended.send_modify(|said| *said += 1);
    }

    /// Tells the watches of the machines, and of the cells, that the
    /// machines may have changed.
    fn machines_moved(&self) {
        self.machines.send_modify(|said| *said += 1);
        self.cells.send_modify(|said| *said += 1);
    }

    /// Tells the watches of the messages, and of the ties, that a message
    /// came.
    fn messages_moved(&self) {
        self.messages.send_modify(|said| *said += 1);
        self.ties.send_modify(|said| *said += 1);
    }

    /// Tells the watches of the ties that a minute passed, in which a
    /// message may have left a tie's hour or its day.
    fn minute_passed(&self) {
        self.ties.send_modify(|said| *said += 1);
        self.ended_moved();
    }

    /// Tells the watches of the receptors, and of the cells, that the
    /// receptors changed.
    fn receptors_moved(&self) {
        self.receptors.send_modify(|said| *said += 1);
        self.cells.send_modify(|said| *said += 1);
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
            processes: self.processes.clone(),
            client: self.client,
            carries: self.carries.clone(),
            settles: self.settles.clone(),
            scratch: self.scratch.clone(),
            hostname: Arc::clone(&self.hostname),
            unreadable: Arc::clone(&self.unreadable),
            runs: self.runs.clone(),
            hosts: self.hosts.clone(),
            orchestrations: self.orchestrations.clone(),
            logs: self.logs.clone(),
            words: self.words.clone(),
            talks: self.talks.clone(),
            declared: self.declared.clone(),
            placements: self.placements.clone(),
            fences: self.fences.clone(),
            formed: self.formed.clone(),
            node: self.node.clone(),
            shares: self.shares.clone(),
            claiming: self.claiming.clone(),
            updater: self.updater.clone(),
            starter: self.starter.clone(),
            asks: self.asks.clone(),
            inner: self.inner.clone(),
            said: self.said.clone(),
            served: self.served.clone(),
            providers: self.providers.clone(),
            builders: self.builders.clone(),
            murmurs: self.murmurs.clone(),
            book: self.book.clone(),
            to_dial: self.to_dial.clone(),
            flows: self.flows.clone(),
            measured: self.measured.clone(),
            health: self.health.clone(),
            built: self.built.clone(),
            receptors: self.receptors.clone(),
            stimuli: self.stimuli.clone(),
            spoken: Arc::clone(&self.spoken),
            refused: Arc::clone(&self.refused),
            forages: self.forages.clone(),
            to_read: self.to_read.clone(),
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
        let [
            sessions,
            machines,
            messages,
            teams,
            networks,
            cells,
            open,
            words,
            events,
            receptors,
            builders,
            carries,
            forages,
            murmurs,
            ties,
            formed,
            ended,
        ] = HOLDS.map(str::to_owned);
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
                    "doing",
                    "events",
                    "working_on",
                    "presence",
                    "standing",
                    "told",
                    "fence_shown",
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
            // A message is said once and never changes, nor does an event.
            (messages, Vec::new()),
            (events, Vec::new()),
            (teams, vec!["title", "worst", "counts", "members", "landed"]),
            (
                networks,
                vec!["title", "glyph", "within", "cells", "machines", "counts"],
            ),
            (cells, cell::LIVE.to_vec()),
            // A hold says nothing more until it ends, when it is gone.
            (open, vec!["waiting_on"]),
            (words, vec!["taken_up", "answered", "failed"]),
            (receptors, vec!["state", "read_at", "counts"]),
            (builders, vec!["state", "said", "heard_at"]),
            // What was kept of a carry never changes; how it stands does.
            (carries, vec!["state", "refused"]),
            (
                forages,
                vec!["purpose", "status", "documents", "waits", "lineage"],
            ),
            (murmurs, vec!["text", "tied_to", "purpose", "settled"]),
            (ties, tie::LIVE.to_vec()),
            (formed, vec!["left"]),
            (ended, ended::LIVE.to_vec()),
        ])
        .into_iter()
        .chain(served)
        .collect()
    }

    // A session's presence is most of its record, and its time moves at
    // every beat, so said whole it went again each time: of Joel's 80
    // sessions' changes held back over 90 s on 2026-11-15, 128 KB of
    // 207 KB was presence that hadn't moved.
    fn deep(&self, kind: &str) -> Vec<&'static str> {
        if kind == HOLDS[0] {
            vec!["presence"]
        } else {
            Vec::new()
        }
    }

    // A page reads the latest murmurs first, and most read only those.
    fn newest_first(&self, kind: &str) -> bool {
        kind == murmur::KIND
    }

    fn records(&self, kind: &str) -> Option<BTreeMap<String, daemon::Record>> {
        let made = match kind {
            network::KIND => return Some(self.network_records()),
            formed::KIND => {
                // The making is what forms a network, so it runs first.
                self.network_records();
                let now = jiff::Timestamp::now().as_millisecond();
                return Some(self.formed.records(now));
            }
            cell::KIND => return Some(self.cell_records()),
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
            "machine" => Some(self.machine_records()),
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
            team::KIND => Some(self.team_records()),
            unanswered::KIND => Some(self.unanswered_records()),
            words::KIND => Some(self.word_records()),
            EVENT => Some(self.event_records()),
            receptor::KIND => Some(self.receptor_records()),
            BUILDER => Some(self.builder_records()),
            kept::KIND => Some(self.carry_records()),
            forage::KIND => Some(self.forage_records()),
            murmur::KIND => Some(self.murmur_records()),
            tie::KIND => return Some(self.tie_records()),
            ended::KIND => Some(self.ended_records()),
            _ => self.served.records(kind),
        }?;
        // Each held as its bytes as soon as it is made, so the kind is never
        // held whole twice.
        Some(
            made.into_iter()
                .map(|(key, record)| (key, daemon::Record::from(record)))
                .collect(),
        )
    }

    fn told(&self, kind: &str) -> Option<watch::Receiver<u64>> {
        match kind {
            // A team changes only as its sessions do; a quest's new title
            // shows with the next change to any. A network also holds the
            // machines, which come and go far more seldom than sessions
            // change, so a machine come or gone shows with the next.
            // An event comes with its session's presence.
            "session" | team::KIND | network::KIND | EVENT => Some(self.said.sessions.subscribe()),
            // A network forms, is left and fades as a making sees it,
            // which the minute's pass makes too, so one fading on time
            // alone is told.
            formed::KIND => Some(self.formed.told()),
            "machine" => Some(self.said.machines.subscribe()),
            // A cell is a session's, a machine's or a receptor's, so is
            // told as any of them changes, and a machine's shows at once.
            cell::KIND => Some(self.said.cells.subscribe()),
            // What waits changes as messages come, and a peer's addresses,
            // which change far more seldom, show with the next message,
            // as does a question or hold past its two hours.
            "message" | unanswered::KIND => Some(self.said.messages.subscribe()),
            // The person's words are taken up and answered as their
            // session takes its turn on them, which its presence says, and
            // a word kept tells its watchers too. A carry stands as its
            // session is seen, so it changes as sessions do, and a carry
            // kept tells its watchers too. A refusal comes as its session
            // ends the turn that wrote it, which its presence says.
            words::KIND | kept::KIND => Some(self.said.sessions.subscribe()),
            // An end is told as the sessions or the quests change, and as
            // a minute settles it or its day ends.
            ended::KIND => Some(self.said.ended.subscribe()),
            receptor::KIND => Some(self.said.receptors.subscribe()),
            BUILDER => Some(self.builders.told()),
            forage::KIND => Some(self.said.forages.subscribe()),
            // A cell's purpose shows with its next murmur.
            murmur::KIND => Some(self.murmurs.told()),
            // A tie moves as its messages come, and as one leaves its hour
            // or its day, which the minute's tick tells.
            tie::KIND => Some(self.said.ties.subscribe()),
            _ => self.served.told(kind),
        }
    }

    fn built(&self) -> Option<&daemon::Built> {
        Some(&self.built)
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
                    // Why its last link was refused, as for another version,
                    // says what brings it back, which its last word doesn't.
                    "why": self.store.link_refused(&machine.name).unwrap_or_else(|| format!(
                        "no link reaches it; it last said itself at {}",
                        machine.measured_at
                    )),
                }),
            })
            .chain(self.served.health())
            .chain(self.builders.health())
            .chain(self.murmurs.health())
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
            Response::Steered { machine, .. } => Ok(machine),
            other => Err(format!(
                "its machine answered {other:?}, not that it was steered"
            )),
        }
    }

    async fn update(&self, machine: &str) -> Result<serde_json::Value, String> {
        match Shared::update(self, machine).await? {
            Response::Updated(ending) => Ok(ending),
            other => Err(format!(
                "its machine answered {other:?}, not how its update ended"
            )),
        }
    }

    async fn carry(&self, quest: &str, to: &str) -> Result<serde_json::Value, String> {
        match Shared::carry_back(self, quest, to).await? {
            Response::Carried(carried) => Ok(carried),
            other => Err(format!(
                "its machine answered {other:?}, not where its sessions went"
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
    /// The held sessions waiting to come back, and those on their first
    /// turn, kept only while the node runs (see [`turns`]).
    turns: Turns,
    /// What the node has seen of sessions ending here, kept only while it
    /// runs (see [`ended`]).
    ended: ended::Seen,
    /// The starts placed lately, each load on its machine until its room
    /// shows it, kept only while the node runs.
    starts: placement::Starts,
}

/// Watches a folder of records, of what is held or what is parked, or,
/// when it can't be watched, says so: `what` it holds is then read again
/// only as a presence changes, so its change waits on the next one.
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
            eprintln!("{what} isn't watched, so is read again only as a presence changes: {error}");
            (None, mpsc::unbounded_channel().1)
        }
    }
}

/// The machines of those of `sessions` that weigh on their machine beyond
/// what its room already shows, one for each: only those busy, since an
/// idle one costs memory its room already counts as taken.
fn running(sessions: &[Session]) -> impl Iterator<Item = &str> {
    sessions
        .iter()
        .filter(|one| one.state == SessionState::Busy)
        .map(|one| one.machine.as_str())
}

/// Keeps `forages` true to the quests in `places` for the cells of
/// `machine` that `inner` holds, telling `said` as they change, and gives
/// what asks for a quest to be read.
fn keep_forages(
    places: &Places,
    forages: &forage::Held,
    said: &Said,
    inner: &Arc<Mutex<Inner>>,
    machine: &str,
) -> mpsc::UnboundedSender<String> {
    let (to_read, wanted) = mpsc::unbounded_channel();
    let (state, machine) = (Arc::clone(inner), machine.to_owned());
    tokio::spawn(forage::keep(
        places.quests.clone(),
        Arc::clone(forages),
        wanted,
        said.sessions.subscribe(),
        move || {
            forage_cells(
                &state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                &machine,
            )
        },
        said.forages.clone(),
    ));
    to_read
}

/// The cells of `machine` that work on a quest, each with what its forage
/// is made of, ordered by address.
fn forage_cells(inner: &Inner, machine: &str) -> Vec<forage::Cell> {
    let mut cells: Vec<forage::Cell> = inner
        .state
        .sessions()
        .into_iter()
        .filter(|session| session.machine == machine)
        .filter_map(|session| {
            Some(forage::Cell {
                quest: session.quest?,
                address: session.address,
                session: session.id,
                started_by: session.orchestrated_by,
            })
        })
        .collect();
    cells.sort_by(|one, other| one.address.cmp(&other.address));
    cells
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

/// The receptors the node judges in `folder`, if it has one, kept as each
/// changes and telling `said` so.
fn keep_receptors(
    folder: Option<PathBuf>,
    realm: &str,
    said: &Arc<Said>,
) -> (receptor::Held, Option<tokio::task::JoinHandle<()>>) {
    let held = receptor::Held::default();
    let keeping = folder.map(|folder| {
        let telling = Arc::clone(said);
        tokio::spawn(receptor::keep(
            folder,
            realm.to_owned(),
            Arc::clone(&held),
            move || telling.receptors_moved(),
        ))
    });
    (held, keeping)
}

impl<S: Store> Shared<S> {
    /// What a node of `hello`'s machine and realm shares, from what it was
    /// `given`, telling `to_dial` of each machine it makes, with what its
    /// loop keeps apart.
    fn of(
        store: S,
        (hello, node): (Hello, PathBuf),
        given: Given,
        to_dial: mpsc::UnboundedSender<(Made, Arc<Notify>)>,
    ) -> (Self, Kept) {
        let Given {
            sharing,
            starter,
            refusals,
            steers,
            waiting,
            awake,
            updater,
            serves,
            providers,
            book,
            flows,
            parked,
            held: held_here,
            processes,
            carries,
            runs,
            hosts,
            orchestrations,
            logs,
            words,
            declared,
            receptors,
            stimuli,
            builders,
            placements,
            health,
            scratch,
            eases: _,
            settles,
        } = given;
        let own = own_health::Slot::new(None);
        if let Some(socket) = health {
            tokio::spawn(own_health::keep(socket, own.clone()));
        }
        let said = Arc::<Said>::default();
        let inner = Arc::new(Mutex::new(Inner::after(&flows, &hello.realm)));
        let forages: forage::Held = Arc::new(Mutex::new(forage::Shelf::with_waits(waiting)));
        let to_read = sharing
            .as_ref()
            .map(|places| keep_forages(places, &forages, &said, &inner, &hello.machine));
        let (held, keeping_receptors) = keep_receptors(receptors, &hello.realm, &said);
        let shared = Self {
            processes: processes.into(),
            // Beside the carries it judges, so it needs no place of its own.
            fences: Arc::new(fences::Seen::at(carries.with_file_name("fences.json"))),
            // Beside the carries too, its realm's own in the machine's state.
            formed: Arc::new(Formed::beside(&carries, &hello.realm)),
            carries,
            settles: settles.map(|settles| Arc::new(Settler::new(settles))),
            scratch: sharing.as_ref().map(|places| places.scratch.clone()),
            hostname: rustix::system::uname().nodename().to_string_lossy().into(),
            unreadable: Arc::default(),
            runs: Arc::new(runs),
            hosts,
            orchestrations,
            logs,
            words: Arc::new(words),
            talks: Arc::default(),
            declared,
            placements: placements.map(Arc::new),
            node,
            shares: sharing.clone().map(Arc::new),
            claiming: Arc::default(),
            client: None,
            served: Arc::new(Served::start(serves)),
            builders: Arc::new(Builders::start(builders)),
            murmurs: Arc::new(Murmurs::start(scratch, hello.machine.clone())),
            providers: providers.into(),
            book,
            to_dial,
            held: held_here,
            parked,
            boot: Arc::new(standing::Boot::this()),
            steers,
            updater,
            store,
            inner,
            machine: hello.machine,
            realm: hello.realm,
            starter: starter.map(Arc::new),
            asks: Asks::default(),
            said,
            flows: Arc::new(flows),
            measured: Arc::default(),
            health: own,
            built: Arc::default(),
            receptors: held,
            stimuli,
            spoken: Arc::default(),
            refused: Arc::default(),
            forages,
            to_read,
        };
        let kept = Kept {
            sharing,
            refusals,
            awake,
            receptors: keeping_receptors,
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
            let refused = self.refused_ids();
            let inner = self.lock();
            ids.into_iter()
                .filter(|id| !inner.state.holds_message(id) && !refused.contains(id))
                .collect()
        };
        let (waiting, messages): (Vec<Message>, Vec<Message>) = unread
            .iter()
            .filter_map(|id| mesh::message(&self.store.message(id)?))
            .filter(|message| self.takes(message))
            .partition(|message| message.for_realm.is_some());
        self.lock().state.add_messages(messages);
        self.wait_to_cross(waiting);
        self.said.messages_moved();
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
            self.said.sessions_moved();
        }
        for event in events {
            inner.send_all(&event);
        }
    }

    /// The questions waiting and holds standing on the mesh as the daemon
    /// interface holds them, a peer going by each address its sessions'
    /// presences give it.
    fn unanswered_records(&self) -> BTreeMap<String, serde_json::Value> {
        let now = jiff::Timestamp::now().as_millisecond();
        let inner = self.lock();
        let mut aliases: HashMap<String, Vec<String>> = HashMap::new();
        for session in inner.state.sessions() {
            let mut addresses: Vec<String> = session.presence["addresses"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_ascii_uppercase)
                .collect();
            addresses.push(session.address.to_ascii_uppercase());
            for address in &addresses {
                aliases
                    .entry(address.clone())
                    .or_default()
                    .extend(addresses.iter().cloned());
            }
        }
        let open = inner.state.unanswered(
            |address| {
                aliases
                    .get(&address.to_ascii_uppercase())
                    .cloned()
                    .unwrap_or_default()
            },
            now,
        );
        drop(inner);
        open.iter()
            .map(|open| {
                let (id, fields) = open.fields();
                let heard = millis_at(&open.message.at);
                let record = daemon::record(unanswered::KIND, &id, heard, fields);
                (daemon::key(unanswered::KIND, &id), record)
            })
            .collect()
    }

    /// The carries this node ordered over the last day, as the daemon
    /// interface holds them, each standing as `kept::judged` says from
    /// what is seen of its session: where it runs now, by the node's
    /// state, and what it wrote in its quest's handover here since.
    /// Nothing is shown when the file can't be read, since a carry's
    /// line is never guessed at.
    fn carry_records(&self) -> BTreeMap<String, serde_json::Value> {
        let since = shown_since();
        let runs_on = self.runs_on();
        let here = self.carries_here(since).into_iter().map(|carry| {
            let seen = self.handed(&carry);
            (self.machine.clone(), carry, seen)
        });
        // Another machine's node tells of the carries it ordered, which
        // are shown for the person as told, and never taken as orders.
        let heard = self
            .store
            .told_carries()
            .into_iter()
            .filter(|(machine, _)| *machine != self.machine && self.linked_since(machine, since))
            .flat_map(|(machine, told)| {
                told.iter()
                    .filter_map(kept::heard)
                    .filter(|(carry, _)| kept::shown(carry, since))
                    .map(|(carry, seen)| (machine.clone(), carry, seen))
                    .collect::<Vec<_>>()
            });
        here.chain(heard)
            .map(|(machine, carry, mut seen)| {
                seen.runs_on = runs_on.get(carry.said("session")).cloned();
                let mut fields = kept::fields(&carry, &kept::judged(&carry, &seen));
                if let Some(fields) = fields.as_object_mut() {
                    fields.insert("machine".to_owned(), serde_json::Value::from(&*machine));
                }
                // Two machines' nodes may keep carries of the same session
                // at the same moment, so the machine names each.
                let id = format!("{machine}:{}", carry.id);
                let record = daemon::record(kept::KIND, &id, carry.at.as_millisecond(), fields);
                (daemon::key(kept::KIND, &id), record)
            })
            .collect()
    }

    /// The carries this node ordered that are shown at `since`
    /// (`kept::shown`), newest first.
    fn carries_here(&self, since: jiff::Timestamp) -> Vec<kept::Kept> {
        let settled = match self.settles.as_ref().map(|settler| settler.settled()) {
            Some(Ok(settled)) => settled,
            Some(Err(error)) => {
                let error = format!("what settling carries came to can't be read: {error}");
                if self.unreadable.first(&error) {
                    eprintln!("{error}");
                }
                HashMap::new()
            }
            None => HashMap::new(),
        };
        self.carries_looked_at()
            .into_iter()
            .map(|mut carry| {
                if let (Some(said), Some(line)) =
                    (settled.get(&carry.id), carry.line.as_object_mut())
                {
                    line.insert("settle".to_owned(), said.clone());
                }
                carry
            })
            .filter(|carry| kept::shown(carry, since))
            .collect()
    }

    /// The carries this node ordered as far back as settling looks
    /// (`settle::LOOKED_FOR`), newest first.
    fn carries_looked_at(&self) -> Vec<kept::Kept> {
        let now = jiff::Timestamp::now();
        let since = now.checked_sub(settle::LOOKED_FOR).unwrap_or(now);
        kept::read(&self.carries, since).unwrap_or_default()
    }

    /// Settles each carry ordered from here that is due (`settle::due`),
    /// off the node's loop, and only one whose settling came to nothing
    /// kept yet, each session's newest carry alone (`settle::newest_each`).
    /// Where what settling came to can't be read, none is, since it can't
    /// be told which were. It watches where the sessions of the rest run,
    /// for `settle_moved`.
    fn settle_carries(&self) {
        let Some(settler) = &self.settles else {
            return;
        };
        let Ok(settled) = settler.settled() else {
            return;
        };
        let now = jiff::Timestamp::now();
        let runs_on = self.runs_on();
        let mut places = HashMap::new();
        for carry in settle::to_look_at(
            self.carries_looked_at(),
            |id| settled.contains_key(id),
            &self.machine,
        ) {
            let session = carry.said("session");
            let mut seen = self.handed(&carry);
            seen.runs_on = runs_on.get(session).cloned();
            places.insert(session.to_owned(), seen.runs_on.clone());
            let state = settler.stands(&carry, kept::judged(&carry, &seen));
            if settle::due(&carry, &state, &self.machine, now) {
                let said = Arc::clone(&self.said);
                settler.settle(&carry, move || said.sessions_moved());
            }
        }
        settler.watch(places);
    }

    /// Settles as `settle_carries` does, but only once a session of a
    /// carry not settled yet runs elsewhere than when last looked at,
    /// since presences change far more often than carries arrive.
    fn settle_moved(&self) {
        if self
            .settles
            .as_ref()
            .is_some_and(|settler| settler.moved(&self.runs_on()))
        {
            self.settle_carries();
        }
    }

    /// What only this machine's handover shows of `carry`: whether its
    /// session refused, saying why, and whether it left.
    fn handed(&self, carry: &kept::Kept) -> kept::Seen {
        let session = carry.said("session");
        let Some(handover) = self
            .scratch
            .as_ref()
            .map(|scratch| through::handover(scratch, carry.said("quest"), &self.hostname))
        else {
            return kept::Seen::default();
        };
        kept::Seen {
            refused: through::refused(&handover, session, carry.at, |error| {
                if self.unreadable.first(&error) {
                    eprintln!("a carry's refusal couldn't be read: {error}");
                }
            }),
            left: through::left(&handover, session, carry.at),
            runs_on: None,
        }
    }

    /// Whether a link carried `machine` here since `since`.
    fn linked_since(&self, machine: &str, since: jiff::Timestamp) -> bool {
        self.store
            .linked_until(machine)
            .and_then(|at| jiff::Timestamp::from_millisecond(at).ok())
            .is_some_and(|at| at >= since)
    }

    /// Keeps the carries this node ordered within the last day as its
    /// links tell them, when they moved, and lets go of those another
    /// machine told of once no link has carried it for a day.
    ///
    /// It reads each carry's handover files every time, which is cheap at
    /// the carries of one day and a minute between, but not at a second
    /// between: run more often, it would want to hear the handover's
    /// changes instead.
    fn tell_carries(&self) {
        let since = shown_since();
        let told = kept::bounded(
            self.carries_here(since)
                .iter()
                .map(|carry| {
                    let seen = self.handed(carry);
                    kept::told(carry, seen.refused.as_deref(), seen.left)
                })
                .collect(),
        );
        let heard = self.store.told_carries();
        let moved = heard
            .get(&self.machine)
            .map_or(!told.is_empty(), |was| *was != told);
        if moved && let Err(error) = self.store.put_told_carries(&self.machine, &told) {
            eprintln!("this machine's carries can't be told: {error}");
        }
        for machine in heard.keys() {
            if *machine != self.machine
                && !self.linked_since(machine, since)
                && let Err(error) = self.store.put_told_carries(machine, &[])
            {
                eprintln!("the carries {machine} told of can't be let go: {error}");
            }
        }
    }

    /// Where each session seen running, or parked, runs, by its id.
    fn runs_on(&self) -> HashMap<String, String> {
        let runs =
            |session: &Session| matches!(session.state, SessionState::Busy | SessionState::Idle);
        self.lock()
            .state
            .sessions()
            .into_iter()
            .filter(|session| runs(session) || parked_presence(session))
            .map(|session| {
                let id = session
                    .presence
                    .get("session")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&session.id)
                    .to_owned();
                (id, session.machine)
            })
            .collect()
    }

    /// The person's words as the daemon interface holds them, each marked
    /// as its talk's file here says it was taken up and answered.
    fn word_records(&self) -> BTreeMap<String, serde_json::Value> {
        let words = self.words.words();
        let talks: Vec<Option<PathBuf>> = words
            .iter()
            .map(|word| {
                let talk = word.talk.as_deref()?;
                steer::replies(&self.steers, &word.address, talk)
            })
            .collect();
        let marks = self.talks.marks(talks.iter().map(Option::as_deref));
        words
            .into_iter()
            .zip(marks)
            .map(|(word, marks)| {
                let heard = millis_at(&word.at);
                let record = daemon::record(words::KIND, &word.id, heard, word.fields(&marks));
                (daemon::key(words::KIND, &word.id), record)
            })
            .collect()
    }

    /// Keeps what the person said to `address`, on `machine`, as `steer`
    /// put it, and tells the watchers of their words.
    fn keep_word(&self, steer: &Steer, machine: &str) {
        let text = |field: &str| {
            steer
                .body
                .get(field)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        let talk = steer.talk().map(str::to_owned);
        let word = words::Word {
            id: talk.clone().unwrap_or_else(words::talk_id),
            at: jiff::Timestamp::now().to_string(),
            address: steer.address.clone(),
            machine: machine.to_owned(),
            text: text("text").unwrap_or_default(),
            act: text("act"),
            talk,
        };
        if let Err(error) = self.words.keep(word) {
            eprintln!(
                "the person's words to {} weren't kept: {error}",
                steer.address
            );
        }
        self.said.sessions_moved();
    }

    /// The sessions here that ended while their quest goes on, as
    /// [`ended`] says, each standing as it does here, against the quests
    /// this node serves; none where it serves none.
    fn ended_records(&self) -> BTreeMap<String, serde_json::Value> {
        let quests: Vec<serde_json::Value> = self
            .served
            .records("quest")
            .unwrap_or_default()
            .into_values()
            .collect();
        // Where the park markers can't be read, which park its holder
        // wakes can't be told, so none is, and that is said once.
        if let Err(error) = fs::read_dir(&self.parked)
            && error.kind() != io::ErrorKind::NotFound
        {
            let why = format!(
                "no end is told, as the park markers in {} can't be read: {error}",
                self.parked.display()
            );
            if self.unreadable.first(&why) {
                eprintln!("{why}");
            }
            return BTreeMap::new();
        }
        let parked = standing::parked(&self.parked);
        // Where the carries can't be read, which ended for one can't be
        // told, so none is.
        let carries = match kept::read(&self.carries, shown_since()) {
            Ok(carries) => carries,
            Err(error) => {
                let why = format!(
                    "no end is told, as the carries in {} can't be read: {error}",
                    self.carries.display()
                );
                if self.unreadable.first(&why) {
                    eprintln!("{why}");
                }
                return BTreeMap::new();
            }
        };
        let now = jiff::Timestamp::now().as_millisecond();
        let runs_on = self.runs_on();
        let judge = |carry: &kept::Kept| {
            let mut seen = self.handed(carry);
            seen.runs_on = runs_on.get(carry.said("session")).cloned();
            kept::judged(carry, &seen)
        };
        // Judged before the node's state is locked, as judging reads the
        // carries' handovers; the turns waiting are read with the
        // sessions, under one lock, so a session that asks a turn is never
        // told as ended for being read a moment apart.
        let away = ended::carried(&carries, judge, now);
        let standing = |session: &Session| {
            let here = session.machine == self.machine;
            standing::standing(session, here, &parked, &self.boot)
        };
        let mut inner = self.lock();
        let sessions = inner.state.sessions();
        let coming_back = ended::coming_back(away, inner.turns.waiting());
        let judged = ended::Judged {
            machine: &self.machine,
            standing: &standing,
            coming_back: &coming_back,
            settled_after: ENDS_SETTLE,
            written: &|at| {
                at.parse::<jiff::Timestamp>()
                    .ok()
                    .map(jiff::Timestamp::as_millisecond)
            },
        };
        ended::records(ended::ended(
            &sessions,
            &judged,
            &quests,
            now,
            &mut inner.ended,
        ))
    }

    /// The swarm's teams as the daemon interface holds them, titled and
    /// named by the quests it serves, where it serves them.
    fn team_records(&self) -> BTreeMap<String, serde_json::Value> {
        let quests: BTreeMap<String, serde_json::Value> = self
            .served
            .records("quest")
            .unwrap_or_default()
            .into_values()
            .filter_map(|quest| {
                let address = quest.get("address")?.as_str()?.to_owned();
                Some((address, quest))
            })
            .collect();
        let parked = standing::parked(&self.parked);
        let sessions = self.lock().state.sessions();
        team::teams(&sessions, &quests, &parked, millis_at)
            .into_iter()
            .map(|(id, heard, fields)| {
                let record = daemon::record(team::KIND, &id, heard, fields);
                (daemon::key(team::KIND, &id), record)
            })
            .collect()
    }

    /// Every builder this machine sends Nix builds to, as the daemon
    /// interface holds it, heard when it was last heard, or never.
    fn builder_records(&self) -> BTreeMap<String, serde_json::Value> {
        self.builders
            .builders()
            .into_iter()
            .map(|builder| {
                let heard = builder.heard_at.map_or(0, jiff::Timestamp::as_millisecond);
                let record = daemon::record(BUILDER, &builder.name, heard, json_of(&builder));
                (daemon::key(BUILDER, &builder.name), record)
            })
            .collect()
    }

    /// Every murmur this machine's cells said in the last day, as the
    /// daemon interface holds it, each with what its cell's session says
    /// it works on as its purpose.
    fn murmur_records(&self) -> BTreeMap<String, serde_json::Value> {
        let purposes = self.lock().state.purposes(&self.machine);
        self.murmurs
            .records(jiff::Timestamp::now().as_second(), &purposes)
    }

    /// Every machine of the realm as the daemon interface holds it, each
    /// saying whether it is this one.
    fn machine_records(&self) -> BTreeMap<String, serde_json::Value> {
        let kind = "machine";
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
            .collect()
    }

    /// The realm's networks as the daemon interface holds them, as the
    /// networks view makes them of the node's teams, sessions, machines,
    /// receptors and builders.
    fn network_records(&self) -> BTreeMap<String, daemon::Record> {
        self.made_records(network::records)
    }

    /// The realm's cells as the daemon interface holds them, as the
    /// networks view makes them of the same records as its networks.
    fn cell_records(&self) -> BTreeMap<String, daemon::Record> {
        self.made_records(cell::records)
    }

    /// The ties between the realm's cells, and each cell's talk, as the
    /// messages held say of the last day, taken shared while the node is
    /// locked and folded once it is let go, since a fold of a busy day
    /// takes milliseconds that every other reader would wait out.
    fn tie_records(&self) -> BTreeMap<String, daemon::Record> {
        let now = jiff::Timestamp::now().as_millisecond();
        let (aliases, messages) = {
            let inner = self.lock();
            let aliases = tie::aliases(inner.state.each_session());
            (aliases, inner.state.shared_messages())
        };
        tie::records(talk(messages.iter().map(AsRef::as_ref), &aliases, now))
    }

    /// The records `making` makes of what the node holds, as the networks
    /// view reads it, each session standing as it does here.
    fn made_records(
        &self,
        making: fn(&Holding<'_>, &Standing<'_>) -> BTreeMap<String, daemon::Record>,
    ) -> BTreeMap<String, daemon::Record> {
        let teams = self.team_records();
        let machines = self.machine_records();
        let receptors = self.receptor_records();
        let builders = self.builder_records();
        let parked = standing::parked(&self.parked);
        let sessions = self.lock().state.sessions();
        let standing = |session: &Session| {
            let here = session.machine == self.machine;
            standing::standing(session, here, &parked, &self.boot)
        };
        let holding = Holding {
            machine: &self.machine,
            teams: &teams,
            sessions: &sessions,
            machines: &machines,
            receptors: &receptors,
            builders: &builders,
            formed: &self.formed,
        };
        making(&holding, &standing)
    }

    /// This machine's receptors as the daemon interface holds them.
    fn receptor_records(&self) -> BTreeMap<String, serde_json::Value> {
        receptor::records(
            &self
                .receptors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            &self.machine,
        )
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
            if let Ok(kind) = self.fences.strictest(&session.id)
                && here
                && kind > crate::declared::Kind::None
            {
                fields.insert("fence_shown".to_owned(), serde_json::json!(kind));
            }
        }
        let told = standing::told(&fields);
        if let Some(fields) = fields.as_object_mut() {
            fields.insert("told".to_owned(), told.into());
        }
        let record = daemon::record(kind, &session.id, heard, fields);
        (daemon::key(kind, &session.id), record)
    }

    /// Each event the realm's sessions say, keyed by its session and its
    /// time, with the session's address as its cell.
    /// The forage of each of this machine's cells that works on a quest,
    /// asking for each quest not yet read to be read, which tells the
    /// forages' watchers once it is.
    fn forage_records(&self) -> BTreeMap<String, serde_json::Value> {
        let cells = forage_cells(&self.lock(), &self.machine);
        let (records, unread) = forage::records(
            &mut self
                .forages
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            &cells,
            &self.machine,
        );
        if let Some(to_read) = &self.to_read {
            for quest in unread {
                // Nothing reads quests once the node has stopped.
                let _ = to_read.send(quest);
            }
        }
        records
    }

    fn event_records(&self) -> BTreeMap<String, serde_json::Value> {
        let sessions = self.lock().state.sessions();
        sessions
            .iter()
            .flat_map(|session| {
                session.events.iter().map(move |event| {
                    let id = format!("{}:{}", session.id, event.at);
                    let fields = serde_json::json!({
                        "cell": session.address,
                        "session": session.id,
                        "at": event.at,
                        "what": event.kind,
                        "where": event.place,
                        "title": event.title,
                    });
                    let record = daemon::record(EVENT, &id, millis_at(&event.at), fields);
                    (daemon::key(EVENT, &id), record)
                })
            })
            .collect()
    }

    /// Reads a message that landed and sends it to those who hear it.
    fn read_message(&self, id: &str) {
        let Some(message) = self.store.message(id).as_ref().and_then(mesh::message) else {
            return;
        };
        if !self.takes(&message) {
            return;
        }
        if message.for_realm.is_some() {
            self.wait_to_cross(vec![message]);
            return;
        }
        let mut inner = self.lock();
        if let Some(message) = inner.state.add_message(message) {
            inner.send_message(message);
            self.said.messages_moved();
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
        Some(there(&session.machine, &words))
    }

    /// This machine as it is now, as its realm hears it: a handful of small
    /// files read, or one sysctl on a Mac, quick enough to answer in place.
    fn measure(&self) -> facts::Machine {
        let sources = facts::Sources {
            declared: Some(self.declared.clone()),
            ..facts::Sources::here()
        };
        let mut here = facts::measure_in(&self.machine, &self.realm, &sources);
        here.health.clone_from(&self.health.borrow());
        here.kit = self.given_kit().and_then(start::Kit::said);
        here
    }

    /// The realm's kit as this node was given it, if it was.
    fn given_kit(&self) -> Option<&start::Kit> {
        self.starter
            .as_ref()
            .and_then(|starter| starter.kit.as_ref())
    }

    /// The folder of the realm's kit that goes first on each path, if this
    /// node was given one and carries it.
    fn kit(&self) -> Option<&Path> {
        self.given_kit().and_then(start::Kit::carried)
    }

    /// Keeps this machine's own `health`, just moved, in its last measure
    /// and tells its readers the machines changed, without measuring the
    /// rest again: a word moving is no reason to read the battery.
    fn keep_health(&self, health: Option<own_health::Health>) {
        let mut measured = self
            .measured
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(here) = measured.as_mut() else {
            return;
        };
        here.health = health;
        drop(measured);
        self.said.machines.send_modify(|said| *said += 1);
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

    /// What the node does each minute: it measures this machine, hands its
    /// sessions over once it leaves soon, keeps it awake while work wants
    /// it, tells its links of its carries and judges which tree networks
    /// stand, answering what it runs on.
    fn each_minute(&self, awake: &mut Awake) -> Option<facts::Power> {
        let here = self.measure();
        let power = self.keep_measured(&here);
        tell_hosted(&here);
        self.hand_over(&here);
        self.keep_awake(awake, power.as_ref());
        self.cross_waiting();
        self.tell_carries();
        // A making is what forms, leaves and fades a tree's network, so
        // one runs each minute though nobody asks.
        self.network_records();
        self.said.minute_passed();
        self.settle_carries();
        power
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
        self.said.machines_moved();
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

    /// Places `work` among the realm's machines, each weighed with the
    /// sessions on it and the starts placed there that its room doesn't
    /// yet show. A `start` placed is kept as one, under the same lock, so
    /// a burst of asks spreads rather than piling on whichever led. Each
    /// placement is kept in its log, where it keeps one, once the lock is
    /// let go. Work an agent asks, running in the fence `fence`, is refused
    /// every machine whose ground is looser than here, so nothing is carried
    /// to a machine that would refuse to run it once it got there. Those
    /// machines still count for what the realm reaches, and the record
    /// keeps why each was refused, so a replay refuses them too.
    fn place(&self, work: &Work, start: bool, fence: Option<crate::declared::Kind>) -> Placement {
        let machines = self.machines();
        let refused = fence
            .map(|kind| {
                let here = crate::declared::read(&self.declared);
                ground::looser(kind, here.as_ref(), &self.machine, &machines)
            })
            .unwrap_or_default();
        // This machine is measured first, so work asked on a worker stays
        // there unless it names another.
        let work = &placement::asked_on(work, &machines[0], &machines);
        let now = jiff::Timestamp::now();
        let (loads, decision) = {
            let mut inner = self.lock();
            let inner = &mut *inner;
            let sessions = inner.state.sessions();
            let loads = inner.starts.loads(&machines, running(&sessions), now);
            let decision = placement::decide_refusing(work, &machines, &loads, &refused);
            if start && let Placement::Placed { machine, .. } = &decision.placement {
                inner.starts.placed(machine, now);
            }
            (loads, decision)
        };
        let Some(log) = &self.placements else {
            return decision.placement;
        };
        let record = placements::Record {
            at: now.to_string(),
            start,
            work: work.clone(),
            machines,
            loads,
            refused,
            decision,
        };
        let day = now.to_zoned(jiff::tz::TimeZone::system()).date();
        if let Err(error) = log.keep(&record, day) {
            // A placement unkept costs the monitor one line, never the
            // work its place.
            eprintln!("a placement wasn't kept: {error}");
        }
        record.decision.placement
    }

    fn answer(&self, request: Request) -> Reply {
        match request {
            Request::Sessions => Ok(Response::Sessions(self.lock().state.sessions())),
            Request::Machine => Ok(Response::Machine(Box::new(self.measure()))),
            Request::Machines => Ok(Response::Machines(self.machines())),
            Request::Builders => Ok(Response::Builders(self.builders.builders())),
            Request::Place(_) => Err("a placement is answered by `place_asked`".to_owned()),
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
            Request::Run(_) => Err("a run is answered by `run`".to_owned()),
            Request::Offers => Err("offers are answered by `offers`".to_owned()),
            Request::Steer { .. } => Err("a steer is answered by `steer`".to_owned()),
            Request::End { .. } => Err("an end is answered by `end`".to_owned()),
            Request::Orchestrates { .. } => {
                Err("an orchestration is answered by `orchestrates`".to_owned())
            }
            Request::Free { .. } => Err("freeing a quest is answered by `frees`".to_owned()),
            Request::Update { .. } => Err("an update is answered by `update`".to_owned()),
            Request::Carry { .. } => Err("carrying is answered by `carried`".to_owned()),
            Request::Health => Err("health is answered by `own_health::tell`".to_owned()),
            Request::Turn { .. } => Err("a turn is answered by `take_turn`".to_owned()),
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
                Arc::new(move |machine: &str, words: &[String]| dial.joins(machine, words))
            })),
            steers: self.steers.clone(),
            health: Some(self.health.subscribe()),
            declared: Some(self.declared.clone()),
            kit: self.given_kit().cloned(),
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

    /// Carries `quest` to machine `to` in one act, where the person's own
    /// login asks: from the one machine its sessions run on, by this
    /// node's carry steps when that's here, or through the link reaching
    /// it, vouched for as the person's. Answers where its sessions went.
    async fn carry_back(&self, quest: &str, to: &str) -> Reply {
        let (client, processes) = (self.client, self.processes.clone());
        tokio::task::spawn_blocking(move || {
            asker::persons_own(client, &processes, "carry a quest")
        })
        .await
        .map_err(|error| format!("who asks couldn't be told: {error}"))??;
        let from = crate::carry::runs_on(&self.lock().state.sessions(), quest)?;
        if from != self.machine {
            return self.asks.carry(&from, quest, to, true).await;
        }
        let shares = self
            .shares
            .clone()
            .ok_or_else(|| format!("{} shares no quests, so it carries none", self.machine))?;
        let (node, here) = (self.node.clone(), self.machine.clone());
        let (quest, to) = (quest.to_owned(), to.to_owned());
        tokio::task::spawn_blocking(move || {
            through::for_the_person(node, (*shares).clone(), (&quest, &here, &to))
        })
        .await
        .map_err(|error| format!("its carry stopped: {error}"))?
        .map(Response::Carried)
    }

    /// Puts what the person says into the inbox of the session going by
    /// `address`, on the one machine it runs on: here, or through the link
    /// reaching that machine. A parked session counts as running there,
    /// since it reads its inbox as it wakes, and the person's words are
    /// left for it before the node there wakes it. A talk's file is opened here first,
    /// so the replies carried back land where the person's launcher reads
    /// them.
    async fn steer(&self, address: &str, said: &serde_json::Value) -> Reply {
        if said.get("act").and_then(serde_json::Value::as_str) == Some(ordered::ACT) {
            return self.ordered_carry(address, said).await;
        }
        // A Mac's `ps` takes tens of milliseconds, too long to hold the
        // node's other work up for.
        let (client, processes, asked) = (self.client, self.processes.clone(), said.clone());
        let checked =
            tokio::task::spawn_blocking(move || asker::may_say(client, &processes, &asked))
                .await
                .map_err(|error| format!("who asks couldn't be told: {error}"))??;
        let mut steer = Steer::new(address, said)?;
        steer.checked = checked;
        // The person's words are kept, and go with a talk, so whether they
        // were taken up and answered is its session's own word.
        let person = steer::claims_the_person(said) && !steer.only_wakes();
        let says = steer
            .body
            .get("text")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|text| !text.trim().is_empty());
        if person && says && !steer.body.contains_key("act") && steer.talk().is_none() {
            steer.body.insert("id".to_owned(), words::talk_id().into());
        }
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
                    || parked_presence(session)
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
            let steered = self.asks.steer(&machine, steer.clone()).await;
            if person && matches!(steered, Ok(Response::Steered { .. })) {
                self.keep_word(&steer, &machine);
            }
            return steered;
        }
        self.keep_persons_carry(&steer, &sessions)?;
        self.steer_here(&steer, &sessions)?;
        if person {
            self.keep_word(&steer, &machine);
        }
        Ok(Response::Steered { machine, by: None })
    }

    /// Ends the session held as `held` for good, on the one machine
    /// holding it: here, or through the link reaching that machine. The
    /// person's own login ends any, here or across a link. A session ends
    /// one only here, where both run, and only once this node's record of
    /// orchestrations proves it orchestrates the held one's quest, as an
    /// ordered carry is proved; a presence naming its orchestrator proves
    /// nothing, since a session writes its own. Its presence leaves the
    /// sessions as its program's ends, and its hold's record goes with its
    /// holder.
    async fn end(&self, held: &str) -> Reply {
        let (asker, asking) = self.ender().await?;
        let sessions = self.lock().state.sessions();
        let folder = self.held.clone();
        if hold::held(&folder).iter().any(|one| one.id == held) {
            if let (hold::end::Asker::Session(address), Some(asking)) = (&asker, &asking) {
                let target = sessions
                    .iter()
                    .find(|session| {
                        session.machine == self.machine && session.held.as_deref() == Some(held)
                    })
                    .ok_or_else(|| {
                        format!(
                            "no session here runs in {held}, so {address} isn't proved to \
                             orchestrate it: only the person ends it"
                        )
                    })?;
                let session = target
                    .presence
                    .get("session")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&target.id);
                let asking_known = ordered::Known {
                    session: asking,
                    address,
                };
                let carried = ordered::Known {
                    session,
                    address: &target.address,
                };
                self.ordered_by(asking_known, carried, &sessions)
                    .map_err(|why| {
                        format!(
                            "{address} isn't proved to orchestrate the session held as {held}, \
                             so it can't end it: {why}"
                        )
                    })?;
            }
            let id = held.to_owned();
            let ending =
                tokio::task::spawn_blocking(move || hold::end::end(&folder, &id, hold::end::GRACE))
                    .await
                    .map_err(|error| format!("its end stopped: {error}"))?
                    .map_err(|error| format!("{held} didn't end: {error}"))?;
            eprintln!("ended {held}, held here, for {asker}");
            return Ok(Response::Ended {
                machine: self.machine.clone(),
                held: held.to_owned(),
                ending,
            });
        }
        let mut machines: Vec<&str> = sessions
            .iter()
            .filter(|session| {
                session.machine != self.machine && session.held.as_deref() == Some(held)
            })
            .map(|session| session.machine.as_str())
            .collect();
        machines.sort_unstable();
        machines.dedup();
        match machines.as_slice() {
            [] => Err(format!(
                "nothing is held as {held} here, nor on any machine in reach"
            )),
            [machine] => match asker {
                hold::end::Asker::Person => self.asks.end(machine, held, asker).await,
                hold::end::Asker::Session(address) => Err(format!(
                    "{held} is held on {machine}, and {address}'s end is proved only where \
                     both run: only the person ends it from here"
                )),
            },
            many => Err(format!(
                "{held} is held on {}, so it isn't ended",
                many.join(" and ")
            )),
        }
    }

    /// Who asks an end: the person, where their own login asks, else the
    /// session asking, by its address here, with its id.
    async fn ender(&self) -> Result<(hold::end::Asker, Option<String>), String> {
        let (client, processes) = (self.client, self.processes.clone());
        let person = tokio::task::spawn_blocking(move || {
            asker::persons_own(client, &processes, "end a held session as the person")
        })
        .await
        .map_err(|error| format!("who asks couldn't be told: {error}"))?;
        if person.is_ok() {
            return Ok((hold::end::Asker::Person, None));
        }
        let session = self.asking_session().await.map_err(|why| {
            format!(
                "only the person or the orchestrator of the session it holds ends a hold, \
                 and neither asks: {why}"
            )
        })?;
        let sessions = self.lock().state.sessions();
        let address = self.address_of(&sessions, &session).to_owned();
        Ok((hold::end::Asker::Session(address), Some(session)))
    }

    /// Keeps a carry the person's own login asks of a session here, as an
    /// ordered one is kept, so the person sees it with the rest and the
    /// trees it leaves here are let go once it arrives: by the person, as
    /// this node saw who asked, never as the asker says, and with no why.
    /// Its quest is the one the quests' record the node serves holds its
    /// session in, as an ordered carry's is, never the quest its presence
    /// claims. A carry to no other machine, or of a session no quest's
    /// record holds running here, is carried with nothing kept, since
    /// nothing settles a carry without its quest, and the log says why.
    fn keep_persons_carry(&self, steer: &Steer, sessions: &[Session]) -> Result<(), String> {
        let carries = steer.body.get("act").and_then(serde_json::Value::as_str) == Some("carry");
        if !carries || !steer.checked {
            return Ok(());
        }
        let unkept = |why: &str| {
            eprintln!(
                "the person's carry of {} keeps no line, so nothing lets go of the trees it \
                 leaves here: {why}",
                steer.address
            );
            Ok(())
        };
        let to = steer.body.get("to").and_then(serde_json::Value::as_str);
        // A carry here is no carry, and settling it would let go of the
        // trees the session still works in.
        let Some(to) = to.filter(|to| *to != self.machine) else {
            return unkept("it names no other machine to go to");
        };
        let quests: Vec<serde_json::Value> = self
            .served
            .records("quest")
            .map(|records| records.into_values().collect())
            .unwrap_or_default();
        let Some((known, quest)) = sessions
            .iter()
            .filter(|session| {
                session.machine == self.machine
                    && (matches!(session.state, SessionState::Busy | SessionState::Idle)
                        || parked_presence(session))
            })
            .find_map(|session| {
                let known = ordered::Known {
                    session: crate::carry::session_id(session)?,
                    address: &session.address,
                };
                Some((known, ordered::quest_of(known, &quests).ok()?))
            })
        else {
            return unkept("no quest's record holds a session of it running here");
        };
        let kept = serde_json::json!({
            "at": jiff::Timestamp::now().to_string(),
            "by": "person",
            "session": known.session,
            "address": known.address,
            "quest": quest,
            "from": self.machine,
            "to": to,
        });
        ordered::keep(&self.carries, &kept)
            .map_err(|error| format!("the carry couldn't be kept, so it isn't carried: {error}"))?;
        self.said.sessions.send_modify(|said| *said += 1);
        self.tell_carries();
        Ok(())
    }

    /// The id of the session this connection's client asks for. Each
    /// session here claims its owner in its own presence, which proves
    /// nothing until the kernel and its host back it. They are read from
    /// the store, where each presence is its own, since the node's
    /// sessions keep one of any two that claim one id.
    async fn asking_session(&self) -> Result<String, String> {
        self.asking_session_among(&self.standing()).await
    }

    /// The session proved to ask, as [`Self::asking_session`] proves it,
    /// from these presences, so whoever reads its presence afterwards
    /// reads the one the proof ran over.
    async fn asking_session_among(&self, standing: &[store::Presence]) -> Result<String, String> {
        let claims: Vec<asker::Claim> = standing
            .iter()
            .filter_map(|presence| asker::Claim::of(&presence.session, &presence.body))
            .collect();
        let (client, processes, hosts, logs) = (
            self.client,
            self.processes.clone(),
            self.hosts.clone(),
            self.logs.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let owners = asker::proved(&claims, &processes, &hosts, &logs)?;
            asker::session_of(client, &processes, &owners)
        })
        .await
        .map_err(|error| format!("who asks couldn't be told: {error}"))?
    }

    /// The session whose work trees a run asked here may stand in, as
    /// [`asker::session_alone`] proves it from the presences here.
    async fn session_alone(&self) -> Result<String, String> {
        let claims = self.claims();
        let (client, processes, hosts, logs) = (
            self.client,
            self.processes.clone(),
            self.hosts.clone(),
            self.logs.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let owners = asker::proved(&claims, &processes, &hosts, &logs)?;
            asker::session_alone(client, &processes, &owners, &claims)
        })
        .await
        .map_err(|error| format!("who asks couldn't be told: {error}"))?
    }

    /// The fence `session` shows it runs in, by its owner's process,
    /// proved from the presences here as [`Self::asking_session`] proves
    /// who asks.
    async fn shown_fence(&self, session: &str) -> Result<crate::declared::Kind, String> {
        let claims = self.claims();
        let (processes, hosts, logs, session) = (
            self.processes.clone(),
            self.hosts.clone(),
            self.logs.clone(),
            session.to_owned(),
        );
        tokio::task::spawn_blocking(move || {
            asker::shown_fence(&claims, &processes, &hosts, &logs, &session)
        })
        .await
        .map_err(|error| format!("the fence it runs in couldn't be told: {error}"))?
    }

    /// What the presences of the sessions standing here claim of who owns
    /// each.
    fn claims(&self) -> Vec<asker::Claim> {
        self.standing()
            .iter()
            .filter_map(|presence| asker::Claim::of(&presence.session, &presence.body))
            .collect()
    }

    /// Keeps the strictest fence each session here shows, looking again
    /// as their presences change, at most every [`FENCES_SEEN_EVERY`], so
    /// a fence shown only while a command runs, or switched on after a
    /// session began, still counts when it is carried. Tells the sessions'
    /// watches when one rose.
    async fn see_fences(self) {
        let mut told = self.said.sessions.subscribe();
        loop {
            let looked = tokio::time::Instant::now();
            let (claims, processes, hosts, logs) = (
                self.claims(),
                self.processes.clone(),
                self.hosts.clone(),
                self.logs.clone(),
            );
            let shown = tokio::task::spawn_blocking(move || {
                asker::shown_fences(&claims, &processes, &hosts, &logs)
            })
            .await
            .unwrap_or_default();
            if self.fences.raise(&shown) {
                self.said.sessions.send_modify(|said| *said += 1);
                told.mark_unchanged();
            }
            if told.changed().await.is_err() {
                return;
            }
            tokio::time::sleep_until(looked + FENCES_SEEN_EVERY).await;
        }
    }

    /// Looks each `eases.every` at what this machine would do to ease
    /// itself (`carry::easing`), keeping each choice in `eases.folder`,
    /// and does none of it. It begins once the carries kept here, which
    /// say what moved lately, can be read, saying why while they can't.
    async fn ease(self, eases: Eases) {
        let mut easing = None;
        let mut sampler = easing::Sampler::default();
        let mut unread = None;
        let mut every = tokio::time::interval(eases.every);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            let now = jiff::Timestamp::now();
            if easing.is_none() {
                match self.carried_lately(now).await {
                    Ok(kept) => easing = Some(Easing::after(&kept)),
                    Err(why) if unread.as_ref() == Some(&why) => continue,
                    Err(why) => {
                        let line = serde_json::json!({
                            "at": now.to_string(), "would": true, "choice": "nothing", "why": why,
                        });
                        unread = Some(why);
                        self.keep_easing(&eases.folder, now, line).await;
                        continue;
                    }
                }
            }
            let Some(easing) = easing.as_mut() else {
                continue;
            };
            let (line, back) = self
                .look_to_ease(easing, std::mem::take(&mut sampler), now)
                .await;
            sampler = back;
            if let Some(line) = line {
                self.keep_easing(&eases.folder, now, line).await;
            }
        }
    }

    /// The carries kept here within a day of `now`, or why they can't be
    /// read.
    async fn carried_lately(&self, now: jiff::Timestamp) -> Result<Vec<kept::Kept>, String> {
        let carries = self.carries.clone();
        tokio::task::spawn_blocking(move || kept::read(&carries, now - ease::PIN))
            .await
            .map_err(|error| error.to_string())
            .and_then(|read| read.map_err(|error| error.to_string()))
            .map_err(|error| {
                format!("the carries kept here can't be read ({error}), so what moved lately can't be told")
            })
    }

    /// What this machine would do now to ease itself, as `easing` says
    /// it, with `sampler` back, having counted its sessions' processes.
    async fn look_to_ease(
        &self,
        easing: &mut Easing,
        mut sampler: easing::Sampler,
        now: jiff::Timestamp,
    ) -> (Option<serde_json::Value>, easing::Sampler) {
        let machines = self.machines();
        let (sessions, loads) = {
            let mut inner = self.lock();
            let inner = &mut *inner;
            let sessions = inner.state.sessions();
            let loads = inner.starts.loads(&machines, running(&sessions), now);
            (sessions, loads)
        };
        let here: Vec<&Session> = sessions
            .iter()
            .filter(|session| session.machine == self.machine)
            .collect();
        let fences: BTreeMap<String, crate::declared::Kind> = here
            .iter()
            .filter_map(|session| {
                let id = easing::id(session);
                Some((id.to_owned(), self.fences.strictest(id).ok()?))
            })
            .collect();
        let ids: std::collections::BTreeSet<String> = here
            .iter()
            .map(|session| easing::id(session).to_owned())
            .collect();
        // A session a host runs shares the host's process, which says
        // nothing of the session's own use.
        let pids: BTreeMap<String, i32> = here
            .iter()
            .filter(|session| session.host.is_none())
            .filter_map(|session| {
                let pid = session.presence.pointer("/owner/pid")?.as_i64()?;
                Some((session.id.clone(), i32::try_from(pid).ok()?))
            })
            .collect();
        let procs = match &*self.processes {
            Processes::Folder(procs) => Some(procs.clone()),
            Processes::Listed => None,
        };
        let (store, logs) = (self.store.clone(), self.logs.clone());
        let read = tokio::task::spawn_blocking(move || {
            let holdings = store.holdings();
            let queued = place::queue()
                .map(|queue| room::queued_by(&queue, now.as_second()))
                .unwrap_or_default();
            let sizes = easing::sizes(&logs, &ids);
            let holds = procs
                .map(|procs| sampler.take(&procs, &pids, now))
                .unwrap_or_default();
            (holdings, queued, sizes, holds, sampler)
        })
        .await;
        let Ok((holdings, queued, sizes, holds, sampler)) = read else {
            return (None, easing::Sampler::default());
        };
        let boot = store::folders::boot_id();
        let looked = easing::Looked {
            sessions: &sessions,
            fences: &fences,
            holdings: &holdings,
            queued: &queued,
            sizes: &sizes,
            boot: boot.as_deref(),
            now,
        };
        let cells = easing::cells(&self.machine, &looked);
        let here = ease::Here::of(&self.machine, &machines, &loads);
        let reading = crate::balance::read(&machines, &sessions, now);
        (easing.look(&here, reading, cells, &holds, now), sampler)
    }

    /// Keeps `line`, said `now`, in `folder`, saying so where it can't.
    async fn keep_easing(&self, folder: &Path, now: jiff::Timestamp, line: serde_json::Value) {
        let folder = folder.to_owned();
        let kept = tokio::task::spawn_blocking(move || {
            easing::keep(&folder, now, &line, easing::DAY_HOLDS)
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|kept| kept.map_err(|error| error.to_string()));
        if let Err(error) = kept {
            eprintln!("what this machine would do to ease itself isn't kept: {error}");
        }
    }

    /// The presences of this machine's sessions that run, or wait parked
    /// to run again.
    fn standing(&self) -> Vec<store::Presence> {
        self.store
            .presences()
            .into_iter()
            .filter(|presence| presence.elsewhere.is_none())
            .filter(|presence| {
                presence.lives
                    || presence
                        .body
                        .get("state")
                        .and_then(serde_json::Value::as_str)
                        == Some("parked")
            })
            .collect()
    }

    /// Changes the record of orchestrations as `change` says and keeps it,
    /// holding it so two changes at once never undo each other; refused,
    /// changing nothing, where `change` refuses.
    async fn rerecord(
        &self,
        change: impl FnOnce(&mut chain::Orchestrations) -> Result<(), String> + Send + 'static,
    ) -> Reply {
        let (file, claiming) = (self.orchestrations.clone(), self.claiming.clone());
        tokio::task::spawn_blocking(move || {
            let _claiming = claiming
                .lock()
                .map_err(|_| "the record of orchestrations was left half kept".to_owned())?;
            let mut record = chain::Orchestrations::read(&file).map_err(|error| {
                format!("the record of orchestrations can't be read, so none is changed: {error}")
            })?;
            change(&mut record)?;
            record.keep(&file).map_err(|error| {
                format!("the record of orchestrations couldn't be kept: {error}")
            })?;
            Ok(Response::Orchestrated)
        })
        .await
        .map_err(|error| format!("the orchestration couldn't be recorded: {error}"))?
    }

    /// The address the session `asker` goes by here, or its id where none
    /// is known.
    fn address_of<'a>(&self, sessions: &'a [Session], asker: &'a str) -> &'a str {
        sessions
            .iter()
            .find(|session| {
                session.machine == self.machine
                    && session
                        .presence
                        .get("session")
                        .and_then(serde_json::Value::as_str)
                        == Some(asker)
            })
            .map_or(asker, |session| session.address.as_str())
    }

    /// Records what the session asking orchestrates, as the kernel and
    /// pi's records prove which session asks, or forgets it, where it
    /// names nothing (`carry::chain`). A session its own log says it
    /// continues hands it what it held, and what a session claimed lapses
    /// once it neither runs nor waits parked.
    async fn orchestrates(&self, heads: Option<String>, quests: Vec<String>) -> Reply {
        let asker = self.asking_session().await?;
        let sessions = self.lock().state.sessions();
        let address = self.address_of(&sessions, &asker).to_owned();
        let standing: std::collections::BTreeSet<String> = self
            .standing()
            .into_iter()
            .map(|presence| presence.session)
            .collect();
        let logs = self.logs.clone();
        self.rerecord(move |record| {
            record.lapse(|session| standing.contains(session));
            match heads {
                None if quests.is_empty() => {
                    record.release(&asker);
                    Ok(())
                }
                None => Err("an orchestrator names the quest it heads".to_owned()),
                Some(heads) => {
                    let known = ordered::Known {
                        session: &asker,
                        address: &address,
                    };
                    let continued = chain::continues(&logs, &asker);
                    record.claim(
                        known,
                        &heads,
                        &quests,
                        continued.as_deref(),
                        jiff::Timestamp::now(),
                    )
                }
            }
        })
        .await
    }

    /// Frees `quest` from the session holding it by this node's record of
    /// orchestrations, where the person's own login asks.
    async fn frees(&self, quest: String) -> Reply {
        let (client, processes) = (self.client, self.processes.clone());
        tokio::task::spawn_blocking(move || asker::persons_own(client, &processes, "free a quest"))
            .await
            .map_err(|error| format!("who asks couldn't be told: {error}"))??;
        self.rerecord(move |record| record.free(&quest).map(|_| ()))
            .await
    }

    /// Who orders the carry of `carried` when `asking` asks, as this
    /// node's record of orchestrations proves it, so long as the quests'
    /// record and the mesh's groups agree.
    fn ordered_by(
        &self,
        asking: ordered::Known,
        carried: ordered::Known,
        sessions: &[Session],
    ) -> Result<ordered::Order, String> {
        let quests: Vec<serde_json::Value> = self
            .served
            .records("quest")
            .ok_or("this node serves no quests' record, so who orchestrates can't be told")?
            .into_values()
            .collect();
        let quest = ordered::quest_of(carried, &quests)?;
        let mut record = chain::Orchestrations::read(&self.orchestrations).map_err(|error| {
            format!("this node's record of orchestrations can't be read, so no carry is ordered: {error}")
        })?;
        let standing: std::collections::BTreeSet<String> = self
            .standing()
            .into_iter()
            .map(|presence| presence.session)
            .collect();
        record.lapse(|session| standing.contains(session));
        let order = record.chain(&quest, asking)?;
        chain::agrees(&order, &ordered::ordered_by(asking, carried, &quests)?)?;
        ordered::agreed(&order, asking, &quests, &grouped(sessions, &self.machine))?;
        Ok(order)
    }

    /// Puts a carry an orchestrator orders into the inbox of the session
    /// going by `address`, once the node has proved from its own record
    /// that the session asking orchestrates that session's quest: the
    /// asker found by its process, the chain by what each orchestrator
    /// told it (`carry::chain`), checked against the quests it serves
    /// (`carry::ordered`). The steer then names the orchestrator as who it
    /// is `by` and `from`, whatever the client said, so it never reads as
    /// the person's, and the carry is kept before it is delivered. It is
    /// proved only where both sessions run, so a session on another
    /// machine is refused.
    async fn ordered_carry(&self, address: &str, said: &serde_json::Value) -> Reply {
        let runs =
            |session: &Session| matches!(session.state, SessionState::Busy | SessionState::Idle);
        let address = Steer::new(address, said)?.address;
        let sessions = self.lock().state.sessions();
        let carried: Vec<&Session> = sessions
            .iter()
            .filter(|session| session.address.eq_ignore_ascii_case(&address))
            .filter(|session| runs(session) || parked_presence(session))
            .collect();
        let target = match carried.as_slice() {
            [one] if one.machine == self.machine => *one,
            [one] => {
                return Err(format!(
                    "{address} runs on {}, and a carry is ordered only on the machine its \
                     session runs on",
                    one.machine
                ));
            }
            [] => return Err(format!("no session goes by {address} now")),
            _ => return Err(format!("{address} runs twice, so it isn't carried")),
        };
        let asker = self.asking_session().await?;
        let session = target
            .presence
            .get("session")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&target.id);
        let asking = ordered::Known {
            session: &asker,
            address: self.address_of(&sessions, &asker),
        };
        let carried = ordered::Known {
            session,
            address: &address,
        };
        let order = self.ordered_by(asking, carried, &sessions)?;
        let to = said
            .get("to")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if to == self.machine {
            return Err(format!("{address} already runs on {to}"));
        }
        let claimed = target
            .presence
            .get("fence")
            .and_then(|kind| serde_json::from_value::<crate::declared::Kind>(kind.clone()).ok());
        // Never looser than any it was seen in, since what a process
        // shows comes and goes.
        let shown = self
            .shown_fence(session)
            .await?
            .max(self.fences.strictest(session)?);
        let kind = ground::as_shown(claimed, shown, &address)?;
        let there = self.others().into_iter().find(|machine| machine.name == to);
        ground::firm(
            kind,
            crate::declared::read(&self.declared).as_ref(),
            to,
            there.as_ref(),
        )?;
        let mut body = said.as_object().cloned().unwrap_or_default();
        body.insert("by".to_owned(), order.by.clone().into());
        body.insert("from".to_owned(), order.by.clone().into());
        let steer = Steer::new(&address, &serde_json::Value::Object(body))?;
        let kept = serde_json::json!({
            "at": jiff::Timestamp::now().to_string(),
            "by": order.by.clone(),
            "asker": asker,
            "session": session,
            "address": address,
            "quest": order.quest,
            "chain": order.chain.iter().map(|link| serde_json::json!({
                "quest": link.quest,
                "orchestrator": link.orchestrator,
            })).collect::<Vec<_>>(),
            "source": chain::SOURCE,
            "from": self.machine,
            "to": steer.body.get("to"),
            "why": steer.body.get("why"),
        });
        ordered::keep(&self.carries, &kept)
            .map_err(|error| format!("the carry couldn't be kept, so it isn't ordered: {error}"))?;
        self.said.sessions.send_modify(|said| *said += 1);
        self.tell_carries();
        self.steer_here(&steer, std::slice::from_ref(target))?;
        Ok(Response::Steered {
            machine: self.machine.clone(),
            by: Some(order.by),
        })
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

    /// Asks a turn for the holder resuming `session`, as the sessions
    /// stand now.
    fn ask_turn(&self, session: &str) -> Asked {
        let mut inner = self.lock();
        let sessions = inner.state.sessions();
        inner
            .turns
            .ask(session, &sessions, tokio::time::Instant::now())
    }

    /// Ends the turns their sessions are done with and lets the next
    /// through, at most once each [`turns::LOOKS_EVERY`] however many
    /// holders wait, since each look reads every session record.
    fn pass_turns(&self) {
        let now = tokio::time::Instant::now();
        let mut inner = self.lock();
        if !inner.turns.due(now) {
            return;
        }
        let sessions = inner.state.sessions();
        inner.turns.pass(&sessions, now);
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
    /// realm's machines takes it, on a machine made for it: one that costs
    /// nothing for any start, and one that costs only for a start that
    /// says it may.
    async fn start(&self, asked: Start) -> Reply {
        if asked.command.is_empty() {
            return Err("nothing to start: name a program".to_owned());
        }
        let (misses, nowhere) = match self.place(&asked.work, true, None) {
            Placement::Placed { machine, .. } => return self.start_on(machine, asked).await,
            Placement::Pending { misses } => {
                let nowhere = format!(
                    "nowhere now, though a machine out of reach would do: {}",
                    placement::said(&misses)
                );
                (misses, nowhere)
            }
            Placement::Refused { misses } => {
                let nowhere = format!("nowhere: {}", placement::said(&misses));
                (misses, nowhere)
            }
        };
        if asked.may_cost {
            return self.start_on_made(asked, &misses).await;
        }
        self.start_on_free(asked, nowhere).await
    }

    /// Makes a machine for `asked`, which `misses` say no machine of the
    /// realm takes, from the cheapest offer that would, and starts it there
    /// once it links.
    async fn start_on_made(&self, asked: Start, misses: &[Miss]) -> Reply {
        let who = self.asker_of_a_make().await;
        let offers = self.offers().await;
        let offer = placement::to_make(&asked.work, &offers.offers, &self.machines())
            .map_err(|offered| nothing_offered(misses, &offered, &offers.unanswered))?
            .clone();
        self.start_made_of(asked, &offer, &who).await
    }

    /// Makes a machine for `asked`, which never said it may cost, from an
    /// offer that costs nothing, as any machine of the realm would take it.
    /// Where none would, answers `nowhere`, saying so where one that costs
    /// would have done, so the asker knows to say it may cost.
    async fn start_on_free(&self, asked: Start, nowhere: String) -> Reply {
        let who = self.asker_of_a_make().await;
        let offers = self.offers().await.offers;
        let free: Vec<Offer> = offers
            .iter()
            .filter(|offer| offer.costs.free())
            .cloned()
            .collect();
        let machines = self.machines();
        if let Ok(offer) = placement::to_make(&asked.work, &free, &machines) {
            let offer = offer.clone();
            return self.start_made_of(asked, &offer, &who).await;
        }
        match placement::to_make(&asked.work, &offers, &machines) {
            Ok(costs) => Err(format!(
                "{nowhere}; and {} from {} would, but costs {} {} an hour, so say it may cost",
                costs.shape, costs.provider, costs.costs.hourly, costs.costs.currency
            )),
            Err(_) => Err(nowhere),
        }
    }

    /// Makes a machine of `offer` for `asked`, telling its provider `who`
    /// asks, and starts it there once it links. A machine made that
    /// nothing starts on is let go.
    async fn start_made_of(&self, asked: Start, offer: &Offer, who: &Asker) -> Reply {
        let lasting = lasting(&asked.work, offer)?;
        if let Some(why) = &who.unknown_because {
            eprintln!(
                "{} from {} is told its asker is {}: {why}",
                offer.shape,
                offer.provider,
                made::UNKNOWN
            );
        }
        let made = made::make(offer, lasting, &who.told)
            .await
            .map_err(|refused| match &who.unknown_because {
                Some(why) => format!(
                    "{refused}; it was told its asker is {}: {why}",
                    made::UNKNOWN
                ),
                None => refused,
            })?;
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
        match self.place(&there, true, None) {
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

    /// Runs `asked` on the machine placement chooses, answering where, and
    /// what it says as it goes, or why it doesn't run: this machine runs
    /// it as its own, another through the link reaching it. An agent
    /// asking runs it only where it would stand on ground as firm as
    /// here, as an ordered carry would, and there only through the
    /// launcher that machine declares for its fence. Each run is kept in
    /// the record of runs.
    async fn run(&self, mut asked: Run) -> Result<(String, mpsc::Receiver<Ran>), String> {
        if asked.command.is_empty() {
            return Err("nothing to run: name a command".to_owned());
        }
        let (judged, sessions) = self.judged().await?;
        // What it says of who asks only ever makes it stricter, and an
        // agent runs fenced at least.
        let by = match judged.max(asked.by.unwrap_or(By::Person)) {
            By::Agent(kind) => By::Agent(kind.max(crate::declared::Kind::Fence)),
            By::Person => By::Person,
        };
        asked.by = Some(by);
        let fence = match by {
            By::Agent(kind) => Some(kind),
            By::Person => None,
        };
        // A run ends when its asker stops waiting, so it isn't kept as a
        // start the machine's room has yet to show. An agent's is placed
        // only on ground as firm as here, where it stands in the fence its
        // launcher sets.
        let machine = match self.place(&asked.work, false, fence) {
            Placement::Placed { machine, .. } => machine,
            Placement::Pending { misses } => {
                return Err(format!(
                    "nowhere now, though a machine out of reach would do: {}",
                    placement::said(&misses)
                ));
            }
            Placement::Refused { misses } => {
                if let Some(why) = looser_named(&asked.work, &misses) {
                    return Err(format!(
                        "this asks as {by}, so it runs nothing on looser ground: {why}"
                    ));
                }
                return Err(format!("nowhere: {}", placement::said(&misses)));
            }
        };
        let from = asked.from.clone().unwrap_or_else(|| self.machine.clone());
        let id = format!(
            "{}.{}",
            self.machine,
            jiff::Timestamp::now().as_nanosecond()
        );
        let started = serde_json::json!({
            "run": id,
            "at": jiff::Timestamp::now().to_string(),
            "by": by,
            "sessions": sessions,
            "from": from,
            "on": machine,
            "folder": asked.folder,
            "command": asked.command,
        });
        let said = if machine == self.machine {
            let home =
                std::env::home_dir().ok_or_else(|| format!("{machine} has no home to run in"))?;
            let folder = start::begins_in(asked.folder.as_deref(), &home);
            let launcher = match by {
                By::Person => Vec::new(),
                By::Agent(kind) => {
                    crate::declared::launcher(&self.declared, kind, &self.runs.declarers).map_err(
                        |why| {
                            format!("this asks as {by}, so it runs here only in that fence: {why}")
                        },
                    )?
                }
            };
            let words = run::words(launcher, &self.realm, self.kit(), asked.command);
            // An agent's fence grants the work trees its session holds, so
            // the run is told the session the kernel and pi's records prove
            // asks, never one its words name; none where none is proved,
            // or asked from another machine, so the fence grants none.
            let session = match by {
                By::Agent(_) if asked.from.is_none() => self
                    .session_alone()
                    .await
                    .inspect_err(|why| eprintln!("a run here is told no session: {why}"))
                    .ok(),
                _ => None,
            };
            let (ran, said) = mpsc::channel(run::BACKLOG);
            tokio::spawn(run::here(words, folder, session, ran));
            said
        } else {
            self.asks.run(&machine, asked).await?
        };
        run::journal(&self.runs.journal, &started);
        Ok((machine, run::journaled(self.runs.journal.clone(), id, said)))
    }

    /// Who asks for a machine to be made, as its provider is told it
    /// ([`made::ASKER`]): the address of the session the kernel and pi's
    /// records prove asks ([`Self::asking_session`]), else `person` where
    /// [`asker::judge`] tells the person's own login, else `unknown`, so a
    /// provider serving only some refuses it. The address is the first
    /// the proved session's own presence says, and a proved session whose
    /// presence says none that reads as an address ([`address_told`]) is
    /// `unknown` too, so no presence is told as the person. Like the
    /// judging of a run, this fences habit, not an agent set on passing as
    /// another. A Mac proves no session, so every agent's make there is
    /// `unknown`. A start relayed by another machine's node never makes
    /// one, since the link pins it to this machine by name, which no offer
    /// meets. It is read before any provider is asked what it offers, so
    /// an asker that ends meanwhile is still told as it was.
    async fn asker_of_a_make(&self) -> Asker {
        let unknown = |why: String| Asker {
            told: made::UNKNOWN.to_owned(),
            unknown_because: Some(why),
        };
        let told = |told: &str| Asker {
            told: told.to_owned(),
            unknown_because: None,
        };
        let standing = self.standing();
        match self.asking_session_among(&standing).await {
            Ok(session) => {
                let address = standing
                    .iter()
                    .find(|presence| presence.session == session)
                    .and_then(|presence| presence.body.get("addresses")?.get(0)?.as_str())
                    .and_then(address_told);
                match address {
                    Some(address) => told(&address),
                    None => unknown(format!("{session} says no address")),
                }
            }
            Err(why) => match self.judged().await {
                Ok((By::Person, _)) => told(made::PERSON),
                Ok((By::Agent(_), _)) => unknown(why),
                Err(judging) => unknown(format!("{why}; {judging}")),
            },
        }
    }

    /// Places `work` as [`Node::place`] does for whoever asks, held to an
    /// agent's ground where an agent asks, as a run of its would be, so
    /// what it is told would take the work is where the work can run.
    async fn place_asked(&self, work: &Work) -> Reply {
        let (judged, _) = self.judged().await?;
        let fence = match judged {
            By::Agent(kind) => Some(kind.max(crate::declared::Kind::Fence)),
            By::Person => None,
        };
        Ok(Response::Placement(self.place(work, false, fence)))
    }

    /// Who asks a run, as [`asker::judge`] tells it, and the sessions
    /// claiming the process that asks, nearest first, each of whose
    /// presence says which fence it runs in.
    async fn judged(&self) -> Result<(By, Vec<String>), String> {
        let standing = self.standing();
        let claims: Vec<asker::Claim> = standing
            .iter()
            .filter_map(|presence| asker::Claim::of(&presence.session, &presence.body))
            .collect();
        // A fence this node doesn't know is none it can read.
        let fences: HashMap<String, Option<crate::declared::Kind>> = standing
            .into_iter()
            .map(|presence| {
                let fence = presence
                    .body
                    .get("fence")
                    .and_then(|said| serde_json::from_value(said.clone()).ok());
                (presence.session, fence)
            })
            .collect();
        let (client, processes) = (self.client, self.processes.clone());
        tokio::task::spawn_blocking(move || {
            let above = asker::claimed_above(client, &processes, &claims);
            let kinds: Vec<_> = above
                .iter()
                .map(|session| fences.get(session).copied().flatten())
                .collect();
            (asker::judge(client, &processes, &kinds), above)
        })
        .await
        .map_err(|error| format!("who asks couldn't be told: {error}"))
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
async fn serve<S: Store>(stream: UnixStream, mut shared: Shared<S>) {
    // The kernel says which process is at the other end, so no client can
    // say it is present for another, nor an agent's steer as the person.
    let process = stream.peer_cred().ok().and_then(|cred| cred.pid());
    shared.client = process;
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
        if let Ok(Request::Turn { session }) = request {
            take_turn(&shared, &session, lines, writing).await;
            return;
        }
        if let Ok(Request::Carry { machine }) = request {
            link::carried::to_link(&shared.asks, &machine, lines, writing).await;
            return;
        }
        if let Ok(Request::Health) = request {
            own_health::tell(shared.health.subscribe(), lines, &mut writing).await;
            return;
        }
        if let Ok(Request::Run(asked)) = request {
            hand_on_run(&shared, asked, lines, writing).await;
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
            Ok(Request::End { held }) => shared.end(&held).await.into(),
            Ok(Request::Orchestrates { heads, quests }) => {
                shared.orchestrates(heads, quests).await.into()
            }
            Ok(Request::Free { quest }) => shared.frees(quest).await.into(),
            Ok(Request::Update { machine }) => shared.update(&machine).await.into(),
            Ok(Request::Offers) => Answer::Ok(Response::Offers(shared.offers().await)),
            // A receptor's cell is named `<source>@<machine>`, and only the
            // node holding it speaks as it, so no client is a name with @.
            Ok(Request::Send(draft)) if draft.from.contains('@') => Answer::Err(
                "only the node speaks as a name with @ in it, such as a receptor's".to_owned(),
            ),
            Ok(Request::Send(draft))
                if draft
                    .realm
                    .as_ref()
                    .is_some_and(|realm| *realm != shared.realm) =>
            {
                shared.send_across(draft).await
            }
            // A crossing is another realm's node handing on what it took,
            // and its receptors' signals stay with it.
            Ok(Request::Cross(crossing))
                if crate::stimuli::speaks_as_receptor(&crossing.message.from) =>
            {
                Answer::Err("a receptor's signal stays in its own realm".to_owned())
            }
            Ok(Request::Cross(crossing)) => shared.take(crossing),
            Ok(Request::Place(work)) => shared.place_asked(&work).await.into(),
            asked => asked.and_then(|request| shared.answer(request)).into(),
        };
        if write(&mut writing, &reply).await.is_err() {
            return;
        }
    }
}

/// Why the machine `work` names refused it for its looser ground, where
/// one did, so an agent naming such a machine is told so plainly rather
/// than among every other machine's reasons.
fn looser_named(work: &Work, misses: &[Miss]) -> Option<String> {
    misses
        .iter()
        .filter(|miss| work.needs.contains(&Need::Machine(miss.machine.clone())))
        .flat_map(|miss| &miss.because)
        .find_map(|because| match because {
            placement::Because::Refuses(taint) if taint.key == placement::LOOSER_GROUND => {
                Some(taint.why.clone())
            }
            _ => None,
        })
}

/// Runs what a client asks, writing where it runs and then what it says as
/// it goes, each line written before the next is taken, so a client that
/// reads slowly slows the run. A client that hangs up, or says anything
/// more, stops it.
async fn hand_on_run<S: Store>(
    shared: &Shared<S>,
    asked: Run,
    mut lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    mut writing: tokio::net::unix::OwnedWriteHalf,
) {
    let (machine, mut said) = match shared.run(asked).await {
        Ok(running) => running,
        Err(why) => {
            // A client that hung up hears nothing.
            let _ = write(&mut writing, &Reply::Err(why)).await;
            return;
        }
    };
    if write(&mut writing, &Reply::Ok(Response::Running { machine }))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            ran = said.recv() => {
                let ran = ran.unwrap_or_else(|| Ran::Failed("the run ended without saying how".to_owned()));
                let ends = ran.ends();
                if write(&mut writing, &ran).await.is_err() || ends {
                    return;
                }
            }
            // Dropping what it says stops the run.
            _ = lines.next_line() => return,
        }
    }
}

/// Answers a holder's ask for a turn once it may resume `session`,
/// looking at the turns each [`turns::LOOKS_EVERY`] while it waits. A holder that
/// hangs up first leaves the queue.
async fn take_turn<S: Store>(
    shared: &Shared<S>,
    session: &str,
    mut lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    mut writing: tokio::net::unix::OwnedWriteHalf,
) {
    if let Asked::Wait { ticket, mut go } = shared.ask_turn(session) {
        let mut look = tokio::time::interval(turns::LOOKS_EVERY);
        loop {
            tokio::select! {
                _ = &mut go => break,
                _ = look.tick() => shared.pass_turns(),
                // A waiting holder says nothing; anything it does send,
                // or its hanging up, ends its wait.
                _ = lines.next_line() => {
                    shared.lock().turns.leave(ticket);
                    return;
                }
            }
        }
    }
    // A holder that hung up as it was let through starts nothing to tell.
    let _ = write(&mut writing, &Reply::Ok(Response::Turn)).await;
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
        let holder = dir.join("holder");
        crate::stand_in::program(
            &holder,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\necho held\n",
                given.display()
            ),
        );
        (holder, given)
    }

    /// Holds session `0e50` on kestrel, whose store is `store`, as its
    /// holder there keeps it by `5e551011`, answering the held folder.
    fn hold_a_session(dir: &Path, store: &Memory) -> PathBuf {
        let held = dir.join("kestrel.held");
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
            waiting: "/nowhere/waiting".into(),
            awake: Awake::by(None),
            updater: "/nowhere".into(),
            serves: BTreeMap::new(),
            providers: Vec::new(),
            book: Book::at("/nowhere/made.jsonl".into()),
            flows: Flows::default(),
            parked: "/nowhere/parked".into(),
            held: "/nowhere/held".into(),
            processes: Processes::Folder("/nowhere/proc".into()),
            carries: "/nowhere/carries.jsonl".into(),
            runs: run::Given::default(),
            hosts: "/nowhere/hosts".into(),
            orchestrations: "/nowhere/orchestrations.json".into(),
            logs: "/nowhere/sessions".into(),
            words: words::Book::at("/nowhere/said.jsonl".into()),
            declared: "/nowhere/declared.json".into(),
            receptors: None,
            stimuli: None,
            builders: builders::Given::default(),
            placements: None,
            health: None,
            settles: None,
            scratch: None,
            eases: None,
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
        node_sharing(store, machine, dials, starter, socket, None)
    }

    /// Runs a node as [`node`] does, dialling nothing and starting
    /// nothing.
    fn alone(
        store: &Memory,
        machine: &str,
        socket: &Path,
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<io::Result<()>>,
    ) {
        node(store, machine, Vec::<Piped>::new(), None, socket)
    }

    /// Runs a node of skiff in the work realm on `socket`, as `given`
    /// says and linked to nothing, until `stop` is dropped.
    fn alone_with(
        store: &Memory,
        socket: &Path,
        given: Given,
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<io::Result<()>>,
    ) {
        let membership = Realm {
            name: "work".to_owned(),
            machine: "skiff".to_owned(),
        };
        let bound = bind(socket).expect("the socket binds");
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let store = store.clone();
        let node = tokio::spawn(async move {
            let stopped = async {
                // A dropped sender stops the node as surely as a sent stop.
                let _ = stopped.await;
            };
            let nobody = Listed::<Ssh>(Vec::new());
            run(store, membership, nobody, given, bound, stopped).await
        });
        (stop, node)
    }

    /// Runs a node as [`node`] does, sharing the quests of `sharing`.
    fn node_sharing<D: Dial + Clone>(
        store: &Memory,
        machine: &str,
        dials: Vec<D>,
        starter: Option<Starter>,
        socket: &Path,
        sharing: Option<Places>,
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
        let held = held_of(socket);
        let processes = Processes::Folder(procs_of(socket));
        let carries = socket.with_file_name("carries.jsonl");
        let runs = runs_of(socket);
        let hosts = socket.with_file_name("hosts");
        let orchestrations = socket.with_file_name("orchestrations.json");
        let logs = socket.with_file_name("sessions");
        let words = words::Book::at(socket.with_file_name("said.jsonl"));
        let declared = socket.with_file_name("declared.json");
        let receptors = Some(socket.with_file_name("receptors"));
        let stimuli = Some(socket.with_file_name("stimuli.json"));
        let builders = builders_of(socket);
        let scratch = Some(socket.with_file_name("scratch"));
        // A test that settles a carry puts its stand-in `pi-carry` here.
        let settles = Some(Settles {
            kept: socket.with_file_name("settles.jsonl"),
            program: socket.with_file_name("pi-carry"),
            realm: "work".to_owned(),
            answers_within: PATIENCE,
        });
        let node = tokio::spawn(async move {
            let given = Given {
                sharing,
                starter,
                refusals: Refusals::default(),
                steers,
                waiting: "/nowhere/waiting".into(),
                awake: Awake::by(None),
                updater,
                serves,
                providers,
                book,
                flows: Flows::default(),
                parked,
                held,
                processes,
                carries,
                runs,
                hosts,
                orchestrations,
                logs,
                words,
                declared,
                receptors,
                stimuli,
                builders,
                placements: None,
                health: None,
                scratch,
                eases: None,
                settles,
            };
            let stopped = async {
                // A dropped sender stops the node as surely as a sent stop.
                let _ = stopped.await;
            };
            run(store, membership, Listed(dials), given, bound, stopped).await
        });
        (stop, node)
    }

    /// A dial to kestrel over a pipe, whose far end runs kestrel's
    /// end of the link on `store`, handing starts to its node at `socket`.
    #[derive(Clone)]
    struct Piped {
        store: Memory,
        socket: PathBuf,
        held: PathBuf,
        /// Whether kestrel's end knows the far end is skiff, as a
        /// pinned key proves it.
        proven: bool,
        /// Whether kestrel's end carries what its node asks of
        /// skiff, as the end of a link the other machine dialled does.
        asks_back: bool,
        /// Where kestrel keeps the quests it shares, when it shares them.
        shares: Option<Places>,
    }

    impl Dial for Piped {
        fn describe(&self) -> String {
            "a pipe to kestrel".to_owned()
        }

        fn dial(&self, _realm: &str) -> impl Future<Output = io::Result<Connection>> + Send {
            let (near, far) = tokio::io::duplex(1 << 16);
            let store = self.store.clone();
            let node = self.socket.clone();
            let held_here = self.held.clone();
            let steers = inboxes(&self.socket);
            let proven = self.proven;
            let asking = self.asks_back.then_some(Asking::Dialled);
            let shares = self.shares.clone();
            tokio::spawn(async move {
                let realm = Realm {
                    name: "work".to_owned(),
                    machine: "kestrel".to_owned(),
                };
                let starting = Starting {
                    declared: Some(node.with_file_name("declared.json")),
                    node,
                    held: held_here,
                    asking,
                    steers,
                    health: None,
                    kit: None,
                };
                // The test judges the link by what crosses it.
                let _ = if proven {
                    let by_key = Proven {
                        by: realm,
                        far: "skiff".to_owned(),
                    };
                    link::run(store, &by_key, piped(far), shares, Some(starting)).await
                } else {
                    link::run(store, &realm, piped(far), shares, Some(starting)).await
                };
            });
            std::future::ready(Ok(piped(near)))
        }

        fn there(&self, words: &[String]) -> Vec<String> {
            let mut there = vec!["into-kestrel".to_owned()];
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

    /// Steers the session held on kestrel from the node at
    /// `near_socket`, and has its talk answered there, as its session would,
    /// until the answer is carried back to `near_steers`.
    async fn steers_the_session_on_kestrel(
        near_socket: &Path,
        near_steers: &Path,
        far_steers: &Path,
    ) {
        let asked = Request::Steer {
            address: "sak9fz".to_owned(),
            steer: json!({"text": "how goes it?", "from": "joel", "id": "t1"}),
        };
        // Nothing the near node can read tells it who asks, so it lets
        // the steer go but can't say it saw the person ask, and the far
        // end takes nothing as the person on its word.
        let Err(unseen) = ask(near_socket.to_owned(), asked.clone()).await else {
            panic!("a steer as Joel nobody saw is refused across the link");
        };
        assert!(
            unseen.starts_with("the machine it was asked on didn't say it saw who asked"),
            "saying why: {unseen}"
        );
        // The client runs in this process, which the stand-in for /proc
        // now tells as the person's own.
        let client = procs_of(near_socket).join(std::process::id().to_string());
        fs::create_dir_all(&client).expect("its folder is made");
        fs::write(client.join("status"), "Name:\tmycelium\nPPid:\t1\n")
            .expect("its status is written");
        fs::write(
            client.join("cgroup"),
            "0::/user.slice/user-1000.slice/user@1000.service/session.slice/quickshell.service\n",
        )
        .expect("its group is written");
        assert_eq!(
            ask(near_socket.to_owned(), asked).await,
            Ok(Response::Steered {
                machine: "kestrel".to_owned(),
                by: None,
            }),
            "a session on another machine is steered there"
        );
        let inbox: Vec<_> = fs::read_dir(far_steers.join("SAK9FZ"))
            .expect("kestrel's inbox for it is made")
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

    /// The held sessions' folder of a node on `socket`, beside it.
    fn held_of(socket: &Path) -> PathBuf {
        socket.with_file_name("work.held")
    }

    /// Where a node on `socket` is told who its clients are, a stand-in
    /// for `/proc` beside it: the tests run as an agent, so the machine's
    /// own would refuse every steer they make as the person.
    fn procs_of(socket: &Path) -> PathBuf {
        socket.with_file_name("proc")
    }

    /// Where a node on `socket` keeps its runs, beside it, trusting this
    /// account and root to say how a fenced run starts. A build's sandbox
    /// shows root as nobody, 65534.
    fn runs_of(socket: &Path) -> run::Given {
        run::Given {
            journal: socket.with_file_name("runs.jsonl"),
            declarers: vec![0, 65534, rustix::process::getuid().as_raw()],
        }
    }

    /// Tells the stand-in for /proc beside `socket` that the client, this
    /// process, is a shell in the person's terminal.
    fn asks_as_person(socket: &Path) {
        let client = procs_of(socket).join(std::process::id().to_string());
        fs::create_dir_all(&client).expect("its folder is made");
        fs::write(client.join("status"), "Name:\tsh\nPPid:\t1\n").expect("its status is written");
        fs::write(
            client.join("cgroup"),
            "0::/user.slice/user-1000.slice/user@1000.service/session.slice/kitty.scope\n",
        )
        .expect("its group is written");
    }

    /// Where the updater of a node on `socket` is asked, beside it.
    fn updater_of(socket: &Path) -> PathBuf {
        socket.with_file_name("update.sock")
    }

    /// The builders of a node on `socket`: those a lent machines file
    /// beside it names, as a test may write one, never dialled.
    fn builders_of(socket: &Path) -> builders::Given {
        builders::Given {
            files: vec![builders::File {
                name: "lent".to_owned(),
                path: socket.with_file_name("lent-machines"),
                lent: true,
            }],
            lent_health: None,
            ssh: "ssh".to_owned(),
        }
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
            worker: false,
            named: None,
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
                Some("joel@kestrel: Permission denied (publickey).".to_owned())
            }));
            std::future::ready(Ok(connection))
        }

        fn there(&self, words: &[String]) -> Vec<String> {
            let mut there = vec!["into-kestrel".to_owned()];
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
            health: None,
            declared: Some(dir.path().join("declared.json")),
            kit: None,
        };
        let membership = Realm {
            name: "work".to_owned(),
            machine: "skiff".to_owned(),
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
    fn only_a_busy_session_weighs_on_its_machine() {
        let now = jiff::Timestamp::now();
        let heard = |name: &str, cpu: f64, free: u64| facts::Machine {
            name: name.to_owned(),
            cpu: facts::Cpu {
                model: None,
                cores: Some(8),
                busy: None,
            },
            memory: Some(64 << 30),
            declared: None,
            room: Some(room::Room {
                said_at: now.to_string(),
                memory_available: free << 30,
                pressure: room::Pressure {
                    cpu,
                    memory: 0.0,
                    io: 0.0,
                },
                takes: room::Takes {
                    builds: 1,
                    vm_tests: 1,
                },
                over: Some(Vec::new()),
            }),
            ..kestrel_at("2026-10-29T06:00:00Z")
        };
        let machines = [heard("kestrel", 0.0, 48), heard("skiff", 30.0, 40)];
        let many = |state: &str| -> Vec<Session> {
            (0..40)
                .map(|one| {
                    serde_json::from_value(serde_json::json!({
                        "id": format!("{one:04x}"),
                        "address": format!("{one:06X}"),
                        "machine": "kestrel",
                        "groups": [],
                        "state": state,
                        "at": now.to_string(),
                    }))
                    .expect("a session")
                })
                .collect()
        };
        let placed = |sessions: &[Session]| {
            let loads = placement::Starts::default().loads(&machines, running(sessions), now);
            match placement::place(&work(Vec::new()), &machines, &loads) {
                Placement::Placed { machine, .. } => machine,
                missed => panic!("placed nowhere: {missed:?}"),
            }
        };
        assert_eq!(placed(&[]), "kestrel", "the machine with more room");
        for state in ["idle", "ended", "unreachable"] {
            assert_eq!(
                placed(&many(state)),
                "kestrel",
                "forty {state} take nothing its room doesn't already show"
            );
        }
        assert_eq!(
            placed(&many("busy")),
            "skiff",
            "forty busy weigh on their machine's processors"
        );
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
    async fn a_node_serves_its_cells_murmurs_newest_first() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let (_stop, _node) = alone(&Memory::default(), "skiff", &socket);
        let (mut client, hello) = Speaking::hello(&socket).await;
        assert!(hello["kinds"]["murmur"]["live"].is_array(), "{hello}");
        client
            .ask(json!({"id": 1, "ask": "watch", "kind": "murmur"}))
            .await;
        let whole = client.next().await;
        assert_eq!(whole["records"], json!([]), "none said yet: {whole}");
        let now = jiff::Timestamp::now().as_second();
        let web = crate::web::Web::of(&socket.with_file_name("scratch"), "QEST-20261112-HARBOR")
            .expect("a web");
        let say = |by: &str, words: &str, at: i64| {
            let said = crate::web::Said::Murmur {
                words: words.to_owned(),
            };
            web.say("skiff", Some(by), said, at).expect("said");
        };
        say("KP0L6A", "Landed is slow", now - 20);
        say("FZ9MFJ", "the dock wakes late", now - 10);
        let mut heard = std::collections::BTreeSet::new();
        while heard.len() < 2 {
            let changed = client.next().await;
            for record in changed["changed"].as_array().into_iter().flatten() {
                heard.insert(record["key"].as_str().unwrap_or_default().to_owned());
            }
        }
        let (older, newer) = (
            format!("murmur:KP0L6A:{}", now - 20),
            format!("murmur:FZ9MFJ:{}", now - 10),
        );
        assert_eq!(
            heard,
            std::collections::BTreeSet::from([older.clone(), newer.clone()])
        );
        client
            .ask(json!({"id": 2, "ask": "list", "kind": "murmur", "fields": ["text"]}))
            .await;
        let listed = client.next().await;
        let keys: Vec<&str> = listed["records"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|record| record["key"].as_str())
            .collect();
        assert_eq!(keys, [&newer, &older], "newest first: {listed}");
        assert_eq!(
            listed["records"][0]["text"], "the dock wakes late",
            "{listed}"
        );
        client
            .ask(json!({"id": 3, "ask": "watch", "kind": "murmur"}))
            .await;
        let whole = client.next().await;
        assert_eq!(
            (&whole["records"][0]["key"], &whole["records"][1]["key"]),
            (&json!(newer), &json!(older)),
            "a watch's first answer is newest first too: {whole}"
        );
        client
            .ask(json!({"id": 4, "ask": "get", "keys": [older]}))
            .await;
        let got = client.next().await;
        assert_eq!(got["records"][0]["writer"], "KP0L6A", "{got}");
        assert_eq!(got["records"][0]["machine"], "skiff", "{got}");
    }

    #[tokio::test]
    async fn a_node_says_its_builders_apart_from_its_machines() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let (_stop, _node) = alone(&Memory::default(), "skiff", &socket);
        let (mut client, hello) = Speaking::hello(&socket).await;
        assert!(hello["kinds"]["builder"]["live"].is_array(), "{hello}");
        client
            .ask(json!({"id": 1, "ask": "watch", "kind": "builder"}))
            .await;
        let whole = client.next().await;
        assert_eq!(
            whole["records"],
            json!([]),
            "none before a file names one: {whole}"
        );
        fs::write(
            socket.with_file_name("lent-machines"),
            "ssh-ng://hook@kestrel x86_64-linux /etc/nix/key 16 1 big-parallel\n",
        )
        .expect("the machines file is written");
        let mut changed = client.next().await;
        while changed["changed"][0]["state"] != json!("unknown") {
            changed = client.next().await;
        }
        let kestrel = &changed["changed"][0];
        assert_eq!(
            (
                &kestrel["key"],
                &kestrel["jobs"],
                &kestrel["lines"],
                &kestrel["lent"]
            ),
            (
                &json!("builder:kestrel"),
                &json!(16),
                &json!(["lent"]),
                &json!(true)
            ),
            "{changed}"
        );
        let Ok(Response::Builders(builders)) = ask(socket.clone(), Request::Builders).await else {
            panic!("the node says its builders");
        };
        assert_eq!(
            builders
                .iter()
                .map(|one| one.name.as_str())
                .collect::<Vec<_>>(),
            ["kestrel"]
        );
        let Ok(Response::Machines(machines)) = ask(socket.clone(), Request::Machines).await else {
            panic!("the node says its machines");
        };
        assert!(
            machines.iter().all(|one| one.name != "kestrel"),
            "a builder is no machine of the realm"
        );
        client.ask(json!({"id": 2, "ask": "health"})).await;
        let health = client.next().await;
        assert!(
            health["sources"]
                .as_array()
                .expect("its sources")
                .iter()
                .any(|source| source["name"] == "builders lent" && source["state"] == "answering"),
            "{health}"
        );
    }

    #[tokio::test]
    async fn a_node_says_what_its_providers_offer_and_why_one_couldnt() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        // The node makes its socket's folder, open to no one else; its
        // providers are asked only when offers are.
        let (_stop, _node) = alone(&Memory::default(), "skiff", &socket);
        crate::stand_in::program(
            &socket.with_file_name("provider"),
            "#!/bin/sh\necho '{\"shape\": \"scratch\", \"costs\": {\"hourly\": \"0.12\", \"currency\": \"USD\"}}'\n",
        );

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

    /// Puts a provider beside `socket` offering a free machine reaching
    /// `basin`, whose make writes who it is told asks to `told` and fails,
    /// so nothing is left to link.
    fn provider_telling(socket: &Path, told: &Path) {
        crate::stand_in::program(
            &socket.with_file_name("provider"),
            &r#"#!/bin/sh
case "$1" in
offers) echo '{"shape": "cell", "costs": {"hourly": "0", "currency": "USD"}, "lasts": 600, "facts": {"declared": {"reach": ["basin"], "outlives": [], "costs_money": false, "taints": []}}}' ;;
make) printf '%s\n' "$ASKER" > 'TOLD'; exit 1 ;;
esac
"#
                .replace("ASKER", made::ASKER)
                .replace("TOLD", &told.display().to_string()),
        );
    }

    /// Asks the node on `socket` to start work only a made machine
    /// reaching `basin` would take, answering who its provider was told
    /// asks, as written to `told`.
    async fn told_asker(socket: &Path, told: &Path) -> String {
        let asked = Request::Start(Start {
            work: work(vec![Need::Reach("basin".to_owned())]),
            folder: None,
            command: vec!["pi".to_owned()],
            may_cost: false,
        });
        let started = ask(socket.to_owned(), asked).await;
        assert!(
            started
                .as_ref()
                .is_err_and(|why| why.contains("make cell ended")),
            "the stand-in makes nothing: {started:?}"
        );
        fs::read_to_string(told).expect("the provider was asked to make one")
    }

    #[tokio::test]
    async fn a_provider_is_told_the_person_asked_from_their_own_login() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let told = dir.path().join("told");
        let (_stop, _node) = alone(&Memory::default(), "skiff", &socket);
        provider_telling(&socket, &told);
        asks_as_person(&socket);
        assert_eq!(
            told_asker(&socket, &told).await,
            "person\n",
            "a start from the person's terminal is the person's"
        );
    }

    #[tokio::test]
    async fn a_provider_is_told_an_agent_no_session_is_proved_for_is_unknown() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let told = dir.path().join("told");
        let (_stop, _node) = alone(&Memory::default(), "skiff", &socket);
        provider_telling(&socket, &told);
        asks_below(&socket, 7009);
        assert_eq!(
            told_asker(&socket, &told).await,
            "unknown\n",
            "an agent no presence proves is neither the person nor a session"
        );
    }

    #[tokio::test]
    async fn a_provider_is_told_the_address_of_the_session_proved_to_ask() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let told = dir.path().join("told");
        let (_stop, _node, _client) = orchestrated(&socket, &orchestrations(), &[]).await;
        provider_telling(&socket, &told);
        asks_below(&socket, 7001);
        assert_eq!(
            told_asker(&socket, &told).await,
            "WCJQSG\n",
            "the session whose pi the asker runs below, by its own presence's address"
        );
    }

    #[tokio::test]
    async fn a_provider_is_never_told_a_session_only_claimed_above_the_asker() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let told = dir.path().join("told");
        // It claims the strictly fenced pi the asker runs below, as the
        // steward's presence claims its own, but holds no log of it, so
        // nothing proves the pi is its own.
        let forged = json!({
            "session": "s-forged", "addresses": ["P9R1KI"],
            "groups": ["#orchestration/wcjqsg"], "busy": false,
            "state": "listening", "at": jiff::Timestamp::now().to_string(),
            "quest": "QEST-20261007-WCJQSG", "orchestratedBy": "WCJQSG",
            "fence": "strict",
            "owner": {"pid": 7009, "startTicks": started_at(7009)},
        });
        let store = Memory::default();
        let (_stop, _node, _client) =
            orchestrated_in(&store, &socket, &orchestrations(), &[]).await;
        // Filed under its own name, since a claim filed under another
        // names no owner at all.
        store.start_here("s-forged", forged, true);
        provider_telling(&socket, &told);
        pi_runs(&socket, 7009, STRICT);
        asks_below(&socket, 7009);
        assert_eq!(
            told_asker(&socket, &told).await,
            "unknown\n",
            "a claim nothing proves can't name who asks"
        );
    }

    /// What a provider is told of who asks, when the steward asks for a
    /// machine with its proved presence saying `address` as its address.
    async fn told_of_the_steward_addressed(address: &str) -> String {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let told = dir.path().join("told");
        let store = Memory::default();
        let (_stop, _node, _client) =
            orchestrated_in(&store, &socket, &orchestrations(), &[]).await;
        let mut presence = (*store
            .presences()
            .into_iter()
            .find(|one| one.session == "s-steward")
            .expect("the steward stands")
            .body)
            .clone();
        presence["addresses"] = json!([address]);
        store.start_here("s-steward", presence, true);
        provider_telling(&socket, &told);
        asks_below(&socket, 7001);
        told_asker(&socket, &told).await
    }

    #[tokio::test]
    async fn a_proved_session_whose_address_reads_as_a_word_is_unknown() {
        for address in [
            "person",
            "Person",
            "PERSON",
            "",
            "UNKNOWN",
            "WC-JQSG",
            "WCJQSG0123456789A",
        ] {
            assert_eq!(
                told_of_the_steward_addressed(address).await,
                "unknown\n",
                "a presence saying {address:?} as its address is told as no one"
            );
        }
    }

    #[tokio::test]
    async fn a_proved_session_whose_address_is_in_small_letters_is_told_it_in_capitals() {
        for (address, told_as) in [
            ("4c3144b7", "4C3144B7\n"),
            ("WCJQSG012345678A", "WCJQSG012345678A\n"),
        ] {
            assert_eq!(
                told_of_the_steward_addressed(address).await,
                told_as,
                "a presence saying {address:?} is told by that address, in capitals"
            );
        }
    }

    #[tokio::test]
    async fn a_machine_that_has_left_is_no_longer_one_of_the_realms() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let lent = |name: &str, leaves: jiff::Timestamp| facts::Machine {
            declared: Some(crate::declared::Declared {
                costs_money: true,
                leaves_at: Some(leaves),
                ..crate::declared::Declared::default()
            }),
            ..facts::measure(name, &facts::Sources::nowhere())
        };
        let now = jiff::Timestamp::now();
        let minute = jiff::SignedDuration::from_mins(1);
        store
            .put_machine(&lent("gone", now - minute))
            .expect("what it said is kept");
        store
            .put_machine(&lent("staying", now + minute * 60))
            .expect("what it said is kept");
        let (_stop, _node) = alone(&store, "skiff", &socket);
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
            ["skiff", "staying"],
            "a machine past the time it said it leaves is no longer listed"
        );
    }

    /// The presence of `session`, going by `addresses`, listening and
    /// idle now.
    fn listening(session: &str, addresses: &[&str]) -> Value {
        json!({
            "session": session, "addresses": addresses, "groups": [],
            "busy": false, "state": "listening", "at": jiff::Timestamp::now().to_string(),
        })
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

        /// A client of the node at `socket` that has said hello and
        /// heard its answer, so the node is up and the next answer is
        /// to what the test asks.
        async fn greeted(socket: &Path) -> Self {
            Self::hello(socket).await.0
        }

        /// A client of the node at `socket` that has said hello, with
        /// the node's answer.
        async fn hello(socket: &Path) -> (Self, Value) {
            let mut client = Self::to(socket).await;
            client.ask(json!({"id": 1, "ask": "hello"})).await;
            let hello = client.next().await;
            (client, hello)
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

    /// Waits until `store` knows of the machine `name`. Nothing tells a
    /// test when a link has said what a machine is, so it looks again
    /// every few milliseconds until it has.
    async fn known_to(store: &Memory, name: &str) {
        tokio::time::timeout(PATIENCE, async {
            while !store.machines().iter().any(|one| one.name == name) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the link says what {name} is"));
    }

    /// What kestrel says of itself, as measured at `at`.
    fn kestrel_at(at: &str) -> facts::Machine {
        facts::Machine {
            measured_at: at.to_owned(),
            ..facts::measure("kestrel", &facts::Sources::nowhere())
        }
    }

    #[tokio::test]
    async fn a_list_naming_fields_is_sent_only_those_and_one_naming_none_all() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        store
            .put_machine(&kestrel_at("2026-10-31T04:00:00Z"))
            .expect("what it said is kept");
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let mut client = Speaking::greeted(&socket).await;

        client
            .ask(json!({"id": 2, "ask": "list", "kind": "machine"}))
            .await;
        let whole = client.next().await;
        let whole = whole["records"].as_array().expect("a list's records");
        assert!(
            whole.len() == 2
                && whole
                    .iter()
                    .all(|record| record.as_object().is_some_and(|fields| fields.len() > 3)),
            "an older client, naming no fields, is answered with records whole: {whole:?}"
        );

        client
            .ask(json!({"id": 3, "ask": "list", "kind": "machine", "fields": ["name", "nothing.here"]}))
            .await;
        let projected = client.next().await;
        let projected = projected["records"].as_array().expect("a list's records");
        let expected: Vec<Value> = whole
            .iter()
            .map(|whole| json!({"key": whole["key"], "kind": whole["kind"], "name": whole["name"]}))
            .collect();
        assert_eq!(
            projected, &expected,
            "a list naming fields gets only those, with each record's key and kind"
        );

        client
            .ask(json!({"id": 4, "ask": "list", "kind": "machine", "fields": ["name..x"]}))
            .await;
        let refused = client.next().await;
        assert_eq!(
            (&refused["id"], &refused["error"]["code"]),
            (&json!(4), &json!("not-an-ask")),
            "a field that isn't a path is refused: {refused}"
        );
    }

    #[tokio::test]
    async fn a_client_opening_with_hello_is_answered_in_the_daemon_interface() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        store
            .put_machine(&kestrel_at("2026-10-31T04:00:00Z"))
            .expect("what it said is kept");
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let (mut client, hello) = Speaking::hello(&socket).await;
        assert_eq!(
            (&hello["id"], &hello["version"], &hello["domain"]),
            (&json!(1), &json!(1), &json!("node")),
            "hello says which version it speaks: {hello}"
        );
        assert!(
            hello["kinds"]["session"]["live"].is_array()
                && hello["kinds"]["machine"].is_object()
                && hello["kinds"]["team"]["live"].is_array(),
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
            [&json!("machine:kestrel"), &json!("machine:skiff")],
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
            .put_machine(&kestrel_at("2026-10-31T04:01:00Z"))
            .expect("what it said is kept");
        // This machine's own minute's measure may be told first.
        let mut changed = client.next().await;
        while changed["changed"][0]["key"] == json!("machine:skiff") {
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
                &json!("machine:kestrel"),
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
        let socket = dir.path().join("skiff").join("work.sock");
        let (_stop, _node) = alone(&Memory::default(), "skiff", &socket);
        let (mut client, hello) = Speaking::hello(&socket).await;
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
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node) = alone(&store, "skiff", &socket);
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
        let (mut client, hello) = Speaking::hello(&socket).await;
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
    async fn a_watch_of_ties_is_sent_each_tie_a_message_thickens() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let said = |from: &str, to: &str| {
            let draft = Draft {
                from: from.to_owned(),
                groups: vec![Group::new(format!("peer.{to}")).expect("a valid group")],
                intent: Some(Intent::Question),
                text: "words that stay here".to_owned(),
                re: None,
                realm: None,
            };
            mesh::written(&draft, 1)
        };
        let (first, body) = said("KP0L6A", "hnp07b");
        store.send_here(&first, body);
        let (mut client, hello) = Speaking::hello(&socket).await;
        assert_eq!(
            hello["kinds"]["tie"]["live"],
            json!(tie::LIVE),
            "hello names the ties and what of them changes: {hello}"
        );
        client
            .ask(json!({"id": 2, "ask": "watch", "kind": "tie"}))
            .await;
        let records = client.next().await["records"].clone();
        let tie = records
            .as_array()
            .into_iter()
            .flatten()
            .find(|record| record["key"] == "tie:HNP07B~KP0L6A")
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            (&tie["today"], &tie["intents"]),
            (&json!([0, 1]), &json!(["question"])),
            "the ties already made come first: {records}"
        );
        let (second, body) = said("HNP07B", "kp0l6a");
        store.send_here(&second, body);
        let changed = client.next().await;
        let tie = changed["changed"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|record| record["key"] == "tie:HNP07B~KP0L6A")
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            tie["today"],
            json!([1, 1]),
            "a message thickening a tie is sent as it arrives: {changed}"
        );
        assert!(
            !changed.to_string().contains("words"),
            "no tie says what was said: {changed}"
        );
    }

    #[tokio::test]
    async fn what_waits_on_the_mesh_is_watched_until_it_is_answered_or_ended() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        // SKIFF1's session goes by its own address too.
        let presence = listening("skiff", &["SKIFF1", "5F6018AA"]);
        store.start_here("skiff", presence, true);
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let now = jiff::Timestamp::now();
        let sent = |n: u32, from: &str, to: &str, intent: &str, more: Value| {
            let id = format!("{}-{from}-1-{n:08x}", now.as_millisecond());
            let mut body = json!({
                "v": 1, "id": id, "at": now.to_string(), "from": from, "to": [to],
                "intent": intent, "text": format!("{intent} {n}"), "pid": 1,
            });
            if let (Some(body), Some(more)) = (body.as_object_mut(), more.as_object()) {
                body.extend(more.clone());
            }
            store.send_here(&id, body);
            id
        };
        let asked = sent(1, "KESTRL", "@SKIFF1", "question", json!({}));
        let held = sent(2, "KESTRL", "#machine", "hold", json!({}));
        let (mut client, hello) = Speaking::hello(&socket).await;
        assert_eq!(
            hello["kinds"]["unanswered"],
            json!({"live": ["waiting_on"]}),
            "hello names what waits: {hello}"
        );
        client
            .ask(json!({"id": 2, "ask": "watch", "kind": "unanswered"}))
            .await;
        let records = client.next().await["records"].clone();
        assert_eq!(
            (
                &records[0]["key"],
                &records[0]["intent"],
                &records[0]["from"],
                &records[0]["waiting_on"],
                &records[0]["groups"],
            ),
            (
                &json!(format!("unanswered:{asked}")),
                &json!("question"),
                &json!("KESTRL"),
                &json!(["SKIFF1"]),
                &json!(["@SKIFF1"]),
            ),
            "the question waits on the peer it was put to: {records}"
        );
        assert_eq!(
            (&records[1]["key"], &records[1]["groups"]),
            (&json!(format!("unanswered:{held}")), &json!(["#machine"])),
            "and the hold stands: {records}"
        );

        sent(3, "5F6018AA", "@KESTRL", "fyi", json!({"re": asked}));
        let answered = client.next().await;
        assert_eq!(
            answered["gone"],
            json!([format!("unanswered:{asked}")]),
            "its session's own address answers for SKIFF1: {answered}"
        );
        sent(
            4,
            "SKIFF1",
            "#machine",
            "fyi",
            json!({"re": held, "endsHold": true}),
        );
        let ended = client.next().await;
        assert_eq!(
            ended["gone"],
            json!([format!("unanswered:{held}")]),
            "a hold ended on the person's word stands no longer: {ended}"
        );
    }

    #[tokio::test]
    async fn the_persons_words_are_kept_with_a_talk_that_says_what_became_of_them() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let presence = listening("here", &["HERE12"]);
        store.start_here("here", presence, true);
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let mut client = Speaking::greeted(&socket).await;

        let words = json!({"text": "ship it", "from": "joel"});
        client
            .ask(json!({"id": 2, "ask": "steer", "address": "HERE12", "steer": words}))
            .await;
        client.next().await;
        let machines = json!({"text": "this machine leaves at noon", "from": "machine"});
        client
            .ask(json!({"id": 3, "ask": "steer", "address": "HERE12", "steer": machines}))
            .await;
        client.next().await;
        let inbox: Vec<Value> = held_in(&inboxes(&socket).join("HERE12"));
        let talk = inbox
            .iter()
            .find(|steer| steer["text"] == json!("ship it"))
            .and_then(|steer| steer["id"].as_str())
            .expect("the person's words go with a talk")
            .to_owned();
        assert!(
            inbox
                .iter()
                .any(|steer| steer["from"] == json!("machine") && steer.get("id").is_none()),
            "the machine's words open none: {inbox:?}"
        );

        client
            .ask(json!({"id": 4, "ask": "list", "kind": "said"}))
            .await;
        let records = client.next().await["records"].clone();
        assert_eq!(
            records.as_array().map(|records| records
                .iter()
                .map(|record| (
                    record["key"].clone(),
                    record["address"].clone(),
                    record["machine"].clone(),
                    record["text"].clone(),
                    record.get("taken_up").cloned(),
                ))
                .collect::<Vec<_>>()),
            Some(vec![(
                json!(format!("said:{talk}")),
                json!("HERE12"),
                json!("skiff"),
                json!("ship it"),
                None,
            )]),
            "only the person's words are kept, not yet taken up: {records}"
        );
        let kept = socket.with_file_name("said.jsonl");
        let mode = fs::metadata(&kept).expect("kept").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "only its account reads what the person said");

        let replies = steer::replies(&inboxes(&socket), "HERE12", &talk).expect("a talk");
        fs::write(
            &replies,
            concat!(
                r#"{"stage":"delivered","session":"here","text":"ship it","at":"2026-11-15T12:00:01Z"}"#,
                "\n",
                r#"{"stage":"replied","text":"shipped","at":"2026-11-15T12:02:00Z"}"#,
                "\n",
            ),
        )
        .expect("the session answers");
        client
            .ask(json!({"id": 5, "ask": "list", "kind": "said"}))
            .await;
        let records = client.next().await["records"].clone();
        assert_eq!(
            (&records[0]["taken_up"], &records[0]["answered"]),
            (
                &json!("2026-11-15T12:00:01Z"),
                &json!("2026-11-15T12:02:00Z")
            ),
            "its talk says when it was taken up and answered: {records}"
        );
    }

    #[tokio::test]
    async fn an_agent_is_refused_a_steer_as_the_person_and_let_say_its_own() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let presence = listening("here", &["HERE12"]);
        store.start_here("here", presence, true);
        let (_stop, _node) = alone(&store, "skiff", &socket);
        // The client runs in this process, which the stand-in for /proc
        // tells as running in one of pi's hosts.
        let asker = procs_of(&socket).join(std::process::id().to_string());
        fs::create_dir_all(&asker).expect("its folder is made");
        fs::write(
            asker.join("cgroup"),
            "0::/user.slice/user-1000.slice/user@1000.service/agents.slice/\
             pi-host-work-background.service\n",
        )
        .expect("its group is written");
        let refusal = "this asks from pi-host-work-background.service, an agent's, and an \
                       agent can't speak as the person: only the person's own login says \
                       what they say";

        let paused = ask(
            socket.clone(),
            Request::Steer {
                address: "HERE12".to_owned(),
                steer: json!({"act": "pause", "from": "person"}),
            },
        )
        .await;
        assert_eq!(
            paused,
            Err(refusal.to_owned()),
            "an agent's pause as the person is refused, saying why"
        );

        let mut client = Speaking::greeted(&socket).await;
        let said = json!({"text": "stop there", "from": "joel"});
        client
            .ask(json!({"id": 2, "ask": "steer", "address": "HERE12", "steer": said}))
            .await;
        assert_eq!(
            client.next().await["error"]["message"],
            json!(refusal),
            "so is its steer as Joel in the daemon interface"
        );
        let inbox = inboxes(&socket).join("HERE12");
        let held = || held_in(&inbox);
        assert_eq!(
            held(),
            Vec::<Value>::new(),
            "nothing refused reaches the inbox"
        );

        let own = json!({"text": "the build is done", "from": "machine"});
        client
            .ask(json!({"id": 3, "ask": "steer", "address": "HERE12", "steer": own}))
            .await;
        let steered = client.next().await;
        assert_eq!(
            steered["steered"]["machine"],
            json!("skiff"),
            "what an agent says as no one but itself is let through: {steered}"
        );
        assert_eq!(held(), [own], "and reaches the inbox");
    }

    /// Tells the stand-in for /proc beside `socket` that the client, this
    /// process, runs below process `parent`, a pi's.
    fn asks_below(socket: &Path, parent: i32) {
        let procs = procs_of(socket);
        let client = procs.join(std::process::id().to_string());
        let pi = procs.join(parent.to_string());
        if !pi.join("stat").exists() {
            pi_runs(socket, parent, "HOME=/home/jsama\0");
        }
        for (folder, status) in [
            (&client, format!("Name:\tmycelium\nPPid:\t{parent}\n")),
            (&pi, "Name:\tpi\nPPid:\t1\n".to_owned()),
        ] {
            fs::create_dir_all(folder).expect("its folder is made");
            fs::write(folder.join("status"), status).expect("its status is written");
            fs::write(
                folder.join("cgroup"),
                "0::/user.slice/user-1000.slice/user@1000.service/agents.slice/\
                 mycelium-pi-f3d9.scope\n",
            )
            .expect("its group is written");
        }
    }

    /// Tells the stand-in for /proc beside `socket` that pi `pid` runs,
    /// started when its presence says, by [`started_at`], with `environ`
    /// the environment it began with.
    fn pi_runs(socket: &Path, pid: i32, environ: &str) {
        let pi = procs_of(socket).join(pid.to_string());
        let zeros = "0 ".repeat(17);
        fs::create_dir_all(&pi).expect("its folder is made");
        fs::write(
            pi.join("stat"),
            format!("{pid} (pi) S 1 {zeros}{} 0 0\n", started_at(pid)),
        )
        .expect("its stat is written");
        fs::write(pi.join("status"), "Name:\tpi\nPPid:\t1\n").expect("its status is written");
        fs::write(pi.join("environ"), environ).expect("its environment is written");
    }

    /// The environment of a pi the harness started strictly fenced.
    const STRICT: &str = "PI_SANDBOX=strict\0";

    /// The environment of a pi started in no fence.
    const UNFENCED: &str = "HOME=/home/jsama\0";

    /// Tells the stand-in for /proc beside `socket` that a bubblewrap
    /// `pid` runs below process `parent`, as the agent fence starts one
    /// for each command it fences.
    fn fences_below(socket: &Path, pid: i32, parent: i32) {
        let bwrap = procs_of(socket).join(pid.to_string());
        fs::create_dir_all(&bwrap).expect("its folder is made");
        fs::write(
            bwrap.join("status"),
            format!("Name:\tbwrap\nPPid:\t{parent}\n"),
        )
        .expect("its status is written");
    }

    /// When the test's pi `pid` started, in clock ticks since boot.
    fn started_at(pid: i32) -> u64 {
        u64::try_from(pid).expect("a pid") * 10
    }

    /// The quests' record of WCJQSG's realm, where it orchestrates OSPHT4
    /// and HARBOR.
    fn orchestrations() -> Value {
        json!({"quests": [
            {"id": "QEST-20261007-WCJQSG", "address": "WCJQSG", "orchestrator": null,
             "sessions": ["s-steward"]},
            {"id": "QEST-20261108-OSPHT4", "address": "OSPHT4", "orchestrator": "WCJQSG",
             "sessions": ["s-weigh"]},
            {"id": "QEST-20261115-HARBOR", "address": "HARBOR", "orchestrator": "WCJQSG",
             "sessions": ["s-harbour"]},
        ]})
    }

    /// A node on `socket` serving `quests`, the quests' record: WCJQSG's
    /// own, orchestrating OSPHT4's and HARBOR's, each with one session
    /// owned by its own pi (7001, 7002 and 7003), and the presences `also`
    /// beside them. OSPHT4's presence says it works on WCJQSG's quest too,
    /// which the record doesn't hold. It answers once the node serves the
    /// record, with what keeps the node running.
    async fn orchestrated(
        socket: &Path,
        quests: &Value,
        also: &[Value],
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<io::Result<()>>,
        Speaking,
    ) {
        orchestrated_in(&Memory::default(), socket, quests, also).await
    }

    /// As [`orchestrated`], keeping its sessions in `store`.
    async fn orchestrated_in(
        store: &Memory,
        socket: &Path,
        quests: &Value,
        also: &[Value],
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<io::Result<()>>,
        Speaking,
    ) {
        harbour_claiming(store, socket, quests, also, ("strict", STRICT)).await
    }

    /// As [`orchestrated_in`], HARBOR's presence claiming it runs in the
    /// fence `fence`, as each of the others claims the strict one.
    async fn harbour_claiming(
        store: &Memory,
        socket: &Path,
        quests: &Value,
        also: &[Value],
        (fence, began): (&str, &str),
    ) -> (
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<io::Result<()>>,
        Speaking,
    ) {
        let at = jiff::Timestamp::now().to_string();
        // The groups the mesh put each in, as WCJQSG orchestrates them.
        for (session, address, quest, pid, groups) in [
            (
                "s-steward",
                "WCJQSG",
                "QEST-20261007-WCJQSG",
                7001,
                json!(["#orchestration/wcjqsg"]),
            ),
            (
                "s-weigh",
                "OSPHT4",
                "QEST-20261007-WCJQSG",
                7002,
                json!(["#orchestration/ospht4", "#orchestration/wcjqsg"]),
            ),
            (
                "s-harbour",
                "HARBOR",
                "QEST-20261115-HARBOR",
                7003,
                json!(["#orchestration/wcjqsg", "#repo/mycelium"]),
            ),
        ] {
            let presence = json!({
                "session": session, "addresses": [address], "groups": groups,
                "busy": false, "state": "listening", "at": at, "quest": quest,
                "orchestratedBy": "WCJQSG",
                "fence": if session == "s-harbour" { fence } else { "strict" },
                "owner": {"pid": pid, "startTicks": started_at(pid)},
            });
            store.start_here(session, presence, true);
        }
        for (at, presence) in also.iter().enumerate() {
            store.start_here(&format!("also-{at}"), presence.clone(), true);
        }
        let (stop, running) = alone(store, "skiff", socket);
        let mut client = Speaking::greeted(socket).await;
        // Once the node has started, as a link carries it, since a machine
        // no link claims as the node starts is marked out of reach.
        store
            .put_machine(&fenced("tern", &["remote"]))
            .expect("what it said is kept");
        client
            .ask(json!({"id": 2, "ask": "watch", "kind": "quest"}))
            .await;
        assert_eq!(client.next().await["records"], json!([]), "none yet");
        let beside = socket.with_file_name(".quests.json.new");
        fs::write(&beside, quests.to_string()).expect("written");
        fs::rename(&beside, socket.with_file_name("quests.json")).expect("renamed into place");
        client.next().await;
        for (pid, session) in [(7001, "s-steward"), (7002, "s-weigh"), (7003, "s-harbour")] {
            pi_runs(socket, pid, if pid == 7003 { began } else { STRICT });
            holds_log(socket, pid, session);
        }
        let here = fenced("skiff", &["remote", "big"]).declared;
        fs::write(
            socket.with_file_name("declared.json"),
            serde_json::to_vec(&here).expect("it is JSON"),
        )
        .expect("this machine's declaration is written");
        (stop, running, client)
    }

    /// What `name` says of itself, running its sessions strictly fenced,
    /// building only on `builders`, reaching no forge and released no
    /// secret.
    fn fenced(name: &str, builders: &[&str]) -> facts::Machine {
        let strict = crate::declared::Doors {
            builders: builders.iter().map(|&one| one.to_owned()).collect(),
            forges: std::collections::BTreeSet::new(),
            secrets: std::collections::BTreeSet::new(),
            runs: Vec::new(),
        };
        facts::Machine {
            declared: Some(crate::declared::Declared {
                fence: Some(BTreeMap::from([(crate::declared::Kind::Strict, strict)])),
                ..crate::declared::Declared::default()
            }),
            ..facts::measure(name, &facts::Sources::nowhere())
        }
    }

    /// Opens, for the test's pi `pid`, the log pi keeps of `session` in the
    /// logs beside `socket`, as a link in its `fd` folder in the stand-in
    /// for /proc.
    fn holds_log(socket: &Path, pid: i32, session: &str) {
        let log = socket
            .with_file_name("sessions")
            .join("--home-joel--")
            .join(format!("2026-11-15T17-00-00-000Z_{session}.jsonl"));
        fs::create_dir_all(log.parent().expect("its folder")).expect("made");
        fs::write(
            &log,
            format!("{{\"type\":\"session\",\"id\":\"{session}\"}}\n"),
        )
        .expect("written");
        let fds = procs_of(socket).join(pid.to_string()).join("fd");
        fs::create_dir_all(&fds).expect("made");
        std::os::unix::fs::symlink(&log, fds.join("3")).expect("linked");
    }

    /// What the node at `socket` answers the session owned by pi `pid` as it
    /// says it heads `heads` and orchestrates `quests`.
    async fn claims(socket: &Path, pid: i32, heads: Option<&str>, quests: &[&str]) -> Reply {
        asks_below(socket, pid);
        ask(
            socket.to_owned(),
            Request::Orchestrates {
                heads: heads.map(str::to_owned),
                quests: quests.iter().map(|quest| (*quest).to_owned()).collect(),
            },
        )
        .await
    }

    /// WCJQSG's session tells the node at `socket` it orchestrates OSPHT4's
    /// quest and HARBOR's, as the quests' record says.
    async fn steward_claims(socket: &Path) {
        assert_eq!(
            claims(
                socket,
                7001,
                Some("QEST-20261007-WCJQSG"),
                &["QEST-20261108-OSPHT4", "QEST-20261115-HARBOR"],
            )
            .await,
            Ok(Response::Orchestrated),
            "the steward's orchestration is recorded"
        );
    }

    /// Tells the stand-in for /proc beside `socket` that the client, this
    /// process, is the person's own login's.
    fn asks_as_the_person(socket: &Path) {
        let client = procs_of(socket).join(std::process::id().to_string());
        fs::create_dir_all(&client).expect("its folder is made");
        fs::write(client.join("status"), "Name:\tmycelium\nPPid:\t1\n")
            .expect("its status is written");
        fs::write(
            client.join("cgroup"),
            "0::/user.slice/user-1000.slice/session-2.scope\n",
        )
        .expect("its group is written");
    }

    /// What the node at `socket` answers as it is asked to free `quest`.
    async fn frees(socket: &Path, quest: &str) -> Reply {
        ask(
            socket.to_owned(),
            Request::Free {
                quest: quest.to_owned(),
            },
        )
        .await
    }

    #[tokio::test]
    async fn an_orchestrator_started_afresh_claims_its_quests_again() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node, _client) =
            orchestrated_in(&store, &socket, &orchestrations(), &[]).await;
        steward_claims(&socket).await;
        store.start_here(
            "s-fresh",
            json!({"session": "s-fresh", "addresses": ["WCJQSG"], "groups": [],
                   "busy": false, "state": "listening",
                   "at": jiff::Timestamp::now().to_string(),
                   "owner": {"pid": 7004, "startTicks": started_at(7004)}}),
            true,
        );
        holds_log(&socket, 7004, "s-fresh");
        let heads = Some("QEST-20261007-WCJQSG");
        let quests = ["QEST-20261108-OSPHT4", "QEST-20261115-HARBOR"];
        assert!(
            claims(&socket, 7004, heads, &quests).await.is_err(),
            "while the steward runs, a fresh session takes nothing of its"
        );
        store.ended(7001);
        assert_eq!(
            claims(&socket, 7003, heads, &["QEST-20261108-OSPHT4"]).await,
            Ok(Response::Orchestrated),
            "once it has ended, its claims lapse, so a helper may race for them"
        );
        asks_below(&socket, 7003);
        assert_eq!(
            ask(
                socket.clone(),
                Request::Steer {
                    address: "ospht4".to_owned(),
                    steer: json!({"act": "ordered-carry", "to": "tern",
                                  "why": "it needs the camera"}),
                },
            )
            .await,
            Err(
                "HARBOR doesn't orchestrate QEST-20261108-OSPHT4 by the quests' record, so it \
                 can't order OSPHT4's carry: only a session of WCJQSG could"
                    .to_owned()
            ),
            "but orders nothing the quests' record doesn't agree with"
        );
        assert!(
            claims(&socket, 7004, heads, &quests).await.is_err(),
            "and holds the quests from the fresh steward"
        );
        asks_as_the_person(&socket);
        assert_eq!(
            frees(&socket, "QEST-20261007-WCJQSG").await,
            Ok(Response::Orchestrated),
            "until the person frees them"
        );
        assert_eq!(
            claims(&socket, 7004, heads, &quests).await,
            Ok(Response::Orchestrated),
            "so the fresh steward claims its quests again"
        );
    }

    /// What the node `client` speaks to answers as it is asked, under id
    /// `id`, to carry `quest` to `to`.
    async fn carries(client: &mut Speaking, id: u64, quest: &str, to: &str) -> Value {
        client
            .ask(json!({"id": id, "ask": "carry", "quest": quest, "to": to}))
            .await;
        client.next().await
    }

    #[tokio::test]
    async fn only_the_persons_own_login_carries_a_quest_in_one_act() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let at = jiff::Timestamp::now().to_string();
        store.start_here(
            "s-harbour",
            json!({"session": "s-harbour", "addresses": ["HARBOR"], "groups": [],
                   "busy": false, "state": "listening", "at": at,
                   "quest": "QEST-20261115-HARBOR",
                   "owner": {"pid": 7003, "startTicks": started_at(7003)}}),
            true,
        );
        let far = json!({"session": "s-far", "addresses": ["FARAWY"], "groups": [],
                         "busy": true, "state": "working", "at": at,
                         "quest": "QEST-20261115-FARAWY"});
        let places = dir.path().join("places");
        let sharing = Places {
            quests: places.join("quests"),
            scratch: places.join("scratch"),
            home: places.join("home"),
            state: places.join("state"),
        };
        let (_stop, _node) = node_sharing(
            &store,
            "skiff",
            Vec::<Piped>::new(),
            None,
            &socket,
            Some(sharing),
        );
        let mut client = Speaking::greeted(&socket).await;
        // Once the node has started, as a link carries it, since a session
        // of a machine no link claims as the node starts is let go.
        store
            .put_presence("s-far", "tern", &far)
            .expect("tern's session is kept");
        listed(socket.clone(), "s-far").await;

        asks_below(&socket, 7003);
        assert_eq!(
            carries(&mut client, 2, "QEST-20261115-HARBOR", "osprey").await["error"]["message"],
            json!(
                "this asks from mycelium-pi-f3d9.scope, an agent's, so it can't carry a quest: \
                 only the person's own login does"
            ),
            "an agent carries nothing in the person's one act"
        );
        asks_as_the_person(&socket);
        assert_eq!(
            carries(&mut client, 3, "QEST-20261115-HARBOR", "osprey").await["error"]["message"],
            json!(
                "QEST-20261115-HARBOR wasn't carried to osprey: its find failed: osprey isn't a \
                 machine of the realm in reach; those are none"
            ),
            "the person's carry runs the node's own carry steps, which say why it stopped"
        );
        assert_eq!(
            carries(&mut client, 4, "QEST-20261115-5HJZ8Z", "tern").await["error"]["message"],
            json!(
                "no session of QEST-20261115-5HJZ8Z runs on any machine in reach, so there's \
                 nothing to carry"
            ),
            "a quest nothing runs is carried from nowhere"
        );
        assert_eq!(
            carries(&mut client, 5, "QEST-20261115-FARAWY", "skiff").await["error"]["message"],
            json!("no link reaches tern now"),
            "a quest running on another machine is carried from there, over its link"
        );
    }

    #[tokio::test]
    async fn only_the_persons_own_login_frees_a_quest() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let (_stop, _node, _client) = orchestrated(&socket, &orchestrations(), &[]).await;
        steward_claims(&socket).await;
        asks_below(&socket, 7002);
        assert_eq!(
            frees(&socket, "QEST-20261115-HARBOR").await,
            Err(
                "this asks from mycelium-pi-f3d9.scope, an agent's, so it can't free a quest: \
                 only the person's own login does"
                    .to_owned()
            ),
            "an agent frees nothing"
        );
        asks_as_the_person(&socket);
        assert_eq!(
            frees(&socket, "QEST-20261115-5HJZ8Z").await,
            Err(
                "no session holds QEST-20261115-5HJZ8Z by this node's record, so nothing is \
                 freed"
                    .to_owned()
            ),
            "a quest nobody holds is said to be free already"
        );
        assert_eq!(
            frees(&socket, "QEST-20261115-HARBOR").await,
            Ok(Response::Orchestrated),
            "the person frees one an orchestrator holds"
        );
        assert_eq!(
            orders_harbour(&socket, 7001).await,
            Err(
                "no orchestrator has told this node it orchestrates QEST-20261115-HARBOR, so \
                 only the person carries it"
                    .to_owned()
            ),
            "so it orders that quest's carry no more"
        );
    }

    // WCJQSG (2026-11-16): an orchestrator ends a session it orchestrates,
    // held where both run, as this node's record of orchestrations proves.
    #[tokio::test]
    async fn an_orchestrator_ends_a_session_it_orchestrates_held_here() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node, _client) =
            orchestrated_in(&store, &socket, &orchestrations(), &[]).await;
        steward_claims(&socket).await;
        let held = held_of(&socket);
        let (holder, program) = holds_for_real(&held, "e0d4");
        // HARBOR's pi is a stand-in, so its presence names its hold by the
        // file that wakes it rather than by the program's pid.
        store.start_here(
            "s-harbour",
            json!({
                "session": "s-harbour", "addresses": ["HARBOR"],
                "groups": ["#orchestration/wcjqsg", "#repo/mycelium"],
                "busy": false, "state": "listening",
                "at": jiff::Timestamp::now().to_string(),
                "quest": "QEST-20261115-HARBOR", "orchestratedBy": "WCJQSG",
                "fence": "strict", "wake": held.join("e0d4.wake"),
                "owner": {"pid": 7003, "startTicks": started_at(7003)},
            }),
            true,
        );
        let listing = socket.clone();
        let listed = tokio::task::spawn_blocking(move || {
            let mut client = Client::connect(&listing).expect("the client connects");
            // Nothing tells a client when the node has read the hold, so it
            // asks again every few milliseconds until it has.
            loop {
                let Ok(Ok(Response::Sessions(all))) = client.ask(&Request::Sessions) else {
                    panic!("the node lists its sessions");
                };
                if all.iter().any(|one| one.held.as_deref() == Some("e0d4")) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        tokio::time::timeout(PATIENCE, listed)
            .await
            .expect("the node reads HARBOR's hold in time")
            .expect("the client asks");
        asks_below(&socket, 7001);

        let ended = ask(
            socket.clone(),
            Request::End {
                held: "e0d4".to_owned(),
            },
        )
        .await;

        assert_eq!(
            ended,
            Ok(Response::Ended {
                machine: "skiff".to_owned(),
                held: "e0d4".to_owned(),
                ending: hold::end::Ending::Gently,
            }),
            "WCJQSG ends HARBOR's session, which it orchestrates"
        );
        assert!(
            !runs(holder) && !runs(program),
            "its holder and program are gone"
        );
        assert!(
            !held.join("e0d4.json").exists() && !held.join("e0d4.sock").exists(),
            "its record and socket are taken away"
        );
    }

    /// What `asker`'s pi answers as it orders HARBOR's carry to tern.
    async fn orders_harbour(socket: &Path, asker: i32) -> Reply {
        asks_below(socket, asker);
        ask(
            socket.to_owned(),
            Request::Steer {
                address: "harbor".to_owned(),
                steer: json!({"act": "ordered-carry", "to": "tern",
                              "why": "it needs the camera"}),
            },
        )
        .await
    }

    #[tokio::test]
    async fn an_ordered_carry_takes_the_stricter_of_the_fence_claimed_and_shown() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node, _client) =
            harbour_claiming(&store, &socket, &orchestrations(), &[], ("fence", STRICT)).await;
        steward_claims(&socket).await;
        assert_eq!(
            orders_harbour(&socket, 7001).await,
            Err(
                "HARBOR says it runs fenced, but its process shows it strictly fenced, so where \
                 it may land can't be told"
                    .to_owned()
            ),
            "a session claiming a looser fence than its pi began in is refused"
        );
        assert!(
            !socket.with_file_name("carries.jsonl").exists(),
            "and nothing is kept of it"
        );
        fs::remove_file(procs_of(&socket).join("7003").join("environ")).expect("removed");
        assert_eq!(
            orders_harbour(&socket, 7001).await,
            Err(
                "s-harbour's process can't be read, so the fence it runs in can't be told"
                    .to_owned()
            ),
            "nor where its process can't be read"
        );
    }

    #[tokio::test]
    async fn an_ordered_carry_counts_a_fenced_command_below_a_session_claiming_none() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node, _client) =
            harbour_claiming(&store, &socket, &orchestrations(), &[], ("none", UNFENCED)).await;
        steward_claims(&socket).await;
        fences_below(&socket, 7100, 7003);
        assert_eq!(
            orders_harbour(&socket, 7001).await,
            Err(
                "HARBOR says it runs unfenced, but its process shows it fenced, so where it \
                 may land can't be told"
                    .to_owned()
            ),
            "a bubblewrap below its lone pi shows it fenced, whatever its environment says"
        );
        assert!(
            !socket.with_file_name("carries.jsonl").exists(),
            "and nothing is kept of it"
        );
    }

    #[tokio::test]
    async fn an_ordered_carry_of_a_session_whose_owner_isnt_proved_is_refused() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node, _client) =
            orchestrated_in(&store, &socket, &orchestrations(), &[]).await;
        steward_claims(&socket).await;
        fs::remove_dir_all(procs_of(&socket).join("7003").join("fd")).expect("removed");
        assert_eq!(
            orders_harbour(&socket, 7001).await,
            Err(
                "who owns s-harbour can't be proved here, so the fence it runs in can't be told"
                    .to_owned()
            ),
            "a process that doesn't hold the session's log shows nothing of its fence"
        );
    }

    #[tokio::test]
    async fn an_ordered_carry_remembers_the_strictest_fence_its_session_was_seen_in() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node, mut watching) =
            harbour_claiming(&store, &socket, &orchestrations(), &[], ("none", UNFENCED)).await;
        steward_claims(&socket).await;
        watching
            .ask(json!({"id": 3, "ask": "watch", "kind": "session"}))
            .await;
        assert!(
            watching.next().await["records"].is_array(),
            "the sessions are sent whole first"
        );
        fences_below(&socket, 7100, 7003);
        // A presence changing has the node look at its sessions' processes.
        let passing = listening("s-passing", &["PASSNG"]);
        store.start_here("s-passing", passing, true);
        let seen = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let said = watching.next().await;
                let fenced = said["changed"].as_array().is_some_and(|changed| {
                    changed.iter().any(|record| {
                        record["key"] == "session:s-harbour" && record["fence_shown"] == "fence"
                    })
                });
                if fenced {
                    break;
                }
            }
        })
        .await;
        assert!(seen.is_ok(), "the node sees the fenced command below it");
        fs::remove_dir_all(procs_of(&socket).join("7100")).expect("its command ended");
        assert_eq!(
            orders_harbour(&socket, 7001).await,
            Err(
                "HARBOR says it runs unfenced, but its process shows it fenced, so where it \
                 may land can't be told"
                    .to_owned()
            ),
            "a fence seen once still counts once its command has ended"
        );
    }

    #[tokio::test]
    async fn an_ordered_carry_never_lands_on_looser_ground() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node, _client) =
            orchestrated_in(&store, &socket, &orchestrations(), &[]).await;
        steward_claims(&socket).await;
        let to = |machine: &str| {
            let socket = socket.clone();
            let machine = machine.to_owned();
            async move {
                asks_below(&socket, 7001);
                ask(
                    socket,
                    Request::Steer {
                        address: "harbor".to_owned(),
                        steer: json!({"act": "ordered-carry", "to": machine,
                                      "why": "it needs the camera"}),
                    },
                )
                .await
            }
        };
        assert_eq!(
            to("skiff").await,
            Err("HARBOR already runs on skiff".to_owned()),
            "a carry goes somewhere else"
        );
        assert_eq!(
            to("kestrel").await,
            Err(
                "kestrel isn't one of this realm's machines, so nothing is carried there"
                    .to_owned()
            ),
            "a machine this realm doesn't know, as one of another realm"
        );
        store
            .put_machine(&fenced("tern", &["remote", "cloud"]))
            .expect("what it said is kept");
        assert_eq!(
            to("tern").await,
            Err(
                "tern's strictly fenced session opens builders cloud, which here's \
                 doesn't, so the session would land looser there"
                    .to_owned()
            ),
            "a machine whose fence opens a builder here's doesn't"
        );
        assert!(
            !socket.with_file_name("carries.jsonl").exists(),
            "and nothing refused is kept as carried"
        );
        assert_eq!(
            held_in(&inboxes(&socket).join("HARBOR")),
            Vec::<Value>::new(),
            "nor reaches the inbox"
        );
    }

    #[tokio::test]
    async fn a_quest_no_orchestrator_told_the_node_of_orders_no_carry() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let (_stop, _node, _client) = orchestrated(&socket, &orchestrations(), &[]).await;
        assert_eq!(
            orders_harbour(&socket, 7001).await,
            Err(
                "no orchestrator has told this node it orchestrates QEST-20261115-HARBOR, so \
                 only the person carries it"
                    .to_owned()
            ),
            "the quests' record and the mesh agreeing prove nothing alone"
        );
        assert_eq!(
            held_in(&inboxes(&socket).join("HARBOR")),
            Vec::<Value>::new(),
            "nothing refused reaches the inbox"
        );
    }

    #[tokio::test]
    async fn only_the_session_holding_an_orchestration_hands_it_on() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let (_stop, _node, _client) = orchestrated(&socket, &orchestrations(), &[]).await;
        steward_claims(&socket).await;
        assert_eq!(
            claims(
                &socket,
                7002,
                Some("QEST-20261108-OSPHT4"),
                &["QEST-20261115-HARBOR"]
            )
            .await,
            Err(
                "WCJQSG's session already orchestrates QEST-20261115-HARBOR by this node's \
                 record, so OSPHT4 can't: only that session hands it on, or the person"
                    .to_owned()
            ),
            "a session taking another's quest is refused"
        );
        assert_eq!(
            orders_harbour(&socket, 7002).await,
            Err(
                "OSPHT4 doesn't orchestrate QEST-20261115-HARBOR by this node's record, so it \
                 can't order its carry: only WCJQSG's session could"
                    .to_owned()
            ),
            "and orders nothing"
        );
        assert_eq!(
            claims(&socket, 7001, None, &[]).await,
            Ok(Response::Orchestrated),
            "the steward releases what it held"
        );
        assert_eq!(
            claims(
                &socket,
                7002,
                Some("QEST-20261108-OSPHT4"),
                &["QEST-20261115-HARBOR"]
            )
            .await,
            Ok(Response::Orchestrated),
            "and another session may take it up"
        );
        let kept = chain::Orchestrations::read(&socket.with_file_name("orchestrations.json"))
            .expect("the record reads");
        assert_eq!(
            kept.sessions.keys().collect::<Vec<_>>(),
            ["s-weigh"],
            "the node keeps what it was told"
        );
    }

    #[tokio::test]
    async fn a_session_whose_log_continues_an_orchestrator_takes_on_its_quests() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let (_stop, _node, _client) = orchestrated(&socket, &orchestrations(), &[]).await;
        steward_claims(&socket).await;
        let quests = ["QEST-20261108-OSPHT4", "QEST-20261115-HARBOR"];
        assert!(
            claims(&socket, 7002, Some("QEST-20261007-WCJQSG"), &quests)
                .await
                .is_err(),
            "a session that doesn't continue the steward takes nothing of its"
        );
        let logs = socket.with_file_name("sessions").join("--home-joel--");
        fs::create_dir_all(&logs).expect("made");
        fs::write(
            logs.join("2026-11-15T17-00-00-000Z_s-weigh.jsonl"),
            "{\"type\":\"session\",\"id\":\"s-weigh\",\"parentSession\":\"s-steward\"}\n",
        )
        .expect("written");
        assert_eq!(
            claims(&socket, 7002, Some("QEST-20261007-WCJQSG"), &quests).await,
            Ok(Response::Orchestrated),
            "one whose log continues it does"
        );
    }

    /// The steers waiting in `inbox`, as JSON.
    fn held_in(inbox: &Path) -> Vec<Value> {
        fs::read_dir(inbox)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|it| it == "json"))
            .map(|entry| {
                serde_json::from_str(&fs::read_to_string(entry.path()).expect("it reads"))
                    .expect("it is JSON")
            })
            .collect()
    }

    #[tokio::test]
    async fn a_carry_ordered_is_shown_standing_as_its_session_is_seen() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node, mut client) =
            orchestrated_in(&store, &socket, &orchestrations(), &[]).await;
        steward_claims(&socket).await;
        client
            .ask(json!({"id": 3, "ask": "watch", "kind": "carry"}))
            .await;
        assert_eq!(
            client.next().await["records"],
            json!([]),
            "none ordered yet"
        );
        let stands = |said: &Value| said["changed"][0]["state"].clone();

        assert!(
            orders_harbour(&socket, 7001).await.is_ok(),
            "WCJQSG orders HARBOR's carry"
        );
        let ordered = client.next().await;
        let record = &ordered["changed"][0];
        assert_eq!(
            (
                &record["by"],
                &record["address"],
                &record["to"],
                &record["why"]
            ),
            (
                &json!("WCJQSG"),
                &json!("HARBOR"),
                &json!("tern"),
                &json!("it needs the camera")
            ),
            "the carry as kept: {ordered}"
        );
        assert_eq!(stands(&ordered), json!("ordered"), "not gone yet");
        assert_eq!(
            (
                &record["machine"],
                record["key"]
                    .as_str()
                    .map(|key| key.split(':').take(3).collect::<Vec<_>>())
            ),
            (&json!("skiff"), Some(vec!["carry", "skiff", "s-harbour"])),
            "kept by skiff's node, which its key names before the session: {record}"
        );
        let told = store.told_carries();
        assert_eq!(
            told["skiff"]
                .iter()
                .map(|carry| (&carry["session"], &carry["left"]))
                .collect::<Vec<_>>(),
            [(&json!("s-harbour"), &json!(false))],
            "told to the machines skiff links as soon as it is ordered"
        );

        let presence = |busy: bool| {
            json!({"session": "s-harbour", "addresses": ["HARBOR"], "groups": [],
                   "busy": busy, "state": "listening",
                   "at": jiff::Timestamp::now().to_string(),
                   "quest": "QEST-20261115-HARBOR"})
        };
        store.start_here("s-harbour", presence(false), false);
        assert_eq!(
            stands(&client.next().await),
            json!("carrying"),
            "its session ended here"
        );
        store
            .put_presence("s-harbour", "tern", &presence(true))
            .expect("the presence is kept");
        assert_eq!(
            stands(&client.next().await),
            json!("arrived"),
            "its session runs on tern"
        );
    }

    /// A `pi-carry` beside `socket`, where the node there asks it, that
    /// settles at once, letting two trees go, after writing down what it
    /// was given, a line each time, in the file it answers.
    fn stand_in_settling(socket: &Path) -> PathBuf {
        // The node's own, as a socket's folder must be.
        fs::DirBuilder::new()
            .mode(0o700)
            .create(socket.parent().expect("its folder"))
            .expect("its folder is made");
        let given = socket.with_file_name("given");
        let text = socket.with_file_name("pi-carry.sh");
        fs::write(
            &text,
            format!(
                "#!/bin/sh\necho \"$*\" >> {}\necho '{{\"kind\":\"settled\",\"let\":2}}'\n",
                given.display()
            ),
        )
        .expect("the stand-in is written");
        // A child process puts it in place, since another test's fork
        // holding the file open for writing would keep it from running.
        let put = std::process::Command::new("install")
            .args(["-m", "755"])
            .arg(&text)
            .arg(socket.with_file_name("pi-carry"))
            .status()
            .expect("install runs");
        assert!(put.success(), "the stand-in is put in place");
        given
    }

    #[tokio::test]
    async fn a_carry_arrived_a_day_since_lets_the_trees_it_left_here_go() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let given = stand_in_settling(&socket);
        let two_days_ago =
            (jiff::Timestamp::now() - jiff::SignedDuration::from_hours(48)).to_string();
        let carried = |session: &str, address: &str, from: &str| {
            json!({"at": two_days_ago, "by": "WCJQSG", "session": session, "address": address,
                   "quest": format!("QEST-20261115-{address}"), "from": from, "to": "tern",
                   "why": "it needs the camera"})
            .to_string()
        };
        fs::write(
            socket.with_file_name("carries.jsonl"),
            format!(
                "{}\n{}\n",
                carried("s-harbour", "HARBOR", "skiff"),
                carried("s-loomer", "LOOMER", "kestrel")
            ),
        )
        .expect("the carries are kept");
        let store = Memory::default();
        let (_stop, _node) = node(&store, "skiff", Vec::<Piped>::new(), None, &socket);
        let mut client = Speaking::to(&socket).await;
        client.ask(json!({"id": 1, "ask": "hello"})).await;
        client.next().await;
        // Once the node has started, as a link carries it, since a machine
        // no link claims as the node starts is marked out of reach.
        store
            .put_machine(&fenced("tern", &["remote"]))
            .expect("what it said is kept");
        client
            .ask(json!({"id": 2, "ask": "watch", "kind": "carry"}))
            .await;
        assert_eq!(
            client.next().await["records"],
            json!([]),
            "ordered more than a day ago, so not shown"
        );

        for (session, address) in [("s-harbour", "HARBOR"), ("s-loomer", "LOOMER")] {
            store
                .put_presence(
                    session,
                    "tern",
                    &json!({"session": session, "addresses": [address], "groups": [],
                            "busy": true, "state": "listening",
                            "at": jiff::Timestamp::now().to_string(),
                            "quest": format!("QEST-20261115-{address}")}),
                )
                .expect("the presence is kept");
        }
        let settled = tokio::time::timeout(PATIENCE, async {
            loop {
                let said = client.next().await;
                if let Some(record) = said["changed"]
                    .as_array()
                    .and_then(|changed| changed.iter().find(|one| one["settle"].is_object()))
                {
                    return record.clone();
                }
            }
        })
        .await
        .expect("its settling is shown in time");
        assert_eq!(
            (
                &settled["session"],
                &settled["state"],
                &settled["settle"]["did"],
                &settled["settle"]["let"]
            ),
            (
                &json!("s-harbour"),
                &json!("arrived"),
                &json!("settled"),
                &json!(2)
            ),
            "HARBOR's old trees here were let go, shown for a day after: {settled}"
        );
        assert_eq!(
            fs::read_to_string(&given).expect("it ran"),
            "settle QEST-20261115-HARBOR --session s-harbour\n",
            "asked once, and never for a carry kestrel ordered"
        );
    }

    #[tokio::test]
    async fn a_carry_another_machine_told_of_is_shown_as_its_session_is_seen_here() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let now = jiff::Timestamp::now();
        let at = now.to_string();
        let told = |session: &str| {
            json!({"at": at, "by": "WCJQSG", "session": session, "address": "LOOMER",
                   "quest": "QEST-20261115-LOOMER", "from": "kestrel", "to": "tern",
                   "why": "it needs the lens", "left": true})
        };
        let two_days = 2 * 24 * 60 * 60 * 1000;
        for (machine, linked, session) in [
            ("kestrel", now.as_millisecond(), "s-loomer"),
            ("osprey", now.as_millisecond() - two_days, "s-gone"),
        ] {
            store
                .mark_linked(machine, linked)
                .expect("the time is kept");
            store
                .put_told_carries(machine, &[told(session)])
                .expect("the carries are kept");
        }
        let (_stop, _node, mut client) =
            orchestrated_in(&store, &socket, &orchestrations(), &[]).await;
        client
            .ask(json!({"id": 3, "ask": "watch", "kind": "carry"}))
            .await;
        let records = client.next().await["records"].clone();
        assert_eq!(
            records
                .as_array()
                .expect("records")
                .iter()
                .map(|record| (
                    &record["key"],
                    &record["machine"],
                    &record["state"],
                    &record["why"]
                ))
                .collect::<Vec<_>>(),
            [(
                &json!(format!("carry:kestrel:s-loomer:{at}")),
                &json!("kestrel"),
                &json!("carrying"),
                &json!("it needs the lens")
            )],
            "kestrel's, gone from there; osprey's, a day out of reach, isn't shown"
        );
        store
            .put_presence(
                "s-loomer",
                "tern",
                &json!({"session": "s-loomer", "addresses": ["LOOMER"], "groups": [],
                        "busy": true, "state": "listening", "at": at,
                        "quest": "QEST-20261115-LOOMER"}),
            )
            .expect("the presence is kept");
        assert_eq!(
            client.next().await["changed"][0]["state"],
            json!("arrived"),
            "skiff sees it run on tern"
        );
        let mut changes = store.changes().expect("a memory store tells its changes");
        let let_go = tokio::time::timeout(PATIENCE, async {
            while store.told_carries().contains_key("osprey") {
                changes.changes.recv().await;
            }
        })
        .await;
        assert!(let_go.is_ok(), "osprey's carries are let go");
        assert!(
            store.told_carries().contains_key("kestrel"),
            "kestrel's stay"
        );
    }

    #[tokio::test]
    async fn only_an_orchestrator_by_the_quests_record_orders_a_carry() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let (_stop, _node, _client) = orchestrated(&socket, &orchestrations(), &[]).await;
        steward_claims(&socket).await;
        let inbox = inboxes(&socket).join("HARBOR");
        let held = || held_in(&inbox);
        let kept = socket.with_file_name("carries.jsonl");
        let order = |said: Value| {
            ask(
                socket.clone(),
                Request::Steer {
                    address: "harbor".to_owned(),
                    steer: said,
                },
            )
        };
        let ordered = json!({"act": "ordered-carry", "to": "tern",
                             "why": "it needs the camera", "from": "person", "by": "OSPHT4"});

        asks_below(&socket, 7002);
        assert_eq!(
            order(ordered.clone()).await,
            Err(
                "OSPHT4 doesn't orchestrate QEST-20261115-HARBOR by this node's record, so it \
                 can't order its carry: only WCJQSG's session could"
                    .to_owned()
            ),
            "a session whose presence claims the orchestrator's quest is refused"
        );
        assert_eq!(
            held(),
            Vec::<Value>::new(),
            "nothing refused reaches the inbox"
        );
        assert!(!kept.exists(), "and nothing refused is kept as carried");

        asks_below(&socket, 7001);
        assert_eq!(
            order(json!({"act": "carry", "to": "tern", "from": "person"})).await,
            Err(
                "this asks from mycelium-pi-f3d9.scope, an agent's, and an agent can't speak \
                 as the person: only the person's own login says what they say"
                    .to_owned()
            ),
            "the orchestrator still can't carry as the person"
        );
        assert_eq!(
            order(ordered).await,
            Ok(Response::Steered {
                machine: "skiff".to_owned(),
                by: Some("WCJQSG".to_owned()),
            }),
            "its orchestrator, by the record, orders the carry"
        );
        assert_eq!(
            held(),
            [
                json!({"act": "ordered-carry", "to": "tern", "why": "it needs the camera",
                    "by": "WCJQSG", "from": "WCJQSG"})
            ],
            "under the orchestrator's name, never the person's or the one the client gave"
        );
        let line: Value = serde_json::from_str(
            fs::read_to_string(&kept)
                .expect("the carry is kept")
                .trim_end(),
        )
        .expect("one JSON line");
        assert_eq!(
            (
                &line["by"],
                &line["asker"],
                &line["address"],
                &line["session"],
                &line["quest"],
                &line["from"],
                &line["to"],
                &line["why"],
            ),
            (
                &json!("WCJQSG"),
                &json!("s-steward"),
                &json!("HARBOR"),
                &json!("s-harbour"),
                &json!("QEST-20261115-HARBOR"),
                &json!("skiff"),
                &json!("tern"),
                &json!("it needs the camera"),
            ),
            "kept saying who ordered which carry, from where to where, and why: {line}"
        );
        assert_eq!(
            (&line["chain"], &line["source"]),
            (
                &json!([{"quest": "QEST-20261115-HARBOR", "orchestrator": "WCJQSG"}]),
                &json!(chain::SOURCE),
            ),
            "and the chain it was taken on, and where that came from, so a forged one is \
             traced: {line}"
        );
    }

    #[tokio::test]
    async fn a_persons_carry_keeps_nothing_where_no_quests_record_holds_its_session() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let mut quests = orchestrations();
        quests["quests"][2]["sessions"] = json!([]);
        let (_stop, _node, _client) = orchestrated(&socket, &quests, &[]).await;
        asks_as_the_person(&socket);
        assert_eq!(
            ask(
                socket.clone(),
                Request::Steer {
                    address: "harbor".to_owned(),
                    steer: json!({"act": "carry", "to": "tern", "from": "person"}),
                },
            )
            .await,
            Ok(Response::Steered {
                machine: "skiff".to_owned(),
                by: None,
            }),
            "the person carries it"
        );
        assert!(
            !socket.with_file_name("carries.jsonl").exists(),
            "with nothing kept, since HARBOR's presence claims a quest whose record doesn't \
             hold it, and a presence's word names no quest to settle"
        );
    }

    #[tokio::test]
    async fn a_carry_the_persons_own_login_asks_is_kept_as_theirs() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let (_stop, _node, _client) = orchestrated(&socket, &orchestrations(), &[]).await;
        let kept = socket.with_file_name("carries.jsonl");
        let carry = || {
            ask(
                socket.clone(),
                Request::Steer {
                    address: "harbor".to_owned(),
                    steer: json!({"act": "carry", "to": "tern", "from": "person"}),
                },
            )
        };

        asks_below(&socket, 7001);
        assert!(carry().await.is_err(), "an agent can't carry as the person");
        assert!(!kept.exists(), "so nothing is kept as the person's carry");

        asks_as_the_person(&socket);
        assert!(
            ask(
                socket.clone(),
                Request::Steer {
                    address: "harbor".to_owned(),
                    steer: json!({"act": "carry", "to": "skiff", "from": "person"}),
                },
            )
            .await
            .is_ok(),
            "the person asks it to go where it is"
        );
        assert!(
            !kept.exists(),
            "which keeps nothing, since settling would let go of trees it still works in"
        );
        assert_eq!(
            carry().await,
            Ok(Response::Steered {
                machine: "skiff".to_owned(),
                by: None,
            }),
            "the person carries it"
        );
        let line: Value = serde_json::from_str(
            fs::read_to_string(&kept)
                .expect("the person's carry is kept")
                .trim_end(),
        )
        .expect("one JSON line");
        assert_eq!(
            (
                &line["by"],
                &line["address"],
                &line["session"],
                &line["quest"],
                &line["from"],
                &line["to"],
            ),
            (
                &json!("person"),
                &json!("HARBOR"),
                &json!("s-harbour"),
                &json!("QEST-20261115-HARBOR"),
                &json!("skiff"),
                &json!("tern"),
            ),
            "kept as the person's, as the node told who asked: {line}"
        );
        assert!(
            line.get("why").is_none() && line.get("chain").is_none(),
            "the person gives no reason and is ordered by no chain: {line}"
        );

        assert!(
            ask(
                socket.clone(),
                Request::Steer {
                    address: "harbor".to_owned(),
                    steer: json!({"act": "pause", "from": "person"}),
                },
            )
            .await
            .is_ok(),
            "the person pauses it"
        );
        assert_eq!(
            fs::read_to_string(&kept).expect("it reads").lines().count(),
            1,
            "and only a carry is kept as one"
        );
    }

    #[tokio::test]
    async fn a_carry_back_over_a_link_is_kept_where_its_quest_runs() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (near_socket, far_socket, scratch, nodes) =
            harbour_on_kestrel_linked_to_skiff(dir.path()).await;
        let mut client = Speaking::to(&near_socket).await;
        client.ask(json!({"id": 1, "ask": "hello"})).await;
        client.next().await;
        client
            .ask(
                json!({"id": 2, "ask": "carry", "quest": "QEST-20261115-HARBOR",
                        "to": "skiff"}),
            )
            .await;
        let kept = far_socket.with_file_name("carries.jsonl");
        // Nothing tells the test when kestrel keeps it, so it looks again
        // every few milliseconds, until the carry answers otherwise.
        let keeping = async {
            loop {
                let all = fs::read_to_string(&kept).unwrap_or_default();
                if all.ends_with('\n') {
                    break serde_json::from_str::<Value>(all.trim_end()).expect("one JSON line");
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let line = tokio::time::timeout(PATIENCE, async {
            tokio::select! {
                line = keeping => line,
                answered = client.next() => panic!("it ended before kestrel kept it: {answered}"),
            }
        })
        .await
        .expect("kestrel keeps the carry back");
        assert_eq!(
            (
                &line["by"],
                &line["address"],
                &line["session"],
                &line["quest"],
                &line["from"],
                &line["to"],
            ),
            (
                &json!("person"),
                &json!("HARBOR"),
                &json!("s-harbour"),
                &json!("QEST-20261115-HARBOR"),
                &json!("kestrel"),
                &json!("skiff"),
            ),
            "kept as the person's on kestrel, where its session runs: {line}"
        );
        assert!(
            !near_socket.with_file_name("carries.jsonl").exists(),
            "and once, not on skiff too"
        );

        let host = rustix::system::uname()
            .nodename()
            .to_string_lossy()
            .into_owned();
        let handover = through::handover(&scratch, "QEST-20261115-HARBOR", &host);
        fs::create_dir_all(&handover).expect("its handover is made");
        let refused = json!({"session": "s-harbour", "why": "a push is running",
                             "at": jiff::Timestamp::now().to_string()});
        fs::write(handover.join("s-harbour.refused.json"), refused.to_string())
            .expect("its refusal is written");
        let ended = tokio::time::timeout(PATIENCE, client.next())
            .await
            .expect("the carry ends");
        assert!(ended["error"].is_object(), "refused, so it ends: {ended}");
        drop(client);
        stop(nodes).await;
    }

    /// Skiff's node linked to kestrel's, which shares its quests and runs
    /// HARBOR's session: their sockets, kestrel's scratch, and both nodes.
    async fn harbour_on_kestrel_linked_to_skiff(
        dir: &Path,
    ) -> (PathBuf, PathBuf, PathBuf, [Running; 2]) {
        let far_socket = dir.join("kestrel").join("work.sock");
        let near_socket = dir.join("skiff").join("work.sock");
        let far_store = Memory::default();
        far_store.start_here(
            "s-harbour",
            json!({"session": "s-harbour", "addresses": ["HARBOR"], "groups": [],
                   "busy": true, "state": "working",
                   "at": jiff::Timestamp::now().to_string(),
                   "quest": "QEST-20261115-HARBOR"}),
            true,
        );
        let places = dir.join("places");
        let sharing = Places {
            quests: places.join("quests"),
            scratch: places.join("scratch"),
            home: places.join("home"),
            state: places.join("state"),
        };
        let scratch = sharing.scratch.clone();
        let shares = sharing.clone();
        // Kestrel's record of the quests, which holds HARBOR's session,
        // as the node takes a carry's quest from it.
        fs::DirBuilder::new()
            .mode(0o700)
            .create(dir.join("kestrel"))
            .expect("kestrel's folder, its owner's alone");
        fs::write(
            far_socket.with_file_name("quests.json"),
            orchestrations().to_string(),
        )
        .expect("kestrel's quests written");
        let far = node_sharing(
            &far_store,
            "kestrel",
            Vec::<Piped>::new(),
            None,
            &far_socket,
            Some(sharing),
        );
        let dial = Piped {
            store: far_store,
            socket: far_socket.clone(),
            held: dir.join("held"),
            proven: true,
            asks_back: false,
            shares: Some(shares),
        };
        let near = node(&Memory::default(), "skiff", vec![dial], None, &near_socket);
        listed(near_socket.clone(), "s-harbour").await;
        // The person asks skiff's node, and kestrel's node then asks
        // itself, as its own process.
        asks_as_the_person(&near_socket);
        asks_as_the_person(&far_socket);
        (near_socket, far_socket, scratch, [near, far])
    }

    #[tokio::test]
    async fn a_presence_borrowing_the_orchestrators_session_orders_no_carry() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        // A helper of OSPHT4's writes a presence naming WCJQSG's session
        // as its own, owned by the helper's own process.
        let helper = i32::try_from(std::process::id()).expect("a pid");
        let borrowed = json!({
            "session": "s-steward", "addresses": ["BORROW"], "groups": [],
            "busy": false, "state": "listening", "at": jiff::Timestamp::now().to_string(),
            "owner": {"pid": helper, "startTicks": 1},
        });
        let (_stop, _node, _client) = orchestrated(&socket, &orchestrations(), &[borrowed]).await;
        asks_below(&socket, 7002);
        assert_eq!(
            ask(
                socket.clone(),
                Request::Steer {
                    address: "harbor".to_owned(),
                    steer: json!({"act": "ordered-carry", "to": "tern",
                                  "why": "it needs the camera"}),
                },
            )
            .await,
            Err(
                "two presences here claim the session s-steward, so who owns it can't be told \
                 and no carry is ordered"
                    .to_owned()
            ),
            "a session claimed twice orders nothing"
        );
        assert_eq!(
            held_in(&inboxes(&socket).join("HARBOR")),
            Vec::<Value>::new(),
            "nothing refused reaches the inbox"
        );
    }

    #[tokio::test]
    async fn a_chain_forged_in_the_quests_record_orders_no_carry() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        // OSPHT4 rewrites HARBOR's orchestrator in the quests' record as
        // itself; the mesh still puts HARBOR in WCJQSG's orchestration.
        let mut forged = orchestrations();
        forged["quests"][2]["orchestrator"] = json!("OSPHT4");
        let (_stop, _node, _client) = orchestrated(&socket, &forged, &[]).await;
        steward_claims(&socket).await;
        assert_eq!(
            orders_harbour(&socket, 7002).await,
            Err(
                "OSPHT4 doesn't orchestrate QEST-20261115-HARBOR by this node's record, so it \
                 can't order its carry: only WCJQSG's session could"
                    .to_owned()
            ),
            "a forged orchestrator orders nothing"
        );
        assert_eq!(
            orders_harbour(&socket, 7001).await,
            Err(
                "the quests' record gives QEST-20261115-HARBOR's chain as OSPHT4 then WCJQSG, \
                 but this node's record gives it as WCJQSG, so the chain isn't believed"
                    .to_owned()
            ),
            "nor does the true one while the quests' record disagrees"
        );
        assert_eq!(
            held_in(&inboxes(&socket).join("HARBOR")),
            Vec::<Value>::new(),
            "nothing refused reaches the inbox"
        );
        assert!(
            !socket.with_file_name("carries.jsonl").exists(),
            "nor is kept as carried"
        );
    }

    #[tokio::test]
    async fn a_steer_asked_in_the_daemon_interface_reaches_its_sessions_inbox() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let presence = listening("here", &["HERE12"]);
        store.start_here("here", presence, true);
        // One parked has no process, and reads its inbox as it wakes.
        let parked = json!({
            "session": "napper", "addresses": ["NAPPER"], "groups": [],
            "busy": false, "state": "parked", "at": jiff::Timestamp::now().to_string(),
        });
        store.start_here("napper", parked, false);
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let mut client = Speaking::greeted(&socket).await;

        let said = json!({"act": "pause", "from": "joel", "at": "2026-11-07T18:00:00Z"});
        client
            .ask(json!({"id": 2, "ask": "steer", "address": "here12", "steer": said}))
            .await;
        let steered = client.next().await;
        assert_eq!(
            (&steered["id"], &steered["steered"]["machine"]),
            (&json!(2), &json!("skiff")),
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
            (&json!(4), &json!("skiff")),
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
    async fn an_update_asked_in_the_daemon_interface_answers_how_it_ended() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let done = json!({"step": "done", "commit": "c0ffee", "applied": "now",
            "restart": null, "restarted": []});
        let store = Memory::default();
        let (_stop, _node) = alone(&store, "skiff", &socket);
        // The node has made its folder by now, as only it may.
        stand_in_updater(&updater_of(&socket), done.clone());
        let mut client = Speaking::greeted(&socket).await;

        client
            .ask(json!({"id": 2, "ask": "update", "machine": "skiff"}))
            .await;
        let updated = client.next().await;
        assert_eq!(
            (&updated["id"], &updated["updated"]),
            (&json!(2), &done),
            "its updater's ending comes back, as `Update` answers it: {updated}"
        );

        client.ask(json!({"id": 3, "ask": "update"})).await;
        assert_eq!(
            client.next().await["error"]["code"],
            json!("not-an-ask"),
            "an update for no machine isn't an ask"
        );
        client
            .ask(json!({"id": 4, "ask": "update", "machine": "nowhere"}))
            .await;
        assert_eq!(
            client.next().await["error"]["code"],
            json!("refused"),
            "an update for a machine no link reaches is refused"
        );
    }

    #[tokio::test]
    async fn a_watch_of_sessions_is_sent_each_session_that_arrives() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let mut client = Speaking::greeted(&socket).await;

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
            .put_presence("there", "kestrel", &presence)
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
    async fn a_watch_of_events_is_sent_each_thing_a_session_says_it_did() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let (mut client, hello) = Speaking::hello(&socket).await;
        assert_eq!(
            hello["kinds"]["event"]["live"],
            json!([]),
            "an event never changes: {hello}"
        );

        client
            .ask(json!({"id": 2, "ask": "watch", "kind": "event"}))
            .await;
        assert_eq!(
            client.next().await,
            json!({"id": 2, "records": []}),
            "no session has done anything yet"
        );
        let presence = json!({
            "session": "there", "addresses": ["THERE1"], "groups": [],
            "state": "busy", "at": jiff::Timestamp::now().to_string(),
            "events": [{"at": "2026-11-15T15:40:00Z", "kind": "merge",
                "where": "github:Jitsusama/mycelium", "title": "Serve what waits"}],
        });
        store
            .put_presence("there", "kestrel", &presence)
            .expect("the presence is kept");
        let arrived = client.next().await;
        let event = &arrived["changed"][0];
        assert_eq!(
            (
                &event["key"],
                &event["cell"],
                &event["session"],
                &event["what"],
                &event["where"],
                &event["title"],
            ),
            (
                &json!("event:there:2026-11-15T15:40:00Z"),
                &json!("THERE1"),
                &json!("there"),
                &json!("merge"),
                &json!("github:Jitsusama/mycelium"),
                &json!("Serve what waits"),
            ),
            "an event arrives with its session's presence, as its cell: {arrived}"
        );
    }

    #[tokio::test]
    async fn a_cell_working_on_a_quest_is_served_its_forage_as_it_changes() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let quests = dir.path().join("quests");
        let quest = quests.join("QEST-20261116-SKIFFS");
        fs::create_dir_all(quest.join("plans")).expect("its folders");
        fs::write(
            quest.join("README.md"),
            "---\nid: QEST-20261116-SKIFFS\nstatus: active\n---\n# Row\n\n## Summary\n\nSkiff rows to tern. Then back.\n",
        )
        .expect("its README");
        let plan = quest.join("plans").join("PLAN-20261116-ROWING.md");
        let written = |steps: &str| {
            fs::write(
                &plan,
                format!("---\nid: PLAN-20261116-ROWING\nkind: plan\nstage: build\n---\n# Row to Tern\n\n{steps}"),
            )
            .expect("its plan");
        };
        written("- [x] push off\n- [ ] row\n");
        let places = Places {
            quests,
            scratch: dir.path().join("scratch"),
            home: dir.path().to_owned(),
            state: dir.path().join("state"),
        };
        let store = Memory::default();
        let (_stop, _node) = node_sharing(
            &store,
            "skiff",
            Vec::<Piped>::new(),
            None,
            &socket,
            Some(places),
        );
        let (mut client, hello) = Speaking::hello(&socket).await;
        assert!(
            hello["kinds"]["forage"]["live"].is_array(),
            "hello names forages among the kinds it holds: {hello}"
        );
        client
            .ask(json!({"id": 2, "ask": "watch", "kind": "forage"}))
            .await;
        assert_eq!(
            client.next().await,
            json!({"id": 2, "records": []}),
            "no cell works on a quest yet"
        );
        let presence = json!({
            "session": "rower", "addresses": ["ROWER1"], "groups": [],
            "state": "busy", "at": jiff::Timestamp::now().to_string(),
            "quest": "QEST-20261116-SKIFFS", "orchestratedBy": "TERN01",
        });
        store
            .put_presence("rower", "skiff", &presence)
            .expect("the presence is kept");
        let forage = loop {
            let arrived = client.next().await;
            let changed = arrived["changed"].as_array().cloned().unwrap_or_default();
            if let Some(forage) = changed
                .into_iter()
                .find(|one| one["key"] == "forage:ROWER1")
            {
                break forage;
            }
        };
        assert_eq!(
            (
                &forage["quest"],
                &forage["purpose"],
                &forage["lineage"]["started_by"],
                &forage["documents"][0]["ticked"],
            ),
            (
                &json!("QEST-20261116-SKIFFS"),
                &json!("Skiff rows to tern."),
                &json!("TERN01"),
                &json!(1),
            ),
            "the cell's forage is read from its quest: {forage}"
        );
        written("- [x] push off\n- [x] row\n");
        loop {
            let arrived = client.next().await;
            let changed = arrived["changed"].as_array().cloned().unwrap_or_default();
            if changed
                .iter()
                .any(|one| one["key"] == "forage:ROWER1" && one["documents"][0]["ticked"] == 2)
            {
                break;
            }
        }
    }

    /// Waits on `client`'s watch until a record keyed `key` comes, as
    /// `holds` says it must be, and gives it.
    async fn watched_until(
        client: &mut Speaking,
        key: &str,
        holds: impl Fn(&Value) -> bool,
    ) -> Value {
        loop {
            let said = client.next().await;
            let records = said["records"].as_array().into_iter().flatten();
            let changed = said["changed"].as_array().into_iter().flatten();
            if let Some(one) = records
                .chain(changed)
                .find(|one| one["key"] == key && holds(one))
            {
                break one.clone();
            }
        }
    }

    #[tokio::test]
    async fn a_tree_two_cells_formed_stays_while_one_holds_it_though_the_node_restarts() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let holding = |store: &Memory, session: &str| {
            let presence = json!({
                "session": session, "addresses": [session.to_uppercase()], "groups": [],
                "busy": true, "state": "busy", "at": jiff::Timestamp::now().to_string(),
                "trees": [{"key": "worktree-github-harbour-oars-jsama-tides",
                            "repo": "github:harbour/oars", "branch": "jsama/tides"}],
            });
            store
                .put_presence(session, "kestrel", &presence)
                .expect("the presence is kept");
        };
        let tides = "tree.github:harbour/oars:jsama/tides";
        let network = format!("network:{tides}");
        let cells = |record: &Value, many: usize| {
            record["cells"]
                .as_array()
                .is_some_and(|cells| cells.len() == many)
        };

        let both = Memory::default();
        let (stop, running) = node(&both, "skiff", Vec::<Piped>::new(), None, &socket);
        let mut client = Speaking::to(&socket).await;
        client.ask(json!({"id": 0, "ask": "hello"})).await;
        client.next().await;
        client
            .ask(json!({"id": 1, "ask": "watch", "kind": "network"}))
            .await;
        holding(&both, "reef01");
        holding(&both, "kelp01");
        watched_until(&mut client, &network, |one| cells(one, 2)).await;
        drop(client);
        stop_one((stop, running)).await;
        assert!(
            socket.with_file_name("formed-work.json").exists()
                && !socket.with_file_name("formed.json").exists(),
            "each realm's node keeps its formed networks in a file of its own"
        );

        let one = Memory::default();
        let (_stop, _running) = node(&one, "skiff", Vec::<Piped>::new(), None, &socket);
        let mut client = Speaking::to(&socket).await;
        client.ask(json!({"id": 0, "ask": "hello"})).await;
        client.next().await;
        client
            .ask(json!({"id": 2, "ask": "watch", "kind": "cell"}))
            .await;
        holding(&one, "reef01");
        watched_until(&mut client, "cell:REEF01", |_| true).await;
        client
            .ask(json!({"id": 3, "ask": "list", "kind": "network"}))
            .await;
        let listed = loop {
            let said = client.next().await;
            if said["id"] == 3 {
                break said;
            }
        };
        let kept = listed["records"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|one| one["key"] == network.as_str());
        assert_eq!(
            kept.map(|one| &one["cells"]),
            Some(&json!(["REEF01"])),
            "formed before the node restarted, it stays while one cell holds it: {listed}"
        );
        client
            .ask(json!({"id": 4, "ask": "list", "kind": "formed"}))
            .await;
        let formed = loop {
            let said = client.next().await;
            if said["id"] == 4 {
                break said;
            }
        };
        assert!(
            formed["records"].as_array().is_some_and(|records| {
                records.iter().any(|one| {
                    one["key"] == format!("formed:{tides}") && one["at"].as_i64().is_some()
                })
            }),
            "and the node says when it formed: {formed}"
        );
    }

    #[tokio::test]
    async fn a_tree_two_cells_hold_is_formed_though_only_formed_networks_are_asked() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _running) = node(&store, "skiff", Vec::<Piped>::new(), None, &socket);
        let mut client = Speaking::to(&socket).await;
        client.ask(json!({"id": 0, "ask": "hello"})).await;
        client.next().await;
        client
            .ask(json!({"id": 1, "ask": "watch", "kind": "session"}))
            .await;
        for session in ["reef01", "kelp01"] {
            let presence = json!({
                "session": session, "addresses": [session.to_uppercase()], "groups": [],
                "busy": true, "state": "busy", "at": jiff::Timestamp::now().to_string(),
                "trees": [{"key": "worktree-github-harbour-oars-jsama-tides",
                            "repo": "github:harbour/oars", "branch": "jsama/tides"}],
            });
            store
                .put_presence(session, "kestrel", &presence)
                .expect("the presence is kept");
        }
        watched_until(&mut client, "session:kelp01", |_| true).await;
        client
            .ask(json!({"id": 2, "ask": "list", "kind": "formed"}))
            .await;
        let formed = loop {
            let said = client.next().await;
            if said["id"] == 2 {
                break said;
            }
        };
        assert!(
            formed["records"].as_array().is_some_and(|records| {
                records
                    .iter()
                    .any(|one| one["key"] == "formed:tree.github:harbour/oars:jsama/tides")
            }),
            "asked alone, formed networks are made first: {formed}"
        );
    }

    /// Stops one node, as [`stop`] stops two.
    async fn stop_one((stop, node): Running) {
        drop(stop);
        let stopped = node.await.expect("the node runs to its end");
        assert!(stopped.is_ok(), "the node stops cleanly: {stopped:?}");
    }

    #[tokio::test]
    async fn a_watch_of_networks_is_sent_each_network_as_the_networks_view_makes_it() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let at = jiff::Timestamp::now().to_string();
        let put = |session: &str, groups: &[&str]| {
            let presence = json!({
                "session": session, "addresses": [session.to_uppercase()], "groups": groups,
                "busy": true, "state": "busy", "at": at,
            });
            store
                .put_presence(session, "kestrel", &presence)
                .expect("the presence is kept");
        };
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let (mut client, hello) = Speaking::hello(&socket).await;
        assert!(
            hello["kinds"]["network"]["live"].is_array(),
            "hello names networks among the kinds it holds: {hello}"
        );

        client
            .ask(json!({"id": 2, "ask": "watch", "kind": "network"}))
            .await;
        let whole = client.next().await;
        assert!(
            whole["records"].is_array(),
            "the records come whole first: {whole}"
        );
        put("reef01", &["#repo/skiff", "#peer/reef01"]);
        put("kelp01", &["#repo/skiff", "#harbor"]);
        let mut seen = std::collections::BTreeSet::new();
        while !(seen.contains("network:repo.skiff") && seen.contains("network:harbor")) {
            let changed = client.next().await;
            let keys = changed["changed"].as_array().into_iter().flatten();
            seen.extend(keys.filter_map(|one| one["key"].as_str().map(str::to_owned)));
        }

        let mut listed = Vec::new();
        for (id, kind) in [(3, "team"), (4, "session"), (5, "machine"), (6, "network")] {
            client
                .ask(json!({"id": id, "ask": "list", "kind": kind}))
                .await;
            let answer = client.next().await;
            listed.push(answer["records"].as_array().cloned().unwrap_or_default());
        }
        let made = crate::networks::Mycelium::of("work", &listed[0], &listed[1], &listed[2]);
        // This machine's own measure may come between the lists, so the
        // machines' network is left out of what is compared.
        let expected: BTreeMap<String, Value> = made
            .networks
            .iter()
            .filter(|network| network.name != crate::networks::MACHINES)
            .map(|network| (format!("network:{}", network.name), json!(network)))
            .collect();
        assert!(
            expected.contains_key("network:repo.skiff") && expected.contains_key("network:harbor"),
            "the sessions share a repository and a named group: {expected:?}"
        );

        assert!(
            listed[3]
                .iter()
                .all(|record| record["heard"].as_i64() > Some(0)),
            "each network is heard as its cells last were: {:?}",
            listed[3]
        );
        let served: BTreeMap<String, Value> = listed[3]
            .iter()
            .filter(|record| record["key"] != "network:machines")
            .map(|record| {
                let mut fields = record.clone();
                let key = fields["key"].as_str().unwrap_or_default().to_owned();
                if let Some(fields) = fields.as_object_mut() {
                    fields.remove("key");
                    fields.remove("heard");
                }
                (key, fields)
            })
            .collect();
        assert_eq!(
            served, expected,
            "one record each, by its name, with the fields the networks view makes"
        );

        put("skiff01", &["#repo/skiff"]);
        let mut changed = client.next().await;
        let skiff = |changed: &Value| {
            changed["changed"]
                .as_array()
                .and_then(|each| each.iter().find(|one| one["key"] == "network:repo.skiff"))
                .cloned()
        };
        // The machines' network, counting every session, may be told first.
        while skiff(&changed).is_none() {
            changed = client.next().await;
        }
        assert_eq!(
            skiff(&changed).map(|record| record["cells"].clone()),
            Some(json!(["KELP01", "REEF01", "SKIFF01"])),
            "a session joining a network is sent as what changed in it: {changed}"
        );
    }

    #[tokio::test]
    async fn a_watch_of_cells_is_sent_each_cell_as_the_networks_view_makes_it() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let put = |session: &str, state: &str, groups: &[&str]| {
            store
                .put_presence(session, "kestrel", &planning(session, state, groups))
                .expect("the presence is kept");
        };
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let (mut client, hello) = Speaking::hello(&socket).await;
        assert!(
            hello["kinds"]["cell"]["live"].is_array(),
            "hello names cells among the kinds it holds: {hello}"
        );

        client
            .ask(json!({"id": 2, "ask": "watch", "kind": "cell"}))
            .await;
        let whole = client.next().await;
        assert!(
            whole["records"].is_array(),
            "the records come whole first: {whole}"
        );
        put("reef01", "busy", &["#repo/skiff", "#peer/reef01"]);
        put("kelp01", "busy", &["#repo/skiff", "#harbor"]);
        let mut seen = std::collections::BTreeSet::new();
        while !(seen.contains("cell:REEF01") && seen.contains("cell:KELP01")) {
            let changed = client.next().await;
            let keys = changed["changed"].as_array().into_iter().flatten();
            seen.extend(keys.filter_map(|one| one["key"].as_str().map(str::to_owned)));
        }

        let mut listed = Vec::new();
        for (id, kind) in [(3, "team"), (4, "session"), (5, "machine"), (6, "cell")] {
            client
                .ask(json!({"id": id, "ask": "list", "kind": kind}))
                .await;
            let answer = client.next().await;
            listed.push(answer["records"].as_array().cloned().unwrap_or_default());
        }
        let made = crate::networks::Mycelium::of("work", &listed[0], &listed[1], &listed[2]);
        // This machine's own measure may come between the lists, so the
        // machines' cells are left out of what is compared.
        let sessions = |record: &Value| record["type"] == "session";
        let expected: BTreeMap<String, Value> = made
            .cells
            .iter()
            .map(|cell| (format!("cell:{}", cell.name), json!(cell)))
            .filter(|(_, cell)| sessions(cell))
            .collect();
        assert!(
            expected["cell:REEF01"]["plan"]["next"] == "serve the cells",
            "the networks view draws what a session's plan does next: {expected:?}"
        );
        assert!(
            listed[3]
                .iter()
                .all(|record| record["heard"].as_i64() > Some(0)),
            "each cell is heard as its record was: {:?}",
            listed[3]
        );
        assert_eq!(
            served(listed[3].iter().filter(|record| sessions(record))),
            expected,
            "one record each, by its name, with the fields the networks view draws"
        );

        put("kelp01", "idle", &["#repo/skiff", "#harbor"]);
        let mut changed = client.next().await;
        while sent(&changed, "cell:KELP01").is_none() {
            changed = client.next().await;
        }
        let keys: Vec<&Value> = changed["changed"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|one| one["key"] != "cell:skiff")
            .map(|one| &one["key"])
            .collect();
        assert_eq!(
            keys,
            [&json!("cell:KELP01")],
            "a session's change is sent as its cell alone: {changed}"
        );

        client
            .ask(json!({"id": 7, "ask": "list", "kind": "cell", "fields": ["standing"]}))
            .await;
        let projected = client.next().await;
        assert_eq!(
            keyed(&projected["records"], "cell:KELP01"),
            Some(json!({"key": "cell:KELP01", "kind": "cell", "standing": "quiet"})),
            "a list naming fields is sent only those of each cell: {projected}"
        );
    }

    /// Writes beside the node at `socket` that this test's own process
    /// reads Slack for the work realm, as a receptor says so.
    fn slack_read_beside(socket: &Path) {
        slack_asking_beside(socket, 1);
    }

    /// A slack receptor reading beside `socket`, `addressed` of its five
    /// today asking its person.
    fn slack_asking_beside(socket: &Path, addressed: u32) {
        let now = jiff::Timestamp::now();
        let pid = std::process::id();
        let reading = receptor::Record {
            source: receptor::Source::Slack,
            realm: "work".to_owned(),
            state: receptor::State::Reading,
            since: now,
            read_at: now,
            every: 300,
            pid,
            started: receptor::started_at(pid),
            counts: receptor::Counts {
                hour: 2,
                today: 5,
                addressed,
            },
        };
        let folder = socket.with_file_name("receptors");
        receptor::write(&folder, "work", reading, &receptor::Here).expect("the receptor is kept");
    }

    /// A presence of `session` in `state` hearing `groups`, saying what its
    /// plan does next.
    fn planning(session: &str, state: &str, groups: &[&str]) -> Value {
        let at = jiff::Timestamp::now().to_string();
        json!({
            "session": session, "addresses": [session.to_uppercase()], "groups": groups,
            "busy": state == "busy", "state": state, "at": at,
            "cwd": format!("/home/someone/src/{session}"), "repo": "skiff",
            "doing": {"plan": "PLAN-20261116-AAAAAA", "item": 2,
                "next": "serve the cells", "at": at},
        })
    }

    /// Each of `records` by its key, holding only the fields a cell of the
    /// networks view says.
    fn served<'a>(records: impl Iterator<Item = &'a Value>) -> BTreeMap<String, Value> {
        records
            .map(|record| {
                let mut fields = record.clone();
                let key = fields["key"].as_str().unwrap_or_default().to_owned();
                if let Some(fields) = fields.as_object_mut() {
                    fields.remove("key");
                    fields.remove("heard");
                }
                (key, fields)
            })
            .collect()
    }

    /// The record of `key` among `records`, if any.
    fn keyed(records: &Value, key: &str) -> Option<Value> {
        records
            .as_array()
            .and_then(|each| each.iter().find(|one| one["key"] == key))
            .cloned()
    }

    /// The record of `key` among what a watch's `changed` sent, if any.
    fn sent(changed: &Value, key: &str) -> Option<Value> {
        keyed(&changed["changed"], key)
    }

    #[tokio::test]
    async fn a_receptor_reading_is_sent_to_a_watch_of_cells_at_once() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let mut client = Speaking::greeted(&socket).await;
        client
            .ask(json!({"id": 2, "ask": "watch", "kind": "cell"}))
            .await;
        client.next().await;

        slack_read_beside(&socket);
        // No session changes, so only the receptor's reading tells the
        // watch.
        let slack = |changed: &Value| sent(changed, "cell:slack@skiff");
        let mut changed = client.next().await;
        while slack(&changed).is_none() {
            changed = client.next().await;
        }
        let cell = slack(&changed).unwrap_or_default();
        assert_eq!(
            (&cell["type"], &cell["state"], &cell["networks"]),
            (
                &json!("receptor"),
                &json!("reading"),
                &json!(["outside.slack"])
            ),
            "a receptor's reading is sent to a watch of cells as its cell: {changed}"
        );
    }

    #[tokio::test]
    async fn the_source_a_receptor_senses_is_a_network_the_node_serves() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let mut client = Speaking::greeted(&socket).await;
        client
            .ask(json!({"id": 2, "ask": "watch", "kind": "receptor"}))
            .await;
        client.next().await;

        slack_read_beside(&socket);
        while sent(&client.next().await, "receptor:slack").is_none() {}
        client
            .ask(json!({"id": 3, "ask": "get", "keys": ["network:outside.slack"]}))
            .await;
        let got = client.next().await;
        assert_eq!(
            (&got["records"][0]["shape"], &got["records"][0]["cells"]),
            (&json!("outside"), &json!(["slack@skiff"])),
            "the source a receptor senses is a network the node serves: {got}"
        );
    }

    /// A message to `outside.slack` from `from`, written as the store holds
    /// one: its id and its body.
    fn to_outside_slack(from: &str, text: &str) -> (String, Value) {
        let draft = Draft {
            from: from.to_owned(),
            groups: vec![Group::new("outside.slack").expect("a valid group")],
            intent: Some(Intent::Fyi),
            text: text.to_owned(),
            re: None,
            realm: None,
        };
        mesh::written(&draft, 1)
    }

    #[tokio::test]
    async fn a_receptors_climb_the_table_names_is_said_as_its_cell_and_a_planted_one_isnt() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        fs::create_dir_all(dir.path().join("skiff")).expect("the node's folder");
        fs::set_permissions(dir.path().join("skiff"), fs::Permissions::from_mode(0o700))
            .expect("the node's folder is its own");
        fs::write(
            socket.with_file_name("stimuli.json"),
            r#"{"say": ["slack"]}"#,
        )
        .expect("the table is written");
        let store = Memory::default();
        let (planted, body) =
            to_outside_slack("slack@skiff", "slack: 9 asked of you, up from 0; 9 today");
        store.send_here(&planted, body);
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let mut receptors = Speaking::greeted(&socket).await;
        receptors
            .ask(json!({"id": 2, "ask": "watch", "kind": "receptor"}))
            .await;
        receptors.next().await;
        let mut messages = Speaking::greeted(&socket).await;
        messages
            .ask(json!({"id": 2, "ask": "watch", "kind": "message"}))
            .await;
        let records = messages.next().await["records"].clone();
        assert_eq!(
            records,
            json!([]),
            "a message planted in the store as a receptor was taken"
        );
        let (late, body) =
            to_outside_slack("Slack@skiff", "slack: 8 asked of you, up from 0; 8 today");
        store.send_here(&late, body);
        let (after, body) = to_outside_slack("OSPR01", "heard after");
        store.send_here(&after, body);
        // The planted one went first, so it would be heard with the plain
        // one or before it, never after.
        let mut keys = Vec::new();
        while !keys.contains(&json!(format!("message:{after}"))) {
            let changed = messages.next().await;
            keys.extend(
                changed["changed"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|one| one["key"].clone()),
            );
        }
        assert!(
            !keys.contains(&json!(format!("message:{late}"))),
            "one planted as a receptor while the node ran was taken: {keys:?}"
        );

        slack_asking_beside(&socket, 1);
        while sent(&receptors.next().await, "receptor:slack").is_none() {}
        slack_asking_beside(&socket, 3);
        let changed = messages.next().await;
        let said = &changed["changed"][0];
        assert_eq!(
            (
                &said["from"],
                &said["groups"],
                &said["intent"],
                &said["text"]
            ),
            (
                &json!("slack@skiff"),
                &json!(["outside.slack"]),
                &json!("fyi"),
                &json!("slack: 3 asked of you, up from 1; 5 today")
            ),
            "a climb the table names wasn't said as its receptor's cell: {changed}"
        );
        assert!(
            said["re"].is_null(),
            "a receptor's signal answered something: {changed}"
        );
    }

    #[tokio::test]
    async fn a_client_cant_send_as_a_receptor() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let answered = tokio::task::spawn_blocking(move || {
            let mut sender = Client::connect(&socket).expect("the sender connects");
            let forged = Draft {
                from: "slack@skiff".to_owned(),
                groups: vec![Group::new("outside.slack").expect("a valid group")],
                intent: Some(Intent::Fyi),
                text: "slack: 9 asked of you, up from 0; 9 today".to_owned(),
                re: None,
                realm: None,
            };
            sender
                .ask(&Request::Send(forged))
                .expect("the node answers")
        });
        let answered = tokio::time::timeout(PATIENCE, answered)
            .await
            .expect("the node answers in time")
            .expect("the client runs");
        assert!(
            answered.is_err(),
            "a client sent as a receptor: {answered:?}"
        );
        assert!(
            store.message_ids(0).is_empty(),
            "a forged receptor's message was stored"
        );
    }

    #[tokio::test]
    async fn a_watch_asked_when_told_moves_holds_back_what_only_an_open_launcher_shows() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let mut client = Speaking::greeted(&socket).await;
        let at = jiff::Timestamp::now().to_string();
        let presence = |session: &str, step: &str, title: &str| {
            json!({
                "session": session, "addresses": [session.to_uppercase()], "groups": [],
                "busy": true, "state": "busy", "step": step, "at": at, "title": title,
            })
        };
        let put_as = |session, step, title| {
            store
                .put_presence(session, "kestrel", &presence(session, step, title))
                .expect("the presence is kept");
        };
        let put = |step, title| put_as("there", step, title);

        // A shut launcher's watch, as the shell asks for one.
        let shut = json!({"id": 4, "ask": "watch", "kind": "session", "when": ["told"]});
        client.ask(shut).await;
        assert_eq!(
            client.next().await["records"],
            json!([]),
            "no session runs yet"
        );
        put("read", "one");
        let came = client.next().await;
        assert_eq!(
            came["changed"][0]["key"],
            json!("session:there"),
            "a session coming is sent: {came}"
        );
        put_as("beside", "read", "other");
        client.next().await;

        put("edit", "one");
        put_as("beside", "edit", "other");
        put("edit", "two");
        let retitled = client.next().await;
        assert_eq!(
            (
                retitled["changed"].as_array().map(Vec::len),
                &retitled["changed"][0]["presence"]["step"],
                &retitled["changed"][0]["presence"]["title"]
            ),
            (Some(1), &json!("edit"), &json!("two")),
            "its step wasn't sent alone, but with its retitling, \
             and the other's step stayed held back: {retitled}"
        );

        put("look", "two");
        // The launcher opening asks for every change, under the same id.
        client
            .ask(json!({"id": 4, "ask": "watch", "kind": "session"}))
            .await;
        let opened = client.next().await;
        let step = |key: &str| {
            opened["changed"]
                .as_array()
                .and_then(|changed| changed.iter().find(|record| record["key"] == key))
                .map(|record| record["presence"]["step"].clone())
        };
        assert_eq!(
            (
                opened.get("records"),
                step("session:there"),
                step("session:beside")
            ),
            (None, Some(json!("look")), Some(json!("edit"))),
            "asked again, it sends all it held back at once, not everything whole: {opened}"
        );
    }

    #[tokio::test]
    async fn a_watch_asked_deep_hears_only_what_moved_in_a_presence() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let (mut client, hello) = Speaking::hello(&socket).await;
        assert_eq!(
            (
                &hello["kinds"]["session"]["deep"],
                hello["kinds"]["machine"].get("deep")
            ),
            (&json!(["presence"]), None),
            "hello names what a session offers deep, and nothing for a kind offering none"
        );
        let put = |busy: bool, step: &str, at: &str| {
            let presence = json!({
                "session": "kestrel", "addresses": ["KESTREL"], "groups": ["#quest/skiff"],
                "busy": busy, "step": step, "at": at, "title": "Hear the Tide",
                "worksOn": "charting the skiff's tides",
            });
            store
                .put_presence("kestrel", "osprey", &presence)
                .expect("the presence is kept");
        };
        let at = |seconds: i64| {
            jiff::Timestamp::now()
                .checked_add(jiff::SignedDuration::from_secs(seconds))
                .expect("a time")
                .to_string()
        };
        put(true, "read", &at(0));
        let shut = json!({
            "id": 4, "ask": "watch", "kind": "session", "when": ["told"],
            "deep": ["presence", "standing"],
        });
        client.ask(shut).await;
        client.next().await;

        let (stepped, ticked) = (at(1), at(2));
        put(true, "edit", &stepped);
        put(false, "edit", &ticked);
        let ended = client.next().await;
        assert_eq!(
            &ended["changed"][0]["presence"],
            &json!({"busy": false, "step": "edit", "at": ticked}),
            "its working ending is told shut, with only what moved in its presence: {ended}"
        );

        put(false, "look", &at(3));
        // The same deep in another order, and said twice, is the same watch.
        client
            .ask(json!({
                "id": 4, "ask": "watch", "kind": "session",
                "deep": ["standing", "presence", "presence"],
            }))
            .await;
        let opened = client.next().await;
        assert_eq!(
            (
                opened.get("records"),
                opened["changed"][0]["presence"].get("worksOn"),
                &opened["changed"][0]["presence"]["step"]
            ),
            (None, None, &json!("look")),
            "asked again with the same deep, in any order, what it held back comes deep too, \
             not every record whole: {opened}"
        );
    }

    #[tokio::test]
    async fn a_watch_of_sessions_says_how_each_stands_and_hears_a_park_marked() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
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
        let (_stop, _node) = alone(&store, "skiff", &socket);
        let mut client = Speaking::greeted(&socket).await;

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
    async fn a_cell_here_is_dormant_only_once_its_park_is_marked() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let now = jiff::Timestamp::now().to_string();
        let boot = store::folders::boot_id();
        let parked = json!({
            "session": "napper", "addresses": ["NAPPER"], "groups": [],
            "busy": false, "state": "parked", "at": now, "owner": {"bootId": boot},
        });
        store.start_here("napper", parked, false);
        let (_stop, _node) = node(&store, "skiff", Vec::<Piped>::new(), None, &socket);
        let mut client = Speaking::to(&socket).await;
        client.ask(json!({"id": 1, "ask": "hello"})).await;
        client.next().await;
        client
            .ask(json!({"id": 2, "ask": "watch", "kind": "cell"}))
            .await;
        let records = client.next().await;
        assert!(
            records["records"].is_array(),
            "the cells come whole first: {records}"
        );
        let napper = |records: &Value, field: &str| -> Option<Value> {
            records[field]
                .as_array()?
                .iter()
                .find(|record| record["key"] == "cell:NAPPER")
                .map(|record| record["standing"].clone())
        };
        assert_eq!(
            napper(&records, "records"),
            None,
            "ended with nothing marked to wake it, it's no cell: {records}"
        );

        fs::write(parks_of(&socket).join("napper.json"), "{}").expect("a marker");
        // The machine's own cell may change first, as it's measured again.
        let mut marked = client.next().await;
        while napper(&marked, "changed").is_none() {
            marked = client.next().await;
        }
        assert_eq!(
            napper(&marked, "changed"),
            Some(json!("dormant")),
            "its park marked, it's dormant: {marked}"
        );
    }

    /// Serves `quests` to the node at `socket`, as the harness writes them:
    /// beside, then renamed into place.
    fn serve_quests(socket: &Path, quests: &Value) {
        let beside = socket.with_file_name(".quests.json.new");
        fs::write(&beside, quests.to_string()).expect("the quests are written");
        fs::rename(&beside, socket.with_file_name("quests.json")).expect("renamed into place");
    }

    /// Reads what `watching` is told until it has listed each of `keys`.
    async fn until_listed(watching: &mut Speaking, keys: &[&str]) {
        let mut listed = HashSet::new();
        while listed.len() < keys.len() {
            let said = watching.next().await;
            for record in ["records", "changed"]
                .iter()
                .filter_map(|field| said[*field].as_array())
                .flatten()
            {
                if let Some(key) = record["key"].as_str().filter(|key| keys.contains(key)) {
                    listed.insert(key.to_owned());
                }
            }
        }
    }

    #[tokio::test]
    async fn a_session_here_that_ended_on_an_orchestrated_quest_is_told_while_the_quest_is_active()
    {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let now = jiff::Timestamp::now().to_string();
        let boot = store::folders::boot_id();
        let presence = |session: &str, state: &str| {
            json!({
                "session": session, "addresses": [session.to_uppercase()], "groups": [],
                "busy": false, "state": state, "at": now, "owner": {"bootId": boot},
            })
        };
        let (_stop, _node) = node(&store, "skiff", Vec::<Piped>::new(), None, &socket);
        let (mut client, hello) = Speaking::hello(&socket).await;
        assert_eq!(
            hello["kinds"]["ended"],
            json!({"live": []}),
            "hello names the kind, whose records never change: {hello}"
        );
        client
            .ask(json!({"id": 3, "ask": "watch", "kind": "quest"}))
            .await;
        client.next().await;
        let quests = json!({"quests": [{
            "id": "QEST-20261116-HARBOR", "address": "HARBOR", "status": "active",
            "orchestrator": "oars", "sessions": ["napper", "sleeper"],
        }]});
        serve_quests(&socket, &quests);
        client.next().await;
        let mut sessions = Speaking::greeted(&socket).await;
        sessions
            .ask(json!({"id": 4, "ask": "watch", "kind": "session"}))
            .await;
        // Parked, its process ended, and not marked: nothing can wake it.
        store.start_here("napper", presence("napper", "parked"), false);
        // Parked and marked this boot: its holder wakes it.
        fs::write(parks_of(&socket).join("sleeper.json"), "{}").expect("a marker");
        store.start_here("sleeper", presence("sleeper", "parked"), false);
        // The node holds both before the ended kind is first built, so
        // that build is when it first sees them end.
        until_listed(&mut sessions, &["session:napper", "session:sleeper"]).await;

        client
            .ask(json!({"id": 2, "ask": "watch", "kind": "ended"}))
            .await;
        // Whole first, then as it changes, since the node may hear the
        // session after the watch began.
        let mut said = client.next().await;
        assert!(
            said["records"].is_array(),
            "the records come whole first: {said}"
        );
        let napper = |said: &Value| {
            ["records", "changed"]
                .iter()
                .filter_map(|field| said[*field].as_array())
                .flatten()
                .find(|record| record["key"] == "ended:napper")
                .cloned()
        };
        // Seen ended, and settling at once here, it is told.
        let told = loop {
            assert!(
                ["records", "changed"]
                    .iter()
                    .filter_map(|field| said[*field].as_array())
                    .flatten()
                    .all(|record| record["key"] != "ended:sleeper"),
                "a park its holder can wake is never told: {said}"
            );
            if let Some(told) = napper(&said) {
                break told;
            }
            said = client.next().await;
        };
        assert_eq!(
            (&told["orchestrator"], &told["why"], &told["quest"]),
            (
                &json!("OARS"),
                &json!("parked-unwakeable"),
                &json!("QEST-20261116-HARBOR")
            ),
            "a park nothing can wake is told to the quest's orchestrator: {told}"
        );

        // The quest concluded, its stopping is nothing to carry on. The
        // quests changing alone builds the kind again.
        let mut concluded = quests;
        concluded["quests"][0]["status"] = json!("concluded");
        serve_quests(&socket, &concluded);
        // Well within the minute, which builds it again too.
        let changed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let changed = client.next().await;
                if changed["gone"]
                    .as_array()
                    .is_some_and(|gone| !gone.is_empty())
                {
                    break changed;
                }
            }
        })
        .await
        .expect("the quests changing builds the kind again");
        assert_eq!(
            changed["gone"],
            json!(["ended:napper"]),
            "once the quest isn't active, nobody is told its work stopped: {changed}"
        );
    }

    #[tokio::test]
    async fn another_machines_node_starts_joins_and_steers_its_sessions_for_this_one() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (holder, given) = stand_in_holder(dir.path());
        let far_socket = dir.path().join("kestrel").join("work.sock");
        let near_socket = dir.path().join("skiff").join("work.sock");
        let near_steers = inboxes(&near_socket);
        let far_steers = inboxes(&far_socket);
        let far_store = Memory::default();
        let holds = Starter {
            program: holder,
            scope: start::Scope::None,
            kit: None,
        };
        let nobody = Vec::<Piped>::new();
        let far = node(&far_store, "kestrel", nobody, Some(holds), &far_socket);
        let near_store = Memory::default();
        let held = hold_a_session(dir.path(), &far_store);
        let dial = Piped {
            store: far_store,
            socket: far_socket,
            held,
            proven: false,
            asks_back: false,
            shares: None,
        };
        let near = node(&near_store, "skiff", vec![dial], None, &near_socket);
        known_to(&near_store, "kestrel").await;
        let carried = listed(near_socket.clone(), "0e50").await;
        assert_eq!(carried.held.as_deref(), Some("5e551011"));
        assert_eq!(
            carried.attach,
            Some(
                [
                    "into-kestrel",
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

        let there = Need::Machine("kestrel".to_owned());
        let asked = Request::Start(Start {
            work: work(vec![there]),
            folder: Some(dir.path().to_owned()),
            command: vec!["pi".to_owned()],
            may_cost: false,
        });
        let started = ask(near_socket.clone(), asked).await;
        let Ok(Response::Started(started)) = started else {
            panic!("kestrel's node starts it: {started:?}");
        };
        assert_eq!(started.machine, "kestrel");
        assert_eq!(
            started.attach,
            [
                "into-kestrel",
                "mycelium",
                "--realm",
                "work",
                "attach",
                &started.held
            ],
            "its words join it through the dial that reaches kestrel"
        );
        let given = fs::read_to_string(&given).expect("kestrel's holder was started");
        assert!(
            given.lines().any(|word| word == started.held),
            "under the id its node answered: {given}"
        );

        steers_the_session_on_kestrel(&near_socket, &near_steers, &far_steers).await;

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
            })
            .into(),
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
            session_held(&presence, "skiff", &[hold("0d1e", &join)]).expect("a session here");
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
        let alone =
            session_held(&presence, "skiff", &[hold("0d1e", &fell_back)]).expect("a session here");
        assert_eq!(
            (alone.held, alone.window),
            (None, None),
            "a join fallen back to plain pi on its log holds a session of its own, not this"
        );
    }

    #[tokio::test]
    async fn a_conversation_a_pi_host_holds_is_joined_through_its_host() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let presence = json!({
            "session": "01a1",
            "addresses": ["4UHC5Q"],
            "at": "2026-11-09T19:00:00Z",
            "owner": {"pid": 4242},
            "hosted": {"host": "work-attended", "conversation": "c-01a1"},
        });
        store.start_here("01a1", presence, true);
        let (stop, node) = alone(&store, "skiff", &socket);
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
        let far_socket = dir.path().join("kestrel").join("work.sock");
        let near_socket = dir.path().join("skiff").join("work.sock");
        let far_store = Memory::default();
        let far = alone(&far_store, "kestrel", &far_socket);
        let holds = Starter {
            program: holder,
            scope: start::Scope::None,
            kit: None,
        };
        let dial = Piped {
            store: far_store.clone(),
            socket: far_socket.clone(),
            held: dir.path().join("held"),
            proven: false,
            asks_back: true,
            shares: None,
        };
        let near_store = Memory::default();
        let near = node(&near_store, "skiff", vec![dial], Some(holds), &near_socket);
        // Nothing tells the test when kestrel's node has heard of
        // skiff, or when its link has taken up what the node asks of it,
        // so it asks again every few milliseconds until a start goes
        // through.
        let there = Need::Machine("skiff".to_owned());
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
        .expect("kestrel starts work on skiff over the link skiff dialled");
        assert_eq!(started.machine, "skiff");
        assert_eq!(
            started.attach,
            [
                "mycelium",
                "--realm",
                "work",
                "attach",
                "--on",
                "skiff",
                &started.held,
                "--",
                "ssh",
                "-t",
                "--",
                "skiff",
                "mycelium",
                "--realm",
                "work",
                "attach",
                &started.held
            ],
            "kestrel never dialled skiff, so its words join it over SSH by name, \
             joining again here each time SSH drops"
        );
        let given = fs::read_to_string(&given).expect("skiff's holder was started");
        assert!(
            given.lines().any(|word| word == started.held),
            "under the id skiff's node answered: {given}"
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

    /// Runs kestrel's node and skiff's, linked, kestrel's end
    /// `proven` or not, once skiff has heard what kestrel is.
    async fn linked_to_kestrel(dir: &Path, proven: bool) -> (PathBuf, [Running; 2]) {
        linked_to_kestrel_in(&Memory::default(), dir, proven).await
    }

    /// As [`linked_to_kestrel`], keeping what skiff knows in `near_store`.
    async fn linked_to_kestrel_in(
        near_store: &Memory,
        dir: &Path,
        proven: bool,
    ) -> (PathBuf, [Running; 2]) {
        let far_socket = dir.join("kestrel").join("work.sock");
        let near_socket = dir.join("skiff").join("work.sock");
        let far_store = Memory::default();
        let far = alone(&far_store, "kestrel", &far_socket);
        let dial = Piped {
            store: far_store,
            socket: far_socket,
            held: dir.join("held"),
            proven,
            asks_back: false,
            shares: None,
        };
        let near = node(near_store, "skiff", vec![dial], None, &near_socket);
        known_to(near_store, "kestrel").await;
        // The test asks both nodes, as the person would.
        asks_as_person(&near_socket);
        asks_as_person(&dir.join("kestrel").join("work.sock"));
        (near_socket, [near, far])
    }

    #[tokio::test]
    async fn a_machine_updates_when_asked_over_a_link_its_key_proved_and_only_then() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let done = json!({"step": "done", "commit": "c0ffee", "applied": "now",
            "restart": null, "restarted": []});
        let (near_socket, nodes) = linked_to_kestrel(dir.path(), true).await;
        // kestrel's node has made its folder by now, as only it may.
        stand_in_updater(
            &updater_of(&dir.path().join("kestrel").join("work.sock")),
            done.clone(),
        );
        let update = |machine: &str| Request::Update {
            machine: machine.to_owned(),
        };
        let updated = ask(near_socket.clone(), update("kestrel")).await;
        assert_eq!(
            updated,
            Ok(Response::Updated(done)),
            "its node asks its updater, and the ending comes back"
        );
        let here = ask(near_socket, update("skiff")).await;
        assert!(
            here.as_ref().is_err_and(
                |why| why.starts_with("no updater answers at") && why.contains("update.sock")
            ),
            "a machine with no updater says where it looked: {here:?}"
        );
        stop(nodes).await;

        let dir = tempfile::tempdir().expect("a temporary folder");
        let (near_socket, nodes) = linked_to_kestrel(dir.path(), false).await;
        let refused = ask(near_socket, update("kestrel")).await;
        assert_eq!(
            refused,
            Err(
                "kestrel updates only when asked over a link its key proved, which this one isn't"
                    .to_owned()
            ),
            "a link nothing proved can't ask"
        );
        stop(nodes).await;
    }

    /// A process that is waited on as it ends, as a holder's parent waits
    /// on it, so it never lingers as a zombie that still seems to run.
    fn reaped(mut child: std::process::Child) -> u32 {
        let pid = child.id();
        std::thread::spawn(move || child.wait());
        pid
    }

    /// Whether process `pid` still runs.
    fn runs(pid: u32) -> bool {
        i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .is_some_and(|it| rustix::process::test_kill_process(it).is_ok())
    }

    /// Holds a session in `folder` as `id`, its holder and its program
    /// each a process that ends as it's told to, answering their pids.
    fn holds_for_real(folder: &Path, id: &str) -> (u32, u32) {
        use std::os::unix::process::CommandExt;
        let sleeps = || {
            let mut sleep = std::process::Command::new("sleep");
            sleep.arg("1000");
            sleep
        };
        let holder = reaped(sleeps().spawn().expect("the holder starts"));
        let program = reaped(
            sleeps()
                .process_group(0)
                .spawn()
                .expect("the program starts"),
        );
        fs::create_dir_all(folder).expect("the held folder is made");
        let record = hold::Held {
            id: id.to_owned(),
            command: vec!["pi".to_owned()],
            folder: folder.to_owned(),
            holder,
            pid: program,
            parked: false,
            since: "2026-11-16T15:00:00Z".to_owned(),
            windows: Vec::new(),
        };
        let record = serde_json::to_vec(&record).expect("a record");
        fs::write(folder.join(format!("{id}.json")), record).expect("the record is written");
        let answers = std::os::unix::net::UnixListener::bind(folder.join(format!("{id}.sock")))
            .expect("its socket answers");
        // It answers for as long as the test runs, as a live holder's does.
        std::mem::forget(answers);
        (holder, program)
    }

    // Joel (2026-11-16): a held session its owner couldn't end, KQL3KB,
    // is ended from here by the person, or by its orchestrator.
    #[tokio::test]
    async fn the_person_ends_a_session_held_here() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let (_stop, _node) = alone(&Memory::default(), "skiff", &socket);
        let (holder, program) = holds_for_real(&held_of(&socket), "e0d1");
        asks_as_person(&socket);

        let ended = ask(
            socket.clone(),
            Request::End {
                held: "e0d1".to_owned(),
            },
        )
        .await;

        assert_eq!(
            ended,
            Ok(Response::Ended {
                machine: "skiff".to_owned(),
                held: "e0d1".to_owned(),
                ending: hold::end::Ending::Gently,
            }),
            "it ends when told to"
        );
        assert!(
            !runs(holder) && !runs(program),
            "its holder and program are gone"
        );
        assert!(
            hold::held(&held_of(&socket)).is_empty(),
            "and nothing says it's held"
        );
        let again = ask(
            socket,
            Request::End {
                held: "e0d1".to_owned(),
            },
        )
        .await;
        assert_eq!(
            again,
            Err("nothing is held as e0d1 here, nor on any machine in reach".to_owned()),
            "what isn't held isn't ended"
        );
    }

    #[tokio::test]
    async fn an_end_is_refused_to_a_stranger_saying_why() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let (_stop, _node) = alone(&Memory::default(), "skiff", &socket);
        let (holder, program) = holds_for_real(&held_of(&socket), "e0d2");
        // The client runs in this process, which the stand-in for /proc
        // tells as an agent of no session this node knows.
        let asker = procs_of(&socket).join(std::process::id().to_string());
        fs::create_dir_all(&asker).expect("its folder is made");
        fs::write(
            asker.join("cgroup"),
            "0::/user.slice/user-1000.slice/user@1000.service/agents.slice/\
             pi-host-work-background.service\n",
        )
        .expect("its group is written");

        let refused = ask(
            socket.clone(),
            Request::End {
                held: "e0d2".to_owned(),
            },
        )
        .await;

        assert!(
            refused.as_ref().is_err_and(|why| why.starts_with(
                "only the person or the orchestrator of the session it holds ends a hold, and \
                 neither asks: "
            )),
            "a stranger is refused, saying why: {refused:?}"
        );
        assert!(
            runs(holder) && runs(program),
            "what it asked to end runs on"
        );
        assert_eq!(hold::held(&held_of(&socket)).len(), 1, "and is still held");
        for pid in [holder, program] {
            let _ = rustix::process::kill_process(
                rustix::process::Pid::from_raw(i32::try_from(pid).expect("a pid")).expect("a pid"),
                rustix::process::Signal::KILL,
            );
        }
    }

    /// Runs kestrel's node and skiff's, linked, kestrel's end `proven` or
    /// not, with a session held on kestrel as `e0d3`, once skiff lists it
    /// as held there; answering skiff's socket, kestrel's held folder, and
    /// the hold's holder and program.
    async fn held_on_kestrel(
        dir: &Path,
        proven: bool,
    ) -> (PathBuf, PathBuf, (u32, u32), [Running; 2]) {
        let far_socket = dir.join("kestrel").join("work.sock");
        let near_socket = dir.join("skiff").join("work.sock");
        let far_held = dir.join("held");
        let pids = holds_for_real(&far_held, "e0d3");
        let far_store = Memory::default();
        let presence = json!({
            "session": "0e53",
            "addresses": ["SAK9FZ"],
            "at": jiff::Timestamp::now().to_string(),
            "owner": {"pid": pids.1},
        });
        far_store.start_here("0e53", presence, true);
        let far = alone(&far_store, "kestrel", &far_socket);
        let dial = Piped {
            store: far_store,
            socket: far_socket,
            held: far_held.clone(),
            proven,
            asks_back: false,
            shares: None,
        };
        let near = node(&Memory::default(), "skiff", vec![dial], None, &near_socket);
        let socket = near_socket.clone();
        let listed = tokio::task::spawn_blocking(move || {
            let mut client = Client::connect(&socket).expect("the client connects");
            // Nothing tells a client when the link has said what kestrel
            // holds, so it asks again every few milliseconds until it has.
            loop {
                let Ok(Ok(Response::Sessions(all))) = client.ask(&Request::Sessions) else {
                    panic!("the node lists its sessions");
                };
                if all.iter().any(|one| one.held.as_deref() == Some("e0d3")) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        tokio::time::timeout(PATIENCE, listed)
            .await
            .expect("skiff hears what kestrel holds in time")
            .expect("the client asks");
        asks_as_person(&near_socket);
        (near_socket, far_held, pids, [near, far])
    }

    #[tokio::test]
    async fn an_end_asked_here_ends_the_session_held_on_the_machine_holding_it() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (socket, far_held, (holder, program), nodes) = held_on_kestrel(dir.path(), true).await;

        let ended = ask(
            socket,
            Request::End {
                held: "e0d3".to_owned(),
            },
        )
        .await;

        assert_eq!(
            ended,
            Ok(Response::Ended {
                machine: "kestrel".to_owned(),
                held: "e0d3".to_owned(),
                ending: hold::end::Ending::Gently,
            }),
            "kestrel ends it, and says so"
        );
        assert!(
            !runs(holder) && !runs(program),
            "its holder and program are gone"
        );
        assert!(
            hold::held(&far_held).is_empty(),
            "and nothing says it's held there"
        );
        stop(nodes).await;

        let dir = tempfile::tempdir().expect("a temporary folder");
        let (socket, far_held, (holder, program), nodes) = held_on_kestrel(dir.path(), false).await;
        let refused = ask(
            socket,
            Request::End {
                held: "e0d3".to_owned(),
            },
        )
        .await;
        assert_eq!(
            refused,
            Err(
                "kestrel ends a held session only when asked over a link its key proved, which \
                 this one isn't"
                    .to_owned()
            ),
            "a link nothing proved can't end it"
        );
        assert!(runs(holder) && runs(program), "so it runs on");
        assert_eq!(hold::held(&far_held).len(), 1, "held as it was");
        for pid in [holder, program] {
            let _ = rustix::process::kill_process(
                rustix::process::Pid::from_raw(i32::try_from(pid).expect("a pid")).expect("a pid"),
                rustix::process::Signal::KILL,
            );
        }
        stop(nodes).await;
    }

    /// What a run asked of the node at `socket` came to: the node's first
    /// answer, then all it wrote to its output and its errors, and how it
    /// ended, if it ran.
    #[derive(Debug, Default)]
    struct Came {
        out: Vec<u8>,
        err: Vec<u8>,
        ended: Option<Ran>,
    }

    /// A run of `script` through `/bin/sh` on `machine`.
    fn run_of(machine: &str, script: &str) -> Run {
        Run {
            work: Work {
                needs: vec![Need::Machine(machine.to_owned())],
                prefers: Vec::new(),
                tolerates: Vec::new(),
                may_wait: false,
                lasts: None,
            },
            // A full path, since a build's home is no folder at all.
            folder: Some(std::env::temp_dir()),
            command: ["/bin/sh", "-c", script].map(str::to_owned).to_vec(),
            by: None,
            from: None,
        }
    }

    /// Asks the node at `socket` for `run` and reads all it says, or why
    /// it won't run.
    async fn run_through(socket: PathBuf, run: Run) -> Result<Came, String> {
        let asked = tokio::task::spawn_blocking(move || {
            let mut client = Client::connect(&socket).expect("the client connects");
            client.ask(&Request::Run(run)).expect("the node answers")?;
            let mut came = Came::default();
            while let Some(ran) = client.next_ran().expect("the node says more") {
                match ran {
                    Ran::Out(piece) => came.out.extend(run::bytes(&piece).expect("base64")),
                    Ran::Err(piece) => came.err.extend(run::bytes(&piece).expect("base64")),
                    ended => {
                        came.ended = Some(ended);
                        break;
                    }
                }
            }
            Ok(came)
        });
        tokio::time::timeout(PATIENCE, asked)
            .await
            .expect("the run ends in time")
            .expect("the client asks")
    }

    #[tokio::test]
    async fn a_run_here_hands_back_what_it_writes_and_its_own_exit_code() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let running = alone(&store, "skiff", &socket);
        asks_as_person(&socket);
        let came = run_through(
            socket.clone(),
            run_of("skiff", "echo out; echo err >&2; exit 7"),
        )
        .await
        .expect("it runs here");
        assert_eq!(came.out, b"out\n");
        assert_eq!(came.err, b"err\n");
        assert_eq!(came.ended, Some(Ran::Exited(7)));
        let nowhere = run_through(socket.clone(), run_of("kestrel", "true")).await;
        assert!(
            nowhere
                .as_ref()
                .is_err_and(|why| why.starts_with("nowhere")),
            "a machine the realm doesn't have takes nothing: {nowhere:?}"
        );
        let nothing = run_through(
            socket,
            Run {
                command: Vec::new(),
                ..run_of("skiff", "")
            },
        )
        .await;
        assert_eq!(
            nothing.err(),
            Some("nothing to run: name a command".to_owned())
        );
        let (stop, node) = running;
        drop(stop);
        let stopped = node.await.expect("the node runs to its end");
        assert!(stopped.is_ok(), "the node stops cleanly: {stopped:?}");
    }

    #[tokio::test]
    async fn a_node_given_a_kit_runs_with_its_tools_first_and_names_them() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let kit = dir.path().join("kit");
        fs::create_dir_all(kit.join("bin")).expect("the kit's bin is made");
        fs::write(kit.join("bin/gcloud"), "").expect("the kit holds gcloud");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let starter = Starter {
            // Nothing is held here, only run.
            program: PathBuf::from("/bin/false"),
            scope: start::Scope::None,
            kit: Some(start::Kit::Carried(kit.clone())),
        };
        let running = node(&store, "skiff", Vec::<Piped>::new(), Some(starter), &socket);
        asks_as_person(&socket);
        let came = run_through(socket.clone(), run_of("skiff", "printf %s \"$PATH\""))
            .await
            .expect("it runs here");
        let path = String::from_utf8_lossy(&came.out);
        assert!(
            path.starts_with(&format!("{}/bin:", kit.display())),
            "the kit's tools come first, after the profile: {path}"
        );
        let Ok(Response::Machine(machine)) = ask(socket, Request::Machine).await else {
            panic!("the node says its machine")
        };
        assert_eq!(
            machine.kit.map(|kit| kit.tools),
            Some(vec!["gcloud".to_owned()]),
            "its machine names what the kit holds"
        );
        let (stop, node) = running;
        drop(stop);
        let stopped = node.await.expect("the node runs to its end");
        assert!(stopped.is_ok(), "the node stops cleanly: {stopped:?}");
    }

    #[tokio::test]
    async fn a_run_on_another_machine_streams_back_over_a_link_its_key_proved_and_only_then() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (near_socket, nodes) = linked_to_kestrel(dir.path(), true).await;
        let came = run_through(
            near_socket.clone(),
            run_of("kestrel", "echo out; echo err >&2; exit 7"),
        )
        .await
        .expect("it runs on kestrel");
        assert_eq!(came.out, b"out\n");
        assert_eq!(came.err, b"err\n");
        assert_eq!(came.ended, Some(Ran::Exited(7)), "its exit code is its own");

        // Far more than the link's window and every channel's backlog hold.
        let flood = run_through(near_socket, run_of("kestrel", "head -c 4194304 /dev/zero"))
            .await
            .expect("a flood runs");
        assert_eq!(flood.out.len(), 4 * 1024 * 1024, "all of it comes back");
        assert_eq!(flood.ended, Some(Ran::Exited(0)));
        stop(nodes).await;

        let dir = tempfile::tempdir().expect("a temporary folder");
        let (near_socket, nodes) = linked_to_kestrel(dir.path(), false).await;
        let refused = run_through(near_socket, run_of("kestrel", "echo ran"))
            .await
            .expect("skiff asks it of kestrel");
        assert_eq!(refused.out, b"", "nothing ran");
        assert_eq!(
            refused.ended,
            Some(Ran::Failed(
                "kestrel runs only what is asked over a link its key proved, which this one isn't"
                    .to_owned()
            )),
            "a link nothing proved can't ask"
        );
        stop(nodes).await;
    }

    #[tokio::test]
    async fn a_run_isnt_asked_of_the_machine_that_dialled() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let far_socket = dir.path().join("kestrel").join("work.sock");
        let near_socket = dir.path().join("skiff").join("work.sock");
        let far_store = Memory::default();
        let far = alone(&far_store, "kestrel", &far_socket);
        let dial = Piped {
            store: far_store.clone(),
            socket: far_socket.clone(),
            held: dir.path().join("held"),
            proven: true,
            asks_back: true,
            shares: None,
        };
        let near = node(&Memory::default(), "skiff", vec![dial], None, &near_socket);
        asks_as_person(&far_socket);
        // Nothing tells the test when kestrel's link has taken up what its
        // node asks of skiff, so it asks again every few milliseconds
        // until the link answers rather than the node.
        let why = tokio::time::timeout(PATIENCE, async {
            loop {
                let asked = run_through(far_socket.clone(), run_of("skiff", "echo ran")).await;
                if let Err(why) = asked
                    && why.contains("a run goes only where this machine dialled")
                {
                    break why;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("kestrel's ask of skiff is refused at the link skiff dialled");
        assert!(why.contains("skiff"), "{why}");
        stop([near, far]).await;
    }

    #[tokio::test]
    async fn an_asker_that_hangs_up_stops_a_run_on_another_machine() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (near_socket, nodes) = linked_to_kestrel(dir.path(), true).await;
        let mark = dir.path().join("pid");
        let script = format!(
            "sleep 600 & echo $! > {}; echo started; wait",
            mark.display()
        );
        let asked = tokio::task::spawn_blocking(move || {
            let mut client = Client::connect(&near_socket).expect("the client connects");
            let running = client.ask(&Request::Run(run_of("kestrel", &script)));
            let first = client.next_ran().expect("the node says more");
            // Dropping the client hangs up.
            (running.expect("the node answers"), first)
        });
        let (running, first) = tokio::time::timeout(PATIENCE, asked)
            .await
            .expect("the run starts in time")
            .expect("the client asks");
        assert_eq!(
            running,
            Ok(Response::Running {
                machine: "kestrel".to_owned()
            })
        );
        assert!(matches!(first, Some(Ran::Out(_))), "{first:?}");
        let pid: i32 = fs::read_to_string(&mark)
            .expect("its child said its pid")
            .trim()
            .parse()
            .expect("a pid");
        let pid = rustix::process::Pid::from_raw(pid).expect("a real pid");
        // Nothing says when the far end has stopped it, so this looks
        // again every few milliseconds.
        tokio::time::timeout(PATIENCE, async {
            while rustix::process::test_kill_process(pid).is_ok() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("what it ran on kestrel is stopped");
        stop(nodes).await;
    }

    #[tokio::test]
    async fn a_fenced_session_runs_nothing_on_looser_ground() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node, _client) =
            orchestrated_in(&store, &socket, &orchestrations(), &[]).await;
        store
            .put_machine(&fenced("tern", &["remote", "cloud"]))
            .expect("what it said is kept");
        let unlinked = Some("no link reaches tern now".to_owned());
        asks_as_person(&socket);
        assert_eq!(
            run_through(socket.clone(), run_of("tern", "true"))
                .await
                .err(),
            unlinked.clone(),
            "the person asks, so it goes as far as the link"
        );
        let looser = Some(
            "this asks as a strictly fenced agent, so it runs nothing on looser ground: \
             tern's strictly fenced session opens builders cloud, which here's doesn't, so \
             the session would land looser there"
                .to_owned(),
        );
        assert_eq!(
            run_through(
                socket.clone(),
                Run {
                    by: Some(By::Agent(crate::declared::Kind::Strict)),
                    ..run_of("tern", "true")
                }
            )
            .await
            .err(),
            looser.clone(),
            "what it says of who asks makes it stricter"
        );
        asks_below(&socket, 7001);
        assert_eq!(
            run_through(socket.clone(), run_of("tern", "true"))
                .await
                .err(),
            looser.clone(),
            "a machine whose fence opens a builder here's doesn't"
        );
        assert_eq!(
            run_through(
                socket.clone(),
                Run {
                    by: Some(By::Person),
                    ..run_of("tern", "true")
                }
            )
            .await
            .err(),
            looser,
            "an agent saying it is the person is still an agent"
        );
        store
            .put_machine(&fenced("tern", &["remote"]))
            .expect("what it said is kept");
        assert_eq!(
            run_through(socket, run_of("tern", "true")).await.err(),
            unlinked,
            "a machine as firm as here's goes as far as the link"
        );
    }

    #[tokio::test]
    async fn a_fenced_agent_is_never_offered_looser_ground() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        let (_stop, _node, _client) =
            orchestrated_in(&store, &socket, &orchestrations(), &[]).await;
        store
            .put_machine(&fenced("tern", &["remote", "cloud"]))
            .expect("what it said is kept");
        let looser = "tern refuses: tern's strictly fenced session opens builders cloud, which \
                      here's doesn't, so the session would land looser there";
        // Work that would rather go to tern, so ranking alone would send it
        // there: only its looser ground keeps it here.
        let toward_tern = Work {
            prefers: vec![Need::Machine("tern".to_owned())],
            ..work(Vec::new())
        };
        let place = |socket: PathBuf| {
            let toward_tern = toward_tern.clone();
            async move {
                let asked = tokio::task::spawn_blocking(move || {
                    Client::connect(&socket)
                        .expect("the client connects")
                        .ask(&Request::Place(toward_tern))
                        .expect("the node answers")
                });
                let answer = tokio::time::timeout(PATIENCE, asked)
                    .await
                    .expect("the node answers in time")
                    .expect("the client asks");
                let Ok(Response::Placement(placement)) = answer else {
                    panic!("a placement: {answer:?}");
                };
                placement
            }
        };
        asks_below(&socket, 7001);
        let Placement::Placed { machine, passed } = place(socket.clone()).await else {
            panic!("here takes it");
        };
        assert_eq!(
            machine, "skiff",
            "an agent is placed here, never on looser ground"
        );
        assert!(
            placement::said(&passed).contains(looser),
            "and is told why not there: {}",
            placement::said(&passed)
        );
        let unnamed = Run {
            work: work(Vec::new()),
            ..run_of("skiff", "true")
        };
        assert_eq!(
            run_through(socket.clone(), unnamed).await.err(),
            Some(
                "this asks as a strictly fenced agent, so it runs here only in that fence: \
                 this machine declares no way to run a command as its strictly fenced \
                 sessions run"
                    .to_owned()
            ),
            "an agent's run naming no machine stays here, on its own ground"
        );
        asks_as_person(&socket);
        let Placement::Placed { passed, .. } = place(socket).await else {
            panic!("somewhere takes the person's");
        };
        assert!(
            !placement::said(&passed).contains("looser"),
            "the person's work isn't held to an agent's ground: {}",
            placement::said(&passed)
        );
    }

    /// Declares, beside `socket`, that its machine runs its sessions
    /// strictly fenced, building only remotely, and starts a strictly
    /// fenced command through `runs`. The test's stand-in for a fence's
    /// launcher is `sh`, whose words end with `--`, as a declared
    /// launcher's must; its script drops that `--` and says
    /// `FENCED=strict` to what it starts.
    fn declares_runs(socket: &Path, runs: bool) {
        declares_launcher(socket, runs.then_some("shift; FENCED=strict exec \"$@\""));
    }

    /// As [`declares_runs`], the launcher's stand-in running `script`, or
    /// no launcher declared where there is none.
    fn declares_launcher(socket: &Path, script: Option<&str>) {
        let folder = socket.parent().expect("its folder");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(folder)
            .expect("its folder is made");
        let mut said = fenced("skiff", &["remote"]).declared.expect("it declares");
        let launcher = if let Some(script) = script {
            let sh = std::env::split_paths(&std::env::var_os("PATH").expect("a PATH"))
                .map(|folder| folder.join("sh"))
                .find(|sh| sh.is_file())
                .expect("sh is on the PATH");
            [&sh.display().to_string(), "-c", script, "fence", "--"]
                .map(ToOwned::to_owned)
                .to_vec()
        } else {
            Vec::new()
        };
        for doors in said.fence.iter_mut().flat_map(BTreeMap::values_mut) {
            doors.runs.clone_from(&launcher);
        }
        fs::write(
            socket.with_file_name("declared.json"),
            serde_json::to_vec(&said).expect("it is JSON"),
        )
        .expect("this machine's declaration is written");
    }

    /// What it ran says as it goes, "unfenced" where no fence started it.
    const SAYS_ITS_FENCE: &str = "echo ${FENCED:-unfenced}";

    /// A launcher's stand-in saying which session it was told it runs
    /// for, as pi-fence reads it to know whose trees the run may stand
    /// in, before it starts what was asked.
    const SAYS_WHOSE: &str =
        "echo \"the fence runs for ${PI_SESSION_ID-none}\"; shift; exec \"$@\"";

    /// An ask that names another session in its own words, inside the
    /// fence.
    const NAMES_ANOTHER: &str = "PI_SESSION_ID=s-other; echo \"it says $PI_SESSION_ID\"";

    #[tokio::test]
    async fn an_agents_run_here_is_fenced_for_the_session_proved_to_ask_and_no_other() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        store.start_here(
            "s-asker",
            json!({"session": "s-asker", "addresses": ["ASKER1"], "groups": [],
                   "busy": false, "state": "listening", "fence": "strict",
                   "at": jiff::Timestamp::now().to_string(),
                   "owner": {"pid": 7001, "startTicks": started_at(7001)}}),
            true,
        );
        let running = alone(&store, "skiff", &socket);
        declares_launcher(&socket, Some(SAYS_WHOSE));
        asks_below(&socket, 7001);
        let unproved = run_through(socket.clone(), run_of("skiff", NAMES_ANOTHER))
            .await
            .expect("an agent's runs here");
        assert_eq!(
            String::from_utf8_lossy(&unproved.out),
            "the fence runs for none\nit says s-other\n",
            "where no log proves who asks, the fence is told no session, \
             whatever the ask's words say"
        );
        holds_log(&socket, 7001, "s-asker");
        let proved = run_through(socket.clone(), run_of("skiff", NAMES_ANOTHER))
            .await
            .expect("an agent's runs here");
        assert_eq!(
            String::from_utf8_lossy(&proved.out),
            "the fence runs for s-asker\nit says s-other\n",
            "the fence is told the session proved to ask, and the ask's words \
             reach only what runs inside it"
        );
        assert_eq!(proved.ended, Some(Ran::Exited(0)));
        let mut far = run_of("skiff", NAMES_ANOTHER);
        far.from = Some("tern".to_owned());
        let relayed = run_through(socket.clone(), far)
            .await
            .expect("a run relayed from another machine runs here");
        assert_eq!(
            String::from_utf8_lossy(&relayed.out),
            "the fence runs for none\nit says s-other\n",
            "a run asked of another machine is told no session here, \
             since this machine proved no one asked it"
        );
        asks_as_person(&socket);
        let person = run_through(socket, run_of("skiff", "echo ${PI_SESSION_ID-none}"))
            .await
            .expect("the person's runs here");
        assert_eq!(
            person.out, b"none\n",
            "the person's run, through no fence, names no session either"
        );
        let (stop, node) = running;
        drop(stop);
        node.await
            .expect("the node runs to its end")
            .expect("it stops cleanly");
    }

    #[tokio::test]
    async fn a_hosts_run_is_fenced_for_none_of_its_sessions_whichever_its_command_names() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let store = Memory::default();
        for session in ["s-one", "s-two"] {
            store.start_here(
                session,
                json!({"session": session, "addresses": [], "groups": [],
                       "busy": false, "state": "listening", "fence": "strict",
                       "at": jiff::Timestamp::now().to_string(),
                       "owner": {"pid": 7101, "startTicks": started_at(7101),
                                 "host": "work"}}),
                true,
            );
        }
        let running = alone(&store, "skiff", &socket);
        declares_launcher(&socket, Some(SAYS_WHOSE));
        asks_below(&socket, 7101);
        holds_log(&socket, 7101, "s-one");
        let hosts = socket.with_file_name("hosts");
        fs::create_dir_all(&hosts).expect("its folder is made");
        let record = hosts.join(format!("7101-{}.json", started_at(7101)));
        let hosting = |sessions: &[&str]| {
            let sessions: Vec<Value> = sessions.iter().map(|id| json!({"id": id})).collect();
            fs::write(
                &record,
                json!({"key": "work", "sessions": sessions,
                       "owner": {"pid": 7101, "startTicks": started_at(7101)}})
                .to_string(),
            )
            .expect("the host's record is written");
        };
        fs::write(
            procs_of(&socket)
                .join(std::process::id().to_string())
                .join("environ"),
            "PI_SESSION_ID=s-one\0",
        )
        .expect("its command names s-one");
        hosting(&["s-one"]);
        let unproved = run_through(socket.clone(), run_of("skiff", "true"))
            .await
            .expect("a hosted agent's runs here");
        assert_eq!(
            String::from_utf8_lossy(&unproved.out),
            "the fence runs for none\n",
            "a host's command is told none of its sessions, though only the one \
             it names proves, since its other still shares the host"
        );
        let fds = procs_of(&socket).join("7101").join("fd");
        fs::rename(fds.join("3"), fds.join("4")).expect("s-one's log is held as 4");
        holds_log(&socket, 7101, "s-two");
        hosting(&["s-one", "s-two"]);
        let proved = run_through(socket.clone(), run_of("skiff", "true"))
            .await
            .expect("a hosted agent's runs here");
        assert_eq!(
            String::from_utf8_lossy(&proved.out),
            "the fence runs for none\n",
            "nor once both prove, whichever its command names"
        );
        let (stop, node) = running;
        drop(stop);
        node.await
            .expect("the node runs to its end")
            .expect("it stops cleanly");
    }

    #[tokio::test]
    async fn an_agents_run_here_runs_only_as_its_fence_says() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("skiff").join("work.sock");
        let running = alone(&Memory::default(), "skiff", &socket);
        asks_as_person(&socket);
        declares_runs(&socket, true);
        let person = run_through(socket.clone(), run_of("skiff", SAYS_ITS_FENCE))
            .await
            .expect("the person's runs here");
        assert_eq!(person.out, b"unfenced\n", "the person runs as themself");
        asks_below(&socket, 7001);
        let agent = run_through(socket.clone(), run_of("skiff", SAYS_ITS_FENCE))
            .await
            .expect("an agent's runs here");
        assert_eq!(agent.out, b"strict\n", "through its fence's launcher");
        assert_eq!(agent.ended, Some(Ran::Exited(0)));
        declares_runs(&socket, false);
        assert_eq!(
            run_through(socket, run_of("skiff", SAYS_ITS_FENCE))
                .await
                .err(),
            Some(
                "this asks as a strictly fenced agent, so it runs here only in that fence: \
                 this machine declares no way to run a command as its strictly fenced \
                 sessions run"
                    .to_owned()
            ),
            "with no launcher declared, an agent runs nothing"
        );
        let (stop, node) = running;
        drop(stop);
        node.await
            .expect("the node runs to its end")
            .expect("it stops cleanly");
    }

    #[tokio::test]
    async fn a_kit_an_agent_could_change_is_left_off_every_path_and_said_to_be() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let kit = dir.path().join("kit");
        fs::create_dir_all(kit.join("bin")).expect("the kit's bin is made");
        fs::write(kit.join("bin/gcloud"), "").expect("the kit holds gcloud");
        let resolved = fs::canonicalize(&kit).expect("the kit resolves");
        let declarers = [0, 65534, rustix::process::getuid().as_raw()];
        // The PATH a person's run and an agent's fenced run each find, with
        // the kit as the node checked it, each after the fence it ran in.
        let paths = |checked: start::Kit| {
            let socket = dir.path().join("skiff").join("work.sock");
            async move {
                let store = Memory::default();
                let starter = Starter {
                    program: PathBuf::from("/bin/false"),
                    scope: start::Scope::None,
                    kit: Some(checked),
                };
                let running = node(&store, "skiff", Vec::<Piped>::new(), Some(starter), &socket);
                asks_as_person(&socket);
                declares_runs(&socket, true);
                let says_its_path = "printf %s \"${FENCED:-unfenced}=$PATH\"";
                let person = run_through(socket.clone(), run_of("skiff", says_its_path))
                    .await
                    .expect("the person's runs here");
                asks_below(&socket, 7001);
                let agent = run_through(socket.clone(), run_of("skiff", says_its_path))
                    .await
                    .expect("an agent's runs here, in its fence");
                let Ok(Response::Machine(machine)) = ask(socket.clone(), Request::Machine).await
                else {
                    panic!("the node says its machine")
                };
                let (stop, node) = running;
                drop(stop);
                node.await
                    .expect("the node runs to its end")
                    .expect("it stops cleanly");
                let _ = fs::remove_file(&socket);
                (
                    String::from_utf8_lossy(&person.out).into_owned(),
                    String::from_utf8_lossy(&agent.out).into_owned(),
                    machine.kit,
                )
            }
        };
        let first = format!("{}/bin:", resolved.display());

        // Given through a link, as a profile is, which is repointed once the
        // node has checked it.
        let profile = dir.path().join("profile");
        std::os::unix::fs::symlink(&kit, &profile).expect("a link to the kit");
        let checked = crate::declared::kit_checked(&profile, &declarers);
        let other = dir.path().join("other");
        fs::create_dir_all(other.join("bin")).expect("another kit");
        fs::remove_file(&profile).expect("the link is taken away");
        std::os::unix::fs::symlink(&other, &profile).expect("and repointed");
        let (person, agent, said) = paths(checked).await;
        assert!(
            person.starts_with(&format!("unfenced={first}"))
                && agent.starts_with(&format!("strict={first}")),
            "a kit only its declarers can change comes first for both, as it was checked: \
             {person} / {agent}"
        );
        assert_eq!(said.map(|kit| kit.tools), Some(vec!["gcloud".to_owned()]));

        fs::set_permissions(kit.join("bin/gcloud"), fs::Permissions::from_mode(0o666))
            .expect("a tool anyone writes");
        let (person, agent, said) = paths(crate::declared::kit_checked(&kit, &declarers)).await;
        let given = format!("{}/bin", kit.display());
        assert!(
            person.starts_with("unfenced=") && agent.starts_with("strict="),
            "one ran as the person, the other in its fence: {person} / {agent}"
        );
        assert!(
            [&first, &given]
                .iter()
                .all(|kit| !person.contains(kit.as_str()) && !agent.contains(kit.as_str())),
            "a kit an agent could change is on neither path, as given or resolved: \
             {person} / {agent}"
        );
        assert_eq!(
            said,
            Some(facts::Kit {
                path: kit.display().to_string(),
                tools: Vec::new(),
                refused: Some(format!(
                    "an agent could change what its tools are: {}/bin/gcloud may be written \
                     by others than its owner",
                    resolved.display()
                )),
            }),
            "its machine names no tools in it, and says why"
        );
    }

    #[tokio::test]
    async fn an_agents_run_on_another_machine_runs_there_as_that_machines_fence_says() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let far_socket = dir.path().join("kestrel").join("work.sock");
        declares_runs(&dir.path().join("skiff").join("work.sock"), false);
        declares_runs(&far_socket, false);
        let near_store = Memory::default();
        let (near_socket, nodes) = linked_to_kestrel_in(&near_store, dir.path(), true).await;
        // kestrel says what it declares as its own account's configuration
        // says it, which the test can't write, and says it again only a
        // minute on, so skiff is told it here.
        let mut kestrel = near_store
            .machines()
            .into_iter()
            .find(|one| one.name == "kestrel")
            .expect("skiff knows kestrel");
        kestrel.declared = fenced("kestrel", &["remote"]).declared;
        near_store
            .put_machine(&kestrel)
            .expect("what it said is kept");
        // On kestrel the test asks as the person, so only what skiff says
        // of who asks fences it.
        asks_below(&near_socket, 7001);
        let refused = run_through(near_socket.clone(), run_of("kestrel", SAYS_ITS_FENCE))
            .await
            .expect("skiff asks it of kestrel");
        assert_eq!(refused.out, b"", "nothing ran");
        assert_eq!(
            refused.ended,
            Some(Ran::Failed(
                "this asks as a strictly fenced agent, so it runs here only in that fence: \
                 this machine declares no way to run a command as its strictly fenced \
                 sessions run"
                    .to_owned()
            )),
            "kestrel runs an agent's command only as its fence says"
        );
        declares_runs(&far_socket, true);
        let fenced = run_through(near_socket, run_of("kestrel", SAYS_ITS_FENCE))
            .await
            .expect("skiff asks it of kestrel");
        assert_eq!(fenced.out, b"strict\n", "through kestrel's launcher");
        assert_eq!(fenced.ended, Some(Ran::Exited(0)));
        stop(nodes).await;
    }

    #[tokio::test]
    async fn each_machine_a_run_crosses_keeps_it_in_its_record_of_runs() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (near_socket, nodes) = linked_to_kestrel(dir.path(), true).await;
        let came = run_through(near_socket, run_of("kestrel", "exit 3"))
            .await
            .expect("it runs on kestrel");
        assert_eq!(came.ended, Some(Ran::Exited(3)));
        for machine in ["skiff", "kestrel"] {
            let record = dir.path().join(machine).join("runs.jsonl");
            // Each end keeps how the run ended once it has handed that
            // on, and nothing says when, so this looks again every few
            // milliseconds.
            let lines = tokio::time::timeout(PATIENCE, async {
                loop {
                    let lines: Vec<Value> = fs::read_to_string(&record)
                        .unwrap_or_default()
                        .lines()
                        .map(|line| serde_json::from_str(line).expect("a line is JSON"))
                        .collect();
                    if lines.len() >= 2 {
                        break lines;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{machine} keeps the run's start and end"));
            let [started, ended] = lines.as_slice() else {
                panic!("{machine} keeps one run: {lines:?}");
            };
            assert_eq!(started["by"], json!("Person"), "{machine}: {started}");
            assert_eq!(started["from"], json!("skiff"), "{machine}: {started}");
            assert_eq!(started["on"], json!("kestrel"), "{machine}: {started}");
            assert_eq!(
                started["command"],
                json!(["/bin/sh", "-c", "exit 3"]),
                "{machine}: {started}"
            );
            assert_eq!(ended["run"], started["run"], "{machine}");
            assert_eq!(ended["ended"], json!({"Exited": 3}), "{machine}: {ended}");
        }
        stop(nodes).await;
    }

    #[tokio::test]
    async fn a_quest_running_on_another_machine_is_carried_from_there_over_a_proved_link() {
        for (proven, why) in [
            (true, "kestrel carries no sessions over this link"),
            (
                false,
                "kestrel carries a session only when asked over a link its key proved, which \
                 this one isn't",
            ),
        ] {
            let dir = tempfile::tempdir().expect("a temporary folder");
            let far_socket = dir.path().join("kestrel").join("work.sock");
            let near_socket = dir.path().join("skiff").join("work.sock");
            let far_store = Memory::default();
            far_store.start_here(
                "s-far",
                json!({"session": "s-far", "addresses": ["FARAWY"], "groups": [],
                       "busy": true, "state": "working",
                       "at": jiff::Timestamp::now().to_string(),
                       "quest": "QEST-20261115-FARAWY"}),
                true,
            );
            let far = alone(&far_store, "kestrel", &far_socket);
            let dial = Piped {
                store: far_store,
                socket: far_socket,
                held: dir.path().join("held"),
                proven,
                asks_back: false,
                shares: None,
            };
            let near = node(&Memory::default(), "skiff", vec![dial], None, &near_socket);
            listed(near_socket.clone(), "s-far").await;
            asks_as_the_person(&near_socket);
            let mut client = Speaking::greeted(&near_socket).await;
            assert_eq!(
                carries(&mut client, 2, "QEST-20261115-FARAWY", "skiff").await["error"]["message"],
                json!(why),
                "skiff asks kestrel, where the quest runs, which judges the ask itself \
                 (proven: {proven})"
            );
            drop(client);
            stop([near, far]).await;
        }
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
        let (stop, node) = node::<Ssh>(&store, "skiff", Vec::new(), None, &socket);
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
        let (stop, node) = node::<Ssh>(&store, "skiff", Vec::new(), None, &socket);

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

    /// How a client that connects the instant a socket's path appears
    /// finds it: the path's mode, and whether the connection was taken.
    fn the_instant_it_appears(path: &Path) -> (u32, io::Result<()>) {
        let waiting = std::time::Instant::now();
        loop {
            if let Ok(made) = fs::symlink_metadata(path) {
                let connected = std::os::unix::net::UnixStream::connect(path).map(drop);
                return (made.mode() & 0o777, connected);
            }
            assert!(
                waiting.elapsed() < Duration::from_secs(5),
                "the socket never appeared"
            );
        }
    }

    #[tokio::test]
    async fn a_socket_appears_already_listening_and_private() {
        for _ in 0..300 {
            let dir = tempfile::tempdir().expect("a temporary folder");
            let socket = dir.path().join("mycelium").join("work.sock");
            let watched = socket.clone();
            let client = std::thread::spawn(move || the_instant_it_appears(&watched));
            let bound = bind(&socket).expect("the socket binds");
            let (mode, connected) = client.join().expect("the client ran");
            assert!(
                connected.is_ok(),
                "a client was refused the instant the socket appeared: {connected:?}"
            );
            assert_eq!(mode, 0o600, "the socket appeared open to others");
            drop(bound);
        }
    }

    #[tokio::test]
    async fn a_second_node_is_refused_the_socket_a_first_serves() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("mycelium").join("work.sock");
        let first = bind(&socket).expect("the first binds");
        let second = bind(&socket).map(drop);
        assert_eq!(
            second.as_ref().map_err(io::Error::kind),
            Err(io::ErrorKind::AddrInUse),
            "the second was let take it: {second:?}"
        );

        // A second that looked before the first had linked its socket in.
        let late = staged(&socket);
        let latecomer = UnixListener::bind(&late).expect("the latecomer binds");
        let placed = link_in(&late, &socket);
        drop(latecomer);
        let _ = fs::remove_file(&late);
        assert_eq!(
            placed.as_ref().map_err(io::Error::kind),
            Err(io::ErrorKind::AddrInUse),
            "the latecomer was let take it: {placed:?}"
        );
        assert!(
            std::os::unix::net::UnixStream::connect(&socket).is_ok(),
            "the first still serves it"
        );
        let staged = fs::read_dir(socket.parent().expect("its folder"))
            .expect("its folder reads")
            .count();
        assert_eq!(staged, 1, "nothing but the socket is left beside it");
        drop(first);
    }

    #[tokio::test]
    async fn a_bind_sweeps_only_what_dead_binds_left_behind() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("mycelium").join("work.sock");
        let folder = socket.parent().expect("its folder").to_owned();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&folder)
            .expect("the folder is made");
        let mut ended = std::process::Command::new("true")
            .spawn()
            .expect("a process starts");
        let dead = ended.id();
        ended.wait().expect("it ends");
        let mut running = std::process::Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .expect("a process starts");
        let left = |name: String| {
            let at = folder.join(name);
            drop(std::os::unix::net::UnixListener::bind(&at).expect("it binds"));
            at
        };
        let dead_left = left(format!(".{dead}-0"));
        let live_left = left(format!(".{}-0", running.id()));
        let unlike = left(format!(".{dead}-0x"));
        let plain = folder.join(format!(".{dead}-1"));
        fs::write(&plain, "").expect("a plain file");

        let bound = bind(&socket).expect("the socket binds");
        running.kill().expect("the running one is stopped");
        running.wait().expect("it ends");

        assert!(!dead_left.exists(), "a dead bind's socket stayed");
        assert!(live_left.exists(), "a running bind's socket was taken");
        assert!(unlike.exists(), "a name unlike a staged one was taken");
        assert!(plain.exists(), "a file that isn't a socket was taken");
        drop(bound);
    }

    #[test]
    fn a_staged_name_says_who_bound_it() {
        let staged = staged(Path::new("/run/mycelium/work.sock"));
        let name = staged.file_name().and_then(|name| name.to_str());
        assert_eq!(name.and_then(staged_by), Some(std::process::id()));
        for unlike in [
            "work.sock",
            ".12",
            ".12-",
            ".-3",
            ".+12-3",
            ".12-3x",
            "12-3",
        ] {
            assert_eq!(
                staged_by(unlike),
                None,
                "{unlike} was taken for a staged name"
            );
        }
    }

    #[tokio::test]
    async fn a_watch_gets_the_whole_state_then_a_message_a_client_sent() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("mycelium").join("work.sock");
        let store = Memory::default();
        let presence =
            json!({"session": "01a1", "addresses": ["4UHC5Q"], "at": "2026-10-26T20:00:00Z"});
        store.start_here("01a1", presence, true);
        let given = bare(dir.path().join("steer"), None);
        let (stop, node) = alone_with(&store, &socket, given);
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
    async fn a_node_easing_says_what_it_would_do_and_does_none_of_it() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("mycelium").join("work.sock");
        let steers = dir.path().join("steer");
        let folder = dir.path().join("eases");
        let carries = dir.path().join("carries.jsonl");
        let eases = Eases {
            folder: folder.clone(),
            every: Duration::from_millis(10),
        };
        let given = Given {
            eases: Some(eases),
            carries: carries.clone(),
            ..bare(steers, None)
        };
        let (stop, node) = alone_with(&Memory::default(), &socket, given);

        let day = folder.join(format!(
            "{}.jsonl",
            jiff::Timestamp::now().strftime("%Y-%m-%d")
        ));
        let said = tokio::time::timeout(PATIENCE, async {
            // Nothing tells a test when a line is kept, so it looks again
            // every few milliseconds until one is.
            loop {
                if let Some(line) = fs::read_to_string(&day)
                    .ok()
                    .and_then(|kept| kept.lines().next().map(str::to_owned))
                {
                    return line;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a line is kept in time");
        let line: Value = serde_json::from_str(&said).expect("a line");
        assert_eq!(
            (&line["would"], &line["choice"]),
            (&serde_json::json!(true), &serde_json::json!("nothing")),
            "a lone machine has nowhere to ease to: {line}"
        );
        assert!(line["reading"]["held"].is_array(), "{line}");
        stop.send(()).expect("the node still runs");
        node.await.expect("the node ends").expect("the node ran");
        assert!(
            !carries.exists(),
            "what it would do never reaches the record of carries"
        );
        let kept = fs::read_to_string(&day).expect("the day's file");
        assert_eq!(
            kept.lines().count(),
            1,
            "nothing is said again for the same reason: {kept}"
        );
    }

    #[tokio::test]
    async fn a_node_keeps_each_placement_with_what_it_knew() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("mycelium").join("work.sock");
        let kept = dir.path().join("placements");
        let given = Given {
            placements: Some(placements::Log::at(kept.clone())),
            ..bare(dir.path().join("steer"), None)
        };
        let (stop, node) = alone_with(&Memory::default(), &socket, given);

        let answer = tokio::task::spawn_blocking(move || {
            let mut client = Client::connect(&socket).expect("the client connects");
            client
                .ask(&Request::Place(work(Vec::new())))
                .expect("the node answers")
        });
        let answer = tokio::time::timeout(PATIENCE, answer)
            .await
            .expect("the node answers in time")
            .expect("the client asks");
        let Ok(Response::Placement(placement)) = answer else {
            panic!("a placement: {answer:?}");
        };
        let days: Vec<PathBuf> = fs::read_dir(&kept)
            .expect("the log's folder")
            .map(|entry| entry.expect("an entry").path())
            .collect();
        let [day] = days.as_slice() else {
            panic!("one day's file: {days:?}");
        };
        let records: Vec<placements::Record> = fs::read_to_string(day)
            .expect("the day's file")
            .lines()
            .map(|line| serde_json::from_str(line).expect("a record"))
            .collect();
        let [record] = records.as_slice() else {
            panic!("one placement kept: {records:?}");
        };
        assert!(!record.start, "asked where, not started");
        assert_eq!(record.decision.placement, placement, "as it answered");
        assert_eq!(
            record
                .machines
                .iter()
                .map(|one| one.name.as_str())
                .collect::<Vec<_>>(),
            ["skiff"],
            "with the machines it placed among"
        );
        assert_eq!(record.decision.judged.len(), 1, "and how it judged each");
        stop.send(()).expect("the node is still running");
        node.await
            .expect("the node runs to its end")
            .expect("the node stops cleanly");
    }

    #[tokio::test]
    async fn a_node_starts_what_it_is_asked_where_placement_says_or_says_why_not() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("mycelium").join("work.sock");
        let (holder, given) = stand_in_holder(dir.path());
        let holds = Starter {
            program: holder,
            scope: start::Scope::None,
            kit: Some(start::Kit::Carried(PathBuf::from("/nix/store/kit"))),
        };
        let (stop, node) = alone_with(
            &Memory::default(),
            &socket,
            bare(dir.path().join("steer"), Some(holds)),
        );

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
            let elsewhere = Need::Machine("kestrel".to_owned());
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
            Err("nowhere: skiff isn't kestrel".to_owned()),
            "a start nowhere can take says why, as the launcher shows it"
        );
        let Ok(Response::Started(started)) = started else {
            panic!("a start any machine can take starts here: {started:?}");
        };
        assert_eq!(started.machine, "skiff");
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
            [
                "--login",
                "--kit",
                "/nix/store/kit",
                "--",
                "pi",
                "--continue"
            ],
            "each start of the program runs through the login shell, with the realm's kit"
        );

        drop(stop);
        let stopped = node.await.expect("the node runs to its end");
        assert!(stopped.is_ok(), "the node stops cleanly: {stopped:?}");
    }

    /// The next line `lines` holds, read in time.
    async fn next_line(
        lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    ) -> String {
        tokio::time::timeout(PATIENCE, lines.next_line())
            .await
            .expect("a line in time")
            .expect("read")
            .expect("a line")
    }

    #[tokio::test]
    async fn a_node_says_its_own_health_to_whoever_watches_it() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("mycelium").join("work.sock");
        let daemon = dir.path().join("system.sock");
        let listener = UnixListener::bind(&daemon).expect("the stand-in daemon listens");
        let membership = Realm {
            name: "work".to_owned(),
            machine: "skiff".to_owned(),
        };
        let bound = bind(&socket).expect("the socket binds");
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let steers = dir.path().join("steer");
        let node = tokio::spawn(async move {
            let given = Given {
                health: Some(daemon),
                ..bare(steers, None)
            };
            let stopped = async {
                let _ = stopped.await;
            };
            let nobody = Listed::<Ssh>(Vec::new());
            run(Memory::default(), membership, nobody, given, bound, stopped).await
        });

        let (reading, mut writing) = UnixStream::connect(&socket)
            .await
            .expect("the watcher connects")
            .into_split();
        let mut watched = BufReader::new(reading).lines();
        writing
            .write_all(b"\"Health\"\n")
            .await
            .expect("the watcher asks");
        let first: Reply = serde_json::from_str(&next_line(&mut watched).await).expect("a reply");
        assert_eq!(first, Ok(Response::Health(None)), "unknown until said");

        let (stream, _) = tokio::time::timeout(PATIENCE, listener.accept())
            .await
            .expect("the node asks its daemon in time")
            .expect("the node connects");
        let (asks, mut says) = stream.into_split();
        let mut asks = BufReader::new(asks).lines();
        for _ in 0..3 {
            next_line(&mut asks).await;
        }
        let said = json!({"id": 1, "records": [
            {"key": "summary", "kind": "summary", "health": "watch"},
            {"key": "part:code", "kind": "part", "part": "code", "health": "watch"},
        ]});
        says.write_all(format!("{said}\n").as_bytes())
            .await
            .expect("the node reads");
        let moved: Reply = serde_json::from_str(&next_line(&mut watched).await).expect("a reply");
        let Ok(Response::Health(Some(health))) = moved else {
            panic!("a health: {moved:?}");
        };
        assert_eq!(health.health, "watch");
        assert_eq!(health.parts["code"], "watch");

        let Ok(Response::Machines(machines)) = ask(socket.clone(), Request::Machines).await else {
            panic!("the machines");
        };
        assert_eq!(
            machines[0].health.as_ref(),
            Some(&health),
            "its measure carries it"
        );
        drop(stop);
        let stopped = node.await.expect("the node runs to its end");
        assert!(stopped.is_ok(), "the node stops cleanly: {stopped:?}");
    }

    #[tokio::test]
    async fn a_word_moving_is_kept_in_the_last_measure_at_once() {
        let (to_dial, _) = mpsc::unbounded_channel();
        let membership = Realm {
            name: "work".to_owned(),
            machine: "skiff".to_owned(),
        };
        let (shared, _) = Shared::of(
            Memory::default(),
            (membership.hello(), "/nowhere/node".into()),
            bare("/nowhere/steer".into(), None),
            to_dial,
        );
        shared.keep_measured(&shared.measure());
        let mut machines = shared.said.machines.subscribe();
        machines.borrow_and_update();
        let health = own_health::Health {
            health: "bad".to_owned(),
            parts: BTreeMap::from([("code".to_owned(), "bad".to_owned())]),
        };
        shared.keep_health(Some(health.clone()));
        assert!(
            machines.has_changed().expect("still kept"),
            "its readers are told"
        );
        assert_eq!(shared.known_machines()[0].health, Some(health));
    }

    fn made_by(provider: &str, machine: &str, until: jiff::Timestamp) -> Made {
        Made {
            provider: provider.to_owned(),
            shape: "scratch".to_owned(),
            place: format!("{machine}-place"),
            machine: machine.to_owned(),
            until,
            costs: offer::Costs {
                hourly: "0.12".to_owned(),
                currency: "USD".to_owned(),
            },
        }
    }

    #[test]
    fn a_node_started_again_links_what_it_still_asks_for_and_books_what_ended() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let book = Book::at(dir.path().join("made.jsonl")).of_realm("work");
        let later = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1);
        let earlier = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1);
        let (old, new) = (
            "/nix/store/".to_owned() + &"a".repeat(32),
            "/nix/store/".to_owned() + &"b".repeat(32),
        );
        let asked = made_by("/usr/bin/lend", "skiff", later);
        let moved = made_by(&format!("{old}-hire-1/bin/hire"), "tern", later);
        let dropped = made_by(&format!("{old}-rent-1/bin/hire"), "osprey", later);
        let ended = made_by("/usr/bin/lend", "kestrel", earlier);
        for made in [&asked, &moved, &dropped, &ended] {
            book.made(made).expect("the make is kept");
        }
        let providers = ["/usr/bin/lend".to_owned(), format!("{new}-hire-1/bin/hire")];
        let store = Memory::default();
        store
            .put_machine(&facts::Machine {
                declared: Some(crate::declared::Declared {
                    reach: Vec::new(),
                    probes: BTreeMap::new(),
                    outlives: Vec::new(),
                    costs_money: true,
                    taints: Vec::new(),
                    cores: None,
                    agents: BTreeMap::new(),
                    leaves_at: None,
                    within: None,
                    hosts: Vec::new(),
                    fence: None,
                }),
                ..facts::measure("kestrel", &facts::Sources::nowhere())
            })
            .expect("what it said is kept");
        let (to_dial, mut dialled) = mpsc::unbounded_channel();

        redial_standing(&book, &providers, &store, &to_dial);

        let leaves = store.machines()[0]
            .declared
            .as_ref()
            .and_then(|declared| declared.leaves_at);
        assert!(
            leaves.is_some_and(|at| at <= jiff::Timestamp::now()),
            "the machine whose time was up leaves the realm's machines: {leaves:?}"
        );

        let mut sent = Vec::new();
        while let Ok((made, _)) = dialled.try_recv() {
            sent.push(made);
        }
        let moved_here = Made {
            provider: providers[1].clone(),
            ..moved.clone()
        };
        assert_eq!(
            sent,
            vec![asked.clone(), moved_here],
            "each it still asks for is dialled, by the provider's path as given now"
        );
        assert_eq!(
            book.standing().expect("the book reads").made,
            vec![asked, moved, dropped],
            "the one whose time was up is booked as let go, the others left standing"
        );
    }

    #[test]
    fn a_provider_is_still_asked_by_its_path_or_as_built_again() {
        let at =
            |hash: char, rest: &str| format!("/nix/store/{}-{rest}", hash.to_string().repeat(32));
        let providers = [
            at('b', "lend-1/bin/lend"),
            at('b', "hire-1/bin/hire"),
            at('c', "hire-1/bin/hire"),
            "/usr/bin/rent".to_owned(),
        ];
        assert_eq!(still_asked(&providers, &providers[3]), Some(&providers[3]));
        assert_eq!(
            still_asked(&providers, &at('a', "lend-1/bin/lend")),
            Some(&providers[0]),
            "the same program built again is still asked"
        );
        assert_eq!(
            still_asked(&providers, &at('a', "hire-1/bin/hire")),
            None,
            "two built the same leave it unknown"
        );
        assert_eq!(
            still_asked(&providers, &at('a', "loan-1/bin/lend")),
            None,
            "another program of the same file name isn't it"
        );
        assert_eq!(still_asked(&providers, "/opt/bin/rent"), None);
    }
}
