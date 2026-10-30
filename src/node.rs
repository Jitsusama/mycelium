//! The node: one per machine and realm. It keeps its store linked with the
//! stores of the machines discovery finds, holds the sessions and messages
//! in memory, and serves the local socket every client reads.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::facts;
use crate::group::{Pattern, Routes};
use crate::hold::{self, start, start::Starter};
use crate::link::refused::Refusals;
use crate::link::starts::{Asks, Starting};
use crate::link::{self, dial::Dial, discovery::Discovery, membership::Membership};
use crate::mesh;
use crate::place;
use crate::placement::{self, Placement};
use crate::record::{Message, Session, State as SessionState};
use crate::share::exchange::Places;
use crate::state::State;
use crate::steer::{self, Steer};
use crate::store::{self, Change, Store};
use crate::wire::{Draft, Event, Reply, Request, Response, Start, Started};

/// How long a link waits before dialling again after it dropped, at first
/// and at most; each failure in a row doubles the wait.
const REDIAL_FIRST: Duration = Duration::from_secs(2);
const REDIAL_MOST: Duration = Duration::from_secs(60);

/// How long a link's way in may take to finish saying why it ended, once
/// the link is over.
const SAID_WITHIN: Duration = Duration::from_secs(5);

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
    let hello = membership.hello();
    let Given {
        sharing,
        starter,
        refusals,
        steers,
    } = given;
    let shared = Shared {
        held: place::held(&hello.realm),
        steers,
        store: store.clone(),
        machine: hello.machine,
        realm: hello.realm,
        starter: starter.map(Arc::new),
        asks: Asks::default(),
        inner: Arc::default(),
    };
    let mut changes = store.changes()?;
    let (_watching_held, mut held) = match hold::watch(&shared.held) {
        Ok((watcher, held)) => (Some(watcher), held),
        Err(error) => {
            eprintln!("what is held isn't watched, so is read only with presence: {error}");
            (None, mpsc::unbounded_channel().1)
        }
    };
    shared.read_store();

    let mut links = JoinSet::new();
    let mut found = discovery.found();
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
                let starting = Starting {
                    node: socket.path.clone(),
                    held: shared.held.clone(),
                    asking: Some((shared.asks.clone(), {
                        let dial = dial.clone();
                        Arc::new(move |words: &[String]| dial.there(words))
                    })),
                    steers: shared.steers.clone(),
                };
                let carries = (sharing.clone(), starting, refusals.clone());
                links.spawn(keep_linked(dial, store.clone(), membership.clone(), carries));
            }
            Some(()) = held.recv() => shared.read_sessions(),
            change = changes.changes.recv() => match change {
                Some(Change::Message(id)) => shared.read_message(&id),
                Some(Change::Presence) => shared.read_sessions(),
                Some(Change::Rescan) => shared.read_store(),
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
            Ok(()) => eprintln!("the link over {} ended", dial.describe()),
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

/// What the node's loop and every client share.
struct Shared<S> {
    store: S,
    machine: String,
    realm: String,
    /// The realm's folder of held sessions on this machine.
    held: PathBuf,
    /// This machine's sessions' steer inboxes.
    steers: PathBuf,
    starter: Option<Arc<Starter>>,
    /// The links this node asks other machines' starts through.
    asks: Asks,
    inner: Arc<Mutex<Inner>>,
}

impl<S: Clone> Clone for Shared<S> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            machine: self.machine.clone(),
            realm: self.realm.clone(),
            held: self.held.clone(),
            steers: self.steers.clone(),
            starter: self.starter.clone(),
            asks: self.asks.clone(),
            inner: self.inner.clone(),
        }
    }
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
}

