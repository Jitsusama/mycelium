//! What waits on the mesh: each question still waiting on a peer it was
//! put to, and each hold still standing, from the last two hours, as the
//! harness's `mesh_read unanswered` and its hold leases judge them.
//!
//! A question counts as put to each peer it went to by address, and as
//! answered for one once a message from that peer, under any address it
//! goes by, answers it or anything in its thread. One its asker withdrew
//! (`supersedes`) or moved on from, by asking again in answer to it, is
//! closed. A hold stands until its sender answers or supersedes it, or a
//! message answering it says `endsHold`, and for two hours at most.
//!
//! The index is kept as each message is heard, so reading it never goes
//! back over every message the node holds: only what it heard within the
//! two hours, and only the questions and holds among them are read whole.
//! Which addresses are one peer's is asked as it is read, since a
//! session's addresses are its presence's, which change apart from the
//! messages.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::{Value, json};

use crate::forage::one_line;
use crate::mesh;
use crate::record::{Intent, Message};
use crate::swarm::Look;

/// The kind the daemon interface holds these as.
pub const KIND: &str = "unanswered";

/// How far back a question or a hold is still open, in milliseconds.
pub const WITHIN_MS: i64 = 2 * 60 * 60 * 1000;

/// How far up a thread an answer is followed, a guard against loops.
const MOST_THREAD_DEPTH: usize = 20;

/// What the index keeps of each message heard within the two hours.
#[derive(Debug)]
struct Heard {
    /// What it answers.
    re: Option<String>,
    /// When it was sent, in milliseconds since the epoch.
    at: i64,
}

/// What others' messages said to one message, which may arrive before
/// it does, as one caught up over a link can.
#[derive(Debug, Default)]
struct Said {
    /// Who answered it or anything in its thread.
    answered_by: HashSet<String>,
    /// Who answered it directly, or superseded it.
    ended_by: HashSet<String>,
    /// Who asked again in answer to it.
    asked_again_by: HashSet<String>,
    /// Whether any message superseded it.
    superseded: bool,
    /// Whether an answer to it said its hold is over.
    ends_hold: bool,
}

/// The open questions and standing holds among the messages heard.
#[derive(Debug, Default)]
pub struct Unanswered {
    heard: HashMap<String, Heard>,
    /// Ids by when they were sent, so the oldest are let go first.
    by_time: BTreeSet<(i64, String)>,
    said: HashMap<String, Said>,
    /// The questions and holds themselves, whole.
    asked: HashMap<String, Message>,
}

/// One question waiting, or one hold standing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Open {
    /// The question or the hold.
    pub message: Message,
    /// For a question, the addresses it went to that haven't answered.
    pub waiting_on: Vec<String>,
}

impl Unanswered {
    /// Hears `message`, as `now` stands, letting go of what is older than
    /// two hours. A message already heard, or older itself, changes
    /// nothing.
    pub fn add(&mut self, message: &Message, now: i64) {
        self.forget_before(now - WITHIN_MS);
        let Some(at) = millis(&message.at) else {
            return;
        };
        if at < now - WITHIN_MS || self.heard.contains_key(&message.id) {
            return;
        }
        self.heard.insert(
            message.id.clone(),
            Heard {
                re: message.re.clone(),
                at,
            },
        );
        self.by_time.insert((at, message.id.clone()));
        if matches!(message.intent, Some(Intent::Question | Intent::Hold)) {
            self.asked.insert(message.id.clone(), message.clone());
        }
        if let Some(gone) = &message.supersedes {
            let said = self.said.entry(gone.clone()).or_default();
            said.superseded = true;
            said.ended_by.insert(message.from.clone());
        }
        let Some(re) = &message.re else {
            // Answers to it that came first answer nothing further up.
            return;
        };
        {
            let said = self.said.entry(re.clone()).or_default();
            said.ended_by.insert(message.from.clone());
            said.ends_hold |= message.ends_hold;
            if message.intent == Some(Intent::Question) {
                said.asked_again_by.insert(message.from.clone());
            }
        }
        // Whoever answered this one, before it was heard, answered its
        // thread too.
        let mut answering: HashSet<String> = self
            .said
            .get(&message.id)
            .map(|said| said.answered_by.clone())
            .unwrap_or_default();
        answering.insert(message.from.clone());
        let mut up = Some(re.clone());
        for _ in 0..MOST_THREAD_DEPTH {
            let Some(id) = up else {
                break;
            };
            self.said
                .entry(id.clone())
                .or_default()
                .answered_by
                .extend(answering.iter().cloned());
            up = self.heard.get(&id).and_then(|heard| heard.re.clone());
        }
    }

