//! Steers: what the person says to a session from outside its window, as
//! the launcher's Talk, Pause, Resume and Stop do, carried to the machine
//! the session runs on and put in its steer inbox there, in the harness's
//! own shape (agentic-harness.pi's `lib/mesh/steer.ts`).
//!
//! Each address has a folder in the inboxes, `<inboxes>/<ADDRESS>/`. A
//! steer goes in under a temporary name, then is renamed to end in
//! `.json`, its name starting with the time so the session takes them in
//! the order they were said. A steer with an `id` opens a talk, whose
//! events the session appends one whole line at a time to
//! `<inboxes>/<ADDRESS>/replies/<id>.jsonl`, and which the launcher reads
//! on its own machine: those lines are carried back, whole lines only, in
//! their order, each end knowing how far it has by the file's length.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::hold;
use crate::record::{Session, State};

/// The fields of a steer the harness reads, which are all that is written.
const FIELDS: [&str; 10] = [
    "text", "act", "to", "at", "from", "id", "want", "look", "value", "other",
];

/// What the person may do besides talk, as the harness's `STEER_ACTS`
/// says: close ends a session for good, where stop only interrupts it,
/// and carry ends it here once its work is carried for the machine `to`
/// names; fill puts its words in the session's prompt for the person to
/// send, and move takes it into a host or back out of one, as `to` says;
/// answer answers an open ask, by the `want` it raised, with a choice's
/// `value` or the person's words as `other`, or a look on the person's
/// list, by its `look` id, with a choice's `value` only.
const ACTS: [&str; 8] = [
    "pause", "resume", "fill", "stop", "close", "carry", "move", "answer",
];

/// The fields only an answer carries.
const ANSWERING: [&str; 4] = ["want", "look", "value", "other"];

/// Where a move goes, as the harness's `MOVE_TARGETS` says: a host of
/// its realm, by kind, or plain pi.
const MOVES: [&str; 3] = ["attended", "background", "pi"];

/// The act that only wakes a parked session, as the launcher's Wake
/// does: nothing goes in its inbox, since the harness has no such act,
/// and its holder's wake file is written on the machine it is parked on.
const WAKE: &str = "wake";

/// The stages that end a talk; after one, nothing more is followed.
const ENDED: [&str; 2] = ["replied", "failed"];

/// How long a talk not yet ended is still followed after it was last
/// written, so one whose session never answered isn't followed forever.
pub const FOLLOWED_FOR: Duration = Duration::from_secs(24 * 60 * 60);

/// A steer, as it is written: one session's address, and what was said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Steer {
    /// The address of the session it is for, in capitals.
    pub address: String,
    /// What is written, only the fields the harness reads.
    pub body: Map<String, Value>,
}

