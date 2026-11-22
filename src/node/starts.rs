//! The sessions a node starts, each written down, and an agent's held to
//! a number an hour. Cells start cells, so one gone wrong could start
//! them without end, each spending on the person's account: the node
//! refuses an agent's start past what one cell, and all of them together,
//! may start in an hour, and every one while the person has stopped them,
//! which takes one act. The person's own starts are never held back, and
//! nor is pi's first start in the hour bringing back a session that
//! ended, since it begins no new work, up to [`RESUMED_AN_HOUR`] through
//! the node.
//!
//! The book is kept in `starts.jsonl` beside the node's runs, a line a
//! start: `{"at", "by", "started_by", "folder", "program"}`, `"resumes"`
//! with the session's id when it brings one back uncounted, `"reopens"`
//! with it when it brings one back counted or not, then where it
//! started (`"machine"` and `"held"`), or why it didn't (`"failed"`), or
//! why it was refused (`"refused"`). The person's stop is the file
//! `starts.stopped` beside it, there for as long as starts are stopped.

use std::collections::{BTreeMap, VecDeque};
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

/// The word before the session pi is to bring back. Never
/// `--session-id`, with which pi, finding no session of that id, begins
/// a new one under it, so a presence an agent wrote could make new work
/// look like a session brought back.
const RESUMING: [&str; 1] = ["--session"];

/// The most sessions one cell may start in an hour.
pub const CELL_AN_HOUR: usize = 8;

/// The most sessions agents together may start through one node in an
/// hour.
pub const NODE_AN_HOUR: usize = 32;

/// What a refusal of an agent's start, while the person's stop stands,
/// ends by telling it to do: only the person lifts it, and an agent is
/// to ask no one but its orchestrator.
pub const STOPPED_FOR_AN_AGENT: &str =
    "don't ask them to, but tell your orchestrator, as a finding";

/// What a refusal of an agent's start past the limits ends by telling
/// it to do: the limits pace agents, so the person is no one to ask.
pub const PACED_FOR_AN_AGENT: &str =
    "It paces agents and isn't the person's to lift: tell your orchestrator, as a finding";

/// The most ended sessions agents together may bring back through one
/// node in an hour uncounted: past it, each counts as new work. A
/// machine's sessions coming back after a restart fit within it, and an
/// agent that ends the sessions it starts to bring them back free gains
/// at most this many an hour beside the [`NODE_AN_HOUR`].
pub const RESUMED_AN_HOUR: usize = 128;

/// How long after a start that brings a session back a second is
/// refused, in milliseconds: the first may still be on its way, its pi
/// not yet saying it runs, and a second would open the same log beside
/// it. One still going refuses a second however long it takes, and one
/// that failed, having brought nothing back, refuses none.
pub const COMING_BACK: i64 = 120_000;

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
    /// session claiming the asker's process says, or on a pi host the
    /// claim its `PI_SESSION_ID` names; none for the person's, or an
    /// agent's no session claims. A claim proves nothing, so this
    /// says who it counted toward, not who it was.
    pub started_by: Option<String>,
    /// The folder it was asked to begin in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
    /// The program it starts, its first word alone, so no argument it was
    /// given is kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// The session it brings back, one the node knew had ended, on the
    /// machine it ran on, as [`brings_back`] tells: the first such start
    /// in the hour counts toward no number, since it begins no new work,
    /// so every session a restarted machine held comes back at once. The
    /// person's stop still holds it. A second within the hour counts as
    /// new work, so bringing one back again and again is bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumes: Option<String>,
    /// The session it brings back, counted or not, as [`brings_back`]
    /// tells, so a second start of it within [`COMING_BACK`] is refused
    /// rather than opening its log beside the first's, however the first
    /// was counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reopens: Option<String>,
    /// The session it starts where a carry its cell ordered from here
    /// took it, as the node holds it to the carry it kept: like
    /// [`Kept::resumes`], the first in the hour counts toward no number,
    /// since the session it starts already ran, and together they stay
    /// within [`RESUMED_AN_HOUR`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub carried: Option<String>,
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
    /// The asker's name for it, under which an ask again joins it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub once: Option<String>,
    /// How many asks again under its name joined it while it landed,
    /// none of which started or counted another.
    #[serde(default, skip_serializing_if = "is_none_joined")]
    pub joined: u32,
    /// Whether it asks again, of the machine it went to under the same
    /// name, a start that machine answered nothing to: that start counted
    /// already, so this one doesn't.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub again: bool,
    /// The fence it declared its session runs in, and what declared it,
    /// where it declared one: kept whether or not it moved who asked, so
    /// the book says what each start claimed. A start relayed from
    /// another machine keeps none, since that machine weighed it. One
    /// this build can't read is none, costing the line only that.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::placement::declared_or_none"
    )]
    pub declared: Option<crate::placement::DeclaredFence>,
}

/// Whether no ask joined a start, so its line says nothing of it.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_none_joined(joined: &u32) -> bool {
    *joined == 0
}

impl Kept {
    /// Whether it counts toward its cell's number and agents' number
    /// through the node: an agent's start of new work the node let go
    /// ahead, started or not, since a start that fails can still have
    /// spent; one asked again where it went counted as the first.
    fn counts(&self) -> bool {
        matches!(self.by, By::Agent(_))
            && self.refused.is_none()
            && !self.again
            && self.resumes.is_none()
            && self.carried.is_none()
    }

