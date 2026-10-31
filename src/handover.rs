//! A machine leaving. A machine that says when it leaves (`leaves_at`, as
//! one lent for a while does) takes with it whatever is kept only there.
//! So from `HANDOVER` before then, its node tells each session running
//! there, once, to put its work somewhere that lasts and stop. It says so
//! through the session's steer inbox, as coming from the machine rather
//! than the person, and asks for nothing but words: a steer from the
//! machine never pauses or stops a session itself. Once none runs there,
//! it has handed over ([`handed_over`]), and a link whose way in can
//! release the machine lets it go, as [`crate::link`] says.
//!
//! A machine on its battery below the mark (`awake::low`) leaves for a
//! while too: it takes no new work and is let sleep. Its node tells each
//! session there, once while it stays low, to put its work somewhere that
//! lasts, though not to stop, since a session waits out a sleep
//! ([`Sleeping`]).

use std::collections::BTreeSet;

use jiff::{SignedDuration, Timestamp};
use serde_json::json;

use crate::hold::Held;
use crate::record::{Session, State};
use crate::steer::Steer;

/// How long before its machine leaves a session is told to hand over:
/// time to commit and push what it has and say what's next.
pub const HANDOVER: SignedDuration = SignedDuration::from_mins(10);

/// Who a steer from the machine itself says it is from.
pub const FROM: &str = "machine";

/// The sessions already told their machine leaves, so each is told once.
#[derive(Debug, Default)]
pub struct Told {
    addresses: BTreeSet<String>,
}

impl Told {
    /// The steers due at `now` to the sessions running on `machine`, which
    /// leaves at `leaves`, that haven't been told yet: none until
    /// `HANDOVER` before it leaves, and none for a machine that says
    /// nothing of leaving.
    #[must_use]
    pub fn due(
        &self,
        machine: &str,
        leaves: Option<Timestamp>,
        now: Timestamp,
        sessions: &[Session],
    ) -> Vec<Steer> {
        let Some(leaves) = leaves else {
            return Vec::new();
        };
        if now < leaves - HANDOVER {
            return Vec::new();
        }
        untold(&self.addresses, machine, sessions, &said(leaves, now), now)
    }

    /// Counts the session at `address` told, once its steer is delivered.
    pub fn told(&mut self, address: &str) {
        self.addresses.insert(address.to_owned());
    }
}

/// The sessions told their machine runs low on its battery, so each is
/// told once while it stays low, and again should it run low once more
/// after charging.
#[derive(Debug, Default)]
pub struct Sleeping {
    addresses: BTreeSet<String>,
}

impl Sleeping {
    /// The steers due at `now` to the sessions running on `machine`, which
    /// is low on its battery or not as `low` says, that haven't been told
    /// yet: none while it isn't. Such a machine takes no new work and is
    /// let sleep, which pauses what runs there until it wakes, so each is
    /// told to put its work somewhere that lasts, though not to stop.
    #[must_use]
    pub fn due(
        &self,
        machine: &str,
        low: bool,
        now: Timestamp,
        sessions: &[Session],
    ) -> Vec<Steer> {
        if !low {
            return Vec::new();
        }
        let said = format!(
            "This machine's battery is below {}%, so it takes no new work and \
             will be let sleep. Commit and push what you have, and put what \
             can't be pushed and a note of what's next where your work is \
             shared. While it sleeps this session waits, and it carries on \
             once the machine wakes.",
            crate::awake::LOW_CHARGE
        );
        untold(&self.addresses, machine, sessions, &said, now)
    }

    /// Counts the session at `address` told, once its steer is delivered.
    pub fn told(&mut self, address: &str) {
        self.addresses.insert(address.to_owned());
    }

    /// Forgets who was told, once the machine is no longer low.
    pub fn charged(&mut self) {
        self.addresses.clear();
    }
}

/// A steer from the machine, saying `text` at `now`, to each session
/// running on `machine` that isn't among `told`.
fn untold(
    told: &BTreeSet<String>,
    machine: &str,
    sessions: &[Session],
    text: &str,
    now: Timestamp,
) -> Vec<Steer> {
    let said = json!({
        "text": text,
        "at": now.to_string(),
        "from": FROM,
    });
    sessions
        .iter()
        .filter(|session| {
            session.machine == machine
                && matches!(session.state, State::Busy | State::Idle)
                && !told.contains(&session.address)
        })
        .filter_map(|session| Steer::new(&session.address, &said).ok())
        .collect()
}