impl Steer {
    /// A steer for `address` saying `said`, or why it isn't one: an
    /// address is letters and digits, and a steer has words, an act, or
    /// both, and an id fit to name a file.
    pub fn new(address: &str, said: &Value) -> Result<Self, String> {
        let address = address.to_ascii_uppercase();
        if address.is_empty() || !address.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
            return Err(format!("{address:?} is not an address"));
        }
        let Some(said) = said.as_object() else {
            return Err("a steer is an object of text, act, at, from and id".to_owned());
        };
        let body: Map<String, Value> = FIELDS
            .iter()
            .filter_map(|field| Some(((*field).to_owned(), said.get(*field)?.clone())))
            .collect();
        let text = body.get("text").and_then(Value::as_str).unwrap_or("");
        let act = body.get("act").and_then(Value::as_str);
        if act.is_some_and(|act| act != WAKE && !ACTS.contains(&act)) {
            return Err(format!("an act is one of {}, or {WAKE}", ACTS.join(", ")));
        }
        if act == Some(WAKE) && (!text.trim().is_empty() || body.contains_key("id")) {
            return Err("a wake says nothing; it only wakes a parked session".to_owned());
        }
        if text.trim().is_empty() && act.is_none() {
            return Err("a steer says something or does something".to_owned());
        }
        if act == Some("fill") && text.trim().is_empty() {
            return Err("a fill has words to put in the prompt".to_owned());
        }
        if act == Some("answer") {
            answers(&body)?;
        } else if ANSWERING.iter().any(|field| body.contains_key(*field)) {
            return Err("only an answer names an ask, a look or a choice".to_owned());
        }
        // A carry names where to, as a machine names itself, a move one of
        // the places a session moves to, and nothing else names a place.
        let to = body.get("to").map(|to| to.as_str());
        match (act, to) {
            (Some("carry"), to) if !to.flatten().is_some_and(is_machine) => {
                return Err("a carry names the machine it goes to".to_owned());
            }
            (Some("move"), to) if !to.flatten().is_some_and(|to| MOVES.contains(&to)) => {
                return Err(format!("a move goes to one of {}", MOVES.join(", ")));
            }
            (Some("carry" | "move"), _) | (_, None) => {}
            (_, Some(_)) => {
                return Err("only a carry or a move names where it goes".to_owned());
            }
        }
        if let Some(id) = body.get("id")
            && !id.as_str().is_some_and(is_talk)
        {
            return Err(format!("{id} can't name a talk"));
        }
        Ok(Self { address, body })
    }

    /// Whether it only wakes a parked session, so nothing is delivered.
    #[must_use]
    pub fn only_wakes(&self) -> bool {
        self.body.get("act").and_then(Value::as_str) == Some(WAKE)
    }

    /// The talk it opens, if it opens one.
    #[must_use]
    pub fn talk(&self) -> Option<&str> {
        self.body.get("id").and_then(Value::as_str)
    }

    /// Writes it into `inboxes`, making its address's folder if it isn't
    /// there yet, and answers the name it was given.
    pub fn deliver(&self, inboxes: &Path) -> io::Result<String> {
        let folder = inboxes.join(&self.address);
        fs::create_dir_all(&folder)?;
        let name = format!("{}.json", named_now());
        let writing = folder.join(format!("{name}.tmp"));
        fs::write(&writing, Value::Object(self.body.clone()).to_string())?;
        fs::rename(&writing, folder.join(&name))?;
        Ok(name)
    }
}

/// Why an answer isn't one, if it isn't: it names the want or the look it
/// answers, never both, and gives a choice's value or, to a want only, the
/// person's words, as the harness's `steerFrom` reads it.
fn answers(body: &Map<String, Value>) -> Result<(), String> {
    let named = |field: &str| {
        body.get(field)
            .and_then(Value::as_str)
            .is_some_and(|it| !it.trim().is_empty())
    };
    match (named("want"), named("look")) {
        (false, false) => return Err("an answer names the want or the look it answers".to_owned()),
        (true, true) => return Err("an answer answers a want or a look, not both".to_owned()),
        _ => {}
    }
    match (named("value"), named("other")) {
        (false, false) => Err("an answer gives a choice's value or the person's words".to_owned()),
        (true, true) => Err("an answer gives a choice or words, not both".to_owned()),
        (false, true) if named("look") => Err("a look takes a choice, never words".to_owned()),
        _ => Ok(()),
    }
}

/// The file whose appearing wakes `session`, where it is parked this
/// `boot`: a steer left in a parked session's inbox is read only as it
/// wakes, and nothing else may wake it soon. Its holder's own wake file
/// where this node knows its holder in `held`, else the one its presence
/// names, taken only as a holder's wake file beside `held`, since a
/// presence's path is written into. A park of an earlier boot has no
/// holder left to wake it; where the machine names no boot, any park is
/// taken as this one's.
#[must_use]
pub fn wake_for(session: &Session, boot: Option<&str>, held: &Path) -> Option<PathBuf> {
    let presence = &session.presence;
    let parked = session.state == State::Ended
        && presence.get("state").and_then(Value::as_str) == Some("parked");
    let began = presence.pointer("/owner/bootId").and_then(Value::as_str);
    let this_boot = match (boot, began) {
        (Some(boot), Some(began)) => boot == began,
        _ => true,
    };
    if !parked || !this_boot {
        return None;
    }
    if let Some(id) = &session.held {
        return Some(hold::wake(held, id));
    }
    let named = PathBuf::from(presence.get("wake")?.as_str()?);
    let folder = named.parent()?;
    let a_holders = named.extension().is_some_and(|it| it == "wake")
        && folder.extension().is_some_and(|it| it == "held")
        && folder.parent() == held.parent();
    a_holders.then_some(named)
}

