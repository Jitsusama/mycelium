//! Placing kept work again under other weights, to say which set would
//! have placed it better. Each start a node kept is decided again with
//! the candidate weights, as of when it was placed and from the facts and
//! loads it was placed by. Where the candidate would have drawn the kept
//! choice too, the kept choice stands, so replaying the weights that
//! placed it changes nothing; where it couldn't have, the candidate's
//! choice replaces it. Both are then judged in hindsight by the same
//! balance readings.
//!
//! An ask of where, which started nothing, is answered again the same
//! way and counted, with each answer that would have differed, but never
//! judged: it added no load, so hindsight has nothing of it to weigh.
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
pub const MEASURES: &str = "replay/1";

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
    /// How many kept asks of where, which started nothing, were answered
    /// again.
    #[serde(default)]
    pub asks: usize,
    /// Each ask the weights would have answered elsewhere. An ask added
    /// no load, so none is judged in hindsight: this says only how often
    /// the weights would have told an asker something else.
    #[serde(default)]
    pub answered: Vec<Change>,
    /// Hindsight on the starts as they were placed.
    pub kept: Hindsight,
    /// Hindsight on the starts as the weights would have placed them.
    pub replayed: Hindsight,
}

/// A start the weights replayed would have placed elsewhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// When it was placed.
    pub at: String,
    /// Where it went.
    pub kept: String,
    /// Where the weights would have put it; none where they would have
    /// put it nowhere.
    pub replayed: Option<String>,
}

/// Places each start in `records` again under `weights`, judging both
/// placings by the balance `readings` kept around them.
#[must_use]
pub fn replay(records: &[Record], readings: &[Reading], weights: &Weights) -> Replay {
    let mut starts = 0;
    let mut changed = Vec::new();
    let mut asks = 0;
    let mut answered = Vec::new();
    let mut replayed = Vec::with_capacity(records.len());
    for record in records {
        let Some((kept, at)) = answer(record) else {
            replayed.push(record.clone());
            continue;
        };
        let decision =
            placement::decide_as(&record.work, &record.machines, &record.loads, at, weights);
        let drawn = decision
            .judged
            .iter()
            .any(|judged| judged.machine == kept && judged.chance > 0.0);
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
        let mut decision = decide_as(&work, &machines, &loads, at, &Weights::STARTING);
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
    /// all free and it carries four sessions on its eight cores, so the
    /// starting weights choose skiff and doubled session weight kestrel.
    fn crowded_skiff() -> (Vec<crate::facts::Machine>, Loads) {
        let machines = vec![
            pressed(machine("kestrel", "linux", 32, &[]), 40.0),
            machine("skiff", "linux", 32, &[]),
        ];
        let loads = Loads::from([(
            "skiff".to_owned(),
            Load {
                sessions: 4,
                placed: 0,
            },
        )]);
        (machines, loads)
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
        assert_eq!(replay.replayed, replay.kept, "an ask added no load");
    }

    #[test]
    fn an_ask_the_weights_would_answer_alike_is_counted_alone() {
        let (machines, loads) = crowded_skiff();
        let mut asked = kept("skiff", machines, loads);
        asked.start = false;
        let replay = replay(&[asked], &[], &Weights::STARTING);
        assert_eq!(replay.asks, 1);
        assert_eq!(replay.answered, []);
    }
}
