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

use rustix::fs::FlockOperation;
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
        let name = format!("{}.jsonl", safe(writer)?);
        let folder = self.events_folder();
        fs::create_dir_all(&folder)?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(folder.join(name))?;
        // Two sessions on one machine write as one writer, so the number
        // is taken and the line written under the file's own lock.
        rustix::fs::flock(&file, FlockOperation::LockExclusive)?;
        let mut held = String::new();
        io::Read::read_to_string(&mut file, &mut held)?;
        let seq = lines_of(&held).map(|event| event.seq).max().unwrap_or(0) + 1;
        let event = Event {
            id: format!("{writer}-{seq}"),
            writer: writer.to_owned(),
            seq,
            at,
            by: by.map(str::to_owned),
            said,
        };
        let mut line = serde_json::to_string(&event).map_err(io::Error::other)?;
        line.push('\n');
        // A line a write cut off ends here, so this one starts whole; the
        // cut one never parses, so it's read as nothing.
        if !held.is_empty() && !held.ends_with('\n') {
            line.insert(0, '\n');
        }
        // In one write, so a reader on another machine sees all of it or
        // none of it, as the share carries the file whole.
        file.write_all(line.as_bytes())?;
        Ok(event)
    }

    /// Every writer's events, each once, by when, then writer, then number.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        let mut events = BTreeMap::new();
        let Ok(files) = fs::read_dir(self.events_folder()) else {
            return Vec::new();
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|kind| kind != "jsonl") {
                continue;
            }
            let Ok(held) = fs::read_to_string(&path) else {
                continue;
            };
            for event in lines_of(&held) {
                events.entry(event.id.clone()).or_insert(event);
            }
        }
        let mut events: Vec<Event> = events.into_values().collect();
        events.sort_by(|one, other| {
            (one.at, &one.writer, one.seq).cmp(&(other.at, &other.writer, other.seq))
        });
        events
    }

    /// Folds the web into `into`, written whole, unless what it holds
    /// already says the same; answers whether it wrote.
    ///
    /// # Errors
    ///
    /// When `into` can't be written.
    pub fn fold_into(&self, into: &Path, at: i64) -> io::Result<bool> {
        let folded = fold(&self.events(), at);
        let same = fs::read(into)
            .ok()
            .and_then(|held| serde_json::from_slice::<Value>(&held).ok())
            .is_some_and(|held| without_at(&held) == without_at(&folded));
        if same {
            return Ok(false);
        }
        let folder = into.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(folder)?;
        // Written aside and moved in, so a page reading it never reads half.
        let staged = folder.join(format!(".web.{}.tmp", std::process::id()));
        let written = serde_json::to_vec(&folded)
            .map_err(io::Error::other)
            .and_then(|bytes| fs::write(&staged, bytes))
            .and_then(|()| fs::rename(&staged, into));
        if written.is_err() {
            let _ = fs::remove_file(&staged);
        }
        written.map(|()| true)
    }
}

/// The whole lines of a writer's file that read as events; a line still
/// being written, or cut off, is none.
fn lines_of(held: &str) -> impl Iterator<Item = Event> + '_ {
    let whole = held.rfind('\n').map_or("", |end| &held[..end]);
    whole
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
}

/// A fold without the time it was made, to tell whether it says anything
/// new.
fn without_at(folded: &Value) -> Value {
    let mut folded = folded.clone();
    if let Some(fields) = folded.as_object_mut() {
        fields.remove("at");
    }
    folded
}

/// What says who, if it was someone at the writer rather than the writer.
fn with_by(mut said: Value, by: Option<&String>) -> Value {
    if let (Some(by), Some(fields)) = (by, said.as_object_mut()) {
        fields.insert("by".to_owned(), json!(by));
    }
    said
}

/// What `events`, in order, say now, as of `at`: each murmur, newest first,
/// with its ties and how it was settled, the ties between other things, and
/// each writer's last number, so a reader can see what it has from whom.
#[must_use]
pub fn fold(events: &[Event], at: i64) -> Value {
    let mut writers: BTreeMap<&str, u64> = BTreeMap::new();
    let mut murmurs: BTreeMap<&str, Value> = BTreeMap::new();
    for event in events {
        let last = writers.entry(&event.writer).or_default();
        *last = (*last).max(event.seq);
        if let Said::Murmur { words } = &event.said {
            let murmur = json!({
                "id": event.id, "writer": event.writer, "at": event.at,
                "words": words, "ties": [], "settled": null,
            });
            murmurs.insert(&event.id, with_by(murmur, event.by.as_ref()));
        }
    }
    let mut ties = Vec::new();
    for event in events {
        let made = |fields: Value| {
            let mut made = json!({"id": event.id, "writer": event.writer, "at": event.at});
            if let (Some(made), Some(fields)) = (made.as_object_mut(), fields.as_object()) {
                made.extend(fields.clone());
            }
            with_by(made, event.by.as_ref())
        };
        match &event.said {
            Said::Murmur { .. } => {}
            Said::Tie { from, to, how } => {
                let how = how.as_ref().map(|how| ("how", json!(how)));
                let tie = |end: &str, other: &str| {
                    let mut fields = serde_json::Map::from_iter([(end.to_owned(), json!(other))]);
                    fields.extend(how.clone().map(|(key, how)| (key.to_owned(), how)));
                    made(Value::Object(fields))
                };
                // A tie from or to a murmur goes with it, where it's read.
                if let Some(murmur) = murmurs.get_mut(from.as_str()) {
                    push(murmur, "ties", tie("to", to));
                } else if let Some(murmur) = murmurs.get_mut(to.as_str()) {
                    push(murmur, "ties", tie("from", from));
                } else {
                    let mut fields = serde_json::Map::from_iter([("from".to_owned(), json!(from))]);
                    fields.insert("to".to_owned(), json!(to));
                    fields.extend(how.map(|(key, how)| (key.to_owned(), how)));
                    ties.push(made(Value::Object(fields)));
                }
            }
            Said::Settle { of, how } => {
                // The last settling said stands, as events come in order.
                if let Some(murmur) = murmurs.get_mut(of.as_str()) {
                    murmur["settled"] = made(json!({"how": how}));
                }
            }
        }
    }
    let mut murmurs: Vec<Value> = murmurs.into_values().collect();
    let order = |murmur: &Value| {
        (
            murmur["at"].as_i64().unwrap_or(0),
            murmur["writer"].as_str().unwrap_or("").to_owned(),
            murmur["id"].as_str().unwrap_or("").to_owned(),
        )
    };
    murmurs.sort_by_key(|murmur| std::cmp::Reverse(order(murmur)));
    json!({"at": at, "writers": writers, "murmurs": murmurs, "ties": ties})
}

/// Adds `value` to the list at `key` in `into`.
fn push(into: &mut Value, key: &str, value: Value) {
    if let Some(list) = into[key].as_array_mut() {
        list.push(value);
    }
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
