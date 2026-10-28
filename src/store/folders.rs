//! The harness mesh's own folders as a store: one JSON file per message in
//! `messages/` and one per session in `presence/`.
//!
//! Every file is written whole, through a dot-file renamed into place, as
//! the mesh writes its own, so no reader sees half of one. A session's file
//! stays after it ends, so whether it lives is judged from its process, as
//! the mesh judges it, and its end is told as a change the moment its
//! process exits (see `ends`). The presence of a session elsewhere is written with
//! its process taken out, which the mesh takes as there, and with `machine`
//! naming where it runs; it is never written over a session of this
//! machine's or of another machine's.
//!
//! Claims are locks on files of mycelium's own, outside the mesh, so they
//! hold across every mycelium process on the machine and go when the
//! process holding one does, however it ends.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use notify::event::ModifyKind;
use notify::{EventKind, RecursiveMode, Watcher};
use rustix::fs::FlockOperation;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;

use super::ends::Ends;
use super::{Change, Changes, Claim, Presence, Store};

/// The field naming the machine a session runs on, written only on the
/// presence of a session elsewhere.
const MACHINE: &str = "machine";

/// The field saying since when, in milliseconds since the epoch, no link
/// has reached the machine of a session elsewhere.
const UNREACHABLE: &str = "unreachableSince";

/// Numbers each staged write of this process, so no two share a file.
static STAGED: AtomicU64 = AtomicU64::new(0);

/// The harness mesh's folders on this machine.
#[derive(Debug, Clone)]
pub struct Folders {
    dir: Arc<PathBuf>,
    claims: Arc<PathBuf>,
}

impl Folders {
    /// The mesh kept in `dir`, its two folders made if they are missing,
    /// with claims kept as locks in `claims`.
    pub fn open(dir: &Path, claims: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir.join("messages"))?;
        fs::create_dir_all(dir.join("presence"))?;
        fs::create_dir_all(claims)?;
        // Changes name the real path, so the folders are kept by it: a path
        // through a link, as macOS's temporary folders are, would match none.
        Ok(Self {
            dir: Arc::new(dir.canonicalize()?),
            claims: Arc::new(claims.canonicalize()?),
        })
    }

    fn messages(&self) -> PathBuf {
        self.dir.join("messages")
    }

    fn presence(&self) -> PathBuf {
        self.dir.join("presence")
    }

    /// Watches the process of each living session of this machine's, so
    /// its end is told.
    fn watch_ends(&self, ends: &Ends) {
        let boot = boot_id();
        let presence = self.presence();
        let owners = records(&presence)
            .filter_map(|path| read::<Facts>(&path))
            .filter(|facts| facts.machine.is_none())
            .filter_map(|facts| facts.owner);
        for owner in owners {
            if owner.lives(boot.as_deref()) {
                ends.watch(owner.pid, || owner.lives(boot.as_deref()));
            }
        }
    }

    /// The presence file of `session`, if `machine` carries it here.
    fn carried(&self, session: &str, machine: &str) -> io::Result<Option<PathBuf>> {
        let path = self.presence().join(format!("{}.json", safe(session)?));
        let carried =
            read::<Facts>(&path).is_some_and(|facts| facts.machine.as_deref() == Some(machine));
        Ok(carried.then_some(path))
    }
}

impl Store for Folders {
    fn message_ids(&self, since: i64) -> Vec<String> {
        records(&self.messages())
            .filter_map(|path| Some(path.file_stem()?.to_str()?.to_owned()))
            .filter(|id| sent_at(id).is_some_and(|at| at >= since))
            .collect()
    }

    fn message(&self, id: &str) -> Option<Value> {
        read(&self.messages().join(format!("{}.json", safe(id).ok()?)))
    }

    fn put_message(&self, id: &str, body: &Value) -> io::Result<()> {
        let file = format!("{}.json", safe(id)?);
        if self.messages().join(&file).exists() {
            return Ok(());
        }
        write_whole(&self.messages(), &file, body)
    }

