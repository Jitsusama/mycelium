//! The person's own words, as the node put them into its sessions'
//! inboxes, and whether each was taken up and answered.
//!
//! Each steer the node puts from the person, words or an act, is kept as
//! a JSON line in `said.jsonl` in mycelium's folder. The file is the
//! person's words, so only its account reads it (mode 0600), and it is
//! kept short: past [`MOST_LINES`] lines it is written again with only
//! its newest [`KEPT_LINES`]. It is this machine's own and crosses no
//! link: each machine serves what was said through its own node.
//!
//! Words go with a talk, as the launcher's do, so the session's harness
//! says in the talk's file when it took them (`delivered`) and when it
//! answered (`replied`, or `failed`); the node gives words that come
//! without one a talk of their own. A talk to another machine's session
//! is carried back here as it goes, so the marks read the same wherever
//! the session runs. An act opens no talk, and a presence is written for
//! more than a turn, so an act is never marked taken up.

use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::hash::{BuildHasher, RandomState};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The kind the daemon interface holds these as.
pub const KIND: &str = "said";

/// How many of the newest words are served.
pub const SERVED: usize = 200;

/// How many lines the file holds before it is written again shorter.
pub const MOST_LINES: usize = 1_000;

/// How many of its newest lines it keeps when it is.
pub const KEPT_LINES: usize = 500;

/// Who may read and write the file: its account alone.
const MODE: u32 = 0o600;

/// One thing the person said or did to a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Word {
    /// Its id: its talk's, where it has one.
    pub id: String,
    /// When it was said, as an RFC 3339 instant.
    pub at: String,
    /// The session it was said to, by address.
    pub address: String,
    /// The machine that session ran on.
    pub machine: String,
    /// What was said; empty for an act alone.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// What was done besides talk, such as `pause`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub act: Option<String>,
    /// The talk it opened, whose file says what became of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub talk: Option<String>,
}

/// What became of words, as their talk's file says.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Marks {
    /// When the session took them.
    pub taken_up: Option<String>,
    /// When it answered.
    pub answered: Option<String>,
    /// When it ended its turn on them without an answer, and why.
    pub failed: Option<(String, String)>,
}

impl Marks {
    /// What a talk's lines say became of its words: each whole line of
    /// JSON, its `stage` and `at`. A line that isn't one is passed over.
    #[must_use]
    pub fn of(lines: &str) -> Self {
        let mut marks = Self::default();
        for line in lines.lines() {
            let Ok(event) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let at = event["at"].as_str().map(str::to_owned);
            match event["stage"].as_str() {
                Some("delivered") => marks.taken_up = marks.taken_up.or(at),
                Some("replied") => marks.answered = marks.answered.or(at),
                Some("failed") => {
                    let why = event["why"].as_str().unwrap_or_default().to_owned();
                    marks.failed = marks.failed.or(at.map(|at| (at, why)));
                }
                _ => {}
            }
        }
        marks
    }
}

impl Word {
    /// Its fields as the daemon interface holds them, with `marks`.
    #[must_use]
    pub fn fields(&self, marks: &Marks) -> Value {
        let mut fields = serde_json::to_value(self).unwrap_or_default();
        if let Some(at) = &marks.taken_up {
            fields["taken_up"] = json!(at);
        }
        if let Some(at) = &marks.answered {
            fields["answered"] = json!(at);
        }
        if let Some((at, why)) = &marks.failed {
            fields["failed"] = json!({"at": at, "why": why});
        }
        fields
    }
}

/// A fresh id for a talk, fit to name its file: when it was made, and a
/// random tail keeping two made at once apart.
#[must_use]
pub fn talk_id() -> String {
    // Each `RandomState` is seeded afresh, so its hash of nothing is a
    // random number without a dependency to draw one.
    let tail = RandomState::new().hash_one(()) & 0xffff_ffff;
    format!(
        "said-{}-{tail:08x}",
        jiff::Timestamp::now().as_millisecond()
    )
}

/// The book of the person's words, kept in a file of JSON lines.
#[derive(Debug)]
pub struct Book {
    path: PathBuf,
    kept: Mutex<Kept>,
}

/// What the book holds in memory: the newest words, and how many lines
/// its file holds.
#[derive(Debug, Default)]
struct Kept {
    words: VecDeque<Word>,
    lines: usize,
}

