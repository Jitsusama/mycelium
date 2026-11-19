//! The sessions a node starts, each written down, and an agent's held to
//! a number an hour. Cells start cells, so one gone wrong could start
//! them without end, each spending on the person's account: the node
//! refuses an agent's start past what one cell, and all of them together,
//! may start in an hour, and every one while the person has stopped them,
//! which takes one act. The person's own starts are never held back, and
//! nor is pi's first start in the hour bringing back a session that
//! ended, since it begins no new work.
//!
//! The book is kept in `starts.jsonl` beside the node's runs, a line a
//! start: `{"at", "by", "started_by", "folder", "program"}`, `"resumes"`
//! with the session's id when it brings one back uncounted, then where it
//! started (`"machine"` and `"held"`), or why it didn't (`"failed"`), or
//! why it was refused (`"refused"`). The person's stop is the file
//! `starts.stopped` beside it, there for as long as starts are stopped.

use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use crate::record::{Session, State};
use crate::wire::By;

/// The program whose sessions a start may bring back.
const PI: &str = "pi";

/// Where a pi named by its path is the person's own: folders only the
/// system's own build points, so no agent can put a program of its own
/// there named `pi`. The store is not one, since anyone may build into it.
const PIS_FOLDERS: [&str; 2] = ["/run/current-system/sw/bin/", "/etc/profiles/per-user/"];

/// The words before the session pi is to bring back.
const RESUMING: [&str; 2] = ["--session", "--session-id"];

/// The most sessions one cell may start in an hour.
pub const CELL_AN_HOUR: usize = 8;

/// The most sessions agents together may start through one node in an
/// hour.
pub const NODE_AN_HOUR: usize = 32;

/// An hour, in milliseconds.
const HOUR: i64 = 3_600_000;

/// The most starts held in memory, the newest, which reach back further
/// than an hour of starts within the limits. One that counts within the
/// hour is never let go for room, so asking past the limits pushes none
/// of them out.
const KEPT_MOST: usize = 2048;

/// How many bytes the book's file grows by, at least, before it is
/// written again from what memory holds, so asks refused without end fill
/// no disk; past that, it waits till the file has doubled since.
const WRITTEN_MOST: u64 = 8 * 1024 * 1024;

/// The most characters of why a start failed that the book keeps.
const FAILED_MOST: usize = 512;

/// The longest folder a start keeps, in bytes: a longer one is kept as
/// none. With [`PROGRAM_MOST`], it keeps the [`KEPT_MOST`] starts held
/// far under [`WRITTEN_MOST`], so a rewrite always leaves room to grow.
const FOLDER_MOST: usize = 1024;

/// The most characters of a program's name a start keeps.
const PROGRAM_MOST: usize = 128;

/// A start the node made, or was asked and refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    /// When it was asked, in milliseconds since the epoch.
    pub at: i64,
    /// Who asked it.
    pub by: By,
    /// The cell whose session asked, by its address, as the nearest
    /// session claiming the asker's process says; none for the person's,
    /// or an agent's no session claims. A claim proves nothing, so this
    /// says who it counted toward, not who it was.
    pub started_by: Option<String>,
    /// The folder it was asked to begin in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
    /// The program it starts, its first word alone, so no argument it was
    /// given is kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// The session it brings back, one the node knew had ended, as
    /// [`brings_back`] tells: the first such start in the hour counts
    /// toward agents' number through the node but not its cell's, since
    /// it begins no new work, so a wave brought back starts whole and no
    /// agent brings back more than the node's number uncounted. The
    /// person's stop still holds it. A second within the hour counts
    /// toward both, so bringing one back again and again is bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumes: Option<String>,
    /// The machine it started on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// Its held id there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<String>,
    /// Why it was asked but didn't start: the node's own reason, naming no
    /// argument.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
    /// Why the node refused it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

impl Kept {
    /// Whether it counts toward agents' number through the node: an
    /// agent's start the node let go ahead, started or not, since a start
    /// that fails can still have spent.
    fn counts(&self) -> bool {
        matches!(self.by, By::Agent(_)) && self.refused.is_none()
    }

    /// Whether it counts toward its cell's number too: one that counts
    /// and begins new work, rather than bringing a session back.
    fn counts_for_cell(&self) -> bool {
        self.counts() && self.resumes.is_none()
    }
}

