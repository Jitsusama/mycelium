//! A web: a quest's shared space of murmurs and the ties between things,
//! kept as one log that every machine sharing the quest's `web` scratch
//! folder reads, and folded into what it says now.
//!
//! Each writer, a machine, appends only to its own file, `events/<writer>.jsonl`,
//! one event a line, numbered in turn, so an event's id, `<writer>-<seq>`, is
//! one no other writer makes. Merging every machine's files is then only a
//! union: nothing conflicts, and their order comes from the lines
//! themselves, by when, then writer, then number. An event is never
//! rewritten or taken back; a murmur is settled by a later event saying how.
//! The quest's share carries the files between machines as it carries any
//! of its scratch, so a machine that was away catches up by reading them.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::store::folders::safe;

/// The folder of a quest's scratch a web lives in.
pub const FOLDER: &str = "web";

/// What an event says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Said {
    /// A line of intent said to nobody, for whoever heeds it.
    Murmur {
        /// Its words.
        words: String,
    },
    /// A tie from one thing to another: a murmur's id, a quest's, a
    /// change's or anything else a reader knows by name.
    Tie {
        /// What it ties.
        from: String,
        /// What it's tied to.
        to: String,
        /// How, in a word or a few, such as "about".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        how: Option<String>,
    },
    /// A murmur settled: filed, tied, answered or let go.
    Settle {
        /// The murmur's id.
        of: String,
        /// How it was settled.
        how: String,
    },
}

/// One line of a writer's file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// `<writer>-<seq>`, which no other writer makes.
    pub id: String,
    /// The machine that wrote it.
    pub writer: String,
    /// Its number among its writer's, from 1.
    pub seq: u64,
    /// When, in whole seconds since the epoch.
    pub at: i64,
    /// Who said it there, such as a session's address, if not the machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// What it says.
    #[serde(flatten)]
    pub said: Said,
}

/// A quest's web, in its scratch folder on this machine.
#[derive(Debug, Clone)]
pub struct Web {
    folder: PathBuf,
}

impl Web {
    /// The web of `quest`, whose scratch folders are in `scratch`.
    ///
    /// # Errors
    ///
    /// When the quest's id isn't safe as a folder's name.
    pub fn of(scratch: &Path, quest: &str) -> io::Result<Self> {
        Ok(Self {
            folder: scratch.join(safe(quest)?).join(FOLDER),
        })
    }

    fn events_folder(&self) -> PathBuf {
        self.folder.join("events")
    }

    /// Appends what `writer` says, by `by` if someone there, at `at`,
    /// numbered after the writer's last, and answers the event written.
    ///
    /// # Errors
    ///
    /// When the writer's name isn't safe as a file's, or its file can't be
    /// read or written.
    pub fn say(&self, writer: &str, by: Option<&str>, said: Said, at: i64) -> io::Result<Event> {
        let _ = (writer, by, said, at);
        Err(io::Error::other("not yet"))
    }

    /// Every writer's events, each once, by when, then writer, then number.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        Vec::new()
    }

    /// Folds the web into `into`, written whole, unless what it holds
    /// already says the same; answers whether it wrote.
    ///
    /// # Errors
    ///
    /// When `into` can't be written.
    pub fn fold_into(&self, into: &Path, at: i64) -> io::Result<bool> {
        let _ = (into, at);
        Ok(false)
    }
}

