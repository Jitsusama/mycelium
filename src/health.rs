//! How a realm's network stands, as `mycelium health` prints it for a
//! daily read: one JSON line saying each link and how often it dropped in
//! the last day, the sessions on each machine, the presences that still
//! say they run with their process gone, and how long the node took to
//! answer.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Value, json};

use crate::facts::{Machine, Readiness};
use crate::record::{Session, State};

/// What one read of a node found.
pub struct Read<'a> {
    /// The realm read.
    pub realm: &'a str,
    /// When, in seconds since the epoch.
    pub at: i64,
    /// This machine, whose node was read, which is no link of its own.
    pub here: &'a str,
    /// Every machine of the realm, as the node holds them.
    pub machines: &'a [Machine],
    /// Every session the node knows of.
    pub sessions: &'a [Session],
    /// The slowest of the node's answers to this read.
    pub slowest: Duration,
}

/// The line `read` makes, with `ended` saying how many times the link with
/// a machine dropped in the last day.
#[must_use]
pub fn line(read: &Read<'_>, ended: impl Fn(&str) -> usize) -> Value {
    let mut links: Vec<Value> = read
        .machines
        .iter()
        .filter(|machine| machine.name != read.here)
        .map(|machine| {
            let state = match machine.state {
                Readiness::Ready => "up",
                Readiness::Unreachable => "unreachable",
            };
            json!({"machine": machine.name, "state": state, "ended24h": ended(&machine.name)})
        })
        .collect();
    links.sort_by(|a, b| a["machine"].as_str().cmp(&b["machine"].as_str()));
    let mut sessions: BTreeMap<&str, BTreeMap<&str, u64>> = BTreeMap::new();
    for session in read.sessions {
        let counts = sessions.entry(&session.machine).or_insert_with(|| {
            ["busy", "idle", "ended", "unreachable"]
                .into_iter()
                .map(|state| (state, 0))
                .collect()
        });
        *counts.entry(named(session.state)).or_default() += 1;
    }
    let dead = read.sessions.iter().filter(|one| dead(one)).count();
    // Whole seconds, as the day's other figures are, rounded so a 7.9 s
    // answer reads as the 8 it nearly was.
    let slowest = (read.slowest.as_millis() + 500) / 1000;
    json!({
        "realm": read.realm,
        "at": read.at,
        "links": links,
        "sessions": sessions,
        "deadPresences": dead,
        "slowestAsk": u64::try_from(slowest).unwrap_or(u64::MAX),
    })
}

/// A session's state, as the line counts it.
const fn named(state: State) -> &'static str {
    match state {
        State::Busy => "busy",
        State::Idle => "idle",
        State::Ended => "ended",
        State::Unreachable => "unreachable",
    }
}

/// Whether `session` ended with its presence still saying it runs: its
/// process went without its session saying so, as a parked or closed one
/// does.
fn dead(session: &Session) -> bool {
    session.state == State::Ended
        && (session.presence["busy"] == Value::Bool(true)
            || matches!(
                session.presence["state"].as_str(),
                Some("busy" | "listening")
            ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(name: &str, state: Readiness) -> Machine {
        serde_json::from_value(json!({
            "name": name,
            "measured_at": "2026-11-12T09:00:00Z",
            "state": state,
            "arch": "x86_64",
            "cpu": {},
        }))
        .expect("a machine reads")
    }

    fn session(id: &str, machine: &str, state: &str, presence: &Value) -> Session {
        serde_json::from_value(json!({
            "id": id,
            "address": id.to_uppercase(),
            "machine": machine,
            "groups": [],
            "state": state,
            "at": "2026-11-12T09:00:00Z",
            "presence": presence,
        }))
        .expect("a session reads")
    }

    #[test]
    fn a_read_says_each_link_its_drops_and_each_machines_sessions() {
        let machines = [
            machine("optimus", Readiness::Ready),
            machine("penelope", Readiness::Ready),
            machine("methuselah", Readiness::Unreachable),
        ];
        let busy = json!({"state": "busy", "busy": true});
        let listening = json!({"state": "listening"});
        let parked = json!({"state": "parked"});
        let sessions = [
            session("a", "optimus", "busy", &busy),
            session("b", "optimus", "idle", &listening),
            session("c", "optimus", "ended", &parked),
            session("d", "optimus", "ended", &listening),
            session("e", "optimus", "ended", &busy),
            session("f", "methuselah", "unreachable", &listening),
        ];
        let read = Read {
            realm: "personal",
            at: 1_794_470_400,
            here: "optimus",
            machines: &machines,
            sessions: &sessions,
            slowest: Duration::from_millis(7_900),
        };
        let line = line(&read, |machine| {
            usize::from(machine == "methuselah") * 1_410
        });
        assert_eq!(
            line,
            json!({
                "realm": "personal",
                "at": 1_794_470_400,
                "links": [
                    {"machine": "methuselah", "state": "unreachable", "ended24h": 1_410},
                    {"machine": "penelope", "state": "up", "ended24h": 0},
                ],
                "sessions": {
                    "methuselah": {"busy": 0, "idle": 0, "ended": 0, "unreachable": 1},
                    "optimus": {"busy": 1, "idle": 1, "ended": 3, "unreachable": 0},
                },
                "deadPresences": 2,
                "slowestAsk": 8,
            }),
            "this machine is no link, and only an ended one still saying it runs is dead"
        );
    }
}
