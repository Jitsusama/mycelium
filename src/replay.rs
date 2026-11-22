//! Placing kept work again under other weights, to say which set would
//! have placed it better. Each start a node kept is decided again with
//! the candidate weights, as of when it was placed and from the facts and
//! loads it was placed by. Where the candidate would have drawn the kept
//! choice too, the kept choice stands, so replaying the weights that
//! placed it changes nothing; where it couldn't have, the candidate's
//! choice replaces it. Both are then judged in hindsight by the same
//! balance readings.
//!
//! Every other answer that named a machine, an ask of where or a run,
//! which a record doesn't tell apart, is answered again the same way and
//! counted, with each that would have differed, but never judged:
//! hindsight judges starts, the load a machine's room has yet to show, and
//! a run's load passes with its asker. Answers that named no machine are
//! left as they were, starts and asks alike.
//!
//! An answer the rules placing work now would no longer give under the
//! weights that gave it, as a worker refusing work that names none, is
//! left as it was and counted apart, neither a start nor an ask: what
//! the rules changed since is never read as the replayed weights' doing.
//!
//! Those readings carry the work where it was kept, so a changed choice
//! is judged on a machine that never carried it: a replay flatters a set
//! that changes much, and how many it changed is said beside its
//! hindsight so a reader can see how far it leans on that.

use serde::{Deserialize, Serialize};

use crate::balance::Reading;
use crate::hindsight::{self, Hindsight};
use crate::placement::{self, Placement, Weights};
use crate::placements::Record;

/// The name of what a replay means. A new definition gets a new name, so
/// lines kept under an old one are never read as the new.
pub const MEASURES: &str = "replay/2";

/// What placing kept work again under other weights says.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Replay {
    /// What this means: [`MEASURES`].
    pub measures: String,
    /// The weights replayed.
    pub weights: Weights,
    /// How many kept starts were placed again.
    pub starts: usize,
    /// Each start the weights would have placed elsewhere.
    pub changed: Vec<Change>,
    /// How many kept answers that weren't starts, asks of where and runs,
    /// named a machine and were answered again.
    pub asks: usize,
    /// Each of those the weights would have answered elsewhere. None is
    /// judged in hindsight: this says only how often the weights would
    /// have told an asker something else.
    pub answered: Vec<Change>,
    /// How many kept answers that named a machine the rules now wouldn't
    /// draw under the weights that gave them. Each is left as it was and
    /// counted in neither `starts` nor `asks`.
    pub stale: usize,
    /// Hindsight on the starts as they were placed.
    pub kept: Hindsight,
    /// Hindsight on the starts as the weights would have placed them.
    pub replayed: Hindsight,
}

/// A kept answer the weights replayed would have given elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// When it was answered.
    pub at: String,
    /// The machine it named.
    pub kept: String,
    /// The machine the weights would have named; none where they would
    /// have named none.
    pub replayed: Option<String>,
}

/// Answers each kept placement in `records` that named a machine again
/// under `weights`, judging the starts, as placed and as replayed, by the
/// balance `readings` kept around them.
#[must_use]
pub fn replay(records: &[Record], readings: &[Reading], weights: &Weights) -> Replay {
    let mut starts = 0;
    let mut changed = Vec::new();
    let mut asks = 0;
    let mut answered = Vec::new();
    let mut stale = 0;
    let mut replayed = Vec::with_capacity(records.len());
    for record in records {
        let Some((kept, at)) = answer(record) else {
            replayed.push(record.clone());
            continue;
        };
        let decide = |weights| {
            placement::decide_as(
                &record.work,
                &record.machines,
                &record.loads,
                &record.refused,
                at,
                weights,
            )
        };
        if !draws(&decide(&record.decision.weights), kept) {
            stale += 1;
            replayed.push(record.clone());
            continue;
        }
        let decision = decide(weights);
        let drawn = draws(&decision, kept);
        let change = Change {
            at: record.at.clone(),
            kept: kept.to_owned(),
            replayed: placed_on(&decision.placement).map(str::to_owned),
        };
        if !record.start {
            asks += 1;
            if !drawn {
                answered.push(change);
            }
            replayed.push(record.clone());
            continue;
        }
        starts += 1;
        if drawn {
            replayed.push(record.clone());
            continue;
        }
        changed.push(change);
        replayed.push(Record {
            decision,
            ..record.clone()
        });
    }
    Replay {
        measures: MEASURES.to_owned(),
        weights: *weights,
        starts,
        changed,
        asks,
        answered,
        stale,
        kept: hindsight::judge(records, readings),
        replayed: hindsight::judge(&replayed, readings),
    }
}

/// Where `record` was answered and when, if its answer named a machine at
/// a time that reads, whether it started work there or only asked.
fn answer(record: &Record) -> Option<(&str, jiff::Timestamp)> {
    Some((
        placed_on(&record.decision.placement)?,
        record.at.parse().ok()?,
    ))
}

/// Whether `decision` might have drawn `machine`.
fn draws(decision: &placement::Decision, machine: &str) -> bool {
    decision
        .judged
        .iter()
        .any(|judged| judged.machine == machine && judged.chance > 0.0)
}

