//! Putting to sleep the sessions a node's easing chose, by asking their
//! harness to park them. Parking is the harness's own: a session parks
//! only once it is idle and its own rule holds (nothing running or
//! queued, no open prompt, no window it is seen in), its process ends,
//! and it wakes on its next message, steer, key or window. The node only
//! leaves a file named for each session in a folder the harness reads,
//! asking it to park that one sooner than it would, and takes the file
//! back once its machine is no longer over. The harness takes it back
//! too, once the session wakes or closes.
//!
//! A session parks only where a holder or a host keeps it, since that is
//! what wakes it, so the node asks for none that neither keeps.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, DirBuilder};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rustix::fs::{Mode, OFlags};
use serde_json::{Value, json};

use crate::record::{Session, State};

/// The sessions to ask to park of those a look chose to sleep, `chosen`
/// by their record's id, among the realm's `sessions`, each as its
/// record's id and its own id as its harness names it, or why it isn't
/// asked: one gone from `here`, in a turn, kept by neither a holder nor
/// a host, or whose presence names no own id or one another session's
/// names too, since a session writes its presence itself.
#[must_use]
pub fn asks(
    here: &str,
    sessions: &[Session],
    chosen: &[String],
) -> Vec<Result<(String, String), String>> {
    chosen
        .iter()
        .map(|id| {
            let session = sessions
                .iter()
                .find(|session| session.id == *id && session.machine == here)
                .ok_or_else(|| format!("{id} no longer runs here, so it isn't put to sleep"))?;
            let who = &session.address;
            match session.state {
                State::Idle => {}
                State::Busy => return Err(format!("{who} is in a turn, so it isn't put to sleep")),
                _ => return Err(format!("{who} isn't running, so it isn't put to sleep")),
            }
            if session.held.is_none() && session.host.is_none() {
                return Err(format!(
                    "{who} is kept by no holder or host, which is what would wake it, so it \
                     isn't put to sleep"
                ));
            }
            let Some(own) = super::session_id(session) else {
                return Err(format!(
                    "{who}'s presence names no id of its own, so it isn't put to sleep"
                ));
            };
            if !named_safely(own) {
                return Err(format!(
                    "{who}'s id {own:?} can't name a file, so it isn't put to sleep"
                ));
            }
            let claiming = sessions
                .iter()
                .filter(|other| super::session_id(other) == Some(own))
                .count();
            if claiming > 1 {
                return Err(format!(
                    "{who}'s id {own:?} is named by {claiming} sessions, so it isn't put to \
                     sleep"
                ));
            }
            Ok((session.id.clone(), own.to_owned()))
        })
        .collect()
}

/// Whether `id` names a file in the folder and nothing beyond it.
fn named_safely(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Asks the harness to park the session `id`, in `folder`, the folder and
/// file each its owner's alone: the file says `at` and `why` for whoever
/// reads it, and is written whole before it appears under its name, so
/// the harness never reads half of one.
///
/// # Errors
///
/// Where the folder can't be made or the file written.
pub fn ask(folder: &Path, id: &str, at: Timestamp, why: &str) -> io::Result<()> {
    if !named_safely(id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{id:?} can't name a file"),
        ));
    }
    own_folder(folder)?;
    let writing = folder.join(format!(".{id}.{}", std::process::id()));
    let mut text = serde_json::to_string(&json!({"at": at.to_string(), "why": why}))?;
    text.push('\n');
    // One an earlier node of the same number left is this node's own.
    match fs::remove_file(&writing) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let mut file = fs::File::from(rustix::fs::open(
        &writing,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?);
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    fs::rename(&writing, folder.join(id))
}

/// Makes `folder`, this account's alone, where it isn't; one already
/// there is left as it is, so long as it is a folder of this account's
/// that no other can write in.
fn own_folder(folder: &Path) -> io::Result<()> {
    match fs::symlink_metadata(folder) {
        Ok(found) => {
            let own = rustix::process::geteuid().as_raw();
            if !found.is_dir() || found.uid() != own || found.mode() & 0o022 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the folder isn't one this account alone writes in",
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            DirBuilder::new().recursive(true).mode(0o700).create(folder)
        }
        Err(error) => Err(error),
    }
}

