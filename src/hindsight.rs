//! How well placement chose, judged in hindsight from the node's
//! placements log and the balance kept each minute. A placement regrets
//! when, a minute on, the machine it chose carries more load than another
//! with room by more than placement's own dead band; a herd is three
//! placements or more to one machine within a minute while another had
//! more headroom. Both are what a weight set is tuned against, so they
//! are measured the same way on every machine, from what each keeps.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::balance::Reading;
use crate::placement::{Judgement, NEAR_BEST, Placement};
use crate::placements::Record;

/// The name of what these measures mean. A new definition gets a new
/// name, so lines kept under an old one are never read as the new.
pub const MEASURES: &str = "hindsight/1";

/// How long after a placement its machine is judged: long enough for
/// the work to start and its load to show in the balance.
pub const SETTLED_AFTER: jiff::SignedDuration = jiff::SignedDuration::from_secs(60);

/// How long after [`SETTLED_AFTER`] a balance reading still judges a
/// placement; past it, too much else has happened to blame the choice.
pub const JUDGED_WITHIN: jiff::SignedDuration = jiff::SignedDuration::from_secs(120);

/// How close together placements to one machine count as one herd.
pub const HERD_WITHIN: jiff::SignedDuration = jiff::SignedDuration::from_secs(60);

/// How many placements to one machine within [`HERD_WITHIN`] are a herd.
pub const HERD_OF: usize = 3;

/// The share of judged placements regretted past which placement is
/// flagged as choosing badly.
pub const REGRETFUL: f64 = 0.1;

/// The fewest judged placements a share of regrets is flagged on, so one
/// unlucky placement in a quiet hour isn't a pattern.
pub const REGRETFUL_OF: usize = 5;

/// What hindsight says of a stretch of placements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hindsight {
    /// What these measures mean: [`MEASURES`].
    pub measures: String,
    /// How many starts put work somewhere; an ask of where alone isn't
    /// judged.
    pub placed: usize,
    /// How many of those had a balance reading to judge them by.
    pub judged: usize,
    /// Each placement regretted.
    pub regrets: Vec<Regret>,
    /// The share of judged placements regretted, where any were judged.
    pub regret_share: Option<f64>,
    /// Each herd.
    pub herds: Vec<Herd>,
    /// How many cores each busy session used, by machine, for placing's
    /// estimate of what a start will take.
    #[serde(default)]
    pub bursts: Vec<Burst>,
    /// What is wrong.
    pub flags: Vec<Flag>,
}

/// A placement whose machine, a minute on, carried more than another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Regret {
    /// When the work was placed.
    pub at: String,
    /// The machine chosen.
    pub machine: String,
    /// Its load when judged.
    pub load: f64,
    /// The machine with room that carried least when judged.
    pub better: String,
    /// That machine's load.
    pub better_load: f64,
}

/// Placements bunched onto one machine while another had more headroom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Herd {
    /// When the first of them was placed.
    pub from: String,
    /// The machine they went to.
    pub machine: String,
    /// How many there were.
    pub count: usize,
    /// A machine with more headroom beyond the dead band at one of them.
    pub roomier: String,
}

/// How many cores each busy session on a machine used, over the readings
/// where any was busy: a session's processor share is not kept, so this
/// is the machine's processor use over its busy sessions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Burst {
    /// The machine.
    pub machine: String,
    /// How many readings it is over.
    pub readings: usize,
    /// The middle reading, to the hundredth.
    pub median: f64,
    /// The reading nine in ten fall at or below, to the hundredth.
    pub high: f64,
}

/// Something wrong with how placement chose.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "flag")]
pub enum Flag {
    /// More than [`REGRETFUL`] of at least [`REGRETFUL_OF`] judged
    /// placements were regretted.
    Regretful,
    /// Work herded onto `machine`.
    Herded {
        /// The machine.
        machine: String,
    },
}

