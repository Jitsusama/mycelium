//! What one node holds in memory for its clients: the sessions it knows of
//! and the latest messages, each change returned as the event a watch
//! sends.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::group::{Group, Pattern};
use crate::record::{Message, Session};
use crate::unanswered::{Open, Unanswered};
use crate::wire::Event;

/// How many messages the node keeps in memory, oldest let go first. The
/// mesh keeps a week of them, a few thousand, so this holds all of them
/// with room to spare.
pub const KEPT_MESSAGES: usize = 50_000;

/// The sessions and messages one node holds.
#[derive(Debug, Default)]
pub struct State {
    sessions: BTreeMap<String, Session>,
    /// Each shared, so a reader takes them all while the node is locked
    /// by counting each once more, and reads them once it is let go.
    messages: VecDeque<Arc<Message>>,
    message_ids: HashSet<String>,
    /// What waits among the messages, kept as each comes.
    unanswered: Unanswered,
}

impl State {
    /// Every session, by id.
    pub fn sessions(&self) -> Vec<Session> {
        self.sessions.values().cloned().collect()
    }

    /// Every session, by id, as held.
    pub fn each_session(&self) -> impl Iterator<Item = &Session> {
        self.sessions.values()
    }

    /// Every message held, in the order they were sent.
    pub fn messages(&self) -> impl Iterator<Item = &Message> {
        self.messages.iter().map(AsRef::as_ref)
    }

    /// Every message held, in the order they were sent, shared, to read
    /// once the node is let go: 50,000 take well under a millisecond, as
    /// `tests/ties.rs` measures.
    #[must_use]
    pub fn shared_messages(&self) -> Vec<Arc<Message>> {
        self.messages.iter().cloned().collect()
    }

    /// What each session says it works on, by its address, for those that
    /// say: a session on `machine` before one of the same address
    /// elsewhere, as a cell there is the one murmuring there.
    pub fn purposes(&self, machine: &str) -> HashMap<String, String> {
        let mut purposes = HashMap::new();
        let (here, elsewhere): (Vec<&Session>, Vec<&Session>) = self
            .sessions
            .values()
            .partition(|session| session.machine == machine);
        for session in elsewhere.into_iter().chain(here) {
            if let Some(working_on) = &session.working_on {
                purposes.insert(session.address.clone(), working_on.clone());
            }
        }
        purposes
    }

    /// Takes `sessions` as every session there is, returning an event for
    /// each that changed, came or went.
    pub fn set_sessions(&mut self, sessions: Vec<Session>) -> Vec<Event> {
        let mut events = Vec::new();
        let gone: Vec<String> = self
            .sessions
            .keys()
            .filter(|id| !sessions.iter().any(|session| &session.id == *id))
            .cloned()
            .collect();
        for id in gone {
            self.sessions.remove(&id);
            events.push(Event::SessionForgotten { id });
        }
        for session in sessions {
            if self.sessions.get(&session.id) != Some(&session) {
                self.sessions.insert(session.id.clone(), session.clone());
                events.push(Event::SessionChanged {
                    session: Box::new(session),
                });
            }
        }
        events
    }

    /// Keeps `message` unless it is already held, returning it if it is new,
    /// its time said in [one form](in_one_form). Messages are kept in the
    /// order they were sent, however late one arrives, as one caught up
    /// over a link does.
    pub fn add_message(&mut self, message: Message) -> Option<Message> {
        if !self.message_ids.insert(message.id.clone()) {
            return None;
        }
        let message = in_one_form(message, jiff::Timestamp::now());
        let at = self
            .messages
            .partition_point(|held| sent_first(held, &message));
        self.messages.insert(at, Arc::new(message.clone()));
        self.unanswered
            .add(&message, jiff::Timestamp::now().as_millisecond());
        while self.messages.len() > KEPT_MESSAGES {
            if let Some(gone) = self.messages.pop_front() {
                self.message_ids.remove(&gone.id);
            }
        }
        Some(message)
    }

    /// The questions waiting and holds standing among the messages held,
    /// as `now` stands, oldest first, a peer going by every address
    /// `aliases` gives it.
    pub fn unanswered(&self, aliases: impl Fn(&str) -> Vec<String>, now: i64) -> Vec<Open> {
        self.unanswered.open(aliases, now)
    }

