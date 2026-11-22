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

use std::collections::BTreeSet;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use jiff::Timestamp;
use serde_json::{Value, json};

use super::easing;
use crate::record::{Session, State};

/// The sessions to ask to park of those a look chose to sleep, `chosen`
/// by their record's id, among the realm's `sessions`, each by its own
/// id as its harness names it, or why it isn't asked: one gone from
/// `here`, in a turn, or kept by neither a holder nor a host.
#[must_use]
pub fn asks(here: &str, sessions: &[Session], chosen: &[String]) -> Vec<Result<String, String>> {
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
            let own = easing::id(session);
            if !named_safely(own) {
                return Err(format!(
                    "{who}'s id {own:?} can't name a file, so it isn't put to sleep"
                ));
            }
            Ok(own.to_owned())
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
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(folder)?;
    fs::set_permissions(folder, fs::Permissions::from_mode(0o700))?;
    let writing = folder.join(format!(".{id}.{}", std::process::id()));
    let mut text = serde_json::to_string(&json!({"at": at.to_string(), "why": why}))?;
    text.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&writing)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    fs::rename(&writing, folder.join(id))
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

/// Takes back every ask in `folder`, as a node starting does, since the
/// folder is its own and nothing it asked before is known to hold now.
/// Answers how many it took back.
///
/// # Errors
///
/// Where the folder is there but can't be read.
pub fn withdraw_all(folder: &Path) -> io::Result<usize> {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut taken = 0;
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            fs::remove_file(entry.path())?;
            taken += 1;
        }
    }
    Ok(taken)
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
/// beside it the ids `asked` and why each other was `refused`.
pub fn acted(line: &mut Value, asked: &[String], refused: &[String]) {
    line["would"] = json!(false);
    line["asked"] = json!(asked);
    if !refused.is_empty() {
        line["refused"] = json!(refused);
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

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
            session("s7", "idle", &json!({"held": "h7", "presence": {"session": "../x"}})),
        ];
        let chosen: Vec<String> = ["s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            asks("skiff", &sessions, &chosen),
            [
                Ok("s1-own".to_owned()),
                Ok("s2-own".to_owned()),
                Err("S3 is in a turn, so it isn't put to sleep".to_owned()),
                Err("S4 is kept by no holder or host, which is what would wake it, so it isn't \
                     put to sleep"
                    .to_owned()),
                Err("S5 isn't running, so it isn't put to sleep".to_owned()),
                Err("s6 no longer runs here, so it isn't put to sleep".to_owned()),
                Err("S7's id \"../x\" can't name a file, so it isn't put to sleep".to_owned()),
                Err("s8 no longer runs here, so it isn't put to sleep".to_owned()),
            ]
        );
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
            .map(|entry| entry.expect("an entry").file_name().into_string().expect("a name"))
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
        assert!(withdraw(&folder, &ids).is_empty(), "one taken back already is gone");
        assert!(!folder.join("s1-own").exists());

        ask(&folder, "s3-own", at, "over").expect("asked");
        assert_eq!(withdraw_all(&folder).expect("taken back"), 1);
        assert_eq!(
            withdraw_all(&dir.path().join("none")).expect("no folder"),
            0,
            "no folder holds no ask"
        );
    }

    #[test]
    fn a_sleep_line_says_what_it_chose_and_how_asking_went() {
        let mut line = json!({"choice": "sleep", "would": true, "sessions": ["s1", "s2"]});
        assert_eq!(chosen(&line), Some(vec!["s1".to_owned(), "s2".to_owned()]));
        assert_eq!(chosen(&json!({"choice": "carry", "session": "s1"})), None);
        acted(
            &mut line,
            &["s1-own".to_owned()],
            &["S2 is in a turn".to_owned()],
        );
        assert_eq!(
            line,
            json!({
                "choice": "sleep", "would": false, "sessions": ["s1", "s2"],
                "asked": ["s1-own"], "refused": ["S2 is in a turn"],
            })
        );
    }
}