    /// Lets go of everything sent before `since`.
    fn forget_before(&mut self, since: i64) {
        while let Some((at, id)) = self.by_time.first().cloned() {
            if at >= since {
                break;
            }
            self.by_time.pop_first();
            self.heard.remove(&id);
            self.said.remove(&id);
            self.asked.remove(&id);
        }
    }

    /// The questions waiting and holds standing as `now` stands, oldest
    /// first. `aliases` gives every address the peer going by one goes
    /// by, that one among them.
    pub fn open(&self, aliases: impl Fn(&str) -> Vec<String>, now: i64) -> Vec<Open> {
        let same_as = |one: &str, other: &str| {
            one.eq_ignore_ascii_case(other)
                || aliases(one)
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(other))
        };
        let nothing = Said::default();
        let mut open: Vec<(i64, Open)> = self
            .asked
            .values()
            .filter_map(|message| {
                let at = self.heard.get(&message.id)?.at;
                if at < now - WITHIN_MS {
                    return None;
                }
                let said = self.said.get(&message.id).unwrap_or(&nothing);
                let waiting_on = if message.intent == Some(Intent::Question) {
                    let withdrawn = said.superseded
                        || said
                            .asked_again_by
                            .iter()
                            .any(|by| same_as(by, &message.from));
                    if withdrawn {
                        return None;
                    }
                    let waiting: Vec<String> = addressed(message)
                        .filter(|address| {
                            !same_as(address, &message.from)
                                && !said.answered_by.iter().any(|by| same_as(by, address))
                        })
                        .collect();
                    if waiting.is_empty() {
                        return None;
                    }
                    waiting
                } else {
                    let ended =
                        said.ends_hold || said.ended_by.iter().any(|by| same_as(by, &message.from));
                    if ended {
                        return None;
                    }
                    Vec::new()
                };
                Some((
                    at,
                    Open {
                        message: message.clone(),
                        waiting_on,
                    },
                ))
            })
            .collect();
        open.sort_by(|(a, one), (b, other)| {
            a.cmp(b).then_with(|| one.message.id.cmp(&other.message.id))
        });
        open.into_iter().map(|(_, open)| open).collect()
    }
}

/// The addresses a message went to by a peer's own group, as the mesh
/// writes them.
fn addressed(message: &Message) -> impl Iterator<Item = String> + '_ {
    message
        .groups
        .iter()
        .filter_map(|group| mesh::target_for(group).strip_prefix('@').map(str::to_owned))
}

/// An RFC 3339 time in milliseconds since the epoch, if it reads as one.
fn millis(time: &str) -> Option<i64> {
    time.parse::<jiff::Timestamp>()
        .ok()
        .map(jiff::Timestamp::as_millisecond)
}

impl Open {
    /// Its id, and the fields its record holds: who sent it, what it is,
    /// where it went as the mesh writes them, what it said, since when,
    /// and for a question whom it waits on, for a hold until when it
    /// stands at most.
    #[must_use]
    pub fn fields(&self) -> (String, Value) {
        let message = &self.message;
        let mut fields = json!({
            "id": message.id,
            "from": message.from,
            "intent": message.intent,
            "groups": message.groups.iter().map(mesh::target_for).collect::<Vec<_>>(),
            "text": message.text,
            "since": message.at,
        });
        if message.intent == Some(Intent::Hold) {
            let until = millis(&message.at)
                .and_then(|at| jiff::Timestamp::from_millisecond(at + WITHIN_MS).ok())
                .map(|until| until.to_string());
            fields["until"] = json!(until);
        } else {
            fields["waiting_on"] = json!(self.waiting_on);
        }
        if let Some(re) = &message.re {
            fields["re"] = json!(re);
        }
        (message.id.clone(), fields)
    }
}

/// How far what a question or a hold says sits in under its heading.
const SAYS: &str = "    ";

/// How many lines of what one says are drawn in a terminal, before the
/// rest is left to its whole text in `-o json`.
const SAYS_LINES: usize = 2;

