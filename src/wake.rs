//! Waking a parked session for a message carried in from another machine.
//!
//! A parked session's process is gone, and whoever holds it starts it again
//! once something writes the wake file its marker names. On its own machine
//! the sender writes it, judging whether the message would give the peer a
//! turn (the harness's `wakeParked`). A message carried in from another
//! machine has no sender here, so it rested until something local started:
//! a question from another machine sat ten minutes in a parked peer's
//! folder on 23 Nov 2026. So a link, having written such a message, wakes each parked
//! session it gives a turn to, as its sender would have.
//!
//! A turn is given, as the harness judges it without its classifier, by a
//! pivot that reaches the session, a question or hold sent to its own
//! address, a question to a named group it is in, or an answer to a message
//! it sent. Only this account's own markers are read, and only a wake file
//! in a folder of this account's that no other may write is written: where
//! the markers are another account's, as on a worker machine whose
//! sessions run as their own account, a marker could name any file of this
//! one's to be written over.
//!
//! A worker machine's sessions run as an account of their own, so a
//! message carried from one wakes less ([`worker_gives_turn`]): only a
//! question or hold to a session's own address, or an answer to one it
//! sent, never a pivot nor a group's question, so one session there can't
//! start a turn in every session of a group here; and a worker's link
//! wakes at most [`WORKER_WAKES`] a minute, the rest reading theirs at
//! their next start.

use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

use crate::store::Presence;
use crate::store::folder::Folder;

/// The group every peer hears.
const ALL: &str = "#all";

/// The most sessions a worker machine's link wakes in a minute.
pub const WORKER_WAKES: usize = 6;

/// A parked session, as its marker says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parked {
    /// The session's id.
    pub session: String,
    /// The addresses it answers to.
    pub addresses: Vec<String>,
    /// The file that wakes it, once written.
    pub wake: PathBuf,
}

/// Every session parked on this machine as the markers in `park`'s
/// `parked` folder say, each written since `booted` (milliseconds since
/// the epoch) where that is known, since a marker from before the machine
/// started names a holder that is gone. Only a marker this account wrote,
/// in a folder no other account may write, is read.
#[must_use]
pub fn parked(park: &Path, booted: Option<i64>) -> Vec<Parked> {
    let Ok(folder) = Folder::open(park, &["parked"]) else {
        return Vec::new();
    };
    if !folder.own() {
        return Vec::new();
    }
    let names = folder.names().unwrap_or_default();
    names
        .into_iter()
        // The harness names markers in lower case, so a name ending
        // otherwise is no marker.
        .filter(|(name, _)| name.strip_suffix(".json").is_some() && !name.starts_with('.'))
        .filter_map(|(name, _)| marker(&folder, &name, booted))
        .collect()
}

/// The marker `name` in `folder`, if it is this account's, written since
/// `booted`, and says what a marker says.
fn marker(folder: &Folder, name: &str, booted: Option<i64>) -> Option<Parked> {
    use std::os::unix::fs::MetadataExt;
    let file = folder.file(name).ok()??;
    let found = file.metadata().ok()?;
    if found.uid() != rustix::process::geteuid().as_raw() {
        return None;
    }
    let written = found
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_millis()).ok())?;
    if booted.is_some_and(|booted| written < booted) {
        return None;
    }
    let said: Value = serde_json::from_slice(&folder.read(name).ok()??).ok()?;
    let session = said.get("session")?.as_str()?.to_owned();
    let addresses = said
        .get("addresses")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let wake = PathBuf::from(said.get("wake")?.as_str()?);
    (wake.is_absolute() && name.strip_suffix(".json") == Some(session.as_str())).then_some(Parked {
        session,
        addresses,
        wake,
    })
}

/// Whether `message` would give `parked`, a member of `groups`, a turn were
/// it there: a pivot reaching it, a question or hold to its own address, a
/// question to a named group it is in, or an answer to one it sent, as
/// `earlier` says of the message answered. Never one it sent itself.
#[must_use]
pub fn gives_turn(
    message: &Value,
    parked: &Parked,
    groups: &[String],
    earlier: impl Fn(&str) -> Option<Value>,
) -> bool {
    let its = |address: &str| {
        parked
            .addresses
            .iter()
            .any(|own| own.eq_ignore_ascii_case(address))
    };
    if message.get("from").and_then(Value::as_str).is_some_and(its) {
        return false;
    }
    let to: Vec<&str> = message
        .get("to")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let home = |group: &str| group.strip_prefix('@').is_some_and(its);
    let member = |group: &str| {
        groups
            .iter()
            .any(|own| own == group || own.starts_with(&format!("{group}/")))
    };
    let reaches = to
        .iter()
        .any(|group| *group == ALL || home(group) || member(group));
    if !reaches {
        return false;
    }
    let answers = message
        .get("re")
        .and_then(Value::as_str)
        .and_then(earlier)
        .and_then(|answered| answered.get("from")?.as_str().map(its));
    match message.get("intent").and_then(Value::as_str) {
        Some("pivot") => true,
        Some("hold") if to.iter().any(|group| home(group)) => true,
        Some("question")
            if to.iter().any(|group| {
                home(group) || (group.starts_with('#') && *group != ALL && member(group))
            }) =>
        {
            true
        }
        _ => answers == Some(true),
    }
}

