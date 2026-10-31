//! Placement: which machine of the realm work should go to, judged from
//! what each machine last said it is. Work names what it needs, what it
//! would prefer, and the taints it tolerates; the answer is the machine
//! it goes to, that it waits, or that it can't go anywhere, with each
//! machine's reasons.
//!
//! A machine's taints are those its configuration declares and those its
//! conditions set while they last: running on its battery, or being past
//! its queue's memory limit. A taint that refuses never keeps work off a
//! machine; one that would rather not only ranks the machine after every
//! other that qualifies.
//!
//! Placement only chooses. Whether work starts is the machine's to say
//! when it is asked, by its own queue and its own taints, so nothing here
//! fits work against the room a machine has: its room only ranks it.

use std::cmp::Reverse;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::declared::{Effect, Taint};
use crate::disk::Band;
use crate::facts::{AcceleratorKind, Machine, Readiness};
use crate::room::Limit;

/// Work to be placed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Work {
    /// What a machine must be to take it.
    #[serde(default)]
    pub needs: Vec<Need>,
    /// What it would rather a machine were, most wanted first.
    #[serde(default)]
    pub prefers: Vec<Need>,
    /// The keys of the taints it doesn't mind.
    #[serde(default)]
    pub tolerates: Vec<String>,
    /// Whether it may wait for a machine out of reach to come back.
    #[serde(default)]
    pub may_wait: bool,
    /// How long it runs, where it says: a machine that leaves before then
    /// is passed over, as one that has left always is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lasts: Option<jiff::SignedDuration>,
}

/// Something a machine is, which work can need or prefer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Need {
    /// This machine, by name.
    Machine(String),
    /// Its kind of system, such as `linux` or `macos`.
    System(String),
    /// Its architecture, such as `x86_64` or `aarch64`.
    Arch(String),
    /// A network it reaches, as its configuration declares.
    Reach(String),
    /// A kind of accelerator it has.
    Accelerator(AcceleratorKind),
}

/// Where work goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// To `machine`; every other machine is in `passed`, with why.
    Placed {
        /// The machine chosen.
        machine: String,
        /// Each machine passed over, and why.
        passed: Vec<Miss>,
    },
    /// Nowhere now, but a machine out of reach would do once it is back.
    Pending {
        /// Each machine, and why not now.
        misses: Vec<Miss>,
    },
    /// Nowhere the realm has.
    Refused {
        /// Each machine, and why not.
        misses: Vec<Miss>,
    },
}

/// Why work didn't go to a machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Miss {
    /// The machine.
    pub machine: String,
    /// Each reason.
    pub because: Vec<Because>,
}

/// A reason work didn't go to a machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Because {
    /// It isn't what the work needs.
    Lacks(Need),
    /// Whether it is what the work needs isn't known.
    Unknown(Need),
    /// A taint it carries refuses the work.
    Refuses(Taint),
    /// A taint would rather not, and another machine qualified.
    RatherNot(Taint),
    /// It was a lesser choice than the one chosen.
    Ranked,
    /// No link reaches it now.
    Unreachable,
    /// It leaves at this time, before the work would end.
    LeavesFirst(jiff::Timestamp),
}

impl fmt::Display for Need {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Need::Machine(name) => write!(f, "machine {name}"),
            Need::System(system) => write!(f, "system {system}"),
            Need::Arch(arch) => write!(f, "arch {arch}"),
            Need::Reach(network) => write!(f, "reach {network}"),
            Need::Accelerator(AcceleratorKind::Gpu) => write!(f, "a gpu"),
            Need::Accelerator(AcceleratorKind::Npu) => write!(f, "an npu"),
        }
    }
}

impl fmt::Display for Because {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Because::Lacks(Need::Machine(name)) => write!(f, "isn't {name}"),
            Because::Lacks(need) => write!(f, "lacks {need}"),
            Because::Unknown(need) => write!(f, "hasn't said {need}"),
            Because::Refuses(taint) => write!(f, "refuses: {}", taint.why),
            Because::RatherNot(taint) => write!(f, "would rather not: {}", taint.why),
            Because::Ranked => write!(f, "ranked lower"),
            Because::Unreachable => write!(f, "is out of reach"),
            Because::LeavesFirst(at) => write!(f, "leaves at {at}, before the work ends"),
        }
    }
}

