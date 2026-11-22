//! A carry's hands as the `mycelium carry` command has them: this
//! machine's node over its socket, the quest's handover in its scratch,
//! `pi-carry` for what no session carries, and the quest's share.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::cell::{self, Brought};
use super::{Done, Hands, PI_CARRY, Refused, Untaken};
use crate::client::Client;
use crate::placement::Placement;
use crate::record::Session;
use crate::share::exchange::Places;
use crate::wire::{Reply, Request, Response, Start, Started};

/// How long between looks at what a carry waits for.
const LOOK_EVERY: Duration = Duration::from_secs(2);

/// How long the node may take to list its ties.
const TIES_WITHIN: Duration = Duration::from_secs(30);

/// How long a carry waits, all told, for the sessions to carry and then
/// to end: time for a busy one to finish its turn and push.
pub const PATIENCE: Duration = Duration::from_secs(15 * 60);

/// The command's hands: the node at `socket`, and the quest's share.
pub struct Through {
    socket: PathBuf,
    shares: Places,
    quest: String,
    /// Where this machine's carries are put: the quest's scratch's
    /// `handover/<this machine's short hostname>/`, as the harness puts
    /// them.
    handover: PathBuf,
    until: Instant,
    /// When the carry began, before any session was told: a refusal
    /// older than this is an earlier carry's.
    began: jiff::Timestamp,
}

impl Through {
    /// Hands for carrying `quest` through the node at `socket`, with the
    /// quest's share in `shares`, waiting no longer than `patience`.
    #[must_use]
    pub fn new(socket: PathBuf, shares: Places, quest: &str, patience: Duration) -> Self {
        // The hostname, not the name the machine goes by in the realm:
        // the harness writes and reads `handover/<hostname>/`, so a
        // machine whose `--machine` differs would lose its carry's notes
        // here until both read the node's record instead.
        let host = rustix::system::uname()
            .nodename()
            .to_string_lossy()
            .into_owned();
        let handover = handover(&shares.scratch, quest, &host);
        Self {
            socket,
            shares,
            quest: quest.to_owned(),
            handover,
            until: Instant::now() + patience,
            began: jiff::Timestamp::now(),
        }
    }

    /// The node's answer to `request`, or why there is none.
    fn ask(&self, request: &Request) -> Result<Response, String> {
        Client::connect(&self.socket)
            .and_then(|mut client| client.ask(request))
            .map_err(|error| format!("the node didn't answer: {error}"))?
    }
}

impl Hands for Through {
    fn machines(&mut self) -> Result<Vec<String>, String> {
        match self.ask(&Request::Machines)? {
            // This one first, then each other one.
            Response::Machines(machines) => {
                Ok(machines.into_iter().skip(1).map(|one| one.name).collect())
            }
            other => Err(format!("the node answered {other:?} for its machines")),
        }
    }

    fn sessions(&mut self) -> Result<Vec<Session>, String> {
        match self.ask(&Request::Sessions)? {
            Response::Sessions(sessions) => Ok(sessions),
            other => Err(format!("the node answered {other:?} for its sessions")),
        }
    }

