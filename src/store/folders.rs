//! The harness mesh's own folders as a store: one JSON file per message in
//! `messages/`, and one per session in `presence/` and in `leases/`.
//!
//! Every file is written whole, through a dot-file renamed into place, as
//! the mesh writes its own, so no reader sees half of one. A session's file
//! stays after it ends, so whether it lives is judged from its process, as
//! the mesh judges it, and its end is told as a change the moment its
//! process exits (see `ends`), or the moment the connection it held to its
//! node to say it was present closes. The presence of a session elsewhere is written with
//! its process taken out, which the mesh takes as there, and with `machine`
//! naming where it runs; it is never written over a session of another
//! machine's, nor over one of this machine's but once it has ended here,
//! as a session carried to that machine does. The leases such a session
//! holds are kept in one holding of its own, marked the same way, which
//! goes with its presence.
//!
//! Claims are locks on files of mycelium's own, outside the mesh, so they
//! hold across every mycelium process on the machine and go when the
//! process holding one does, however it ends.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use notify::event::ModifyKind;
use notify::{EventKind, RecursiveMode, Watcher};
use rustix::fs::FlockOperation;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;

use super::ends::Ends;
use super::{Change, Changes, Claim, Presence, Store};
use crate::facts::Machine;

/// How what another machine says it is ends, beside its claim.
const SAID: &str = ".machine";

/// What the name of a file holding the carries a machine's node told of
/// ends with, after the machine's name.
const TOLD: &str = ".carries";

/// The field naming the machine a session runs on, written only on the
/// presence of a session elsewhere.
pub(crate) const MACHINE: &str = "machine";

/// The field saying since when, in milliseconds since the epoch, no link
/// has reached the machine of a session elsewhere.
const UNREACHABLE: &str = "unreachableSince";

/// How long the ends of a machine's links are kept, in milliseconds: a
/// day, which is as far back as anything asks.
const ENDINGS_KEPT: i64 = 24 * 60 * 60 * 1000;

/// How long a verdict on a session's process is kept while no end is told,
/// so a process whose end no watch tells is still judged again this often.
const JUDGED_FOR: Duration = Duration::from_secs(60);

/// Numbers each staged write of this process, so no two share a file.
static STAGED: AtomicU64 = AtomicU64::new(0);

/// The harness mesh's folders on this machine.
#[derive(Debug, Clone)]
pub struct Folders {
    dir: Arc<PathBuf>,
    claims: Arc<PathBuf>,
    /// This machine's processes said to have ended as their connections
    /// closed, whose sessions are judged ended whatever their process.
    ended: Arc<Mutex<BTreeSet<i32>>>,
    /// Each reader of this store's changes, told of an end said here.
    told: Arc<Mutex<Vec<mpsc::UnboundedSender<Change>>>>,
    /// Each presence as last read, by its path, kept until its file is
    /// another: each session rewrites its own every 30 seconds, and the
    /// node reads them all as any one changes, so reading only those that
    /// changed keeps an idle node from reading 150 files a second.
    kept: Arc<Mutex<BTreeMap<PathBuf, Kept>>>,
    /// Whether each session's process lives, as last judged.
    judged: Arc<Judged>,
}

/// Whether each session's process lives, as last judged, kept until an end
/// may have been told. The node reads every presence at each change and
/// each message, many a second, and judging a process reads the system's
/// word on it; with a hundred sessions that was over a thousand reads a
/// second, most of a busy node's reading.
#[derive(Debug, Default)]
struct Judged {
    /// Moves each time a process's end may be told, before it is.
    ends: Arc<AtomicU64>,
    verdicts: Mutex<HashMap<Owner, Verdict>>,
}

/// A process judged: whether it lived, when, at which count of `ends`, and
/// how often it has been judged.
#[derive(Debug, Clone, Copy)]
struct Verdict {
    lives: bool,
    at: Instant,
    ends: u64,
    times: u32,
}

/// A presence as it was read, and the file it was read from, by what
/// changes whenever it is written: written whole by rename, a file is a
/// new inode, and written in place, its length or time moves.
#[derive(Debug)]
struct Kept {
    inode: u64,
    len: u64,
    modified: Option<SystemTime>,
    body: Option<Arc<Value>>,
}

impl Folders {
    /// The mesh kept in `dir`, its two folders made if they are missing,
    /// with claims kept as locks in `claims`.
    pub fn open(dir: &Path, claims: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir.join("messages"))?;
        fs::create_dir_all(dir.join("presence"))?;
        fs::create_dir_all(dir.join("leases"))?;
        fs::create_dir_all(claims)?;
        // Changes name the real path, so the folders are kept by it: a path
        // through a link, as macOS's temporary folders are, would match none.
        Ok(Self {
            dir: Arc::new(dir.canonicalize()?),
            claims: Arc::new(claims.canonicalize()?),
            ended: Arc::default(),
            told: Arc::default(),
            kept: Arc::default(),
            judged: Arc::default(),
        })
    }

    /// When each link with `machine` kept here ended, oldest first; none
    /// for a machine whose name isn't safe as a file's, or never ended.
    fn ended_at(&self, machine: &str) -> Vec<i64> {
        safe(machine)
            .ok()
            .and_then(|name| read(&self.claims.join(format!("{name}.ended"))))
            .unwrap_or_default()
    }

    /// Each presence in the folder, read again only where its file changed
    /// since it was last read, and none kept of one gone; each shared, not
    /// copied, with what reads it.
    fn presence_bodies(&self) -> Vec<Arc<Value>> {
        use std::os::unix::fs::MetadataExt;
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let mut seen = BTreeMap::new();
        for path in records(&self.presence()) {
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            let same = kept.remove(&path).filter(|one| {
                one.inode == meta.ino()
                    && one.len == meta.len()
                    && one.modified == meta.modified().ok()
            });
            let one = same.unwrap_or_else(|| Kept {
                inode: meta.ino(),
                len: meta.len(),
                modified: meta.modified().ok(),
                body: read::<Value>(&path).map(Arc::new),
            });
            seen.insert(path, one);
        }
        *kept = seen;
        kept.values().filter_map(|one| one.body.clone()).collect()
    }

    /// Whether `owner`'s process lives, judged again only once an end may
    /// have been told since it last was, or [`JUDGED_FOR`] has passed.
    /// Each owner not among `present` is forgotten.
    fn lives(&self, owner: &Owner, boot: Option<&str>) -> bool {
        let ends = self.judged.ends.load(Ordering::Acquire);
        let mut verdicts = self
            .judged
            .verdicts
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let was = verdicts.get(owner).copied();
        if let Some(was) = was
            && was.ends == ends
            && was.at.elapsed() < JUDGED_FOR
        {
            return was.lives;
        }
        let lives = owner.lives(boot);
        verdicts.insert(
            owner.clone(),
            Verdict {
                lives,
                at: Instant::now(),
                ends,
                times: was.map_or(0, |was| was.times) + 1,
            },
        );
        lives
    }

    /// Whether the session of this machine's that `facts` are of still
    /// lives: its process neither said to have `ended` nor judged gone.
    fn own_lives(&self, facts: &Facts, ended: &BTreeSet<i32>, boot: Option<&str>) -> bool {
        facts
            .owner
            .as_ref()
            .is_some_and(|owner| !ended.contains(&owner.pid) && self.lives(owner, boot))
    }

    /// Forgets each verdict on a process no presence names any more.
    fn forget_judged(&self, present: &BTreeSet<&Owner>) {
        self.judged
            .verdicts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|owner, _| present.contains(owner));
    }

    fn ended_processes(&self) -> MutexGuard<'_, BTreeSet<i32>> {
        // Each change to the set is one insert or remove, whole either way.
        self.ended.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Tells every reader the sessions should be judged again.
    fn tell_presence(&self) {
        self.told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|told| told.send(Change::Presence).is_ok());
    }

    fn messages(&self) -> PathBuf {
        self.dir.join("messages")
    }

    /// The folder the mesh moves the message `id` into once it is six
    /// hours old: `messages/<YYYY-MM-DD>/`, by the UTC day its id says it
    /// was sent on. Holds stay at the top until they end.
    fn moved(&self, id: &str) -> Option<PathBuf> {
        Some(self.messages().join(day_of(sent_at(id)?)?))
    }

    fn presence(&self) -> PathBuf {
        self.dir.join("presence")
    }

    fn leases(&self) -> PathBuf {
        self.dir.join("leases")
    }

    /// The presence file of `session`, if `machine` carries it here.
    fn carried(&self, session: &str, machine: &str) -> io::Result<Option<PathBuf>> {
        let path = self.presence().join(format!("{}.json", safe(session)?));
        let carried =
            read::<Facts>(&path).is_some_and(|facts| facts.machine.as_deref() == Some(machine));
        Ok(carried.then_some(path))
    }
}

