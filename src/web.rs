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
//!
//! Every reader reads a web's murmurs one way, through [`Reading`], which
//! reads each file once and then only what was appended to it, and
//! [`murmurs`], which weaves each murmur with its ties and settling: the
//! fold a page reads, and the node's `murmur` kind.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read as _, Seek as _, Write};
use std::os::unix::fs::MetadataExt as _;
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

    /// The folder each writer's file is kept in.
    #[must_use]
    pub fn events_folder(&self) -> PathBuf {
        events_in(&self.folder)
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
        let mut reading = Reading::default();
        reading.read(&self.events_folder());
        reading.events()
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
        let beside = into.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(beside)?;
        // Written aside and moved in, so a page reading it never reads half.
        let staged = beside.join(format!(".web.{}.tmp", std::process::id()));
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

/// The folder of the writers' files in the web kept in `web`.
#[must_use]
pub fn events_in(web: &Path) -> PathBuf {
    web.join("events")
}

/// The whole lines of a writer's file that read as events; a line still
/// being written, or cut off, is none.
fn lines_of(held: &str) -> impl Iterator<Item = Event> + '_ {
    let whole = held.rfind('\n').map_or("", |end| &held[..end]);
    whole
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
}

/// A web's writers' files, read as they grow: each file read once, then,
/// while it is the same file grown, only what was appended since, and not
/// opened at all while its size and time say it is as it was. A file
/// replaced, cut shorter or gone is read again whole, or let go.
#[derive(Debug, Default)]
pub struct Reading {
    files: BTreeMap<PathBuf, Kept>,
}

/// What one writer's file held when last read.
#[derive(Debug)]
struct Kept {
    /// The file it was, its size and when it was written.
    stamp: Stamp,
    /// How far its whole lines reach, where the next read starts.
    read_to: u64,
    /// Its events, from its whole lines.
    events: Vec<Event>,
}

/// What tells a file is as it was: which file, how long, when written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    file: (u64, u64),
    size: u64,
    written: (i64, i64),
}

impl Stamp {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            file: (metadata.dev(), metadata.ino()),
            size: metadata.len(),
            written: (metadata.mtime(), metadata.mtime_nsec()),
        }
    }
}

impl Reading {
    /// Reads the writers' files in `folder` as they stand now, opening
    /// only those changed since, and answers whether any were.
    pub fn read(&mut self, folder: &Path) -> bool {
        self.files.clear(); // RED STUB
        let mut seen = Vec::new();
        let mut changed = false;
        for file in fs::read_dir(folder).into_iter().flatten().flatten() {
            let path = file.path();
            if path.extension().is_none_or(|kind| kind != "jsonl") {
                continue;
            }
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            let stamp = Stamp::of(&metadata);
            seen.push(path.clone());
            let kept = self.files.get_mut(&path);
            if kept.as_ref().is_some_and(|kept| kept.stamp == stamp) {
                continue;
            }
            // The same file grown is read from where its lines ended;
            // any other is read whole.
            let grown =
                kept.filter(|kept| kept.stamp.file == stamp.file && kept.stamp.size <= stamp.size);
            changed |= match grown {
                Some(kept) => read_on(&path, kept, stamp),
                None => {
                    let mut kept = Kept {
                        stamp,
                        read_to: 0,
                        events: Vec::new(),
                    };
                    read_on(&path, &mut kept, stamp);
                    self.files.insert(path, kept);
                    true
                }
            };
        }
        let before = self.files.len();
        self.files.retain(|path, _| seen.contains(path));
        changed || self.files.len() != before
    }

    /// Every writer's events, each once, by when, then writer, then
    /// number.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        let mut events = BTreeMap::new();
        for kept in self.files.values() {
            for event in &kept.events {
                events
                    .entry(event.id.as_str())
                    .or_insert_with(|| event.clone());
            }
        }
        let mut events: Vec<Event> = events.into_values().collect();
        events.sort_by(|one, other| {
            (one.at, &one.writer, one.seq).cmp(&(other.at, &other.writer, other.seq))
        });
        events
    }
}

/// Reads `path` on from where `kept` last stopped, keeping its new whole
/// lines' events and `stamp`; answers whether it read any. A file that
/// can't be read now keeps what it held, to be read again at its next
/// change.
fn read_on(path: &Path, kept: &mut Kept, stamp: Stamp) -> bool {
    let mut added = String::new();
    let read = fs::File::open(path).and_then(|mut file| {
        file.seek(io::SeekFrom::Start(kept.read_to))?;
        file.read_to_string(&mut added)
    });
    if read.is_err() {
        return false;
    }
    kept.stamp = stamp;
    let Some(end) = added.rfind('\n') else {
        return false;
    };
    kept.read_to += u64::try_from(end + 1).unwrap_or(u64::MAX);
    let before = kept.events.len();
    kept.events.extend(
        added[..end]
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok()),
    );
    kept.events.len() != before
}

