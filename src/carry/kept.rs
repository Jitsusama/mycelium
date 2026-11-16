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

/// Whether `kept` is shown at `since`: ordered since, or settled since,
/// so what its settling came to is seen for a day after it, as it is
/// settled only a day after it was ordered (`carry::settle`).
#[must_use]
pub fn shown(kept: &Kept, since: jiff::Timestamp) -> bool {
    let settled = kept
        .line
        .get("settle")
        .and_then(|settle| settle.get("at"))
        .and_then(Value::as_str)
        .and_then(|at| at.parse::<jiff::Timestamp>().ok());
    kept.at >= since || settled.is_some_and(|at| at >= since)
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

/// The fields of a kept line the person reads, with what settling its
/// carry came to once it settled. Its chain and asker stay in the file,
/// which proves them.
const READ: [&str; 9] = [
    "at", "by", "session", "address", "quest", "from", "to", "why", "settle",
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

/// The most carries a node tells another machine's of at once.
pub const MOST_TOLD: usize = 200;

/// The most bytes the carries a node tells another machine's of may come
/// to, as JSON.
pub const MOST_TOLD_BYTES: usize = 256 << 10;

/// `kept` as told to the node of another machine, so the person sees it
/// from there: what the person reads of it, with whether its session
/// refused, saying why, and whether it left, as only this machine's
/// handover shows. How it stands the other machine judges for itself,
/// from the sessions it sees.
#[must_use]
pub fn told(kept: &Kept, refused: Option<&str>, left: bool) -> Value {
    let mut told: Map<String, Value> = READ
        .iter()
        .filter_map(|name| Some(((*name).to_owned(), kept.line.get(*name)?.clone())))
        .collect();
    if let Some(why) = refused {
        told.insert("refused".to_owned(), Value::from(why));
    }
    told.insert("left".to_owned(), Value::from(left));
    Value::Object(told)
}

/// The newest of `told`, given newest first, that fit what one machine
/// tells another: at most [`MOST_TOLD`], coming to at most
/// [`MOST_TOLD_BYTES`].
#[must_use]
pub fn bounded(mut told: Vec<Value>) -> Vec<Value> {
    told.truncate(MOST_TOLD);
    while fits(&told).is_err() {
        told.pop();
    }
    told
}

/// Whether `told` is within what one machine tells another of its
/// carries.
///
/// # Errors
///
/// Why not: too many, or too many bytes.
pub fn fits(told: &[Value]) -> Result<(), String> {
    if told.len() > MOST_TOLD {
        return Err(format!(
            "it tells of {} carries, more than the {MOST_TOLD} a machine may at once",
            told.len()
        ));
    }
    let bytes = serde_json::to_vec(told).map_or(usize::MAX, |json| json.len());
    if bytes > MOST_TOLD_BYTES {
        return Err(format!(
            "its carries come to {bytes} bytes, more than the {MOST_TOLD_BYTES} a machine may \
             tell at once"
        ));
    }
    Ok(())
}

/// A carry another machine's node told of, as [`told`] says it, with what
/// that machine saw of its session: none where it names no session or
/// time.
#[must_use]
pub fn heard(told: &Value) -> Option<(Kept, Seen)> {
    let said = told.get("at").and_then(Value::as_str)?;
    let at: jiff::Timestamp = said.parse().ok()?;
    let session = told.get("session").and_then(Value::as_str)?;
    let seen = Seen {
        refused: told
            .get("refused")
            .and_then(Value::as_str)
            .map(str::to_owned),
        left: told.get("left").and_then(Value::as_bool).unwrap_or(false),
        runs_on: None,
    };
    let kept = Kept {
        id: format!("{session}:{said}"),
        at,
        line: told.clone(),
    };
    Some((kept, seen))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{Value, json};

    use super::{
        Kept, MOST_TOLD, MOST_TOLD_BYTES, Seen, State, bounded, fields, fits, heard, judged, read,
        shown, told,
    };

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
    fn a_settled_carry_says_so_and_is_shown_a_day_after_it_settled() {
        let mut kept = harbour();
        let since: jiff::Timestamp = "2026-11-16T21:00:00Z".parse().expect("a time");
        assert!(!shown(&kept, since), "ordered a day before, unsettled");
        let settle = json!({"did": "settled", "let": 2, "at": "2026-11-16T21:00:01Z"});
        kept.line["settle"] = settle.clone();
        assert!(shown(&kept, since), "settled since");
        assert_eq!(
            fields(&kept, &State::Arrived)["settle"],
            settle,
            "its record says what settling came to"
        );
        let (heard, _) = heard(&told(&kept, None, true)).expect("heard");
        assert_eq!(
            heard.line["settle"], settle,
            "and so does the record another machine shows of it"
        );
        assert!(shown(&heard, since), "which shows it as long");
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

    #[test]
    fn a_carry_told_to_another_machine_holds_only_what_the_person_reads() {
        let kept = harbour();
        let said = told(&kept, Some("it holds a lease"), true);
        assert_eq!(
            said,
            json!({
                "at": "2026-11-15T20:00:00Z", "by": "WCJQSG", "session": "s-harbour",
                "address": "HARBOR", "quest": "QEST-20261115-HARBOR", "from": "skiff",
                "to": "tern", "why": "it needs the camera",
                "refused": "it holds a lease", "left": true,
            }),
            "its chain and asker stay with the node that proved them"
        );
        let (back, seen) = heard(&said).expect("heard");
        assert_eq!(
            (back.id.as_str(), back.at, &seen),
            (
                "s-harbour:2026-11-15T20:00:00Z",
                kept.at,
                &Seen {
                    refused: Some("it holds a lease".to_owned()),
                    left: true,
                    runs_on: None,
                }
            ),
            "heard as told, where it runs left for the hearer to see"
        );
        assert_eq!(
            heard(&told(&kept, None, false)).map(|(_, seen)| seen),
            Some(Seen::default()),
            "neither refused nor left"
        );
        assert!(
            heard(&json!({"session": "s-harbour", "at": "yesterday"})).is_none(),
            "one with no time it can read is passed over"
        );
    }

    #[test]
    fn a_machine_tells_at_most_its_newest_carries_that_fit() {
        let many: Vec<Value> = (0..=MOST_TOLD)
            .map(|at| json!({"session": format!("s-{at}")}))
            .collect();
        assert!(fits(&many).is_err(), "one more than the most is refused");
        let kept = bounded(many);
        assert_eq!(
            (kept.len(), &kept[0]["session"]),
            (MOST_TOLD, &json!("s-0")),
            "the newest are kept"
        );
        assert!(fits(&kept).is_ok(), "and they fit");

        let long = "x".repeat(MOST_TOLD_BYTES / 4);
        let heavy: Vec<Value> = (0..5).map(|at| json!({"at": at, "why": long})).collect();
        assert!(fits(&heavy).is_err(), "too many bytes is refused");
        let kept = bounded(heavy);
        assert_eq!(
            kept.iter().map(|one| one["at"].clone()).collect::<Vec<_>>(),
            [json!(0), json!(1), json!(2)],
            "the newest that fit, the oldest let go"
        );
    }
}