/// Watches the process of each living session of this machine's whose
/// presence is at `paths`, so its end is told.
fn watch_ends(ends: &Ends, paths: impl Iterator<Item = PathBuf>) {
    let boot = boot_id();
    let owners = paths
        .filter_map(|path| read::<Facts>(&path))
        .filter(|facts| facts.machine.is_none())
        .filter_map(|facts| facts.owner);
    for owner in owners {
        if owner.lives(boot.as_deref()) {
            ends.watch(owner.pid, || owner.lives(boot.as_deref()));
        }
    }
}

impl Store for Folders {
    fn message_ids(&self, since: i64) -> Vec<String> {
        // A day folder holds only messages sent that day, so one before
        // `since`'s day holds none that new.
        let earliest = day_of(since).unwrap_or_default();
        let messages = self.messages();
        let days: Vec<PathBuf> = days(&messages)
            .filter(|day| *day >= earliest)
            .map(|day| messages.join(day))
            .collect();
        let ids: BTreeSet<String> = records(&messages)
            .chain(days.iter().flat_map(|day| records(day)))
            .filter_map(|path| Some(path.file_stem()?.to_str()?.to_owned()))
            .filter(|id| sent_at(id).is_some_and(|at| at >= since))
            .collect();
        ids.into_iter().collect()
    }

    fn message(&self, id: &str) -> Option<Value> {
        let file = format!("{}.json", safe(id).ok()?);
        read(&self.messages().join(&file)).or_else(|| read(&self.moved(id)?.join(&file)))
    }

    fn put_message(&self, id: &str, body: &Value) -> io::Result<()> {
        let file = format!("{}.json", safe(id)?);
        let moved = self.moved(id).is_some_and(|day| day.join(&file).exists());
        if moved || self.messages().join(&file).exists() {
            return Ok(());
        }
        write_whole(&self.messages(), &file, body)
    }

    fn presences(&self) -> Vec<Presence> {
        let boot = boot_id();
        let ended = self.ended_processes().clone();
        let read: Vec<(Facts, Arc<Value>)> = self
            .presence_bodies()
            .into_iter()
            .filter_map(|body| Some((Facts::deserialize(&*body).ok()?, body)))
            .collect();
        let owners = read
            .iter()
            .filter(|(facts, _)| facts.machine.is_none())
            .filter_map(|(facts, _)| facts.owner.as_ref())
            .collect();
        self.forget_judged(&owners);
        read.into_iter()
            .map(|(facts, body)| {
                let lives =
                    facts.machine.is_some() || self.own_lives(&facts, &ended, boot.as_deref());
                Presence {
                    session: facts.session,
                    unreachable_since: facts.machine.as_ref().and(facts.unreachable_since),
                    elsewhere: facts.machine,
                    lives,
                    body,
                }
            })
            .collect()
    }

