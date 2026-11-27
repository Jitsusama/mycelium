//! The worker socket: where the sessions of a worker machine, running as
//! an account of their own, are present and speak on the mesh through the
//! node, rather than writing the mesh's folders, which would let them say
//! anything as anyone. The node answers only that account, keeps each
//! presence under its own process, and judges every message before it is
//! kept, so a worker reaches its own quest and peers by address and
//! steers nothing.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};

use super::{
    Shared, Socket, WATCH_PATIENCE, in_use, no_socket, staged, sweep_staged, take_over, write,
};
use crate::forage;
use crate::group::{Group, Pattern};
use crate::mesh;
use crate::record::Message;
use crate::store::Store;
use crate::wire::Event;

/// Where a node takes its worker machine's sessions, and whose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worker {
    /// The socket, in a folder no account but the node's, or the
    /// system's, may change.
    pub socket: PathBuf,
    /// The account whose processes it answers; no other is.
    pub uid: u32,
}

impl Worker {
    /// The socket each program the node holds is told to speak through:
    /// this one, where it answers the account the node runs as, which is
    /// the account every program the node starts runs as; none where it
    /// answers another, since it would refuse them.
    #[must_use]
    pub fn told_to_starts(&self) -> Option<PathBuf> {
        // The kernel names a connecting process by its effective account.
        (self.uid == rustix::process::geteuid().as_raw()).then(|| self.socket.clone())
    }

    /// Whether the sessions its node starts run as this account rather
    /// than the node's own, as a room's fence runs them (9R6RA6's image,
    /// whose launcher runs each as `worker`), so none of them opens the
    /// node's own socket, which only the node's account may. A machine
    /// naming its node's own account here keeps nothing apart.
    #[must_use]
    pub fn keeps_starts_apart(&self) -> bool {
        self.uid != rustix::process::geteuid().as_raw()
    }
}

/// How far from the node's clock a message's time may be, in
/// milliseconds: its sender stamps it as it writes it, a moment before.
const SENT_WITHIN: i64 = 60_000;

/// The most of the latest messages a watch from a marker looks through,
/// when it names no count of its own.
const SINCE_AT_MOST: usize = 1000;

/// The worker socket, bound, with the account it answers.
pub(super) struct Listening {
    socket: Socket,
    uid: u32,
}

/// Binds `worker`'s socket: in a folder only the node's account or the
/// system's may change, which is made if it isn't there, open for any
/// account to connect to, since the account is judged by the kernel's
/// word as each connects. A socket a live node still serves, or a path
/// that isn't a socket, is refused, as the node's own are.
pub(super) fn bind(worker: &Worker) -> io::Result<Listening> {
    let path = &worker.socket;
    let Some(folder) = path.parent() else {
        return Err(no_socket(format!(
            "{} has no folder, so the worker socket can't go there",
            path.display()
        )));
    };
    unchangeable_folder(folder)?;
    sweep_staged(folder);
    take_over(path)?;
    let staged = staged(path);
    let _ = fs::remove_file(&staged);
    let listener = UnixListener::bind(&staged)?;
    let placed = fs::set_permissions(&staged, fs::Permissions::from_mode(0o666))
        .and_then(|()| fs::hard_link(&staged, path).map_err(|error| in_use(error, path)));
    let _ = fs::remove_file(&staged);
    placed.map(|()| Listening {
        socket: Socket {
            listener,
            path: path.clone(),
            also: None,
        },
        uid: worker.uid,
    })
}

/// Makes `folder` if it isn't there, refusing one an account other than
/// the node's or the system's owns, or any other account may change, where
/// a socket could be put in the worker socket's place.
fn unchangeable_folder(folder: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(folder)?;
    let made = fs::metadata(folder)?;
    let owner = made.uid();
    if (owner != rustix::process::geteuid().as_raw() && owner != 0) || made.mode() & 0o022 != 0 {
        return Err(no_socket(format!(
            "{} may be changed by others, so the worker socket can't go there",
            folder.display()
        )));
    }
    Ok(())
}

/// The next client of the worker socket, with the account it answers;
/// never, for a node with none.
pub(super) async fn accept(listening: Option<&Listening>) -> io::Result<(UnixStream, u32)> {
    match listening {
        Some(listening) => Ok((listening.socket.accept().await?, listening.uid)),
        None => std::future::pending().await,
    }
}

