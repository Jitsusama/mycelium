//! The node: one per machine and realm. It keeps its store linked with the
//! stores of the machines discovery finds, holds the sessions and messages
//! in memory, and serves the local socket every client reads.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use crate::group::{Pattern, Routes};
use crate::link::{self, dial::Dial, discovery::Discovery, membership::Membership};
use crate::mesh;
use crate::record::Message;
use crate::state::State;
use crate::store::{Change, Store};
use crate::wire::{Draft, Event, Reply, Request, Response};

/// How often sessions are judged again, since a session ending changes its
/// process, not its presence.
const LIVENESS_EVERY: Duration = Duration::from_secs(5);

/// How long a link waits before dialling again after it dropped, at first
/// and at most; each failure in a row doubles the wait.
const REDIAL_FIRST: Duration = Duration::from_secs(2);
const REDIAL_MOST: Duration = Duration::from_secs(60);

/// How many events a watch may fall behind by before the node lets it go;
/// a client that can't keep up connects again and is sent the whole state.
const WATCH_BACKLOG: usize = 4096;

/// Runs a node serving `socket` until `stop` resolves, then takes the
/// socket down.
pub async fn run<S, M, D>(
    store: S,
    membership: M,
    discovery: D,
    socket: &Path,
    stop: impl Future<Output = ()>,
) -> io::Result<()>
where
    S: Store,
    M: Membership + Clone,
    D: Discovery,
{
    let listener = bind(socket)?;
    let machine = membership.hello().machine;
    let_go_of_elsewhere(&store);
    let shared = Shared::new(store.clone(), machine);
    let mut changes = store.changes()?;
    shared.read_store();

    let mut found = discovery.found();
    let mut tick = tokio::time::interval(LIVENESS_EVERY);
    tokio::pin!(stop);
    let result = loop {
        tokio::select! {
            () = &mut stop => break Ok(()),
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    tokio::spawn(serve(stream, shared.clone()));
                }
                Err(error) => break Err(error),
            },
            Some(dial) = found.recv() => {
                tokio::spawn(keep_linked(dial, store.clone(), membership.clone()));
            }
            _ = tick.tick() => shared.read_sessions(),
            change = changes.changes.recv() => match change {
                Some(Change::Message(id)) => shared.read_message(&id),
                Some(Change::Presence) => shared.read_sessions(),
                None => break Err(io::Error::other("the store stopped telling its changes")),
            },
        }
    };
    // The socket is the node's alone, made at bind; if it's already gone
    // there is nothing to take down.
    let _ = fs::remove_file(socket);
    result
}

/// Lets go of every session elsewhere a node before this one left behind:
/// no link stands yet, and each link brings its machine's sessions afresh.
fn let_go_of_elsewhere(store: &impl Store) {
    for presence in store.presences() {
        if presence.elsewhere.is_some() {
            // A presence that won't go now goes at the next start.
            let _ = store.remove_presence(&presence.session);
        }
    }
}