/// How much work runs on a machine: each session living there, as
/// `held_as` names the held program each is (if any), and each program
/// `held` there that no living session is, such as a build sent to it,
/// which is work as much as a session is.
#[must_use]
pub fn running(held_as: &[Option<String>], held: &[Held]) -> usize {
    let sessions: BTreeSet<&str> = held_as.iter().flatten().map(String::as_str).collect();
    held_as.len()
        + held
            .iter()
            .filter(|one| !sessions.contains(one.id.as_str()))
            .count()
}

/// Whether a machine that leaves at `leaves` has handed over at `now`,
/// with `running` sessions there now and, as `ran` says, whether any ran
/// there since it linked: it says when it leaves, and none runs there now
/// after one has, or after its handover has begun. So a machine lent for
/// work isn't let go before any work reaches it, nor kept once it's done.
#[must_use]
pub fn handed_over(leaves: Option<Timestamp>, now: Timestamp, ran: bool, running: usize) -> bool {
    leaves.is_some_and(|leaves| running == 0 && (ran || now >= leaves - HANDOVER))
}

/// What a session is told, at `now`, of its machine leaving at `leaves`.
fn said(leaves: Timestamp, now: Timestamp) -> String {
    let minutes = u64::try_from(now.duration_until(leaves).as_secs())
        .unwrap_or(0)
        .div_ceil(60);
    let when = match minutes {
        0 => "now".to_owned(),
        1 => "in a minute".to_owned(),
        _ => format!("in {minutes} minutes"),
    };
    format!(
        "This machine leaves at {} ({when}), and nothing kept only here \
         outlasts it. Commit and push what you have, put what can't be pushed \
         and a note of what's next where your work is shared, and stop.",
        leaves.strftime("%H:%M UTC")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn session(address: &str, machine: &str, state: &str) -> Session {
        serde_json::from_value(json!({
            "id": address.to_lowercase(),
            "address": address,
            "machine": machine,
            "groups": [],
            "state": state,
            "at": "2026-10-30T18:00:00Z",
        }))
        .expect("a session")
    }

    fn leaves() -> Timestamp {
        "2026-10-30T18:34:00Z".parse().expect("a time")
    }

    fn addresses(steers: &[Steer]) -> Vec<&str> {
        steers.iter().map(|steer| steer.address.as_str()).collect()
    }

    /// A program held as `id`.
    fn held(id: &str) -> Held {
        serde_json::from_value(json!({
            "id": id, "command": ["sh"], "folder": "/", "holder": 1, "pid": 2,
            "since": "2026-10-30T18:00:00Z"
        }))
        .expect("a held program")
    }

    #[test]
    fn a_program_held_with_no_session_is_running_work() {
        assert_eq!(
            running(&[], &[held("build")]),
            1,
            "a build held there runs, though no session does"
        );
        assert_eq!(
            running(&[Some("pi".to_owned())], &[held("pi"), held("build")]),
            2,
            "a session held there counts once, beside the build"
        );
        assert_eq!(
            running(&[None], &[]),
            1,
            "a session nobody holds there still runs"
        );
    }

    #[test]
    fn a_machine_hands_over_once_its_work_is_done_or_its_handover_begun() {
        let early = leaves() - HANDOVER - SignedDuration::from_secs(1);
        assert!(
            handed_over(Some(leaves()), early, true, 0),
            "a machine whose sessions have all ended has handed over"
        );
        assert!(
            !handed_over(Some(leaves()), early, true, 1),
            "one with a session running hasn't"
        );
        assert!(
            !handed_over(Some(leaves()), early, false, 0),
            "nor has one no work has reached yet"
        );
        assert!(
            handed_over(Some(leaves()), leaves() - HANDOVER, false, 0),
            "unless its handover has begun"
        );
        assert!(
            !handed_over(None, leaves(), true, 0),
            "a machine that says nothing of leaving never hands over"
        );
    }

    #[test]
    fn a_machine_that_says_nothing_of_leaving_tells_nobody() {
        let sessions = [session("AAAAAA", "lent", "busy")];
        let due = Told::default().due("lent", None, leaves(), &sessions);
        assert!(due.is_empty(), "nobody is told: {:?}", addresses(&due));
    }

    #[test]
    fn sessions_are_told_from_the_handover_on() {
        let sessions = [session("AAAAAA", "lent", "busy")];
        let told = Told::default();
        let early = leaves() - HANDOVER - SignedDuration::from_secs(1);
        assert!(
            told.due("lent", Some(leaves()), early, &sessions)
                .is_empty(),
            "nobody is told before the handover"
        );
        let due = told.due("lent", Some(leaves()), leaves() - HANDOVER, &sessions);
        assert_eq!(addresses(&due), ["AAAAAA"], "the session is told at it");
        let late = told.due("lent", Some(leaves()), leaves(), &sessions);
        assert_eq!(addresses(&late), ["AAAAAA"], "and later, until it is told");
    }

    #[test]
    fn only_the_sessions_running_on_the_machine_are_told() {
        let sessions = [
            session("AAAAAA", "lent", "busy"),
            session("BBBBBB", "lent", "idle"),
            session("CCCCCC", "lent", "ended"),
            session("DDDDDD", "home", "busy"),
            session("EEEEEE", "lent", "unreachable"),
        ];
        let due = Told::default().due("lent", Some(leaves()), leaves(), &sessions);
        assert_eq!(
            addresses(&due),
            ["AAAAAA", "BBBBBB"],
            "a session that ended, or runs on another machine, isn't told"
        );
    }

    #[test]
    fn sessions_are_told_once_while_their_machine_runs_low() {
        let sessions = [
            session("AAAAAA", "laptop", "busy"),
            session("BBBBBB", "laptop", "ended"),
            session("CCCCCC", "home", "busy"),
        ];
        let mut sleeping = Sleeping::default();
        assert!(
            sleeping
                .due("laptop", false, leaves(), &sessions)
                .is_empty(),
            "nobody is told while the machine isn't low"
        );
        let due = sleeping.due("laptop", true, leaves(), &sessions);
        assert_eq!(
            addresses(&due),
            ["AAAAAA"],
            "the session running on it is told once it is"
        );
        let said = due[0]
            .body
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            said.contains("below 20%") && said.contains("carries on once the machine wakes"),
            "it says why, and that the session isn't to stop: {said}"
        );
        sleeping.told("AAAAAA");
        assert!(
            sleeping.due("laptop", true, leaves(), &sessions).is_empty(),
            "a session told isn't told again while the machine stays low"
        );
        sleeping.charged();
        assert_eq!(
            addresses(&sleeping.due("laptop", true, leaves(), &sessions)),
            ["AAAAAA"],
            "but is once it runs low again after charging"
        );
    }

    #[test]
    fn a_session_is_told_once_its_steer_is_delivered() {
        let sessions = [
            session("AAAAAA", "lent", "busy"),
            session("BBBBBB", "lent", "busy"),
        ];
        let mut told = Told::default();
        told.told("AAAAAA");
        let due = told.due("lent", Some(leaves()), leaves(), &sessions);
        assert_eq!(
            addresses(&due),
            ["BBBBBB"],
            "a session told isn't told again, and one not yet told still is"
        );
    }

    #[test]
    fn a_session_is_told_when_its_machine_leaves_by_the_machine() {
        let sessions = [session("AAAAAA", "lent", "busy")];
        let now = leaves() - SignedDuration::from_secs(9 * 60 + 30);
        let due = Told::default().due("lent", Some(leaves()), now, &sessions);
        let said = &due.first().expect("a steer").body;
        assert_eq!(
            said.get("text").and_then(Value::as_str),
            Some(
                "This machine leaves at 18:34 UTC (in 10 minutes), and nothing \
                 kept only here outlasts it. Commit and push what you have, put \
                 what can't be pushed and a note of what's next where your work \
                 is shared, and stop."
            ),
            "it says when, and what to do before then"
        );
        assert_eq!(
            said.get("from").and_then(Value::as_str),
            Some(FROM),
            "as the machine, not the person"
        );
        assert!(said.get("act").is_none(), "it asks only in words");
    }
}