/// A murmur as its web says it now: what was said, what it is tied to and
/// how it was settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Murmur<'a> {
    /// The event that said it.
    pub said: &'a Event,
    /// Its words.
    pub words: &'a str,
    /// Its ties, in the order they were made.
    pub ties: Vec<Tied<'a>>,
    /// The last settling said of it, and how, if any.
    pub settled: Option<(&'a Event, &'a str)>,
}

impl Murmur<'_> {
    /// Who said it: the one at its writer who did, else the writer.
    #[must_use]
    pub fn cell(&self) -> &str {
        self.said.by.as_deref().unwrap_or(&self.said.writer)
    }

    /// What its ties name at their other ends, each once, in the order
    /// they were made.
    #[must_use]
    pub fn tied_to(&self) -> Vec<&str> {
        if std::hint::black_box(true) {
            return Vec::new(); // RED STUB
        }
        let mut named: Vec<&str> = Vec::new();
        for tie in &self.ties {
            if !named.contains(&tie.other) {
                named.push(tie.other);
            }
        }
        named
    }
}

/// One of a murmur's ties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tied<'a> {
    /// The event that made it.
    pub made: &'a Event,
    /// Whether it runs from the murmur to the other end, or to it.
    pub from_murmur: bool,
    /// What it names at its other end.
    pub other: &'a str,
    /// How, if it says.
    pub how: Option<&'a str>,
}

/// What a web's events weave: each murmur, newest first, with its ties and
/// settling, and the ties between other things.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Woven<'a> {
    /// Each murmur, newest first: by when, then writer, then id.
    pub murmurs: Vec<Murmur<'a>>,
    /// The ties neither of whose ends is a murmur, in order.
    pub apart: Vec<&'a Event>,
}

/// What `events`, in order, say of their murmurs now, as every reader of
/// a web reads them.
#[must_use]
pub fn murmurs(events: &[Event]) -> Woven<'_> {
    let mut murmurs: BTreeMap<&str, Murmur<'_>> = BTreeMap::new();
    for event in events {
        if let Said::Murmur { words } = &event.said {
            let murmur = Murmur {
                said: event,
                words,
                ties: Vec::new(),
                settled: None,
            };
            murmurs.insert(&event.id, murmur);
        }
    }
    let mut apart = Vec::new();
    for event in events {
        match &event.said {
            Said::Murmur { .. } => {}
            Said::Tie { from, to, how } => {
                // A tie from or to a murmur goes with it, where it's read.
                let (murmur, from_murmur, other) = if murmurs.contains_key(from.as_str()) {
                    (murmurs.get_mut(from.as_str()), true, to)
                } else {
                    (murmurs.get_mut(to.as_str()), false, from)
                };
                match murmur {
                    Some(murmur) => murmur.ties.push(Tied {
                        made: event,
                        from_murmur,
                        other: other.as_str(),
                        how: how.as_deref(),
                    }),
                    None => apart.push(event),
                }
            }
            Said::Settle { of, how } => {
                // The last settling said stands, as events come in order.
                if let Some(murmur) = murmurs.get_mut(of.as_str()) {
                    murmur.settled = Some((event, how));
                }
            }
        }
    }
    let mut murmurs: Vec<Murmur<'_>> = murmurs.into_values().collect();
    murmurs.sort_by(|one, other| {
        (other.said.at, &other.said.writer, &other.said.id).cmp(&(
            one.said.at,
            &one.said.writer,
            &one.said.id,
        ))
    });
    Woven { murmurs, apart }
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

/// `event`'s id, writer and time with `fields`, and who said it, if it
/// was someone at the writer rather than the writer.
fn made(event: &Event, fields: Value) -> Value {
    let mut made = json!({"id": event.id, "writer": event.writer, "at": event.at});
    if let (Some(made), Value::Object(fields)) = (made.as_object_mut(), fields) {
        made.extend(fields);
        if let Some(by) = &event.by {
            made.insert("by".to_owned(), json!(by));
        }
    }
    made
}

/// What `events`, in order, say now, as of `at`: each murmur, newest first,
/// with its ties and how it was settled, the ties between other things, and
/// each writer's last number, so a reader can see what it has from whom.
#[must_use]
pub fn fold(events: &[Event], at: i64) -> Value {
    let mut writers: BTreeMap<&str, u64> = BTreeMap::new();
    for event in events {
        let last = writers.entry(&event.writer).or_default();
        *last = (*last).max(event.seq);
    }
    let woven = murmurs(events);
    let murmurs: Vec<Value> = woven.murmurs.iter().map(folded).collect();
    let ties: Vec<Value> = woven
        .apart
        .iter()
        .filter_map(|event| match &event.said {
            Said::Tie { from, to, how } => {
                let mut tie = json!({"from": from, "to": to});
                if let Some(how) = how {
                    tie["how"] = json!(how);
                }
                Some(made(event, tie))
            }
            _ => None,
        })
        .collect();
    json!({"at": at, "writers": writers, "murmurs": murmurs, "ties": ties})
}