/// What waits on the mesh drawn for a person, from the node's `records`
/// as [`Open::fields`] writes them: the questions, then the holds, each
/// oldest first. A question says who asks whom, since when and how long
/// it has waited; a hold, who holds whom and until when; each its id as
/// the mesh's tools take it, then the opening of what it says, an
/// ellipsis after it where more lines follow. In `width` columns, a
/// terminal's; with none, into a pipe, nothing is cut and each says its
/// first line whole.
#[must_use]
pub fn drawn(records: &[Value], look: &Look, width: Option<usize>) -> String {
    let mut records: Vec<&Value> = records.iter().collect();
    records.sort_by_key(|record| millis(record["since"].as_str().unwrap_or_default()));
    let (questions, holds): (Vec<&Value>, Vec<&Value>) = records
        .into_iter()
        .partition(|record| record["intent"] != "hold");
    if questions.is_empty() && holds.is_empty() {
        return "Nothing waits: no question is open and no hold stands.\n".to_owned();
    }
    let mut blocks = Vec::new();
    if !questions.is_empty() {
        blocks.push(block(
            &counted(
                questions.len(),
                "question waits on an answer",
                "questions wait on an answer",
            ),
            &questions,
            look,
            width,
        ));
    }
    if !holds.is_empty() {
        blocks.push(block(
            &counted(holds.len(), "hold stands", "holds stand"),
            &holds,
            look,
            width,
        ));
    }
    blocks.join("\n")
}

/// A heading and each of `records` under it.
fn block(heading: &str, records: &[&Value], look: &Look, width: Option<usize>) -> String {
    let mut out = format!("{}\n", cut(heading, width));
    for record in records {
        out.push_str(&headed(record, look, width));
        out.push('\n');
        let opening = says(record);
        if opening.is_empty() {
            continue;
        }
        match width {
            None => {
                out.push_str(SAYS);
                out.push_str(&opening);
                out.push('\n');
            }
            Some(width) => out.push_str(&opening_in(&opening, width)),
        }
    }
    out
}

/// The fewest columns the names are cut to before the rest of the line
/// before the wait gives way instead.
const NAMES_LEAST: usize = 4;

/// The fewest columns kept before the wait and the id; narrower than
/// that, the whole line is cut from its end.
const HEAD_LEAST: usize = 8;

/// The most of a sender's name, or of an id, drawn, so one written long,
/// or written to look like a line's end, can't push the real wait and
/// id off the line.
const NAME_MOST: usize = 16;

/// A record's line: when, who, of whom, how long or until when, its id.
/// In `width` columns the names give way first, then the sender and the
/// time, so the wait and the id a person answers by stay whole; only
/// where even they don't fit is the line cut from its end.
fn headed(record: &Value, look: &Look, width: Option<usize>) -> String {
    use crate::swarm::fit;
    let since = record["since"].as_str().unwrap_or_default();
    let when = look.when(since).unwrap_or_default();
    let from = fit(
        &one_line(record["from"].as_str().unwrap_or_default()),
        NAME_MOST,
    );
    let short = fit(
        &one_line(short_id(record["id"].as_str().unwrap_or_default())),
        NAME_MOST,
    );
    let (verb, named, after) = if record["intent"] == "hold" {
        let until = record["until"]
            .as_str()
            .and_then(|until| lapses(look, until))
            .map_or_else(String::new, |until| format!(", until {until}"));
        ("holds", names(&record["groups"]), until)
    } else {
        let waited = millis(since).map_or_else(String::new, |at| {
            format!(
                ", {} so far",
                waited(look.now.timestamp().as_millisecond() - at)
            )
        });
        ("asks", names(&record["waiting_on"]), waited)
    };
    let lead = format!("  {when}  {from} {verb} ");
    let tail = format!("{after}  [{short}]");
    let Some(width) = width else {
        return format!("{lead}{named}{tail}");
    };
    let tail_cells = tail.chars().count();
    let room = width.saturating_sub(lead.chars().count() + tail_cells);
    if room >= NAMES_LEAST {
        format!("{lead}{}{tail}", fit(&named, room))
    } else if width >= tail_cells + HEAD_LEAST {
        format!(
            "{}{tail}",
            fit(&format!("{lead}{named}"), width - tail_cells)
        )
    } else {
        cut(&format!("{lead}{named}{tail}"), Some(width))
    }
}

