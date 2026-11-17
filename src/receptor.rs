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
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use jiff::Timestamp;
use notify::Watcher as _;
use rustix::fd::{AsFd, OwnedFd};
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

/// The kind of a receptor's records in the daemon interface.
pub const KIND: &str = "receptor";

/// The network every receptor of `source` joins is `outside.<source>`.
pub const OUTSIDE: &str = "outside";

/// How many of its rounds a receptor may miss before it reads `failing`.
const LATE_AFTER_ROUNDS: u32 = 3;

/// The longest round a receptor may say it reads on: a day.
const LONGEST_ROUND_SECS: u32 = 86_400;

/// Mode of the receptors' folder: its owner's alone. A raw mode is as
/// wide as the platform's `mode_t`, which is 16 bits on macOS.
const FOLDER_MODE: rustix::fs::RawMode = 0o700;

/// Mode of a receptor's file: its owner's alone.
const FILE_MODE: rustix::fs::RawMode = 0o600;

/// The most a receptor's file may hold. Its fields take under 300 bytes,
/// so anything larger was put there by something else.
const LARGEST_FILE: u64 = 4096;

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
    /// Reviews and proposals asked of its person.
    Reviews,
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
        Self::Reviews,
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
            Self::Reviews => "reviews",
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
            Self::Reviews => "Reviews",
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
    /// When that process started, as [`started_at`] tells it where the
    /// system says, so a pid used again isn't taken for it.
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

/// Whether `record` keeps every bound its fields have: a round of a
/// second to a day, and no count past today's. Held as a record is
/// written and again as it is read, so a file written by hand keeps them
/// too.
fn sound(record: &Record) -> io::Result<()> {
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
    Ok(())
}

/// `folder` opened as itself, never through a link, or an error where it
/// isn't a folder of its own.
fn opened_folder(folder: &Path) -> io::Result<OwnedFd> {
    rustix::fs::open(
        folder,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| refused(format!("{} isn't a folder of its own", folder.display())))
}

/// `folder` made where it isn't there and opened as itself, its mode set
/// on it and on nothing a link points at. What is done in it after is
/// done through the handle, so a folder swapped for a link meanwhile
/// takes nothing.
fn owned(folder: &Path) -> io::Result<OwnedFd> {
    fs::create_dir_all(folder)?;
    let opened = opened_folder(folder)?;
    rustix::fs::fchmod(&opened, Mode::from_raw_mode(FOLDER_MODE))?;
    Ok(opened)
}

/// `record` written whole to its file in `folder`, for `realm`, under a
/// dot-name renamed into place, the folder its owner's alone and the file
/// too. Its `since` stays the one its file already holds while the same
/// process keeps it.
///
/// # Errors
///
/// When the record is of another realm, reads more often than it could
/// or less than daily, counts more addressed than it has, names a process
/// `living` says isn't running, or the file can't be written.
pub fn write(
    folder: &Path,
    realm: &str,
    mut record: Record,
    living: &impl Living,
) -> io::Result<PathBuf> {
    if record.realm != realm {
        return Err(refused(format!(
            "a receptor of realm {:?} can't be written in realm {realm:?}",
            record.realm
        )));
    }
    sound(&record)?;
    if !living.lives(record.pid, record.started) {
        return Err(refused(format!(
            "process {} isn't running, so it can't keep a receptor",
            record.pid
        )));
    }
    let opened = owned(folder)?;
    let name = format!("{}.json", record.source.name());
    if let Some(was) = read_at(&opened, &name)
        && was.pid == record.pid
        && was.started == record.started
    {
        record.since = was.since;
    }
    let staged = format!(".{}.{}", record.source.name(), std::process::id());
    // Whatever stands at the staged name, a link among them, is removed
    // rather than written through, and the file is made new or not at
    // all, so nothing a link points at is ever truncated.
    match rustix::fs::unlinkat(&opened, &staged, AtFlags::empty()) {
        Err(rustix::io::Errno::NOENT) | Ok(()) => {}
        Err(error) => return Err(error.into()),
    }
    let mut out = fs::File::from(rustix::fs::openat(
        &opened,
        &staged,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(FILE_MODE),
    )?);
    out.write_all(&serde_json::to_vec(&record).map_err(io::Error::other)?)?;
    out.sync_all()?;
    drop(out);
    rustix::fs::renameat(&opened, &staged, &opened, &name)?;
    Ok(file(folder, record.source))
}