/// Makes the socket in a folder only its owner can enter, refusing to take
/// over one a live node still serves.
fn bind(socket: &Path) -> io::Result<UnixListener> {
    if let Some(parent) = socket.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    if socket.exists() {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("a node already serves {}", socket.display()),
            ));
        }
        fs::remove_file(socket)?;
    }
    let listener = UnixListener::bind(socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Keeps a link up for as long as the node runs, dialling again after each
/// drop.
async fn keep_linked(dial: impl Dial, store: impl Store, membership: impl Membership) {
    let mut wait = REDIAL_FIRST;
    loop {
        let started = tokio::time::Instant::now();
        let ended = match dial.dial(&membership.hello().realm).await {
            Ok(connection) => link::run(store.clone(), &membership, connection).await,
            Err(error) => Err(error),
        };
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

/// What the node's loop and every client share.
struct Shared<S> {
    store: S,
    machine: String,
    inner: Arc<Mutex<Inner>>,
}

impl<S: Clone> Clone for Shared<S> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            machine: self.machine.clone(),
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
}

impl<S: Store> Shared<S> {
    fn new(store: S, machine: String) -> Self {
        Self {
            store,
            machine,
            inner: Arc::default(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding the lock leaves the state as it was before
        // the change that panicked, so carrying on with it is sound.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reads every message and session the store holds.
    fn read_store(&self) {
        let messages = self
            .store
            .message_ids(0)
            .iter()
            .filter_map(|id| mesh::message(&self.store.message(id)?))
            .collect();
        self.lock().state.add_messages(messages);
        self.read_sessions();
    }

    /// Judges every session again and sends what changed.
    fn read_sessions(&self) {
        let sessions = self
            .store
            .presences()
            .iter()
            .filter_map(|presence| mesh::session(presence, &self.machine))
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
        let (id, body) = mesh::written(draft, std::process::id());
        self.store
            .put_message(&id, &body)
            .map_err(|error| format!("the store refused the message: {error}"))?;
        mesh::message(&body).ok_or_else(|| "the message didn't read back".to_owned())
    }

    fn answer(&self, request: Request) -> Reply {
        match request {
            Request::Sessions => Ok(Response::Sessions(self.lock().state.sessions())),
            Request::Messages { groups, limit } => {
                Ok(Response::Messages(self.lock().state.recent(&groups, limit)))
            }
            Request::Send(draft) => self.send(&draft).map(Response::Sent),
            Request::Watch { .. } => Err("a watch is answered by `watch`".to_owned()),
        }
    }

    /// Starts a watch: its first events are the whole state, taken under
    /// the same lock as every later change, so none is missed or doubled.
    fn watch(&self, groups: &Pattern, recent: usize) -> (u64, mpsc::Receiver<Event>) {
        let mut inner = self.lock();
        let (sender, receiver) = mpsc::channel(WATCH_BACKLOG);
        let first = [
            Event::SessionsChanged {
                sessions: inner.state.sessions(),
            },
            Event::MessagesChanged {
                messages: inner.state.recent(groups, recent),
            },
        ];
        for event in first {
            // The channel is new and holds more than two, so this can't fail.
            let _ = sender.try_send(event);
        }
        let id = inner.next_watch;
        inner.next_watch += 1;
        inner.watches.insert(id, sender);
        inner.routes.insert(groups, id);
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
    let (reading, mut writing) = stream.into_split();
    let mut lines = BufReader::new(reading).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let request = serde_json::from_str::<Request>(&line)
            .map_err(|error| format!("not a request: {error}"));
        if let Ok(Request::Watch { groups, recent }) = request {
            if write(&mut writing, &Reply::Ok(Response::Watching))
                .await
                .is_ok()
            {
                let (id, mut events) = shared.watch(&groups, recent);
                while let Some(event) = events.recv().await {
                    if write(&mut writing, &event).await.is_err() {
                        break;
                    }
                }
                shared.unwatch(id);
            }
            return;
        }
        let reply = request.and_then(|request| shared.answer(request));
        if write(&mut writing, &reply).await.is_err() {
            return;
        }
    }
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
    use crate::link::dial::Ssh;
    use crate::link::discovery::Listed;
    use crate::link::membership::Realm;
    use crate::record::Intent;
    use crate::store::memory::Memory;

    /// How long the test waits on the node before failing.
    const PATIENCE: Duration = Duration::from_secs(5);

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
        let node = tokio::spawn({
            let (store, socket) = (store.clone(), socket.clone());
            async move {
                let stopped = async {
                    // A dropped sender stops the node as surely as a sent stop.
                    let _ = stopped.await;
                };
                run(
                    store,
                    membership,
                    Listed::<Ssh>(Vec::new()),
                    &socket,
                    stopped,
                )
                .await
            }
        });
        // The node binds its socket as soon as it runs; until then there is
        // nothing to hear, so this test yields to it until the socket is there.
        tokio::time::timeout(PATIENCE, async {
            while !socket.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the node binds its socket");

        let asked = tokio::task::spawn_blocking(move || {
            let mut watch = Client::connect(&socket).expect("the watch connects");
            let watching = watch.ask(&Request::Watch {
                groups: Pattern::new("peer.*").expect("a valid pattern"),
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
}