    /// Whether the message `id` is held.
    #[must_use]
    pub fn holds_message(&self, id: &str) -> bool {
        self.message_ids.contains(id)
    }

    /// Keeps many messages at once, as when a node first reads its store.
    pub fn add_messages(&mut self, messages: Vec<Message>) {
        let heard = jiff::Timestamp::now();
        let mut messages: Vec<Message> = messages
            .into_iter()
            .map(|message| in_one_form(message, heard))
            .collect();
        messages.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
        for message in messages {
            self.add_message(message);
        }
    }

    /// The latest `limit` messages to the groups `groups` hears, oldest
    /// first.
    pub fn recent(&self, groups: &Pattern, limit: usize) -> Vec<Message> {
        let mut found: Vec<Message> = self
            .messages
            .iter()
            .rev()
            .filter(|message| message.groups.iter().any(|group| groups.matches(group)))
            .take(limit)
            .map(|message| Message::clone(message))
            .collect();
        found.reverse();
        found
    }

    /// The latest `each` messages of every group, oldest first: what each
    /// conversation was saying lately, however quiet the others are. A
    /// message to several groups is among them while it is among the
    /// latest of any.
    #[must_use]
    pub fn latest_by_group(&self, each: usize) -> Vec<Message> {
        let mut counted: HashMap<&Group, usize> = HashMap::new();
        let mut found: Vec<Message> = self
            .messages
            .iter()
            .rev()
            .filter(|message| {
                let mut latest = false;
                for group in &message.groups {
                    let count = counted.entry(group).or_default();
                    latest |= *count < each;
                    *count += 1;
                }
                latest
            })
            .map(|message| Message::clone(message))
            .collect();
        found.reverse();
        found
    }
}

/// `message` with its time said in UTC to the millisecond, as
/// `2026-11-16T17:00:00.000Z`, so the order of the times held is the
/// order of their words: one written with an offset, or more finely,
/// would otherwise sort apart from when it was sent. A time that is no
/// time is when it was `heard`, which it then stays.
fn in_one_form(mut message: Message, heard: jiff::Timestamp) -> Message {
    let sent = message.at.parse::<jiff::Timestamp>().unwrap_or(heard);
    message.at = format!("{sent:.3}");
    message
}