/// Whether `message`, carried from a worker machine, gives `parked` a turn:
/// as [`gives_turn`] judges, and only as a question or hold to its own
/// address or an answer to one it sent, never a pivot nor a question to a
/// group.
#[must_use]
pub fn worker_gives_turn(
    message: &Value,
    parked: &Parked,
    groups: &[String],
    earlier: impl Fn(&str) -> Option<Value>,
) -> bool {
    let its = |address: &str| {
        parked
            .addresses
            .iter()
            .any(|own| own.eq_ignore_ascii_case(address))
    };
    let to_it = message
        .get("to")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|group| group.strip_prefix('@').is_some_and(its));
    let asks = matches!(
        message.get("intent").and_then(Value::as_str),
        Some("question" | "hold")
    ) && to_it;
    let answers = || {
        message
            .get("re")
            .and_then(Value::as_str)
            .and_then(&earlier)
            .and_then(|answered| answered.get("from")?.as_str().map(its))
            == Some(true)
    };
    (asks || answers()) && gives_turn(message, parked, groups, &earlier)
}

/// Wakes `parked` for the message `id`, writing its wake file whole as the
/// harness does, `at` milliseconds since the epoch. Refused where the
/// file's folder isn't this account's alone.
pub fn wake(parked: &Parked, id: &str, at: i64) -> io::Result<()> {
    let (Some(folder), Some(name)) = (
        parked.wake.parent(),
        parked.wake.file_name().and_then(|name| name.to_str()),
    ) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a wake file names no folder",
        ));
    };
    let folder = Folder::open(folder, &[])?;
    if !folder.own() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "a wake file's folder another account may write",
        ));
    }
    let at = jiff::Timestamp::from_millisecond(at).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
    let said = json!({"session": parked.session, "messageId": id, "at": at.to_string()});
    folder.write_whole(name, format!("{said}\n").as_bytes())
}

