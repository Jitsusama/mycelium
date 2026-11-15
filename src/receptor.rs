//! Receptors: the cells that sense one outside source each, such as Slack
//! or mail, and bring what happens there in, intake only. Nothing here
//! ever sends, replies or marks anything read at a source.
//!
//! Each receptor says how it stands in one file of the receptors' folder,
//! `<state>/receptors/<source>.json`, written whole under a dot-name and
//! renamed into place, as a presence is. The file holds counts and times
//! alone: never a message's text, its sender, its subject or the channel
//! it came from, since the node serves it to every reader and across
//! links. So the file's shape is an allow-list: a field it doesn't name,
//! or a word outside the ones each field takes, is refused as it is
//! written, and a file holding one is dropped as it is read.
//!
//! The node judges what each file says rather than believing it: one of
//! another realm is dropped, and one whose process is gone, or late past
//! three of its rounds, reads `off` or `failing`, so a receptor that died
//! never reads `reading`.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write as _};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use notify::Watcher as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, watch};

/// The kind of a receptor's records in the daemon interface.
pub const KIND: &str = "receptor";

/// The network every receptor of `source` joins is `outside.<source>`.
pub const OUTSIDE: &str = "outside";

/// How many of its rounds a receptor may miss before it reads `failing`.
const LATE_AFTER_ROUNDS: u32 = 3;

/// The longest round a receptor may say it reads on: a day.
const LONGEST_ROUND_SECS: u32 = 86_400;

/// Mode of the receptors' folder: its owner's alone.
const FOLDER_MODE: u32 = 0o700;

/// Mode of a receptor's file: its owner's alone.
const FILE_MODE: u32 = 0o600;

/// What a receptor senses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Slack: mentions and messages to its person.
    Slack,
    /// Mail to its person.
    Mail,
    /// Its person's calendar.
    Calendar,
    /// What a call leaves behind: its transcript and summary.
    Calls,
    /// Vault: reviews and mentions asked of its person.
    Vault,
    /// GitHub: reviews asked of and mentions of its person.
    Github,
    /// Instant messages to its person.
    Messages,
}

impl Source {
    /// Each, in the order they are drawn.
    pub const EACH: [Self; 7] = [
        Self::Slack,
        Self::Mail,
        Self::Calendar,
        Self::Calls,
        Self::Vault,
        Self::Github,
        Self::Messages,
    ];

    /// Its name, as its file and its network are named.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Slack => "slack",
            Self::Mail => "mail",
            Self::Calendar => "calendar",
            Self::Calls => "calls",
            Self::Vault => "vault",
            Self::Github => "github",
            Self::Messages => "messages",
        }
    }

    /// How a person reads its name.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Slack => "Slack",
            Self::Mail => "Mail",
            Self::Calendar => "Calendar",
            Self::Calls => "Calls",
            Self::Vault => "Vault",
            Self::Github => "GitHub",
            Self::Messages => "Messages",
        }
    }

    /// The source `name` names, or none.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::EACH.into_iter().find(|each| each.name() == name)
    }

    /// The network its receptors join, such as `outside.slack`.
    #[must_use]
    pub fn network(self) -> String {
        format!("{OUTSIDE}.{}", self.name())
    }
}

/// How a receptor stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// It reads its source, and its last round found something.
    Reading,
    /// It reads its source, and its last round found nothing new.
    Quiet,
    /// It can't read its source, or is late past three of its rounds.
    Failing,
    /// It isn't running.
    Off,
}

/// What a receptor brought in, counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counts {
    /// Signals in the last hour.
    pub hour: u32,
    /// Signals today.
    pub today: u32,
    /// Of today's, those addressed to its person.
    pub addressed: u32,
}