    /// Whether it was asked under the name `once`, by an asker like `by`
    /// for `cell`.
    fn under(&self, by: By, cell: Option<&str>, once: &str) -> bool {
        // The person apart from an agent, but not one fence from
        // another, since the asker's fence may come out otherwise when it
        // asks again; the cell and the name tell one agent's asks apart.
        let alike = matches!(
            (self.by, by),
            (By::Person, By::Person) | (By::Agent(_), By::Agent(_))
        );
        self.once.as_deref() == Some(once) && alike && self.started_by.as_deref() == cell
    }
}

/// The session of `sessions` that `command` brings back, if it is pi's
/// and says only one that ended, by its id, after `--session`: no
/// other word, so no prompt or flag gives the
/// session new work as it comes back, and no file, which an agent may
/// have written, nor an id that could name one: only letters, digits,
/// `-` and `_`, never first `-`, which pi would read as a flag, since a
/// presence's id is its writer's word. Only pi itself is read, named
/// bare, as the person's login finds it, or by a path in
/// [`PIS_FOLDERS`], so no shell, nor a program an agent wrote and named
/// `pi` elsewhere, hides other work behind the words; and only an ended
/// session, so one running, or on a machine out of reach, where it may
/// still run, is never brought back uncounted. A bare `pi` is fenced by
/// habit alone, since the login finds it on a path an agent may write:
/// each session the node knows ended, once an hour through each node,
/// and [`RESUMED_AN_HOUR`] still bound what that hides. Where it runs,
/// and in which folder, is the caller's to pin to the session's own.
#[must_use]
pub fn brings_back<'a>(command: &[String], sessions: &'a [Session]) -> Option<&'a Session> {
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
    let named = match &command[1..] {
        [flag, named] if RESUMING.contains(&flag.as_str()) => named.as_str(),
        [word] => word
            .split_once('=')
            .and_then(|(flag, named)| RESUMING.contains(&flag).then_some(named))?,
        _ => return None,
    };
    let plain = |id: &str| {
        !id.is_empty()
            && !id.starts_with('-')
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    sessions
        .iter()
        .find(|session| plain(&session.id) && session.id == named)
        .filter(|session| session.state == State::Ended)
}

/// Whether the id of `session` begins another of the `sessions` the
/// node knows on its machine: pi opens a session by an id it begins
/// when none there has that id, so a resume of it may open that other
/// instead, and counts. Only that machine's, since pi looks only at
/// its own, so a presence another machine's agent wrote makes no resume
/// here count. The session named is still the one [`brings_back`]
/// finds, so a start of it is judged parked or coming back all the same.
#[must_use]
pub fn begins_another(session: &Session, sessions: &[Session]) -> bool {
    sessions.iter().any(|other| {
        other.machine == session.machine
            && other.id != session.id
            && other.id.starts_with(&session.id)
    })
}

/// A start let go ahead, held as counted till it is kept.
#[derive(Debug)]
#[must_use = "a start let go ahead is kept once it ends"]
pub struct Admitted {
    id: u64,
    asked: Kept,
}

/// Who started each session a node's book of starts names, as
/// [`Starts::starters`] read it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Starters(BTreeMap<(String, String), String>);

impl Starters {
    /// The cell that started the session held as `held` on `machine`, if
    /// the book names one.
    #[must_use]
    pub fn of(&self, machine: &str, held: &str) -> Option<&str> {
        self.0
            .get(&(machine.to_owned(), held.to_owned()))
            .map(String::as_str)
    }
}

/// The node's book of starts.
#[derive(Debug)]
pub struct Starts {
    file: PathBuf,
    stopped: PathBuf,
    book: Mutex<Book>,
}

#[derive(Debug, Default, Clone)]
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
                        // agent's resume in the hour counts toward
                        // nothing but is kept, since a second of its
                        // session counts, whether its line says
                        // `reopens` or, from an earlier node, `resumes`
                        // alone; one asked again of where it went counts
                        // toward nothing, whatever it brings back, since the
                        // first ask's line stands for it, and is let go.
                        .position(|one| {
                            !matches!(one.by, By::Agent(_))
                                || one.at <= since
                                || one.at > ahead
                                || one.again
                                || !(one.counts()
                                    || one.reopens.is_some()
                                    || one.resumes.is_some()
                                    || one.carried.is_some())
                        })
                });
            match oldest {
                Some(at) => drop(self.kept.remove(at)),
                None => break,
            }
        }
    }
}