/// Judges `records` against the balance `readings` kept around them.
#[must_use]
pub fn judge(records: &[Record], readings: &[Reading]) -> Hindsight {
    let placed: Vec<Placed> = records.iter().filter_map(Placed::of).collect();
    let mut readings: Vec<(jiff::Timestamp, &Reading)> = readings
        .iter()
        .filter_map(|reading| Some((reading.at.parse().ok()?, reading)))
        .collect();
    readings.sort_by_key(|&(at, _)| at);
    let mut judged = 0;
    let mut regrets = Vec::new();
    for one in &placed {
        let Some((reading, load)) = judged_by(one, &readings) else {
            continue;
        };
        judged += 1;
        regrets.extend(regret(one, reading, load));
    }
    #[expect(clippy::cast_precision_loss, reason = "an hour's placements are few")]
    let regret_share = (judged > 0).then(|| regrets.len() as f64 / judged as f64);
    let herds = herds(&placed);
    let bursts = bursts(&readings);
    let mut flags: Vec<Flag> = herds
        .iter()
        .map(|herd| Flag::Herded {
            machine: herd.machine.clone(),
        })
        .collect();
    if judged >= REGRETFUL_OF && regret_share.is_some_and(|share| share > REGRETFUL) {
        flags.push(Flag::Regretful);
    }
    flags.sort();
    flags.dedup();
    Hindsight {
        measures: MEASURES.to_owned(),
        placed: placed.len(),
        judged,
        regrets,
        regret_share,
        herds,
        bursts,
        flags,
    }
}

/// Each machine's [`Burst`] over `readings`.
fn bursts(readings: &[(jiff::Timestamp, &Reading)]) -> Vec<Burst> {
    let mut by_machine: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for (_, reading) in readings {
        for load in &reading.machines {
            let (Some(processor), Some(per_core)) = (load.processor, load.busy_per_core) else {
                continue;
            };
            if per_core > 0.0 && processor.is_finite() {
                by_machine
                    .entry(&load.machine)
                    .or_default()
                    .push(processor / per_core);
            }
        }
    }
    by_machine
        .into_iter()
        .map(|(machine, mut cores)| {
            cores.sort_by(f64::total_cmp);
            Burst {
                machine: machine.to_owned(),
                readings: cores.len(),
                median: hundredths(ranked(&cores, MEDIAN)),
                high: hundredths(ranked(&cores, HIGH)),
            }
        })
        .collect()
}

/// The share of readings at or below a burst's middle.
const MEDIAN: f64 = 0.5;

/// The share of readings at or below a burst's high.
const HIGH: f64 = 0.9;

/// The value of `sorted` (never empty) at or below which `share` of them
/// fall, by nearest rank, so it is always one that was read.
fn ranked(sorted: &[f64], share: f64) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "an hour's readings are few, and the rank is a positive count"
    )]
    let rank = (share * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// `value` to the hundredth, so a line changes only when the use does.
fn hundredths(value: f64) -> f64 {
    (value * HUNDREDTHS).round() / HUNDREDTHS
}

/// How many parts of one a burst is said to.
const HUNDREDTHS: f64 = 100.0;

/// A placement that put work somewhere, when it did.
struct Placed<'a> {
    at: jiff::Timestamp,
    machine: &'a str,
    record: &'a Record,
}

impl<'a> Placed<'a> {
    /// `record`, where it started work somewhere and its time reads. An
    /// ask of where alone adds no load, and a spawn asks before it starts,
    /// so only starts are judged.
    fn of(record: &'a Record) -> Option<Self> {
        let Placement::Placed { machine, .. } = &record.decision.placement else {
            return None;
        };
        if !record.start {
            return None;
        }
        Some(Self {
            at: record.at.parse().ok()?,
            machine,
            record,
        })
    }

