//! The sessions a node starts, each written down, and an agent's held to
//! a number an hour. Cells start cells, so one gone wrong could start
//! them without end, each spending on the person's account: the node
//! refuses an agent's start past what one cell, and all of them together,
//! may start in an hour, and every one while the person has stopped them,
//! which takes one act. The person's own starts are never held back.
//!
//! The book is kept in `starts.jsonl` beside the node's runs, a line a
//! start: `{"at", "by", "started_by", "folder", "program"}`, then where it
//! started (`"machine"` and `"held"`), or why it didn't (`"failed"`), or
//! why it was refused (`"refused"`). The person's stop is the file
//! `starts.stopped` beside it, there for as long as starts are stopped.

use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use crate::wire::By;

/// The most sessions one cell may start in an hour.
pub const CELL_AN_HOUR: usize = 8;

/// The most sessions agents together may start through one node in an
/// hour.
pub const NODE_AN_HOUR: usize = 32;

/// An hour, in milliseconds.
const HOUR: i64 = 3_600_000;

/// The most starts held in memory, the newest, which reach back further
/// than an hour of starts within the limits.
const KEPT_MOST: usize = 2048;

/// A start the node made, or was asked and refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    /// When it was asked, in milliseconds since the epoch.
    pub at: i64,
    /// Who asked it.
    pub by: By,
    /// The cell whose session asked, by its address, as the node proved
    /// it; none for the person's, or an agent's the node could tell no
    /// session of.
    pub started_by: Option<String>,
    /// The folder it was asked to begin in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
    /// The program it starts, its first word alone, so no argument it was
    /// given is kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// The machine it started on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// Its held id there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<String>,
    /// Why it was asked but didn't start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
    /// Why the node refused it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

impl Kept {
    /// Whether it counts toward an agent's limits: an agent's start the
    /// node let go ahead, started or not, since a start that fails can
    /// still have spent.
    fn counts(&self) -> bool {
        matches!(self.by, By::Agent(_)) && self.refused.is_none()
    }
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
}

impl Starts {
    /// The book kept in `file`, read back as far as [`KEPT_MOST`] starts,
    /// a line it can't read passed over alone.
    #[must_use]
    pub fn at(file: PathBuf) -> Self {
        let mut kept = VecDeque::new();
        if let Ok(read) = std::fs::File::open(&file) {
            for line in std::io::BufReader::new(read).lines() {
                let Ok(line) = line else { break };
                if let Ok(start) = serde_json::from_str::<Kept>(&line) {
                    if kept.len() == KEPT_MOST {
                        kept.pop_front();
                    }
                    kept.push_back(start);
                }
            }
        }
        Self {
            stopped: file.with_file_name("starts.stopped"),
            file,
            book: Mutex::new(Book {
                kept,
                ..Book::default()
            }),
        }
    }

    /// Whether the person has stopped agents' starts.
    #[must_use]
    pub fn stopped(&self) -> bool {
        self.stopped.symlink_metadata().is_ok()
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
        let refused = {
            let mut book = self.lock();
            let refused = self.refusal(&book, &asked);
            if refused.is_none() {
                let id = book.next;
                book.next += 1;
                book.going.push((id, asked.clone()));
                return Ok(Admitted { id, asked });
            }
            refused
        };
        asked.refused.clone_from(&refused);
        self.keep_line(asked);
        Err(refused.unwrap_or_default())
    }

    /// Why `asked` is refused, if it is.
    fn refusal(&self, book: &Book, asked: &Kept) -> Option<String> {
        if !asked.counts() {
            return None;
        }
        if self.stopped() {
            return Some(
                "the person stopped agents starting sessions; only they resume it, \
                 with `mycelium starts resume`"
                    .to_owned(),
            );
        }
        let since = asked.at - HOUR;
        let counted: Vec<&Kept> = book
            .kept
            .iter()
            .chain(book.going.iter().map(|(_, going)| going))
            .filter(|one| one.counts() && one.at > since)
            .collect();
        let of_cell = counted
            .iter()
            .filter(|one| one.started_by == asked.started_by)
            .count();
        let cell = asked
            .started_by
            .as_deref()
            .map_or_else(|| "agents of no known cell".to_owned(), str::to_owned);
        if of_cell >= CELL_AN_HOUR {
            return Some(format!(
                "{cell} started {of_cell} sessions in the last hour, and a cell starts at most \
                 {CELL_AN_HOUR} an hour"
            ));
        }
        (counted.len() >= NODE_AN_HOUR).then(|| {
            format!(
                "agents started {} sessions through this node in the last hour, and they start \
                 at most {NODE_AN_HOUR} an hour",
                counted.len()
            )
        })
    }