/// Whether `held` was sent before `message`, by time and then by id.
fn sent_first(held: &Message, message: &Message) -> bool {
    (&held.at, &held.id) < (&message.at, &message.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::Group;
    use crate::record::State as Standing;

    /// Times a second apart, the first at index one.
    const AT: [&str; 5] = [
        "2026-10-26T20:00:00Z",
        "2026-10-26T20:00:01Z",
        "2026-10-26T20:00:02Z",
        "2026-10-26T20:00:03Z",
        "2026-10-26T20:00:04Z",
    ];

    fn message(id: &str, at: &str, group: &str) -> Message {
        Message {
            id: id.to_owned(),
            at: at.to_owned(),
            from: "4UHC5Q".to_owned(),
            groups: vec![Group::new(group).expect("the test names a valid group")],
            intent: None,
            text: format!("message {id}"),
            re: None,
            supersedes: None,
            from_realm: None,
            for_realm: None,
            ends_hold: false,
            pid: None,
            hosted: false,
            invite: None,
        }
    }

    fn session(id: &str, state: Standing) -> Session {
        Session {
            id: id.to_owned(),
            address: id.to_uppercase(),
            machine: "skiff".to_owned(),
            title: None,
            working_on: None,
            groups: Vec::new(),
            state,
            step: None,
            since: None,
            orchestrated_by: None,
            quest: None,
            wants: None,
            landings: Vec::new(),
            doing: None,
            events: Vec::new(),
            window: None,
            held: None,
            started_by: None,
            attach: None,
            host: None,
            conversation: None,
            at: "2026-10-26T20:00:00Z".to_owned(),
            presence: serde_json::Value::Null.into(),
        }
    }

    #[test]
    fn a_purpose_is_what_a_session_says_it_works_on_here_first() {
        let mut state = State::default();
        let working = |id: &str, machine: &str, on: Option<&str>| Session {
            address: "KP0L6A".to_owned(),
            machine: machine.to_owned(),
            working_on: on.map(str::to_owned),
            ..session(id, Standing::Idle)
        };
        state.set_sessions(vec![
            working("a", "skiff", Some("the murmur kind")),
            working("b", "kestrel", Some("the tide")),
            working("c", "skiff", None),
        ]);
        assert_eq!(
            state.purposes("skiff"),
            HashMap::from([("KP0L6A".to_owned(), "the murmur kind".to_owned())]),
            "the session here says it, and one saying nothing says no other"
        );
    }

    #[test]
    fn a_message_is_kept_once() {
        let mut state = State::default();
        assert!(
            state.add_message(message("a", AT[1], "quest.x")).is_some(),
            "a new message is kept"
        );
        assert!(
            state.add_message(message("a", AT[1], "quest.x")).is_none(),
            "a repeat is dropped"
        );
        assert_eq!(state.recent(&Pattern::everything(), 10).len(), 1);
    }

    #[test]
    fn each_groups_latest_are_kept_however_quiet_the_others() {
        let mut state = State::default();
        state.add_messages(vec![
            message("a1", AT[1], "quest.a"),
            message("b1", AT[2], "quest.b"),
            message("b2", AT[3], "quest.b"),
            message("b3", AT[4], "quest.b"),
        ]);
        let ids: Vec<String> = state
            .latest_by_group(2)
            .into_iter()
            .map(|message| message.id)
            .collect();
        assert_eq!(
            ids,
            ["a1", "b2", "b3"],
            "a quiet group's last message stays beside a busy one's latest two, oldest first"
        );
    }

    #[test]
    fn recent_messages_come_oldest_first_from_the_groups_asked_for() {
        let mut state = State::default();
        state.add_messages(vec![
            message("c", AT[3], "quest.x"),
            message("a", AT[1], "quest.x"),
            message("b", AT[2], "peer.y"),
            message("d", AT[4], "quest.x"),
        ]);
        let quests = Pattern::new("quest.*").expect("the test names a valid pattern");
        let ids: Vec<String> = state.recent(&quests, 2).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["c", "d"]);
    }

    #[test]
    fn a_message_arriving_late_takes_its_place_by_when_it_was_sent() {
        let mut state = State::default();
        state.add_message(message("b", "2026-10-26T20:00:02Z", "quest.x"));
        state.add_message(message("c", "2026-10-26T20:00:03Z", "quest.x"));
        state.add_message(message("a", "2026-10-26T20:00:01Z", "quest.x"));
        let ids: Vec<String> = state
            .recent(&Pattern::everything(), 2)
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, ["b", "c"], "the latest two, not the last to arrive");
    }

    #[test]
    fn each_time_is_held_in_one_form_so_its_words_sort_as_it_was_sent() {
        let mut state = State::default();
        state.add_messages(vec![
            message("a", "2026-10-26T20:00:00.5Z", "quest.x"),
            message("b", "2026-10-26T15:00:01-05:00", "quest.x"),
        ]);
        let heard = state
            .add_message(message("c", "no time", "quest.x"))
            .expect("a new message");
        let held: Vec<(&str, &str)> = state
            .messages()
            .map(|message| (message.id.as_str(), message.at.as_str()))
            .collect();
        assert_eq!(
            held[..2],
            [
                ("a", "2026-10-26T20:00:00.500Z"),
                ("b", "2026-10-26T20:00:01.000Z")
            ],
            "in UTC to the millisecond, in the order they were sent"
        );
        let at: jiff::Timestamp = heard.at.parse().expect("no time is when it was heard");
        let ago = jiff::Timestamp::now().as_millisecond() - at.as_millisecond();
        assert!((0..60_000).contains(&ago), "heard {ago} ms ago");
        assert_eq!(held[2], ("c", heard.at.as_str()), "held as it was told");
    }

    #[test]
    fn only_what_changed_among_sessions_is_sent() {
        let mut state = State::default();
        assert_eq!(
            state.set_sessions(vec![session("a", Standing::Idle)]).len(),
            1
        );
        assert!(
            state
                .set_sessions(vec![session("a", Standing::Idle)])
                .is_empty(),
            "nothing changed"
        );
        assert_eq!(
            state.set_sessions(vec![session("b", Standing::Busy)]),
            [
                Event::SessionForgotten { id: "a".to_owned() },
                Event::SessionChanged {
                    session: Box::new(session("b", Standing::Busy))
                },
            ]
        );
    }
}