    /// How `machine` was judged when this work was placed.
    fn judgement(&self, machine: &str) -> Option<&'a Judgement> {
        let record = self.record;
        record
            .decision
            .judged
            .iter()
            .find(|judged| judged.machine == machine)
    }

    /// Whether `other` could have taken this work instead: it qualified,
    /// and met as many of the work's preferences as the machine chosen,
    /// so a choice made for a preference is never a mistake. A machine
    /// not judged, as one that joined after, could have.
    fn could_go_to(&self, other: &Judgement) -> bool {
        let chosen = self
            .judgement(self.machine)
            .map_or(0, |chosen| chosen.preferred);
        other.qualifies && other.preferred >= chosen
    }
}

/// The reading that judges `one`, the first of `readings` (in time order)
/// once it has settled and before it is too late to blame, with the load
/// it says `one`'s machine carried; none where no reading does.
fn judged_by<'r>(
    one: &Placed,
    readings: &[(jiff::Timestamp, &'r Reading)],
) -> Option<(&'r Reading, f64)> {
    let settled = one.at.checked_add(SETTLED_AFTER).ok()?;
    let latest = settled.checked_add(JUDGED_WITHIN).ok()?;
    let &(at, reading) = readings.iter().find(|&&(at, _)| at >= settled)?;
    if at > latest {
        return None;
    }
    let load = reading
        .machines
        .iter()
        .find(|load| load.machine == one.machine)?
        .load?;
    Some((reading, load))
}

/// The regret `reading` says of `one`, whose machine carried `load`: a
/// machine with room it could have gone to carried less beyond the dead
/// band.
fn regret(one: &Placed, reading: &Reading, load: f64) -> Option<Regret> {
    // A machine the work couldn't have gone to is no better choice, nor
    // one kept lively, which placement passes on purpose.
    let could = |machine: &str| {
        one.judgement(machine)
            .is_none_or(|judged| one.could_go_to(judged) && !judged.kept_lively)
    };
    let better = reading
        .machines
        .iter()
        .filter(|other| other.machine != one.machine && other.ready && other.room)
        .filter(|other| could(&other.machine))
        .filter_map(|other| Some((other, other.load?)))
        .min_by(|(_, a), (_, b)| a.total_cmp(b));
    better
        .filter(|&(_, better_load)| load - better_load > NEAR_BEST)
        .map(|(other, better_load)| Regret {
            at: one.record.at.clone(),
            machine: one.machine.to_owned(),
            load,
            better: other.machine.clone(),
            better_load,
        })
}

/// Each herd among `placed`: [`HERD_OF`] or more to one machine within
/// [`HERD_WITHIN`] of the first, while at one of them another machine
/// the work could go to had more headroom beyond the dead band. A herd
/// is said once, from its first placement.
fn herds(placed: &[Placed]) -> Vec<Herd> {
    let mut by_machine: BTreeMap<&str, Vec<&Placed>> = BTreeMap::new();
    for one in placed {
        by_machine.entry(one.machine).or_default().push(one);
    }
    let mut herds = Vec::new();
    for (machine, mut ones) in by_machine {
        ones.sort_by_key(|one| one.at);
        let mut first = 0;
        while first < ones.len() {
            let Ok(until) = ones[first].at.checked_add(HERD_WITHIN) else {
                break;
            };
            let bunch: Vec<&Placed> = ones[first..]
                .iter()
                .take_while(|one| one.at <= until)
                .copied()
                .collect();
            let roomier = (bunch.len() >= HERD_OF)
                .then(|| bunch.iter().find_map(|one| roomier(one)))
                .flatten();
            let Some(roomier) = roomier else {
                first += 1;
                continue;
            };
            herds.push(Herd {
                from: bunch[0].record.at.clone(),
                machine: machine.to_owned(),
                count: bunch.len(),
                roomier: roomier.to_owned(),
            });
            first += bunch.len();
        }
    }
    herds
}

/// A machine the work `one` placed could have gone to with more headroom
/// than the one it did, beyond the dead band, and wasn't kept lively,
/// since placement passes one kept lively on purpose.
fn roomier<'a>(one: &Placed<'a>) -> Option<&'a str> {
    let chosen = one.judgement(one.machine)?.headroom;
    one.record
        .decision
        .judged
        .iter()
        .filter(|other| other.machine != one.machine)
        .filter(|other| {
            one.could_go_to(other)
                && !other.over
                && !other.unheard
                && !other.saturated
                && !other.kept_lively
        })
        .find(|other| other.headroom > chosen + NEAR_BEST)
        .map(|other| other.machine.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::balance::Load;
    use crate::placement::{Decision, Judgement, Loads, Work};

    /// A placement at `at` onto `chosen`, among `headrooms` by machine.
    fn placed(at: &str, chosen: &str, headrooms: &[(&str, f64)]) -> Record {
        Record {
            at: at.to_owned(),
            start: true,
            work: Work {
                needs: Vec::new(),
                prefers: Vec::new(),
                tolerates: Vec::new(),
                may_wait: false,
                lasts: None,
                fence: None,
            },
            machines: Vec::new(),
            loads: Loads::new(),
            refused: Vec::new(),
            decision: Decision {
                placement: Placement::Placed {
                    machine: chosen.to_owned(),
                    passed: Vec::new(),
                },
                judged: headrooms
                    .iter()
                    .map(|&(machine, headroom)| Judgement {
                        machine: machine.to_owned(),
                        qualifies: true,
                        over: false,
                        unheard: false,
                        saturated: false,
                        kept_lively: false,
                        preferred: 0,
                        headroom,
                        chance: 0.0,
                    })
                    .collect(),
                weights: crate::placement::Weights::STARTING,
            },
        }
    }

    /// A balance reading at `at` of `loads` by machine, each with room.
    fn reading(at: &str, loads: &[(&str, f64)]) -> Reading {
        Reading {
            measures: crate::balance::MEASURES.to_owned(),
            at: at.to_owned(),
            machines: loads
                .iter()
                .map(|&(machine, load)| Load {
                    machine: machine.to_owned(),
                    ready: true,
                    memory: Some(load),
                    stall: Some(0.0),
                    processor: Some(0.0),
                    load: Some(load),
                    over: false,
                    past: Vec::new(),
                    lively: None,
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

    #[test]
    fn a_placement_regrets_when_its_machine_carries_more_a_minute_on() {
        let records = [
            placed("2026-11-16T03:00:00Z", "kestrel", &[("kestrel", 2.0)]),
            placed("2026-11-16T03:10:00Z", "skiff", &[("skiff", 2.0)]),
        ];
        let readings = [
            // Too soon to judge the first: its work hasn't started.
            reading("2026-11-16T03:00:30Z", &[("kestrel", 0.9), ("skiff", 0.1)]),
            reading("2026-11-16T03:01:10Z", &[("kestrel", 0.6), ("skiff", 0.3)]),
            // Within the dead band of the best: no regret.
            reading(
                "2026-11-16T03:11:05Z",
                &[("kestrel", 0.32), ("skiff", 0.35)],
            ),
        ];
        let hindsight = judge(&records, &readings);
        assert_eq!(hindsight.placed, 2);
        assert_eq!(hindsight.judged, 2);
        assert_eq!(
            hindsight.regrets,
            [Regret {
                at: "2026-11-16T03:00:00Z".to_owned(),
                machine: "kestrel".to_owned(),
                load: 0.6,
                better: "skiff".to_owned(),
                better_load: 0.3,
            }],
            "judged by the first reading a minute on, not the one before"
        );
        assert_eq!(hindsight.regret_share, Some(0.5));
        assert_eq!(hindsight.flags, [], "two judged are too few to flag");
    }

    #[test]
    fn a_placement_without_a_reading_soon_after_is_not_judged() {
        let records = [placed("2026-11-16T03:00:00Z", "kestrel", &[])];
        let readings = [reading(
            "2026-11-16T03:05:00Z",
            &[("kestrel", 0.9), ("skiff", 0.1)],
        )];
        let hindsight = judge(&records, &readings);
        assert_eq!((hindsight.placed, hindsight.judged), (1, 0));
        assert_eq!(hindsight.regret_share, None);
    }

    #[test]
    fn regrets_past_a_tenth_of_enough_placements_are_flagged() {
        let at = |minute: u32| format!("2026-11-16T03:{minute:02}:00Z");
        let later = |minute: u32| format!("2026-11-16T03:{minute:02}:10Z");
        let mut records = Vec::new();
        let mut readings = Vec::new();
        for minute in (0..10).step_by(2) {
            records.push(placed(&at(minute), "kestrel", &[]));
            let kestrel = if minute == 0 { 0.8 } else { 0.2 };
            readings.push(reading(
                &later(minute + 1),
                &[("kestrel", kestrel), ("skiff", 0.3)],
            ));
        }
        let hindsight = judge(&records, &readings);
        assert_eq!(hindsight.judged, 5);
        assert_eq!(hindsight.regrets.len(), 1);
        assert_eq!(
            hindsight.flags,
            [Flag::Regretful],
            "one in five is past a tenth"
        );
    }

    #[test]
    fn placements_kept_off_a_roomier_machine_saturated_are_no_herd() {
        let records: Vec<Record> = ["03:00:00", "03:00:20", "03:00:50"]
            .iter()
            .map(|at| {
                let mut record = placed(
                    &format!("2026-11-16T{at}Z"),
                    "kestrel",
                    &[("kestrel", 1.0), ("skiff", 2.0)],
                );
                record.decision.judged[1].saturated = true;
                record
            })
            .collect();
        assert_eq!(
            judge(&records, &[]).herds,
            [],
            "a saturated machine's room is no room placement passed over"
        );
    }

    #[test]
    fn placements_kept_off_a_machine_kept_lively_are_no_herd_nor_regret() {
        let records: Vec<Record> = ["03:00:00", "03:00:20", "03:00:50"]
            .iter()
            .map(|at| {
                let mut record = placed(
                    &format!("2026-11-16T{at}Z"),
                    "kestrel",
                    &[("kestrel", 1.0), ("skiff", 2.0)],
                );
                record.decision.judged[1].kept_lively = true;
                record
            })
            .collect();
        let readings = [reading(
            "2026-11-16T03:01:10Z",
            &[("kestrel", 0.9), ("skiff", 0.1)],
        )];
        let hindsight = judge(&records, &readings);
        assert_eq!(
            hindsight.herds,
            [],
            "the room of a machine kept lively is room placement passes on purpose"
        );
        assert_eq!(hindsight.judged, 1, "the first is judged");
        assert_eq!(
            hindsight.regrets,
            [],
            "and its passing the lively one is no regret"
        );
    }

    #[test]
    fn three_placements_to_one_machine_in_a_minute_while_another_had_more_are_a_herd() {
        let records = [
            placed(
                "2026-11-16T03:00:00Z",
                "kestrel",
                &[("kestrel", 2.0), ("skiff", 1.98)],
            ),
            placed(
                "2026-11-16T03:00:20Z",
                "kestrel",
                &[("kestrel", 1.9), ("skiff", 1.98)],
            ),
            placed(
                "2026-11-16T03:00:50Z",
                "kestrel",
                &[("kestrel", 1.8), ("skiff", 1.98)],
            ),
            // Alike, but skiff never had more than the dead band beyond.
            placed(
                "2026-11-16T03:20:00Z",
                "kestrel",
                &[("kestrel", 2.0), ("skiff", 2.03)],
            ),
            placed(
                "2026-11-16T03:20:10Z",
                "kestrel",
                &[("kestrel", 2.0), ("skiff", 2.03)],
            ),
            placed(
                "2026-11-16T03:20:20Z",
                "kestrel",
                &[("kestrel", 2.0), ("skiff", 2.03)],
            ),
        ];
        let hindsight = judge(&records, &[]);
        assert_eq!(
            hindsight.herds,
            [Herd {
                from: "2026-11-16T03:00:00Z".to_owned(),
                machine: "kestrel".to_owned(),
                count: 3,
                roomier: "skiff".to_owned(),
            }],
            "the first three, once; the last three were near-ties"
        );
        assert_eq!(
            hindsight.flags,
            [Flag::Herded {
                machine: "kestrel".to_owned()
            }]
        );
    }

    #[test]
    fn bursts_say_the_cores_each_busy_session_used_in_the_middle_and_high() {
        let readings: Vec<Reading> = (1..=10_u32)
            .map(|minute| {
                let mut one = reading(
                    &format!("2026-11-16T03:{minute:02}:00Z"),
                    &[("kestrel", 0.5), ("skiff", 0.5)],
                );
                // Kestrel's busy sessions take a tenth of a core more each
                // minute: a fifth of its cores busy, over a fiftieth per
                // core busy, is ten cores a session at the top.
                let kestrel = &mut one.machines[0];
                kestrel.processor = Some(0.2);
                kestrel.busy_per_core = Some(0.2 / f64::from(minute));
                // Skiff is busy but has no busy session to share it.
                let skiff = &mut one.machines[1];
                skiff.processor = Some(0.9);
                skiff.busy_per_core = Some(0.0);
                one
            })
            .collect();
        assert_eq!(
            judge(&[], &readings).bursts,
            [Burst {
                machine: "kestrel".to_owned(),
                readings: 10,
                median: 5.0,
                high: 9.0,
            }],
            "the fifth and ninth of ten, and none where nobody was busy"
        );
    }

    #[test]
    fn asking_where_without_starting_is_neither_judged_nor_a_herd() {
        // A spawn asks where, then starts, so an ask alone adds no load and
        // counting both would make a herd of every start.
        let records: Vec<Record> = ["00", "20", "40"]
            .iter()
            .map(|second| {
                let mut asked = placed(
                    &format!("2026-11-16T03:00:{second}Z"),
                    "kestrel",
                    &[("kestrel", 1.0), ("skiff", 9.0)],
                );
                asked.start = false;
                asked
            })
            .collect();
        let readings = [reading(
            "2026-11-16T03:01:10Z",
            &[("kestrel", 0.9), ("skiff", 0.1)],
        )];
        let hindsight = judge(&records, &readings);
        assert_eq!((hindsight.placed, hindsight.judged), (0, 0));
        assert_eq!(hindsight.herds, []);
        assert_eq!(hindsight.regrets, []);
    }

    #[test]
    fn a_machine_meeting_fewer_preferences_is_no_better_a_choice() {
        let records: Vec<Record> = ["00", "20", "40"]
            .iter()
            .map(|second| {
                let mut preferred = placed(
                    &format!("2026-11-16T03:00:{second}Z"),
                    "kestrel",
                    &[("kestrel", 1.0), ("skiff", 9.0)],
                );
                preferred.decision.judged[0].preferred = 1;
                preferred
            })
            .collect();
        let readings = [reading(
            "2026-11-16T03:01:10Z",
            &[("kestrel", 0.9), ("skiff", 0.1)],
        )];
        let hindsight = judge(&records, &readings);
        assert_eq!(hindsight.judged, 1, "the first, a minute before it");
        assert_eq!(
            hindsight.regrets,
            [],
            "placed where it was preferred, on purpose"
        );
        assert_eq!(hindsight.herds, []);
    }

    #[test]
    fn placements_spread_over_more_than_a_minute_are_no_herd() {
        let records = [
            placed("2026-11-16T03:00:00Z", "kestrel", &[("skiff", 9.0)]),
            placed("2026-11-16T03:00:40Z", "kestrel", &[("skiff", 9.0)]),
            placed("2026-11-16T03:01:20Z", "kestrel", &[("skiff", 9.0)]),
        ];
        assert_eq!(judge(&records, &[]).herds, []);
    }
}