/// The machine `placement` put work on, if it did.
fn placed_on(placement: &Placement) -> Option<&str> {
    match placement {
        Placement::Placed { machine, .. } => Some(machine),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::balance::Load as Carried;
    use crate::placement::tests::{machine, needing, pressed};
    use crate::placement::{Load, Loads, decide_as};

    /// When the replayed starts were placed, half a minute after their
    /// machines said their room.
    const AT: &str = "2026-10-29T06:00:30Z";

    /// A start kept at [`AT`], placed on `chosen` by the starting weights
    /// among `machines` carrying `loads`.
    fn kept(chosen: &str, machines: Vec<crate::facts::Machine>, loads: Loads) -> Record {
        let work = needing(Vec::new());
        let at = AT.parse().expect("a time");
        let mut decision = decide_as(&work, &machines, &loads, &[], at, &Weights::STARTING);
        let passed = match decision.placement {
            Placement::Placed { passed, .. } => passed,
            other => panic!("the starting weights placed nothing: {other:?}"),
        };
        decision.placement = Placement::Placed {
            machine: chosen.to_owned(),
            passed,
        };
        Record {
            at: AT.to_owned(),
            start: true,
            work,
            machines,
            loads,
            refused: Vec::new(),
            fence: None,
            screen: None,
            decision,
        }
    }

    /// A balance reading a minute after [`AT`] of `loads` by machine, each
    /// with room.
    fn a_minute_on(loads: &[(&str, f64)]) -> Reading {
        Reading {
            measures: crate::balance::MEASURES.to_owned(),
            at: "2026-10-29T06:01:40Z".to_owned(),
            machines: loads
                .iter()
                .map(|&(machine, load)| Carried {
                    machine: machine.to_owned(),
                    ready: true,
                    memory: Some(load),
                    stall: Some(0.0),
                    processor: Some(0.0),
                    load: Some(load),
                    over: false,
                    past: Vec::new(),
                    room: true,
                    busy: 0,
                    idle: 0,
                    busy_per_core: None,
                    age: Some(0),
                })
                .collect(),
            imbalance: None,
            flags: Vec::new(),
            held: None,
        }
    }

    /// Kestrel's processor is 0.6 free and it carries nothing; skiff's is
    /// all free and it carries four starts on its eight cores, so the
    /// starting weights choose skiff and doubled session weight kestrel.
    fn crowded_skiff() -> (Vec<crate::facts::Machine>, Loads) {
        let machines = vec![
            pressed(machine("kestrel", "linux", 32, &[]), 40.0),
            machine("skiff", "linux", 32, &[]),
        ];
        let loads = Loads::from([(
            "skiff".to_owned(),
            Load {
                sessions: 0,
                placed: 4,
            },
        )]);
        (machines, loads)
    }

    #[test]
    fn a_record_kept_before_running_was_said_replays_as_it_was_weighed() {
        // A cool machine with many sessions in a turn, against a pressed
        // one with few: weighed as starts, they sent the ask to osprey.
        let mut cool = pressed(machine("kestrel", "linux", 32, &[]), 10.0);
        cool.cpu.cores = Some(16);
        let machines = vec![cool, pressed(machine("osprey", "linux", 32, &[]), 70.0)];
        let load = |sessions| Load {
            sessions,
            placed: 0,
        };
        let loads = Loads::from([
            ("kestrel".to_owned(), load(48)),
            ("osprey".to_owned(), load(10)),
        ]);
        let work = needing(Vec::new());
        let then = Weights {
            running: None,
            ..Weights::STARTING
        };
        let decision = decide_as(
            &work,
            &machines,
            &loads,
            &[],
            AT.parse().expect("a time"),
            &then,
        );
        let record = Record {
            at: AT.to_owned(),
            start: false,
            work,
            machines,
            loads,
            refused: Vec::new(),
            // A fenced asker's, so a rewrite dropping either says so.
            fence: Some(crate::declared::Kind::Strict),
            screen: Some(crate::placements::Screen::Held),
            decision,
        };
        let said = serde_json::to_value(&record).expect("a record");
        assert_eq!(said["decision"]["weights"].get("running"), None, "{said}");
        let kept: Record = serde_json::from_value(said).expect("a record");
        let replay = replay(&[kept], &[], &Weights::STARTING);
        assert_eq!(replay.stale, 0, "its own weights still draw what it kept");
        assert_eq!(
            replay.answered,
            vec![Change {
                at: AT.to_owned(),
                kept: "osprey".to_owned(),
                replayed: Some("kestrel".to_owned()),
            }],
            "and the running weight answers it with the cool machine"
        );
    }

    #[test]
    fn replaying_the_weights_that_placed_work_changes_nothing() {
        // Both idle, so either may be drawn; the kept draw was skiff,
        // where a replay drawing the best would take kestrel by name.
        let realm = vec![
            machine("kestrel", "linux", 32, &[]),
            machine("skiff", "linux", 32, &[]),
        ];
        let records = [kept("skiff", realm, Loads::new())];
        let readings = [a_minute_on(&[("kestrel", 0.1), ("skiff", 0.3)])];
        let replay = replay(&records, &readings, &Weights::STARTING);
        assert_eq!(replay.starts, 1);
        assert!(replay.changed.is_empty(), "{:?}", replay.changed);
        assert_eq!(replay.replayed, replay.kept);
        assert_eq!(replay.kept.regrets.len(), 1, "skiff carried more");
    }

    #[test]
    fn weights_that_would_place_a_start_elsewhere_are_judged_where_they_would() {
        let (machines, loads) = crowded_skiff();
        let records = [kept("skiff", machines, loads)];
        let readings = [a_minute_on(&[("kestrel", 0.3), ("skiff", 0.6)])];
        let heavier = Weights {
            session: 1.0,
            ..Weights::STARTING
        };
        let replay = replay(&records, &readings, &heavier);
        assert_eq!(replay.weights, heavier);
        assert_eq!(
            replay.changed,
            [Change {
                at: AT.to_owned(),
                kept: "skiff".to_owned(),
                replayed: Some("kestrel".to_owned()),
            }]
        );
        assert_eq!(replay.kept.regrets.len(), 1, "skiff carried more");
        assert!(
            replay.replayed.regrets.is_empty(),
            "kestrel carried least: {:?}",
            replay.replayed.regrets
        );
    }

    #[test]
    fn an_ask_of_where_is_answered_again_but_never_judged() {
        let (machines, loads) = crowded_skiff();
        let mut asked = kept("skiff", machines, loads);
        asked.start = false;
        let readings = [a_minute_on(&[("kestrel", 0.3), ("skiff", 0.6)])];
        let heavier = Weights {
            session: 1.0,
            ..Weights::STARTING
        };
        let replay = replay(&[asked], &readings, &heavier);
        assert_eq!(replay.starts, 0);
        assert_eq!(replay.changed, []);
        assert_eq!(replay.asks, 1);
        assert_eq!(
            replay.answered,
            [Change {
                at: AT.to_owned(),
                kept: "skiff".to_owned(),
                replayed: Some("kestrel".to_owned()),
            }]
        );
    }

    #[test]
    fn a_start_and_an_ask_are_each_counted_and_only_the_start_judged() {
        let (machines, loads) = crowded_skiff();
        let started = kept("skiff", machines, loads);
        let asked = Record {
            start: false,
            ..started.clone()
        };
        let readings = [a_minute_on(&[("kestrel", 0.3), ("skiff", 0.6)])];
        let heavier = Weights {
            session: 1.0,
            ..Weights::STARTING
        };
        let replay = replay(&[started, asked], &readings, &heavier);
        let elsewhere = [Change {
            at: AT.to_owned(),
            kept: "skiff".to_owned(),
            replayed: Some("kestrel".to_owned()),
        }];
        assert_eq!((replay.starts, replay.asks), (1, 1));
        assert_eq!(
            (&replay.changed[..], &replay.answered[..]),
            (&elsewhere[..], &elsewhere[..])
        );
        assert_eq!(
            (replay.kept.placed, replay.replayed.placed),
            (1, 1),
            "the ask is judged neither as kept nor as replayed"
        );
    }

    #[test]
    fn an_answer_the_rules_since_would_refuse_is_left_out_of_what_the_weights_change() {
        // Placed on a paid machine before a worker refused work that
        // doesn't name it: no weights would draw it there now, which is
        // the rule's doing, not theirs.
        let mut cloud = machine("osprey", "linux", 32, &["internet"]);
        cloud.declared.as_mut().expect("declared").costs_money = true;
        let home = pressed(machine("kestrel", "linux", 32, &["internet"]), 90.0);
        let realm = vec![home, cloud];
        let mut started = kept("kestrel", realm, Loads::new());
        started.decision.placement = Placement::Placed {
            machine: "osprey".to_owned(),
            passed: Vec::new(),
        };
        let asked = Record {
            start: false,
            ..started.clone()
        };
        let readings = [a_minute_on(&[("kestrel", 0.9), ("osprey", 0.1)])];
        for weights in [
            Weights::STARTING,
            Weights {
                cpu: 3.0,
                ..Weights::STARTING
            },
        ] {
            let replay = replay(&[started.clone(), asked.clone()], &readings, &weights);
            assert_eq!(replay.stale, 2, "under {weights:?}");
            assert_eq!((replay.starts, replay.asks), (0, 0), "under {weights:?}");
            assert_eq!(
                (&replay.changed[..], &replay.answered[..]),
                (&[][..], &[][..]),
                "under {weights:?}"
            );
            assert_eq!(replay.replayed, replay.kept, "under {weights:?}");
        }
    }

    #[test]
    fn an_ask_the_weights_would_answer_alike_is_counted_but_not_listed() {
        let (machines, loads) = crowded_skiff();
        let mut asked = kept("skiff", machines, loads);
        asked.start = false;
        let replay = replay(&[asked], &[], &Weights::STARTING);
        assert_eq!(replay.asks, 1);
        assert_eq!(replay.answered, []);
    }
}