/// How one receptor stands, as its file says it: counts and times alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// What it senses, which names its file.
    pub source: Source,
    /// The realm it reads for, whose node alone serves it.
    pub realm: String,
    /// How it stands, as it says.
    pub state: State,
    /// When it began reading.
    pub since: Timestamp,
    /// When it last read its source.
    pub read_at: Timestamp,
    /// How often it reads, in seconds.
    pub every: u32,
    /// The process that keeps it, so the node marks it off once that's
    /// gone.
    pub pid: u32,
    /// When that process started, in clock ticks since boot, where the
    /// system says (Linux), so a pid used again isn't taken for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<u64>,
    /// What it brought in.
    pub counts: Counts,
}

/// The receptors' folder within mycelium's own folder, `state`.
#[must_use]
pub fn folder(state: &Path) -> PathBuf {
    state.join("receptors")
}

/// The file of `source`'s receptor in `folder`.
#[must_use]
pub fn file(folder: &Path, source: Source) -> PathBuf {
    folder.join(format!("{}.json", source.name()))
}

/// Why a record can't be written.
fn refused(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, why.into())
}

/// `record` written whole to its file in `folder`, for `realm`, under a
/// dot-name renamed into place, the folder its owner's alone and the file
/// too. Its `since` stays the one its file already holds while the same
/// process keeps it.
///
/// # Errors
///
/// When the record is of another realm, reads more often than it could
/// or less than daily, counts more addressed than it has, or the file
/// can't be written.
pub fn write(folder: &Path, realm: &str, mut record: Record) -> io::Result<PathBuf> {
    if record.realm != realm {
        return Err(refused(format!(
            "a receptor of realm {:?} can't be written in realm {realm:?}",
            record.realm
        )));
    }
    if record.every == 0 || record.every > LONGEST_ROUND_SECS {
        return Err(refused(format!(
            "a receptor reads every 1 to {LONGEST_ROUND_SECS} seconds, not {}",
            record.every
        )));
    }
    if record.counts.addressed > record.counts.today || record.counts.hour > record.counts.today {
        return Err(refused(
            "a receptor counts no more in an hour, or addressed, than today",
        ));
    }
    fs::create_dir_all(folder)?;
    fs::set_permissions(folder, fs::Permissions::from_mode(FOLDER_MODE))?;
    let path = file(folder, record.source);
    if let Some(was) = read_file(&path)
        && was.pid == record.pid
        && was.started == record.started
    {
        record.since = was.since;
    }
    let staged = folder.join(format!(".{}.{}", record.source.name(), std::process::id()));
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(FILE_MODE)
        .open(&staged)?;
    out.write_all(&serde_json::to_vec(&record).map_err(io::Error::other)?)?;
    out.sync_all()?;
    drop(out);
    fs::rename(&staged, &path)?;
    Ok(path)
}

/// The record `path` holds, or none where it holds none or a malformed
/// one: a field it doesn't name, a word a field doesn't take, or a file
/// named for another source.
fn read_file(path: &Path) -> Option<Record> {
    let text = fs::read(path).ok()?;
    let record: Record = serde_json::from_slice(&text).ok()?;
    let named = path.file_stem()?.to_str()?;
    (named == record.source.name()).then_some(record)
}

/// Whether a process lives, as the node can tell.
pub trait Living {
    /// Whether process `pid`, started at `started` where that's known,
    /// still runs.
    fn lives(&self, pid: u32, started: Option<u64>) -> bool;
}

/// This machine's processes, as its kernel tells them.
pub struct Here;

impl Living for Here {
    fn lives(&self, pid: u32, started: Option<u64>) -> bool {
        let Some(id) = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        else {
            return false;
        };
        if rustix::process::test_kill_process(id) == Err(rustix::io::Errno::SRCH) {
            return false;
        }
        match (started, started_at(pid)) {
            (Some(said), Some(is)) => said == is,
            // Where the system can't say when a process started, its
            // living is all that is known.
            _ => true,
        }
    }
}