/// The record the file `name` in the folder `opened` holds, or none where
/// it holds none or a malformed one: a field it doesn't name, a word a
/// field doesn't take, a bound broken, or a file named for another
/// source. Only a regular file of at most [`LARGEST_FILE`] is read: its
/// type is asked before it is opened, never through a link, and asked
/// again of what was opened, which is opened without waiting on a writer,
/// so nothing planted in the folder can hold or flood its reader.
fn read_at(opened: impl AsFd, name: &str) -> Option<Record> {
    let stat = rustix::fs::statat(&opened, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return None;
    }
    let opened = rustix::fs::openat(
        &opened,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let file = fs::File::from(opened);
    if !file.metadata().ok()?.file_type().is_file() {
        return None;
    }
    let mut text = Vec::new();
    file.take(LARGEST_FILE + 1).read_to_end(&mut text).ok()?;
    if u64::try_from(text.len()).ok()? > LARGEST_FILE {
        return None;
    }
    let record: Record = serde_json::from_slice(&text).ok()?;
    sound(&record).ok()?;
    let named = name.strip_suffix(".json")?;
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
        same_start(started, started_at(pid))
    }
}

/// Whether a living process whose start is `is` now, where that can be
/// read, is the one a receptor said started at `said`. A start said but
/// unreadable now may be another account's process holding the pid, as
/// macOS keeps some processes' starts from other accounts, so it isn't
/// taken as the receptor. Where no start was said, as on a system that
/// can't say one, its living is all that is known.
fn same_start(said: Option<u64>, is: Option<u64>) -> bool {
    match (said, is) {
        (Some(said), Some(is)) => said == is,
        (Some(_), None) => false,
        (None, _) => true,
    }
}

/// When process `pid` started, or None where it's gone or the system
/// can't say. On Linux it's in clock ticks since boot, as its `stat`
/// tells it: the twentieth field after its name, which is in brackets
/// and may hold spaces or brackets itself. On macOS it's in seconds since
/// the epoch, as libproc tells it.
#[must_use]
pub fn started_at(pid: u32) -> Option<u64> {
    if cfg!(target_os = "linux") {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let (_, after) = stat.rsplit_once(')')?;
        after.split_whitespace().nth(19)?.parse().ok()
    } else {
        started_as_libproc_says(pid)
    }
}

/// When process `pid` started, in seconds since the epoch, as libproc's
/// BSD information for it says, or None where it's gone. `ps` would say
/// the same, but it is setuid and can't run inside a Nix build.
#[cfg(target_os = "macos")]
fn started_as_libproc_says(pid: u32) -> Option<u64> {
    let pid = libc::c_int::try_from(pid).ok()?;
    let size = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    #[expect(
        unsafe_code,
        reason = "macOS says when a process started only through libproc"
    )]
    // SAFETY: the buffer is a proc_bsdinfo of exactly the size given, and
    // it is read only once proc_pidinfo says it filled all of it.
    let filled = unsafe {
        let wrote = libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        );
        if wrote == size {
            Some(info.assume_init())
        } else {
            None
        }
    };
    filled.map(|info| info.pbi_start_tvsec)
}

/// None: this system says nowhere when a process started, so its living
/// is all that is known.
#[cfg(not(target_os = "macos"))]
fn started_as_libproc_says(_pid: u32) -> Option<u64> {
    None
}