    fn claim(&self, machine: &str) -> io::Result<Option<Claim>> {
        let lock = fs::File::create(self.claims.join(format!("{}.lock", safe(machine)?)))?;
        match rustix::fs::flock(&lock, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(Box::new(lock))),
            Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn linked_until(&self, machine: &str) -> Option<i64> {
        let file = self.claims.join(format!("{}.linked", safe(machine).ok()?));
        fs::read_to_string(file).ok()?.trim().parse().ok()
    }

    fn mark_linked(&self, machine: &str, at: i64) -> io::Result<()> {
        let file = format!("{}.linked", safe(machine)?);
        write_whole(&self.claims, &file, &Value::from(at))
    }

    fn mark_ended(&self, machine: &str, at: i64) -> io::Result<()> {
        let file = format!("{}.ended", safe(machine)?);
        // Written whole, the last day's alone, so a link dropping each
        // minute keeps the file to about 1,440 numbers.
        let mut ended = self.ended_at(machine);
        ended.retain(|then| *then > at - ENDINGS_KEPT);
        ended.push(at);
        write_whole(&self.claims, &file, &Value::from(ended))
    }

    fn endings(&self, machine: &str, since: i64) -> usize {
        self.ended_at(machine)
            .into_iter()
            .filter(|then| *then >= since)
            .count()
    }

    fn refuse_link(&self, machine: &str, why: &str) -> io::Result<()> {
        let file = format!("{}.refused", safe(machine)?);
        write_whole(&self.claims, &file, &Value::from(why))
    }

    fn link_refused(&self, machine: &str) -> Option<String> {
        read(&self.claims.join(format!("{}.refused", safe(machine).ok()?)))
    }

    fn link_admitted(&self, machine: &str) -> io::Result<()> {
        let file = self.claims.join(format!("{}.refused", safe(machine)?));
        match fs::remove_file(file) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    fn put_presence(&self, session: &str, machine: &str, body: &Value) -> io::Result<()> {
        let file = format!("{}.json", safe(session)?);
        let mut body = body.clone();
        let Some(fields) = body.as_object_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a presence is an object",
            ));
        };
        if fields.get("session").and_then(Value::as_str) != Some(session) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the presence of {session:?} names another session"),
            ));
        }
        // A session of this machine's that ended here and runs on
        // `machine` now, carried there, is that machine's to speak for:
        // the mesh keeps a session's file after it ends, which would
        // otherwise stand for it here as ended for good.
        if let Some(held) = read::<Facts>(&self.presence().join(&file))
            && held.machine.as_deref() != Some(machine)
            && (held.machine.is_some()
                || self.own_lives(&held, &self.ended_processes().clone(), boot_id().as_deref()))
        {
            let whose = held.machine.unwrap_or_else(|| "this machine".to_owned());
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{session:?} is a session of {whose}'s, not {machine}'s"),
            ));
        }
        // Only the session's own machine can judge its process, and a link
        // carrying it has reached that machine.
        fields.remove("owner");
        fields.remove(UNREACHABLE);
        fields.insert(MACHINE.to_owned(), Value::String(machine.to_owned()));
        write_whole(&self.presence(), &file, &body)
    }

    fn mark_unreachable(&self, session: &str, machine: &str, since: i64) -> io::Result<()> {
        let Some(path) = self.carried(session, machine)? else {
            return Ok(());
        };
        let Some(mut body) = read::<Value>(&path) else {
            return Ok(());
        };
        let Some(fields) = body.as_object_mut() else {
            return Ok(());
        };
        if fields.contains_key(UNREACHABLE) {
            return Ok(());
        }
        // The file's time is when a link last said the session stands,
        // which the mesh reads, so marking it keeps that time.
        let vouched = fs::metadata(&path)?.modified()?;
        fields.insert(UNREACHABLE.to_owned(), Value::from(since));
        write_whole(&self.presence(), &format!("{}.json", safe(session)?), &body)?;
        fs::File::options()
            .append(true)
            .open(&path)?
            .set_modified(vouched)
    }

    fn keep_presence(&self, session: &str, machine: &str) -> io::Result<()> {
        match self.carried(session, machine)? {
            Some(path) => fs::File::options()
                .append(true)
                .open(path)?
                .set_modified(SystemTime::now()),
            None => Ok(()),
        }
    }

    fn remove_presence(&self, session: &str, machine: &str) -> io::Result<()> {
        let Some(path) = self.carried(session, machine)? else {
            return Ok(());
        };
        fs::remove_file(path)?;
        self.put_leases(session, machine, &[])
    }

    fn holdings(&self) -> Vec<Value> {
        records(&self.leases())
            .filter_map(|path| read::<Value>(&path))
            .collect()
    }

    fn put_leases(&self, session: &str, machine: &str, leases: &[Value]) -> io::Result<()> {
        let file = format!("{}.json", safe(session)?);
        let path = self.leases().join(&file);
        // A holding of this machine's goes with its session, once that is
        // carried here from `machine`.
        if let Some(held) = read::<Value>(&path)
            && held.get(MACHINE).and_then(Value::as_str) != Some(machine)
            && (held.get(MACHINE).is_some() || self.carried(session, machine)?.is_none())
        {
            let whose = held
                .get(MACHINE)
                .and_then(Value::as_str)
                .unwrap_or("this machine");
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{session:?} holds leases of {whose}'s, not {machine}'s"),
            ));
        }
        if leases.is_empty() {
            return match fs::remove_file(path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        let holding = serde_json::json!({"session": session, "leases": leases, MACHINE: machine});
        write_whole(&self.leases(), &file, &holding)
    }

    fn machines(&self) -> Vec<Machine> {
        let Ok(entries) = fs::read_dir(&*self.claims) else {
            return Vec::new();
        };
        let mut machines: Vec<Machine> = entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(SAID))
            })
            .filter_map(|entry| read(&entry.path()))
            .collect();
        machines.sort_by(|a, b| a.name.cmp(&b.name));
        machines
    }

    fn put_machine(&self, machine: &Machine) -> io::Result<()> {
        let file = format!("{}{SAID}", safe(&machine.name)?);
        let body = serde_json::to_value(machine).map_err(io::Error::other)?;
        write_whole(&self.claims, &file, &body)
    }

    fn told_carries(&self) -> BTreeMap<String, Vec<Value>> {
        let Ok(entries) = fs::read_dir(&*self.claims) else {
            return BTreeMap::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let machine = told(&entry.path())?;
                Some((machine, read(&entry.path())?))
            })
            .collect()
    }

    fn put_told_carries(&self, machine: &str, told: &[Value]) -> io::Result<()> {
        let file = format!("{}{TOLD}", safe(machine)?);
        if told.is_empty() {
            return match fs::remove_file(self.claims.join(file)) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        write_whole(&self.claims, &file, &Value::from(told.to_vec()))
    }

    fn ended(&self, pid: i32) {
        {
            let mut ended = self.ended_processes();
            // An id no process holds any more might be taken by a new one,
            // which has said nothing of ending.
            ended.retain(|&each| process_exists(each));
            ended.insert(pid);
        }
        self.tell_presence();
    }

    fn present(&self, pid: i32) {
        if self.ended_processes().remove(&pid) {
            self.tell_presence();
        }
    }

    fn changes(&self) -> io::Result<Changes> {
        let (sender, changes) = mpsc::unbounded_channel();
        self.told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(sender.clone());
        let messages = self.messages();
        let presence = self.presence();
        let leases = self.leases();
        let claims = Arc::clone(&self.claims);
        let ends = Arc::new(Ends::new(sender.clone(), Arc::clone(&self.judged.ends))?);
        let watching = {
            let every = presence.clone();
            let ends = Arc::clone(&ends);
            move |only: Option<Vec<PathBuf>>| match only {
                Some(paths) => watch_ends(&ends, paths.into_iter()),
                None => watch_ends(&ends, records(&every)),
            }
        };
        watching(None);
        let mut watcher = crate::watching::watcher(move |event: notify::Result<notify::Event>| {
            let written: Vec<PathBuf> = event.as_ref().map_or_else(
                |_| Vec::new(),
                |event| {
                    let here = |path: &&PathBuf| path.parent() == Some(presence.as_path());
                    event.paths.iter().filter(here).cloned().collect()
                },
            );
            let seen: Vec<Change> = match event {
                // A failed or overflowing watch may have missed something.
                Err(_) => vec![Change::Rescan],
                Ok(event) if event.need_rescan() => vec![Change::Rescan],
                // A file opened or read holds nothing new; taken as a
                // change, every read would bring another.
                Ok(event) if !alters(event.kind) => Vec::new(),
                Ok(event) => event
                    .paths
                    .iter()
                    .filter_map(|path| {
                        if path.parent() == Some(claims.as_path()) {
                            return said(path)
                                .map(Change::Machine)
                                .or_else(|| told(path).map(Change::Carries));
                        }
                        if !is_record(path) {
                            return None;
                        }
                        if path.parent() == Some(messages.as_path()) {
                            Some(Change::Message(path.file_stem()?.to_str()?.to_owned()))
                        } else if path.parent() == Some(presence.as_path())
                            || path.parent() == Some(leases.as_path())
                        {
                            Some(Change::Presence)
                        } else {
                            None
                        }
                    })
                    .collect(),
            };
            // Only a presence written may name a new process; a watch
            // that missed something has every one read again.
            if seen.contains(&Change::Rescan) {
                watching(None);
            } else if seen.contains(&Change::Presence) && !written.is_empty() {
                watching(Some(written));
            }
            for change in seen {
                // Nobody receives once the store's reader has gone; the
                // watcher goes with it.
                let _ = sender.send(change);
            }
        })
        .map_err(io::Error::other)?;
        watcher
            .watch(&self.messages(), RecursiveMode::NonRecursive)
            .map_err(io::Error::other)?;
        watcher
            .watch(&self.presence(), RecursiveMode::NonRecursive)
            .map_err(io::Error::other)?;
        watcher
            .watch(&self.leases(), RecursiveMode::NonRecursive)
            .map_err(io::Error::other)?;
        watcher
            .watch(&self.claims, RecursiveMode::NonRecursive)
            .map_err(io::Error::other)?;
        Ok(Changes {
            changes,
            keep: Box::new((watcher, ends)),
        })
    }
}