/// What `events`, in order, say now, as of `at`: each murmur, newest first,
/// with its ties and how it was settled, the ties between other things, and
/// each writer's last number, so a reader can see what it has from whom.
#[must_use]
pub fn fold(events: &[Event], at: i64) -> Value {
    let _ = events;
    json!({"at": at})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn web() -> (tempfile::TempDir, Web) {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let web = Web::of(dir.path(), "QEST-20261013-4UHC5Q").expect("a safe quest");
        (dir, web)
    }

    fn murmur(words: &str) -> Said {
        Said::Murmur {
            words: words.to_owned(),
        }
    }

    #[test]
    fn each_writer_appends_to_its_own_file_numbered_in_turn() {
        let (dir, web) = web();
        let first = web
            .say("optimus", Some("KP0L6A"), murmur("Landed is slow"), 100)
            .expect("optimus says");
        let second = web
            .say("optimus", None, murmur("so is Recent"), 101)
            .expect("optimus says again");
        let far = web
            .say("equilibrius", None, murmur("the dock wakes late"), 100)
            .expect("equilibrius says");
        assert_eq!(
            [first.id.as_str(), second.id.as_str(), far.id.as_str()],
            ["optimus-1", "optimus-2", "equilibrius-1"],
            "each writer numbers its own"
        );
        let events = dir
            .path()
            .join("QEST-20261013-4UHC5Q")
            .join("web")
            .join("events");
        let lines = fs::read_to_string(events.join("optimus.jsonl")).expect("optimus's file");
        assert_eq!(lines.lines().count(), 2, "optimus's two, one a line");
        assert!(lines.ends_with('\n'), "each line is whole, newline and all");
        assert!(
            web.say("../escape", None, murmur("out"), 100).is_err(),
            "a writer names only its own file"
        );
    }

    #[test]
    fn every_writers_events_are_read_once_in_order_leaving_a_half_written_line() {
        let (dir, web) = web();
        let events = dir
            .path()
            .join("QEST-20261013-4UHC5Q")
            .join("web")
            .join("events");
        fs::create_dir_all(&events).expect("the folder is made");
        let line = |id: &str, writer: &str, seq: u64, at: i64| {
            let event = Event {
                id: id.to_owned(),
                writer: writer.to_owned(),
                seq,
                at,
                by: None,
                said: murmur(id),
            };
            serde_json::to_string(&event).expect("an event writes") + "\n"
        };
        let optimus = line("optimus-1", "optimus", 1, 100) + &line("optimus-2", "optimus", 2, 102);
        fs::write(
            events.join("optimus.jsonl"),
            optimus + "{\"id\": \"optimus-3\"",
        )
        .expect("optimus's file");
        // A copy carried twice, as a file moved aside and back may be.
        let far = line("equilibrius-1", "equilibrius", 1, 100).repeat(2)
            + &line("equilibrius-2", "equilibrius", 2, 101);
        fs::write(events.join("equilibrius.jsonl"), far).expect("equilibrius's file");
        let ids: Vec<String> = web.events().into_iter().map(|event| event.id).collect();
        assert_eq!(
            ids,
            ["equilibrius-1", "optimus-1", "equilibrius-2", "optimus-2"],
            "by when, then writer; once each; the half-written line waits"
        );
    }

    #[test]
    fn a_fold_says_each_murmur_newest_first_with_its_ties_and_settling() {
        let event = |writer: &str, seq: u64, at: i64, said: Said| Event {
            id: format!("{writer}-{seq}"),
            writer: writer.to_owned(),
            seq,
            at,
            by: None,
            said,
        };
        let events = [
            event("optimus", 1, 100, murmur("Landed is slow")),
            event("equilibrius", 1, 101, murmur("the dock wakes late")),
            event(
                "optimus",
                2,
                102,
                Said::Tie {
                    from: "optimus-1".to_owned(),
                    to: "QEST-20261106-KP0L6A".to_owned(),
                    how: Some("about".to_owned()),
                },
            ),
            event(
                "equilibrius",
                2,
                103,
                Said::Settle {
                    of: "optimus-1".to_owned(),
                    how: "filed".to_owned(),
                },
            ),
            event(
                "optimus",
                3,
                104,
                Said::Tie {
                    from: "QEST-20261106-KP0L6A".to_owned(),
                    to: "core.nix#3696".to_owned(),
                    how: None,
                },
            ),
        ];
        assert_eq!(
            fold(&events, 200),
            json!({
                "at": 200,
                "writers": {"equilibrius": 2, "optimus": 3},
                "murmurs": [
                    {
                        "id": "equilibrius-1", "writer": "equilibrius", "at": 101,
                        "words": "the dock wakes late", "ties": [], "settled": null,
                    },
                    {
                        "id": "optimus-1", "writer": "optimus", "at": 100,
                        "words": "Landed is slow",
                        "ties": [{
                            "id": "optimus-2", "writer": "optimus", "at": 102,
                            "to": "QEST-20261106-KP0L6A", "how": "about",
                        }],
                        "settled": {
                            "id": "equilibrius-2", "writer": "equilibrius", "at": 103, "how": "filed",
                        },
                    },
                ],
                "ties": [{
                    "id": "optimus-3", "writer": "optimus", "at": 104,
                    "from": "QEST-20261106-KP0L6A", "to": "core.nix#3696",
                }],
            }),
            "a murmur's ties and settling go with it; others stand apart"
        );
    }

    #[test]
    fn a_fold_is_written_only_when_it_says_something_new() {
        let (dir, web) = web();
        let into = dir.path().join("site").join("web.json");
        web.say("optimus", None, murmur("Landed is slow"), 100)
            .expect("optimus says");
        assert!(
            web.fold_into(&into, 200).expect("it folds"),
            "first, it writes"
        );
        assert!(
            !web.fold_into(&into, 201).expect("it folds again"),
            "nothing new, so nothing written, though the time moved"
        );
        web.say("optimus", None, murmur("so is Recent"), 202)
            .expect("optimus says again");
        assert!(
            web.fold_into(&into, 203).expect("it folds"),
            "something new"
        );
        let folded: Value =
            serde_json::from_slice(&fs::read(&into).expect("the fold reads")).expect("JSON");
        assert_eq!(folded["murmurs"][0]["words"], "so is Recent", "{folded}");
    }
}