/// What a worker's session asks, a line each.
#[derive(Debug, Deserialize)]
enum Asked {
    /// Its presence, as the harness writes its file: first on the
    /// connection that holds it, and again whenever it changes.
    Presence {
        /// The presence.
        presence: Value,
    },
    /// A message, as the harness writes its file, on the connection that
    /// holds its sender's presence.
    Send {
        /// The message.
        message: Value,
    },
    /// An end of the hold `held`, on the connection that holds its
    /// presence, when the session asking runs in it.
    End {
        /// The hold to end, which the session runs in.
        held: String,
    },
    /// The messages within its reach, on a connection of its own.
    Watch {
        /// The session, present through this process, whose reach it is.
        session: String,
        /// Narrows what is sent to these groups, within its reach.
        #[serde(default)]
        groups: Option<Pattern>,
        /// The message after which the first state begins.
        #[serde(default)]
        since: Option<String>,
        /// How many of the latest messages the first state holds.
        #[serde(default)]
        recent: Option<usize>,
    },
}

/// What the node answers a worker's ask with, when it is done.
#[derive(Debug, Serialize)]
enum Said {
    /// Its presence is held.
    Present,
    /// Its message is kept, as the node wrote it.
    Sent(Value),
    /// Its watch has begun.
    Watching,
    /// The hold it runs in is ending, as it asked: its program is told
    /// to end next.
    Ending,
}

/// The node's answer to one ask.
type Reply = Result<Said, String>;

/// What a watch hears, each message as the mesh's file holds it.
#[derive(Debug, Serialize)]
enum Heard {
    /// The latest within its reach, oldest first.
    MessagesChanged { messages: Vec<Value> },
    /// One more within its reach.
    Message { message: Value },
}

/// The presences held through the worker socket, by connection.
#[derive(Clone, Default)]
pub(super) struct Holdings(Arc<Mutex<Held>>);

#[derive(Default)]
struct Held {
    next: u64,
    connections: BTreeMap<u64, Connection>,
}

/// A connection holding a presence, from the process the kernel names.
struct Connection {
    pid: i32,
    holding: Option<Holding>,
}

/// The session a connection is present as, and its reach.
#[derive(Debug, Clone)]
struct Holding {
    session: String,
    /// Its id and every address its presence lists.
    names: Vec<String>,
    /// Its own quest's group, if its presence names a quest.
    quest: Option<Group>,
}

impl Holdings {
    fn lock(&self) -> MutexGuard<'_, Held> {
        // Each change is one insert or remove, whole either way.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A new connection from `pid`, holding nothing yet.
    fn open(&self, pid: i32) -> u64 {
        let mut held = self.lock();
        let id = held.next;
        held.next += 1;
        held.connections
            .insert(id, Connection { pid, holding: None });
        id
    }

    /// What connection `id` holds.
    fn of(&self, id: u64) -> Option<Holding> {
        self.lock().connections.get(&id)?.holding.clone()
    }

    /// Has connection `id` hold `holding`, unless another holds its
    /// session; what it held before, if anything.
    fn hold(&self, id: u64, holding: Holding) -> Result<Option<Holding>, String> {
        let mut held = self.lock();
        let elsewhere = held.connections.iter().any(|(other, connection)| {
            *other != id
                && connection
                    .holding
                    .as_ref()
                    .is_some_and(|its| its.session == holding.session)
        });
        if elsewhere {
            return Err(format!(
                "{} is present through another connection",
                holding.session
            ));
        }
        let connection = held
            .connections
            .get_mut(&id)
            .ok_or_else(|| "the connection is closed".to_owned())?;
        Ok(connection.holding.replace(holding))
    }

    /// Has connection `id` hold `was` again, as it did before a presence
    /// that couldn't be kept.
    fn restore(&self, id: u64, was: Option<Holding>) {
        if let Some(connection) = self.lock().connections.get_mut(&id) {
            connection.holding = was;
        }
    }

    /// Closes connection `id`, with what it held.
    fn close(&self, id: u64) -> Option<Holding> {
        self.lock().connections.remove(&id)?.holding
    }

    /// What `session` holds through a connection of process `pid`'s.
    fn held_by(&self, pid: i32, session: &str) -> Option<Holding> {
        self.lock()
            .connections
            .values()
            .filter(|connection| connection.pid == pid)
            .filter_map(|connection| connection.holding.clone())
            .find(|holding| holding.session == session)
    }
}