    fn presences(&self) -> Vec<Presence> {
        let boot = boot_id();
        records(&self.presence())
            .filter_map(|path| read::<Value>(&path))
            .filter_map(|body| {
                let facts = Facts::deserialize(&body).ok()?;
                let lives = facts.machine.is_some()
                    || facts
                        .owner
                        .as_ref()
                        .is_some_and(|owner| owner.lives(boot.as_deref()));
                Some(Presence {
                    session: facts.session,
                    unreachable_since: facts.machine.as_ref().and(facts.unreachable_since),
                    elsewhere: facts.machine,
                    lives,
                    body,
                })
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
        if let Some(held) = read::<Facts>(&self.presence().join(&file))
            && held.machine.as_deref() != Some(machine)
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
        match self.carried(session, machine)? {
            Some(path) => fs::remove_file(path),
            None => Ok(()),
        }
    }

    fn changes(&self) -> io::Result<Changes> {
        let (sender, changes) = mpsc::unbounded_channel();
        let messages = self.messages();
        let presence = self.presence();
        let ends = Arc::new(Ends::new(sender.clone())?);
        let watching = {
            let folders = self.clone();
            let ends = Arc::clone(&ends);
            move || folders.watch_ends(&ends)
        };
        watching();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
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
                        .filter(|path| is_record(path))
                        .filter_map(|path| {
                            if path.parent() == Some(messages.as_path()) {
                                Some(Change::Message(path.file_stem()?.to_str()?.to_owned()))
                            } else if path.parent() == Some(presence.as_path()) {
                                Some(Change::Presence)
                            } else {
                                None
                            }
                        })
                        .collect(),
                };
                // A presence written or read again may name a new process.
                if seen
                    .iter()
                    .any(|change| matches!(change, Change::Presence | Change::Rescan))
                {
                    watching();
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Owner {
    pid: i32,
    boot_id: Option<String>,
    start_ticks: Option<u64>,
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
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if safe {
        Ok(name)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name:?} is not a mesh name: use letters, digits, - and _"),
        ))
    }
}

/// When a message was sent, from the milliseconds its id starts with.
fn sent_at(id: &str) -> Option<i64> {
    id.split('-').next()?.parse().ok()
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

/// This boot's id, where the system has one (Linux).
fn boot_id() -> Option<String> {
    Some(
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()?
            .trim()
            .to_owned(),
    )
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

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_session_ending_is_told_the_moment_its_process_exits() {
        let (_dir, store) = store();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("a child starts");
        let pid = child.id().cast_signed();
        // Written before the changes are asked for, so the folder tells
        // nothing of it: only the process's end can.
        let presence = json!({
            "session": "ending",
            "owner": {"pid": pid, "bootId": boot_id(), "startTicks": start_ticks(pid)},
        });
        write_whole(&store.presence(), "ending.json", &presence).expect("its presence written");
        let mut changes = store.changes().expect("the store tells its changes");
        let lives = |store: &Folders| store.presences().iter().any(|one| one.lives);
        assert!(lives(&store), "the session lives while its process runs");

        // Killed and left unreaped, as a zombie, which a signal still
        // reaches, until the test is done with it.
        child.kill().expect("the child is killed");
        let told = tokio::time::timeout(PATIENCE, async {
            while let Some(change) = changes.changes.recv().await {
                if change == Change::Presence && !lives(&store) {
                    return true;
                }
            }
            false
        })
        .await;
        let _ = child.wait();
        assert_eq!(
            told,
            Ok(true),
            "the session's end wasn't told within {PATIENCE:?}"
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
    fn an_id_that_names_a_path_is_refused() {
        let (_dir, store) = store();
        assert!(
            store.put_message("../escape", &json!({})).is_err(),
            "a path is not an id"
        );
        let escape = json!({"session": "../escape"});
        assert!(
            store
                .put_presence("../escape", "equilibrius", &escape)
                .is_err(),
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
            .put_presence("there", "equilibrius", &there)
            .expect("the session elsewhere is kept");

        let mut held = store.presences();
        held.sort_by(|a, b| a.session.cmp(&b.session));
        assert_eq!(held[0].elsewhere, None);
        assert!(
            held[0].lives,
            "this machine's session lives, being this test"
        );
        assert_eq!(held[1].elsewhere.as_deref(), Some("equilibrius"));
        assert_eq!(
            held[1].body,
            json!({"session": "there", "title": "π - walgit", "machine": "equilibrius"})
        );

        store
            .remove_presence("here", "equilibrius")
            .expect("asking to remove this machine's session is quiet");
        store
            .remove_presence("there", "penelope")
            .expect("asking for another machine's is quiet");
        assert_eq!(
            store.presences().len(),
            2,
            "neither was another machine's to remove"
        );
        store
            .remove_presence("there", "equilibrius")
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
            .put_presence("there", "equilibrius", &there)
            .expect("the session elsewhere is kept");
        let file = store.presence().join("there.json");
        let vouched = SystemTime::UNIX_EPOCH + Duration::from_secs(1_793_046_604);
        fs::File::options()
            .append(true)
            .open(&file)
            .and_then(|file| file.set_modified(vouched))
            .expect("its time is set");

        store
            .mark_unreachable("there", "equilibrius", 7)
            .expect("marked");
        store
            .mark_unreachable("there", "equilibrius", 9)
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
            .put_presence("there", "equilibrius", &there)
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
            store.put_presence("here", "equilibrius", &claimed).is_err(),
            "this machine's own"
        );
        let there = json!({"session": "there"});
        store
            .put_presence("there", "equilibrius", &there)
            .expect("the Mac's own session is kept");
        assert!(
            store.put_presence("there", "penelope", &there).is_err(),
            "another machine's"
        );
        let other = json!({"session": "other"});
        assert!(
            store.put_presence("there", "equilibrius", &other).is_err(),
            "a body naming another session"
        );
        assert_eq!(store.presences().len(), 2, "nothing was written over");
    }

    #[test]
    fn a_claim_is_held_by_one_at_a_time() {
        let (_dir, store) = store();
        let first = store
            .claim("equilibrius")
            .expect("a claim can be asked for");
        assert!(first.is_some(), "the first claim is granted");
        assert!(
            store.claim("equilibrius").expect("asked").is_none(),
            "a second waits"
        );
        assert!(
            store.claim("penelope").expect("asked").is_some(),
            "another machine's is free"
        );
        drop(first);
        assert!(
            store.claim("equilibrius").expect("asked").is_some(),
            "a dropped claim is free again"
        );
    }

    #[test]
    fn when_a_machine_was_last_linked_is_kept() {
        let (_dir, store) = store();
        assert_eq!(store.linked_until("equilibrius"), None, "never linked");
        store
            .mark_linked("equilibrius", 1_793_046_604_647)
            .expect("the time is kept");
        assert_eq!(store.linked_until("equilibrius"), Some(1_793_046_604_647));
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