impl Book {
    /// The book kept at `path`, with what it holds already. A line that
    /// isn't a word is passed over; a file not there holds nothing.
    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        let written = fs::read_to_string(&path).unwrap_or_default();
        let lines: Vec<&str> = written.lines().filter(|line| !line.is_empty()).collect();
        let words = lines
            .iter()
            .filter_map(|line| serde_json::from_str::<Word>(line).ok())
            .collect::<Vec<_>>();
        let words = words[words.len().saturating_sub(SERVED)..]
            .iter()
            .cloned()
            .collect();
        Self {
            path,
            kept: Mutex::new(Kept {
                words,
                lines: lines.len(),
            }),
        }
    }

    /// Keeps `word`, adding its line to the file, which only its account
    /// may read, and writing the file again with its newest lines once it
    /// holds too many.
    ///
    /// # Errors
    ///
    /// When the file can't be written.
    pub fn keep(&self, word: Word) -> io::Result<()> {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let line = serde_json::to_string(&word).map_err(io::Error::other)?;
        if let Some(folder) = self.path.parent() {
            fs::create_dir_all(folder)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(MODE)
            .open(&self.path)?;
        // A file made before by someone else is held to its account too.
        file.set_permissions(fs::Permissions::from_mode(MODE))?;
        writeln!(file, "{line}")?;
        kept.lines += 1;
        kept.words.push_back(word);
        while kept.words.len() > SERVED {
            kept.words.pop_front();
        }
        if kept.lines > MOST_LINES {
            kept.lines = self.shorten()?;
        }
        Ok(())
    }

    /// Writes the file again with only its newest lines, through a file
    /// renamed into place, answering how many it holds.
    fn shorten(&self) -> io::Result<usize> {
        let written = fs::read_to_string(&self.path)?;
        let lines: Vec<&str> = written.lines().filter(|line| !line.is_empty()).collect();
        let newest = &lines[lines.len().saturating_sub(KEPT_LINES)..];
        let writing = self.path.with_extension("jsonl.tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(MODE)
            .open(&writing)?;
        file.set_permissions(fs::Permissions::from_mode(MODE))?;
        for line in newest {
            writeln!(file, "{line}")?;
        }
        fs::rename(&writing, &self.path)?;
        Ok(newest.len())
    }

    /// The newest words, oldest first.
    #[must_use]
    pub fn words(&self) -> Vec<Word> {
        let kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        kept.words.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(n: usize) -> Word {
        Word {
            id: format!("said-{n}"),
            at: "2026-11-15T12:00:00Z".to_owned(),
            address: "KESTRL".to_owned(),
            machine: "skiff".to_owned(),
            text: format!("word {n}"),
            act: None,
            talk: Some(format!("said-{n}")),
        }
    }

    #[test]
    fn words_are_kept_where_only_their_account_reads_them() {
        let dir = tempfile::tempdir().expect("a folder");
        let path = dir.path().join("state").join("said.jsonl");
        let book = Book::at(path.clone());
        book.keep(word(1)).expect("kept");
        let mode = fs::metadata(&path).expect("written").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "only its account reads it");
        assert_eq!(
            Book::at(path).words(),
            vec![word(1)],
            "read again as it was kept"
        );
    }

    #[test]
    fn the_file_is_written_shorter_once_it_holds_too_many() {
        let dir = tempfile::tempdir().expect("a folder");
        let path = dir.path().join("said.jsonl");
        let book = Book::at(path.clone());
        for n in 0..=MOST_LINES {
            book.keep(word(n)).expect("kept");
        }
        let lines = fs::read_to_string(&path).expect("written").lines().count();
        assert_eq!(lines, KEPT_LINES, "the file holds only its newest");
        let mode = fs::metadata(&path).expect("written").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "and still only its account reads it");
        let words = Book::at(path).words();
        assert_eq!(words.len(), SERVED, "the newest are served");
        assert_eq!(words.last(), Some(&word(MOST_LINES)), "the last said last");
    }

    #[test]
    fn a_talk_says_when_its_words_were_taken_up_and_answered() {
        let lines = [
            r#"{"stage":"delivered","session":"s","text":"hi","at":"2026-11-15T12:00:05Z"}"#,
            r#"{"stage":"reading","after":"read","at":"2026-11-15T12:00:06Z"}"#,
            "not a line of json",
            r#"{"stage":"replied","text":"done","at":"2026-11-15T12:01:00Z"}"#,
        ]
        .join("\n");
        let marks = Marks::of(&lines);
        assert_eq!(marks.taken_up.as_deref(), Some("2026-11-15T12:00:05Z"));
        assert_eq!(marks.answered.as_deref(), Some("2026-11-15T12:01:00Z"));
        let fields = word(1).fields(&marks);
        assert_eq!(fields["taken_up"], json!("2026-11-15T12:00:05Z"));
        assert_eq!(fields["answered"], json!("2026-11-15T12:01:00Z"));
        assert_eq!(
            word(1).fields(&Marks::of("")),
            json!({"id": "said-1", "at": "2026-11-15T12:00:00Z", "address": "KESTRL",
                   "machine": "skiff", "text": "word 1", "talk": "said-1"}),
            "a talk not taken says only what was said"
        );
    }
}