/// Serves one client of the worker socket: a presence and what it sends,
/// or a watch, as its first line asks. A client of any account but
/// `uid`'s, or one the kernel won't name, is refused.
pub(super) async fn serve<S: Store>(
    stream: UnixStream,
    shared: Shared<S>,
    uid: u32,
    holdings: Holdings,
) {
    let cred = stream.peer_cred().ok();
    let (reading, mut writing) = stream.into_split();
    let Some(pid) = cred
        .filter(|cred| cred.uid() == uid)
        .and_then(|cred| cred.pid())
    else {
        let refused: Reply = Err("the worker socket answers only its worker's account".to_owned());
        // A client that hung up already hears nothing either way.
        let _ = write(&mut writing, &refused).await;
        return;
    };
    let mut lines = BufReader::new(reading).lines();
    let Ok(Some(line)) = lines.next_line().await else {
        return;
    };
    match serde_json::from_str::<Asked>(&line) {
        Ok(Asked::Presence { presence }) => {
            let present = Present {
                shared: &shared,
                holdings: &holdings,
                id: holdings.open(pid),
                pid,
            };
            present.keep(presence, lines, writing).await;
        }
        Ok(Asked::Watch {
            session,
            groups,
            since,
            recent,
        }) => {
            let Some(holding) = holdings.held_by(pid, &session) else {
                let refused: Reply = Err(format!(
                    "{session} isn't present through this process, so it watches nothing"
                ));
                let _ = write(&mut writing, &refused).await;
                return;
            };
            let watch = Watch {
                holding,
                pid,
                groups,
                since,
            };
            let recent = recent.unwrap_or(if watch.since.is_some() {
                SINCE_AT_MOST
            } else {
                0
            });
            watch.keep(&shared, &holdings, recent, lines, writing).await;
        }
        Ok(Asked::Send { .. } | Asked::End { .. }) => {
            let refused: Reply = Err("a connection opens with a presence or a watch".to_owned());
            let _ = write(&mut writing, &refused).await;
        }
        Err(error) => {
            let refused: Reply = Err(format!("not an ask of the worker socket: {error}"));
            let _ = write(&mut writing, &refused).await;
        }
    }
}

/// A connection holding a presence.
struct Present<'a, S: Store> {
    shared: &'a Shared<S>,
    holdings: &'a Holdings,
    id: u64,
    /// The process the kernel names at the connection's other end.
    pid: i32,
}

