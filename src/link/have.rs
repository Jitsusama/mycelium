//! What one end's Have says of the messages it lists, and what this end
//! hears again of those it holds.
//!
//! Each end works out its link's reach on its own, from when it last
//! heard the other, so the two can differ: an end whose record of the
//! last link is older, or missing, reaches further back. It offered then
//! every message between the two reaches, which the other end holds,
//! though it left them out of its Have as older than what it listed. A
//! Have says where its list begins, so an end offers nothing older; an
//! end older than saying so is offered what it was before. A worker can't
//! write over a message this machine holds, so each such message it said
//! was refused with a line of its own, a bout of a thousand lines at
//! every link-up, which buried the refusals that mattered. One said just
//! as this machine holds it is counted instead, and told in one line.

use serde_json::Value;

/// Where this end's offer of messages begins: at its own `reach`, or, where
/// the other end's Have said it lists every message it holds from `since`,
/// at whichever is later. The other end holds what it left out from before
/// `since`, or let it go, so offering it again only has it refused.
pub(super) fn offered_from(reach: i64, since: Option<i64>) -> i64 {
    since.map_or(reach, |since| reach.max(since))
}

/// Whether a worker's message `said` is the one this machine holds as
/// `held`: the same, as when it hands back one this machine's sessions
/// sent, or the same as this machine kept it from the worker `worker`, as
/// when it hands back one of its own.
pub(super) fn held_as_said(held: &Value, said: &Value, worker: &str) -> bool {
    held == said || *held == super::said_on_worker(said.clone(), worker)
}

/// How many messages the other end said again that this machine holds as
/// they were said, since the log last told of them.
#[derive(Debug, Default)]
pub(super) struct HeldAgain {
    count: u64,
}

impl HeldAgain {
    /// Counts one more.
    pub(super) fn count(&mut self) {
        self.count += 1;
    }

    /// The line telling of those counted since it was last told, as said by
    /// `other`, if any were, and counts again from none.
    pub(super) fn told(&mut self, other: &str) -> Option<String> {
        let count = std::mem::take(&mut self.count);
        let messages = if count == 1 { "message" } else { "messages" };
        (count > 0).then(|| {
            format!(
                "{other} said {} {messages} this machine holds already, taken as said",
                grouped(count)
            )
        })
    }
}

/// `count` in digits grouped by thousands, as a person reads a number.
fn grouped(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_offer_begins_at_the_later_of_the_reach_and_the_haves_start() {
        assert_eq!(offered_from(1_000, Some(5_000)), 5_000, "the Have's start");
        assert_eq!(offered_from(5_000, Some(1_000)), 5_000, "this end's reach");
        assert_eq!(
            offered_from(1_000, None),
            1_000,
            "an older end's: the reach"
        );
    }

    #[test]
    fn a_message_is_held_as_said_as_it_was_sent_or_as_it_was_kept_from_the_worker() {
        let sent = json!({"id": "1-WRENAA-1-0", "from": "WRENAA", "text": "hello"});
        let kept = super::super::said_on_worker(sent.clone(), "tern");
        let changed = json!({"id": "1-WRENAA-1-0", "from": "WRENAA", "text": "goodbye"});

        assert!(held_as_said(&sent, &sent, "tern"), "one this machine sent");
        assert!(held_as_said(&kept, &sent, "tern"), "one the worker sent");
        assert!(!held_as_said(&sent, &changed, "tern"), "one written over");
        assert!(
            !held_as_said(&kept, &changed, "tern"),
            "one of its own written over"
        );
        assert!(
            !held_as_said(&kept, &sent, "kestrel"),
            "one kept from another worker"
        );
    }

    #[test]
    fn many_messages_held_already_are_told_in_one_line() {
        let mut held = HeldAgain::default();
        for _ in 0..1_087 {
            held.count();
        }

        assert_eq!(
            held.told("tern").as_deref(),
            Some("tern said 1,087 messages this machine holds already, taken as said")
        );
        assert_eq!(held.told("tern"), None, "none are told twice");
    }

    #[test]
    fn one_message_held_already_is_told_as_one() {
        let mut held = HeldAgain::default();
        held.count();

        assert_eq!(
            held.told("tern").as_deref(),
            Some("tern said 1 message this machine holds already, taken as said")
        );
    }

    #[test]
    fn a_count_is_grouped_by_thousands() {
        let counts = [0, 7, 999, 1_000, 12_345, 1_234_567].map(grouped);
        assert_eq!(counts, ["0", "7", "999", "1,000", "12,345", "1,234,567"]);
    }
}
