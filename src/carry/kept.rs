//! The carries this node ordered, as the person sees them: each line
//! `carries.jsonl` keeps (`carry::ordered`), with how it stands now,
//! judged from what the node itself sees rather than from anything the
//! session or its orchestrator says.
//!
//! A carry stands refused once its session writes a refusal since it was
//! ordered; carrying once the session leaves here, by a done file written
//! since or by ending; arrived once it runs on the machine it was carried
//! to, or ended there, or its quest goes on there in another session;
//! and back once it runs here again after leaving, or its quest goes on
//! here alone in another session. Until any of those, it stands ordered,
//! and one that stands ordered or carrying [`STUCK_AFTER`] after it was
//! ordered stands stuck, saying why. The node keeps no state of its own
//! for it, so a node started afresh judges each as it did before.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::Path;

use serde_json::{Map, Value};

/// The kind of record a carry is in the daemon interface.
pub const KIND: &str = "carry";

/// How long a carry is shown after it was ordered.
pub const SHOWN_FOR: jiff::SignedDuration = jiff::SignedDuration::from_hours(24);

/// How long a carry may go without arriving before it stands stuck: a
/// carry takes a few minutes, its trees' bundles most of them.
pub const STUCK_AFTER: jiff::SignedDuration = jiff::SignedDuration::from_mins(30);

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
    /// Neither arrived nor back [`STUCK_AFTER`] after it was ordered,
    /// saying how it stands instead.
    Stuck(String),
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
    /// The machine it last ran on, when it is seen ended.
    pub ended_on: Option<String>,
    /// The machines a session of its quest runs on now.
    pub quest_on: BTreeSet<String>,
}

/// Where the node sees sessions, by its own state, for judging carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sightings {
    /// Where each session seen running, or parked, runs, by its id.
    pub runs_on: HashMap<String, String>,
    /// Where each session seen ended last ran, by its id.
    pub ended_on: HashMap<String, String>,
    /// The machines each quest has a session running, or parked, on, by
    /// the quest's id.
    pub quest_on: HashMap<String, BTreeSet<String>>,
}

impl Sightings {
    /// `seen` of `kept`'s session with where the node sees it, and its
    /// quest, added.
    #[must_use]
    pub fn seen(&self, kept: &Kept, seen: Seen) -> Seen {
        let session = kept.said("session");
        Seen {
            runs_on: self.runs_on.get(session).cloned(),
            ended_on: self.ended_on.get(session).cloned(),
            quest_on: self
                .quest_on
                .get(kept.said("quest"))
                .cloned()
                .unwrap_or_default(),
            ..seen
        }
    }
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

/// The lines of `bytes` that are whole text, in order: one torn part way
/// into a letter, or holding a byte no letter has, is passed over, so it
/// is never read as some other line.
pub fn lines_of(bytes: &[u8]) -> impl Iterator<Item = &str> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| std::str::from_utf8(line).ok())
}

/// Whether `file`, open to read, ends part way into a line, as a write
/// cut short leaves it; the next line kept then starts a line of its own.
///
/// # Errors
///
/// When the file can't be read.
pub fn cut_short(file: &mut fs::File) -> io::Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    if file.metadata()?.len() == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0];
    file.read_exact(&mut last)?;
    Ok(last != *b"\n")
}