impl<S: Store> Present<'_, S> {
    /// Holds `presence`, then answers each ask after it in turn, until the
    /// client hangs up, when its presence is let go. One whose first
    /// presence is refused is closed, holding nothing.
    async fn keep(
        &self,
        presence: Value,
        mut lines: tokio::io::Lines<BufReader<OwnedReadHalf>>,
        mut writing: OwnedWriteHalf,
    ) {
        let first = self.be(presence);
        let held = first.is_ok();
        if write(&mut writing, &first).await.is_ok() && held {
            while let Ok(Some(line)) = lines.next_line().await {
                let mut ending = None;
                let reply = match serde_json::from_str::<Asked>(&line) {
                    Ok(Asked::Presence { presence }) => self.be(presence),
                    Ok(Asked::Send { message }) => self.send(&message),
                    Ok(Asked::End { held }) => {
                        let owned = self.own_hold(&held);
                        if owned.is_ok() {
                            ending = Some(held);
                        }
                        owned.map(|()| Said::Ending)
                    }
                    Ok(Asked::Watch { .. }) => {
                        Err("a watch is asked on a connection of its own".to_owned())
                    }
                    Err(error) => Err(format!("not an ask of the worker socket: {error}")),
                };
                // The end is answered before it begins, since the session
                // asking is told to end with its program, and may hear
                // nothing after.
                let answered = write(&mut writing, &reply).await.is_ok();
                if let Some(held) = ending {
                    self.end(held).await;
                }
                if !answered {
                    break;
                }
            }
        }
        if let Some(holding) = self.holdings.close(self.id)
            && let Err(error) = self.shared.store.remove_own_presence(&holding.session)
        {
            eprintln!(
                "a worker's presence of {} couldn't be let go: {error}",
                holding.session
            );
        }
    }

    /// Holds `presence` as this connection's, in place of any it held,
    /// unless its session or a name it answers to is another's.
    fn be(&self, presence: Value) -> Reply {
        let holding = holding_of(&presence)?;
        let before = self.holdings.of(self.id);
        let ours = |session: &str| before.as_ref().is_some_and(|its| its.session == session);
        let taken = self
            .shared
            .store
            .presences()
            .into_iter()
            .filter(|other| other.elsewhere.is_some() || !ours(&other.session))
            .filter(|other| other.lives || parked(&other.body))
            .flat_map(|other| names(&other.session, &other.body))
            .find(|name| {
                holding
                    .names
                    .iter()
                    .any(|said| said.eq_ignore_ascii_case(name))
            });
        if let Some(name) = taken {
            return Err(format!(
                "{name} is a session living or parked here or elsewhere, so a worker can't be it"
            ));
        }
        let mut body = presence;
        if let Some(fields) = body.as_object_mut() {
            // What only the node, or a link, says of a presence.
            for kept in [
                "owner",
                "machine",
                "unreachableSince",
                "heldAs",
                "startedByAs",
            ] {
                fields.remove(kept);
            }
        }
        let session = holding.session.clone();
        let was = self.holdings.hold(self.id, holding)?;
        if let Err(error) = self.shared.store.put_own_presence(&session, &body) {
            // What it held stays held, as its file still says.
            self.holdings.restore(self.id, was);
            return Err(format!("the presence of {session} isn't kept: {error}"));
        }
        if let Some(was) = was.filter(|was| was.session != session)
            && let Err(error) = self.shared.store.remove_own_presence(&was.session)
        {
            eprintln!(
                "a worker's presence of {} couldn't be let go: {error}",
                was.session
            );
        }
        Ok(Said::Present)
    }

    /// Refuses an end of `held` unless this connection holds a presence
    /// and its process runs in that hold's program here, so a worker's
    /// session ends only the hold it runs in, never another's: what its
    /// presence says proves nothing, since the session writes it.
    fn own_hold(&self, held: &str) -> Result<(), String> {
        let holding = self
            .holdings
            .of(self.id)
            .ok_or_else(|| "no presence is held on this connection".to_owned())?;
        let holds = crate::hold::held(&self.shared.held);
        super::runs_in(&holds, held, self.pid, &self.shared.processes)
            .map_err(|why| format!("{} can't end {held}: {why}", holding.session))
    }

    /// Ends the hold `held` for the session running in it, as the
    /// person's own end does, saying how on the node's log.
    async fn end(&self, held: String) {
        let folder = self.shared.held.clone();
        let id = held.clone();
        let grace = crate::hold::end::GRACE;
        let ends = move || crate::hold::end::end(&folder, &id, grace);
        match tokio::task::spawn_blocking(ends).await {
            Ok(Ok(ending)) => {
                eprintln!("ended {held}, held here, for the worker's session in it: {ending:?}");
            }
            Ok(Err(error)) => eprintln!("{held} didn't end as its own session asked: {error}"),
            Err(error) => eprintln!("the end of {held} stopped: {error}"),
        }
    }

    /// Keeps `message`, sent by the session this connection holds, as the
    /// node writes it again, when a worker may say it.
    fn send(&self, message: &Value) -> Reply {
        let holding = self
            .holdings
            .of(self.id)
            .ok_or_else(|| "no presence is held on this connection".to_owned())?;
        let now = jiff::Timestamp::now().as_millisecond();
        let (id, body) = rebuilt(message, &holding, now)?;
        let store = &self.shared.store;
        if store.message(&id).is_some() {
            return Err(format!(
                "{id} is said already, and a worker can't write over it"
            ));
        }
        if let Some(steers) = mesh::steers(&body, |earlier| store.message(earlier)) {
            return Err(format!("{id} is {steers}, which a worker can't say"));
        }
        match store.put_message(&id, &body) {
            Ok(true) => Ok(Said::Sent(body)),
            Ok(false) => Err(format!(
                "{id} is said already, and a worker can't write over it"
            )),
            Err(error) => Err(format!("the store refused {id}: {error}")),
        }
    }
}