    fn boot(&self) -> Option<String> {
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|boot| boot.trim().to_owned())
    }

    fn steer(&mut self, address: &str, steer: Value) -> Result<Option<String>, String> {
        let asked = Request::Steer {
            address: address.to_owned(),
            steer,
        };
        match self.ask(&asked)? {
            Response::Steered { by, .. } => Ok(by),
            other => Err(format!("the node answered {other:?} for a steer")),
        }
    }

    fn refused(&mut self) -> Result<Vec<Refused>, String> {
        refusals(&self.handover, self.began)
    }

    fn done(&mut self) -> Result<Vec<Done>, String> {
        unstarted(&self.handover)
    }

    fn started(&mut self, one: &Done) -> Result<(), String> {
        mark_started(&self.handover, &one.session)
    }

    fn ties(&mut self) -> Result<Vec<Value>, String> {
        crate::swarm::records(&self.socket, crate::node::TIE_KIND, TIES_WITHIN)
            .map_err(|error| error.to_string())
    }

    fn cells(&mut self) -> Result<Vec<Value>, String> {
        crate::swarm::records(&self.socket, crate::node::CELL_KIND, TIES_WITHIN)
            .map_err(|error| error.to_string())
    }

    fn formed(&mut self) -> Result<Vec<Value>, String> {
        crate::swarm::records(&self.socket, crate::networks::FORMED, TIES_WITHIN)
            .map_err(|error| error.to_string())
    }

    fn bring(&mut self, brought: &Brought) -> Result<(), String> {
        put_brought(&self.handover, brought)
    }

    fn forget(&mut self, keeping: &BTreeSet<String>) -> Result<Vec<String>, String> {
        forget_stale(&self.handover, keeping)
    }

    fn carry_rest(&mut self, quest: &str, to: &str) -> Result<(), String> {
        let ran = Command::new(PI_CARRY)
            .args([quest, "--to", to])
            .output()
            .map_err(|error| format!("{PI_CARRY} didn't run: {error}"))?;
        if ran.status.success() {
            Ok(())
        } else {
            Err(format!(
                "{PI_CARRY} failed: {}",
                failed_why(&ran.stdout, &ran.stderr)
            ))
        }
    }

    fn shareable(&mut self, quest: &str) -> Result<(), String> {
        self.shares
            .shareable(quest)
            .map_err(|error| error.to_string())
    }

    fn share(&mut self, quest: &str) -> Result<(), String> {
        debug_assert_eq!(quest, self.quest, "a carry shares its own quest");
        self.shares
            .offer(quest, &["handover".to_owned()])
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn start(&mut self, start: Start) -> Result<Started, String> {
        match self.ask(&Request::Start(start))? {
            Response::Started(started) => Ok(started),
            other => Err(format!("the node answered {other:?} for a start")),
        }
    }

    fn takes(&mut self, start: &Start) -> Result<(), Untaken> {
        let answer = Client::connect(&self.socket)
            .and_then(|mut client| client.ask(&Request::Takes(start.clone())))
            .map_err(|error| Untaken::Unasked(format!("it didn't answer: {error}")))?;
        untaken(answer)
    }

    fn wait(&mut self) -> bool {
        if Instant::now() >= self.until {
            return false;
        }
        thread::sleep(LOOK_EVERY);
        true
    }
}

/// What a done file's name ends with, after its session's id.
const DONE: &str = ".done.json";

/// What the mark that a done file's session started ends with.
const STARTED: &str = ".started";

/// What a refusal's file name ends with, after its session's id.
const REFUSED: &str = ".refused.json";

/// A refusal as the harness writes it: the session, why, and when.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Refusal {
    session: String,
    why: String,
    at: jiff::Timestamp,
}

/// Carries `quest` from `here` to `to` as the person asks, through the
/// node at `socket` with the quest's share in `shares`, answering the
/// carry's last line, where each session went. It waits as `mycelium
/// carry` does, for each session to carry itself and end, so it blocks
/// for minutes.
///
/// # Errors
///
/// Why it went no further, as the step that failed said it.
pub fn for_the_person(
    socket: PathBuf,
    shares: Places,
    (quest, here, to): (&str, &str, &str),
) -> Result<Value, String> {
    let mut hands = Through::new(socket, shares, quest, PATIENCE);
    let mut last = None;
    let carried = super::carry(&mut hands, quest, here, to, None, &mut |line| {
        last = Some(line.clone());
    });
    if let Ok(carried) = carried {
        return serde_json::to_value(carried).map_err(|error| error.to_string());
    }
    let said = |field| {
        last.as_ref()
            .and_then(|line: &Value| line.get(field)?.as_str().map(str::to_owned))
            .unwrap_or_default()
    };
    Err(format!(
        "{quest} wasn't carried to {to}: its {} failed: {}",
        said("step"),
        said("said")
    ))
}

/// The refusals in `handover` written since `since`, by session: each
/// file changed since, saying a time since, whose session is the one
/// its name gives. One changed before is passed over unread, whatever
/// it holds. One changed since that can't be read is passed over this
/// time and read again at the next look, so a refusal caught half
/// written still gives its reason once whole, though the harness writes
/// each whole, beside it and renamed into place. One that names another
/// session than its file stops the carry, since it can't be told whose
/// it is.
fn refusals(handover: &Path, since: jiff::Timestamp) -> Result<Vec<Refused>, String> {
    let entries = match fs::read_dir(handover) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", handover.display())),
    };
    let mut refused = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let named = path.file_name().and_then(OsStr::to_str).unwrap_or("");
        let Some(session) = named.strip_suffix(REFUSED) else {
            continue;
        };
        if let Some(why) = refusal(&path, session, since)? {
            refused.push(Refused {
                session: session.to_owned(),
                why,
            });
        }
    }
    refused.sort_by(|one, other| one.session.cmp(&other.session));
    Ok(refused)
}