/// When process `pid` started, in clock ticks since boot, as its `stat`
/// tells it on Linux: the twentieth field after its name, which is in
/// brackets and may hold spaces or brackets itself. None elsewhere.
#[must_use]
pub fn started_at(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, after) = stat.rsplit_once(')')?;
    after.split_whitespace().nth(19)?.parse().ok()
}

/// Every receptor `folder` holds for `realm`, by source, each judged as
/// of `now`: one whose process is gone reads `off`, and one late past
/// three of its rounds `failing`. A file of another realm, a malformed
/// one or one being written is left out.
pub fn judged(
    folder: &Path,
    realm: &str,
    now: Timestamp,
    living: &impl Living,
) -> BTreeMap<Source, Record> {
    let Ok(files) = fs::read_dir(folder) else {
        return BTreeMap::new();
    };
    files
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "json")
                && !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with('.'))
        })
        .filter_map(|path| read_file(&path))
        .filter(|record| record.realm == realm)
        .map(|mut record| {
            record.state = judged_state(&record, now, living);
            (record.source, record)
        })
        .collect()
}

/// The daemon interface's records of `receptors`, those of `machine`, by
/// key, each heard when it last read its source.
#[must_use]
pub fn records(receptors: &BTreeMap<Source, Record>, machine: &str) -> BTreeMap<String, Value> {
    receptors
        .values()
        .map(|record| {
            let mut fields = serde_json::to_value(record).unwrap_or_default();
            fields["machine"] = machine.into();
            let id = record.source.name();
            let heard = record.read_at.as_millisecond();
            (
                crate::daemon::key(KIND, id),
                crate::daemon::record(KIND, id, heard, fields),
            )
        })
        .collect()
}

/// The receptors a node holds, judged, kept as they change.
pub type Held = Arc<Mutex<BTreeMap<Source, Record>>>;

/// How often a node judges its receptors again with nothing written, so
/// one whose process died or that fell late reads so within this.
const JUDGED_EVERY: Duration = Duration::from_secs(30);

/// How long after a change the node waits for more before reading, so a
/// burst of writes is read once.
const SETTLED_AFTER: Duration = Duration::from_millis(20);

/// Keeps `held` as the receptors of `folder` stand for `realm`, judged
/// each time a file there changes and every [`JUDGED_EVERY`] besides,
/// telling `tell` each time they differ. The folder is made, its owner's
/// alone, where it isn't there, so it can be watched from the start.
pub async fn keep(folder: PathBuf, realm: String, held: Held, tell: watch::Sender<u64>) {
    let judge = || {
        let now = judged(&folder, &realm, Timestamp::now(), &Here);
        let mut held = held.lock().unwrap_or_else(PoisonError::into_inner);
        if *held != now {
            *held = now;
            tell.send_modify(|changes| *changes += 1);
        }
    };
    let made = fs::create_dir_all(&folder)
        .and_then(|()| fs::set_permissions(&folder, fs::Permissions::from_mode(FOLDER_MODE)));
    let (heard, mut changed) = mpsc::unbounded_channel();
    // Without a watch, as on a folder that can't be made, the receptors
    // are still judged each round. The watch lasts as long as this does.
    let _watching = made.ok().and_then(|()| {
        let mut watcher = crate::watching::watcher(move |_: notify::Result<notify::Event>| {
            // Nothing hears this once the node has stopped.
            let _ = heard.send(());
        })
        .ok()?;
        watcher
            .watch(&folder, notify::RecursiveMode::NonRecursive)
            .ok()?;
        Some(watcher)
    });
    let mut rounds = tokio::time::interval(JUDGED_EVERY);
    loop {
        judge();
        tokio::select! {
            _ = rounds.tick() => {}
            Some(()) = changed.recv() => {
                tokio::time::sleep(SETTLED_AFTER).await;
                while changed.try_recv().is_ok() {}
            }
        }
    }
}