/// The session `presence` is of, and its reach, if it names one.
fn holding_of(presence: &Value) -> Result<Holding, String> {
    let session = presence
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| "a presence names its session".to_owned())?;
    crate::store::folders::safe(session).map_err(|error| format!("session: {error}"))?;
    let quest = presence
        .get("quest")
        .and_then(Value::as_str)
        .and_then(|quest| quest.rsplit('-').next())
        .and_then(|tail| mesh::group_for(&format!("#quest/{}", tail.to_lowercase())));
    Ok(Holding {
        session: session.to_owned(),
        names: names(session, presence),
        quest,
    })
}

/// The names a presence of `session` saying `body` answers to: its id and
/// each address it lists.
fn names(session: &str, body: &Value) -> Vec<String> {
    let mut names = forage::addresses(body.get("addresses").unwrap_or(&Value::Null));
    names.push(session.to_owned());
    names
}

/// Whether a presence says its session is parked, to be started again
/// for what is sent it.
fn parked(body: &Value) -> bool {
    body.get("state").and_then(Value::as_str) == Some("parked")
}

/// The mesh's id `id` in its parts, when it is of the mesh's shape
/// `<millis>-<from>-<pid>-<tail>`: its time, its sender and its process.
fn id_parts(id: &str) -> Option<(i64, &str, u32)> {
    let (rest, tail) = id.rsplit_once('-')?;
    let (rest, pid) = rest.rsplit_once('-')?;
    let (millis, from) = rest.split_once('-')?;
    let alphanumeric = !tail.is_empty() && tail.bytes().all(|byte| byte.is_ascii_alphanumeric());
    if !alphanumeric || from.is_empty() {
        return None;
    }
    Some((millis.parse().ok()?, from, pid.parse().ok()?))
}

/// `message`, as a worker's session `holding` sent it at `now` (in
/// milliseconds), written again by the node with only the fields the mesh
/// reads, each judged: its id of the mesh's shape, naming its sender and
/// stamped within a minute of the node's clock at its time to the
/// millisecond; its sender one of the session's names; every group it
/// goes to within the worker's reach; and no intent but news, a question
/// or a finding. Anything else in it is let go.
fn rebuilt(message: &Value, holding: &Holding, now: i64) -> Result<(String, Value), String> {
    let words = |field: &str| -> Result<Option<&str>, String> {
        match message.get(field) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(said)) => Ok(Some(said)),
            Some(_) => Err(format!("its {field} isn't words")),
        }
    };
    let id = words("id")?.ok_or("a message has an id")?;
    let (millis, named, pid) =
        id_parts(id).ok_or_else(|| format!("{id} isn't of the mesh's shape"))?;
    if (now - millis).abs() > SENT_WITHIN {
        return Err(format!(
            "{id} was stamped more than a minute from the node's clock"
        ));
    }
    let at = words("at")?.ok_or("a message says when it was sent")?;
    let stamped = at
        .parse::<jiff::Timestamp>()
        .map_err(|error| format!("its time {at:?}: {error}"))?;
    if stamped.as_millisecond() != millis {
        return Err(format!("its time {at} isn't its id's"));
    }
    let from = words("from")?.ok_or("a message names its sender")?;
    if from != named {
        return Err(format!("{id} names {named} as its sender, not {from}"));
    }
    if !holding
        .names
        .iter()
        .any(|name| name.eq_ignore_ascii_case(from))
    {
        return Err(format!(
            "{from} isn't a name of {}, present here",
            holding.session
        ));
    }
    let to = targets(message.get("to"), holding)?;
    let text = words("text")?.ok_or("a message says something")?;
    let mut body = json!({
        "v": 1, "id": id, "at": at, "from": from, "to": to, "text": text, "pid": pid,
    });
    match words("intent")? {
        None => {}
        Some(intent @ ("fyi" | "question" | "finding")) => body["intent"] = json!(intent),
        Some(intent) => return Err(format!("a worker can't say a {intent}")),
    }
    if message.get("endsHold").and_then(Value::as_bool) == Some(true) {
        return Err("a worker can't end a hold".to_owned());
    }
    for field in ["re", "supersedes"] {
        if let Some(said) = words(field)? {
            body[field] = json!(said);
        }
    }
    if message.get("hosted").and_then(Value::as_bool) == Some(true) {
        body["hosted"] = json!(true);
    }
    if let Some(invite) = words("invite")? {
        if !own_quest(invite, holding) {
            return Err(format!(
                "a worker invites only to its own quest, not {invite}"
            ));
        }
        body["invite"] = json!(invite);
    }
    Ok((id.to_owned(), body))
}

