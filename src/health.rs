//! How a realm's network stands, as `mycelium health` prints it for a
//! daily read: one JSON line saying each link, how often it dropped in
//! the last day and, while it is down, for how long, the sessions on
//! each machine, the presences written in the last day that still say
//! they run with their process gone, how long the node took to answer,
//! and, while this machine's node refuses its kit, why.

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

/// What the store kept of each link, by the machine at its other end.
pub struct Kept<Ended, Refused, Linked> {
    /// How many times the link dropped in the last day.
    pub ended: Ended,
    /// Why its last was refused, if none was admitted since.
    pub refused: Refused,
    /// When it last stood, in seconds since the epoch, if it ever did.
    pub linked: Linked,
}

/// The line `read` makes, with what `kept` says of each link. A link not
/// up says for how long, in whole seconds, as `downFor`: since it last
/// stood, or null for a machine never linked. So a link down a minute
/// for a restart reads apart from one down all night.
#[must_use]
pub fn line<Ended, Refused, Linked>(read: &Read<'_>, kept: &Kept<Ended, Refused, Linked>) -> Value
where
    Ended: Fn(&str) -> usize,
    Refused: Fn(&str) -> Option<String>,
    Linked: Fn(&str) -> Option<i64>,
{
    let mut links: Vec<Value> = read
        .machines
        .iter()
        .filter(|machine| machine.name != read.here)
        .map(|machine| {
            let ended = (kept.ended)(&machine.name);
            if machine.state == Readiness::Ready {
                return json!({"machine": machine.name, "state": "up", "ended24h": ended});
            }
            // A mark ahead of the read, as a clock stepped back leaves
            // it, reads as down no time rather than a time to come.
            let down = (kept.linked)(&machine.name).map(|at| (read.at - at).max(0));
            // A machine refused, as for speaking another version, says
            // why, so it isn't read as one that is only asleep.
            match (kept.refused)(&machine.name) {
                Some(why) => json!({
                    "machine": machine.name, "state": "refused", "why": why,
                    "ended24h": ended, "downFor": down,
                }),
                None => json!({
                    "machine": machine.name, "state": "unreachable",
                    "ended24h": ended, "downFor": down,
                }),
            }
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
    let dead = read
        .sessions
        .iter()
        .filter(|one| dead(one) && lately(one, read.at))
        .count();
    // Whole seconds, as the day's other figures are, rounded so a 7.9 s
    // answer reads as the 8 it nearly was.
    let slowest = (read.slowest.as_millis() + 500) / 1000;
    let mut line = json!({
        "realm": read.realm,
        "at": read.at,
        "links": links,
        "sessions": sessions,
        "deadPresences": dead,
        "slowestAsk": u64::try_from(slowest).unwrap_or(u64::MAX),
    });
    // A kit this machine's node refused stays off every path until the
    // node starts again, so the read says why while it lasts, escaped as
    // the node's start-up line says it, since a planted file's name can be
    // in it.
    if let Some(why) = read
        .machines
        .iter()
        .find(|machine| machine.name == read.here)
        .and_then(|machine| machine.kit.as_ref()?.refused.as_ref())
    {
        line["kitRefused"] = json!(crate::declared::controls_escaped(why));
    }
    line
}

/// How long a presence left saying it runs is counted: a day, as the
/// links' endings are, since a daily read wants what is new, and the
/// node keeps an ended session's record long after.
const DEAD_FOR: i64 = 24 * 60 * 60;

/// Whether `session`'s presence was last written within [`DEAD_FOR`] of
/// `at`, in seconds since the epoch. One whose time can't be read counts,
/// so a record gone wrong is seen rather than hidden.
fn lately(session: &Session, at: i64) -> bool {
    session
        .at
        .parse::<jiff::Timestamp>()
        .ok()
        .is_none_or(|written| at - written.as_second() < DEAD_FOR)
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

    /// A reader of what a store kept of a link, by its machine.
    type Reads<T> = fn(&str) -> T;

    /// What a store kept of links that never dropped, were never refused
    /// and never stood.
    fn nothing_kept() -> Kept<Reads<usize>, Reads<Option<String>>, Reads<Option<i64>>> {
        Kept {
            ended: |_: &str| 0,
            refused: |_: &str| None,
            linked: |_: &str| None,
        }
    }

    #[test]
    fn a_read_says_each_link_its_drops_and_each_machines_sessions() {
        let machines = [
            machine("skiff", Readiness::Ready),
            machine("tern", Readiness::Ready),
            machine("osprey", Readiness::Unreachable),
        ];
        let busy = json!({"state": "busy", "busy": true});
        let listening = json!({"state": "listening"});
        let parked = json!({"state": "parked"});
        let sessions = [
            session("a", "skiff", "busy", &busy),
            session("b", "skiff", "idle", &listening),
            session("c", "skiff", "ended", &parked),
            session("d", "skiff", "ended", &listening),
            session("e", "skiff", "ended", &busy),
            session("f", "osprey", "unreachable", &listening),
        ];
        let read = Read {
            realm: "personal",
            at: 1_794_470_400,
            here: "skiff",
            machines: &machines,
            sessions: &sessions,
            slowest: Duration::from_millis(7_900),
        };
        let line = line(
            &read,
            &Kept {
                ended: |machine: &str| usize::from(machine == "osprey") * 1_410,
                refused: |_: &str| None,
                linked: |_: &str| Some(1_794_470_400 - 7 * 60),
            },
        );
        assert_eq!(
            line,
            json!({
                "realm": "personal",
                "at": 1_794_470_400,
                "links": [
                    {
                        "machine": "osprey", "state": "unreachable", "ended24h": 1_410,
                        "downFor": 420,
                    },
                    {"machine": "tern", "state": "up", "ended24h": 0},
                ],
                "sessions": {
                    "osprey": {"busy": 0, "idle": 0, "ended": 0, "unreachable": 1},
                    "skiff": {"busy": 1, "idle": 1, "ended": 3, "unreachable": 0},
                },
                "deadPresences": 2,
                "slowestAsk": 8,
            }),
            "this machine is no link, and only an ended one still saying it runs is dead"
        );
    }

    #[test]
    fn a_link_refused_says_why_until_it_is_up() {
        let machines = [
            machine("skiff", Readiness::Ready),
            machine("tern", Readiness::Ready),
            machine("osprey", Readiness::Unreachable),
        ];
        let read = Read {
            realm: "personal",
            at: 1_794_470_400,
            here: "skiff",
            machines: &machines,
            sessions: &[],
            slowest: Duration::ZERO,
        };
        let why = "osprey speaks link version 2 and skiff 1, so they link again once \
                   skiff's mycelium is as new as osprey's";
        let line = line(
            &read,
            &Kept {
                ended: |_: &str| 0,
                refused: |_: &str| Some(why.to_owned()),
                linked: |_: &str| None,
            },
        );
        assert_eq!(
            line["links"],
            json!([
                {
                    "machine": "osprey", "state": "refused", "why": why, "ended24h": 0,
                    "downFor": null,
                },
                {"machine": "tern", "state": "up", "ended24h": 0},
            ]),
            "a refusal says why while no link reaches the machine, and a link up outranks it"
        );
    }

    #[test]
    fn a_link_down_says_since_it_last_stood_and_one_never_linked_says_null() {
        let machines = [
            machine("skiff", Readiness::Ready),
            machine("tern", Readiness::Unreachable),
            machine("osprey", Readiness::Unreachable),
            machine("wren", Readiness::Unreachable),
        ];
        let at = 1_794_470_400;
        let read = Read {
            realm: "personal",
            at,
            here: "skiff",
            machines: &machines,
            sessions: &[],
            slowest: Duration::ZERO,
        };
        let line = line(
            &read,
            &Kept {
                linked: |machine: &str| match machine {
                    "tern" => Some(at - 9 * 60 * 60),
                    // As a clock stepped back leaves a mark.
                    "wren" => Some(at + 30),
                    _ => None,
                },
                ended: |_: &str| 0,
                refused: |_: &str| None,
            },
        );
        let down: Vec<(&str, &Value)> = line["links"]
            .as_array()
            .expect("links")
            .iter()
            .map(|link| (link["machine"].as_str().expect("named"), &link["downFor"]))
            .collect();
        assert_eq!(
            down,
            [
                ("osprey", &Value::Null),
                ("tern", &json!(32_400)),
                ("wren", &json!(0)),
            ],
            "down all night reads apart from never linked, and a mark ahead reads as no time"
        );
    }

    #[test]
    fn a_kit_this_machine_refused_says_why() {
        let why = "its bin may be written by others";
        let kitted = |name: &str, refused: Option<&str>| {
            let mut one = machine(name, Readiness::Ready);
            one.kit = Some(crate::facts::Kit {
                path: "/home/someone/kit".to_owned(),
                tools: Vec::new(),
                refused: refused.map(str::to_owned),
            });
            one
        };
        let read = |machines: &[Machine]| {
            line(
                &Read {
                    realm: "personal",
                    at: 1_794_470_400,
                    here: "skiff",
                    machines,
                    sessions: &[],
                    slowest: Duration::ZERO,
                },
                &nothing_kept(),
            )
        };
        assert_eq!(
            read(&[kitted("skiff", Some(why))])["kitRefused"],
            json!(why),
            "a kit this machine's node refused says why"
        );
        let others = read(&[kitted("tern", Some(why)), kitted("skiff", None)]);
        assert!(
            others.get("kitRefused").is_none(),
            "a kit carried here, or refused on another machine, says nothing: {others}"
        );
        assert_eq!(
            read(&[kitted("skiff", Some("its bin holds \"x\nkit ok\""))])["kitRefused"],
            json!("its bin holds \"x\\nkit ok\""),
            "a name in why can't start a line of its own"
        );
    }

    #[test]
    fn a_presence_left_saying_it_runs_is_counted_for_a_day() {
        let machines = [machine("skiff", Readiness::Ready)];
        let busy = json!({"state": "busy", "busy": true});
        let written = |id: &str, at: &str| {
            let mut one = session(id, "skiff", "ended", &busy);
            at.clone_into(&mut one.at);
            one
        };
        let sessions = [
            written("lately", "2026-11-12T07:00:00Z"),
            written("at-the-edge", "2026-11-11T08:00:01Z"),
            written("yesterday", "2026-11-11T07:59:59Z"),
            written("last-week", "2026-11-05T08:00:00Z"),
            written("unreadable", "sometime"),
        ];
        let read = Read {
            realm: "personal",
            at: 1_794_470_400,
            here: "skiff",
            machines: &machines,
            sessions: &sessions,
            slowest: Duration::ZERO,
        };
        let line = line(&read, &nothing_kept());
        assert_eq!(
            line["deadPresences"], 3,
            "a presence last written within the day is counted, and one whose time can't be \
             read too, so it is seen rather than hidden; one older than a day is not"
        );
    }
}
