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

/// The fields of a steer the harness reads, which are all that is written.
const FIELDS: [&str; 5] = ["text", "act", "at", "from", "id"];

/// What the person may do besides talk.
const ACTS: [&str; 3] = ["pause", "resume", "stop"];

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
        if act.is_some_and(|act| !ACTS.contains(&act)) {
            return Err(format!("an act is one of {}", ACTS.join(", ")));
        }
        if text.trim().is_empty() && act.is_none() {
            return Err("a steer says something or does something".to_owned());
        }
        if let Some(id) = body.get("id") {
            if !id.as_str().is_some_and(is_talk) {
                return Err(format!("{id} can't name a talk"));
            }
        }
        Ok(Self { address, body })
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

/// Whether `id` can name a talk's file, as the harness's `isTalkId` holds.
fn is_talk(id: &str) -> bool {
    let mut bytes = id.bytes();
    let fits = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-';
    id.len() <= 64
        && bytes.next().is_some_and(fits)
        && bytes.all(|byte| fits(byte) || byte == b'.')
}

/// A name that sorts after every one made before it here: the time in
/// milliseconds, then what tells apart two made in the same one.
fn named_now() -> String {
    static MADE: AtomicU64 = AtomicU64::new(0);
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let made = MADE.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}-{:x}{made:04x}",
        since.as_millis(),
        std::process::id()
    )
}

/// The file a talk's events go in.
#[must_use]
pub fn replies(inboxes: &Path, address: &str, talk: &str) -> PathBuf {
    inboxes.join(address).join("replies").join(format!("{talk}.jsonl"))
}

/// Makes a talk's file if it isn't there, empty, so whoever watches it
/// finds it from the start.
pub fn open_talk(inboxes: &Path, address: &str, talk: &str) -> io::Result<()> {
    let path = replies(inboxes, address, talk);
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
            .and_then(|event| event.get("stage")?.as_str().map(|stage| ENDED.contains(&stage)))
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
                path.file_stem().and_then(|stem| stem.to_str()).map(str::to_owned),
            ) else {
                continue;
            };
            if path.extension().is_none_or(|extension| extension != "jsonl") || !is_talk(&id) {
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
        let first = Steer::new("ab12cd", &json!({"text": "hello", "from": "joel", "extra": 1}))
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
            names.iter().all(|name| name.ends_with(".json")),
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
        ] {
            assert!(
                Steer::new(address, &said).is_err(),
                "{address:?} {said} is refused"
            );
        }
        let talking = Steer::new("AB12", &json!({"text": "hi", "id": "talk-1"})).expect("a steer");
        assert_eq!(talking.talk(), Some("talk-1"), "its talk");
    }

    #[test]
    fn only_whole_lines_are_read_and_none_is_appended_twice() {
        let inboxes = inboxes();
        let far = replies(inboxes.path(), "AB12", "talk-1");
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
        file.write_all(b"lying\"}\n{\"stage\":\"replied\"}\n").expect("written");
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
            replies(inboxes.path(), "AB12", "talk-2"),
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
}
