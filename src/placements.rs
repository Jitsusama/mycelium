//! The node's log of placements: each time it places work, what it knew
//! (the work, every machine's facts and load), how it judged each machine
//! and where the work went, a JSON line each in a file a day. It is what
//! a monitor reads to say whether placement chose well, and what a replay
//! places again under other weights to say whether they would choose
//! better, so it keeps everything placing read rather than only the
//! choice.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::facts::Machine;
use crate::placement::{Decision, Loads, Miss, Work};

/// How many days back a day's file is kept, counted from the newest: two
/// weeks of placements is enough to tune weights on, and each line
/// carries every machine's facts, so the log is bounded on a disk that
/// may already be filling.
pub const KEPT_DAYS: i64 = 14;

/// One placement, as the node made it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// When, as an RFC 3339 time.
    pub at: String,
    /// Whether it placed a start, which then counts as load, or only
    /// answered where work would go.
    pub start: bool,
    /// The work placed.
    pub work: Work,
    /// Every machine's facts, as placing read them.
    pub machines: Vec<Machine>,
    /// Every machine's load beyond what its room says.
    pub loads: Loads,
    /// The machines refused before they were judged, and why: those whose
    /// ground is looser than the asker's fence, so a replay refuses them
    /// too.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused: Vec<Miss>,
    /// The fence placement held the asker's work to, as the node judged
    /// the asker, so a refusal for looser ground, or its absence, reads
    /// off the record: none where it was placed unfenced, as the person's
    /// work is and a start relayed here, whose asker the asking machine
    /// judged. None too in a record kept before it was said, or one
    /// saying a fence this build doesn't know.
    #[serde(
        default,
        deserialize_with = "or_none",
        skip_serializing_if = "Option::is_none"
    )]
    pub fence: Option<crate::declared::Kind>,
    /// The screen an agent's work was placed through beside its fence,
    /// where it was fenced, so a Mac taking an agent's held start reads
    /// as the run's screen the agent shown unfenced is placed through,
    /// rather than as a refusal missing. None where `fence` is.
    #[serde(
        default,
        deserialize_with = "or_none",
        skip_serializing_if = "Option::is_none"
    )]
    pub screen: Option<Screen>,
    /// How each machine was judged, and where the work went.
    pub decision: Decision,
}

/// What placement holds an agent's work to, beside ground as firm as
/// where it asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Screen {
    /// Firm ground alone.
    Ground,
    /// An agent's run: only where its fence runs a command.
    Run,
    /// An agent's held start: where its fence runs a command, and on no
    /// Mac, whose holder would start it outside any fence.
    Held,
}

/// A field as said, or none where it isn't in the shape this build reads,
/// which spoils only that field, never the rest of the record.
fn or_none<'de, D: serde::Deserializer<'de>, T: DeserializeOwned>(
    said: D,
) -> Result<Option<T>, D::Error> {
    Ok(Option::<serde_json::Value>::deserialize(said)?
        .and_then(|field| serde_json::from_value(field).ok()))
}

/// Where a node keeps its placements: a folder of `<date>.jsonl` files,
/// each day's by the machine's own calendar.
#[derive(Debug, Clone)]
pub struct Log {
    folder: PathBuf,
}

impl Log {
    /// The log kept in `folder`.
    #[must_use]
    pub fn at(folder: PathBuf) -> Self {
        Self { folder }
    }

    /// Keeps `record` as a line of `day`'s file, letting go of the days
    /// more than [`KEPT_DAYS`] before it when that file is new.
    ///
    /// # Errors
    ///
    /// When the folder can't be made or the file written.
    pub fn keep(&self, record: &Record, day: jiff::civil::Date) -> io::Result<()> {
        fs::create_dir_all(&self.folder)?;
        let file = self.folder.join(format!("{day}.jsonl"));
        if !file.exists() {
            let_go_before(&self.folder, day);
        }
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        // One write of the whole line, appended, so two placements kept
        // at once never interleave.
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)?
            .write_all(&line)
    }
}