/// Whether a watch's event may have changed what a record holds: made,
/// written, renamed or removed, rather than opened, closed or given new
/// permissions.
const fn alters(kind: EventKind) -> bool {
    !matches!(
        kind,
        EventKind::Access(_) | EventKind::Modify(ModifyKind::Metadata(_))
    )
}

/// What a presence says of where its session runs and what holds it.
#[derive(Deserialize)]
struct Facts {
    session: String,
    owner: Option<Owner>,
    machine: Option<String>,
    #[serde(rename = "unreachableSince")]
    unreachable_since: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Owner {
    pid: i32,
    boot_id: Option<String>,
    start_ticks: Option<u64>,
}

/// Whether any process holds the id `pid`, a zombie included.
fn process_exists(pid: i32) -> bool {
    rustix::process::Pid::from_raw(pid)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}

impl Owner {
    /// Whether the process that wrote the presence still runs: the same
    /// boot, and a process of that id that started when it did.
    fn lives(&self, boot: Option<&str>) -> bool {
        if let (Some(written), Some(now)) = (self.boot_id.as_deref(), boot)
            && written != now
        {
            return false;
        }
        let Some(pid) = rustix::process::Pid::from_raw(self.pid) else {
            return false;
        };
        if rustix::process::test_kill_process(pid).is_err() {
            return false;
        }
        let now = stat(self.pid);
        // A process that has exited stays a zombie until its parent reaps
        // it, which a signal still reaches; it has ended all the same.
        if now.as_ref().is_some_and(|now| now.exited) {
            return false;
        }
        match (self.start_ticks, now) {
            (Some(written), Some(now)) => written == now.start_ticks,
            _ => true,
        }
    }
}

/// Reads `path` as `T`, or nothing when it is gone, half-written or not of
/// this kind; the mesh rewrites these files as it runs, so a file that
/// fails now is read again at its next change.
fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Writes `body` to `dir/file` whole: staged under a name no other write
/// shares, then renamed into place.
fn write_whole(dir: &Path, file: &str, body: &Value) -> io::Result<()> {
    let staged = dir.join(format!(
        ".{file}.{}.{}.mycelium.tmp",
        std::process::id(),
        STAGED.fetch_add(1, Ordering::Relaxed)
    ));
    let written = serde_json::to_vec(body)
        .map_err(io::Error::other)
        .and_then(|bytes| fs::write(&staged, bytes))
        .and_then(|()| fs::rename(&staged, dir.join(file)));
    if written.is_err() {
        // A staged file left behind is only clutter, which the mesh's own
        // sweep clears; the write's own error is the one worth telling.
        let _ = fs::remove_file(&staged);
    }
    written
}

/// `name` if it is safe as a file's name. The mesh's ids and sessions are
/// letters, digits, `-` and `_`, so anything else, a path above all, is
/// refused.
pub fn safe(name: &str) -> io::Result<&str> {
    let safe = !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'@'));
    if safe {
        Ok(name)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name:?} is not a mesh name: use letters, digits, -, _ and @"),
        ))
    }
}

/// When a message was sent, from the milliseconds its id starts with.
fn sent_at(id: &str) -> Option<i64> {
    id.split('-').next()?.parse().ok()
}

/// The UTC day `at`, in milliseconds, falls on, as a day folder is named.
fn day_of(at: i64) -> Option<String> {
    let at = jiff::Timestamp::from_millisecond(at).ok()?;
    Some(at.strftime("%Y-%m-%d").to_string())
}

/// The day folders in `dir`, by name.
fn days(dir: &Path) -> impl Iterator<Item = String> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.parse::<jiff::civil::Date>().is_ok())
}

fn records(dir: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_record(path))
}

/// Whether `path` names a record, not a read marker or a staged write.
/// The machine whose facts `path` holds, if it holds a machine's facts.
fn said(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let machine = name.strip_suffix(SAID)?;
    (!machine.is_empty() && !machine.starts_with('.')).then(|| machine.to_owned())
}

/// The machine whose node's carries `path` holds, if it holds those.
fn told(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let machine = name.strip_suffix(TOLD)?;
    (!machine.is_empty() && !machine.starts_with('.')).then(|| machine.to_owned())
}

fn is_record(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    !name.starts_with('.')
        && path
            .extension()
            .is_some_and(|extension| extension == "json")
}

/// This boot's id, where the system has one (Linux): read once, since it
/// can't change under a process, and each pass over the sessions asks it.
#[must_use]
pub fn boot_id() -> Option<String> {
    static BOOT: OnceLock<Option<String>> = OnceLock::new();
    BOOT.get_or_init(|| {
        Some(
            fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .ok()?
                .trim()
                .to_owned(),
        )
    })
    .clone()
}