/// `asked` with what an agent chose held to a bound, so its asks, refused
/// ones too, can't grow the book past what starts would.
fn bounded(mut asked: Kept) -> Kept {
    asked.folder = asked
        .folder
        .filter(|folder| folder.as_os_str().len() <= FOLDER_MOST);
    asked.program = asked
        .program
        .map(|program| program.chars().take(PROGRAM_MOST).collect());
    asked
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
    /// keeping why: an agent's while the person has stopped starts, once
    /// its cell, or agents together, have started their most in the hour
    /// before it, or where it brings back a session already coming back.
    /// Each is judged under the lock that holds it as going, so two asked
    /// at once can't both pass.
    ///
    /// # Errors
    ///
    /// Why it is refused.
    pub fn admit(&self, asked: Kept) -> Result<Admitted, String> {
        let mut asked = bounded(asked);
        let refused = {
            let mut book = self.lock();
            let refused = Self::coming_back(&book, &asked).or_else(|| {
                Self::once_an_hour(&book, &mut asked);
                self.refusal(&book, &asked, asked.at).map(|(why, _)| why)
            });
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

    /// Which of `asked` would be refused first as [`Self::admit`] judges
    /// each, admitted one after another, where waiting `waits`
    /// milliseconds for the limits to let it wouldn't do, and why: each
    /// asked no sooner than the one before it would have gone, as a caller
    /// waiting on each does, and gone when there is room. A refusal's room
    /// is said from when it was asked, not from when it would have gone.
    /// It keeps, counts and holds nothing as going, so a carry can ask for
    /// all its starts before it tells any session to end.
    pub fn would_refuse(&self, asked: &[Kept], waits: i64) -> Option<(usize, String)> {
        let mut book = Book::clone(&self.lock());
        let mut gone = i64::MIN;
        for (index, one) in asked.iter().enumerate() {
            let mut one = bounded(one.clone());
            let said_at = one.at;
            one.at = one.at.max(gone);
            let refused = Self::coming_back(&book, &one).or_else(|| {
                Self::once_an_hour(&book, &mut one);
                match self.refusal(&book, &one, said_at)? {
                    (_, Some(room)) if room <= one.at + waits => {
                        one.at = room;
                        None
                    }
                    (why, _) => Some(why),
                }
            });
            if let Some(why) = refused {
                return Some((index, why));
            }
            gone = one.at;
            book.going.push((book.next, one));
            book.next += 1;
        }
        None
    }

    /// Keeps `asked` as refused for `why`, a reason the node found
    /// outside the book, such as its session being parked for its holder
    /// to wake, so every refusal is in the book whatever refused it; and
    /// answers `why`.
    pub fn refuse(&self, asked: Kept, why: String) -> String {
        let mut asked = bounded(asked);
        asked.refused = Some(why.clone());
        self.keep_line(&mut self.lock(), asked);
        why
    }

    /// Why `asked`, an agent's start bringing back a session, is refused
    /// where that session is coming back already: a start of it an agent
    /// or the person asked, counted or not, is going, or was let go
    /// within [`COMING_BACK`] of it and didn't fail, its pi perhaps not
    /// yet saying it runs, so a second would open its log beside the
    /// first. The person's own start is never refused, and nor is one
    /// asked again of the machine it went to, which that machine joins to
    /// the first under its name. Only a start of a session the node holds
    /// as ended is judged: once its pi says it runs, nothing here refuses
    /// another.
    fn coming_back(book: &Book, asked: &Kept) -> Option<String> {
        let id = asked.reopens.as_deref()?;
        if asked.by == By::Person || asked.again {
            return None;
        }
        let its = |one: &Kept| one.reopens.as_deref().or(one.resumes.as_deref()) == Some(id);
        if book.going.iter().any(|(_, going)| its(going)) {
            return Some(format!(
                "{id} is coming back already, from a start of it still going; \
                 ask again once it has gone only if it isn't running"
            ));
        }
        let let_go = book.kept.iter().any(|one| {
            its(one)
                && one.refused.is_none()
                && one.failed.is_none()
                && one.at > asked.at - COMING_BACK
                && one.at <= asked.at + COMING_BACK
        });
        let_go.then(|| {
            format!(
                "{id} is coming back already, from a start asked in the last {} seconds; \
                 ask again after that only if it isn't running",
                COMING_BACK / 1000
            )
        })
    }

    /// When the limits that refuse `asked` would let it, in milliseconds
    /// since the epoch, or none while they let it or the person's stop
    /// holds it.
    #[must_use]
    pub fn room(&self, asked: &Kept) -> Option<i64> {
        let book = self.lock();
        let mut asked = asked.clone();
        Self::once_an_hour(&book, &mut asked);
        self.refusal(&book, &asked, asked.at)
            .and_then(|(_, room)| room)
    }

    /// Counts `asked` after all where it brings back a session the book
    /// already shows brought back uncounted in the hour before it, going
    /// or kept, so a burst of starts of one ended session counts from the
    /// second on, before the first has run long enough to say so; or
    /// where agents have brought back [`RESUMED_AN_HOUR`] uncounted in
    /// that hour already. So too for one starting a session a carry
    /// took, as [`Kept::carried`] says, the two sharing that ceiling.
    fn once_an_hour(book: &Book, asked: &mut Kept) {
        let since = asked.at - HOUR;
        if asked.resumes.is_none() && asked.carried.is_none() {
            return;
        }
        let free: Vec<&Kept> = book
            .kept
            .iter()
            .chain(book.going.iter().map(|(_, going)| going))
            .filter(|one| {
                matches!(one.by, By::Agent(_))
                    && one.refused.is_none()
                    && (one.resumes.is_some() || one.carried.is_some())
                    && one.at > since
                    && one.at <= asked.at + HOUR
            })
            .collect();
        // One that failed brought nothing back, so it spends no session's
        // hour, though it still counts toward the node's ceiling.
        let again = |of: fn(&Kept) -> Option<&String>| {
            of(asked).is_some_and(|session| {
                free.iter()
                    .any(|one| one.failed.is_none() && of(one) == Some(session))
            })
        };
        let full = free.len() >= RESUMED_AN_HOUR;
        let resumed_again = full || again(|one| one.resumes.as_ref());
        let carried_again = full || again(|one| one.carried.as_ref());
        if resumed_again {
            asked.resumes = None;
        }
        if carried_again {
            asked.carried = None;
        }
    }

    /// Why `asked` is refused, if it is, with its room said from `said_at`,
    /// and when the limits that refuse it would let it, where it is they
    /// that do.
    fn refusal(&self, book: &Book, asked: &Kept, said_at: i64) -> Option<(String, Option<i64>)> {
        if !matches!(asked.by, By::Agent(_)) {
            return None;
        }
        match self.stop_stands() {
            Ok(false) => {}
            Ok(true) => {
                return Some((
                    format!(
                        "the person stopped agents starting sessions, and only they resume \
                         it, with `mycelium starts resume`; {STOPPED_FOR_AN_AGENT}"
                    ),
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
        Some((paced(&why, room, said_at), Some(room)))
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
        let begun: Vec<i64> = counted
            .iter()
            .filter(|one| one.started_by == asked.started_by)
            .map(|one| one.at)
            .collect();
        let all: Vec<i64> = counted.iter().map(|one| one.at).collect();
        let cell = asked
            .started_by
            .as_deref()
            .map_or_else(|| "agents of no known cell".to_owned(), str::to_owned);
        // Room comes once enough of the oldest have passed the hour: the
        // node's for every start, and the cell's for its own.
        let cell_limit = room_after(begun.clone(), CELL_AN_HOUR).map(|room| {
            let why = format!(
                "{cell} started {} sessions in the last hour, and a cell starts at most \
                 {CELL_AN_HOUR} an hour",
                begun.len()
            );
            (why, room)
        });
        let node_limit = room_after(all.clone(), NODE_AN_HOUR).map(|room| {
            let why = format!(
                "agents started {} sessions through this node in the last hour, and they start \
                 at most {NODE_AN_HOUR} an hour",
                all.len()
            );
            (why, room)
        });
        // The limit said is the one whose room comes last.
        match (cell_limit, node_limit) {
            (Some(cell), Some(node)) if node.1 > cell.1 => Some(node),
            (Some(cell), _) => Some(cell),
            (None, node) => node,
        }
    }

    /// Keeps how `admitted` ended: where it started, or why it didn't, as
    /// [`failure`] tells it, and how many asks under its name `joined` it.
    pub fn keep(&self, admitted: Admitted, ended: Result<(String, String), String>, joined: u32) {
        let Admitted { id, mut asked } = admitted;
        asked.joined = joined;
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

    /// The cell that started each session this node started for one, as
    /// its book says, by the machine and held id it landed as: the newest
    /// line for each, read under one hold of the book, so a node judging
    /// every session holds back no start while it does.
    #[must_use]
    pub fn starters(&self) -> Starters {
        let mut starters = BTreeMap::new();
        for one in &self.lock().kept {
            let (Some(machine), Some(held)) = (&one.machine, &one.held) else {
                continue;
            };
            let landed = (machine.clone(), held.clone());
            match &one.started_by {
                Some(by) => starters.insert(landed, by.clone()),
                None => starters.remove(&landed),
            };
        }
        Starters(starters)
    }

    /// Counts one more ask under the name `once`, by `by` for the cell
    /// `cell`, as joining the start still landing under it, as the list
    /// of starts shows it; the count its line keeps is its lead's.
    pub fn joined(&self, by: By, cell: Option<&str>, once: &str) {
        let mut book = self.lock();
        if let Some((_, going)) = book
            .going
            .iter_mut()
            .find(|(_, one)| one.under(by, cell, once))
        {
            going.joined = going.joined.saturating_add(1);
        }
    }

    /// Where the start under the name `once`, by `by` for the cell `cell`,
    /// landed within the hour before `at`, as the book read back says:
    /// its machine and held id.
    #[must_use]
    pub fn landed(
        &self,
        by: By,
        cell: Option<&str>,
        once: &str,
        at: i64,
    ) -> Option<(String, String)> {
        self.lock()
            .kept
            .iter()
            .rev()
            .filter(|one| one.at > at - HOUR && one.under(by, cell, once))
            .find_map(|one| one.machine.clone().zip(one.held.clone()))
    }

    /// The newest `most` starts, newest last, those still landing after
    /// those that ended.
    #[must_use]
    pub fn newest(&self, most: usize) -> Vec<Kept> {
        let book = self.lock();
        let all: Vec<&Kept> = book
            .kept
            .iter()
            .chain(book.going.iter().map(|(_, going)| going))
            .collect();
        all[all.len().saturating_sub(most)..]
            .iter()
            .map(|&one| one.clone())
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
         {PACED_FOR_AN_AGENT}"
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
        starts.keep(admitted, Err(why.to_owned()), 0);
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
            reopens: None,
            carried: None,
            machine: None,
            held: None,
            failed: None,
            refused: None,
            once: None,
            joined: 0,
            again: false,
            declared: None,
        }
    }

    /// A start of `session` where a carry `cell` ordered took it, as the
    /// node asks it once it has held it to that carry.
    fn carrying(cell: &str, session: &str, at: i64) -> Kept {
        Kept {
            carried: Some(session.to_owned()),
            ..agent(cell, at)
        }
    }

    // WCJQSG (2026-11-19): an orchestrator carrying its quest's sessions
    // to another machine begins no new work, so it spends none of its
    // cell's eight; bringing one back and forth is still bounded.
    #[test]
    fn a_carry_an_orchestrator_ordered_starts_its_sessions_uncounted_once_an_hour() {
        let (_dir, starts) = book();
        // Past the cell's eight, as a quest of many sessions carries.
        for one in 0..=CELL_AN_HOUR {
            let at = NOW + i64::try_from(one).expect("few");
            started(&starts, carrying("OSPREY", &format!("s-{one}"), at), "h");
        }
        for _ in 0..CELL_AN_HOUR {
            started(&starts, agent("OSPREY", NOW + 100), "h");
        }
        starts
            .admit(agent("OSPREY", NOW + 100))
            .expect_err("new work still counts");
        starts
            .admit(carrying("OSPREY", "s-0", NOW + 100))
            .expect_err("and so does carrying one again within the hour");
        // The node's ceiling is shared with the sessions brought back.
        let (_dir, starts) = book();
        for one in 0..RESUMED_AN_HOUR {
            let at = NOW + i64::try_from(one).expect("few");
            let cell = format!("C{one:05}");
            let kept = if one % 2 == 0 {
                resuming(&cell, &format!("s-{one}"), at)
            } else {
                carrying(&cell, &format!("s-{one}"), at)
            };
            started(&starts, kept, "h");
        }
        let next = starts
            .admit(carrying("OSPREY", "s-last", NOW + 200))
            .expect("room in the cell");
        assert_eq!(
            next.asked.carried, None,
            "past the ceiling, one more counts"
        );
    }

    // A2VLJN (2026-11-21): a carry asks for all its starts before it
    // tells any session to end, so the ask weighs them together as admit
    // would let them go one after another, and holds none as going.
    #[test]
    fn a_carrys_starts_are_weighed_together_as_admit_would_let_them_go() {
        let (_dir, starts) = book();
        for one in 0..CELL_AN_HOUR - 2 {
            let at = NOW + i64::try_from(one).expect("few");
            started(&starts, agent("OSPREY", at), "h");
        }
        let two = [agent("OSPREY", NOW + 100), agent("OSPREY", NOW + 101)];
        assert_eq!(refused(&starts, &two, 0), None, "two fit its number");
        let three = [
            agent("OSPREY", NOW + 100),
            agent("OSPREY", NOW + 101),
            agent("OSPREY", NOW + 102),
        ];
        let (third, why) = starts.would_refuse(&three, 0).expect("the third doesn't");
        assert_eq!(third, 2, "the third is the one refused");
        assert!(why.contains("OSPREY started 8"), "{why}");
        started(&starts, agent("OSPREY", NOW + 200), "h");
        started(&starts, agent("OSPREY", NOW + 201), "h");
        let late = agent("OSPREY", NOW + 300);
        assert_eq!(
            refused(&starts, std::slice::from_ref(&late), 0),
            starts.admit(late).err(),
            "the ask held none as going, and refuses as admit does"
        );
        let soon = NOW + HOUR - 30_000;
        assert_eq!(
            refused(&starts, &[agent("OSPREY", soon)], 60_000),
            None,
            "room within the wait lets it"
        );
        assert!(
            refused(&starts, &[agent("OSPREY", soon)], 10_000).is_some(),
            "room past it doesn't"
        );
        let carried = [
            carrying("OSPREY", "s-a", NOW + 400),
            carrying("OSPREY", "s-b", NOW + 401),
        ];
        assert_eq!(
            refused(&starts, &carried, 0),
            None,
            "an ordered carry's arrivals start uncounted"
        );
        let twice = [
            carrying("OSPREY", "s-a", NOW + 400),
            carrying("OSPREY", "s-a", NOW + 401),
        ];
        assert!(
            refused(&starts, &twice, 0).is_some(),
            "but one carried twice in the hour counts"
        );
        let back = |at| reopening(By::Agent(Kind::Fence), Some("SKIFFS"), "s-c", at);
        let coming = refused(&starts, &[back(NOW + 500), back(NOW + 501)], 0)
            .expect("one session brought back twice");
        assert!(coming.contains("coming back already"), "{coming}");
    }

    #[test]
    fn each_start_asked_waits_its_minute_from_when_the_one_before_it_went() {
        let (_dir, starts) = book();
        // The cell's eight: the oldest leaves the hour 30 s after the ask,
        // the next 70 s after it, which is 40 s after the first asked goes,
        // and the rest twenty minutes on.
        started(&starts, agent("OSPREY", NOW), "h");
        started(&starts, agent("OSPREY", NOW + 40_000), "h");
        for one in 0..CELL_AN_HOUR - 2 {
            let at = NOW + 1_200_000 + i64::try_from(one).expect("few");
            started(&starts, agent("OSPREY", at), "h");
        }
        let ask = NOW + HOUR - 30_000;
        let two = [agent("OSPREY", ask), agent("OSPREY", ask)];
        assert_eq!(
            refused(&starts, &two, START_WAITS_MOST),
            None,
            "the second waits from when the first went, not from the ask"
        );
        let three = [
            agent("OSPREY", ask),
            agent("OSPREY", ask),
            agent("OSPREY", ask),
        ];
        let third = refused(&starts, &three, START_WAITS_MOST)
            .expect("a third, with no room for minutes after, doesn't");
        // Its room comes 1,230 s after the ask, when the oldest of the
        // rest leaves the hour, said from the ask rather than from 70 s on,
        // when the second would have gone.
        assert!(third.contains("room again in 21 minutes"), "{third}");
    }

    /// The longest the node lets an agent's start wait, as its ask weighs.
    const START_WAITS_MOST: i64 = super::super::START_WAITS_MOST;

    /// Why the first of `asked` [`Starts::would_refuse`] refuses would be.
    fn refused(starts: &Starts, asked: &[Kept], waits: i64) -> Option<String> {
        starts.would_refuse(asked, waits).map(|(_, why)| why)
    }

    fn resuming(cell: &str, session: &str, at: i64) -> Kept {
        Kept {
            resumes: Some(session.to_owned()),
            ..agent(cell, at)
        }
    }

    /// A start bringing `session` back, as the node asks it: uncounted
    /// where the book lets it, and marked as reopening it either way.
    fn reopening(by: By, cell: Option<&str>, session: &str, at: i64) -> Kept {
        Kept {
            resumes: Some(session.to_owned()),
            reopens: Some(session.to_owned()),
            ..asked(by, cell, at)
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
        // Past the cell's number and the node's together, as every session
        // a restarted machine held comes back at once.
        for one in 0..=NODE_AN_HOUR {
            let at = NOW + i64::try_from(one).expect("few");
            started(&starts, resuming("OSPREY", &format!("s-{one}"), at), "h");
        }
        for _ in 1..CELL_AN_HOUR {
            started(&starts, agent("OSPREY", NOW + 100), "h");
        }
        // Bringing one back again within the hour counts, as new work.
        started(&starts, resuming("OSPREY", "s-0", NOW + 100), "h");
        starts
            .admit(agent("OSPREY", NOW + 100))
            .expect_err("new work still counts");
        starts
            .admit(resuming("OSPREY", "s-1", NOW + 100))
            .expect_err("and so does bringing one back again");
        started(&starts, resuming("OSPREY", "s-new", NOW + 100), "h");
        starts.stop().expect("stopped");
        starts
            .admit(resuming("SKIFFS", "s-else", NOW + 100))
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
    fn sessions_brought_back_count_toward_no_number_so_new_work_still_starts() {
        let (_dir, starts) = book();
        for one in 0..=NODE_AN_HOUR {
            let at = NOW + i64::try_from(one).expect("few");
            let cell = format!("CELL{:02}", one % 2);
            started(&starts, resuming(&cell, &format!("s-{one}"), at), "h");
        }
        assert_eq!(
            starts.room(&agent("CELL00", NOW + 100)),
            None,
            "a cell that brought back a wave has its whole hour for new work"
        );
        for one in 0..NODE_AN_HOUR {
            let at = NOW + 100 + i64::try_from(one).expect("few");
            let cell = format!("CELL{:02}", one / CELL_AN_HOUR);
            started(&starts, agent(&cell, at), "h");
        }
        let refused = starts
            .admit(agent("SKIFFS", NOW + 200))
            .expect_err("new work past the node's number still waits");
        assert!(refused.contains("agents started 32 sessions"), "{refused}");
        started(&starts, resuming("SKIFFS", "s-last", NOW + 200), "h");
    }

    #[test]
    fn past_the_nodes_uncounted_resumes_an_hour_each_counts_as_new_work() {
        let (_dir, starts) = book();
        for one in 0..RESUMED_AN_HOUR {
            let at = NOW + i64::try_from(one).expect("few");
            started(&starts, resuming("OSPREY", &format!("s-{one}"), at), "h");
        }
        for _ in 0..CELL_AN_HOUR {
            started(&starts, agent("OSPREY", NOW + 200), "h");
        }
        let refused = starts
            .admit(resuming("OSPREY", "s-more", NOW + 201))
            .expect_err("one more resume in the hour counts");
        assert!(refused.contains("OSPREY started 8"), "{refused}");
        started(&starts, resuming("OSPREY", "s-more", NOW + HOUR + 1), "h");
    }

    #[test]
    fn a_resume_that_failed_still_fills_the_nodes_uncounted() {
        let (_dir, starts) = book();
        for one in 0..RESUMED_AN_HOUR {
            let at = NOW + i64::try_from(one).expect("few");
            let admitted = starts
                .admit(resuming("OSPREY", &format!("s-{one}"), at))
                .expect("it may start");
            starts.keep(admitted, Err("pi didn't start".to_owned()), 0);
        }
        let next = starts
            .admit(resuming("OSPREY", "s-more", NOW + 200))
            .expect("the cell has room for new work");
        assert_eq!(next.asked.resumes, None, "so one more counts as new work");
    }

    #[test]
    fn resumes_dated_up_to_an_hour_ahead_still_fill_the_nodes_uncounted() {
        let ahead = |by: i64| {
            let (dir, starts) = book();
            for one in 0..RESUMED_AN_HOUR {
                let at = NOW + by + i64::try_from(one).expect("few");
                started(&starts, resuming("OSPREY", &format!("s-{one}"), at), "h");
            }
            for _ in 0..CELL_AN_HOUR {
                started(&starts, agent("OSPREY", NOW), "h");
            }
            (dir, starts)
        };
        let (_dir, starts) = ahead(HOUR / 2);
        let refused = starts
            .admit(resuming("OSPREY", "s-more", NOW))
            .expect_err("a clock set back half an hour gains no fresh 128");
        assert!(refused.contains("OSPREY started 8"), "{refused}");
        let (_dir, starts) = ahead(2 * HOUR);
        started(&starts, resuming("OSPREY", "s-more", NOW), "h");
    }

    #[test]
    fn a_session_brought_back_is_coming_back_for_two_minutes_however_it_counted() {
        let (dir, starts) = book();
        let agents = By::Agent(Kind::Fence);
        let back = |session: &str, at: i64| reopening(agents, Some("OSPREY"), session, at);
        started(&starts, back("s-0", NOW), "h");
        let refused = starts
            .admit(back("s-0", NOW + COMING_BACK - 1))
            .expect_err("a retry soon after finds it coming back");
        assert!(refused.contains("s-0 is coming back already"), "{refused}");
        let book = std::fs::read_to_string(dir.path().join("starts.jsonl")).expect("kept");
        let line = book.lines().last().expect("its line");
        assert!(
            line.contains(r#""refused":"s-0 is coming back"#),
            "the refusal is booked: {line}"
        );
        // Its second in the hour counts, and still comes back only once.
        started(&starts, back("s-0", NOW + COMING_BACK), "h");
        let refused = starts
            .admit(back("s-0", NOW + COMING_BACK + 1))
            .expect_err("one counted comes back once too");
        assert!(refused.contains("s-0 is coming back already"), "{refused}");
        started(&starts, back("s-1", NOW + 1), "h");
        // One still going refuses a second however long it has taken.
        let going = starts.admit(back("s-2", NOW)).expect("it may start");
        let refused = starts
            .admit(back("s-2", NOW + 10 * COMING_BACK))
            .expect_err("a start still going is coming back");
        assert!(
            refused.contains("s-2 is coming back already, from a start of it still going"),
            "{refused}"
        );
        starts.keep(going, Ok(("skiff".to_owned(), "h".to_owned())), 0);
        // One dated past the window ahead, as a clock set back would,
        // holds none back.
        started(&starts, back("s-3", NOW + 3 * COMING_BACK), "h");
        started(&starts, back("s-3", NOW + 2), "h");
        started(
            &starts,
            reopening(By::Person, None, "s-0", NOW + COMING_BACK + 2),
            "h",
        );
        let again = Kept {
            again: true,
            ..back("s-0", NOW + COMING_BACK + 3)
        };
        started(&starts, again, "h");
    }

    #[test]
    fn one_brought_back_a_little_ahead_of_an_ask_is_coming_back() {
        let (_dir, starts) = book();
        let back = |at: i64| reopening(By::Agent(Kind::Fence), Some("OSPREY"), "s-4", at);
        started(&starts, back(NOW + COMING_BACK / 2), "h");
        let refused = starts
            .admit(back(NOW))
            .expect_err("a clock a minute behind finds it coming back");
        assert!(refused.contains("s-4 is coming back already"), "{refused}");
    }

    #[test]
    fn a_resume_that_failed_leaves_its_session_free_to_come_back() {
        let (_dir, starts) = book();
        let agents = By::Agent(Kind::Fence);
        let failed = starts
            .admit(reopening(agents, Some("OSPREY"), "s-0", NOW))
            .expect("it may start");
        starts.keep(failed, Err("pi didn't start".to_owned()), 0);
        let next = starts
            .admit(reopening(agents, Some("OSPREY"), "s-0", NOW + 1))
            .expect("one that failed brought nothing back");
        assert_eq!(
            next.asked.resumes.as_deref(),
            Some("s-0"),
            "and spent none of its hour"
        );
    }

    #[test]
    fn the_book_lets_go_of_a_start_asked_again_before_a_resume_or_one_that_counts() {
        let (_dir, starts) = book();
        // One asked again of where it went, bringing back the session its
        // first ask did.
        let again = Kept {
            again: true,
            ..reopening(By::Agent(Kind::Fence), Some("OSPREY"), "s-2", NOW)
        };
        // An uncounted resume, as this node books it and as an earlier one
        // did, with `resumes` alone: each is kept, since a second of its
        // session counts and comes back only once.
        let earlier = resuming("OSPREY", "s-0", NOW);
        let resume = reopening(By::Agent(Kind::Fence), Some("OSPREY"), "s-1", NOW);
        let mut book = starts.lock();
        book.hold(earlier.clone());
        book.hold(resume.clone());
        book.hold(again.clone());
        for _ in 2..KEPT_MOST {
            book.hold(agent("OSPREY", NOW + 1));
        }
        assert_eq!(book.kept.len(), KEPT_MOST, "the book grew past its bound");
        assert!(
            !book.kept.contains(&again),
            "the one counting nothing stayed"
        );
        assert!(
            book.kept.contains(&earlier),
            "an earlier node's resume went"
        );
        assert!(book.kept.contains(&resume), "an uncounted resume went");
    }

    #[test]
    fn a_start_brings_back_a_session_only_by_its_id_alone_while_it_doesnt_run() {
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
            // A presence an agent wrote, naming a file of its own as the id.
            session("/home/x/work/new.jsonl", "ended"),
            session("../new", "ended"),
            // One pi would read as a flag.
            session("--continue", "ended"),
            // A short id an agent wrote, which pi with no session of that
            // id opens as the first whose id it begins, here a busy one.
            session("0193", "ended"),
        ];
        let file = "/home/x/.pi/agent/sessions/--x--/2026-11-18T22-00-00_0193-ended.jsonl";
        for brought in [
            words(&["pi", "--session", "0193-ended"]),
            words(&["/run/current-system/sw/bin/pi", "--session", "0193-ended"]),
            words(&["pi", "--session=0193-ended"]),
            words(&["/etc/profiles/per-user/x/bin/pi", "--session=0193-ended"]),
        ] {
            assert_eq!(
                brings_back(&brought, &sessions).map(|session| session.id.as_str()),
                Some("0193-ended"),
                "{brought:?}"
            );
        }
        for new in [
            words(&["pi", "--session", "0193-busy"]),
            words(&["pi", "--session", "0193-away"]),
            words(&["pi", "--session", "0193-unknown"]),
            // Pi begins a new session under an id it finds none of.
            words(&["pi", "--session-id", "0193-ended"]),
            words(&["pi", "--session-id=0193-ended"]),
            words(&["pi", "--session", "x0193-ended"]),
            words(&["pi", "0193-ended"]),
            words(&["pi", "--session", "0193-busy", "--session", "0193-ended"]),
            words(&["pi", "--session=0193-new", "--session-id", "0193-ended"]),
            words(&["pi", "--session", "0193-ended", "-p", "new work"]),
            words(&["pi", "new work", "--session", "0193-ended"]),
            words(&["pi", "--session", "0193-ended", "--session", "0193-ended"]),
            words(&["pi", "--model", "x", "--session=0193-ended"]),
            words(&["pi", "--session", file]),
            words(&["pi", "--session", "/home/x/work/new.jsonl"]),
            words(&["pi", "--session=../new"]),
            words(&["pi", "--session", "--continue"]),
            words(&["pi", "--session=--continue"]),
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
        // A short id still names its own session, to be judged parked or
        // coming back, but begins another, which pi may open instead.
        let short = words(&["pi", "--session", "0193"]);
        assert_eq!(
            brings_back(&short, &sessions).map(|session| session.id.as_str()),
            Some("0193")
        );
        let named = |id: &str| {
            sessions
                .iter()
                .find(|session| session.id == id)
                .expect("a session")
        };
        assert!(
            begins_another(named("0193"), &sessions),
            "0193 begins 0193-busy"
        );
        assert!(
            !begins_another(named("0193-ended"), &sessions),
            "a whole id begins none"
        );
        let mut elsewhere = sessions.to_vec();
        elsewhere.retain(|session| !session.id.starts_with("0193-"));
        let mut far = session("0193-far", "busy");
        far.machine = "tern".to_owned();
        elsewhere.push(far);
        assert!(
            !begins_another(named("0193"), &elsewhere),
            "nor one another machine's session begins, which pi here never opens"
        );
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
        starts.keep(first, Ok(("kestrel".to_owned(), "h".to_owned())), 0);
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
        starts.keep(admitted, Ok(("kestrel".to_owned(), held.to_owned())), 0);
    }

    fn named(cell: &str, at: i64, once: &str) -> Kept {
        Kept {
            once: Some(once.to_owned()),
            ..agent(cell, at)
        }
    }

    // One asked again of the machine that answered nothing to it counted
    // as the first, so a cell at its number may still ask it there; the
    // person's stop still holds it.
    #[test]
    fn a_start_asked_again_where_it_went_counts_nothing_but_still_heeds_a_stop() {
        let (dir, starts) = book();
        for one in 0..CELL_AN_HOUR {
            let failed = starts
                .admit(agent("OSPREY", NOW + i64::try_from(one).expect("few")))
                .expect("within");
            starts.keep(
                failed,
                Err("kestrel didn't answer within 30s".to_owned()),
                0,
            );
        }
        let again = Kept {
            again: true,
            ..named("OSPREY", NOW + 100, "01J")
        };
        let admitted = starts.admit(again.clone()).expect("it counted already");
        starts.keep(admitted, Ok(("kestrel".to_owned(), "h-1".to_owned())), 2);
        let book = std::fs::read_to_string(dir.path().join("starts.jsonl")).expect("kept");
        let line = book.lines().last().expect("its line");
        assert!(line.contains(r#""again":true"#), "{line}");
        assert!(
            line.contains(r#""joined":2"#),
            "the joins its lead counted: {line}"
        );
        starts.stop().expect("stopped");
        starts
            .admit(again)
            .expect_err("the person's stop holds it too");
    }

    // After the node starts again, a start under a name within the hour
    // is found in the book read back, by its asker's name alone.
    #[test]
    fn a_start_under_a_name_is_found_again_in_the_book_read_back_for_an_hour() {
        let (dir, starts) = book();
        started(&starts, named("OSPREY", NOW, "01J"), "h-1");
        let failed = starts.admit(named("OSPREY", NOW, "01K")).expect("within");
        starts.keep(failed, Err("it didn't start".to_owned()), 0);
        drop(starts);
        let again = Starts::at(dir.path().join("starts.jsonl"));
        let by = By::Agent(Kind::Fence);
        assert_eq!(
            again.landed(by, Some("OSPREY"), "01J", NOW + 1),
            Some(("kestrel".to_owned(), "h-1".to_owned()))
        );
        assert_eq!(
            again.landed(By::Agent(Kind::Strict), Some("OSPREY"), "01J", NOW + 1),
            Some(("kestrel".to_owned(), "h-1".to_owned())),
            "an ask again whose fence came out otherwise is the same asker's"
        );
        assert_eq!(again.landed(by, Some("OSPREY"), "01K", NOW + 1), None);
        assert_eq!(again.landed(by, Some("KESTRL"), "01J", NOW + 1), None);
        assert_eq!(again.landed(By::Person, None, "01J", NOW + 1), None);
        assert_eq!(
            again.landed(by, Some("OSPREY"), "01J", NOW + HOUR + 1),
            None
        );
    }

    // A newer node may declare a fence this build doesn't know: the line
    // still reads, so its start still counts toward the hour's limits.
    #[test]
    fn a_declared_fence_this_build_cant_read_costs_its_line_only_that() {
        let mut line = serde_json::to_value(named("OSPREY", NOW, "01J")).expect("a line");
        line["declared"] = serde_json::json!({"kind": "sealed", "from": "spawn"});
        let kept: Kept = serde_json::from_value(line).expect("the line still reads");
        assert_eq!(kept.declared, None);
        assert_eq!(kept.started_by.as_deref(), Some("OSPREY"));
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
            starts.keep(one, Err("it didn't start".to_owned()), 0);
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
        let starters = again.starters();
        assert_eq!(starters.of("kestrel", "h-1"), Some("OSPREY"));
        assert_eq!(starters.of("osprey", "h-1"), None, "another machine's");
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
            Starts::at(file).starters().of("kestrel", "h-2"),
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
        let mut asked = agent("OSPREY", NOW + 1);
        asked.folder = Some(PathBuf::from("f".repeat(FOLDER_MOST + 1)));
        asked.program = Some("p".repeat(PROGRAM_MOST + 1));
        assert_eq!(
            starts.refuse(asked, "it is parked".to_owned()),
            "it is parked"
        );
        let kept = starts.newest(1);
        assert_eq!(kept[0].refused.as_deref(), Some("it is parked"));
        assert_eq!(kept[0].folder, None, "one refused outside the book too");
        assert_eq!(
            kept[0].program.as_deref().map(str::len),
            Some(PROGRAM_MOST),
            "and its program is cut"
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
