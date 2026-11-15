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

use crate::mesh;
use crate::record::{Intent, Message};

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
}