impl<S: Store> Shared<S> {
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
        let messages = unread
            .iter()
            .filter_map(|id| mesh::message(&self.store.message(id)?))
            .collect();
        self.lock().state.add_messages(messages);
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
                let mut session = mesh::session(presence, &self.machine)?;
                if let Some(one) = mesh::held_by(presence, &held) {
                    session.held = Some(one.id.clone());
                    // A held session is in whichever window joined it
                    // last and is still joined, or in none.
                    session.window = one.windows.last().cloned();
                }
                session.attach = self.attach(&session);
                Some(session)
            })
            .collect();
        let mut inner = self.lock();
        for event in inner.state.set_sessions(sessions) {
            inner.send_all(&event);
        }
    }

    /// Reads a message that landed and sends it to those who hear it.
    fn read_message(&self, id: &str) {
        let Some(message) = self.store.message(id).as_ref().and_then(mesh::message) else {
            return;
        };
        let mut inner = self.lock();
        if let Some(message) = inner.state.add_message(message) {
            inner.send_message(message);
        }
    }

    /// Sends a message on the mesh; the store's change brings it back to
    /// every watch, as any session's message comes.
    fn send(&self, draft: &Draft) -> Result<Message, String> {
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

    /// The words that join `session` from a terminal here, when it is held
    /// here or on a machine a link of this node's reaches.
    fn attach(&self, session: &Session) -> Option<Vec<String>> {
        let words = start::attach_here(&self.realm, session.held.as_ref()?);
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

    /// Every machine of the realm: this one measured now, then each other
    /// as its links last carried it.
    fn machines(&self) -> Vec<facts::Machine> {
        let mut all = vec![self.measure()];
        all.extend(
            self.store
                .machines()
                .into_iter()
                .filter(|one| one.name != self.machine),
        );
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
            Request::Watch { .. } => Err("a watch is answered by `watch`".to_owned()),
            Request::Present => Err("being present is answered by `be_present`".to_owned()),
            Request::Start(_) => Err("a start is answered by `start`".to_owned()),
            Request::Steer { .. } => Err("a steer is answered by `steer`".to_owned()),
        }
    }

    /// Puts what the person says into the inbox of the session going by
    /// `address`, on the one machine it runs on: here, or through the link
    /// reaching that machine. A talk's file is opened here first, so the
    /// replies carried back land where the person's launcher reads them.
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
            .filter(|session| matches!(session.state, SessionState::Busy | SessionState::Idle))
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
            return self.asks.steer(&machine, steer).await;
        }
        steer
            .deliver(&self.steers)
            .map_err(|error| format!("its inbox refused it: {error}"))?;
        Ok(Response::Steered { machine })
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
    /// where and how to join it, or why it didn't start.
    async fn start(&self, asked: Start) -> Reply {
        if asked.command.is_empty() {
            return Err("nothing to start: name a program".to_owned());
        }
        let machine = match placement::place(&asked.work, &self.machines()) {
            Placement::Placed { machine, .. } => machine,
            Placement::Pending { misses } => {
                return Err(format!(
                    "nowhere now, though a machine out of reach would do: {}",
                    placement::said(&misses)
                ));
            }
            Placement::Refused { misses } => {
                return Err(format!("nowhere: {}", placement::said(&misses)));
            }
        };
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
        let command = start::through_login_shell(&asked.command);
        let begins = folder.clone();
        let held = tokio::task::spawn_blocking(move || starter.start(&realm, &begins, &command))
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
        let request = serde_json::from_str::<Request>(&line)
            .map_err(|error| format!("not a request: {error}"));
        if let Ok(Request::Present) = request {
            be_present(&shared, process, lines, writing).await;
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
        let reply = match request {
            Ok(Request::Start(asked)) => shared.start(asked).await,
            Ok(Request::Steer { address, steer }) => shared.steer(&address, &steer).await,
            asked => asked.and_then(|request| shared.answer(request)),
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
    use serde_json::json;

    use super::*;
    use crate::client::Client;
    use crate::group::Group;
    use crate::link::dial::{Connection, Ssh};
    use crate::link::discovery::Listed;
    use crate::link::membership::Realm;
    use crate::record;
    use crate::record::Intent;
    use crate::store::memory::Memory;

    /// How long the test waits on the node before failing.
    const PATIENCE: Duration = Duration::from_secs(5);

    /// A holder in `dir` that says it holds at once, writing down in the
    /// file it answers beside it what it was given to hold.
    fn stand_in_holder(dir: &Path) -> (PathBuf, PathBuf) {
        let given = dir.join("given");
        let holder = dir.join("holder");
        fs::write(
            &holder,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\necho held\n",
                given.display()
            ),
        )
        .expect("the stand-in holder is written");
        fs::set_permissions(&holder, fs::Permissions::from_mode(0o755))
            .expect("the stand-in holder runs");
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

    fn work(needs: Vec<placement::Need>) -> placement::Work {
        placement::Work {
            needs,
            prefers: Vec::new(),
            tolerates: Vec::new(),
            may_wait: false,
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
        let node = tokio::spawn(async move {
            let given = Given {
                sharing: None,
                starter,
                refusals: Refusals::default(),
                steers,
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
            tokio::spawn(async move {
                let realm = Realm {
                    name: "work".to_owned(),
                    machine: "equilibrius".to_owned(),
                };
                let starting = Starting {
                    node,
                    held: held_here,
                    asking: None,
                    steers,
                };
                // The test judges the link by what crosses it.
                let _ = link::run(store, &realm, piped(far), None, Some(starting)).await;
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

    fn piped(pipe: tokio::io::DuplexStream) -> Connection {
        let (reading, writing) = tokio::io::split(pipe);
        Connection {
            reading: Box::new(reading),
            writing: Box::new(writing),
            keep: Box::new(()),
            refused: None,
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

        let there = placement::Need::Machine("equilibrius".to_owned());
        let asked = Request::Start(Start {
            work: work(vec![there]),
            folder: Some(dir.path().to_owned()),
            command: vec!["pi".to_owned()],
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
                let given = Given {
                    sharing: None,
                    starter: None,
                    refusals: Refusals::default(),
                    steers,
                };
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
            };
            let sent = sender.ask(&Request::Send(draft));
            let Ok(Response::Sent(sent)) = sent.expect("the node answers") else {
                panic!("the node sends the message");
            };
            let heard = watch.next_event().expect("the message comes");
            assert_eq!(
                heard,
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
            let given = Given {
                sharing: None,
                starter: Some(Starter {
                    program: holder,
                    scoped: false,
                }),
                refusals: Refusals::default(),
                steers,
            };
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
                });
                client.ask(&asked).expect("the node answers")
            };
            let elsewhere = placement::Need::Machine("equilibrius".to_owned());
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
        // them, is start's own test; here, only that the node uses them.
        let through = start::through_login_shell(&["pi".to_owned(), "--continue".to_owned()]);
        assert_eq!(
            given[7..],
            ["--"]
                .into_iter()
                .chain(through.iter().map(String::as_str))
                .collect::<Vec<_>>(),
            "the program starts through the login shell"
        );

        drop(stop);
        let stopped = node.await.expect("the node runs to its end");
        assert!(stopped.is_ok(), "the node stops cleanly: {stopped:?}");
    }
}