/// Why the refusal at `path`, named for `session`, says it refused since
/// `since`, as [`refusals`] reads each: nothing when it changed before,
/// can't be read yet or says a time before.
fn refusal(path: &Path, session: &str, since: jiff::Timestamp) -> Result<Option<String>, String> {
    let changed = fs::metadata(path)
        .and_then(|file| file.modified())
        .ok()
        .and_then(|changed| jiff::Timestamp::try_from(changed).ok());
    if changed.is_none_or(|changed| changed < since) {
        return Ok(None);
    }
    let Some(one) = fs::read_to_string(path)
        .ok()
        .and_then(|read| serde_json::from_str::<Refusal>(&read).ok())
    else {
        return Ok(None);
    };
    if one.session != session {
        return Err(format!(
            "{} names {}, not its own session, so whose refusal it is can't be told",
            path.display(),
            one.session
        ));
    }
    Ok((one.at >= since).then_some(one.why))
}

/// Where `host`'s sessions of `quest` hand over, under the quests'
/// `scratch`.
#[must_use]
pub fn handover(scratch: &Path, quest: &str, host: &str) -> PathBuf {
    scratch.join(quest).join("handover").join(short_host(host))
}

/// Why a carry stands refused when its session's refusal is there but
/// can't be told as its own, said plainly rather than as the reader's
/// error, path and all.
pub const UNREADABLE_REFUSAL: &str = "its refusal couldn't be read";

/// Why `session` refused a carry in `handover` since `since`, when it
/// did: [`UNREADABLE_REFUSAL`] when its refusal there can't be told as
/// its own, since its session meant one, handing `unreadable` the
/// reader's error, path and all, so it can still be found.
pub fn refused(
    handover: &Path,
    session: &str,
    since: jiff::Timestamp,
    unreadable: impl FnOnce(String),
) -> Option<String> {
    refusal(
        &handover.join(format!("{session}{REFUSED}")),
        session,
        since,
    )
    .unwrap_or_else(|error| {
        unreadable(error);
        Some(UNREADABLE_REFUSAL.to_owned())
    })
}

/// The errors of the refusals that couldn't be read, each said once,
/// since the carries are judged again each time they're asked for.
#[derive(Debug, Default)]
pub struct Unreadable(Mutex<BTreeSet<String>>);

impl Unreadable {
    /// Whether `error` is said for the first time.
    pub fn first(&self, error: &str) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(error.to_owned())
    }
}

/// Whether `session` wrote its done file in `handover` since `since`, as
/// a session leaving does.
#[must_use]
pub fn left(handover: &Path, session: &str, since: jiff::Timestamp) -> bool {
    fs::metadata(handover.join(format!("{session}{DONE}")))
        .and_then(|file| file.modified())
        .ok()
        .and_then(|changed| jiff::Timestamp::try_from(changed).ok())
        .is_some_and(|changed| changed >= since)
}

/// The done files in `handover` whose sessions haven't started on another
/// machine since they were written, by session.
fn unstarted(handover: &Path) -> Result<Vec<Done>, String> {
    let entries = match fs::read_dir(handover) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", handover.display())),
    };
    let mut done = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let named = path.file_name().and_then(OsStr::to_str).unwrap_or("");
        let Some(session) = named.strip_suffix(DONE) else {
            continue;
        };
        let mark = handover.join(format!("{session}{STARTED}"));
        if marked(&path, &mark) {
            continue;
        }
        let read =
            fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let mut one: Done = serde_json::from_str(&read)
            .map_err(|error| format!("{} isn't a carry's done file: {error}", path.display()))?;
        // When it was last marked started, to the nanosecond: a carry
        // taken again before it starts sees the same, one since another.
        one.started_before = fs::metadata(&mark)
            .and_then(|file| file.modified())
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_nanos().to_string());
        done.push(one);
    }
    done.sort_by(|one, other| one.session.cmp(&other.session));
    Ok(done)
}