/// The groups `to` names, as the mesh writes them, when each is within
/// `holding`'s reach: a peer by address, or its own quest.
fn targets(to: Option<&Value>, holding: &Holding) -> Result<Vec<String>, String> {
    let to = to
        .and_then(Value::as_array)
        .filter(|to| !to.is_empty())
        .ok_or("a message goes to a list of groups")?;
    to.iter()
        .map(|target| {
            let target = target.as_str().ok_or("each group it goes to is words")?;
            let peer = target.strip_prefix('@').is_some_and(|address| {
                !address.is_empty() && address.bytes().all(|byte| byte.is_ascii_alphanumeric())
            });
            if peer || own_quest(target, holding) {
                Ok(target.to_owned())
            } else {
                Err(format!(
                    "{target} is beyond a worker's reach: its own quest and peers by address"
                ))
            }
        })
        .collect()
}

/// Whether `target` is `holding`'s own quest's group.
fn own_quest(target: &str, holding: &Holding) -> bool {
    holding.quest.is_some() && mesh::group_for(target) == holding.quest
}

/// A watch of the messages within a worker's session's reach.
struct Watch {
    holding: Holding,
    pid: i32,
    groups: Option<Pattern>,
    since: Option<String>,
}

impl Watch {
    /// Sends the latest `recent` messages within its reach, after its
    /// marker if it names one, then each new one, until the client says
    /// anything more, hangs up or falls behind, or the session is no
    /// longer present through its process.
    async fn keep<S: Store>(
        &self,
        shared: &Shared<S>,
        holdings: &Holdings,
        recent: usize,
        mut lines: tokio::io::Lines<BufReader<OwnedReadHalf>>,
        mut writing: OwnedWriteHalf,
    ) {
        let watching: Reply = Ok(Said::Watching);
        if write(&mut writing, &watching).await.is_err() {
            return;
        }
        let (id, mut events) = shared.watch(Some(&Pattern::everything()), recent);
        loop {
            tokio::select! {
                event = events.recv() => {
                    let Some(event) = event else { break };
                    if holdings.held_by(self.pid, &self.holding.session).is_none() {
                        break;
                    }
                    let Some(heard) = self.heard(&event, |id| shared.store.message(id)) else {
                        continue;
                    };
                    let written =
                        tokio::time::timeout(WATCH_PATIENCE, write(&mut writing, &heard)).await;
                    if !matches!(written, Ok(Ok(()))) {
                        break;
                    }
                }
                // Anything it sends, or its hanging up, ends the watch.
                _ = lines.next_line() => break,
            }
        }
        shared.unwatch(id);
    }

    /// What the watch hears of `event`, each message as `file` reads it:
    /// nothing of sessions, nor of a message beyond its reach.
    fn heard(&self, event: &Event, file: impl Fn(&str) -> Option<Value>) -> Option<Heard> {
        match event {
            Event::MessagesChanged { messages } => Some(Heard::MessagesChanged {
                messages: messages
                    .iter()
                    .filter(|message| self.after(message) && self.reaches(message))
                    .filter_map(|message| file(&message.id))
                    .collect(),
            }),
            Event::Message { message } if self.reaches(message) => {
                file(&message.id).map(|message| Heard::Message { message })
            }
            _ => None,
        }
    }

    /// Whether `message` goes to a group within the session's reach, and
    /// that the watch asked for, if it asked.
    fn reaches(&self, message: &Message) -> bool {
        message.groups.iter().any(|group| {
            let ours = self.holding.quest.as_ref() == Some(group)
                || self.holding.names.iter().any(|name| {
                    group.as_str().strip_prefix("peer.") == Some(name.to_lowercase().as_str())
                });
            ours && self
                .groups
                .as_ref()
                .is_none_or(|groups| groups.matches(group))
        })
    }

    /// Whether `message` comes after the watch's marker: a later time, or
    /// the same time and a later id, so a marker the node doesn't hold
    /// starts from its time.
    fn after(&self, message: &Message) -> bool {
        let Some(since) = &self.since else {
            return true;
        };
        let time = |id: &str| {
            id.split_once('-')
                .and_then(|(millis, _)| millis.parse::<i64>().ok())
        };
        (time(&message.id), message.id.as_str()) > (time(since), since.as_str())
    }
}

#[cfg(test)]
mod tests;
