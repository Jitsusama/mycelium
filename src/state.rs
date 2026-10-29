//! What one node holds in memory for its clients: the sessions it knows of
//! and the latest messages, each change returned as the event a watch
//! sends.

use std::collections::{BTreeMap, HashSet, VecDeque};

use crate::group::Pattern;
use crate::record::{Message, Session};
use crate::wire::Event;

/// How many messages the node keeps in memory, oldest let go first. The
/// mesh keeps a week of them, a few thousand, so this holds all of them
/// with room to spare.
pub const KEPT_MESSAGES: usize = 50_000;

/// The sessions and messages one node holds.
#[derive(Debug, Default)]
pub struct State {
    sessions: BTreeMap<String, Session>,
    messages: VecDeque<Message>,
    message_ids: HashSet<String>,
}

impl State {
    /// Every session, by id.
    pub fn sessions(&self) -> Vec<Session> {
        self.sessions.values().cloned().collect()
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

    /// Keeps `message` unless it is already held, returning it if it is new.
    /// Messages are kept in the order they were sent, however late one
    /// arrives, as one caught up over a link does.
    pub fn add_message(&mut self, message: Message) -> Option<Message> {
        if !self.message_ids.insert(message.id.clone()) {
            return None;
        }
        let at = self
            .messages
            .partition_point(|held| sent_first(held, &message));
        self.messages.insert(at, message.clone());
        while self.messages.len() > KEPT_MESSAGES {
            if let Some(gone) = self.messages.pop_front() {
                self.message_ids.remove(&gone.id);
            }
        }
        Some(message)
    }

    /// Keeps many messages at once, as when a node first reads its store.
    pub fn add_messages(&mut self, mut messages: Vec<Message>) {
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
            .cloned()
            .collect();
        found.reverse();
        found
    }
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
        }
    }

    fn session(id: &str, state: Standing) -> Session {
        Session {
            id: id.to_owned(),
            address: id.to_uppercase(),
            machine: "optimus".to_owned(),
            title: None,
            working_on: None,
            groups: Vec::new(),
            state,
            step: None,
            since: None,
            orchestrated_by: None,
            quest: None,
            wants: None,
            window: None,
            at: "2026-10-26T20:00:00Z".to_owned(),
            presence: serde_json::Value::Null,
        }
    }

    #[test]
    fn a_message_is_kept_once() {
        let mut state = State::default();
        assert!(
            state.add_message(message("a", "1", "quest.x")).is_some(),
            "a new message is kept"
        );
        assert!(
            state.add_message(message("a", "1", "quest.x")).is_none(),
            "a repeat is dropped"
        );
        assert_eq!(state.recent(&Pattern::everything(), 10).len(), 1);
    }

    #[test]
    fn recent_messages_come_oldest_first_from_the_groups_asked_for() {
        let mut state = State::default();
        state.add_messages(vec![
            message("c", "3", "quest.x"),
            message("a", "1", "quest.x"),
            message("b", "2", "peer.y"),
            message("d", "4", "quest.x"),
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