/// When a hold lapses, `until`: its time, and its weekday when that
/// isn't today, as a hold put late in the evening lapses after midnight.
fn lapses(look: &Look, until: &str) -> Option<String> {
    let until = until
        .parse::<jiff::Timestamp>()
        .ok()?
        .to_zoned(look.now.time_zone().clone());
    let format = if until.date() == look.now.date() {
        "%H:%M"
    } else {
        "%a %H:%M"
    };
    Some(until.strftime(format).to_string())
}

/// The names in `list`, a sentence's way, each drawn safe.
fn names(list: &Value) -> String {
    let names: Vec<String> = list
        .as_array()
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(one_line)
                .collect()
        })
        .unwrap_or_default();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    match crate::swarm::listed(&names) {
        none if none.is_empty() => "nobody named".to_owned(),
        some => some,
    }
}

/// A message's id as the mesh's tools show and take it: its random tail.
fn short_id(id: &str) -> &str {
    id.rsplit('-').next().unwrap_or(id)
}

/// The first line of what a record says, drawn safe, with an ellipsis
/// after it where more lines follow.
fn says(record: &Value) -> String {
    let text = record["text"].as_str().unwrap_or_default();
    // Drawn safe before it is trimmed, so a line of controls alone counts
    // as blank and gives way to the next.
    let mut lines = text
        .lines()
        .map(|line| one_line(line).trim().to_owned())
        .filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or_default();
    if lines.next().is_some() {
        format!("{first} …")
    } else {
        first
    }
}

/// `opening` wrapped under its heading to `width` columns, at most
/// [`SAYS_LINES`] lines, the last cut with an ellipsis where more is
/// left.
fn opening_in(opening: &str, width: usize) -> String {
    let wrapped = crate::swarm::wrapped(opening, SAYS, SAYS, width);
    let lines: Vec<&str> = wrapped.lines().collect();
    if lines.len() <= SAYS_LINES {
        return wrapped;
    }
    let (shown, left) = lines.split_at(SAYS_LINES - 1);
    let mut out = String::new();
    for line in shown {
        out.push_str(line);
        out.push('\n');
    }
    // The last line shown takes the lines left, joined again, cut to the
    // room it has, so an ellipsis says there is more.
    let rest: Vec<&str> = left.iter().map(|line| line.trim_start()).collect();
    out.push_str(SAYS);
    out.push_str(&crate::swarm::fit(
        &rest.join(" "),
        width.saturating_sub(SAYS.len()),
    ));
    out.push('\n');
    out
}

/// `line` cut to `width`, where there is one.
fn cut(line: &str, width: Option<usize>) -> String {
    width.map_or_else(|| line.to_owned(), |width| crate::swarm::fit(line, width))
}

/// How many there are, with the words that agree with it.
fn counted(count: usize, one: &str, more: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { more })
}

