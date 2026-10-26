//! The harness mesh's own folders as a store: one JSON file per message in
//! `messages/` and one per session in `presence/`.
//!
//! Every file is written whole, through a dot-file renamed into place, as
//! the mesh writes its own, so no reader sees half of one. A session's file
//! stays after it ends, so whether it lives is judged from its process, as
//! the mesh judges it. The presence of a session elsewhere is written with
//! its process taken out, which the mesh takes as there, and with `machine`
//! naming where it runs.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use notify::{RecursiveMode, Watcher};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;

use super::{Change, Changes, Presence, Store};

/// The field naming the machine a session runs on, written only on the
/// presence of a session elsewhere.
const MACHINE: &str = "machine";

/// The harness mesh's folders on this machine.
#[derive(Debug, Clone)]
pub struct Folders {
    dir: Arc<PathBuf>,
}

impl Folders {
    /// The mesh kept in `dir`, its two folders made if they are missing.
    pub fn open(dir: PathBuf) -> io::Result<Self> {
        let folders = Self { dir: Arc::new(dir) };
        fs::create_dir_all(folders.messages())?;
        fs::create_dir_all(folders.presence())?;
        Ok(folders)
    }

    fn messages(&self) -> PathBuf {
        self.dir.join("messages")
    }

    fn presence(&self) -> PathBuf {
        self.dir.join("presence")
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
                    elsewhere: facts.machine,
                    lives,
                    body,
                })
            })
            .collect()
    }

    fn put_presence(&self, session: &str, machine: &str, mut body: Value) -> io::Result<()> {
        let file = format!("{}.json", safe(session)?);
        let Some(fields) = body.as_object_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a presence is an object",
            ));
        };
        // Only the session's own machine can judge its process.
        fields.remove("owner");
        fields.insert(MACHINE.to_owned(), Value::String(machine.to_owned()));
        write_whole(&self.presence(), &file, &body)
    }

    fn remove_presence(&self, session: &str) -> io::Result<()> {
        let path = self.presence().join(format!("{}.json", safe(session)?));
        match read::<Facts>(&path) {
            Some(facts) if facts.machine.is_some() => fs::remove_file(path),
            _ => Ok(()),
        }
    }

    fn changes(&self) -> io::Result<Changes> {
        let (sender, changes) = mpsc::unbounded_channel();
        let messages = self.messages();
        let presence = self.presence();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else { return };
                for path in event.paths.iter().filter(|path| is_record(path)) {
                    let change = if path.parent() == Some(messages.as_path()) {
                        path.file_stem()
                            .and_then(|id| id.to_str())
                            .map(|id| Change::Message(id.to_owned()))
                    } else if path.parent() == Some(presence.as_path()) {
                        Some(Change::Presence)
                    } else {
                        None
                    };
                    if let Some(change) = change {
                        // Nobody receives once the store's reader has gone; the
                        // watcher goes with it.
                        let _ = sender.send(change);
                    }
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
            keep: Box::new(watcher),
        })
    }
}

/// What a presence says of where its session runs and what holds it.
#[derive(Deserialize)]
struct Facts {
    session: String,
    owner: Option<Owner>,
    machine: Option<String>,
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
        match (self.start_ticks, start_ticks(self.pid)) {
            (Some(written), Some(now)) => written == now,
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

fn write_whole(dir: &Path, file: &str, body: &Value) -> io::Result<()> {
    let staged = dir.join(format!(".{file}.mycelium.tmp"));
    fs::write(&staged, serde_json::to_vec(body).map_err(io::Error::other)?)?;
    fs::rename(staged, dir.join(file))
}

/// `name` if it is safe as a file's name. The mesh's ids and sessions are
/// letters, digits, `-` and `_`, so anything else, a path above all, is
/// refused.
fn safe(name: &str) -> io::Result<&str> {
    let safe = !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if safe {
        Ok(name)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name:?} is not a mesh id"),
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

/// When process `pid` started, in clock ticks since boot (Linux).
fn start_ticks(pid: i32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command's name, in parentheses, may hold spaces, so the fields
    // are counted from the last `)`: the start time is the 22nd field, the
    // 20th after it.
    let after = &stat[stat.rfind(')')? + 1..];
    after.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn store() -> (tempfile::TempDir, Folders) {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let folders = Folders::open(dir.path().to_owned()).expect("the folders open");
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
        assert!(
            store
                .put_presence("../escape", "equilibrius", json!({}))
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
            .put_presence("there", "equilibrius", there)
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
            .remove_presence("here")
            .expect("asking to remove this machine's session is quiet");
        store
            .remove_presence("there")
            .expect("the session elsewhere is let go");
        let left: Vec<String> = store
            .presences()
            .into_iter()
            .map(|presence| presence.session)
            .collect();
        assert_eq!(left, ["here"]);
    }
}