/// Every receptor `folder` holds for `realm`, by source, each judged as
/// of `now`: one whose process is gone reads `off`, and one late past
/// three of its rounds `failing`. A file of another realm, a malformed
/// one or one being written is left out, and a folder that is a link is
/// left unread.
pub fn judged(
    folder: &Path,
    realm: &str,
    now: Timestamp,
    living: &impl Living,
) -> BTreeMap<Source, Record> {
    let Ok(opened) = opened_folder(folder) else {
        return BTreeMap::new();
    };
    let Ok(files) = rustix::fs::Dir::read_from(&opened) else {
        return BTreeMap::new();
    };
    files
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().ok().map(str::to_owned))
        .filter(|name| {
            // A JSON file not being staged under a dot-name; each read
            // holds its name to its record's source.
            name.strip_suffix(".json").is_some() && !name.starts_with('.')
        })
        .filter_map(|name| read_at(&opened, &name))
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
/// calling `tell` each time they differ. The folder is made, its owner's
/// alone, where it isn't there, so it can be watched from the start. Each
/// judging reads the disk off the runtime, so a slow disk holds only
/// this.
pub async fn keep(folder: PathBuf, realm: String, held: Held, tell: impl Fn() + Send + 'static) {
    let making = folder.clone();
    let made = tokio::task::spawn_blocking(move || owned(&making).map(drop))
        .await
        .unwrap_or_else(|_| Err(io::Error::other("making the folder panicked")));
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
        let (reading, of) = (folder.clone(), realm.clone());
        let read =
            tokio::task::spawn_blocking(move || judged(&reading, &of, Timestamp::now(), &Here));
        // A judging that panicked leaves the receptors as last judged,
        // to be judged again next round.
        if let Ok(now) = read.await {
            let mut held = held.lock().unwrap_or_else(PoisonError::into_inner);
            if *held != now {
                *held = now;
                tell();
            }
        }
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
    use std::os::unix::fs::PermissionsExt as _;

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

    /// `record` written as one whose process lives.
    fn write(folder: &Path, realm: &str, record: Record) -> io::Result<PathBuf> {
        super::write(folder, realm, record, &Lives(true))
    }

    /// What `folder` holds for the work realm, judged in a thread of its
    /// own, or none where that doesn't end within a few seconds.
    fn judged_soon(folder: &Path) -> Option<BTreeMap<Source, Record>> {
        let (done, ended) = std::sync::mpsc::channel();
        let folder = folder.to_owned();
        std::thread::spawn(move || {
            // Nothing hears this once the test has given up waiting.
            let _ = done.send(judged(
                &folder,
                "work",
                at("2026-11-15T19:11:00Z"),
                &Lives(true),
            ));
        });
        ended.recv_timeout(Duration::from_secs(5)).ok()
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
        planted["channel"] = "#kestrel-talk".into();
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
        for (field, word) in [("source", "#kestrel-talk"), ("state", "from Skiff")] {
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
    fn counts_that_cant_be_and_rounds_out_of_reach_are_dropped_as_read() {
        let state = tempfile::tempdir().expect("a folder");
        let folder = folder(state.path());
        fs::create_dir_all(&folder).expect("made");
        for (field, value) in [
            ("/counts/addressed", 9),
            ("/counts/hour", 9),
            ("/every", 0),
            ("/every", LONGEST_ROUND_SECS + 1),
        ] {
            let mut bad = serde_json::to_value(slack()).expect("json");
            *bad.pointer_mut(field).expect("a field") = value.into();
            fs::write(folder.join("slack.json"), bad.to_string()).expect("planted");
            assert!(
                judged(&folder, "work", at("2026-11-15T19:11:00Z"), &Lives(true)).is_empty(),
                "{field} {value} drops it"
            );
        }
    }

    #[test]
    fn a_process_that_isnt_running_is_refused_as_written() {
        let state = tempfile::tempdir().expect("a folder");
        let folder = folder(state.path());
        let refused = super::write(&folder, "work", slack(), &Lives(false)).expect_err("refused");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
        assert!(
            !file(&folder, Source::Slack).exists(),
            "nothing was written"
        );
    }

    #[test]
    fn a_file_linked_from_elsewhere_is_dropped() {
        let state = tempfile::tempdir().expect("a folder");
        let folder = folder(state.path());
        let elsewhere = state.path().join("elsewhere.json");
        fs::create_dir_all(&folder).expect("made");
        fs::write(&elsewhere, serde_json::to_vec(&slack()).expect("json")).expect("written");
        std::os::unix::fs::symlink(&elsewhere, folder.join("slack.json")).expect("linked");
        assert_eq!(judged_soon(&folder), Some(BTreeMap::new()));
    }

    #[test]
    fn a_pipe_is_dropped_without_waiting_on_it() {
        let state = tempfile::tempdir().expect("a folder");
        let folder = folder(state.path());
        fs::create_dir_all(&folder).expect("made");
        // The program rather than rustix's mknodat, which macOS doesn't
        // have, and libc's mkfifo would need the unsafe code this crate
        // denies.
        let made = std::process::Command::new("mkfifo")
            .arg(folder.join("slack.json"))
            .status()
            .expect("mkfifo ran");
        assert!(made.success(), "mkfifo made no pipe: {made}");
        assert_eq!(
            judged_soon(&folder),
            Some(BTreeMap::new()),
            "judged at once, and without it"
        );
    }

    #[test]
    fn a_file_larger_than_a_record_could_be_is_dropped() {
        let state = tempfile::tempdir().expect("a folder");
        let folder = folder(state.path());
        fs::create_dir_all(&folder).expect("made");
        let mut text = serde_json::to_vec(&slack()).expect("json");
        text.resize(8192, b' ');
        fs::write(folder.join("slack.json"), text).expect("written");
        assert_eq!(judged_soon(&folder), Some(BTreeMap::new()));
    }

    #[test]
    fn a_staged_name_linked_elsewhere_isnt_written_through() {
        let state = tempfile::tempdir().expect("a folder");
        let folder = folder(state.path());
        let elsewhere = state.path().join("elsewhere");
        fs::create_dir_all(&folder).expect("made");
        fs::write(&elsewhere, "kept").expect("written");
        let staged = folder.join(format!(".slack.{}", std::process::id()));
        std::os::unix::fs::symlink(&elsewhere, &staged).expect("linked");
        write(&folder, "work", slack()).expect("written");
        assert_eq!(fs::read_to_string(&elsewhere).expect("read"), "kept");
        assert!(!staged.exists(), "nothing staged is left behind");
    }

    #[test]
    fn a_folder_linked_from_elsewhere_is_refused_and_left_unread() {
        let state = tempfile::tempdir().expect("a folder");
        let elsewhere = state.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).expect("made");
        fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o755)).expect("opened");
        fs::write(
            elsewhere.join("slack.json"),
            serde_json::to_vec(&slack()).expect("json"),
        )
        .expect("written");
        let folder = folder(state.path());
        std::os::unix::fs::symlink(&elsewhere, &folder).expect("linked");
        assert!(write(&folder, "work", slack()).is_err(), "refused");
        let mode = fs::metadata(&elsewhere)
            .expect("there")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755, "its mode is left as it was");
        assert_eq!(judged_soon(&folder), Some(BTreeMap::new()));
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
    fn a_process_gone_has_no_start_and_lives_no_more() {
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("a process to end");
        let pid = child.id();
        let started = started_at(pid);
        child.wait().expect("it ends");
        assert_eq!(started_at(pid), None, "a pid gone has no start");
        assert!(!Here.lives(pid, started), "a pid gone lives no more");
    }

    #[test]
    fn this_process_lives_here_and_one_started_otherwise_doesnt() {
        let me = std::process::id();
        if cfg!(any(target_os = "linux", target_os = "macos")) {
            assert!(started_at(me).is_some(), "this process's start is known");
        }
        assert!(Here.lives(me, started_at(me)));
        if let Some(started) = started_at(me) {
            assert!(!Here.lives(me, Some(started + 1)), "a pid used again");
        }
    }

    #[test]
    fn a_start_said_is_held_to_the_start_read_and_one_unreadable_isnt_the_receptor() {
        assert!(same_start(Some(700), Some(700)), "the same start");
        assert!(!same_start(Some(700), Some(701)), "another start");
        assert!(
            !same_start(Some(700), None),
            "a start said but unreadable now may be another account's process"
        );
        assert!(
            same_start(None, Some(700)) && same_start(None, None),
            "no start said leaves living all that is known"
        );
    }
}