/// What the system says of a process (Linux).
struct Stat {
    /// When it started, in clock ticks since boot.
    start_ticks: u64,
    /// Whether it has exited, though not yet been reaped.
    exited: bool,
}

/// What the system says of process `pid`, where it says (Linux).
fn stat(pid: i32) -> Option<Stat> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command's name, in parentheses, may hold spaces, so the fields
    // are counted from the last `)`: the state is the 3rd field, the 1st
    // after it, and the start time the 22nd, the 20th after it.
    let mut after = stat[stat.rfind(')')? + 1..].split_whitespace();
    let state = after.next()?;
    Some(Stat {
        exited: matches!(state, "Z" | "X"),
        start_ticks: after.nth(18)?.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::*;

    /// How long a test waits for the system to tell of a change.
    const PATIENCE: Duration = Duration::from_secs(5);

    /// When process `pid` started, in clock ticks since boot (Linux).
    fn start_ticks(pid: i32) -> Option<u64> {
        stat(pid).map(|stat| stat.start_ticks)
    }

    fn store() -> (tempfile::TempDir, Folders) {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let folders = Folders::open(&dir.path().join("mesh"), &dir.path().join("claims"))
            .expect("the folders open");
        (dir, folders)
    }

    fn me() -> Owner {
        let pid = std::process::id().cast_signed();
        Owner {
            pid,
            boot_id: boot_id(),
            start_ticks: start_ticks(pid),
        }
    }

    #[test]
    fn a_presence_is_read_again_only_once_its_file_changes_and_forgotten_once_gone() {
        let (dir, folders) = store();
        let presence = dir.path().join("mesh/presence");
        let write = |name: &str, body: &Value| {
            let staged = presence.join(format!(".{name}.tmp"));
            fs::write(&staged, body.to_string()).expect("written");
            fs::rename(&staged, presence.join(name)).expect("renamed into place");
        };
        let titles = |folders: &Folders| -> Vec<String> {
            let mut titles: Vec<String> = folders
                .presence_bodies()
                .iter()
                .filter_map(|body| body["title"].as_str().map(str::to_owned))
                .collect();
            titles.sort();
            titles
        };
        write("s-one.json", &json!({"session": "s-one", "title": "one"}));
        write("s-two.json", &json!({"session": "s-two", "title": "two"}));
        assert_eq!(titles(&folders), ["one", "two"], "each read");
        // Each pass over the sessions reads them all, so one unchanged is
        // the same reading shared, never copied again.
        let (first, again) = (folders.presence_bodies(), folders.presence_bodies());
        assert!(
            first
                .iter()
                .zip(&again)
                .all(|(one, two)| Arc::ptr_eq(one, two)),
            "an unchanged presence is shared, not copied"
        );
        // Written whole again, as a session rewrites its own.
        write(
            "s-one.json",
            &json!({"session": "s-one", "title": "one again"}),
        );
        assert_eq!(
            titles(&folders),
            ["one again", "two"],
            "the changed one read again"
        );
        // Written in place, its length changing.
        fs::write(
            presence.join("s-two.json"),
            json!({"session": "s-two", "title": "two, longer"}).to_string(),
        )
        .expect("written in place");
        assert_eq!(
            titles(&folders),
            ["one again", "two, longer"],
            "one written in place too"
        );
        fs::remove_file(presence.join("s-one.json")).expect("gone");
        assert_eq!(titles(&folders), ["two, longer"], "one gone, forgotten");
        assert_eq!(
            folders.kept.lock().map_or(0, |kept| kept.len()),
            1,
            "only what stands is kept"
        );
    }

    #[test]
    fn this_process_lives_and_one_from_another_boot_does_not() {
        assert!(
            me().lives(boot_id().as_deref()),
            "this test's own process should live"
        );
        let rebooted = Owner {
            boot_id: Some("another boot".to_owned()),
            ..me()
        };
        assert!(
            !rebooted.lives(Some("this boot")),
            "a presence from another boot has ended"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_process_that_started_at_another_time_is_another_process() {
        // A child started now must have started no earlier than this test,
        // whatever field is read, and its pid with this test's start time
        // names a process that has gone.
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .expect("a child starts");
        let pid = child.id().cast_signed();
        let started = start_ticks(pid).expect("the child has a start time");
        let mine = me().start_ticks.expect("this process has a start time");
        assert!(
            started >= mine,
            "the child started after this test: {started} < {mine}"
        );
        let reused = Owner {
            pid,
            boot_id: boot_id(),
            start_ticks: Some(started + 1),
        };
        let lives = reused.lives(boot_id().as_deref());
        // The child is only needed while its pid is judged.
        let _ = child.kill();
        let _ = child.wait();
        assert!(!lives, "a pid reused by a later process has ended");
    }

    /// A child that sleeps until it is killed, present in `store` as
    /// `session`.
    #[cfg(target_os = "linux")]
    fn present_sleeping(store: &Folders, session: &str) -> std::process::Child {
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("a child starts");
        let pid = child.id().cast_signed();
        let presence = json!({
            "session": session,
            "owner": {"pid": pid, "bootId": boot_id(), "startTicks": start_ticks(pid)},
        });
        write_whole(&store.presence(), &format!("{session}.json"), &presence)
            .expect("its presence written");
        child
    }

    /// Whether `changes` tells a presence once none in `store` lives,
    /// within `PATIENCE`.
    #[cfg(target_os = "linux")]
    async fn ended(
        changes: &mut Changes,
        store: &Folders,
    ) -> Result<bool, tokio::time::error::Elapsed> {
        tokio::time::timeout(PATIENCE, async {
            while let Some(change) = changes.changes.recv().await {
                if change == Change::Presence && !store.presences().iter().any(|one| one.lives) {
                    return true;
                }
            }
            false
        })
        .await
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_session_ending_is_told_the_moment_its_process_exits() {
        let (_dir, store) = store();
        // Present before the changes are asked for, so the folder tells
        // nothing of it: only the process's end can.
        let mut child = present_sleeping(&store, "ending");
        let mut changes = store.changes().expect("the store tells its changes");
        let lives = |store: &Folders| store.presences().iter().any(|one| one.lives);
        assert!(lives(&store), "the session lives while its process runs");

        // Killed and left unreaped, as a zombie, which a signal still
        // reaches, until the test is done with it.
        child.kill().expect("the child is killed");
        let told = ended(&mut changes, &store).await;
        let _ = child.wait();
        assert_eq!(
            told,
            Ok(true),
            "the session's end wasn't told within {PATIENCE:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_session_present_once_changes_are_told_is_told_when_its_process_exits() {
        let (_dir, store) = store();
        let mut changes = store.changes().expect("the store tells its changes");
        let mut child = present_sleeping(&store, "later");
        let lives = |store: &Folders| store.presences().iter().any(|one| one.lives);
        let heard = tokio::time::timeout(PATIENCE, changes.changes.recv()).await;
        assert_eq!(heard, Ok(Some(Change::Presence)), "its presence is told");
        assert!(lives(&store), "the session lives while its process runs");

        child.kill().expect("the child is killed");
        let told = ended(&mut changes, &store).await;
        let _ = child.wait();
        assert_eq!(
            told,
            Ok(true),
            "the end of a process named once the watch began wasn't told within {PATIENCE:?}"
        );
    }

    #[test]
    fn a_living_process_is_judged_once_until_an_end_may_be_told() {
        let (_dir, store) = store();
        let mine = me();
        let presence = json!({
            "session": "mine",
            "owner": {"pid": mine.pid, "bootId": mine.boot_id, "startTicks": mine.start_ticks},
        });
        write_whole(&store.presence(), "mine.json", &presence).expect("its presence written");
        let times = |store: &Folders| {
            let verdicts = store.judged.verdicts.lock().ok()?;
            verdicts.get(&mine).map(|verdict| verdict.times)
        };
        for _ in 0..3 {
            assert!(
                store.presences().iter().all(|one| one.lives),
                "the session lives while its process runs"
            );
        }
        assert_eq!(times(&store), Some(1), "judged once while no end is told");

        store.judged.ends.fetch_add(1, Ordering::Release);
        let _ = store.presences();
        assert_eq!(
            times(&store),
            Some(2),
            "judged again once an end may have been told"
        );

        fs::remove_file(store.presence().join("mine.json")).expect("its presence gone");
        let _ = store.presences();
        assert_eq!(times(&store), None, "forgotten once no presence names it");
    }

    #[tokio::test]
    async fn a_process_said_to_have_ended_ends_its_session_until_it_is_present() {
        let (_dir, store) = store();
        let mine = me();
        let presence = json!({
            "session": "mine",
            "owner": {"pid": mine.pid, "bootId": mine.boot_id, "startTicks": mine.start_ticks},
        });
        write_whole(&store.presence(), "mine.json", &presence).expect("its presence written");
        let mut changes = store.changes().expect("the store tells its changes");
        let lives = |store: &Folders| store.presences().iter().any(|one| one.lives);
        assert!(lives(&store), "the session lives while its process runs");

        store.ended(mine.pid);
        assert!(
            !lives(&store),
            "a session whose process said it ended is ended, though the process runs"
        );
        assert_eq!(
            changes.changes.try_recv(),
            Ok(Change::Presence),
            "the end is told to the store's reader at once"
        );

        store.present(mine.pid);
        assert!(
            lives(&store),
            "the session lives again once its process is present"
        );
    }

    #[test]
    fn a_message_is_kept_once_and_found_by_when_it_was_sent() {
        let (_dir, store) = store();
        let id = "1793046604647-0FX11G-875563-952d5879";
        store
            .put_message(id, &json!({"text": "first"}))
            .expect("the message is kept");
        store
            .put_message(id, &json!({"text": "again"}))
            .expect("a repeat is taken quietly");
        assert_eq!(store.message(id), Some(json!({"text": "first"})));
        assert_eq!(store.message_ids(1_793_046_604_647), [id]);
        assert!(
            store.message_ids(1_793_046_604_648).is_empty(),
            "nothing is that new"
        );
    }

    #[test]
    fn a_message_the_mesh_moved_into_its_day_is_still_found_and_kept_once() {
        let (_dir, store) = store();
        // Sent at 2026-10-26 20:30:04.647 UTC.
        let id = "1793046604647-0FX11G-875563-952d5879";
        let day = store.messages().join("2026-10-26");
        fs::create_dir(&day).expect("a day folder");
        fs::write(day.join(format!("{id}.json")), br#"{"text": "first"}"#)
            .expect("the mesh moves it");
        // Sent at 2026-10-25 12:00 UTC.
        let older = "1792929600000-0FX11G-875563-00000001";
        let earlier = store.messages().join("2026-10-25");
        fs::create_dir(&earlier).expect("an earlier day folder");
        fs::write(earlier.join(format!("{older}.json")), b"{}").expect("an older one");

        assert_eq!(
            store.message(id),
            Some(json!({"text": "first"})),
            "a moved message is read from its day"
        );
        store
            .put_message(id, &json!({"text": "again"}))
            .expect("a repeat is taken quietly");
        assert!(
            !store.messages().join(format!("{id}.json")).exists(),
            "a moved message isn't written again at the top"
        );
        assert_eq!(
            store.message_ids(1_793_046_604_647),
            [id],
            "a moved message is listed, and an older day isn't"
        );
        assert_eq!(
            store.message_ids(0),
            [older, id],
            "every day is listed from the start"
        );
    }

    #[test]
    fn an_id_that_names_a_path_is_refused() {
        let (_dir, store) = store();
        assert!(
            store.put_message("../escape", &json!({})).is_err(),
            "a path is not an id"
        );
        let escape = json!({"session": "../escape"});
        assert!(
            store.put_presence("../escape", "kestrel", &escape).is_err(),
            "nor a session"
        );
    }

    #[test]
    fn a_session_elsewhere_is_kept_without_its_process_and_taken_away_alone() {
        let (_dir, store) = store();
        let pid = std::process::id();
        let here = json!({"session": "here", "owner": {"pid": pid}});
        write_whole(&store.presence(), "here.json", &here)
            .expect("this machine's session is written");
        let there = json!({"session": "there", "owner": {"pid": 1}, "title": "π - walgit"});
        store
            .put_presence("there", "kestrel", &there)
            .expect("the session elsewhere is kept");

        let mut held = store.presences();
        held.sort_by(|a, b| a.session.cmp(&b.session));
        assert_eq!(held[0].elsewhere, None);
        assert!(
            held[0].lives,
            "this machine's session lives, being this test"
        );
        assert_eq!(held[1].elsewhere.as_deref(), Some("kestrel"));
        assert_eq!(
            *held[1].body,
            json!({"session": "there", "title": "π - walgit", "machine": "kestrel"})
        );

        store
            .remove_presence("here", "kestrel")
            .expect("asking to remove this machine's session is quiet");
        store
            .remove_presence("there", "tern")
            .expect("asking for another machine's is quiet");
        assert_eq!(
            store.presences().len(),
            2,
            "neither was another machine's to remove"
        );
        store
            .remove_presence("there", "kestrel")
            .expect("the session elsewhere is let go");
        let left: Vec<String> = store
            .presences()
            .into_iter()
            .map(|presence| presence.session)
            .collect();
        assert_eq!(left, ["here"]);
    }

    #[test]
    fn a_session_out_of_reach_keeps_when_it_was_last_vouched_for() {
        let (_dir, store) = store();
        let there = json!({"session": "there"});
        store
            .put_presence("there", "kestrel", &there)
            .expect("the session elsewhere is kept");
        let file = store.presence().join("there.json");
        let vouched = SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_046_604);
        fs::File::options()
            .append(true)
            .open(&file)
            .and_then(|file| file.set_modified(vouched))
            .expect("its time is set");

        store
            .mark_unreachable("there", "kestrel", 7)
            .expect("marked");
        store
            .mark_unreachable("there", "kestrel", 9)
            .expect("marked again");
        let held = store.presences();
        assert_eq!(
            held[0].unreachable_since,
            Some(7),
            "it is out of reach since it was first marked"
        );
        assert_eq!(
            fs::metadata(&file).and_then(|meta| meta.modified()).ok(),
            Some(vouched),
            "the file keeps when a link last said it stands"
        );

        store
            .put_presence("there", "kestrel", &there)
            .expect("carried again");
        assert_eq!(
            store.presences()[0].unreachable_since,
            None,
            "a link carrying it again has reached its machine"
        );
    }

    #[test]
    fn no_machine_writes_over_a_session_not_its_own() {
        let (_dir, store) = store();
        let here = json!({"session": "here", "owner": {"pid": std::process::id()}});
        write_whole(&store.presence(), "here.json", &here)
            .expect("this machine's session is written");
        let claimed = json!({"session": "here", "title": "not yours"});
        assert!(
            store.put_presence("here", "kestrel", &claimed).is_err(),
            "this machine's own"
        );
        let there = json!({"session": "there"});
        store
            .put_presence("there", "kestrel", &there)
            .expect("the Mac's own session is kept");
        assert!(
            store.put_presence("there", "tern", &there).is_err(),
            "another machine's"
        );
        let other = json!({"session": "other"});
        assert!(
            store.put_presence("there", "kestrel", &other).is_err(),
            "a body naming another session"
        );
        assert_eq!(store.presences().len(), 2, "nothing was written over");
    }

    #[test]
    fn a_session_carried_away_and_running_there_takes_the_place_of_its_end_here() {
        let (_dir, store) = store();
        let pid = std::process::id().cast_signed();
        let here = json!({"session": "moved", "owner": {"pid": pid}, "title": "was here"});
        write_whole(&store.presence(), "moved.json", &here)
            .expect("this machine's session is written");
        let change = json!({"kind": "change", "subject": "core.nix:flake.nix"});
        let own = json!({"session": "moved", "leases": [change]});
        write_whole(&store.leases(), "moved.json", &own).expect("its holding here");
        store.ended(pid);

        let there = json!({"session": "moved", "title": "runs there"});
        store
            .put_presence("moved", "kestrel", &there)
            .expect("its file here ended, so the machine it runs on now speaks for it");
        store
            .put_leases("moved", "kestrel", std::slice::from_ref(&change))
            .expect("and for the leases it holds");
        let held = store.presences();
        assert!(
            matches!(held.as_slice(), [one] if one.elsewhere.as_deref() == Some("kestrel")
                && one.lives && one.body["title"] == "runs there"),
            "it stands where it runs: {held:?}"
        );
        assert_eq!(
            store.holdings(),
            [json!({"session": "moved", "leases": [change], "machine": "kestrel"})],
            "its leases are the Mac's"
        );
        assert!(
            store.put_presence("moved", "tern", &there).is_err(),
            "but no third machine speaks for it"
        );

        store.present(pid);
        write_whole(&store.presence(), "moved.json", &here).expect("it runs here again");
        assert!(
            store.put_presence("moved", "kestrel", &there).is_err(),
            "and while it lives here, this machine alone speaks for it"
        );
    }

    #[test]
    fn a_sessions_leases_elsewhere_are_kept_apart_from_this_machines_and_go_with_it() {
        let (_dir, store) = store();
        let change = json!({"kind": "change", "subject": "core.nix:flake.nix",
                            "since": "s", "until": "u"});
        let own = json!({"session": "here", "leases": [change]});
        write_whole(&store.leases(), "here.json", &own).expect("this machine's holding");
        assert!(
            store
                .put_leases("here", "kestrel", std::slice::from_ref(&change))
                .is_err(),
            "a holding of this machine's is never written over"
        );
        store
            .put_presence("there", "kestrel", &json!({"session": "there"}))
            .expect("the Mac's session is kept");
        store
            .put_leases("there", "kestrel", std::slice::from_ref(&change))
            .expect("and its leases");
        assert!(
            store.put_leases("there", "tern", &[]).is_err(),
            "nor is another machine's let go"
        );
        let carried = read::<Value>(&store.leases().join("there.json")).expect("written");
        assert_eq!(
            carried,
            json!({"session": "there", "leases": [change], "machine": "kestrel"}),
            "held as the mesh holds a peer's, marked as the Mac's"
        );
        store.remove_presence("there", "kestrel").expect("let go");
        assert_eq!(
            store.holdings(),
            vec![own],
            "its leases go with it, and this machine's stay"
        );
    }

    #[test]
    fn a_claim_is_held_by_one_at_a_time() {
        let (_dir, store) = store();
        let first = store.claim("kestrel").expect("a claim can be asked for");
        assert!(first.is_some(), "the first claim is granted");
        assert!(
            store.claim("kestrel").expect("asked").is_none(),
            "a second waits"
        );
        assert!(
            store.claim("tern").expect("asked").is_some(),
            "another machine's is free"
        );
        drop(first);
        // A claim's lock belongs to its open file, and a process another
        // test forks holds a copy of every descriptor until it execs, so
        // the lock can outlast the drop for that instant. It comes free
        // once the copy closes.
        let deadline = Instant::now() + Duration::from_secs(5);
        let again = loop {
            let claim = store.claim("kestrel").expect("asked");
            if claim.is_some() || Instant::now() > deadline {
                break claim;
            }
            std::thread::yield_now();
        };
        assert!(again.is_some(), "a dropped claim is free again");
    }

    #[test]
    fn when_a_machine_was_last_linked_is_kept() {
        let (_dir, store) = store();
        assert_eq!(store.linked_until("kestrel"), None, "never linked");
        store
            .mark_linked("kestrel", 1_793_046_604_647)
            .expect("the time is kept");
        assert_eq!(store.linked_until("kestrel"), Some(1_793_046_604_647));
    }

    #[test]
    fn why_a_machines_link_was_refused_is_kept_until_one_is_admitted() {
        let (dir, store) = store();
        assert_eq!(store.link_refused("tern"), None, "never refused");
        store
            .refuse_link("tern", "tern speaks link version 2")
            .expect("the refusal is kept");
        store
            .refuse_link("tern", "tern speaks link version 3")
            .expect("a later refusal is kept over it");
        let again = Folders::open(&dir.path().join("mesh"), &dir.path().join("claims"))
            .expect("the store opens again");
        assert_eq!(
            again.link_refused("tern").as_deref(),
            Some("tern speaks link version 3"),
            "the last refusal outlives the node that kept it"
        );
        assert_eq!(
            again.link_refused("kestrel"),
            None,
            "another machine's is its own"
        );
        again
            .link_admitted("tern")
            .expect("the refusal is forgotten");
        assert_eq!(again.link_refused("tern"), None, "a link admitted ends it");
        again
            .link_admitted("tern")
            .expect("forgetting none is no error");
    }

    #[test]
    fn a_machines_link_endings_are_counted_for_the_last_day() {
        let (_dir, store) = store();
        let hour = 60 * 60 * 1000;
        let noon = 1_793_046_604_647;
        assert_eq!(store.endings("tern", 0), 0, "never ended");
        for at in [noon - 30 * hour, noon - 2 * hour, noon - hour, noon] {
            store.mark_ended("tern", at).expect("the end is kept");
        }
        store
            .mark_ended("kestrel", noon)
            .expect("another machine's is its own");
        assert_eq!(
            store.endings("tern", noon - 24 * hour),
            3,
            "three ended in the day before noon"
        );
        assert_eq!(
            store.endings("tern", 0),
            3,
            "one over a day before the last isn't kept"
        );
        assert_eq!(store.endings("kestrel", 0), 1, "each machine apart");
        assert_eq!(store.endings("../far", 0), 0, "a path is no machine");
    }

    #[tokio::test]
    async fn a_message_written_through_a_linked_path_is_told() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let real = dir.path().join("mesh");
        fs::create_dir_all(&real).expect("the folder is made");
        let through = dir.path().join("through");
        std::os::unix::fs::symlink(&real, &through).expect("the link is made");
        let store = Folders::open(&through, &dir.path().join("claims")).expect("the folders open");
        let mut changes = store.changes().expect("the store tells its changes");

        let id = "1793046604647-0FX11G-875563-952d5879";
        fs::write(real.join("messages").join(format!("{id}.json")), b"{}")
            .expect("a session writes");
        let told = tokio::time::timeout(PATIENCE, async {
            loop {
                match changes.changes.recv().await {
                    Some(Change::Message(told)) if told == id => return,
                    Some(_) => {}
                    None => panic!("the store stopped telling"),
                }
            }
        })
        .await;
        assert!(told.is_ok(), "the message was told within {PATIENCE:?}");
    }

    #[tokio::test]
    async fn a_machine_saying_itself_again_is_told_by_its_name() {
        let (dir, store) = store();
        let mut changes = store.changes().expect("the store tells its changes");
        let claims = dir.path().join("claims");
        store
            .mark_linked("kestrel", 1_793_046_604_647)
            .expect("the time is kept");
        fs::write(claims.join("kestrel.machine"), b"{}").expect("a link writes");
        let told = tokio::time::timeout(PATIENCE, async {
            loop {
                match changes.changes.recv().await {
                    Some(Change::Machine(name)) => return name,
                    Some(_) => {}
                    None => panic!("the store stopped telling"),
                }
            }
        })
        .await
        .expect("a machine was told");
        assert_eq!(
            told, "kestrel",
            "by its name, and not for when it was linked"
        );
    }

    #[tokio::test]
    async fn the_carries_a_machine_told_of_are_kept_in_place_of_the_last_until_none() {
        let (dir, store) = store();
        let mut changes = store.changes().expect("the store tells its changes");
        assert!(store.told_carries().is_empty(), "none told yet");
        let first = [json!({"session": "s-harbour", "left": false})];
        let again = [json!({"session": "s-harbour", "left": true})];
        for told in [&first[..], &again[..]] {
            store
                .put_told_carries("kestrel", told)
                .expect("the carries are kept");
        }
        fs::write(dir.path().join("claims").join("kestrel.machine"), b"[]").expect("a link writes");
        assert_eq!(
            store.told_carries(),
            [("kestrel".to_owned(), again.to_vec())].into(),
            "the last told, and not what it says of itself"
        );
        let told = tokio::time::timeout(PATIENCE, async {
            loop {
                match changes.changes.recv().await {
                    Some(Change::Carries(name)) => return name,
                    Some(_) => {}
                    None => panic!("the store stopped telling"),
                }
            }
        })
        .await
        .expect("the carries were told");
        assert_eq!(told, "kestrel", "by the machine's name");
        store
            .put_told_carries("kestrel", &[])
            .expect("the carries are let go");
        store
            .put_told_carries("tern", &[])
            .expect("none to let go is no error");
        assert!(store.told_carries().is_empty(), "none let go of them");
    }

    #[tokio::test]
    async fn reading_a_message_is_no_change() {
        let (_dir, store) = store();
        let old = "1792655652402-5GV2AH-4236-9d2030dd";
        let ready = "1793046600000-MYCTST-1-0a1b2c3d";
        let new = "1793046604647-0FX11G-875563-952d5879";
        store
            .put_message(old, &json!({"id": old}))
            .expect("the message is kept");
        let mut changes = store.changes().expect("the store tells its changes");
        let mut next_message = async || {
            tokio::time::timeout(PATIENCE, async {
                loop {
                    match changes.changes.recv().await {
                        Some(Change::Message(told)) => return told,
                        Some(_) => {}
                        None => panic!("the store stopped telling"),
                    }
                }
            })
            .await
            .expect("a message was told")
        };

        // A Mac's watch batches, so the old message's write can be told
        // after the watch begins. One written once it has begun is told
        // after it, so once that one is heard the old write is behind us.
        store
            .put_message(ready, &json!({"id": ready}))
            .expect("the message is kept");
        while next_message().await != ready {}

        let file = store.messages().join(format!("{old}.json"));
        fs::read(&file).expect("a session reads it");
        store
            .put_message(new, &json!({"id": new}))
            .expect("the message is kept");

        // The watch tells changes in order, so past the marker, which one
        // write can tell more than once, the first message it tells after
        // the read must be the one written after it.
        let mut first = next_message().await;
        while first == ready {
            first = next_message().await;
        }
        assert_eq!(first, new, "only the message written was told");
    }
}