/// Each machine's reasons, in words a person reads: `optimus lacks system
/// macos; penelope is out of reach`.
#[must_use]
pub fn said(misses: &[Miss]) -> String {
    if misses.is_empty() {
        return "the realm knows no machine".to_owned();
    }
    misses
        .iter()
        .map(|miss| {
            let reasons: Vec<String> = miss.because.iter().map(ToString::to_string).collect();
            format!("{} {}", miss.machine, reasons.join(", "))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The key of the taint a machine on its battery carries.
pub const ON_BATTERY: &str = "on-battery";

/// The key of the taint a machine past its queue's memory limit carries.
pub const MEMORY_PRESSURE: &str = "memory-pressure";

/// The key of the taint a machine whose home's disk is filling carries.
pub const DISK_PRESSURE: &str = "disk-pressure";

/// Places `work` among `machines`.
#[must_use]
pub fn place(work: &Work, machines: &[Machine]) -> Placement {
    place_at(work, machines, jiff::Timestamp::now())
}

/// Places `work` among `machines` as of `now`.
fn place_at(work: &Work, machines: &[Machine], now: jiff::Timestamp) -> Placement {
    let mut judged: Vec<Judged> = machines.iter().map(|one| judge(work, one, now)).collect();
    // Machines that qualify, those no taint would rather not have first,
    // then by how many preferences each meets, earliest most, then by the
    // memory it has free; a name settles the rest, so one ask answers the
    // same as the next.
    let chosen = judged
        .iter()
        .enumerate()
        .filter(|(_, one)| one.hard.is_empty())
        .min_by_key(|(_, one)| {
            (
                !one.rather_not.is_empty(),
                Reverse(one.preferred.clone()),
                Reverse(one.free),
                one.name.clone(),
            )
        })
        .map(|(at, _)| at);
    if let Some(at) = chosen {
        let machine = judged.remove(at).name;
        let passed = judged
            .into_iter()
            .map(|one| {
                let mut because = one.hard;
                because.extend(one.rather_not.into_iter().map(Because::RatherNot));
                if because.is_empty() {
                    because.push(Because::Ranked);
                }
                Miss {
                    machine: one.name,
                    because,
                }
            })
            .collect();
        return Placement::Placed { machine, passed };
    }
    let waits = work.may_wait
        && judged.iter().any(|one| {
            one.hard
                .iter()
                .all(|reason| matches!(reason, Because::Unreachable))
        });
    let misses = judged
        .into_iter()
        .map(|one| Miss {
            machine: one.name,
            because: one.hard,
        })
        .collect();
    if waits {
        Placement::Pending { misses }
    } else {
        Placement::Refused { misses }
    }
}

/// The taints a machine carries now: those its configuration declares,
/// and those its conditions set while they last.
#[must_use]
pub fn taints(machine: &Machine) -> Vec<Taint> {
    let mut taints = machine
        .declared
        .as_ref()
        .map(|declared| declared.taints.clone())
        .unwrap_or_default();
    let condition = |key: &str, why: &str| Taint {
        key: key.to_owned(),
        effect: Effect::PreferNot,
        why: why.to_owned(),
    };
    if machine.power.is_some_and(|power| power.on_battery) {
        taints.push(condition(ON_BATTERY, "it runs on its battery"));
    }
    let pressed = machine
        .room
        .as_ref()
        .and_then(|room| room.over.as_ref())
        .is_some_and(|over| over.contains(&Limit::Memory));
    if pressed {
        taints.push(condition(
            MEMORY_PRESSURE,
            "its queue starts no work for want of memory",
        ));
    }
    if machine.disk_band.is_some_and(|band| band > Band::Healthy) {
        taints.push(condition(
            DISK_PRESSURE,
            "its disk guard says its disk is filling",
        ));
    }
    taints
}

/// One machine, as work sees it.
struct Judged {
    name: String,
    /// Reasons it can't take the work at all.
    hard: Vec<Because>,
    /// Taints that would rather not have it.
    rather_not: Vec<Taint>,
    /// Whether it meets each preference, in the work's order.
    preferred: Vec<bool>,
    /// The memory it has free, or none known.
    free: u64,
}

fn judge(work: &Work, machine: &Machine, now: jiff::Timestamp) -> Judged {
    let mut hard = Vec::new();
    if machine.state == Readiness::Unreachable {
        hard.push(Because::Unreachable);
    }
    if let Some(leaves) = machine.declared.as_ref().and_then(|said| said.leaves_at) {
        // Work lasting longer than time can say outlasts every machine
        // that leaves.
        let ends = work
            .lasts
            .map_or(Some(now), |lasts| now.checked_add(lasts).ok());
        if ends.is_none_or(|ends| leaves <= ends) {
            hard.push(Because::LeavesFirst(leaves));
        }
    }
    for need in &work.needs {
        match is(machine, need) {
            Some(true) => {}
            Some(false) => hard.push(Because::Lacks(need.clone())),
            None => hard.push(Because::Unknown(need.clone())),
        }
    }
    let mut rather_not = Vec::new();
    for taint in taints(machine) {
        if work.tolerates.contains(&taint.key) {
            continue;
        }
        match taint.effect {
            Effect::Never => hard.push(Because::Refuses(taint)),
            Effect::PreferNot => rather_not.push(taint),
        }
    }
    Judged {
        name: machine.name.clone(),
        hard,
        rather_not,
        preferred: work
            .prefers
            .iter()
            .map(|need| is(machine, need) == Some(true))
            .collect(),
        free: machine
            .room
            .as_ref()
            .map_or(0, |room| room.memory_available),
    }
}

/// Whether `machine` is what `need` asks, or unknown.
fn is(machine: &Machine, need: &Need) -> Option<bool> {
    match need {
        Need::Machine(name) => Some(&machine.name == name),
        Need::System(system) => machine.system.as_ref().map(|its| its == system),
        Need::Arch(arch) => Some(&machine.arch == arch),
        Need::Reach(network) => reaches(machine, network),
        Need::Accelerator(kind) => machine
            .accelerators
            .as_ref()
            .map(|all| all.iter().any(|one| one.kind == *kind)),
    }
}

/// Whether `machine` reaches `network`: always, as its configuration
/// declares; now, as the probe its configuration names last answered; or
/// unknown, while that probe hasn't been heard or nothing is declared.
fn reaches(machine: &Machine, network: &str) -> Option<bool> {
    let declared = machine.declared.as_ref()?;
    if declared.reach.iter().any(|always| always == network) {
        return Some(true);
    }
    if declared.probes.contains_key(network) {
        return machine.reached.get(network).copied();
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::declared::{Declared, Probe};
    use crate::facts::{self, Accelerator, Power, Sources};
    use crate::room::{Pressure, Room, Takes};

    /// A machine as it might say itself: `system` on `x86_64`, plugged
    /// in, with `free` GiB and reaching `reach`.
    fn machine(name: &str, system: &str, free: u64, reach: &[&str]) -> Machine {
        let nowhere = Sources {
            home: "/".into(),
            queue: None,
            disk_guard: None,
            declared: "/nowhere".into(),
            reached: "/nowhere".into(),
            updated: "/nowhere".into(),
        };
        Machine {
            system: Some(system.to_owned()),
            arch: "x86_64".to_owned(),
            accelerators: Some(Vec::new()),
            power: Some(Power {
                on_battery: false,
                charge: None,
            }),
            room: Some(Room {
                said_at: "2026-10-29T06:00:00Z".to_owned(),
                memory_available: free << 30,
                pressure: Pressure {
                    cpu: 0.0,
                    memory: 0.0,
                    io: 0.0,
                },
                takes: Takes {
                    builds: 1,
                    vm_tests: 1,
                },
                over: Some(Vec::new()),
            }),
            declared: Some(Declared {
                reach: reach.iter().map(|&one| one.to_owned()).collect(),
                probes: BTreeMap::new(),
                outlives: Vec::new(),
                costs_money: false,
                taints: Vec::new(),
                cores: None,
                agents: BTreeMap::new(),
                leaves_at: None,
                within: None,
                hosts: Vec::new(),
            }),
            ..facts::measure(name, &nowhere)
        }
    }

    fn placed(placement: &Placement) -> Option<&str> {
        match placement {
            Placement::Placed { machine, .. } => Some(machine),
            _ => None,
        }
    }

    fn needing(needs: Vec<Need>) -> Work {
        Work {
            needs,
            prefers: Vec::new(),
            tolerates: Vec::new(),
            may_wait: false,
            lasts: None,
        }
    }

    #[test]
    fn work_goes_to_a_machine_that_is_what_it_needs() {
        let realm = [
            machine(
                "optimus",
                "linux",
                8,
                &["home", "internet", "shopify-services"],
            ),
            machine("penelope", "linux", 12, &["home", "internet"]),
            machine("equilibrius", "macos", 30, &["home", "internet"]),
        ];
        let placement = place(
            &needing(vec![Need::Reach("shopify-services".to_owned())]),
            &realm,
        );
        assert_eq!(
            placed(&placement),
            Some("optimus"),
            "only optimus reaches it"
        );
        let Placement::Placed { passed, .. } = placement else {
            unreachable!()
        };
        assert!(
            passed
                .iter()
                .all(|miss| miss.because
                    == [Because::Lacks(Need::Reach("shopify-services".to_owned()))]),
            "each other machine lacks it: {passed:?}"
        );
        assert_eq!(
            placed(&place(
                &needing(vec![Need::System("linux".to_owned())]),
                &realm
            )),
            Some("penelope"),
            "of those that qualify, the one with the most memory free"
        );
        let mut work = needing(vec![Need::System("linux".to_owned())]);
        work.prefers = vec![Need::Reach("shopify-services".to_owned())];
        assert_eq!(
            placed(&place(&work, &realm)),
            Some("optimus"),
            "a preference met outranks memory free"
        );
    }

    #[test]
    fn a_machine_that_would_rather_not_takes_work_only_when_none_else_will() {
        let mut equilibrius = machine("equilibrius", "macos", 30, &["home"]);
        equilibrius.power = Some(Power {
            on_battery: true,
            charge: Some(40),
        });
        let mut optimus = machine("optimus", "linux", 2, &["home"]);
        let realm = [equilibrius.clone(), optimus.clone()];
        assert_eq!(
            placed(&place(&needing(Vec::new()), &realm)),
            Some("optimus"),
            "a machine on its battery goes after one plugged in"
        );
        let mut tolerant = needing(Vec::new());
        tolerant.tolerates = vec![ON_BATTERY.to_owned()];
        assert_eq!(
            placed(&place(&tolerant, &realm)),
            Some("equilibrius"),
            "unless the work doesn't mind"
        );
        assert_eq!(
            placed(&place(
                &needing(vec![Need::System("macos".to_owned())]),
                &realm
            )),
            Some("equilibrius"),
            "and it still takes what only it can"
        );

        optimus.room.as_mut().expect("a room").over = Some(vec![Limit::Memory]);
        equilibrius.power = None;
        assert_eq!(
            placed(&place(
                &needing(Vec::new()),
                &[optimus.clone(), equilibrius]
            )),
            Some("equilibrius"),
            "a machine past its queue's memory limit goes after one that isn't"
        );

        optimus.declared.as_mut().expect("declared").taints = vec![Taint {
            key: "fleets".to_owned(),
            effect: Effect::Never,
            why: "no fleets here".to_owned(),
        }];
        let only = [optimus];
        assert!(
            matches!(
                place(&needing(Vec::new()), &only),
                Placement::Refused { .. }
            ),
            "a taint that refuses keeps work off even the only machine"
        );
        let mut fleet = needing(Vec::new());
        fleet.tolerates = vec!["fleets".to_owned()];
        assert_eq!(
            placed(&place(&fleet, &only)),
            Some("optimus"),
            "unless the work tolerates it"
        );
    }

    #[test]
    fn a_machine_whose_disk_is_filling_goes_after_one_with_room() {
        let mut optimus = machine("optimus", "linux", 30, &[]);
        let mut penelope = machine("penelope", "linux", 2, &[]);
        optimus.disk_band = Some(Band::Pressure);
        penelope.disk_band = Some(Band::Healthy);
        let realm = [optimus.clone(), penelope.clone()];
        assert_eq!(
            placed(&place(&needing(Vec::new()), &realm)),
            Some("penelope"),
            "a disk past healthy ranks its machine last, room or not"
        );
        let mut tolerant = needing(Vec::new());
        tolerant.tolerates = vec![DISK_PRESSURE.to_owned()];
        assert_eq!(
            placed(&place(&tolerant, &realm)),
            Some("optimus"),
            "unless the work doesn't mind"
        );
        optimus.disk_band = None;
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[optimus, penelope])),
            Some("optimus"),
            "a disk no guard bands is no reason to pass a machine by"
        );
    }

    #[test]
    fn work_waits_for_a_machine_out_of_reach_only_when_it_may() {
        let mut penelope = machine("penelope", "linux", 12, &["home"]);
        penelope.state = Readiness::Unreachable;
        let realm = [machine("optimus", "linux", 8, &["home"]), penelope];
        let mut work = needing(vec![Need::Machine("penelope".to_owned())]);
        assert!(
            matches!(place(&work, &realm), Placement::Refused { .. }),
            "work that may not wait is refused"
        );
        work.may_wait = true;
        let Placement::Pending { misses } = place(&work, &realm) else {
            panic!("work that may wait waits for penelope")
        };
        assert_eq!(
            misses,
            [
                Miss {
                    machine: "optimus".to_owned(),
                    because: vec![Because::Lacks(Need::Machine("penelope".to_owned()))],
                },
                Miss {
                    machine: "penelope".to_owned(),
                    because: vec![Because::Unreachable],
                },
            ],
            "each machine says why not now"
        );
    }

    #[test]
    fn work_never_goes_to_a_machine_that_leaves_before_it_ends() {
        let now: jiff::Timestamp = "2026-10-30T01:20:00Z".parse().expect("a time");
        let mut sandbox = machine("sandbox", "linux", 30, &[]);
        let leaves = now + jiff::SignedDuration::from_mins(40);
        sandbox.declared.as_mut().expect("declared").leaves_at = Some(leaves);
        let realm = [machine("optimus", "linux", 8, &[]), sandbox];
        let mut work = needing(Vec::new());
        assert_eq!(
            placed(&place_at(&work, &realm, now)),
            Some("sandbox"),
            "work that says nothing of how long it runs may go where there is room"
        );

        work.lasts = Some(jiff::SignedDuration::from_hours(1));
        let Placement::Placed { machine, passed } = place_at(&work, &realm, now) else {
            panic!("work that outlasts the sandbox goes elsewhere")
        };
        assert_eq!(machine, "optimus", "to the machine that stays");
        assert_eq!(
            passed,
            [Miss {
                machine: "sandbox".to_owned(),
                because: vec![Because::LeavesFirst(leaves)],
            }],
            "saying when the sandbox leaves"
        );

        work.lasts = None;
        work.needs = vec![Need::Machine("sandbox".to_owned())];
        work.may_wait = true;
        assert!(
            matches!(place_at(&work, &realm, leaves), Placement::Refused { .. }),
            "once it has left, nothing goes there, and nothing waits for it"
        );
    }

    #[test]
    fn what_a_machine_never_said_meets_no_need() {
        let mut quiet = machine("methuselah", "macos", 16, &[]);
        quiet.declared = None;
        quiet.accelerators = None;
        let Placement::Refused { misses } = place(
            &needing(vec![
                Need::Reach("home".to_owned()),
                Need::Accelerator(AcceleratorKind::Npu),
            ]),
            &[quiet],
        ) else {
            panic!("nothing known meets a need")
        };
        assert_eq!(
            misses[0].because,
            [
                Because::Unknown(Need::Reach("home".to_owned())),
                Because::Unknown(Need::Accelerator(AcceleratorKind::Npu)),
            ],
            "unknown, not lacking"
        );

        let mut npu = machine("optimus", "linux", 8, &[]);
        npu.accelerators = Some(vec![Accelerator {
            kind: AcceleratorKind::Npu,
            driver: "intel_vpu".to_owned(),
        }]);
        assert_eq!(
            placed(&place(
                &needing(vec![Need::Accelerator(AcceleratorKind::Npu)]),
                &[npu]
            )),
            Some("optimus"),
            "an accelerator it lists meets the need"
        );
    }

    #[test]
    fn a_network_reached_now_and_then_is_met_while_its_probe_answers() {
        let mut laptop = machine("optimus", "linux", 8, &["internet"]);
        if let Some(declared) = laptop.declared.as_mut() {
            declared.probes =
                BTreeMap::from([("home".to_owned(), Probe::Connect("192.0.2.1:53".to_owned()))]);
        }
        let home = || needing(vec![Need::Reach("home".to_owned())]);
        let because = |placement: Placement| match placement {
            Placement::Refused { misses } => misses[0].because.clone(),
            other => panic!("refused, not {other:?}"),
        };
        assert_eq!(
            because(place(&home(), std::slice::from_ref(&laptop))),
            [Because::Unknown(Need::Reach("home".to_owned()))],
            "a probe not yet heard says nothing"
        );
        laptop.reached.insert("home".to_owned(), false);
        assert_eq!(
            because(place(&home(), std::slice::from_ref(&laptop))),
            [Because::Lacks(Need::Reach("home".to_owned()))],
            "a probe that didn't answer lacks it"
        );
        laptop.reached.insert("home".to_owned(), true);
        assert_eq!(
            placed(&place(&home(), std::slice::from_ref(&laptop))),
            Some("optimus"),
            "a probe that answered meets it"
        );
        assert_eq!(
            placed(&place(
                &needing(vec![Need::Reach("internet".to_owned())]),
                &[laptop]
            )),
            Some("optimus"),
            "a network declared outright needs no probe"
        );
    }
}