/// Wakes the session whose wake file is `file`, for the steer left for
/// `address`: its holder reads the file once it appears and removes it.
///
/// # Errors
///
/// When the file can't be written.
pub fn wake(file: &Path, address: &str) -> io::Result<()> {
    let said = serde_json::json!({ "steered": address, "at": named_now() });
    fs::write(file, format!("{said}\n"))
}

/// Whether `name` can be a machine's, as a hostname is: letters, digits,
/// dots and dashes, never starting with a dash.
fn is_machine(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
}

/// Whether `id` can name a talk's file, as the harness's `isTalkId` holds.
fn is_talk(id: &str) -> bool {
    let mut bytes = id.bytes();
    let fits = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-';
    id.len() <= 64 && bytes.next().is_some_and(fits) && bytes.all(|byte| fits(byte) || byte == b'.')
}

/// A name that sorts after every one made before it here: the time in
/// milliseconds, then what tells apart two made in the same one.
fn named_now() -> String {
    static MADE: AtomicU64 = AtomicU64::new(0);
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let made = MADE.fetch_add(1, Ordering::Relaxed);
    format!("{}-{:x}{made:04x}", since.as_millis(), std::process::id())
}

/// The file a talk's events go in, if `address` and `talk` can name one:
/// what another machine says of a talk becomes a path here, so neither may
/// reach outside the inboxes.
#[must_use]
pub fn replies(inboxes: &Path, address: &str, talk: &str) -> Option<PathBuf> {
    let addressed = !address.is_empty()
        && address
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit());
    (addressed && is_talk(talk)).then(|| {
        inboxes
            .join(address)
            .join("replies")
            .join(format!("{talk}.jsonl"))
    })
}

/// Makes a talk's file if it isn't there, empty, so whoever watches it
/// finds it from the start.
pub fn open_talk(inboxes: &Path, address: &str, talk: &str) -> io::Result<()> {
    let path = replies(inboxes, address, talk).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{address} {talk} can't name a talk"),
        )
    })?;
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    OpenOptions::new().create(true).append(true).open(path)?;
    Ok(())
}

/// The whole lines a talk's file holds from byte `from` on, each with its
/// newline, and where the next would start; a line still being written is
/// left for the next read. A file not there yet holds none.
pub fn lines_from(path: &Path, from: u64) -> io::Result<(String, u64)> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((String::new(), from)),
        Err(error) => return Err(error),
    };
    file.seek(SeekFrom::Start(from))?;
    let mut read = String::new();
    file.read_to_string(&mut read)?;
    let whole = read.rfind('\n').map_or(0, |last| last + 1);
    read.truncate(whole);
    let next = from + u64::try_from(whole).map_err(io::Error::other)?;
    Ok((read, next))
}

/// Appends `lines`, which the other machine's file holds from byte `from`
/// on, to the talk's file here, but only what this one lacks: lines it
/// already has are not written twice, and lines that would leave a gap are
/// not written at all, to come again from where this file ends.
pub fn append(path: &Path, from: u64, lines: &str) -> io::Result<()> {
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let has = file.metadata()?.len();
    let given = u64::try_from(lines.len()).map_err(io::Error::other)?;
    if has < from || has >= from + given {
        return Ok(());
    }
    let skip = usize::try_from(has - from).map_err(io::Error::other)?;
    if !lines.is_char_boundary(skip) {
        return Ok(());
    }
    file.write_all(&lines.as_bytes()[skip..])
}

/// Whether `lines` hold a stage that ends the talk.
#[must_use]
pub fn ends(lines: &str) -> bool {
    lines.lines().any(|line| {
        serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|event| {
                event
                    .get("stage")?
                    .as_str()
                    .map(|stage| ENDED.contains(&stage))
            })
            .unwrap_or(false)
    })
}

/// A talk being followed, and how much of it this end has.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Talk {
    /// The address whose inbox it is in.
    pub address: String,
    /// Its id.
    pub id: String,
    /// How many bytes of its file this end has.
    pub has: u64,
}