/// How `record` stands as of `now`, whatever it says.
fn judged_state(record: &Record, now: Timestamp, living: &impl Living) -> State {
    if !living.lives(record.pid, record.started) {
        return State::Off;
    }
    let late = i64::from(record.every) * i64::from(LATE_AFTER_ROUNDS);
    if now.as_second() - record.read_at.as_second() > late {
        return State::Failing;
    }
    record.state
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every process lives, or none does.
    struct Lives(bool);

    impl Living for Lives {
        fn lives(&self, _: u32, _: Option<u64>) -> bool {
            self.0
        }
    }

    fn at(text: &str) -> Timestamp {
        text.parse().expect("a time")
    }

    fn slack() -> Record {
        Record {
            source: Source::Slack,
            realm: "work".to_owned(),
            state: State::Reading,
            since: at("2026-11-15T19:00:00Z"),
            read_at: at("2026-11-15T19:10:00Z"),
            every: 300,
            pid: 4242,
            started: Some(700),
            counts: Counts {
                hour: 2,
                today: 5,
                addressed: 1,
            },
        }
    }

    #[test]
    fn a_receptor_is_written_its_owners_alone_and_read_back_as_it_stands() {
        let state = tempfile::tempdir().expect("a folder");
        let folder = folder(state.path());
        let path = write(&folder, "work", slack()).expect("written");
        assert_eq!(path, folder.join("slack.json"));
        let mode = |path: &Path| fs::metadata(path).expect("there").permissions().mode() & 0o777;
        assert_eq!(mode(&folder), 0o700, "the folder is its owner's alone");
        assert_eq!(mode(&path), 0o600, "the file is its owner's alone");
        let read = judged(&folder, "work", at("2026-11-15T19:11:00Z"), &Lives(true));
        assert_eq!(read.get(&Source::Slack), Some(&slack()));
        let left: Vec<_> = fs::read_dir(&folder).expect("listed").flatten().collect();
        assert_eq!(left.len(), 1, "nothing staged is left behind");
    }

    #[test]
    fn a_field_the_record_doesnt_name_is_refused_as_it_is_read() {
        let state = tempfile::tempdir().expect("a folder");
        let folder = folder(state.path());
        write(&folder, "work", slack()).expect("written");
        let mut planted: Value =
            serde_json::from_slice(&fs::read(folder.join("slack.json")).expect("read"))
                .expect("json");
        planted["channel"] = "C0CGANYCGVA".into();
        fs::write(folder.join("slack.json"), planted.to_string()).expect("planted");
        assert!(
            judged(&folder, "work", at("2026-11-15T19:11:00Z"), &Lives(true)).is_empty(),
            "a channel's name planted in the file drops it"
        );
        let mut subject = serde_json::to_value(slack()).expect("json");
        subject["counts"]["subject"] = "Your sign-in code".into();
        fs::write(folder.join("slack.json"), subject.to_string()).expect("planted");
        assert!(
            judged(&folder, "work", at("2026-11-15T19:11:00Z"), &Lives(true)).is_empty(),
            "a subject planted among the counts drops it"
        );
    }

    #[test]
    fn a_word_no_field_takes_is_refused_as_it_is_read() {
        let state = tempfile::tempdir().expect("a folder");
        let folder = folder(state.path());
        fs::create_dir_all(&folder).expect("made");
        for (field, word) in [("source", "C0CGANYCGVA"), ("state", "from Tobi")] {
            let mut bad = serde_json::to_value(slack()).expect("json");
            bad[field] = word.into();
            fs::write(folder.join("slack.json"), bad.to_string()).expect("planted");
            assert!(
                judged(&folder, "work", at("2026-11-15T19:11:00Z"), &Lives(true)).is_empty(),
                "{field} {word:?} drops it"
            );
        }
    }

    #[test]
    fn a_file_named_for_another_source_or_being_written_is_left_out() {
        let state = tempfile::tempdir().expect("a folder");
        let folder = folder(state.path());
        fs::create_dir_all(&folder).expect("made");
        let text = serde_json::to_string(&slack()).expect("json");
        fs::write(folder.join("mail.json"), &text).expect("written");
        fs::write(folder.join(".slack.json"), &text).expect("written");
        assert!(judged(&folder, "work", at("2026-11-15T19:11:00Z"), &Lives(true)).is_empty());
    }

    #[test]
    fn another_realms_receptor_is_refused_as_written_and_dropped_as_read() {
        let state = tempfile::tempdir().expect("a folder");
        let folder = folder(state.path());
        let mut personal = slack();
        personal.realm = "personal".to_owned();
        let refused = write(&folder, "work", personal.clone()).expect_err("refused");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
        fs::create_dir_all(&folder).expect("made");
        fs::write(
            folder.join("slack.json"),
            serde_json::to_string(&personal).expect("json"),
        )
        .expect("planted");
        assert!(
            judged(&folder, "work", at("2026-11-15T19:11:00Z"), &Lives(true)).is_empty(),
            "the work realm's node serves no personal receptor"
        );
        assert_eq!(
            judged(
                &folder,
                "personal",
                at("2026-11-15T19:11:00Z"),
                &Lives(true)
            )
            .len(),
            1,
            "its own realm's does"
        );
    }

    #[test]
    fn counts_that_cant_be_and_rounds_out_of_reach_are_refused() {
        let state = tempfile::tempdir().expect("a folder");
        let folder = folder(state.path());
        let mut more = slack();
        more.counts.addressed = 9;
        assert!(
            write(&folder, "work", more).is_err(),
            "more addressed than today"
        );
        let mut never = slack();
        never.every = 0;
        assert!(write(&folder, "work", never).is_err(), "a round of no time");
        let mut rarely = slack();
        rarely.every = LONGEST_ROUND_SECS + 1;
        assert!(
            write(&folder, "work", rarely).is_err(),
            "a round past a day"
        );
    }

    #[test]
    fn a_receptor_whose_process_is_gone_reads_off_and_one_late_failing() {
        let state = tempfile::tempdir().expect("a folder");
        let folder = folder(state.path());
        write(&folder, "work", slack()).expect("written");
        let state_at = |now: &str, lives: bool| {
            judged(&folder, "work", at(now), &Lives(lives))[&Source::Slack].state
        };
        assert_eq!(state_at("2026-11-15T19:11:00Z", false), State::Off);
        assert_eq!(
            state_at("2026-11-15T19:24:00Z", true),
            State::Reading,
            "two rounds late"
        );
        assert_eq!(
            state_at("2026-11-15T19:26:00Z", true),
            State::Failing,
            "past three"
        );
    }

    #[test]
    fn its_since_stays_while_one_process_keeps_it_and_starts_again_with_another() {
        let state = tempfile::tempdir().expect("a folder");
        let folder = folder(state.path());
        write(&folder, "work", slack()).expect("written");
        let mut next = slack();
        next.since = at("2026-11-15T19:15:00Z");
        next.read_at = at("2026-11-15T19:15:00Z");
        write(&folder, "work", next.clone()).expect("written");
        let since = |folder: &Path| {
            judged(folder, "work", at("2026-11-15T19:16:00Z"), &Lives(true))[&Source::Slack].since
        };
        assert_eq!(since(&folder), at("2026-11-15T19:00:00Z"));
        next.pid = 4343;
        write(&folder, "work", next).expect("written");
        assert_eq!(since(&folder), at("2026-11-15T19:15:00Z"));
    }

    #[test]
    fn this_process_lives_here_and_one_started_otherwise_doesnt() {
        let me = std::process::id();
        assert!(Here.lives(me, started_at(me)));
        if let Some(started) = started_at(me) {
            assert!(!Here.lives(me, Some(started + 1)), "a pid used again");
        }
    }
}
