//! The carries this node ordered, as the person sees them: each line
//! `carries.jsonl` keeps (`carry::ordered`), with how it stands now,
//! judged from what the node itself sees rather than from anything the
//! session or its orchestrator says.
//!
//! A carry stands refused once its session writes a refusal since it was
//! ordered; carrying once the session leaves here, by a done file written
//! since or by ending; arrived once it runs on the machine it was carried
//! to; and back once it runs here again after leaving. Until any of
//! those, it stands ordered. The node keeps no state of its own for it,
//! so a node started afresh judges each as it did before.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::{Map, Value};

/// The kind of record a carry is in the daemon interface.
pub const KIND: &str = "carry";

/// How long a carry is shown after it was ordered.
pub const SHOWN_FOR: jiff::SignedDuration = jiff::SignedDuration::from_hours(24);

/// How a carry stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Told to its session, which hasn't gone or refused yet.
    Ordered,
    /// Refused by its session, saying why.
    Refused(String),
    /// Gone from here, and not yet running where it was carried.
    Carrying,
    /// Running where it was carried.
    Arrived,
    /// Running here again, after it left.
    Back,
}

/// What the node sees of a carry's session since the carry was ordered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Seen {
    /// Why it refused, when it wrote a refusal since.
    pub refused: Option<String>,
    /// Whether it wrote its done file here since, as a session leaving
    /// does.
    pub left: bool,
    /// The machine it runs on now, when it runs anywhere.
    pub runs_on: Option<String>,
}

/// A carry as kept: its id, when it was ordered, and its line.
#[derive(Debug, Clone, PartialEq)]
pub struct Kept {
    /// Its id, its session's and when it was ordered.
    pub id: String,
    /// When it was ordered.
    pub at: jiff::Timestamp,
    /// The line kept for it.
    pub line: Value,
}

impl Kept {
    /// The text of the field `name` its line holds, or nothing.
    #[must_use]
    pub fn said(&self, name: &str) -> &str {
        self.line.get(name).and_then(Value::as_str).unwrap_or("")
    }
}

