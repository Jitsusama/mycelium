//! The node's log of placements: each time it places work, what it knew
//! (the work, every machine's facts and load), how it judged each machine
//! and where the work went, a JSON line each in a file a day. It is what
//! a monitor reads to say whether placement chose well, and what a replay
//! places again under other weights to say whether they would choose
//! better, so it keeps everything placing read rather than only the
//! choice.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::facts::Machine;
use crate::placement::{Decision, Loads, Work};

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
    /// How each machine was judged, and where the work went.
    pub decision: Decision,
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

    /// Keeps `record` as a line of `day`'s file.
    ///
    /// # Errors
    ///
    /// When the folder can't be made or the file written.
    pub fn keep(&self, record: &Record, day: jiff::civil::Date) -> io::Result<()> {
        fs::create_dir_all(&self.folder)?;
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        // One write of the whole line, appended, so two placements kept
        // at once never interleave.
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.folder.join(format!("{day}.jsonl")))?
            .write_all(&line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placement::{Judgement, Placement};

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
            },
            machines: Vec::new(),
            loads: Loads::new(),
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
                    preferred: 0,
                    headroom: 2.25,
                    chance: 1.0,
                }],
            },
        }
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
}