/// Marks `session`'s done file in `handover` started, with a mark beside
/// it. The done file stays where it is: `pi-carry` on the other machine
/// reads it there, and the share would carry its rename or removal there
/// as its deletion, perhaps before it had been read.
fn mark_started(handover: &Path, session: &str) -> Result<(), String> {
    if Path::new(session).file_name() != Some(OsStr::new(session)) {
        return Err(format!(
            "{session} isn't a session's id a file can be named for"
        ));
    }
    let mark = handover.join(format!("{session}{STARTED}"));
    fs::write(&mark, format!("{session}\n")).map_err(|error| format!("{}: {error}", mark.display()))
}

/// Puts what `brought` says of its session's cell in `handover`, beside
/// its done file, whole: written aside and renamed into place, so the
/// share never carries half of it, and replacing what an earlier carry
/// of the session put there rather than adding to it.
fn put_brought(handover: &Path, brought: &Brought) -> Result<(), String> {
    let session = brought.session.as_str();
    if Path::new(session).file_name() != Some(OsStr::new(session)) {
        return Err(format!("{session:?} can't name a file in the handover"));
    }
    let said = serde_json::to_vec(brought).map_err(|error| error.to_string())?;
    // Written before any session is told, so before the harness makes
    // the handover where this is the quest's first carry from here.
    fs::create_dir_all(handover).map_err(|error| format!("{}: {error}", handover.display()))?;
    let aside = handover.join(format!(".{session}{}.part", cell::FILE));
    // The handover is the sessions' to write too, so whatever stands at
    // the aside name, a link planted there included, is taken away and
    // the file made new rather than written through it.
    let _ = fs::remove_file(&aside);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&aside)
        .and_then(|mut file| io::Write::write_all(&mut file, &said))
        .and_then(|()| fs::rename(&aside, handover.join(format!("{session}{}", cell::FILE))))
        .map_err(|error| {
            let _ = fs::remove_file(&aside);
            format!(
                "{session}'s cell wasn't put in {}: {error}",
                handover.display()
            )
        })
}

/// Takes away each cell in `handover` but those of `keeping` whose done
/// file is gone or started: an earlier carry's, which its session
/// refused or which finished. A cell beside a done file not yet started
/// is the one the carry that wrote that done file brought, as a failed
/// carry leaves it, so a carry taken again keeps it. Answers the sessions
/// whose cells it took away, by id.
fn forget_stale(handover: &Path, keeping: &BTreeSet<String>) -> Result<Vec<String>, String> {
    let entries = match fs::read_dir(handover) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", handover.display())),
    };
    let mut forgotten = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let named = path.file_name().and_then(OsStr::to_str).unwrap_or("");
        let Some(session) = named.strip_suffix(cell::FILE) else {
            continue;
        };
        // Only a file is a cell: anything else planted under its name is
        // passed over, so it can't stop every later carry.
        if keeping.contains(session) || entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let done = handover.join(format!("{session}{DONE}"));
        if done.exists() && !marked(&done, &handover.join(format!("{session}{STARTED}"))) {
            continue;
        }
        match fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                return Err(format!("{}: {error}", path.display()));
            }
            _ => forgotten.push(session.to_owned()),
        }
    }
    forgotten.sort();
    Ok(forgotten)
}

/// Whether `mark` says the done file at `done` started: a mark no older
/// than it, since a session carried again writes its done file afresh.
fn marked(done: &Path, mark: &Path) -> bool {
    let modified = |path: &Path| fs::metadata(path).and_then(|file| file.modified()).ok();
    match (modified(done), modified(mark)) {
        (Some(done), Some(mark)) => mark >= done,
        _ => false,
    }
}

/// A hostname up to its first dot, as the harness names a machine's
/// handover: a Mac calls itself `lantern.local`, and its handover goes in
/// `lantern`.
pub(crate) fn short_host(host: &str) -> &str {
    host.split('.')
        .next()
        .filter(|short| !short.is_empty())
        .unwrap_or(host)
}

/// Why `pi-carry` failed: what its `{"kind": "failed", "why"}` lines said
/// on its output, since it says everything there, else what it wrote to
/// its errors.
fn failed_why(output: &[u8], errors: &[u8]) -> String {
    let output = String::from_utf8_lossy(output);
    let said: Vec<String> = output
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| line["kind"] == "failed")
        .filter_map(|line| line["why"].as_str().map(str::to_owned))
        .collect();
    if said.is_empty() {
        String::from_utf8_lossy(errors).trim().to_owned()
    } else {
        said.join("; ")
    }
}