/// How long `millis` is, as a person says a wait.
fn waited(millis: i64) -> String {
    let minutes = millis.max(0) / 60_000;
    match (minutes / 60, minutes % 60) {
        (0, 0) => "under a minute".to_owned(),
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::Group;

    /// Noon, in milliseconds, which the tests' messages are sent around.
    fn noon() -> i64 {
        "2026-11-15T12:00:00Z"
            .parse::<jiff::Timestamp>()
            .expect("a time")
            .as_millisecond()
    }

    /// A message from `from` to `to`, as mesh targets, `minutes` after noon.
    fn said(id: &str, from: &str, to: &[&str], intent: Intent, minutes: i64) -> Message {
        let at = jiff::Timestamp::from_millisecond(noon() + minutes * 60_000).expect("a time");
        Message {
            id: id.to_owned(),
            at: at.to_string(),
            from: from.to_owned(),
            groups: to
                .iter()
                .map(|to| mesh::group_for(to).expect("a group"))
                .collect::<Vec<Group>>(),
            intent: Some(intent),
            text: format!("{id} said"),
            re: None,
            supersedes: None,
            from_realm: None,
            for_realm: None,
            ends_hold: false,
        }
    }

    fn re(mut message: Message, answers: &str) -> Message {
        message.re = Some(answers.to_owned());
        message
    }

    fn heard(messages: &[Message]) -> Unanswered {
        let mut index = Unanswered::default();
        for message in messages {
            index.add(message, noon() + 30 * 60_000);
        }
        index
    }

    /// No peer goes by a second address.
    fn alone(address: &str) -> Vec<String> {
        vec![address.to_owned()]
    }

    fn waiting(
        index: &Unanswered,
        aliases: impl Fn(&str) -> Vec<String>,
    ) -> Vec<(String, Vec<String>)> {
        index
            .open(aliases, noon() + 30 * 60_000)
            .into_iter()
            .map(|open| (open.message.id, open.waiting_on))
            .collect()
    }

    #[test]
    fn a_question_waits_on_each_peer_it_went_to_until_each_answers() {
        let asked = said(
            "q",
            "KESTRL",
            &["@SKIFF1", "@SKIFF2", "#quest/skiff"],
            Intent::Question,
            0,
        );
        let index = heard(&[
            asked.clone(),
            re(said("a", "SKIFF1", &["@KESTRL"], Intent::Fyi, 1), "q"),
        ]);
        assert_eq!(
            waiting(&index, alone),
            vec![("q".to_owned(), vec!["SKIFF2".to_owned()])],
            "one answered, the other still waited on, and a group waits on nobody"
        );
        let index = heard(&[
            asked,
            re(said("a", "SKIFF1", &["@KESTRL"], Intent::Fyi, 1), "q"),
            re(said("b", "SKIFF2", &["@KESTRL"], Intent::Fyi, 2), "q"),
        ]);
        assert_eq!(waiting(&index, alone), Vec::new(), "both answered");
    }

    #[test]
    fn an_answer_anywhere_in_its_thread_answers_it_whatever_order_it_comes_in() {
        let messages = [
            said("q", "KESTRL", &["@SKIFF1"], Intent::Question, 0),
            re(said("f", "KESTRL", &["@SKIFF1"], Intent::Fyi, 1), "q"),
            re(said("a", "SKIFF1", &["@KESTRL"], Intent::Fyi, 2), "f"),
        ];
        assert_eq!(
            waiting(&heard(&messages), alone),
            Vec::new(),
            "answered in its thread"
        );
        let mut late = messages.clone();
        late.reverse();
        assert_eq!(
            waiting(&heard(&late), alone),
            Vec::new(),
            "answered in its thread, though the answer came first"
        );
    }

    #[test]
    fn a_peer_answers_under_any_address_it_goes_by() {
        let index = heard(&[
            said("q", "KESTRL", &["@SKIFF1"], Intent::Question, 0),
            re(said("a", "9F7F6018", &["@KESTRL"], Intent::Fyi, 1), "q"),
        ]);
        assert_eq!(
            waiting(&index, alone),
            vec![("q".to_owned(), vec!["SKIFF1".to_owned()])],
            "an address nobody ties to it doesn't answer for it"
        );
        let both = |address: &str| {
            if ["SKIFF1", "9F7F6018"].contains(&address) {
                vec!["SKIFF1".to_owned(), "9F7F6018".to_owned()]
            } else {
                vec![address.to_owned()]
            }
        };
        assert_eq!(
            waiting(&index, both),
            Vec::new(),
            "its session's own address answers"
        );
    }

    #[test]
    fn a_question_withdrawn_or_asked_again_in_answer_is_closed() {
        let withdrawn = heard(&[
            said("q", "KESTRL", &["@SKIFF1"], Intent::Question, 0),
            Message {
                supersedes: Some("q".to_owned()),
                ..said("w", "KESTRL", &["@SKIFF1"], Intent::Fyi, 1)
            },
        ]);
        assert_eq!(waiting(&withdrawn, alone), Vec::new(), "withdrawn");
        let again = heard(&[
            said("q", "KESTRL", &["@SKIFF1"], Intent::Question, 0),
            re(said("q2", "KESTRL", &["@SKIFF1"], Intent::Question, 1), "q"),
        ]);
        assert_eq!(
            waiting(&again, alone),
            vec![("q2".to_owned(), vec!["SKIFF1".to_owned()])],
            "the asker moved on to its second question"
        );
    }

    #[test]
    fn a_hold_stands_until_its_sender_answers_it_or_an_answer_ends_it() {
        let hold = said("h", "KESTRL", &["#machine"], Intent::Hold, 0);
        let standing = heard(&[
            hold.clone(),
            re(said("p", "SKIFF1", &["@KESTRL"], Intent::Fyi, 1), "h"),
        ]);
        let open = standing.open(alone, noon() + 30 * 60_000);
        assert_eq!(open.len(), 1, "another's answer doesn't end it: {open:?}");
        let (id, fields) = open[0].fields();
        assert_eq!(id, "h");
        assert_eq!(fields["groups"], json!(["#machine"]));
        assert_eq!(fields["since"], json!("2026-11-15T12:00:00Z"));
        assert_eq!(
            fields["until"],
            json!("2026-11-15T14:00:00Z"),
            "two hours at most"
        );
        let answered = heard(&[
            hold.clone(),
            re(said("e", "KESTRL", &["#machine"], Intent::Fyi, 1), "h"),
        ]);
        assert_eq!(waiting(&answered, alone), Vec::new(), "its sender ended it");
        let ended = heard(&[
            hold.clone(),
            Message {
                ends_hold: true,
                ..re(said("e", "SKIFF1", &["#machine"], Intent::Fyi, 1), "h")
            },
        ]);
        assert_eq!(
            waiting(&ended, alone),
            Vec::new(),
            "ended on the person's word"
        );
        let past = heard(&[hold]);
        assert_eq!(
            past.open(alone, noon() + WITHIN_MS + 60_000),
            Vec::new(),
            "two hours on, it stands no longer"
        );
    }

    #[test]
    fn what_is_older_than_two_hours_is_let_go() {
        let mut index = Unanswered::default();
        index.add(
            &said("q", "KESTRL", &["@SKIFF1"], Intent::Question, 0),
            noon(),
        );
        index.add(
            &said("r", "KESTRL", &["@SKIFF1"], Intent::Question, 130),
            noon() + 130 * 60_000,
        );
        assert_eq!(
            index.heard.len(),
            1,
            "the first is forgotten: {:?}",
            index.heard
        );
        index.add(
            &said("old", "KESTRL", &["@SKIFF1"], Intent::Question, 0),
            noon() + 130 * 60_000,
        );
        assert_eq!(index.heard.len(), 1, "one heard too late is never kept");
    }

    /// Half an hour past noon, as the drawing tests read it.
    fn look() -> Look {
        look_at(30)
    }

    /// `minutes` past noon, as the drawing tests read it.
    fn look_at(minutes: i64) -> Look {
        Look {
            width: 80,
            colour: false,
            // UTC as jiff holds it, since a build has no zone database.
            now: jiff::Timestamp::from_millisecond(noon() + minutes * 60_000)
                .expect("a time")
                .to_zoned(jiff::tz::TimeZone::UTC),
            shapes: crate::swarm::Shapes::default(),
        }
    }

    /// A question from TERN01 to `waiting_on`, sent `minutes` past noon,
    /// its id's tail `tail`.
    fn asked_of(waiting_on: &[&str], minutes: i64, tail: &str) -> Value {
        Open {
            message: Message {
                text: "Can you merge?".to_owned(),
                ..said(
                    &format!("1795122095010-TERN01-1-{tail}"),
                    "TERN01",
                    &["@SKIFF1"],
                    Intent::Question,
                    minutes,
                )
            },
            waiting_on: waiting_on.iter().map(|&peer| peer.to_owned()).collect(),
        }
        .fields()
        .1
    }

    /// The lines under the heading line holding `heading`, up to the next
    /// heading line.
    fn said_under<'a>(drawn: &'a str, heading: &str) -> Vec<&'a str> {
        drawn
            .lines()
            .skip_while(|line| !line.contains(heading))
            .skip(1)
            .take_while(|line| line.starts_with(SAYS))
            .collect()
    }

    /// What the question says: a first line too long for a narrow
    /// terminal, and a second.
    const ASKED: &str = "TERN01, keeper of the wish list: these asks of his are marked \
        landed under you, but none carries a try-line he could follow in a minute.\n\
        The second line.";

    /// A hold and a question as the node holds them, the hold first.
    fn records(asked: &str) -> Vec<Value> {
        let question = Open {
            message: Message {
                text: asked.to_owned(),
                ..said(
                    "1795122095010-TERN01-2347120-c0cd280e",
                    "TERN01",
                    &["@SKIFF1", "@KESTRL"],
                    Intent::Question,
                    5,
                )
            },
            waiting_on: vec!["SKIFF1".to_owned(), "KESTRL".to_owned()],
        };
        let hold = Open {
            message: Message {
                text: "merge nothing until I end this hold".to_owned(),
                ..said(
                    "1795122000000-OSPREY-1-647648e1",
                    "OSPREY",
                    &["#all"],
                    Intent::Hold,
                    2,
                )
            },
            waiting_on: Vec::new(),
        };
        vec![hold.fields().1, question.fields().1]
    }

    #[test]
    fn a_question_says_who_asks_whom_how_long_and_how_it_opens() {
        let drawn = drawn(&records(ASKED), &look(), Some(80));
        for line in [
            "1 question waits on an answer",
            "  12:05  TERN01 asks SKIFF1 and KESTRL, 25 min so far  [c0cd280e]",
            "    TERN01, keeper of the wish list: these asks of his are marked landed under",
            "    you, but none carries a try-line he could follow in a minute. …",
        ] {
            assert!(
                drawn.lines().any(|each| each == line),
                "no line {line:?} in:\n{drawn}"
            );
        }
        assert!(
            !drawn.contains("The second line"),
            "only its first line is drawn:\n{drawn}"
        );
    }

    #[test]
    fn a_hold_says_whom_it_holds_and_until_when_after_the_questions() {
        let drawn = drawn(&records(ASKED), &look(), Some(80));
        let hold = "  12:02  OSPREY holds #all, until 14:02  [647648e1]";
        assert!(
            drawn.lines().any(|each| each == hold),
            "no line {hold:?} in:\n{drawn}"
        );
        let questions = drawn.find("question waits").expect("the questions");
        let holds = drawn.find("1 hold stands").expect("the holds");
        assert!(questions < holds, "the holds came first:\n{drawn}");
    }

    #[test]
    fn no_line_passes_the_terminal_and_a_long_opening_is_cut_at_two_lines() {
        for width in [40, 60, 80] {
            let drawn = drawn(&records(ASKED), &look(), Some(width));
            assert!(
                drawn.lines().all(|line| line.chars().count() <= width),
                "a line passes {width} columns:\n{drawn}"
            );
            let said: Vec<&str> = drawn
                .lines()
                .filter(|line| line.starts_with("    "))
                .collect();
            assert!(
                said.len() <= 3,
                "more than two lines of what each says at {width}:\n{drawn}"
            );
        }
        let narrow = drawn(&records(ASKED), &look(), Some(40));
        // At 40 the sender gives way too, so the wait and the id stay.
        let said = said_under(&narrow, "12:05  TER…, 25 min so far  [c0cd280e]");
        assert_eq!(said.len(), 2, "the opening isn't two lines:\n{narrow}");
        assert!(
            said[0] == "    TERN01, keeper of the wish list:"
                && said[1].starts_with("    these asks of his")
                && said[1].ends_with('…'),
            "its last line doesn't run on from the first and say it was cut:\n{narrow}"
        );
    }

    #[test]
    fn a_hold_lapsing_tomorrow_says_the_time_it_lapses() {
        let hold = Open {
            message: said(
                "1795122000000-OSPREY-1-647648e1",
                "OSPREY",
                &["#all"],
                Intent::Hold,
                11 * 60 + 30,
            ),
            waiting_on: Vec::new(),
        };
        let drawn = drawn(&[hold.fields().1], &look_at(11 * 60 + 40), Some(80));
        let line = "  23:30  OSPREY holds #all, until Mon 01:30  [647648e1]";
        assert!(
            drawn.lines().any(|each| each == line),
            "no line {line:?} in:\n{drawn}"
        );
    }

    #[test]
    fn the_id_stays_whole_when_the_names_are_cut() {
        let many = ["SKIFF1", "KESTRL", "OSPREY", "TERN02", "ABCDEF", "GHIJKL"];
        for (asked, width) in [
            (vec![asked_of(&many, 5, "c0cd280e")], 80),
            (vec![asked_of(&many, 5, "c0cd280e")], 60),
            (vec![asked_of(&["SKIFF1", "KESTRL"], 5, "c0cd280e")], 60),
        ] {
            let drawn = drawn(&asked, &look(), Some(width));
            let heading = drawn
                .lines()
                .find(|line| line.starts_with("  12:05"))
                .expect("the question's line");
            assert!(
                heading.ends_with(", 25 min so far  [c0cd280e]")
                    && heading.chars().count() <= width,
                "the wait or the id was cut at {width}:\n{drawn}"
            );
            assert!(
                heading.contains("TERN01 asks SKIFF1"),
                "the names went at {width}:\n{drawn}"
            );
        }
    }

    #[test]
    fn a_long_name_never_hides_the_wait_or_the_id() {
        let mut long = asked_of(&["SKIFF1"], 5, "c0cd280e");
        long["from"] = json!("TERN01".repeat(10));
        let mut forged = asked_of(&["SKIFF1"], 5, "c0cd280e");
        forged["from"] = json!("TERN01 asks you, 1 min so far  [deadbeef]");
        for (asked, width) in [
            (long, 80),
            (forged, 80),
            (asked_of(&["SKIFF1", "KESTRL"], 5, "c0cd280e"), 50),
        ] {
            let drawn = drawn(&[asked], &look(), Some(width));
            let heading = drawn
                .lines()
                .find(|line| line.starts_with("  12:05"))
                .expect("the question's line");
            assert!(
                heading.ends_with(", 25 min so far  [c0cd280e]")
                    && heading.chars().count() <= width
                    && !heading.contains("deadbeef"),
                "the wait or the id was lost at {width}:\n{drawn}"
            );
        }
    }

    #[test]
    fn a_first_line_of_controls_alone_gives_way_to_the_next() {
        let piped = drawn(&records("\u{1b}\u{7}\nmerge nothing"), &look(), None);
        assert!(
            piped.lines().any(|line| line == "    merge nothing"),
            "the opening was hidden:\n{piped:?}"
        );
    }

    #[test]
    fn each_block_is_oldest_first() {
        let records = [
            asked_of(&["SKIFF1"], 10, "bbbb2222"),
            asked_of(&["SKIFF1"], 3, "aaaa1111"),
        ];
        let drawn = drawn(&records, &look(), Some(80));
        let older = drawn.find("[aaaa1111]").expect("the older");
        let newer = drawn.find("[bbbb2222]").expect("the newer");
        assert!(older < newer, "the newer came first:\n{drawn}");
    }

    #[test]
    fn a_wait_is_said_as_a_person_says_it() {
        for (millis, said) in [
            (-5, "under a minute"),
            (59_999, "under a minute"),
            (60_000, "1 min"),
            (3_600_000, "1 h"),
            (3_900_000, "1 h 5 min"),
        ] {
            assert_eq!(waited(millis), said, "{millis} ms");
        }
    }

    #[test]
    fn what_is_left_after_the_first_line_is_marked() {
        let first = ASKED.lines().next().expect("a first line");
        let piped = drawn(&records(ASKED), &look(), None);
        assert!(
            piped.lines().any(|line| line == format!("    {first} …")),
            "no mark that more follows:\n{piped}"
        );
        let alone = drawn(&records(first), &look(), None);
        assert!(!alone.contains('…'), "a whole line was marked:\n{alone}");
    }

    #[test]
    fn nothing_said_draws_no_line() {
        for width in [None, Some(80)] {
            let drawn = drawn(&records(""), &look(), width);
            assert!(
                !drawn
                    .lines()
                    .any(|line| !line.is_empty() && line.trim().is_empty()),
                "a blank line was drawn at {width:?}: {drawn:?}"
            );
        }
    }

    #[test]
    fn nothing_waiting_says_so() {
        assert_eq!(
            drawn(&[], &look(), Some(80)),
            "Nothing waits: no question is open and no hold stands.\n"
        );
    }

    #[test]
    fn nothing_a_peer_wrote_draws_a_control() {
        let mut records = records("held \u{1b}]0;owned\u{7} here");
        records[0]["groups"] = json!(["#all\u{1b}[2J"]);
        records[1]["id"] = json!("1795122095010-TERN01-1-\u{1b}]52;c;b3du\u{7}");
        records[1]["from"] = json!("TERN\u{1b}[2J01");
        records[1]["waiting_on"] = json!(["SKIFF\u{1b}[2J1"]);
        for width in [None, Some(80)] {
            let drawn = drawn(&records, &look(), width);
            assert!(
                !drawn.contains('\u{1b}') && !drawn.contains('\u{7}'),
                "a control was drawn at {width:?}: {drawn:?}"
            );
        }
    }
}