/// The carries `file` keeps that were ordered since `since`, newest
/// first.
///
/// # Errors
///
/// When the file is there but can't be read.
pub fn read(file: &Path, since: jiff::Timestamp) -> io::Result<Vec<Kept>> {
    let lines = match fs::read_to_string(file) {
        Ok(lines) => lines,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    // A line torn by a write cut short is passed over, as is one ordered
    // before `since`.
    let mut kept: Vec<Kept> = lines
        .lines()
        .filter_map(|line| {
            let line: Value = serde_json::from_str(line).ok()?;
            let said = line.get("at").and_then(Value::as_str)?;
            let at: jiff::Timestamp = said.parse().ok()?;
            let session = line.get("session").and_then(Value::as_str)?;
            Some(Kept {
                id: format!("{session}:{said}"),
                at,
                line,
            })
        })
        .filter(|kept| kept.at >= since)
        .collect();
    kept.sort_by_key(|one| std::cmp::Reverse(one.at));
    Ok(kept)
}

/// How `kept` stands, by what was `seen` of its session. A refusal says
/// it whatever else is seen, since the session stayed to write it.
#[must_use]
pub fn judged(kept: &Kept, seen: &Seen) -> State {
    if let Some(why) = &seen.refused {
        return State::Refused(why.clone());
    }
    match seen.runs_on.as_deref() {
        Some(machine) if machine == kept.said("to") => State::Arrived,
        Some(machine) if machine == kept.said("from") => {
            if seen.left {
                State::Back
            } else {
                State::Ordered
            }
        }
        _ => State::Carrying,
    }
}

/// The fields of a kept line the person reads. Its chain and asker stay
/// in the file, which proves them.
const READ: [&str; 8] = [
    "at", "by", "session", "address", "quest", "from", "to", "why",
];

/// What a carry's record says: what was kept of it that the person
/// reads, and how it stands, with why it was refused when it was.
#[must_use]
pub fn fields(kept: &Kept, state: &State) -> Value {
    let mut fields: Map<String, Value> = READ
        .iter()
        .filter_map(|name| Some(((*name).to_owned(), kept.line.get(*name)?.clone())))
        .collect();
    let stands = match state {
        State::Ordered => "ordered",
        State::Refused(why) => {
            fields.insert("refused".to_owned(), Value::from(why.as_str()));
            "refused"
        }
        State::Carrying => "carrying",
        State::Arrived => "arrived",
        State::Back => "back",
    };
    fields.insert("state".to_owned(), Value::from(stands));
    Value::Object(fields)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{Value, json};

    use super::{Kept, Seen, State, fields, judged, read};

    /// HARBOR's carry from skiff to tern, ordered at `at`.
    fn ordered_at(at: &str) -> Value {
        json!({
            "at": at, "by": "WCJQSG", "asker": "s-steward", "session": "s-harbour",
            "address": "HARBOR", "quest": "QEST-20261115-HARBOR",
            "chain": [{"quest": "QEST-20261115-HARBOR", "orchestrator": "WCJQSG"}],
            "source": "orchestrations", "from": "skiff", "to": "tern",
            "why": "it needs the camera",
        })
    }

    fn harbour() -> Kept {
        let at = "2026-11-15T20:00:00Z";
        Kept {
            id: format!("s-harbour:{at}"),
            at: at.parse().expect("a time"),
            line: ordered_at(at),
        }
    }

    fn on(machine: &str, left: bool) -> Seen {
        Seen {
            refused: None,
            left,
            runs_on: Some(machine.to_owned()),
        }
    }

    #[test]
    fn a_carry_stands_as_its_session_is_seen() {
        let kept = harbour();
        for (seen, state, said) in [
            (on("skiff", false), State::Ordered, "still here, never gone"),
            (
                Seen {
                    refused: Some("it holds a lease".to_owned()),
                    ..on("skiff", false)
                },
                State::Refused("it holds a lease".to_owned()),
                "refused, though it runs here still",
            ),
            (
                Seen {
                    runs_on: None,
                    ..on("skiff", true)
                },
                State::Carrying,
                "gone from here with its done file",
            ),
            (
                Seen::default(),
                State::Carrying,
                "ended here, though no done file says so yet",
            ),
            (on("tern", true), State::Arrived, "running where it went"),
            (on("tern", false), State::Arrived, "there, done file or not"),
            (on("skiff", true), State::Back, "here again after leaving"),
            (
                on("kestrel", true),
                State::Carrying,
                "somewhere it wasn't carried, so not arrived",
            ),
        ] {
            assert_eq!(judged(&kept, &seen), state, "{said}: {seen:?}");
        }
    }

    #[test]
    fn a_record_says_what_the_person_reads_and_how_it_stands() {
        let kept = harbour();
        assert_eq!(
            fields(&kept, &State::Arrived),
            json!({
                "at": "2026-11-15T20:00:00Z", "by": "WCJQSG", "session": "s-harbour",
                "address": "HARBOR", "quest": "QEST-20261115-HARBOR", "from": "skiff",
                "to": "tern", "why": "it needs the camera", "state": "arrived",
            }),
            "the chain and asker stay in the file, which proves them"
        );
        assert_eq!(
            fields(&kept, &State::Refused("it holds a lease".to_owned()))["refused"],
            json!("it holds a lease"),
            "a refusal says why"
        );
        for (state, name) in [
            (State::Ordered, "ordered"),
            (State::Refused(String::new()), "refused"),
            (State::Carrying, "carrying"),
            (State::Back, "back"),
        ] {
            assert_eq!(fields(&kept, &state)["state"], json!(name), "{state:?}");
        }
    }

    #[test]
    fn the_last_days_carries_are_read_newest_first() {
        let dir = tempfile::tempdir().expect("a folder");
        let file = dir.path().join("carries.jsonl");
        let since: jiff::Timestamp = "2026-11-14T20:00:00Z".parse().expect("a time");
        assert_eq!(read(&file, since).expect("read"), [], "none kept yet");
        let lines = [
            ordered_at("2026-11-14T19:59:59Z").to_string(),
            ordered_at("2026-11-15T09:00:00Z").to_string(),
            "{\"at\": \"2026-11-15T10:0".to_owned(),
            ordered_at("2026-11-15T20:00:00Z").to_string(),
        ];
        fs::write(&file, lines.join("\n") + "\n").expect("written");
        let read = read(&file, since).expect("read");
        assert_eq!(
            read.iter().map(|kept| kept.id.as_str()).collect::<Vec<_>>(),
            [
                "s-harbour:2026-11-15T20:00:00Z",
                "s-harbour:2026-11-15T09:00:00Z"
            ],
            "newest first, the day before gone, a torn line passed over"
        );
        assert_eq!(read[0], harbour(), "each whole");
    }
}