/// Removes each `<date>.jsonl` file in `folder` from more than
/// [`KEPT_DAYS`] before `day`, leaving anything not named as a day.
pub(crate) fn let_go_before(folder: &Path, day: jiff::civil::Date) {
    let oldest = day.saturating_sub(jiff::Span::new().days(KEPT_DAYS));
    let Ok(entries) = fs::read_dir(folder) else {
        // Unread, nothing is let go; the next new day tries again.
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let named = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".jsonl"))
            .and_then(|stem| stem.parse::<jiff::civil::Date>().ok());
        if named.is_some_and(|named| named < oldest)
            && let Err(error) = fs::remove_file(&path)
        {
            // An old day left costs disk, never what the day keeps.
            eprintln!("{} wasn't let go: {error}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placement::{Judgement, Placement};
    use serde_json::json;

    fn record(machine: &str) -> Record {
        Record {
            at: "2026-11-16T01:20:00Z".to_owned(),
            start: true,
            work: Work {
                needs: Vec::new(),
                prefers: Vec::new(),
                tolerates: Vec::new(),
                may_wait: false,
                lasts: None,
                fence: None,
            },
            machines: Vec::new(),
            loads: Loads::new(),
            refused: Vec::new(),
            fence: None,
            screen: None,
            decision: Decision {
                placement: Placement::Placed {
                    machine: machine.to_owned(),
                    passed: Vec::new(),
                },
                judged: vec![Judgement {
                    machine: machine.to_owned(),
                    qualifies: true,
                    over: false,
                    unheard: false,
                    saturated: false,
                    preferred: 0,
                    headroom: 2.25,
                    chance: 1.0,
                }],
                weights: crate::placement::Weights::STARTING,
            },
        }
    }

    #[test]
    fn a_new_days_file_lets_go_of_days_past_the_fortnight() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let kept = folder.path().join("placements");
        fs::create_dir_all(&kept).expect("the folder");
        for name in [
            "2026-11-01.jsonl",
            "2026-11-02.jsonl",
            "2026-11-03.jsonl",
            "notes.txt",
        ] {
            fs::write(kept.join(name), "").expect("a file");
        }
        let log = Log::at(kept.clone());
        log.keep(&record("kestrel"), jiff::civil::date(2026, 11, 16))
            .expect("kept");
        let mut names: Vec<String> = fs::read_dir(&kept)
            .expect("the folder")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "2026-11-02.jsonl",
                "2026-11-03.jsonl",
                "2026-11-16.jsonl",
                "notes.txt"
            ],
            "the fifteenth day back goes; the fortnight and what isn't a day stay"
        );
    }

    #[test]
    fn each_placement_is_a_line_of_its_days_file() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let log = Log::at(folder.path().join("placements"));
        let day = jiff::civil::date(2026, 11, 16);
        log.keep(&record("kestrel"), day).expect("kept");
        log.keep(&record("skiff"), day).expect("kept");
        log.keep(&record("tern"), day.tomorrow().expect("a day"))
            .expect("kept");
        let read = |name: &str| -> Vec<Record> {
            fs::read_to_string(folder.path().join("placements").join(name))
                .expect("the day's file")
                .lines()
                .map(|line| serde_json::from_str(line).expect("a record"))
                .collect()
        };
        assert_eq!(
            read("2026-11-16.jsonl"),
            [record("kestrel"), record("skiff")],
            "the day's two, in order, read back whole"
        );
        assert_eq!(
            read("2026-11-17.jsonl"),
            [record("tern")],
            "the next day's in its own file"
        );
    }

    #[test]
    fn a_record_says_the_askers_fence_and_screen_only_where_the_asker_was_fenced() {
        use crate::declared::Kind;

        let person = serde_json::to_value(record("kestrel")).expect("a record");
        assert_eq!(
            (person.get("fence"), person.get("screen")),
            (None, None),
            "the person's says neither: {person}"
        );
        for (fence, screen, said_as) in [
            (Kind::Fence, Screen::Held, ("fence", "held")),
            (Kind::Strict, Screen::Run, ("strict", "run")),
            (Kind::Fence, Screen::Ground, ("fence", "ground")),
        ] {
            let fenced = Record {
                fence: Some(fence),
                screen: Some(screen),
                ..record("kestrel")
            };
            let said = serde_json::to_value(&fenced).expect("a record");
            assert_eq!(
                (&said["fence"], &said["screen"]),
                (&json!(said_as.0), &json!(said_as.1))
            );
            let kept: Record = serde_json::from_value(said).expect("a record");
            assert_eq!(kept, fenced, "read back whole");
        }

        let mut line = person.clone();
        line.as_object_mut()
            .expect("a record is an object")
            .retain(|field, _| !matches!(field.as_str(), "refused" | "fence" | "screen"));
        let kept: Record = serde_json::from_value(line.clone()).expect("a record");
        assert_eq!(
            (kept.fence, kept.screen),
            (None, None),
            "a line kept before either was said reads as neither"
        );
        line["fence"] = json!("porous");
        line["screen"] = json!({"later": true});
        let kept: Record = serde_json::from_value(line).expect("the rest still reads");
        assert_eq!(
            (kept.fence, kept.screen, kept.work),
            (None, None, record("kestrel").work),
            "a fence or screen this build doesn't know spoils only itself"
        );
    }
}