/// The carries `file` keeps that were ordered since `since`, newest
/// first.
///
/// # Errors
///
/// When the file is there but can't be read.
pub fn read(file: &Path, since: jiff::Timestamp) -> io::Result<Vec<Kept>> {
    let bytes = match fs::read(file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    // A line torn by a write cut short is passed over, though it end part
    // way into a letter, as is one ordered before `since`.
    let mut kept: Vec<Kept> = lines_of(&bytes)
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

/// How `kept` stands at `now`, by what was `seen` of its session. A
/// refusal says it whatever else is seen, since the session stayed to
/// write it. Where its session runs says it next; where it runs nowhere,
/// its quest does, since a carry may bring its quest up in a session of
/// another id, or the quest go on here in a new one.
#[must_use]
pub fn judged(kept: &Kept, seen: &Seen, now: jiff::Timestamp) -> State {
    if let Some(why) = &seen.refused {
        return State::Refused(why.clone());
    }
    let (from, to) = (kept.said("from"), kept.said("to"));
    let state = match seen.runs_on.as_deref() {
        Some(machine) if machine == to => return State::Arrived,
        Some(machine) if machine == from && seen.left => return State::Back,
        Some(machine) if machine == from => State::Ordered,
        Some(_) => State::Carrying,
        None if seen.ended_on.as_deref() == Some(to) || seen.quest_on.contains(to) => {
            return State::Arrived;
        }
        None if seen.quest_on.contains(from) => return State::Back,
        None => State::Carrying,
    };
    let waited = now.duration_since(kept.at) >= STUCK_AFTER;
    if !waited {
        return state;
    }
    let minutes = STUCK_AFTER.as_mins();
    State::Stuck(match seen.runs_on.as_deref() {
        Some(machine) if machine == from => {
            format!("still running on {from} {minutes} minutes after it was ordered")
        }
        Some(machine) => format!("running on {machine}, not on {to}, where it was carried"),
        None => format!(
            "neither it nor its quest seen running on {to} {minutes} minutes after it was \
             ordered"
        ),
    })
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
        State::Stuck(why) => {
            fields.insert("stuck".to_owned(), Value::from(why.as_str()));
            "stuck"
        }
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
        ..Seen::default()
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
        Kept, MOST_TOLD, MOST_TOLD_BYTES, STUCK_AFTER, Seen, Sightings, State, bounded, fields,
        fits, heard, judged, read, shown, told,
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
            left,
            runs_on: Some(machine.to_owned()),
            ..Seen::default()
        }
    }

    /// A moment `minutes` after [`harbour`] was ordered.
    fn after(minutes: i64) -> jiff::Timestamp {
        harbour().at + jiff::SignedDuration::from_mins(minutes)
    }

    /// What is seen of a session running nowhere, whose quest runs on
    /// `machines`.
    fn quest_on(machines: &[&str]) -> Seen {
        Seen {
            quest_on: machines
                .iter()
                .map(|machine| (*machine).to_owned())
                .collect(),
            ..Seen::default()
        }
    }

    #[test]
    fn a_carry_stands_as_its_session_is_seen() {
        let kept = harbour();
        let now = after(5);
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
            assert_eq!(judged(&kept, &seen, now), state, "{said}: {seen:?}");
        }
    }

    #[test]
    fn a_carry_whose_session_runs_nowhere_stands_as_its_quest_is_seen() {
        let kept = harbour();
        let now = after(5);
        for (seen, state, said) in [
            (
                quest_on(&["tern"]),
                State::Arrived,
                "its quest came up there in a session of another id",
            ),
            (
                quest_on(&["skiff", "tern"]),
                State::Arrived,
                "there, though it goes on here too",
            ),
            (
                quest_on(&["skiff"]),
                State::Back,
                "its quest goes on here alone, in a new session",
            ),
            (
                Seen {
                    ended_on: Some("tern".to_owned()),
                    ..Seen::default()
                },
                State::Arrived,
                "it ended where it went",
            ),
            (
                Seen {
                    ended_on: Some("skiff".to_owned()),
                    ..Seen::default()
                },
                State::Carrying,
                "it ended here, and nothing of it is seen yet",
            ),
            (
                quest_on(&["kestrel"]),
                State::Carrying,
                "its quest runs only somewhere it wasn't carried",
            ),
            (
                Seen {
                    quest_on: ["tern".to_owned()].into(),
                    ..on("skiff", false)
                },
                State::Ordered,
                "its own session, still here, says it first",
            ),
        ] {
            assert_eq!(judged(&kept, &seen, now), state, "{said}: {seen:?}");
        }
    }

    #[test]
    fn a_carry_neither_arrived_nor_back_in_half_an_hour_stands_stuck_saying_why() {
        let kept = harbour();
        let minutes = STUCK_AFTER.as_mins();
        assert_eq!(
            judged(&kept, &Seen::default(), after(minutes - 1)),
            State::Carrying,
            "within its half hour"
        );
        for (seen, why) in [
            (
                Seen::default(),
                "neither it nor its quest seen running on tern 30 minutes after it was ordered",
            ),
            (
                on("skiff", false),
                "still running on skiff 30 minutes after it was ordered",
            ),
            (
                on("kestrel", true),
                "running on kestrel, not on tern, where it was carried",
            ),
        ] {
            assert_eq!(
                judged(&kept, &seen, after(minutes)),
                State::Stuck(why.to_owned()),
                "{seen:?}"
            );
        }
        for (seen, state) in [
            (on("tern", false), State::Arrived),
            (on("skiff", true), State::Back),
            (quest_on(&["tern"]), State::Arrived),
            (
                Seen {
                    refused: Some("it holds a lease".to_owned()),
                    ..Seen::default()
                },
                State::Refused("it holds a lease".to_owned()),
            ),
        ] {
            assert_eq!(
                judged(&kept, &seen, after(24 * 60)),
                state,
                "what settles a carry holds however long it took: {seen:?}"
            );
        }
        assert_eq!(
            fields(&kept, &State::Stuck("why".to_owned())),
            json!({
                "at": "2026-11-15T20:00:00Z", "by": "WCJQSG", "session": "s-harbour",
                "address": "HARBOR", "quest": "QEST-20261115-HARBOR", "from": "skiff",
                "to": "tern", "why": "it needs the camera", "state": "stuck", "stuck": "why",
            }),
            "its record says why it is stuck"
        );
    }

    #[test]
    fn what_the_node_sees_is_added_to_what_its_handover_says() {
        let sightings = Sightings {
            runs_on: [("s-harbour".to_owned(), "tern".to_owned())].into(),
            ended_on: [("s-harbour".to_owned(), "skiff".to_owned())].into(),
            quest_on: [(
                "QEST-20261115-HARBOR".to_owned(),
                ["tern".to_owned()].into(),
            )]
            .into(),
        };
        let handed = Seen {
            left: true,
            ..Seen::default()
        };
        assert_eq!(
            sightings.seen(&harbour(), handed),
            Seen {
                left: true,
                runs_on: Some("tern".to_owned()),
                ended_on: Some("skiff".to_owned()),
                quest_on: ["tern".to_owned()].into(),
                ..Seen::default()
            }
        );
        assert_eq!(
            Sightings::default().seen(&harbour(), Seen::default()),
            Seen::default(),
            "nothing seen"
        );
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
            (State::Stuck(String::new()), "stuck"),
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
    fn a_line_torn_part_way_into_a_letter_is_passed_over_too() {
        let dir = tempfile::tempdir().expect("a folder");
        let file = dir.path().join("carries.jsonl");
        let since: jiff::Timestamp = "2026-11-14T20:00:00Z".parse().expect("a time");
        let mut text = ordered_at("2026-11-15T20:00:00Z").to_string().into_bytes();
        text.extend_from_slice(b"\n{\"session\": \"s-\xc3");
        fs::write(&file, text).expect("written");
        assert_eq!(
            read(&file, since)
                .map(|read| read.len())
                .map_err(|error| error.to_string()),
            Ok(1),
            "a write cut short in a letter leaves the file no longer UTF-8, and the whole \
             lines still read"
        );
    }

    #[test]
    fn a_whole_line_holding_a_byte_no_letter_has_counts_for_none() {
        let dir = tempfile::tempdir().expect("a folder");
        let file = dir.path().join("carries.jsonl");
        let since: jiff::Timestamp = "2026-11-14T20:00:00Z".parse().expect("a time");
        let mut text = ordered_at("2026-11-15T20:00:00Z").to_string().into_bytes();
        text.extend_from_slice(
            b"\n{\"session\": \"s-caf\xe9\", \"at\": \"2026-11-15T21:00:00Z\"}\n",
        );
        fs::write(&file, text).expect("written");
        assert_eq!(
            read(&file, since)
                .map(|read| read.iter().map(|one| one.id.clone()).collect::<Vec<_>>())
                .map_err(|error| error.to_string()),
            Ok(vec!["s-harbour:2026-11-15T20:00:00Z".to_owned()]),
            "a line with a byte no letter has is never read as some other session's"
        );
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
                    ..Seen::default()
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