/// A murmur as a fold says it: its words, each tie by its other end and
/// how, and its settling.
fn folded(murmur: &Murmur<'_>) -> Value {
    let ties: Vec<Value> = murmur
        .ties
        .iter()
        .map(|tie| {
            let end = if tie.from_murmur { "to" } else { "from" };
            let mut fields = json!({ end: tie.other });
            if let Some(how) = tie.how {
                fields["how"] = json!(how);
            }
            made(tie.made, fields)
        })
        .collect();
    let settled = murmur
        .settled
        .map_or(Value::Null, |(event, how)| made(event, json!({"how": how})));
    made(
        murmur.said,
        json!({"words": murmur.words, "ties": ties, "settled": settled}),
    )
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
            .say("skiff", Some("KP0L6A"), murmur("Landed is slow"), 100)
            .expect("skiff says");
        let second = web
            .say("skiff", None, murmur("so is Recent"), 101)
            .expect("skiff says again");
        let far = web
            .say("kestrel", None, murmur("the dock wakes late"), 100)
            .expect("kestrel says");
        assert_eq!(
            [first.id.as_str(), second.id.as_str(), far.id.as_str()],
            ["skiff-1", "skiff-2", "kestrel-1"],
            "each writer numbers its own"
        );
        let events = dir
            .path()
            .join("QEST-20261013-4UHC5Q")
            .join("web")
            .join("events");
        let lines = fs::read_to_string(events.join("skiff.jsonl")).expect("skiff's file");
        assert_eq!(lines.lines().count(), 2, "skiff's two, one a line");
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
        let skiff = line("skiff-1", "skiff", 1, 100) + &line("skiff-2", "skiff", 2, 102);
        fs::write(events.join("skiff.jsonl"), skiff + "{\"id\": \"skiff-3\"")
            .expect("skiff's file");
        // A copy carried twice, as a file moved aside and back may be.
        let far =
            line("kestrel-1", "kestrel", 1, 100).repeat(2) + &line("kestrel-2", "kestrel", 2, 101);
        fs::write(events.join("kestrel.jsonl"), far).expect("kestrel's file");
        let ids: Vec<String> = web.events().into_iter().map(|event| event.id).collect();
        assert_eq!(
            ids,
            ["kestrel-1", "skiff-1", "kestrel-2", "skiff-2"],
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
            event("skiff", 1, 100, murmur("Landed is slow")),
            event("kestrel", 1, 101, murmur("the dock wakes late")),
            event(
                "skiff",
                2,
                102,
                Said::Tie {
                    from: "skiff-1".to_owned(),
                    to: "QEST-20261106-KP0L6A".to_owned(),
                    how: Some("about".to_owned()),
                },
            ),
            event(
                "kestrel",
                2,
                103,
                Said::Settle {
                    of: "skiff-1".to_owned(),
                    how: "filed".to_owned(),
                },
            ),
            event(
                "skiff",
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
                "writers": {"kestrel": 2, "skiff": 3},
                "murmurs": [
                    {
                        "id": "kestrel-1", "writer": "kestrel", "at": 101,
                        "words": "the dock wakes late", "ties": [], "settled": null,
                    },
                    {
                        "id": "skiff-1", "writer": "skiff", "at": 100,
                        "words": "Landed is slow",
                        "ties": [{
                            "id": "skiff-2", "writer": "skiff", "at": 102,
                            "to": "QEST-20261106-KP0L6A", "how": "about",
                        }],
                        "settled": {
                            "id": "kestrel-2", "writer": "kestrel", "at": 103, "how": "filed",
                        },
                    },
                ],
                "ties": [{
                    "id": "skiff-3", "writer": "skiff", "at": 104,
                    "from": "QEST-20261106-KP0L6A", "to": "core.nix#3696",
                }],
            }),
            "a murmur's ties and settling go with it; others stand apart"
        );
    }

    #[test]
    fn a_murmur_is_woven_with_its_cell_what_it_is_tied_to_and_its_settling() {
        let event = |seq: u64, by: Option<&str>, said: Said| Event {
            id: format!("skiff-{seq}"),
            writer: "skiff".to_owned(),
            seq,
            at: 100 + i64::try_from(seq).expect("a small number"),
            by: by.map(str::to_owned),
            said,
        };
        let tie = |from: &str, to: &str| Said::Tie {
            from: from.to_owned(),
            to: to.to_owned(),
            how: None,
        };
        let events = [
            // Tied before it is said, as another machine's file may be read
            // first, it is still its tie.
            event(1, None, tie("mycelium#211", "skiff-2")),
            event(2, Some("KP0L6A"), murmur("Landed is slow")),
            event(3, None, murmur("the dock wakes late")),
            event(4, None, tie("skiff-2", "mycelium")),
            event(5, None, tie("skiff-2", "mycelium")),
            event(
                6,
                None,
                Said::Settle {
                    of: "skiff-2".to_owned(),
                    how: "filed".to_owned(),
                },
            ),
        ];
        let woven = murmurs(&events);
        let said: Vec<(&str, &str, Vec<&str>, Option<&str>)> = woven
            .murmurs
            .iter()
            .map(|murmur| {
                (
                    murmur.said.id.as_str(),
                    murmur.cell(),
                    murmur.tied_to(),
                    murmur.settled.map(|(_, how)| how),
                )
            })
            .collect();
        assert_eq!(
            said,
            [
                ("skiff-3", "skiff", vec![], None),
                (
                    "skiff-2",
                    "KP0L6A",
                    vec!["mycelium#211", "mycelium"],
                    Some("filed")
                ),
            ],
            "newest first, said by whoever said it, each tie's other end once"
        );
        assert!(woven.apart.is_empty(), "every tie here is a murmur's");
    }

    /// Writes `lines` over the start of `file` in place, its size and time
    /// kept as they were, as only a reader opening it could tell.
    fn overwritten_unseen(file: &Path, lines: &str) {
        let written = fs::metadata(file)
            .and_then(|metadata| metadata.modified())
            .expect("its time");
        let mut open = fs::OpenOptions::new()
            .write(true)
            .open(file)
            .expect("it opens");
        open.write_all(lines.as_bytes()).expect("written over");
        open.set_modified(written).expect("its time kept");
    }

    fn words_of(reading: &Reading) -> Vec<String> {
        reading
            .events()
            .into_iter()
            .filter_map(|event| match event.said {
                Said::Murmur { words } => Some(words),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_reading_opens_a_file_only_once_it_changed_and_reads_on_from_its_end() {
        let (dir, web) = web();
        web.say("skiff", None, murmur("aaaa"), 100)
            .expect("skiff says");
        let file = web.events_folder().join("skiff.jsonl");
        let mut reading = Reading::default();
        assert!(reading.read(&web.events_folder()), "first, it reads");
        let first = fs::read_to_string(&file).expect("the file");
        overwritten_unseen(&file, &first.replace("aaaa", "bbbb"));
        assert!(
            !reading.read(&web.events_folder()),
            "unchanged in size and time, it isn't opened"
        );
        assert_eq!(words_of(&reading), ["aaaa"]);
        overwritten_unseen(&file, &first.replace("aaaa", "cccc"));
        web.say("skiff", None, murmur("dddd"), 101)
            .expect("skiff says again");
        assert!(reading.read(&web.events_folder()), "grown, it reads on");
        assert_eq!(
            words_of(&reading),
            ["aaaa", "dddd"],
            "only what was appended is read"
        );
        let beside = dir.path().join("replacing.jsonl");
        fs::write(&beside, first.replace("aaaa", "eeee")).expect("a new file");
        fs::rename(&beside, &file).expect("moved into place");
        assert!(reading.read(&web.events_folder()), "replaced, it reads");
        assert_eq!(words_of(&reading), ["eeee"], "a new file is read whole");
        fs::remove_file(&file).expect("the file goes");
        assert!(reading.read(&web.events_folder()), "gone, it changed");
        assert!(reading.events().is_empty(), "and its events with it");
    }

    #[test]
    fn a_fold_is_written_only_when_it_says_something_new() {
        let (dir, web) = web();
        let into = dir.path().join("site").join("web.json");
        web.say("skiff", None, murmur("Landed is slow"), 100)
            .expect("skiff says");
        assert!(
            web.fold_into(&into, 200).expect("it folds"),
            "first, it writes"
        );
        assert!(
            !web.fold_into(&into, 201).expect("it folds again"),
            "nothing new, so nothing written, though the time moved"
        );
        web.say("skiff", None, murmur("so is Recent"), 202)
            .expect("skiff says again");
        assert!(
            web.fold_into(&into, 203).expect("it folds"),
            "something new"
        );
        let folded: Value =
            serde_json::from_slice(&fs::read(&into).expect("the fold reads")).expect("JSON");
        assert_eq!(folded["murmurs"][0]["words"], "so is Recent", "{folded}");
    }
}