/// The session of `sessions` that `command` brings back, if it is pi's
/// and names one that ended after `--session` or `--session-id`, by its
/// id or by a file whose name ends in `_` and it, as pi names a session's
/// file, and names no other. Only pi itself is read, named bare, as the
/// person's login finds it, or by a path in [`PIS_FOLDERS`], so no shell,
/// nor a program an agent wrote and named `pi` elsewhere, hides other
/// work behind the words; and only an ended session, so one running, or
/// on a machine out of reach, where it may still run, is never brought
/// back uncounted. A bare `pi` is fenced by habit alone, since the login
/// finds it on a path an agent may write: the node's number still bounds
/// what that hides.
#[must_use]
pub fn brings_back(command: &[String], sessions: &[Session]) -> Option<String> {
    let program = command.first()?;
    let named_pi = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        == Some(PI);
    let persons = program == PI
        || (PIS_FOLDERS.iter().any(|folder| program.starts_with(folder))
            && !program.contains("/../"));
    if !(named_pi && persons) {
        return None;
    }
    // In the command's order, as pi reads them; naming two is no resume,
    // since which pi opens can't be told here.
    let mut named: Vec<&str> = Vec::new();
    for (at, word) in command.iter().enumerate() {
        let said = if RESUMING.contains(&word.as_str()) {
            command.get(at + 1).map(String::as_str)
        } else {
            word.split_once('=')
                .and_then(|(flag, named)| RESUMING.contains(&flag).then_some(named))
        };
        if let Some(said) = said
            && !named.contains(&said)
        {
            named.push(said);
        }
    }
    let [named] = named[..] else {
        return None;
    };
    let stem = Path::new(named)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    sessions
        .iter()
        .find(|session| {
            let id = session.id.as_str();
            let is_it = !id.is_empty()
                && (named == id
                    || stem
                        .strip_suffix(id)
                        .is_some_and(|before| before.ends_with('_')));
            is_it && session.state == State::Ended
        })
        .map(|session| session.id.clone())
}

/// A start let go ahead, held as counted till it is kept.
#[derive(Debug)]
#[must_use = "a start let go ahead is kept once it ends"]
pub struct Admitted {
    id: u64,
    asked: Kept,
}

/// The node's book of starts.
#[derive(Debug)]
pub struct Starts {
    file: PathBuf,
    stopped: PathBuf,
    book: Mutex<Book>,
}

#[derive(Debug, Default)]
struct Book {
    kept: VecDeque<Kept>,
    going: Vec<(u64, Kept)>,
    next: u64,
    /// The bytes its file has grown by since it was last written whole.
    written: u64,
    /// The bytes it was last written whole in: it is written whole again
    /// only once it has grown past twice that, so a book whose held starts
    /// weigh more than [`WRITTEN_MOST`], escaped lines and all, is never
    /// rewritten on every ask.
    whole: u64,
}

impl Book {
    /// Holds `start`, letting go of the oldest refused start for room,
    /// else the oldest that no longer counts within the hour, else none.
    fn hold(&mut self, start: Kept) {
        let since = start.at - HOUR;
        let ahead = start.at + HOUR;
        self.kept.push_back(start);
        while self.kept.len() > KEPT_MOST {
            let oldest = self
                .kept
                .iter()
                .position(|one| one.refused.is_some())
                .or_else(|| {
                    self.kept
                        .iter()
                        // As the count reads them: one more than an hour
                        // ahead counts no more than one long gone. An
                        // agent's resume in the hour counts, and is kept,
                        // since a second of its session counts more.
                        .position(|one| !one.counts() || one.at <= since || one.at > ahead)
                });
            match oldest {
                Some(at) => drop(self.kept.remove(at)),
                None => break,
            }
        }
    }
}

impl Starts {
    /// The book kept in `file`, read back as far as [`KEPT_MOST`] starts,
    /// a line it can't read passed over alone.
    #[must_use]
    pub fn at(file: PathBuf) -> Self {
        let mut book = Book::default();
        if let Ok(read) = std::fs::File::open(&file) {
            book.written = read.metadata().map_or(0, |meta| meta.len());
            // By bytes, so a line that isn't UTF-8 is passed over alone
            // rather than ending the read and every start after it.
            for line in std::io::BufReader::new(read).split(b'\n') {
                let Ok(line) = line else { break };
                if let Ok(start) = serde_json::from_slice::<Kept>(&line) {
                    book.hold(start);
                }
            }
        }
        Self {
            stopped: file.with_file_name("starts.stopped"),
            file,
            book: Mutex::new(book),
        }
    }

    /// Whether the person has stopped agents' starts: also while whether
    /// they have can't be told, so a stop never lifts for a fault.
    #[must_use]
    pub fn stopped(&self) -> bool {
        self.stop_stands().unwrap_or(true)
    }