    /// Keeps how `admitted` ended: where it started, or why it didn't.
    pub fn keep(&self, admitted: Admitted, ended: Result<(String, String), String>) {
        let Admitted { id, mut asked } = admitted;
        match ended {
            Ok((machine, held)) => {
                asked.machine = Some(machine);
                asked.held = Some(held);
            }
            Err(why) => asked.failed = Some(why),
        }
        self.lock().going.retain(|(going, _)| *going != id);
        self.keep_line(asked);
    }

    /// Writes `start` in the book, in one write so two kept at once never
    /// interleave, and holds it in memory.
    fn keep_line(&self, start: Kept) {
        let written = serde_json::to_string(&start)
            .map_err(std::io::Error::other)
            .and_then(|line| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.file)?
                    .write_all(format!("{line}\n").as_bytes())
            });
        if let Err(error) = written {
            eprintln!("a start isn't kept in {}: {error}", self.file.display());
        }
        let mut book = self.lock();
        if book.kept.len() == KEPT_MOST {
            book.kept.pop_front();
        }
        book.kept.push_back(start);
    }

    /// The cell that started the session held as `held` on `machine`, as
    /// this node's book says, if this node started it for one.
    #[must_use]
    pub fn started_by(&self, machine: &str, held: &str) -> Option<String> {
        self.lock()
            .kept
            .iter()
            .rev()
            .find(|one| one.machine.as_deref() == Some(machine) && one.held.as_deref() == Some(held))
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

/// Where a node whose runs are kept in `runs` keeps its starts: beside
/// them.
#[must_use]
pub fn beside(runs: &Path) -> PathBuf {
    runs.with_file_name("starts.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::declared::Kind;

    const NOW: i64 = 1_800_000_000_000;

    fn asked(by: By, cell: Option<&str>, at: i64) -> Kept {
        Kept {
            at,
            by,
            started_by: cell.map(str::to_owned),
            folder: None,
            program: Some("pi".to_owned()),
            machine: None,
            held: None,
            failed: None,
            refused: None,
        }
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
            started(&starts, agent("OSPREY", NOW + i64::try_from(one).expect("few")), "h");
        }
        let refused = starts.admit(agent("OSPREY", NOW + 100)).expect_err("past its number");
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
        let refused = starts.admit(agent("TERNSS", NOW)).expect_err("past the node's");
        assert!(refused.contains("through this node"), "{refused}");
        started(&starts, asked(By::Person, None, NOW), "h");
    }

    #[test]
    fn a_start_still_going_counts_and_one_refused_or_the_persons_never_does() {
        let (_dir, starts) = book();
        let going: Vec<Admitted> = (0..CELL_AN_HOUR)
            .map(|_| starts.admit(agent("OSPREY", NOW)).expect("within"))
            .collect();
        starts.admit(agent("OSPREY", NOW)).expect_err("those going count");
        for one in going {
            starts.keep(one, Err("it didn't start".to_owned()));
        }
        starts.admit(agent("OSPREY", NOW)).expect_err("those that failed count");
        for _ in 0..NODE_AN_HOUR {
            started(&starts, asked(By::Person, None, NOW), "h");
        }
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
    fn every_start_is_written_down_and_read_back_past_a_line_it_cant_read() {
        let (dir, starts) = book();
        started(&starts, agent("OSPREY", NOW), "h-1");
        starts.stop().expect("stopped");
        starts.admit(agent("OSPREY", NOW)).expect_err("stopped");
        let file = dir.path().join("starts.jsonl");
        let mut written = std::fs::read_to_string(&file).expect("the book");
        written.push_str("not a start\n");
        std::fs::write(&file, written).expect("a line it can't read");
        let again = Starts::at(file);
        let kept = again.newest(10);
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert_eq!(kept[0].held.as_deref(), Some("h-1"));
        assert!(kept[1].refused.is_some(), "{kept:?}");
        assert_eq!(again.started_by("kestrel", "h-1").as_deref(), Some("OSPREY"));
        assert_eq!(again.started_by("osprey", "h-1"), None, "another machine's");
        assert!(again.stopped(), "the stop outlives the node");
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