/// Whether `name`, in the folder, is one this node writes: an ask, or
/// one being written.
fn written_here(name: &str) -> bool {
    if named_safely(name) {
        return true;
    }
    name.strip_prefix('.')
        .and_then(|rest| rest.rsplit_once('.'))
        .is_some_and(|(id, pid)| {
            named_safely(id) && !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// Takes back each ask in `folder` named in `ids`, answering those still
/// asked for, which couldn't be taken back. One the harness already took
/// back, as its session woke, is gone already.
pub fn withdraw(folder: &Path, ids: &BTreeSet<String>) -> BTreeSet<String> {
    ids.iter()
        .filter(|id| match fs::remove_file(folder.join(id)) {
            Ok(()) => false,
            Err(error) => error.kind() != io::ErrorKind::NotFound,
        })
        .cloned()
        .collect()
}

/// Takes back every ask in `folder`, as a node starting does, since
/// nothing an earlier node asked is known to hold now: only the files
/// a node writes there, asks and those being written, never another
/// the harness or anyone keeps. Answers how many it took back, and
/// the asks it couldn't, to take back later.
///
/// # Errors
///
/// Where the folder is there but can't be read.
pub fn withdraw_all(folder: &Path) -> io::Result<(usize, BTreeSet<String>)> {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((0, BTreeSet::new()));
        }
        Err(error) => return Err(error),
    };
    let mut taken = 0;
    let mut left = BTreeSet::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !written_here(&name) || !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => taken += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {
                left.insert(name);
            }
        }
    }
    Ok((taken, left))
}

/// The asks to park a node has standing in its folder, by each session's
/// record id to its own, and the names an earlier node left there that
/// couldn't be taken back. Dropped, as when the task holding it ends or
/// is stopped, it takes them all back where it is, so no ask outlives
/// the node that made it: a few files, removed on the dropping thread.
#[derive(Debug)]
pub struct Parking {
    folder: PathBuf,
    asked: BTreeMap<String, String>,
    left: BTreeSet<String>,
}

impl Parking {
    /// None yet asked in `folder`, but the names `left` there, which an
    /// earlier node's asks are, as [`withdraw_all`] answers them.
    #[must_use]
    pub fn new(folder: PathBuf, left: BTreeSet<String>) -> Self {
        Self {
            folder,
            asked: BTreeMap::new(),
            left,
        }
    }

    /// The folder its asks are in.
    #[must_use]
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Whether it holds nothing to take back.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.asked.is_empty() && self.left.is_empty()
    }

    /// Notes the session `record`, its own id `own`, asked to park.
    pub fn asked(&mut self, record: String, own: String) {
        self.asked.insert(record, own);
    }

    /// Takes back every ask it holds, keeping those that couldn't be, to
    /// take back next, and answering by their record's id the sessions
    /// whose asks are gone.
    pub fn withdraw(&mut self) -> BTreeSet<String> {
        let names: BTreeSet<String> = self.asked.values().chain(&self.left).cloned().collect();
        let still = withdraw(&self.folder, &names);
        self.left.retain(|name| still.contains(name));
        let mut woken = BTreeSet::new();
        self.asked.retain(|record, own| {
            let kept = still.contains(own);
            if !kept {
                woken.insert(record.clone());
            }
            kept
        });
        woken
    }
}

impl Drop for Parking {
    fn drop(&mut self) {
        if !self.is_empty() {
            self.withdraw();
        }
    }
}

/// The sessions a sleep `line` chose, by their record's id, where it chose
/// to sleep.
#[must_use]
pub fn chosen(line: &Value) -> Option<Vec<String>> {
    if line["choice"] != "sleep" {
        return None;
    }
    line["sessions"]
        .as_array()?
        .iter()
        .map(|id| id.as_str().map(str::to_owned))
        .collect()
}