/// Wakes every session parked under `park` since `booted` that `message`,
/// `id`, gives a turn ([`gives_turn`]), each one's groups read from its
/// presence among `presences`; answers the sessions woken and how many it
/// left. From a worker machine, `worker` says how many more it may wake,
/// and only those [`worker_gives_turn`] says are woken. One that isn't
/// woken reads the message at its next start all the same.
pub fn wake_for(
    park: &Path,
    booted: Option<i64>,
    id: &str,
    message: &Value,
    presences: &[Presence],
    earlier: impl Fn(&str) -> Option<Value>,
    worker: Option<usize>,
) -> (Vec<String>, usize) {
    // Most messages are news, which gives no turn, so no marker is read.
    let intent = message.get("intent").and_then(Value::as_str);
    if !matches!(intent, Some("pivot" | "question" | "hold")) && message.get("re").is_none() {
        return (Vec::new(), 0);
    }
    let (mut woken, mut left) = (Vec::new(), 0);
    for one in parked(park, booted) {
        let groups: Vec<String> = presences
            .iter()
            .find(|presence| presence.elsewhere.is_none() && presence.session == one.session)
            .and_then(|presence| presence.body.get("groups")?.as_array().cloned())
            .into_iter()
            .flatten()
            .filter_map(|group| group.as_str().map(str::to_owned))
            .collect();
        let given = match worker {
            None => gives_turn(message, &one, &groups, &earlier),
            Some(_) => worker_gives_turn(message, &one, &groups, &earlier),
        };
        if !given {
            continue;
        }
        if worker.is_some_and(|most| woken.len() >= most) {
            left += 1;
            continue;
        }
        match wake(&one, id, jiff::Timestamp::now().as_millisecond()) {
            Ok(()) => woken.push(one.session),
            Err(error) => eprintln!("couldn't wake {} for {id}: {error}", one.session),
        }
    }
    (woken, left)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A parked session `session`, answering to `address`, waking by a
    /// file in `wakes`, as the harness parks it under `park`.
    fn park_one(park: &Path, wakes: &Path, session: &str, address: &str) -> PathBuf {
        fs::create_dir_all(park.join("parked")).expect("the parked folder");
        fs::create_dir_all(wakes).expect("the wakes' folder");
        let wake = wakes.join(format!("{session}.wake"));
        fs::write(
            park.join("parked").join(format!("{session}.json")),
            json!({"session": session, "addresses": [address], "wake": wake}).to_string(),
        )
        .expect("a marker");
        wake
    }

    fn turn(message: &Value, groups: &[&str], earlier: Option<&Value>) -> bool {
        let parked = Parked {
            session: "s1".to_owned(),
            addresses: vec!["SAK9FZ".to_owned()],
            wake: PathBuf::from("/nowhere/s1.wake"),
        };
        let groups: Vec<String> = groups.iter().map(|group| (*group).to_owned()).collect();
        gives_turn(message, &parked, &groups, |_| earlier.cloned())
    }

    #[test]
    fn a_question_hold_or_pivot_to_it_or_an_answer_to_it_gives_a_turn() {
        let to_it = |intent: &str| json!({"from": "WCJQSG", "to": ["@SAK9FZ"], "intent": intent});
        assert!(
            turn(&to_it("question"), &[], None),
            "a question to its address"
        );
        assert!(turn(&to_it("hold"), &[], None), "a hold to its address");
        assert!(turn(&to_it("pivot"), &[], None), "a pivot to its address");
        assert!(
            turn(
                &json!({"from": "WCJQSG", "to": ["#quest/wcjqsg"], "intent": "pivot"}),
                &["#quest/wcjqsg/kp0l6a"],
                None
            ),
            "a pivot to a group above one it is in"
        );
        assert!(
            turn(
                &json!({"from": "WCJQSG", "to": ["#harness"], "intent": "question"}),
                &["#harness"],
                None
            ),
            "a question to a named group it is in"
        );
        assert!(
            turn(
                &json!({"from": "WCJQSG", "to": ["@sak9fz"], "intent": "fyi", "re": "m1"}),
                &[],
                Some(&json!({"from": "SAK9FZ"}))
            ),
            "an answer to a message it sent, its address in any case"
        );
    }

    #[test]
    fn news_its_own_words_and_what_never_reaches_it_wake_nothing() {
        assert!(
            !turn(
                &json!({"from": "WCJQSG", "to": ["@SAK9FZ"], "intent": "fyi"}),
                &[],
                None
            ),
            "news waits for its next start"
        );
        assert!(
            !turn(
                &json!({"from": "SAK9FZ", "to": ["@SAK9FZ"], "intent": "question"}),
                &[],
                None
            ),
            "nor its own question"
        );
        assert!(
            !turn(
                &json!({"from": "WCJQSG", "to": ["@OSPREY"], "intent": "question"}),
                &[],
                None
            ),
            "nor one to another"
        );
        assert!(
            !turn(
                &json!({"from": "WCJQSG", "to": ["#harness"], "intent": "hold"}),
                &["#harness"],
                None
            ),
            "nor a hold to a group, which parks peers rather than giving them a turn"
        );
        assert!(
            !turn(
                &json!({"from": "WCJQSG", "to": ["@SAK9FZ"], "intent": "fyi", "re": "m1"}),
                &[],
                Some(&json!({"from": "OSPREY"}))
            ),
            "nor an answer to another's"
        );
    }

    #[test]
    fn a_parked_session_given_a_turn_is_woken_through_its_own_file() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (park, wakes) = (dir.path().join("park"), dir.path().join("held"));
        let wake = park_one(&park, &wakes, "s1", "SAK9FZ");
        let other = park_one(&park, &wakes, "s2", "OSPREY");
        let message =
            json!({"id": "m2", "from": "WCJQSG", "to": ["@SAK9FZ"], "intent": "question"});

        let (woken, left) = wake_for(&park, None, "m2", &message, &[], |_| None, None);

        assert_eq!((woken, left), (vec!["s1".to_owned()], 0));
        let said: Value =
            serde_json::from_slice(&fs::read(&wake).expect("its wake written")).expect("json");
        assert_eq!(
            (said["session"].as_str(), said["messageId"].as_str()),
            (Some("s1"), Some("m2"))
        );
        assert!(!other.exists(), "the session it gives no turn sleeps on");
    }

    #[test]
    fn a_worker_wakes_a_session_only_for_its_own_and_only_so_many() {
        let worker = |message: &Value, groups: &[&str], earlier: Option<&Value>| {
            let parked = Parked {
                session: "s1".to_owned(),
                addresses: vec!["SAK9FZ".to_owned()],
                wake: PathBuf::from("/nowhere/s1.wake"),
            };
            let groups: Vec<String> = groups.iter().map(|group| (*group).to_owned()).collect();
            worker_gives_turn(message, &parked, &groups, |_| earlier.cloned())
        };
        let to_it = |intent: &str| json!({"from": "KESTREL", "to": ["@SAK9FZ"], "intent": intent});
        assert!(
            worker(&to_it("question"), &[], None),
            "a question to its address"
        );
        assert!(worker(&to_it("hold"), &[], None), "a hold to its address");
        assert!(!worker(&to_it("pivot"), &[], None), "never a pivot");
        assert!(
            !worker(
                &json!({"from": "KESTREL", "to": ["#harness"], "intent": "question"}),
                &["#harness"],
                None
            ),
            "never a question to a group it is in"
        );
        assert!(
            worker(
                &json!({"from": "KESTREL", "to": ["#harness"], "intent": "fyi", "re": "m1"}),
                &["#harness"],
                Some(&json!({"from": "SAK9FZ"}))
            ),
            "an answer to one it sent"
        );
        assert!(
            !worker(
                &json!({"from": "KESTREL", "to": ["#harness"], "intent": "fyi", "re": "m1"}),
                &["#harness"],
                Some(&json!({"from": "OSPREY"}))
            ),
            "never an answer to another's"
        );

        let dir = tempfile::tempdir().expect("a temporary folder");
        let (park, wakes) = (dir.path().join("park"), dir.path().join("held"));
        let all: Vec<PathBuf> = ["s1", "s2", "s3"]
            .into_iter()
            .map(|session| park_one(&park, &wakes, session, "SAK9FZ"))
            .collect();
        let message =
            json!({"id": "m2", "from": "KESTREL", "to": ["@SAK9FZ"], "intent": "question"});
        let (woken, left) = wake_for(&park, None, "m2", &message, &[], |_| None, Some(2));
        assert_eq!(
            (woken.len(), left),
            (2, 1),
            "two woken, one left for its next start"
        );
        assert_eq!(all.iter().filter(|wake| wake.exists()).count(), 2);
    }

    #[test]
    fn a_marker_from_before_the_machine_started_wakes_nothing() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (park, wakes) = (dir.path().join("park"), dir.path().join("held"));
        let wake = park_one(&park, &wakes, "s1", "SAK9FZ");
        let message = json!({"from": "WCJQSG", "to": ["@SAK9FZ"], "intent": "question"});
        let later = jiff::Timestamp::now().as_millisecond() + 60_000;

        let (woken, _) = wake_for(&park, Some(later), "m2", &message, &[], |_| None, None);
        assert_eq!(woken, Vec::<String>::new(), "none is woken before its time");
        assert!(!wake.exists());
    }

    #[test]
    fn a_wake_never_goes_through_a_link_nor_into_a_folder_others_may_write() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (park, wakes) = (dir.path().join("park"), dir.path().join("held"));
        let wake = park_one(&park, &wakes, "s1", "SAK9FZ");
        let mine = dir.path().join("mine.json");
        fs::write(&mine, b"{}").expect("a file of the account's own");
        std::os::unix::fs::symlink(&mine, &wake).expect("a link planted as the wake");
        let message = json!({"from": "WCJQSG", "to": ["@SAK9FZ"], "intent": "question"});

        assert_eq!(
            wake_for(&park, None, "m2", &message, &[], |_| None, None).0,
            ["s1"]
        );
        assert_eq!(
            fs::read(&mine).expect("it stands"),
            b"{}",
            "the link is replaced, never written through"
        );

        fs::set_permissions(&wakes, fs::Permissions::from_mode(0o777)).expect("opened to all");
        assert!(
            wake_for(&park, None, "m3", &message, &[], |_| None, None)
                .0
                .is_empty(),
            "a folder another account may write is never written in"
        );
        fs::set_permissions(&wakes, fs::Permissions::from_mode(0o755)).expect("closed again");
        fs::set_permissions(park.join("parked"), fs::Permissions::from_mode(0o777))
            .expect("markers opened to all");
        assert!(
            parked(&park, None).is_empty(),
            "nor is a marker read from a folder another account may write"
        );
    }
}