/// Whether the node's `answer` to `"Takes"` finds the starts taken: only
/// a placement on a machine does. A node too old to know the ask refuses
/// it as no request, which says nothing of the machine the carry goes to.
fn untaken(answer: Reply) -> Result<(), Untaken> {
    match answer {
        Ok(Response::Placement(Placement::Placed { .. })) => Ok(()),
        Err(why) if why.starts_with("not a request") && why.contains("`Takes`") => {
            Err(Untaken::Unasked(
                "it runs a mycelium too old to know the ask, so update it and carry again"
                    .to_owned(),
            ))
        }
        Err(why) => Err(Untaken::Refused(why)),
        Ok(other) => Err(Untaken::Unasked(format!(
            "it answered {other:?} for whether a start is taken"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    use serde_json::{Value, json};

    use super::{
        UNREADABLE_REFUSAL, Unreadable, failed_why, forget_stale, handover, left, mark_started,
        put_brought, refusals, refused, short_host, unstarted, untaken,
    };
    use crate::carry::cell::Brought;
    use crate::carry::{Refused, Untaken};
    use crate::placement::Placement;
    use crate::wire::Response;

    #[test]
    fn only_a_placement_on_a_machine_finds_starts_taken() {
        let placed = Placement::Placed {
            machine: "tern".to_owned(),
            passed: Vec::new(),
        };
        assert_eq!(untaken(Ok(Response::Placement(placed))), Ok(()));
        assert!(
            matches!(
                untaken(Ok(Response::Placement(Placement::Pending {
                    misses: Vec::new()
                }))),
                Err(Untaken::Unasked(_))
            ),
            "a placement nowhere isn't taken"
        );
        assert_eq!(
            untaken(Err("nowhere: tern is a Mac".to_owned())),
            Err(Untaken::Refused("nowhere: tern is a Mac".to_owned())),
            "a refusal is the machine's, as said"
        );
    }

    #[test]
    fn a_node_too_old_to_know_the_ask_is_said_as_this_machines() {
        let old = "not a request: unknown variant `Takes`, expected one of `Start`, `Run`";
        let Err(Untaken::Unasked(why)) = untaken(Err(old.to_owned())) else {
            panic!("an older node's refusal is no refusal of the machine");
        };
        assert!(why.contains("too old"), "saying why: {why}");
        assert!(
            !why.contains("expected one of"),
            "without serde's list: {why}"
        );
    }

    /// Writes `session`'s done file in `handover`, as the harness does.
    fn write_done(handover: &Path, session: &str) {
        fs::write(
            handover.join(format!("{session}.done.json")),
            format!("{{\"session\": \"{session}\", \"trees\": []}}"),
        )
        .expect("a done file");
    }

    /// The sessions of the done files in `handover` not yet started.
    fn sessions(handover: &Path) -> Vec<String> {
        unstarted(handover)
            .expect("read")
            .into_iter()
            .map(|one| one.session)
            .collect()
    }

    #[test]
    fn a_done_file_marked_started_is_carried_no_more_but_stays_for_its_arrival() {
        let handover = tempfile::tempdir().expect("a handover");
        let handover = handover.path();
        write_done(handover, "s-first");
        write_done(handover, "s-second");
        assert_eq!(sessions(handover), ["s-first", "s-second"], "both carried");
        mark_started(handover, "s-first").expect("marked");
        assert_eq!(
            sessions(handover),
            ["s-second"],
            "the one started goes no more"
        );
        assert!(
            handover.join("s-first.done.json").exists(),
            "its done file stays where the other machine reads it"
        );
    }

    #[test]
    fn a_session_carried_again_after_it_started_is_carried_again() {
        let handover = tempfile::tempdir().expect("a handover");
        let handover = handover.path();
        write_done(handover, "s-first");
        mark_started(handover, "s-first").expect("marked");
        write_done(handover, "s-first");
        let later = SystemTime::now() + Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(handover.join("s-first.done.json"))
            .and_then(|file| file.set_modified(later))
            .expect("written after its mark");
        assert_eq!(
            sessions(handover),
            ["s-first"],
            "a done file newer than its mark is a carry of its own"
        );
    }

    #[test]
    fn a_carry_taken_again_sees_the_same_last_start_and_one_since_another() {
        let handover = tempfile::tempdir().expect("a handover");
        let handover = handover.path();
        let before = || {
            unstarted(handover)
                .expect("read")
                .into_iter()
                .map(|one| one.started_before)
                .collect::<Vec<_>>()
        };
        write_done(handover, "s-first");
        assert_eq!(before(), [None], "never started from here");
        mark_started(handover, "s-first").expect("marked");
        let mark = handover.join("s-first.started");
        let earlier = SystemTime::now() - Duration::from_secs(120);
        fs::File::options()
            .write(true)
            .open(&mark)
            .and_then(|file| file.set_modified(earlier))
            .expect("marked before it was carried again");
        write_done(handover, "s-first");
        let first = before();
        assert!(first[0].is_some(), "carried since it started: {first:?}");
        write_done(handover, "s-first");
        assert_eq!(before(), first, "the same, taken again before it starts");
        mark_started(handover, "s-first").expect("marked");
        fs::File::options()
            .write(true)
            .open(&mark)
            .and_then(|file| file.set_modified(earlier + Duration::from_secs(60)))
            .expect("marked again");
        write_done(handover, "s-first");
        assert_ne!(before(), first, "another once it started again");
    }

    /// What `session`'s cell brings, with one tie.
    fn brought(session: &str, strength: f64) -> Brought {
        Brought {
            session: session.to_owned(),
            cell: "BUSY01".to_owned(),
            ties: vec![json!({"key": "tie:BUSY01~KESTRL", "kind": "tie", "strength": strength})],
            talk: None,
            formed: Vec::new(),
        }
    }

    #[test]
    fn a_cells_ties_go_whole_beside_its_done_file() {
        let dir = tempfile::tempdir().expect("a handover");
        let handover = &dir.path().join("skiff");
        put_brought(handover, &brought("s-busy01", 1.0)).expect("put, its handover made");
        write_done(handover, "s-busy01");
        let read = || -> Value {
            let file = fs::read_to_string(handover.join("s-busy01.cell.json")).expect("written");
            serde_json::from_str(&file).expect("whole")
        };
        put_brought(handover, &brought("s-busy01", 2.5)).expect("put");
        assert_eq!(
            read(),
            json!({
                "session": "s-busy01",
                "cell": "BUSY01",
                "ties": [{"key": "tie:BUSY01~KESTRL", "kind": "tie", "strength": 2.5}],
                "talk": null,
                "formed": [],
            }),
            "beside its done file, saying its five fields"
        );
        put_brought(handover, &brought("s-busy01", 3.0)).expect("put again");
        assert_eq!(
            read()["ties"],
            json!([{"key": "tie:BUSY01~KESTRL", "kind": "tie", "strength": 3.0}]),
            "a carry taken again replaces it rather than adding to it"
        );
        assert_eq!(
            fs::read_dir(handover).expect("listed").count(),
            2,
            "and nothing but it and the done file is left there"
        );
        // Where its aside file could be written, an id climbing out would
        // put its cell above the handover, but for being refused.
        fs::create_dir(handover.join("...")).expect("a folder named for a climb");
        for odd in ["../s-elsewhere", "..", ""] {
            assert!(
                put_brought(handover, &brought(odd, 1.0)).is_err(),
                "{odd:?} is refused"
            );
        }
        assert!(
            !dir.path().join("s-elsewhere.cell.json").exists(),
            "and nothing is put above the handover"
        );
    }

    #[test]
    fn a_planted_link_at_the_aside_name_is_never_written_through() {
        let dir = tempfile::tempdir().expect("a handover");
        let handover = &dir.path().join("skiff");
        fs::create_dir(handover).expect("made");
        let target = dir.path().join("elsewhere");
        fs::write(&target, "kept").expect("written");
        std::os::unix::fs::symlink(&target, handover.join(".s-busy01.cell.json.part"))
            .expect("a link planted at the aside name");
        put_brought(handover, &brought("s-busy01", 1.0)).expect("put");
        assert_eq!(
            fs::read_to_string(&target).expect("read"),
            "kept",
            "what the link pointed at is untouched"
        );
        assert!(
            handover.join("s-busy01.cell.json").is_file(),
            "and the cell is put"
        );
    }

    #[test]
    fn only_the_cells_of_done_files_started_or_gone_are_taken_away_but_those_kept() {
        let dir = tempfile::tempdir().expect("a handover");
        let handover = &dir.path().join("skiff");
        let cell = |session: &str| {
            put_brought(handover, &brought(session, 1.0)).expect("put");
        };
        // An earlier carry's, which finished: its done file started.
        cell("s-busy01");
        write_done(handover, "s-busy01");
        mark_started(handover, "s-busy01").expect("marked");
        // A session that refused an earlier carry: no done file.
        cell("s-refuse");
        // A failed attempt's, its done file not yet started.
        cell("s-naps02");
        write_done(handover, "s-naps02");
        // One this carry brings afresh, though its done file started.
        cell("s-there3");
        write_done(handover, "s-there3");
        mark_started(handover, "s-there3").expect("marked");
        // Planted by something that writes the handover: no cell.
        fs::create_dir(handover.join("s-plant.cell.json")).expect("a folder");
        let keeping = BTreeSet::from(["s-there3".to_owned()]);
        assert_eq!(
            forget_stale(handover, &keeping),
            Ok(vec!["s-busy01".to_owned(), "s-refuse".to_owned()]),
            "the cells an earlier carry left, finished or refused, go"
        );
        let cells = |session: &str| handover.join(format!("{session}.cell.json")).exists();
        assert!(
            cells("s-naps02") && cells("s-there3") && cells("s-plant"),
            "a failed attempt's cell beside its work stays, one kept stays, and a folder \
             planted under a cell's name is passed over"
        );
        assert!(
            ["s-busy01", "s-naps02", "s-there3"]
                .iter()
                .all(|one| handover.join(format!("{one}.done.json")).exists()),
            "and every done file stays"
        );
        assert_eq!(
            forget_stale(&dir.path().join("tern"), &keeping),
            Ok(Vec::new()),
            "a handover not made yet holds nothing to take away"
        );
    }

    #[test]
    fn a_mark_is_made_only_for_a_plain_session_id() {
        let handover = tempfile::tempdir().expect("a handover");
        let handover = handover.path().join("skiff");
        fs::create_dir(&handover).expect("this machine's handover");
        for odd in ["../s-elsewhere", "..", ""] {
            assert!(mark_started(&handover, odd).is_err(), "{odd:?} is refused");
        }
        assert_eq!(
            fs::read_dir(handover.parent().expect("above"))
                .expect("listed")
                .count(),
            1,
            "and nothing was written outside the handover"
        );
    }

    #[test]
    fn only_a_refusal_written_since_the_carry_began_counts() {
        let handover = tempfile::tempdir().expect("a handover");
        let handover = handover.path();
        let began: jiff::Timestamp = "2026-11-15T16:00:00Z".parse().expect("a time");
        for (session, at) in [
            ("s-before", "2026-11-15T15:59:59Z"),
            ("s-since", "2026-11-15T16:00:01Z"),
        ] {
            fs::write(
                handover.join(format!("{session}.refused.json")),
                format!(r#"{{"session": "{session}", "why": "it holds a lease", "at": "{at}"}}"#),
            )
            .expect("a refusal");
        }
        write_done(handover, "s-done");
        let since = || {
            [Refused {
                session: "s-since".to_owned(),
                why: "it holds a lease".to_owned(),
            }]
        };
        assert_eq!(
            refusals(handover, began).expect("read"),
            since(),
            "an earlier carry's refusal, and a done file, aren't this one's"
        );

        let old = handover.join("s-old.refused.json");
        fs::write(&old, "{\"session\": \"s-o").expect("broken");
        let before = SystemTime::from(began) - Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(&old)
            .and_then(|file| file.set_modified(before))
            .expect("dated before the carry");
        assert_eq!(
            refusals(handover, began).expect("read"),
            since(),
            "a file changed before the carry is passed over unread, broken or not"
        );

        let half = handover.join("s-half.refused.json");
        fs::write(&half, r#"{"session": "s-half", "why": "it is mid-re"#).expect("half");
        assert_eq!(
            refusals(handover, began).expect("read"),
            since(),
            "one caught half written waits for the next look"
        );
        fs::write(
            &half,
            r#"{"session": "s-half", "why": "it is mid-rebase", "at": "2026-11-15T16:00:02Z"}"#,
        )
        .expect("whole");
        assert_eq!(
            refusals(handover, began).expect("read")[0].why,
            "it is mid-rebase",
            "and gives its reason once whole"
        );
        fs::remove_file(&half).expect("removed");

        fs::write(
            handover.join("s-more.refused.json"),
            r#"{"session": "s-more", "why": "a lease", "at": "2026-11-15T16:00:02Z", "by": "x"}"#,
        )
        .expect("with more");
        assert_eq!(
            refusals(handover, began).expect("read"),
            since(),
            "one saying more than a refusal does isn't read as one"
        );

        fs::write(
            handover.join("s-odd.refused.json"),
            r#"{"session": "s-since", "why": "a lease", "at": "2026-11-15T16:00:02Z"}"#,
        )
        .expect("another's name");
        assert!(
            refusals(handover, began).is_err(),
            "one naming another session than its file stops the carry"
        );
    }

    #[test]
    fn a_session_is_seen_refusing_or_leaving_only_since_the_carry() {
        let handover = tempfile::tempdir().expect("a handover");
        let handover = handover.path();
        let before: jiff::Timestamp = "2026-11-15T16:00:00Z".parse().expect("a time");
        let refused = |handover: &Path, session: &str, since| {
            refused(handover, session, since, |error| {
                panic!("{session}'s refusal was read: {error}")
            })
        };
        assert_eq!(
            (
                refused(handover, "s-since", before),
                left(handover, "s-done", before)
            ),
            (None, false),
            "nothing written, nothing seen"
        );
        fs::write(
            handover.join("s-since.refused.json"),
            r#"{"session": "s-since", "why": "it holds a lease", "at": "2026-11-15T16:00:01Z"}"#,
        )
        .expect("a refusal");
        write_done(handover, "s-done");
        assert_eq!(
            refused(handover, "s-since", before),
            Some("it holds a lease".to_owned()),
            "its refusal since"
        );
        assert_eq!(
            refused(handover, "s-done", before),
            None,
            "another's refusal isn't its"
        );
        fs::write(
            handover.join("s-odd.refused.json"),
            r#"{"session": "s-since", "why": "a lease", "at": "2026-11-15T16:00:02Z"}"#,
        )
        .expect("another's name");
        let mut heard = None;
        assert_eq!(
            super::refused(handover, "s-odd", before, |error| heard = Some(error)).as_deref(),
            Some(UNREADABLE_REFUSAL),
            "one that can't be told as its own says so plainly, with no path"
        );
        let heard = heard.expect("the reader's error is handed on");
        assert!(
            heard.contains(&handover.join("s-odd.refused.json").display().to_string()),
            "the reader's error names the file, so it can be found: {heard}"
        );
        assert!(left(handover, "s-done", before), "its done file since");
        assert!(
            !left(handover, "s-since", before),
            "another's done file isn't its"
        );
        let later = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(60);
        assert_eq!(
            (
                refused(handover, "s-since", later),
                left(handover, "s-done", later)
            ),
            (None, false),
            "neither counts for a carry ordered after"
        );
    }

    #[test]
    fn each_unreadable_refusal_is_said_once() {
        let unreadable = Unreadable::default();
        assert!(unreadable.first("one"), "a new error is said");
        assert!(!unreadable.first("one"), "the same error isn't said again");
        assert!(unreadable.first("another"), "another error is said");
    }

    #[test]
    fn a_quests_handover_is_its_machines_folder_in_the_scratch() {
        assert_eq!(
            handover(
                Path::new("/scratch"),
                "QEST-20261115-HARBOR",
                "lantern.local"
            ),
            Path::new("/scratch/QEST-20261115-HARBOR/handover/lantern"),
            "by its short hostname"
        );
    }

    #[test]
    fn a_handover_goes_by_the_short_hostname() {
        assert_eq!(short_host("lantern.local"), "lantern", "a Mac's");
        assert_eq!(short_host("skiff"), "skiff", "one with no dot");
        assert_eq!(short_host(".odd"), ".odd", "never an empty name");
    }

    #[test]
    fn pi_carry_failing_says_why_from_its_own_lines() {
        let output = concat!(
            "{\"kind\":\"tree\",\"path\":\"~/src/harbour\"}\n",
            "{\"kind\":\"failed\",\"why\":\"~/src/harbour has changes no commit holds\"}\n"
        );
        assert_eq!(
            failed_why(output.as_bytes(), b"node: warning"),
            "~/src/harbour has changes no commit holds",
            "its failed line, not its errors"
        );
        assert_eq!(
            failed_why(b"not a line\n", b"node: not found\n"),
            "node: not found",
            "its errors where it said nothing of its own"
        );
    }
}