    /// Whether the person's stop stands, or why that can't be told.
    fn stop_stands(&self) -> std::io::Result<bool> {
        match self.stopped.symlink_metadata() {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Stops every agent's start till the person resumes them.
    ///
    /// # Errors
    ///
    /// When the stop can't be written.
    pub fn stop(&self) -> std::io::Result<()> {
        std::fs::write(&self.stopped, "")
    }

    /// Lets agents start again, within their limits.
    ///
    /// # Errors
    ///
    /// When the stop is there and can't be taken away.
    pub fn resume(&self) -> std::io::Result<()> {
        match std::fs::remove_file(&self.stopped) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Lets `asked` go ahead, counting it till it is kept, or refuses it,
    /// keeping why: an agent's while the person has stopped starts, or
    /// once its cell, or agents together, have started their most in the
    /// hour before it.
    ///
    /// # Errors
    ///
    /// Why it is refused.
    pub fn admit(&self, mut asked: Kept) -> Result<Admitted, String> {
        // What an agent chose is held to a bound, so its asks, refused
        // ones too, can't grow the book past what starts would.
        asked.folder = asked
            .folder
            .filter(|folder| folder.as_os_str().len() <= FOLDER_MOST);
        asked.program = asked
            .program
            .map(|program| program.chars().take(PROGRAM_MOST).collect());
        let refused = {
            let mut book = self.lock();
            Self::once_an_hour(&book, &mut asked);
            let refused = self.refusal(&book, &asked).map(|(why, _)| why);
            if refused.is_none() {
                let id = book.next;
                book.next += 1;
                book.going.push((id, asked.clone()));
                return Ok(Admitted { id, asked });
            }
            refused
        };
        asked.refused.clone_from(&refused);
        self.keep_line(&mut self.lock(), asked);
        Err(refused.unwrap_or_default())
    }

    /// When the limits that refuse `asked` would let it, in milliseconds
    /// since the epoch, or none while they let it or the person's stop
    /// holds it.
    #[must_use]
    pub fn room(&self, asked: &Kept) -> Option<i64> {
        let book = self.lock();
        let mut asked = asked.clone();
        Self::once_an_hour(&book, &mut asked);
        self.refusal(&book, &asked).and_then(|(_, room)| room)
    }

    /// Counts `asked` after all where it brings back a session the book
    /// already shows brought back uncounted in the hour before it, going
    /// or kept, so a burst of starts of one ended session counts from the
    /// second on, before the first has run long enough to say so.
    fn once_an_hour(book: &Book, asked: &mut Kept) {
        let since = asked.at - HOUR;
        let again = asked.resumes.as_ref().is_some_and(|session| {
            book.kept
                .iter()
                .chain(book.going.iter().map(|(_, going)| going))
                .any(|one| {
                    matches!(one.by, By::Agent(_))
                        && one.refused.is_none()
                        && one.at > since
                        && one.resumes.as_ref() == Some(session)
                })
        });
        if again {
            asked.resumes = None;
        }
    }

    /// Why `asked` is refused, if it is, and when the limits that refuse
    /// it would let it, where it is they that do.
    fn refusal(&self, book: &Book, asked: &Kept) -> Option<(String, Option<i64>)> {
        if !matches!(asked.by, By::Agent(_)) {
            return None;
        }
        match self.stop_stands() {
            Ok(false) => {}
            Ok(true) => {
                return Some((
                    "the person stopped agents starting sessions, and only they resume it, \
                     with `mycelium starts resume`; don't ask them to, but tell your \
                     orchestrator, as a finding"
                        .to_owned(),
                    None,
                ));
            }
            Err(error) => {
                return Some((
                    format!(
                        "agents' starts are held, since whether the person stopped them \
                         can't be told from {}: {error}",
                        self.stopped.display()
                    ),
                    None,
                ));
            }
        }
        if !asked.counts() {
            return None;
        }
        let (why, room) = Self::past_limits(book, asked)?;
        Some((paced(&why, room, asked.at), Some(room)))
    }

    /// Which limit `asked` would pass, said, and when it would no longer.
    fn past_limits(book: &Book, asked: &Kept) -> Option<(String, i64)> {
        let since = asked.at - HOUR;
        let counted: Vec<&Kept> = book
            .kept
            .iter()
            .chain(book.going.iter().map(|(_, going)| going))
            // One dated up to an hour ahead of the ask, as a clock set
            // back leaves it, still counts, so a step back hands no cell a
            // fresh hour; one further ahead, as a clock run far ahead left
            // it, counts no more than one long gone, so it blocks no hour.
            .filter(|one| one.counts() && one.at > since && one.at <= asked.at + HOUR)
            .collect();
        let of_cell: Vec<i64> = counted
            .iter()
            .filter(|one| one.counts_for_cell() && one.started_by == asked.started_by)
            .map(|one| one.at)
            .collect();
        let all: Vec<i64> = counted.iter().map(|one| one.at).collect();
        // Room comes once enough of the oldest have passed the hour, the
        // later of the cell's and the node's where both are past; a
        // session brought back is held to the node's alone.
        let cell_room =
            room_after(of_cell.clone(), CELL_AN_HOUR).filter(|_| asked.counts_for_cell());
        let node_room = room_after(all.clone(), NODE_AN_HOUR);
        let room = cell_room.max(node_room)?;
        let cell = asked
            .started_by
            .as_deref()
            .map_or_else(|| "agents of no known cell".to_owned(), str::to_owned);
        // The limit said is the one whose room comes last.
        let why = if cell_room >= node_room {
            format!(
                "{cell} started {} sessions in the last hour, and a cell starts at most \
                 {CELL_AN_HOUR} an hour",
                of_cell.len()
            )
        } else {
            format!(
                "agents started {} sessions through this node in the last hour, and they start \
                 at most {NODE_AN_HOUR} an hour",
                all.len()
            )
        };
        Some((why, room))
    }

    /// Keeps how `admitted` ended: where it started, or why it didn't, as
    /// [`failure`] tells it.
    pub fn keep(&self, admitted: Admitted, ended: Result<(String, String), String>) {
        let Admitted { id, mut asked } = admitted;
        match ended {
            Ok((machine, held)) => {
                asked.machine = Some(machine);
                asked.held = Some(held);
            }
            Err(why) => asked.failed = Some(failure(&why)),
        }
        // Going and kept under one lock, so a start asked meanwhile never
        // sees it in neither.
        let mut book = self.lock();
        book.going.retain(|(going, _)| *going != id);
        self.keep_line(&mut book, asked);
    }

    /// Writes `start` in the book, in one write so two kept at once never
    /// interleave, and holds it in `book`; once the file has grown by
    /// [`WRITTEN_MOST`], writes it again whole from what `book` holds.
    fn keep_line(&self, book: &mut Book, start: Kept) {
        let written = serde_json::to_string(&start)
            .map_err(std::io::Error::other)
            .and_then(|line| {
                let line = format!("{line}\n");
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.file)?
                    .write_all(line.as_bytes())?;
                Ok(line.len() as u64)
            });
        match written {
            Ok(bytes) => book.written += bytes,
            Err(error) => eprintln!("a start isn't kept in {}: {error}", self.file.display()),
        }
        book.hold(start);
        if book.written > WRITTEN_MOST.max(book.whole.saturating_mul(2)) {
            match self.write_whole(book) {
                Ok(bytes) => {
                    book.written = bytes;
                    book.whole = bytes;
                }
                Err(error) => {
                    // Tried again only once it doubles again, so a full
                    // disk isn't rewritten on every ask, and no copy is
                    // left beside it.
                    book.whole = book.written;
                    let _ = std::fs::remove_file(self.file.with_extension("jsonl.new"));
                    eprintln!(
                        "the starts in {} aren't written again: {error}",
                        self.file.display()
                    );
                }
            }
        }
    }

    /// Writes the book's file again as `book` holds it, through a file
    /// beside it, so a stop part way leaves the old one whole.
    fn write_whole(&self, book: &Book) -> std::io::Result<u64> {
        let mut text = String::new();
        for start in &book.kept {
            text.push_str(&serde_json::to_string(start).map_err(std::io::Error::other)?);
            text.push('\n');
        }
        let beside = self.file.with_extension("jsonl.new");
        std::fs::write(&beside, &text)?;
        std::fs::rename(&beside, &self.file)?;
        Ok(text.len() as u64)
    }

    /// The cell that started the session held as `held` on `machine`, as
    /// this node's book says, if this node started it for one.
    #[must_use]
    pub fn started_by(&self, machine: &str, held: &str) -> Option<String> {
        self.lock()
            .kept
            .iter()
            .rev()
            .find(|one| {
                one.machine.as_deref() == Some(machine) && one.held.as_deref() == Some(held)
            })
            .and_then(|one| one.started_by.clone())
    }

    /// The newest `most` starts, newest last.
    #[must_use]
    pub fn newest(&self, most: usize) -> Vec<Kept> {
        let book = self.lock();
        book.kept
            .iter()
            .skip(book.kept.len().saturating_sub(most))
            .cloned()
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, Book> {
        // A panic while holding the lock leaves the book as it was before
        // it, so carrying on with it is sound.
        self.book.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// When starts dated `ats` leave room under `most` an hour: once the
/// oldest past `most`, one fewer, have passed the hour; none while they
/// leave room already.
fn room_after(mut ats: Vec<i64>, most: usize) -> Option<i64> {
    let past = ats.len().checked_sub(most)?;
    ats.sort_unstable();
    Some(ats[past] + HOUR)
}

/// `why` a start is refused, said with when there is room again, `room`,
/// as from `at`, and what an agent does then: it paces agents, so the
/// person is no one to ask.
fn paced(why: &str, room: i64, at: i64) -> String {
    let minutes = match u64::try_from(room - at).unwrap_or(0).div_ceil(60_000) {
        1 => "1 minute".to_owned(),
        minutes => format!("{minutes} minutes"),
    };
    let clock = jiff::Timestamp::from_millisecond(room)
        .map(|room| {
            room.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%H:%M")
                .to_string()
        })
        .unwrap_or_default();
    format!(
        "{why}; there is room again in {minutes}, at {clock}, so ask again then. \
         It paces agents and isn't the person's to lift: tell your orchestrator, as a finding"
    )
}

/// Where a node whose runs are kept in `runs` keeps its starts: beside
/// them.
#[must_use]
pub fn beside(runs: &Path) -> PathBuf {
    runs.with_file_name("starts.jsonl")
}

/// Why a start failed, as the book keeps it: the node's own reason alone,
/// before the first `: `, past which a holder's words name the whole
/// command, and no more than [`FAILED_MOST`] characters. The asker is
/// told the whole of it; the book keeps no argument rather than more.
fn failure(why: &str) -> String {
    why.split(": ")
        .next()
        .unwrap_or_default()
        .chars()
        .take(FAILED_MOST)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::declared::Kind;

    const NOW: i64 = 1_800_000_000_000;

    #[test]
    fn a_failure_keeps_the_nodes_reason_and_no_argument() {
        let (dir, starts) = book();
        let admitted = starts.admit(agent("OSPREY", NOW)).expect("within");
        let why = "it didn't start: couldn't hold pi --said-once: gone";
        starts.keep(admitted, Err(why.to_owned()));
        let book = std::fs::read_to_string(dir.path().join("starts.jsonl")).expect("kept");
        assert!(book.contains(r#""failed":"it didn't start""#), "{book}");
        assert!(!book.contains("--said-once"), "{book}");
        let long = "x".repeat(FAILED_MOST * 2);
        assert_eq!(failure(&long).chars().count(), FAILED_MOST);
    }

    #[test]
    fn a_failed_rewrite_waits_for_the_next_doubling_and_leaves_no_copy() {
        let (dir, starts) = book();
        let beside = dir.path().join("starts.jsonl.new");
        // A folder where the book goes takes no copy renamed onto it.
        std::fs::create_dir(dir.path().join("starts.jsonl")).expect("a folder");
        started(&starts, agent("OSPREY", NOW), "h-1");
        starts.lock().written = WRITTEN_MOST + 1;
        started(&starts, agent("OSPREY", NOW), "h-2");
        let book = starts.lock();
        assert_eq!(
            book.whole, book.written,
            "the next try waits for a doubling"
        );
        drop(book);
        assert!(!beside.exists(), "no copy is left beside the book");
    }

    #[test]
    fn a_start_dated_far_ahead_is_let_go_before_one_that_counts() {
        let (_dir, starts) = book();
        let mut book = starts.lock();
        book.hold(agent("OSPREY", NOW + 2 * HOUR));
        for one in 1..KEPT_MOST {
            book.hold(agent("OSPREY", NOW + i64::try_from(one).expect("few")));
        }
        book.hold(agent("TERN", NOW + 5_000));
        assert_eq!(book.kept.len(), KEPT_MOST);
        assert!(
            book.kept.iter().all(|one| one.at < NOW + HOUR),
            "the far one went"
        );
    }

    fn asked(by: By, cell: Option<&str>, at: i64) -> Kept {
        Kept {
            at,
            by,
            started_by: cell.map(str::to_owned),
            folder: None,
            program: Some("pi".to_owned()),
            resumes: None,
            machine: None,
            held: None,
            failed: None,
            refused: None,
        }
    }

    fn resuming(cell: &str, session: &str, at: i64) -> Kept {
        Kept {
            resumes: Some(session.to_owned()),
            ..agent(cell, at)
        }
    }

    fn session(id: &str, state: &str) -> Session {
        serde_json::from_value(serde_json::json!({
            "id": id, "address": "OSPREY", "machine": "kestrel", "groups": [],
            "state": state, "at": "2026-11-18T23:00:00Z",
        }))
        .expect("a session")
    }

    #[test]
    fn a_wave_of_sessions_brought_back_all_start_though_the_persons_stop_holds_them() {
        let (_dir, starts) = book();
        for one in 0..=CELL_AN_HOUR {
            let at = NOW + i64::try_from(one).expect("few");
            started(&starts, resuming("OSPREY", &format!("s-{one}"), at), "h");
        }
        for _ in 1..CELL_AN_HOUR {
            started(&starts, agent("OSPREY", NOW + 10), "h");
        }
        // Bringing one back again within the hour counts, as new work.
        started(&starts, resuming("OSPREY", "s-0", NOW + 10), "h");
        starts
            .admit(agent("OSPREY", NOW + 10))
            .expect_err("new work still counts");
        starts
            .admit(resuming("OSPREY", "s-1", NOW + 10))
            .expect_err("and so does bringing one back again");
        started(&starts, resuming("OSPREY", "s-new", NOW + 10), "h");
        starts.stop().expect("stopped");
        starts
            .admit(resuming("OSPREY", "s-else", NOW + 10))
            .expect_err("the person's stop holds every agent's start");
    }

    #[test]
    fn the_book_keeps_an_agents_resume_in_the_hour_and_lets_the_persons_go() {
        let (_dir, starts) = book();
        let persons = Kept {
            resumes: Some("s-1".to_owned()),
            ..asked(By::Person, None, NOW)
        };
        {
            let mut book = starts.lock();
            book.hold(resuming("OSPREY", "s-0", NOW));
            book.hold(persons.clone());
            for _ in 1..KEPT_MOST {
                book.hold(asked(By::Person, None, NOW + 1));
            }
            assert!(!book.kept.contains(&persons), "the person's went first");
        }
        for _ in 0..CELL_AN_HOUR {
            started(&starts, agent("OSPREY", NOW + 2), "h");
        }
        starts
            .admit(resuming("OSPREY", "s-0", NOW + 3))
            .expect_err("the agent's was kept, so a second counts");
    }

    #[test]
    fn a_resume_the_persons_stop_held_leaves_the_next_free() {
        let (_dir, starts) = book();
        starts.stop().expect("stopped");
        starts
            .admit(resuming("OSPREY", "s-0", NOW))
            .expect_err("held");
        starts.resume().expect("resumed");
        for _ in 0..CELL_AN_HOUR {
            started(&starts, agent("OSPREY", NOW + 1), "h");
        }
        started(&starts, resuming("OSPREY", "s-0", NOW + 2), "h");
    }

    #[test]
    fn sessions_brought_back_count_toward_the_nodes_number_but_not_their_cells() {
        let (_dir, starts) = book();
        for one in 0..NODE_AN_HOUR {
            let at = NOW + i64::try_from(one).expect("few");
            started(&starts, resuming("OSPREY", &format!("s-{one}"), at), "h");
        }
        let refused = starts
            .admit(resuming("OSPREY", "s-last", NOW + 100))
            .expect_err("past the node's number");
        assert!(refused.contains("through this node"), "{refused}");
        assert!(
            refused.ends_with("tell your orchestrator, as a finding"),
            "{refused}"
        );
        assert_eq!(
            starts.room(&resuming("OSPREY", "s-last", NOW + 100)),
            Some(NOW + HOUR),
            "room comes as the first brought back passes the hour"
        );
        starts
            .admit(agent("SKIFFS", NOW + 100))
            .expect_err("and new work of another cell waits with them");
    }

    #[test]
    fn a_start_names_a_session_brought_back_by_its_id_or_its_file_while_it_doesnt_run() {
        let words = |words: &[&str]| {
            words
                .iter()
                .map(|&word| word.to_owned())
                .collect::<Vec<_>>()
        };
        let sessions = [
            session("0193-ended", "ended"),
            session("0193-busy", "busy"),
            session("0193-away", "unreachable"),
        ];
        let file = "/home/x/.pi/agent/sessions/--x--/2026-11-18T22-00-00_0193-ended.jsonl";
        for brought in [
            words(&["pi", "--session", "0193-ended"]),
            words(&["/run/current-system/sw/bin/pi", "--session", file]),
            words(&["pi", "--session=0193-ended"]),
            words(&["pi", "--session-id", "0193-ended"]),
            words(&["/etc/profiles/per-user/x/bin/pi", "--session-id=0193-ended"]),
        ] {
            assert_eq!(
                brings_back(&brought, &sessions).as_deref(),
                Some("0193-ended"),
                "{brought:?}"
            );
        }
        for new in [
            words(&["pi", "--session", "0193-busy"]),
            words(&["pi", "--session", "0193-away"]),
            words(&["pi", "--session", "0193-unknown"]),
            words(&["pi", "--session", "x0193-ended"]),
            words(&["pi", "0193-ended"]),
            words(&["pi", "--session", "0193-busy", "--session", "0193-ended"]),
            words(&["pi", "--session=0193-new", "--session-id", "0193-ended"]),
            words(&["sh", "-c", "new work", "--session", "0193-ended"]),
            words(&["pip", "--session", "0193-ended"]),
            words(&["/home/x/work/pi", "--session", "0193-ended"]),
            words(&["./pi", "--session", "0193-ended"]),
            words(&["/nix/store/0000-not-pi/bin/pi", "--session", "0193-ended"]),
            words(&[
                "/run/current-system/sw/bin/../../../../tmp/pi",
                "--session",
                "0193-ended",
            ]),
        ] {
            assert_eq!(brings_back(&new, &sessions), None, "{new:?}");
        }
    }

    #[test]
    fn a_session_brought_back_twice_at_once_counts_the_second_and_its_room_says_so() {
        let (_dir, starts) = book();
        for _ in 0..CELL_AN_HOUR {
            started(&starts, agent("OSPREY", NOW), "h");
        }
        // The first still going, as a burst of the same one is.
        let first = starts
            .admit(resuming("OSPREY", "s-0", NOW + 1))
            .expect("the first is free");
        assert_eq!(
            starts.room(&resuming("OSPREY", "s-0", NOW + 2)),
            Some(NOW + HOUR),
            "the second waits for a counted start's room"
        );
        starts
            .admit(resuming("OSPREY", "s-0", NOW + 2))
            .expect_err("the second counts");
        assert_eq!(
            starts.room(&resuming("OSPREY", "s-1", NOW + 2)),
            None,
            "another session is still free"
        );
        starts.keep(first, Ok(("kestrel".to_owned(), "h".to_owned())));
        starts
            .admit(resuming("OSPREY", "s-0", NOW + 3))
            .expect_err("and counts once the first has started");
        let person = Kept {
            resumes: Some("s-2".to_owned()),
            ..asked(By::Person, None, NOW + 3)
        };
        started(&starts, person, "h");
        started(&starts, resuming("OSPREY", "s-2", NOW + 4), "h");
    }

    #[test]
    fn a_start_past_the_limits_says_when_there_is_room_and_not_to_ask_the_person() {
        let (_dir, starts) = book();
        for one in 0..CELL_AN_HOUR {
            let at = NOW + 60_000 * i64::try_from(one).expect("few");
            started(&starts, agent("OSPREY", at), "h");
        }
        let late = agent("OSPREY", NOW + 30 * 60_000);
        assert_eq!(
            starts.room(&late),
            Some(NOW + HOUR),
            "once the oldest is an hour old"
        );
        let refused = starts.admit(late).expect_err("past its number");
        assert!(refused.contains("room again in 30 minutes"), "{refused}");
        assert!(
            refused.ends_with("tell your orchestrator, as a finding"),
            "{refused}"
        );
        assert_eq!(
            starts.room(&agent("SKIFFS", NOW)),
            None,
            "another cell has room"
        );
        starts.stop().expect("stopped");
        assert_eq!(
            starts.room(&agent("SKIFFS", NOW)),
            None,
            "the stop names no time"
        );
    }

    fn agent(cell: &str, at: i64) -> Kept {
        asked(By::Agent(Kind::Fence), Some(cell), at)
    }

    fn book() -> (tempfile::TempDir, Starts) {
        let dir = tempfile::tempdir().expect("a folder");
        let starts = Starts::at(dir.path().join("starts.jsonl"));
        (dir, starts)
    }

    fn started(starts: &Starts, asked: Kept, held: &str) {
        let admitted = starts.admit(asked).expect("it may start");
        starts.keep(admitted, Ok(("kestrel".to_owned(), held.to_owned())));
    }

    #[test]
    fn a_cell_starts_at_most_its_number_an_hour_and_again_once_the_hour_has_passed() {
        let (_dir, starts) = book();
        for one in 0..CELL_AN_HOUR {
            started(
                &starts,
                agent("OSPREY", NOW + i64::try_from(one).expect("few")),
                "h",
            );
        }
        let refused = starts
            .admit(agent("OSPREY", NOW + 100))
            .expect_err("past its number");
        assert!(refused.contains("OSPREY started 8"), "{refused}");
        started(&starts, agent("SKIFFS", NOW + 100), "h");
        started(&starts, agent("OSPREY", NOW + HOUR + 10), "h");
    }

    #[test]
    fn agents_together_start_at_most_the_nodes_number_an_hour() {
        let (_dir, starts) = book();
        for one in 0..NODE_AN_HOUR {
            let cell = format!("CELL{:02}", one / CELL_AN_HOUR);
            started(&starts, agent(&cell, NOW), "h");
        }
        let refused = starts
            .admit(agent("TERNSS", NOW))
            .expect_err("past the node's");
        assert!(refused.contains("through this node"), "{refused}");
        started(&starts, asked(By::Person, None, NOW), "h");
    }

    #[test]
    fn a_start_still_going_counts_and_one_refused_or_the_persons_never_does() {
        let (_dir, starts) = book();
        let going: Vec<Admitted> = (0..CELL_AN_HOUR)
            .map(|_| starts.admit(agent("OSPREY", NOW)).expect("within"))
            .collect();
        starts
            .admit(agent("OSPREY", NOW))
            .expect_err("those going count");
        for one in going {
            starts.keep(one, Err("it didn't start".to_owned()));
        }
        starts
            .admit(agent("OSPREY", NOW))
            .expect_err("those that failed count");
        for _ in 0..NODE_AN_HOUR {
            started(&starts, asked(By::Person, None, NOW), "h");
        }
        started(&starts, agent("SKIFFS", NOW), "h");
    }

    #[test]
    fn a_start_dated_ahead_counts_for_an_hour_ahead_and_no_further() {
        let (_dir, starts) = book();
        for _ in 0..CELL_AN_HOUR {
            started(&starts, agent("OSPREY", NOW + HOUR), "h");
            started(&starts, agent("SKIFFS", NOW + HOUR + 1), "h");
        }
        starts
            .admit(agent("OSPREY", NOW))
            .expect_err("a clock stepped back hands no cell a fresh hour");
        // SKIFFS's, dated past an hour ahead, block no hour.

        started(&starts, agent("SKIFFS", NOW), "h");
    }

    #[test]
    fn the_persons_stop_refuses_every_agents_start_till_they_resume() {
        let (_dir, starts) = book();
        starts.stop().expect("stopped");
        let refused = starts.admit(agent("OSPREY", NOW)).expect_err("stopped");
        assert!(refused.contains("mycelium starts resume"), "{refused}");
        started(&starts, asked(By::Person, None, NOW), "h");
        starts.resume().expect("resumed");
        starts.resume().expect("resuming twice is no harm");
        started(&starts, agent("OSPREY", NOW), "h");
    }

    #[test]
    fn the_book_is_written_whole_only_once_it_has_doubled() {
        let (dir, starts) = book();
        let file = dir.path().join("starts.jsonl");
        started(&starts, agent("OSPREY", NOW), "h-1");
        let mut written = std::fs::read(&file).expect("the book");
        written.extend(b"left over\n");
        std::fs::write(&file, written).expect("a line a rewrite drops");
        {
            let mut book = starts.lock();
            book.written = WRITTEN_MOST + 1;
            book.whole = WRITTEN_MOST;
        }
        started(&starts, agent("OSPREY", NOW), "h-2");
        let read = || std::fs::read_to_string(&file).expect("the book");
        assert!(read().contains("left over"), "past 8 MiB, not yet doubled");
        starts.lock().written = 2 * WRITTEN_MOST + 1;
        started(&starts, agent("OSPREY", NOW), "h-3");
        assert!(!read().contains("left over"), "doubled, so written whole");
        let book = starts.lock();
        assert_eq!(
            book.whole, book.written,
            "the next bound reads from this one"
        );
        drop(book);
        assert_eq!(read().lines().count(), 3, "{}", read());
    }

    #[test]
    fn every_start_is_written_down_and_read_back_past_a_line_it_cant_read() {
        let (dir, starts) = book();
        started(&starts, agent("OSPREY", NOW), "h-1");
        starts.stop().expect("stopped");
        starts.admit(agent("OSPREY", NOW)).expect_err("stopped");
        let file = dir.path().join("starts.jsonl");
        // A line that isn't UTF-8 first, then one that isn't a start:
        // each is passed over alone.
        let mut written = b"\xff\xfe\n".to_vec();
        written.extend(std::fs::read(&file).expect("the book"));
        written.extend(b"not a start\n");
        std::fs::write(&file, written).expect("lines it can't read");
        let again = Starts::at(file);
        let kept = again.newest(10);
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert_eq!(kept[0].held.as_deref(), Some("h-1"));
        assert!(kept[1].refused.is_some(), "{kept:?}");
        assert_eq!(
            again.started_by("kestrel", "h-1").as_deref(),
            Some("OSPREY")
        );
        assert_eq!(again.started_by("osprey", "h-1"), None, "another machine's");
        assert!(again.stopped(), "the stop outlives the node");
    }

    #[test]
    fn asks_refused_without_end_count_for_nothing_and_push_no_counted_start_out() {
        let (dir, starts) = book();
        starts.stop().expect("stopped");
        for _ in 0..CELL_AN_HOUR {
            starts.admit(agent("OSPREY", NOW)).expect_err("stopped");
        }
        starts.resume().expect("resumed");
        for _ in 0..CELL_AN_HOUR {
            started(&starts, agent("OSPREY", NOW), "h");
        }
        for _ in 0..=KEPT_MOST {
            starts
                .admit(agent("OSPREY", NOW + 1))
                .expect_err("past its number");
        }
        starts
            .admit(agent("OSPREY", NOW + 2))
            .expect_err("its counted starts are all still held");
        let again = Starts::at(dir.path().join("starts.jsonl"));
        again
            .admit(agent("OSPREY", NOW + 3))
            .expect_err("and read back so after a restart");
        started(&again, agent("SKIFFS", NOW + 3), "h");
    }

    #[test]
    fn a_book_grown_past_its_most_is_written_again_from_what_it_holds() {
        let (dir, starts) = book();
        started(&starts, agent("OSPREY", NOW), "h-1");
        starts.lock().written = WRITTEN_MOST;
        started(&starts, agent("OSPREY", NOW), "h-2");
        let file = dir.path().join("starts.jsonl");
        let written = std::fs::read_to_string(&file).expect("the book");
        assert_eq!(written.lines().count(), 2, "{written}");
        assert_eq!(
            starts.lock().written,
            written.len() as u64,
            "it counts from the file written again"
        );
        assert_eq!(
            Starts::at(file).started_by("kestrel", "h-2").as_deref(),
            Some("OSPREY")
        );
    }

    #[test]
    fn a_stop_that_cant_be_looked_at_holds_agents_back() {
        let dir = tempfile::tempdir().expect("a folder");
        let not_a_folder = dir.path().join("state");
        std::fs::write(&not_a_folder, "").expect("a file");
        let starts = Starts::at(not_a_folder.join("starts.jsonl"));
        assert!(starts.stopped(), "a stop that can't be told is a stop");
        let refused = starts.admit(agent("OSPREY", NOW)).expect_err("held back");
        assert!(
            refused.starts_with("agents' starts are held, since whether"),
            "it says the stop can't be told, not that the person stopped: {refused}"
        );
    }

    #[test]
    fn what_an_agent_names_is_kept_within_a_bound() {
        let (_dir, starts) = book();
        let mut asked = agent("OSPREY", NOW);
        asked.folder = Some(PathBuf::from("f".repeat(FOLDER_MOST + 1)));
        asked.program = Some("p".repeat(PROGRAM_MOST + 1));
        started(&starts, asked, "h-1");
        let kept = starts.newest(1);
        assert_eq!(
            kept[0].folder, None,
            "a folder past the bound is kept as none"
        );
        assert_eq!(
            kept[0].program.as_deref().map(str::len),
            Some(PROGRAM_MOST),
            "a program is cut at the bound"
        );
    }

    #[test]
    fn a_line_keeps_the_program_alone_and_never_its_arguments() {
        let line = serde_json::to_value(agent("OSPREY", NOW)).expect("a line");
        assert_eq!(
            line,
            serde_json::json!({
                "at": NOW,
                "by": {"Agent": "fence"},
                "started_by": "OSPREY",
                "program": "pi",
            })
        );
    }
}