/// Says on `line` how asking went: `would` false, as the node acted, and
/// beside it each session `asked`, by its record's id to its own, and
/// why each other was `refused`.
pub fn acted(line: &mut Value, asked: &BTreeMap<String, String>, refused: &[String]) {
    line["would"] = json!(false);
    line["asked"] = json!(asked);
    if !refused.is_empty() {
        line["refused"] = json!(refused);
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn session(id: &str, state: &str, more: &Value) -> Session {
        let mut said = json!({
            "id": id, "address": id.to_uppercase(), "machine": "skiff",
            "groups": [], "state": state, "at": "2026-11-22T07:00:00Z",
            "presence": {"session": format!("{id}-own")},
        });
        if let (Value::Object(said), Value::Object(more)) = (&mut said, more) {
            said.extend(more.clone());
        }
        serde_json::from_value(said).expect("a session")
    }

    #[test]
    fn only_an_idle_session_a_holder_or_host_keeps_here_is_asked_to_park() {
        let mut elsewhere = session("s6", "idle", &json!({"held": "h6"}));
        elsewhere.machine = "tern".to_owned();
        let sessions = [
            session("s1", "idle", &json!({"held": "h1"})),
            session("s2", "idle", &json!({"host": "work-background"})),
            session("s3", "busy", &json!({"held": "h3"})),
            session("s4", "idle", &json!({})),
            session("s5", "ended", &json!({"held": "h5"})),
            elsewhere,
            session(
                "s7",
                "idle",
                &json!({"held": "h7", "presence": {"session": "../x"}}),
            ),
            session("s9", "idle", &json!({"held": "h9", "presence": {}})),
            session(
                "s10",
                "idle",
                &json!({"held": "h10", "presence": {"session": "s1-own"}}),
            ),
        ];
        let chosen: Vec<String> = ["s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let others = &sessions[..sessions.len() - 1];
        assert_eq!(
            asks("skiff", others, &chosen[..2]),
            [
                Ok(("s1".to_owned(), "s1-own".to_owned())),
                Ok(("s2".to_owned(), "s2-own".to_owned())),
            ],
            "each asked by its record's id and its own"
        );
        let twice = "S1's id \"s1-own\" is named by 2 sessions, so it isn't put to sleep";
        assert_eq!(
            asks("skiff", &sessions, &chosen),
            [
                Err(twice.to_owned()),
                Ok(("s2".to_owned(), "s2-own".to_owned())),
                Err("S3 is in a turn, so it isn't put to sleep".to_owned()),
                Err(
                    "S4 is kept by no holder or host, which is what would wake it, so it isn't \
                     put to sleep"
                        .to_owned()
                ),
                Err("S5 isn't running, so it isn't put to sleep".to_owned()),
                Err("s6 no longer runs here, so it isn't put to sleep".to_owned()),
                Err("S7's id \"../x\" can't name a file, so it isn't put to sleep".to_owned()),
                Err("s8 no longer runs here, so it isn't put to sleep".to_owned()),
                Err("S9's presence names no id of its own, so it isn't put to sleep".to_owned()),
                Err(
                    "S10's id \"s1-own\" is named by 2 sessions, so it isn't put to sleep"
                        .to_owned()
                ),
            ],
            "a session can't have another parked by naming its id"
        );
    }

    #[test]
    fn a_folder_already_there_is_left_as_it_is_unless_another_can_write_in_it() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let at: Timestamp = "2026-11-22T07:00:00Z".parse().expect("a time");
        let mode = |path: &Path| fs::metadata(path).expect("its metadata").mode() & 0o777;
        let kept = dir.path().join("kept");
        fs::create_dir(&kept).expect("the harness's folder");
        fs::set_permissions(&kept, fs::Permissions::from_mode(0o750)).expect("its mode");
        ask(&kept, "s1-own", at, "over").expect("asked");
        assert_eq!(mode(&kept), 0o750, "its mode is the harness's");
        let open = dir.path().join("open");
        fs::create_dir(&open).expect("a folder any can write in");
        fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).expect("its mode");
        assert!(
            ask(&open, "s1-own", at, "over").is_err(),
            "a folder another can write in is asked nothing"
        );
        // One a node of the same number left half written is written over,
        // never followed where it points.
        let pointed = dir.path().join("pointed");
        fs::write(&pointed, "untouched").expect("a file");
        std::os::unix::fs::symlink(
            &pointed,
            kept.join(format!(".s2-own.{}", std::process::id())),
        )
        .expect("a link");
        ask(&kept, "s2-own", at, "over").expect("asked");
        assert_eq!(fs::read_to_string(&pointed).expect("the file"), "untouched");
    }

    #[test]
    fn an_ask_appears_whole_and_its_owners_alone_and_is_taken_back() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let folder = dir.path().join("ease");
        let at: Timestamp = "2026-11-22T07:00:00Z".parse().expect("a time");
        ask(&folder, "s1-own", at, "skiff is over its memory").expect("asked");
        ask(&folder, "s2-own", at, "skiff is over its memory").expect("asked");
        let said: Value =
            serde_json::from_str(&fs::read_to_string(folder.join("s1-own")).expect("the ask"))
                .expect("a line");
        assert_eq!(
            said,
            json!({"at": "2026-11-22T07:00:00Z", "why": "skiff is over its memory"})
        );
        let names: BTreeSet<String> = fs::read_dir(&folder)
            .expect("the folder")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .into_string()
                    .expect("a name")
            })
            .collect();
        assert_eq!(
            names,
            BTreeSet::from(["s1-own".to_owned(), "s2-own".to_owned()]),
            "nothing half written is left beside them"
        );
        let mode = |path: &Path| fs::metadata(path).expect("its metadata").mode() & 0o777;
        assert_eq!(
            (mode(&folder), mode(&folder.join("s1-own"))),
            (0o700, 0o600)
        );
        assert!(
            ask(&folder, "../s1", at, "over").is_err(),
            "an id that would name a file beyond the folder is refused"
        );

        fs::remove_file(folder.join("s2-own")).expect("its harness took it back");
        let ids = BTreeSet::from(["s1-own".to_owned(), "s2-own".to_owned()]);
        assert!(
            withdraw(&folder, &ids).is_empty(),
            "one taken back already is gone"
        );
        assert!(!folder.join("s1-own").exists());

        ask(&folder, "s3-own", at, "over").expect("asked");
        fs::write(folder.join(".s4-own.12"), "").expect("one left half written");
        for theirs in ["README.md", ".keep", "s5.json"] {
            fs::write(folder.join(theirs), "").expect("one of another's");
        }
        assert_eq!(
            withdraw_all(&folder).expect("taken back"),
            (2, BTreeSet::new())
        );
        for theirs in ["README.md", ".keep", "s5.json"] {
            assert!(
                folder.join(theirs).exists(),
                "{theirs}, no name a node writes, is left"
            );
        }
        assert_eq!(
            withdraw_all(&dir.path().join("none")).expect("no folder"),
            (0, BTreeSet::new()),
            "no folder holds no ask"
        );
    }

    #[test]
    fn what_a_node_asked_is_taken_back_once_it_lets_go_of_it() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let folder = dir.path().join("ease");
        let at: Timestamp = "2026-11-22T07:00:00Z".parse().expect("a time");
        for own in ["s1-own", "s2-own", "old-own"] {
            ask(&folder, own, at, "over").expect("asked");
        }
        let mut parking = Parking::new(folder.clone(), BTreeSet::from(["old-own".to_owned()]));
        parking.asked("s1".to_owned(), "s1-own".to_owned());
        assert_eq!(parking.withdraw(), BTreeSet::from(["s1".to_owned()]));
        assert!(parking.is_empty(), "what an earlier node left goes too");
        assert!(!folder.join("old-own").exists());
        parking.asked("s2".to_owned(), "s2-own".to_owned());
        drop(parking);
        assert!(
            !folder.join("s2-own").exists(),
            "an ask outlives no node that let go of it"
        );
    }

    #[test]
    fn a_sleep_line_says_what_it_chose_and_how_asking_went() {
        let mut line = json!({"choice": "sleep", "would": true, "sessions": ["s1", "s2"]});
        assert_eq!(chosen(&line), Some(vec!["s1".to_owned(), "s2".to_owned()]));
        assert_eq!(chosen(&json!({"choice": "carry", "session": "s1"})), None);
        acted(
            &mut line,
            &BTreeMap::from([("s1".to_owned(), "s1-own".to_owned())]),
            &["S2 is in a turn".to_owned()],
        );
        assert_eq!(
            line,
            json!({
                "choice": "sleep", "would": false, "sessions": ["s1", "s2"],
                "asked": {"s1": "s1-own"}, "refused": ["S2 is in a turn"],
            })
        );
    }
}