/// Every talk in `inboxes` not yet ended and written within
/// [`FOLLOWED_FOR`], with how much of it is here, for a link to ask the
/// other end to carry on with.
#[must_use]
pub fn open_talks(inboxes: &Path) -> Vec<Talk> {
    let mut talks = Vec::new();
    let Ok(addresses) = fs::read_dir(inboxes) else {
        return talks;
    };
    for address in addresses.flatten() {
        let Ok(files) = fs::read_dir(address.path().join("replies")) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            let (Some(address), Some(id)) = (
                address.file_name().to_str().map(str::to_owned),
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_owned),
            ) else {
                continue;
            };
            if path
                .extension()
                .is_none_or(|extension| extension != "jsonl")
                || !is_talk(&id)
            {
                continue;
            }
            let Ok(meta) = file.metadata() else { continue };
            let fresh = meta
                .modified()
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age < FOLLOWED_FOR);
            let ended = fs::read_to_string(&path).is_ok_and(|text| ends(&text));
            if fresh && !ended {
                talks.push(Talk {
                    address,
                    id,
                    has: meta.len(),
                });
            }
        }
    }
    talks
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn inboxes() -> tempfile::TempDir {
        tempfile::tempdir().expect("a folder")
    }

    #[test]
    fn a_steer_is_written_whole_under_a_name_in_the_order_said() {
        let inboxes = inboxes();
        let first = Steer::new(
            "ab12cd",
            &json!({"text": "hello", "from": "joel", "extra": 1}),
        )
        .expect("a steer");
        let second = Steer::new("AB12CD", &json!({"act": "pause"})).expect("a steer");
        let one = first.deliver(inboxes.path()).expect("delivered");
        let two = second.deliver(inboxes.path()).expect("delivered");
        assert!(one < two, "a later steer sorts after: {one} {two}");
        let folder = inboxes.path().join("AB12CD");
        let written: Value =
            serde_json::from_str(&fs::read_to_string(folder.join(&one)).expect("read"))
                .expect("JSON");
        assert_eq!(
            written,
            json!({"text": "hello", "from": "joel"}),
            "only the harness's fields are written"
        );
        let names: Vec<String> = fs::read_dir(&folder)
            .expect("listed")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names
                .iter()
                .all(|name| Path::new(name).extension().is_some_and(|it| it == "json")),
            "no temporary name is left: {names:?}"
        );
    }

    #[test]
    fn what_isnt_a_steer_is_refused() {
        for (address, said) in [
            ("../X", json!({"text": "hi"})),
            ("", json!({"text": "hi"})),
            ("AB12", json!("hi")),
            ("AB12", json!({"text": " "})),
            ("AB12", json!({"act": "explode"})),
            ("AB12", json!({"text": "hi", "id": "../talk"})),
            ("AB12", json!({"act": "carry"})),
            ("AB12", json!({"act": "carry", "to": "../penelope"})),
            ("AB12", json!({"act": "carry", "to": "-o"})),
            ("AB12", json!({"act": "close", "to": "penelope"})),
            ("AB12", json!({"act": "wake", "text": "hi"})),
            ("AB12", json!({"act": "wake", "id": "talk-1"})),
            ("AB12", json!({"act": "fill"})),
            ("AB12", json!({"act": "fill", "text": " "})),
            ("AB12", json!({"act": "move"})),
            ("AB12", json!({"act": "move", "to": "penelope"})),
            ("AB12", json!({"act": "answer", "value": "yes"})),
            ("AB12", json!({"act": "answer", "want": "question@t"})),
            (
                "AB12",
                json!({"act": "answer", "want": " ", "value": "yes"}),
            ),
            (
                "AB12",
                json!({"act": "answer", "want": "question@t", "look": "l1", "value": "yes"}),
            ),
            (
                "AB12",
                json!({"act": "answer", "want": "question@t", "value": "yes", "other": "no"}),
            ),
            (
                "AB12",
                json!({"act": "answer", "look": "l1", "other": "my words"}),
            ),
            ("AB12", json!({"act": "pause", "value": "yes"})),
            ("AB12", json!({"text": "hi", "look": "l1"})),
        ] {
            assert!(
                Steer::new(address, &said).is_err(),
                "{address:?} {said} is refused"
            );
        }
        for act in ["pause", "resume", "stop", "close"] {
            assert!(
                Steer::new("AB12", &json!({"act": act})).is_ok(),
                "{act} is an act the harness reads"
            );
        }
        let waking = Steer::new("AB12", &json!({"act": "wake"})).expect("a wake alone");
        assert!(
            waking.only_wakes()
                && !Steer::new("AB12", &json!({"act": "pause"})).is_ok_and(|it| it.only_wakes()),
            "a wake only wakes, and only a wake"
        );
        let carrying = Steer::new("AB12", &json!({"act": "carry", "to": "penelope"}))
            .expect("a carry naming its machine");
        assert_eq!(
            carrying.body.get("to"),
            Some(&json!("penelope")),
            "a carry's machine is written for the harness"
        );
        // For You's deal fills a session's prompt with words for Joel to
        // send, and a session moves into a host or back out of one.
        let filling = Steer::new("AB12", &json!({"act": "fill", "text": "ready to deal"}))
            .expect("a fill with its words");
        assert_eq!(
            filling.body.get("text"),
            Some(&json!("ready to deal")),
            "a fill's words are written for the harness"
        );
        for to in ["attended", "background", "pi"] {
            let moving = Steer::new("AB12", &json!({"act": "move", "to": to}))
                .expect("a move naming where it goes");
            assert_eq!(
                moving.body.get("to"),
                Some(&json!(to)),
                "a move's {to} is written for the harness"
            );
        }
        // Needs You answers an open ask with a choice or his own words, and
        // the look list answers a look with a choice, each where it waits.
        for said in [
            json!({"act": "answer", "want": "question@2026-11-12T07:00:00.000Z", "value": "keep", "from": "joel", "at": "t"}),
            json!({"act": "answer", "want": "question@2026-11-12T07:00:00.000Z", "other": "roll it back"}),
            json!({"act": "answer", "look": "look-1", "value": "Keep it"}),
        ] {
            let answering = Steer::new("AB12", &said).expect("an answer");
            assert_eq!(
                Value::Object(answering.body.clone()),
                said,
                "every field an answer carries is written for the harness"
            );
        }
        let talking = Steer::new("AB12", &json!({"text": "hi", "id": "talk-1"})).expect("a steer");
        assert_eq!(talking.talk(), Some("talk-1"), "its talk");
    }

    #[test]
    fn only_whole_lines_are_read_and_none_is_appended_twice() {
        let inboxes = inboxes();
        let far = replies(inboxes.path(), "AB12", "talk-1").expect("a talk's file");
        assert_eq!(
            replies(inboxes.path(), "..", "talk-1"),
            None,
            "no address reaches outside the inboxes"
        );
        assert_eq!(
            replies(inboxes.path(), "AB12", "../x"),
            None,
            "nor does a talk"
        );
        let near = inboxes.path().join("near.jsonl");
        fs::create_dir_all(far.parent().expect("a folder")).expect("made");
        fs::write(&far, "{\"stage\":\"delivered\"}\n{\"stage\":\"rep").expect("written");
        let (lines, next) = lines_from(&far, 0).expect("read");
        assert_eq!(lines, "{\"stage\":\"delivered\"}\n", "the half line waits");
        append(&near, 0, &lines).expect("appended");
        append(&near, 0, &lines).expect("appended again");
        assert_eq!(
            fs::read_to_string(&near).expect("read"),
            lines,
            "lines already here aren't written twice"
        );
        assert!(!ends(&lines), "a talk delivered hasn't ended");

        let mut file = OpenOptions::new().append(true).open(&far).expect("opened");
        file.write_all(b"lying\"}\n{\"stage\":\"replied\"}\n")
            .expect("written");
        let (more, _) = lines_from(&far, next).expect("read");
        append(&near, next + 1, &more).expect("a gap is left alone");
        assert_eq!(fs::read_to_string(&near).expect("read"), lines, "no gap");
        append(&near, next, &more).expect("appended");
        assert_eq!(
            fs::read_to_string(&near).expect("read"),
            fs::read_to_string(&far).expect("read"),
            "the lines follow in order"
        );
        assert!(ends(&more), "a reply ends the talk");
    }

    #[test]
    fn open_talks_are_those_not_yet_answered() {
        let inboxes = inboxes();
        open_talk(inboxes.path(), "AB12", "talk-1").expect("opened");
        open_talk(inboxes.path(), "AB12", "talk-2").expect("opened");
        fs::write(
            replies(inboxes.path(), "AB12", "talk-2").expect("a talk's file"),
            "{\"stage\":\"failed\"}\n",
        )
        .expect("written");
        assert_eq!(
            open_talks(inboxes.path()),
            vec![Talk {
                address: "AB12".to_owned(),
                id: "talk-1".to_owned(),
                has: 0,
            }],
            "only the talk not answered is open"
        );
    }

    /// A session standing as `state` says, with what `presence` adds to
    /// its presence, held as `held` where that says.
    fn session(state: &str, presence: &Value, held: Option<&str>) -> Session {
        let mut session: Session = serde_json::from_value(json!({
            "id": "r-naps", "address": "NAPS02", "machine": "skiff", "groups": [],
            "state": state, "at": "2026-11-08T08:00:00Z", "presence": presence,
        }))
        .expect("a session");
        session.held = held.map(str::to_owned);
        session
    }

    #[test]
    fn a_session_parked_this_boot_is_woken_by_its_holders_file() {
        let held = Path::new("/run/user/1000/mycelium/personal.held");
        let parked = |boot: &str, wake: &str| json!({"state": "parked", "owner": {"bootId": boot}, "wake": wake});
        let beside = "/run/user/1000/mycelium/work.held/5e551011.wake";
        let woken = |one: &Session| wake_for(one, Some("this-boot"), held);
        assert_eq!(
            woken(&session(
                "ended",
                &parked("this-boot", beside),
                Some("dd881110")
            )),
            Some(held.join("dd881110.wake")),
            "by the holder this node knows, first"
        );
        assert_eq!(
            woken(&session("ended", &parked("this-boot", beside), None)),
            Some(PathBuf::from(beside)),
            "else by the file its presence names, a holder's beside this one's"
        );
        for (elsewhere, what) in [
            ("/home/joel/.zshrc", "a file no holder reads"),
            (
                "/run/user/1000/mycelium/work.held/5e551011.json",
                "a holder's record",
            ),
            (
                "/tmp/work.held/5e551011.wake",
                "a holder's name outside the realms' folder",
            ),
        ] {
            assert_eq!(
                woken(&session("ended", &parked("this-boot", elsewhere), None)),
                None,
                "never {what}"
            );
        }
        assert_eq!(
            woken(&session(
                "ended",
                &parked("a-boot-before", beside),
                Some("dd881110")
            )),
            None,
            "nor a park of a boot before, whose holder is gone"
        );
        assert_eq!(
            woken(&session("idle", &json!({"wake": beside}), Some("dd881110"))),
            None,
            "nor one that runs"
        );
        assert_eq!(
            wake_for(
                &session("ended", &parked("a-boot-before", beside), None),
                None,
                held
            ),
            Some(PathBuf::from(beside)),
            "where the machine names no boot, any park is this one's"
        );
    }

    #[test]
    fn a_steer_reaches_each_conversation_a_host_holds_by_its_address() {
        let held = Path::new("/run/user/1000/mycelium/personal.held");
        let hosted = |conversation: &str| {
            json!({
                "state": "parked", "owner": {"pid": 4242, "bootId": "this-boot"},
                "hosted": {"host": "lantern", "conversation": conversation},
            })
        };
        // A conversation its host parked lives while the host does, so it
        // reads idle, and the host's own waker loads it again.
        assert_eq!(
            wake_for(
                &session("idle", &hosted("c-1"), None),
                Some("this-boot"),
                held
            ),
            None,
            "no holder's file wakes a conversation within a host"
        );
        let inboxes = inboxes();
        for (address, text) in [("LANT01", "to the first"), ("LANT02", "to the second")] {
            Steer::new(address, &json!({"text": text}))
                .expect("a steer")
                .deliver(inboxes.path())
                .expect("delivered");
        }
        for (address, text) in [("LANT01", "to the first"), ("LANT02", "to the second")] {
            let said: Vec<Value> = fs::read_dir(inboxes.path().join(address))
                .expect("its inbox")
                .flatten()
                .map(|entry| {
                    serde_json::from_str(&fs::read_to_string(entry.path()).expect("read"))
                        .expect("JSON")
                })
                .collect();
            assert_eq!(
                said,
                [json!({ "text": text })],
                "{address} hears its own steer alone, though one process holds both"
            );
        }
    }

    #[test]
    fn a_wake_is_written_where_its_holder_looks() {
        let dir = inboxes();
        let file = dir.path().join("dd881110.wake");
        wake(&file, "NAPS02").expect("woken");
        let said: Value =
            serde_json::from_str(&fs::read_to_string(&file).expect("written")).expect("JSON");
        assert_eq!(said["steered"], "NAPS02", "saying for whom");
    }
}
