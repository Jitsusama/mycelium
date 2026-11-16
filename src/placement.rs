//! Placement: which machine of the realm work should go to, judged from
//! what each machine last said it is. Work names what it needs, what it
//! would prefer, and the taints it tolerates; the answer is the machine
//! it goes to, that it waits, or that it can't go anywhere, with each
//! machine's reasons.
//!
//! A machine's taints are those its configuration declares and those its
//! conditions set while they last: running on its battery, running on it
//! below the mark at which it is let sleep, which refuses all new work,
//! being past its queue's memory or processor limit, or its disk filling.
//! A taint that refuses keeps work off a machine. A machine past a limit of
//! its queue's, or whose room hasn't been said lately, goes after every
//! other that qualifies; the rest are ranked by the preferences each meets,
//! then by their [`headroom`], where each taint that would rather not is
//! only a weight, and one of those near the best is chosen at random. A
//! start this node placed counts against its machine
//! until the machine's room shows it ([`Starts`]), so a burst spreads.
//!
//! Where no machine of the realm takes work, an offer may
//! ([`to_make`]): each is judged by the facts its provider promises, the
//! same way, and what isn't promised meets no need.
//!
//! Placement only chooses. Whether work starts is the machine's to say
//! when it is asked, by its own queue and its own taints, so nothing here
//! fits work against the room a machine has: its room only ranks it.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::hash::{BuildHasher, RandomState};

use serde::{Deserialize, Serialize};

use crate::awake;
use crate::declared::Declared;
use crate::declared::{Effect, Taint};
use crate::disk::Band;
use crate::facts::{AcceleratorKind, Machine, Readiness};
use crate::offer::Offer;
use crate::room::Limit;

/// The key a worker's refusal carries: a machine lent or paid for takes
/// only work that names it.
pub const WORKER: &str = "worker";

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
    /// One made would live only this long, ending before the work would.
    LivesOnly(jiff::SignedDuration),
    /// Its room hasn't been said lately, so it earns no headroom.
    Unheard,
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
            Because::LivesOnly(lives) => {
                write!(f, "would live only {lives:#}, ending before the work")
            }
            Because::Unheard => write!(f, "hasn't said its room lately"),
        }
    }
}

/// Each machine's reasons, in words a person reads: `skiff lacks system
/// macos; tern is out of reach`.
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

/// The key of the taint a machine on its battery below the mark carries
/// (`awake::LOW_CHARGE`), which takes no new work, as it is let sleep.
pub const LOW_BATTERY: &str = "low-battery";

/// The key of the taint a machine past its queue's memory limit carries.
pub const MEMORY_PRESSURE: &str = "memory-pressure";

/// The key of the taint a machine whose home's disk is filling carries.
pub const DISK_PRESSURE: &str = "disk-pressure";

/// The key of the taint a machine past its queue's processor limit carries.
pub const CPU_PRESSURE: &str = "cpu-pressure";

/// How long a start placed counts against its machine at most, where its
/// room isn't said after it sooner.
pub const STARTS_COUNT_FOR: jiff::SignedDuration = jiff::SignedDuration::from_mins(5);

/// How old a machine's room may be and still earn it headroom: three of
/// the minutes a link carries another machine's facts in, so one gone
/// quiet never wins on what it said long ago.
pub const ROOM_HEARD_WITHIN: jiff::SignedDuration = jiff::SignedDuration::from_mins(3);

/// How far below the best headroom a machine may be and still be chosen at
/// random beside it.
pub const NEAR_BEST: f64 = 0.05;

/// What a processor's whole headroom is worth: all of it free of pressure.
pub const CPU_WEIGHT: f64 = 1.0;

/// What a machine's whole memory free is worth.
pub const MEMORY_WEIGHT: f64 = 1.0;

/// What a machine's whole disk free is worth.
pub const DISK_WEIGHT: f64 = 0.5;

/// What each session or start a core carries costs.
pub const SESSION_WEIGHT: f64 = 0.5;

/// What each taint that would rather not costs, beside those that say a
/// machine is over, which rank it after every machine that isn't.
pub const RATHER_NOT_WEIGHT: f64 = 1.0;

/// What a disk filling costs, beside one nearly or critically full, which
/// costs as any taint that would rather not: a guard clearing what has
/// waited a while is no reason to keep work off.
pub const FILLING_WEIGHT: f64 = 0.25;

/// What each part of a machine's room is worth to placing, and how near
/// the best a machine may be and still be drawn: the one set of knobs a
/// replay of kept placements varies to tune placing. A set read from JSON
/// takes the starting weight for each it leaves out, so a candidate names
/// only the knob it turns.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Weights {
    /// What a processor's whole headroom is worth.
    pub cpu: f64,
    /// What a machine's whole memory free is worth.
    pub memory: f64,
    /// What a machine's whole disk free is worth.
    pub disk: f64,
    /// What each session or start a core carries costs.
    pub session: f64,
    /// What each taint that would rather not costs.
    pub rather_not: f64,
    /// What a disk filling costs.
    pub filling: f64,
    /// How far below the best headroom a machine may be and still be drawn.
    pub near_best: f64,
}

impl Weights {
    /// The weights placing starts from, chosen by judgement until kept
    /// placements say better.
    pub const STARTING: Self = Self {
        cpu: CPU_WEIGHT,
        memory: MEMORY_WEIGHT,
        disk: DISK_WEIGHT,
        session: SESSION_WEIGHT,
        rather_not: RATHER_NOT_WEIGHT,
        filling: FILLING_WEIGHT,
        near_best: NEAR_BEST,
    };

    /// Reads weights `said` as a JSON object, each left out taking its
    /// starting weight. A field that isn't a weight, a weight that isn't a
    /// number or is below zero, and a weight said twice are each refused by
    /// name, so a misspelt knob is never quietly the starting one and a
    /// negative one never turns its sense around.
    ///
    /// # Errors
    ///
    /// Why `said` isn't a set of weights, in a line for a person.
    pub fn read(said: &str) -> Result<Self, String> {
        let serde_json::Value::Object(fields) =
            serde_json::from_str(said).map_err(|error| error.to_string())?
        else {
            return Err("weights are a JSON object, such as {\"session\": 1.0}".to_owned());
        };
        let serde_json::Value::Object(known) =
            serde_json::to_value(Self::STARTING).map_err(|error| error.to_string())?
        else {
            return Err("the starting weights didn't say themselves as an object".to_owned());
        };
        for (name, weight) in &fields {
            // Quoted, so a name can't write to a terminal it's said on.
            if !known.contains_key(name) {
                let names: Vec<&str> = known.keys().map(String::as_str).collect();
                return Err(format!(
                    "{name:?} isn't a weight; the weights are {}",
                    names.join(", ")
                ));
            }
            // JSON holds no infinity or NaN, so a number is finite here.
            let Some(number) = weight.as_f64() else {
                return Err(format!("{name} is {weight}, not a number"));
            };
            if number < 0.0 {
                return Err(format!("{name} is {number}, below zero"));
            }
        }
        // Read again from the text, which, unlike the object, still holds a
        // weight said twice for serde to refuse by name.
        serde_json::from_str(said).map_err(|error| error.to_string())
    }
}

impl Default for Weights {
    fn default() -> Self {
        Self::STARTING
    }
}

/// What `taint`, which would rather not have the work, costs `machine`.
fn costs(machine: &Machine, taint: &Taint, weights: &Weights) -> f64 {
    if taint.key == DISK_PRESSURE && machine.disk_band == Some(Band::Filling) {
        weights.filling
    } else {
        weights.rather_not
    }
}

/// How much room `machine` has for more work, carrying `load` and taints
/// that would rather not have the work costing `rather_not` between them,
/// where its room was `heard` lately: one sum of what is free of its
/// processor, memory and disk, less what its sessions and starts take of
/// its cores and what its taints cost, each by its weight. Its processor
/// is the more of how busy its cores are and how much work waits on them,
/// since the first tells apart machines that wait on none and the second
/// still counts where its cores are shared out short. A share never said
/// counts as none. This is the one place placement weighs a
/// machine's room, to be tuned.
#[must_use]
pub fn headroom(
    machine: &Machine,
    load: Load,
    rather_not: f64,
    heard: bool,
    weights: &Weights,
) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a share needs no more precision than a double gives bytes"
    )]
    fn share(part: Option<u64>, whole: Option<u64>) -> f64 {
        match (part, whole) {
            (Some(part), Some(whole)) if whole > 0 => (part as f64 / whole as f64).clamp(0.0, 1.0),
            _ => 0.0,
        }
    }
    let room = machine.room.as_ref().filter(|_| heard);
    let cpu = room.map_or(0.0, |room| {
        let pressed = room.pressure.cpu / 100.0;
        let processor = machine.cpu.busy.map_or(pressed, |busy| busy.max(pressed));
        // A reading that isn't a number says nothing, so it counts as
        // none: a NaN would rank by nothing and leave no machine near
        // the best.
        if processor.is_nan() {
            0.0
        } else {
            (1.0 - processor).clamp(0.0, 1.0)
        }
    });
    let memory = share(room.map(|room| room.memory_available), machine.memory);
    let disk = share(machine.disk_free, machine.disk);
    let cores = machine
        .declared
        .as_ref()
        .and_then(|declared| declared.cores)
        .or(machine.cpu.cores)
        .unwrap_or(1)
        .max(1);
    let carried = f64::from(load.sessions.saturating_add(load.placed)) / f64::from(cores);
    weights.cpu * cpu + weights.memory * memory + weights.disk * disk
        - weights.session * carried
        - rather_not
}

/// Whether `machine`'s room was said within [`ROOM_HEARD_WITHIN`] of `now`.
fn heard(machine: &Machine, now: jiff::Timestamp) -> bool {
    said_at(machine).is_some_and(|said| now.duration_since(said) <= ROOM_HEARD_WITHIN)
}

/// When `machine` last said its room, where it has and the time reads.
fn said_at(machine: &Machine) -> Option<jiff::Timestamp> {
    machine.room.as_ref()?.said_at.parse().ok()
}

/// What a machine carries beyond what its room says.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Load {
    /// The sessions running on it.
    pub sessions: u32,
    /// The starts placed on it that its room doesn't yet show.
    pub placed: u32,
}

/// Each machine's load, by name; a machine not in it carries none.
pub type Loads = BTreeMap<String, Load>;

/// The starts a node placed lately, kept in memory only: each is load on
/// its machine until that machine says its room after it, as its room then
/// shows it, or for [`STARTS_COUNT_FOR`] at most, and is then forgotten.
#[derive(Debug, Default)]
pub struct Starts {
    placed: Vec<(String, jiff::Timestamp)>,
}

impl Starts {
    /// Keeps that a start was placed on `machine` at `at`.
    pub fn placed(&mut self, machine: &str, at: jiff::Timestamp) {
        self.placed.push((machine.to_owned(), at));
    }

    /// Each of `machines`' load as of `now`, with `sessions` naming the
    /// machine each session running runs on; forgets each start its
    /// machine's room now shows, or that is past its time.
    pub fn loads<'a>(
        &mut self,
        machines: &[Machine],
        sessions: impl IntoIterator<Item = &'a str>,
        now: jiff::Timestamp,
    ) -> Loads {
        let said: BTreeMap<&str, Option<jiff::Timestamp>> = machines
            .iter()
            .map(|one| (one.name.as_str(), said_at(one)))
            .collect();
        self.placed.retain(|(machine, at)| {
            let shown = said
                .get(machine.as_str())
                .copied()
                .flatten()
                .is_some_and(|said| said > *at);
            !shown && now.duration_since(*at) < STARTS_COUNT_FOR
        });
        let mut loads: Loads = machines
            .iter()
            .map(|one| (one.name.clone(), Load::default()))
            .collect();
        for machine in sessions {
            let load = loads.entry(machine.to_owned()).or_default();
            load.sessions = load.sessions.saturating_add(1);
        }
        for (machine, _) in &self.placed {
            let load = loads.entry(machine.clone()).or_default();
            load.placed = load.placed.saturating_add(1);
        }
        loads
    }
}

/// Where work went, and how each machine was judged for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    /// Where it went.
    pub placement: Placement,
    /// Each machine, in the order given.
    pub judged: Vec<Judgement>,
    /// What weighed them; the starting weights where a kept decision
    /// predates saying so.
    #[serde(default)]
    pub weights: Weights,
}

/// How one machine was judged for work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Judgement {
    /// The machine.
    pub machine: String,
    /// Whether it could take the work at all.
    pub qualifies: bool,
    /// Whether it was past a limit of its queue's the work minds.
    pub over: bool,
    /// Whether its room hadn't been said lately.
    pub unheard: bool,
    /// How many of the work's preferences it met.
    pub preferred: usize,
    /// The room it had for more work, as [`headroom`] weighs it.
    pub headroom: f64,
    /// The chance it was the one chosen: one in as many as were near the
    /// best, and none for the rest.
    pub chance: f64,
}

/// Places `work` among `machines`, each carrying its `loads`.
#[must_use]
pub fn place(work: &Work, machines: &[Machine], loads: &Loads) -> Placement {
    decide(work, machines, loads).placement
}

/// Places `work` among `machines`, each carrying its `loads`, saying how
/// each was judged.
#[must_use]
pub fn decide(work: &Work, machines: &[Machine], loads: &Loads) -> Decision {
    let mut draw = |many: usize| {
        // Each `RandomState` is seeded afresh, so its hash of nothing is a
        // fresh draw.
        let draw = RandomState::new().hash_one(());
        usize::try_from(draw % u64::try_from(many.max(1)).unwrap_or(1)).unwrap_or(0)
    };
    let now = jiff::Timestamp::now();
    decide_at(work, machines, loads, now, &mut draw, &Weights::STARTING)
}

/// Places `work` among `machines` as of `now`, with `pick` choosing which
/// of the machines near the best, given how many, takes it.
#[cfg(test)]
fn place_at(
    work: &Work,
    machines: &[Machine],
    loads: &Loads,
    now: jiff::Timestamp,
    pick: &mut dyn FnMut(usize) -> usize,
) -> Placement {
    decide_at(work, machines, loads, now, pick, &Weights::STARTING).placement
}

/// Decides where `work` would go among `machines` as of `now`, weighed by
/// `weights`, choosing the best of those near it rather than at random, so
/// replaying a kept placement says the same each time.
#[must_use]
pub fn decide_as(
    work: &Work,
    machines: &[Machine],
    loads: &Loads,
    now: jiff::Timestamp,
    weights: &Weights,
) -> Decision {
    decide_at(work, machines, loads, now, &mut |_| 0, weights)
}

/// Decides where `work` goes among `machines` as of `now`, weighed by
/// `weights`, with `pick` choosing which of the machines near the best,
/// given how many, takes it.
fn decide_at(
    work: &Work,
    machines: &[Machine],
    loads: &Loads,
    now: jiff::Timestamp,
    pick: &mut dyn FnMut(usize) -> usize,
    weights: &Weights,
) -> Decision {
    let lasting = lasting_reach(machines);
    let judged: Vec<Judged> = machines
        .iter()
        .map(|one| {
            let load = loads.get(&one.name).copied().unwrap_or_default();
            let mut judged = judge(work, one, load, now, weights);
            if one.declared.as_ref().is_some_and(Declared::is_worker)
                && !names_network(work, &lasting)
                && !work.needs.contains(&Need::Machine(one.name.clone()))
            {
                judged.hard.push(worker_refuses());
            }
            judged
        })
        .collect();
    // Machines that qualify, those not over first, then by how many
    // preferences each meets, earliest most, then by the headroom it has,
    // then by name. Of the best and those as good but for headroom within
    // the band, one at random, so asks alike spread rather than herd.
    let mut ranked: Vec<usize> = (0..judged.len())
        .filter(|&at| judged[at].hard.is_empty())
        .collect();
    ranked.sort_by(|&one, &other| {
        let (one, other) = (&judged[one], &judged[other]);
        (one.over || one.unheard)
            .cmp(&(other.over || other.unheard))
            .then_with(|| other.preferred.cmp(&one.preferred))
            .then_with(|| other.headroom.total_cmp(&one.headroom))
            .then_with(|| one.name.cmp(&other.name))
    });
    let near: Vec<usize> = ranked.first().map_or_else(Vec::new, |&best| {
        let best = &judged[best];
        ranked
            .iter()
            .copied()
            .take_while(|&at| {
                let one = &judged[at];
                (one.over || one.unheard) == (best.over || best.unheard)
                    && one.preferred == best.preferred
                    && one.headroom >= best.headroom - weights.near_best
            })
            .collect()
    });
    #[expect(clippy::cast_precision_loss, reason = "a realm's machines are few")]
    let chance = 1.0 / near.len().max(1) as f64;
    let judgements = judged
        .iter()
        .enumerate()
        .map(|(at, one)| Judgement {
            machine: one.name.clone(),
            qualifies: one.hard.is_empty(),
            over: one.over,
            unheard: one.unheard,
            preferred: one.preferred.iter().filter(|&&met| met).count(),
            headroom: one.headroom,
            chance: if near.contains(&at) { chance } else { 0.0 },
        })
        .collect();
    let chosen = (!near.is_empty()).then(|| near[pick(near.len()).min(near.len() - 1)]);
    Decision {
        placement: placement(work, judged, chosen),
        judged: judgements,
        weights: *weights,
    }
}

/// Where `work` goes, `chosen` from among those `judged`, if any was.
fn placement(work: &Work, mut judged: Vec<Judged>, chosen: Option<usize>) -> Placement {
    if let Some(at) = chosen {
        let machine = judged.remove(at).name;
        let passed = judged
            .into_iter()
            .map(|one| {
                let mut because = one.hard;
                because.extend(one.rather_not.into_iter().map(Because::RatherNot));
                if one.unheard {
                    because.push(Because::Unheard);
                }
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

/// The offer to make a machine of for `work`, where no machine of the
/// realm takes it, judged by the facts each promises: of those that would
/// take it, the one no taint would rather not have, then that meets the
/// most preferences, earliest most, then the cheapest an hour, then by
/// name, so one ask answers the same as the next. Otherwise each offer
/// with why not, named as its shape from its provider.
///
/// # Errors
///
/// When no offer would take the work, saying each one's reasons.
///
/// A machine made is a worker, lent or paid for and let go after its
/// work, so one is made only for work that names it: work needing a
/// network none of the person's own `machines` reach.
pub fn to_make<'a>(
    work: &Work,
    offers: &'a [Offer],
    machines: &[Machine],
) -> Result<&'a Offer, Vec<Miss>> {
    let named = names_network(work, &lasting_reach(machines));
    let judged: Vec<(Judged, &Offer)> = offers
        .iter()
        .map(|offer| {
            let mut judged = judge_offer(work, offer);
            if !named {
                judged.hard.push(worker_refuses());
            }
            (judged, offer)
        })
        .collect();
    let chosen = judged
        .iter()
        .filter(|(one, _)| one.hard.is_empty())
        .min_by_key(|(one, offer)| {
            (
                !one.rather_not.is_empty(),
                Reverse(one.preferred.clone()),
                offer.costs.order(),
                one.name.clone(),
            )
        });
    if let Some((_, offer)) = chosen {
        return Ok(offer);
    }
    Err(judged
        .into_iter()
        .map(|(one, _)| Miss {
            machine: one.name,
            because: one.hard,
        })
        .collect())
}

fn judge_offer(work: &Work, offer: &Offer) -> Judged {
    let mut hard = Vec::new();
    if let Some(lives) = offer
        .lasts
        .and_then(|lasts| i64::try_from(lasts).ok())
        .map(jiff::SignedDuration::from_secs)
        && work.lasts.is_some_and(|lasts| lives <= lasts)
    {
        hard.push(Because::LivesOnly(lives));
    }
    let shape = &offer.facts;
    let promised = |need: &Need| match need {
        // A machine not yet made has no name of its own to need.
        Need::Machine(_) => Some(false),
        Need::System(system) => shape.system.as_ref().map(|its| its == system),
        Need::Arch(arch) => shape.arch.as_ref().map(|its| its == arch),
        Need::Reach(network) => reaches(shape.declared.as_ref(), &BTreeMap::new(), network),
        Need::Accelerator(_) => None,
    };
    for need in &work.needs {
        match promised(need) {
            Some(true) => {}
            Some(false) => hard.push(Because::Lacks(need.clone())),
            None => hard.push(Because::Unknown(need.clone())),
        }
    }
    let mut rather_not = Vec::new();
    let taints = shape.declared.iter().flat_map(|declared| &declared.taints);
    for taint in taints.filter(|taint| !work.tolerates.contains(&taint.key)) {
        match taint.effect {
            Effect::Never => hard.push(Because::Refuses(taint.clone())),
            Effect::PreferNot => rather_not.push(taint.clone()),
        }
    }
    Judged {
        name: format!("{} from {}", offer.shape, offer.provider),
        hard,
        rather_not,
        preferred: work
            .prefers
            .iter()
            .map(|need| promised(need) == Some(true))
            .collect(),
        headroom: 0.0,
        over: false,
        unheard: false,
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
    if machine.power.as_ref().is_some_and(awake::low) {
        taints.push(Taint {
            key: LOW_BATTERY.to_owned(),
            effect: Effect::Never,
            why: format!(
                "its battery is below {}%, so it is let sleep",
                awake::LOW_CHARGE
            ),
        });
    }
    let past = |limit: Limit| {
        machine
            .room
            .as_ref()
            .and_then(|room| room.over.as_ref())
            .is_some_and(|over| over.contains(&limit))
    };
    if past(Limit::Memory) {
        taints.push(condition(
            MEMORY_PRESSURE,
            "its queue starts no work for want of memory",
        ));
    }
    if past(Limit::Cpu) {
        taints.push(condition(
            CPU_PRESSURE,
            "its queue starts no build beside a VM test for want of a processor",
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
    /// The room it has for more work, as [`headroom`] weighs it.
    headroom: f64,
    /// Whether it is past a limit of its queue's the work minds.
    over: bool,
    /// Whether its room hasn't been said lately.
    unheard: bool,
}

/// The networks the person's own lasting machines reach, always or by a
/// probe: what a worker alone reaches is the worker's, so work needing it
/// names the worker, and what one of the person's own reaches names none.
fn lasting_reach(machines: &[Machine]) -> BTreeSet<&str> {
    machines
        .iter()
        .filter_map(|one| one.declared.as_ref())
        .filter(|declared| !declared.is_worker())
        .flat_map(|declared| {
            let always = declared.reach.iter().map(String::as_str);
            always.chain(declared.probes.keys().map(String::as_str))
        })
        .collect()
}

/// Whether `work` needs a network none of the person's `lasting` machines
/// reach, as `aquifer` is, which names the workers that do. Work names a
/// worker otherwise only by needing it by name: tolerating its refusal
/// isn't naming it, nor is needing a system, an architecture or an
/// accelerator, so work that says nothing of where it goes never goes to
/// a machine that costs money, leaves, or holds what only it was lent.
fn names_network(work: &Work, lasting: &BTreeSet<&str>) -> bool {
    work.needs
        .iter()
        .any(|need| matches!(need, Need::Reach(network) if !lasting.contains(network.as_str())))
}

/// A worker's refusal of work that doesn't name it, saying how to.
fn worker_refuses() -> Because {
    Because::Refuses(Taint {
        key: WORKER.to_owned(),
        effect: Effect::Never,
        why: "it is lent or paid for, so it takes only work that names it, by machine or by \
              a network only it reaches"
            .to_owned(),
    })
}

/// `work` asked on `here`, as it is to be placed: where `here` is a worker
/// and the work names no worker of `machines`, it stays on `here`, so an
/// agent on a machine lent for its work runs that work where it was lent,
/// never on the person's own machines, which it was never given.
#[must_use]
pub fn asked_on(work: &Work, here: &Machine, machines: &[Machine]) -> Work {
    let mut work = work.clone();
    let is_worker = |one: &Machine| one.declared.as_ref().is_some_and(Declared::is_worker);
    let names_one = names_network(&work, &lasting_reach(machines))
        || work.needs.iter().any(|need| {
            machines
                .iter()
                .any(|one| is_worker(one) && *need == Need::Machine(one.name.clone()))
        });
    if is_worker(here) && !names_one {
        work.needs.push(Need::Machine(here.name.clone()));
    }
    work
}

fn judge(
    work: &Work,
    machine: &Machine,
    load: Load,
    now: jiff::Timestamp,
    weights: &Weights,
) -> Judged {
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
    let is_over = |taint: &&Taint| [MEMORY_PRESSURE, CPU_PRESSURE].contains(&taint.key.as_str());
    // Over is read from the room even where it is unheard, and harmlessly:
    // a machine unheard ranks with those over either way.
    let over = rather_not.iter().any(|taint| is_over(&taint));
    let weighed = rather_not
        .iter()
        .filter(|taint| !is_over(taint))
        .map(|taint| costs(machine, taint, weights))
        .sum();
    let lately = heard(machine, now);
    Judged {
        name: machine.name.clone(),
        hard,
        rather_not,
        preferred: work
            .prefers
            .iter()
            .map(|need| is(machine, need) == Some(true))
            .collect(),
        headroom: headroom(machine, load, weighed, lately, weights),
        over,
        unheard: !lately,
    }
}

/// Whether `machine` is what `need` asks, or unknown.
fn is(machine: &Machine, need: &Need) -> Option<bool> {
    match need {
        Need::Machine(name) => Some(&machine.name == name),
        Need::System(system) => machine.system.as_ref().map(|its| its == system),
        Need::Arch(arch) => Some(&machine.arch == arch),
        Need::Reach(network) => reaches(machine.declared.as_ref(), &machine.reached, network),
        Need::Accelerator(kind) => machine
            .accelerators
            .as_ref()
            .map(|all| all.iter().any(|one| one.kind == *kind)),
    }
}

/// Whether a machine `declared` so reaches `network`: always, as its
/// configuration declares; now, as the probe its configuration names last
/// answered by `reached`; or unknown, while that probe hasn't been heard or
/// nothing is declared.
fn reaches(
    declared: Option<&Declared>,
    reached: &BTreeMap<String, bool>,
    network: &str,
) -> Option<bool> {
    let declared = declared?;
    if declared.reach.iter().any(|always| always == network) {
        return Some(true);
    }
    if declared.probes.contains_key(network) {
        return reached.get(network).copied();
    }
    Some(false)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::declared::{Declared, Probe};
    use crate::facts::{self, Accelerator, Power, Sources};
    use crate::room::{Pressure, Room, Takes};

    /// Half a minute after each machine here says its room.
    fn now() -> jiff::Timestamp {
        "2026-10-29T06:00:30Z".parse().expect("a time")
    }

    /// Places `work` among `machines` that carry nothing beyond what they
    /// say of themselves, half a minute after they said their room.
    fn place(work: &Work, machines: &[Machine]) -> Placement {
        place_at(work, machines, &Loads::new(), now(), &mut first)
    }

    /// Of the machines near the best, the best.
    fn first(_: usize) -> usize {
        0
    }

    #[test]
    fn a_decision_says_how_each_machine_was_judged_and_its_chance() {
        let realm = [
            machine("kestrel", "linux", 32, &[]),
            machine("skiff", "linux", 32, &[]),
            pressed(machine("tern", "linux", 32, &[]), 90.0),
            machine("osprey", "macos", 32, &[]),
        ];
        let work = needing(vec![Need::System("linux".to_owned())]);
        let decision = decide_at(
            &work,
            &realm,
            &Loads::new(),
            now(),
            &mut first,
            &Weights::STARTING,
        );
        assert_eq!(placed(&decision.placement), Some("kestrel"));
        let judged: Vec<(&str, bool, f64)> = decision
            .judged
            .iter()
            .map(|one| (one.machine.as_str(), one.qualifies, one.chance))
            .collect();
        assert_eq!(
            judged,
            [
                ("kestrel", true, 0.5),
                ("skiff", true, 0.5),
                ("tern", true, 0.0),
                ("osprey", false, 0.0),
            ],
            "the two alike share the draw; one far behind and one not Linux have none"
        );
        let headroom = |name: &str| {
            decision
                .judged
                .iter()
                .find(|one| one.machine == name)
                .map(|one| one.headroom)
        };
        assert!(
            headroom("tern") < headroom("kestrel"),
            "a machine waiting on its processors has less headroom"
        );
    }

    #[test]
    fn a_set_of_weights_decides_as_its_weights_say() {
        // Kestrel's processor is 0.6 free and it carries nothing; skiff's is
        // all free and it carries four sessions on its eight cores.
        let realm = [
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
        let work = needing(Vec::new());
        let starting = decide_as(&work, &realm, &loads, now(), &Weights::STARTING);
        assert_eq!(placed(&starting.placement), Some("skiff"));
        assert_eq!(starting.weights, Weights::STARTING);
        let heavier = Weights {
            session: 1.0,
            ..Weights::STARTING
        };
        let decided = decide_as(&work, &realm, &loads, now(), &heavier);
        assert_eq!(
            placed(&decided.placement),
            Some("kestrel"),
            "sessions costing twice as much outweigh the busier processor"
        );
        assert_eq!(decided.weights, heavier, "a decision says what weighed it");
    }

    #[test]
    fn weights_read_as_json_start_from_the_starting_ones() {
        let read: Weights = serde_json::from_str(r#"{"session": 1.0}"#).expect("weights");
        assert_eq!(
            read,
            Weights {
                session: 1.0,
                ..Weights::STARTING
            }
        );
        let kept = serde_json::json!({"placement": {"refused": {"misses": []}}, "judged": []});
        let decision: Result<Decision, _> = serde_json::from_value(kept);
        assert!(
            decision.is_ok_and(|decision| decision.weights == Weights::STARTING),
            "a decision kept before weights were said was weighed by the starting ones"
        );
    }

    #[test]
    fn weights_read_refuse_what_isnt_a_weight_by_name() {
        assert_eq!(
            Weights::read(r#"{"session": 1.0}"#),
            Ok(Weights {
                session: 1.0,
                ..Weights::STARTING
            })
        );
        let misspelt = Weights::read(r#"{"sesion": 1.0}"#).expect_err("a misspelt weight");
        assert!(
            misspelt.contains("sesion") && misspelt.contains("session"),
            "names what was said and what the weights are: {misspelt}"
        );
        let worded = Weights::read(r#"{"cpu": "high"}"#).expect_err("a word for a weight");
        assert!(worded.contains("cpu"), "names the weight: {worded}");
        let listed = Weights::read("[1.0]").expect_err("a list");
        assert!(listed.contains("object"), "says what weights are: {listed}");
        for (said, why) in [
            (r#"{"cpu": null}"#, "null for a weight"),
            (r#"{"cpu": true}"#, "true for a weight"),
            (r#"{"near_best": -0.05}"#, "a weight below zero"),
            (r#"{"cpu": 1.0, "cpu": 2.0}"#, "a weight said twice"),
        ] {
            let refused = Weights::read(said).expect_err(why);
            let named = if said.contains("near_best") {
                "near_best"
            } else {
                "cpu"
            };
            assert!(
                refused.contains(named),
                "{why} is refused by name: {refused}"
            );
        }
        let escaped = Weights::read("{\"\\u001b[2J\": 1.0}").expect_err("an escape");
        assert!(
            !escaped.contains('\u{1b}'),
            "a name is said quoted: {escaped}"
        );
    }

    #[test]
    fn weights_read_take_nothing_said_and_whole_numbers_as_weights() {
        assert_eq!(Weights::read("{}"), Ok(Weights::STARTING));
        assert_eq!(
            Weights::read(r#"{"cpu": 2, "near_best": 0}"#),
            Ok(Weights {
                cpu: 2.0,
                near_best: 0.0,
                ..Weights::STARTING
            })
        );
    }

    #[test]
    fn a_kept_decision_still_reads_a_weight_it_doesnt_know() {
        let kept = serde_json::json!({
            "placement": {"refused": {"misses": []}},
            "judged": [],
            "weights": {"session": 1.0, "one_from_later": 3.0},
        });
        let decision: Result<Decision, _> = serde_json::from_value(kept);
        assert!(
            decision.is_ok_and(|decision| decision.weights.session > 0.9),
            "a record kept by a later node is read leniently"
        );
    }

    #[test]
    fn work_goes_at_random_to_one_of_the_machines_near_the_best() {
        let realm = [
            machine("skiff", "linux", 31, &[]),
            machine("kestrel", "linux", 32, &[]),
            machine("tern", "linux", 20, &[]),
            over(machine("osprey", "linux", 32, &[]), &[Limit::Cpu]),
        ];
        let mut near = Vec::new();
        for chosen in 0..2 {
            let placement = place_at(
                &needing(Vec::new()),
                &realm,
                &Loads::new(),
                now(),
                &mut |many| {
                    near.push(many);
                    chosen
                },
            );
            assert_eq!(
                placed(&placement),
                Some(["kestrel", "skiff"][chosen]),
                "the best first, then the next"
            );
        }
        assert_eq!(
            near,
            [2, 2],
            "kestrel and skiff are within the band, but neither tern, further off, nor osprey, over"
        );
    }

    /// A machine as it might say itself: `system` on `x86_64` with eight
    /// cores, plugged in, idle, with `free` GiB of its 64 and half its
    /// disk free, and reaching `reach`.
    pub(crate) fn machine(name: &str, system: &str, free: u64, reach: &[&str]) -> Machine {
        Machine {
            system: Some(system.to_owned()),
            arch: "x86_64".to_owned(),
            cpu: facts::Cpu {
                model: None,
                cores: Some(8),
                busy: None,
            },
            memory: Some(64 << 30),
            disk: Some(1 << 40),
            disk_free: Some(1 << 39),
            disk_band: None,
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
                ..Declared::default()
            }),
            ..facts::measure(name, &Sources::nowhere())
        }
    }

    fn placed(placement: &Placement) -> Option<&str> {
        match placement {
            Placement::Placed { machine, .. } => Some(machine),
            _ => None,
        }
    }

    pub(crate) fn needing(needs: Vec<Need>) -> Work {
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
            machine("skiff", "linux", 8, &["home", "internet", "work-services"]),
            machine("tern", "linux", 12, &["home", "internet"]),
            machine("kestrel", "macos", 30, &["home", "internet"]),
        ];
        let placement = place(
            &needing(vec![Need::Reach("work-services".to_owned())]),
            &realm,
        );
        assert_eq!(placed(&placement), Some("skiff"), "only skiff reaches it");
        let Placement::Placed { passed, .. } = placement else {
            unreachable!()
        };
        assert!(
            passed
                .iter()
                .all(|miss| miss.because
                    == [Because::Lacks(Need::Reach("work-services".to_owned()))]),
            "each other machine lacks it: {passed:?}"
        );
        assert_eq!(
            placed(&place(
                &needing(vec![Need::System("linux".to_owned())]),
                &realm
            )),
            Some("tern"),
            "of those that qualify, the one with the most memory free"
        );
        let mut work = needing(vec![Need::System("linux".to_owned())]);
        work.prefers = vec![Need::Reach("work-services".to_owned())];
        assert_eq!(
            placed(&place(&work, &realm)),
            Some("skiff"),
            "a preference met outranks memory free"
        );
    }

    #[test]
    fn a_machine_on_its_battery_below_the_mark_takes_no_new_work() {
        let mut kestrel = machine("kestrel", "macos", 30, &["home"]);
        kestrel.power = Some(Power {
            on_battery: true,
            charge: Some(awake::LOW_CHARGE - 1),
        });
        let realm = [kestrel.clone()];
        let only_it = needing(vec![Need::System("macos".to_owned())]);
        assert_eq!(
            placed(&place(&only_it, &realm)),
            None,
            "not even what only it can take"
        );
        let mut tolerant = needing(Vec::new());
        tolerant.tolerates = vec![LOW_BATTERY.to_owned(), ON_BATTERY.to_owned()];
        assert_eq!(
            placed(&place(&tolerant, &realm)),
            Some("kestrel"),
            "unless the work doesn't mind"
        );
        kestrel.power = Some(Power {
            on_battery: true,
            charge: Some(awake::LOW_CHARGE),
        });
        assert_eq!(
            placed(&place(&only_it, &[kestrel])),
            Some("kestrel"),
            "at the mark it still takes work"
        );
    }

    #[test]
    fn a_machine_that_would_rather_not_takes_work_only_when_none_else_will() {
        let mut kestrel = machine("kestrel", "macos", 30, &["home"]);
        kestrel.power = Some(Power {
            on_battery: true,
            charge: Some(40),
        });
        let mut skiff = machine("skiff", "linux", 2, &["home"]);
        let realm = [kestrel.clone(), skiff.clone()];
        assert_eq!(
            placed(&place(&needing(Vec::new()), &realm)),
            Some("skiff"),
            "a machine on its battery goes after one plugged in"
        );
        let mut tolerant = needing(Vec::new());
        tolerant.tolerates = vec![ON_BATTERY.to_owned()];
        assert_eq!(
            placed(&place(&tolerant, &realm)),
            Some("kestrel"),
            "unless the work doesn't mind"
        );
        assert_eq!(
            placed(&place(
                &needing(vec![Need::System("macos".to_owned())]),
                &realm
            )),
            Some("kestrel"),
            "and it still takes what only it can"
        );

        skiff.room.as_mut().expect("a room").over = Some(vec![Limit::Memory]);
        kestrel.power = None;
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[skiff.clone(), kestrel])),
            Some("kestrel"),
            "a machine past its queue's memory limit goes after one that isn't"
        );

        skiff.declared.as_mut().expect("declared").taints = vec![Taint {
            key: "fleets".to_owned(),
            effect: Effect::Never,
            why: "no fleets here".to_owned(),
        }];
        let only = [skiff];
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
            Some("skiff"),
            "unless the work tolerates it"
        );
    }

    #[test]
    fn a_machine_whose_disk_is_filling_goes_after_one_with_room() {
        let mut skiff = machine("skiff", "linux", 30, &[]);
        let mut tern = machine("tern", "linux", 2, &[]);
        skiff.disk_band = Some(Band::NearlyFull);
        tern.disk_band = Some(Band::Healthy);
        let realm = [skiff.clone(), tern.clone()];
        assert_eq!(
            placed(&place(&needing(Vec::new()), &realm)),
            Some("tern"),
            "a disk past healthy weighs against its machine, though it has more memory free"
        );
        let mut tolerant = needing(Vec::new());
        tolerant.tolerates = vec![DISK_PRESSURE.to_owned()];
        assert_eq!(
            placed(&place(&tolerant, &realm)),
            Some("skiff"),
            "unless the work doesn't mind"
        );
        skiff.disk_band = None;
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[skiff, tern])),
            Some("skiff"),
            "a disk no guard bands is no reason to pass a machine by"
        );
    }

    /// `machine` working as hard as `cpu` percent of the last ten seconds
    /// waited on a processor, as its queue says.
    pub(crate) fn pressed(mut machine: Machine, cpu: f64) -> Machine {
        machine.room.as_mut().expect("a room").pressure.cpu = cpu;
        machine
    }

    /// `machine` past each of `limits`, as its queue says.
    fn over(mut machine: Machine, limits: &[Limit]) -> Machine {
        machine.room.as_mut().expect("a room").over = Some(limits.to_vec());
        machine
    }

    #[test]
    fn a_machine_over_a_limit_is_never_chosen_while_another_is_not() {
        let kestrel = over(
            pressed(machine("kestrel", "macos", 60, &[]), 90.0),
            &[Limit::Cpu],
        );
        let mut skiff = machine("skiff", "linux", 2, &[]);
        skiff.disk_band = Some(Band::Filling);
        skiff.power = Some(Power {
            on_battery: true,
            charge: Some(60),
        });
        let realm = [kestrel.clone(), skiff];
        let Placement::Placed {
            machine: chosen,
            passed,
        } = place(&needing(Vec::new()), &realm)
        else {
            panic!("skiff takes it")
        };
        assert_eq!(
            chosen, "skiff",
            "a disk filling and a battery weigh less than a processor over its queue's limit"
        );
        assert!(
            passed[0]
                .because
                .iter()
                .any(|why| matches!(why, Because::RatherNot(taint) if taint.key == CPU_PRESSURE)),
            "kestrel says it is over: {passed:?}"
        );

        let osprey = over(machine("osprey", "linux", 1, &[]), &[Limit::Memory]);
        assert_eq!(
            placed(&place(
                &needing(Vec::new()),
                &[osprey.clone(), kestrel.clone()]
            )),
            Some("kestrel"),
            "of machines all over, the one with the most headroom"
        );
        let mut tolerant = needing(Vec::new());
        tolerant.tolerates = vec![CPU_PRESSURE.to_owned()];
        assert_eq!(
            placed(&place(&tolerant, &[osprey, kestrel])),
            Some("kestrel"),
            "and work that doesn't mind a pressed processor sees no limit there"
        );
    }

    #[test]
    fn a_machine_whose_room_is_unheard_goes_after_one_heard_lately() {
        let mut kestrel = machine("kestrel", "linux", 60, &[]);
        kestrel.room.as_mut().expect("a room").said_at = "2026-10-29T05:50:00Z".to_owned();
        let tern = pressed(machine("tern", "linux", 2, &[]), 80.0);
        let Placement::Placed {
            machine: chosen,
            passed,
        } = place(&needing(Vec::new()), &[kestrel.clone(), tern.clone()])
        else {
            panic!("tern takes it")
        };
        assert_eq!(
            chosen, "tern",
            "a room said ten minutes ago earns nothing, however roomy"
        );
        assert_eq!(passed[0].because, [Because::Unheard], "kestrel says why");
        kestrel.room = None;
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[kestrel, tern])),
            Some("tern"),
            "and nor does a room never said"
        );
    }

    #[test]
    fn machines_are_ranked_by_their_headroom() {
        // The name that settles a tie would choose kestrel each time.
        let idle = || machine("skiff", "linux", 32, &[]);
        let busy = pressed(machine("kestrel", "linux", 32, &[]), 70.0);
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[busy, idle()])),
            Some("skiff"),
            "the processor less pressed"
        );
        let mut full = machine("kestrel", "linux", 32, &[]);
        full.disk_free = Some(1 << 36);
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[full, idle()])),
            Some("skiff"),
            "the disk more free"
        );
        let crowded = machine("kestrel", "linux", 32, &[]);
        let loads = Loads::from([(
            "skiff".to_owned(),
            Load {
                sessions: 6,
                placed: 0,
            },
        )]);
        assert_eq!(
            placed(&place_at(
                &needing(Vec::new()),
                &[crowded, idle()],
                &loads,
                now(),
                &mut first
            )),
            Some("kestrel"),
            "the fewer sessions"
        );

        let mut filling = machine("tern", "linux", 56, &[]);
        filling.disk_band = Some(Band::Filling);
        let healthy = pressed(machine("osprey", "linux", 4, &[]), 80.0);
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[healthy, filling])),
            Some("tern"),
            "a disk filling is only a weight, outweighed by a machine idle beside one busy"
        );
    }

    #[test]
    fn of_machines_unpressed_the_one_whose_cores_are_less_busy_goes_first() {
        // Neither waits on its processors, so pressure can't tell them
        // apart; kestrel would win the tie by name.
        let mut busy = machine("kestrel", "linux", 32, &[]);
        busy.cpu.busy = Some(0.9);
        let mut quiet = machine("skiff", "linux", 32, &[]);
        quiet.cpu.busy = Some(0.1);
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[busy.clone(), quiet])),
            Some("skiff"),
            "a tenth busy has more room than nine tenths"
        );
        let pressed_quiet = pressed(machine("skiff", "linux", 32, &[]), 95.0);
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[busy, pressed_quiet])),
            Some("kestrel"),
            "a machine saying no use but waiting on every core has none to spare"
        );
    }

    #[test]
    fn a_processor_reading_not_a_number_counts_as_unknown() {
        // A share that isn't a number would be kept as null in the
        // placements log and never read back, and would rank by nothing.
        let mut odd = machine("kestrel", "linux", 32, &[]);
        odd.cpu.busy = Some(f64::NAN);
        let mut pressed_odd = pressed(machine("skiff", "linux", 32, &[]), f64::NAN);
        pressed_odd.cpu.busy = None;
        let unknown = pressed(machine("tern", "linux", 32, &[]), f64::INFINITY);
        let decision = decide_at(
            &needing(Vec::new()),
            &[odd, pressed_odd, unknown],
            &Loads::new(),
            now(),
            &mut first,
            &Weights::STARTING,
        );
        assert!(
            decision
                .judged
                .iter()
                .all(|judged| judged.headroom.is_finite()),
            "every headroom a number: {:?}",
            decision.judged
        );
        assert!(
            placed(&decision.placement).is_some(),
            "and the work still placed: {:?}",
            decision.placement
        );
    }

    #[test]
    fn a_disk_weighs_by_how_full_it_is() {
        let mut tern = machine("tern", "linux", 32, &[]);
        let osprey = pressed(machine("osprey", "linux", 32, &[]), 40.0);
        tern.disk_band = Some(Band::Filling);
        assert_eq!(
            placed(&place(
                &needing(Vec::new()),
                &[osprey.clone(), tern.clone()]
            )),
            Some("tern"),
            "a disk filling costs little, outweighed by a processor far less pressed"
        );
        for band in [Band::NearlyFull, Band::CriticallyFull] {
            tern.disk_band = Some(band);
            assert_eq!(
                placed(&place(
                    &needing(Vec::new()),
                    &[osprey.clone(), tern.clone()]
                )),
                Some("osprey"),
                "a disk {band:?} costs as any taint that would rather not"
            );
        }
    }

    #[test]
    fn a_burst_spreads_across_machines_alike() {
        let realm = [
            machine("kestrel", "linux", 32, &[]),
            machine("skiff", "linux", 32, &[]),
            over(machine("osprey", "linux", 60, &[]), &[Limit::Cpu]),
        ];
        let mut starts = Starts::default();
        let mut landed = BTreeMap::<String, u32>::new();
        let mut at = now();
        for _ in 0..10 {
            let loads = starts.loads(&realm, std::iter::empty(), at);
            let chosen = placed(&place_at(
                &needing(Vec::new()),
                &realm,
                &loads,
                at,
                &mut first,
            ))
            .expect("placed")
            .to_owned();
            starts.placed(&chosen, at);
            *landed.entry(chosen).or_default() += 1;
            at += jiff::SignedDuration::from_secs(1);
        }
        assert_eq!(
            landed,
            BTreeMap::from([("kestrel".to_owned(), 5), ("skiff".to_owned(), 5)]),
            "each start counts against its machine, and none goes to one over"
        );
    }

    #[test]
    fn a_start_counts_until_its_machine_says_its_room_after_it() {
        let mut kestrel = machine("kestrel", "linux", 32, &[]);
        let at: jiff::Timestamp = "2026-10-29T06:00:05Z".parse().expect("a time");
        let mut starts = Starts::default();
        starts.placed("kestrel", at);
        let sessions = ["kestrel", "kestrel", "skiff"];
        let later = at + jiff::SignedDuration::from_secs(10);
        assert_eq!(
            starts.loads(std::slice::from_ref(&kestrel), sessions, later)["kestrel"],
            Load {
                sessions: 2,
                placed: 1
            },
            "a start its room doesn't yet show counts beside its sessions"
        );
        kestrel.room.as_mut().expect("a room").said_at = "2026-10-29T06:00:06Z".to_owned();
        assert_eq!(
            starts.loads(std::slice::from_ref(&kestrel), sessions, later)["kestrel"].placed,
            0,
            "and no longer once its room is said after it"
        );
        kestrel.room.as_mut().expect("a room").said_at = "2026-10-29T06:00:00Z".to_owned();
        assert_eq!(
            starts.loads(std::slice::from_ref(&kestrel), sessions, later)["kestrel"].placed,
            0,
            "nor comes back, as it is forgotten"
        );
        starts.placed("kestrel", at);
        assert_eq!(
            starts.loads(
                std::slice::from_ref(&kestrel),
                std::iter::empty(),
                at + STARTS_COUNT_FOR
            )["kestrel"]
                .placed,
            0,
            "nor past its time, said after or not"
        );
    }

    #[test]
    fn work_waits_for_a_machine_out_of_reach_only_when_it_may() {
        let mut tern = machine("tern", "linux", 12, &["home"]);
        tern.state = Readiness::Unreachable;
        let realm = [machine("skiff", "linux", 8, &["home"]), tern];
        let mut work = needing(vec![Need::Machine("tern".to_owned())]);
        assert!(
            matches!(place(&work, &realm), Placement::Refused { .. }),
            "work that may not wait is refused"
        );
        work.may_wait = true;
        let Placement::Pending { misses } = place(&work, &realm) else {
            panic!("work that may wait waits for tern")
        };
        assert_eq!(
            misses,
            [
                Miss {
                    machine: "skiff".to_owned(),
                    because: vec![Because::Lacks(Need::Machine("tern".to_owned()))],
                },
                Miss {
                    machine: "tern".to_owned(),
                    because: vec![Because::Unreachable],
                },
            ],
            "each machine says why not now"
        );
    }

    #[test]
    fn work_never_goes_to_a_machine_that_leaves_before_it_ends() {
        let now = now();
        let mut sandbox = machine("sandbox", "linux", 30, &[]);
        let leaves = now + jiff::SignedDuration::from_mins(40);
        sandbox.declared.as_mut().expect("declared").leaves_at = Some(leaves);
        let realm = [machine("skiff", "linux", 8, &[]), sandbox];
        let mut work = needing(vec![Need::Machine("sandbox".to_owned())]);
        assert_eq!(
            placed(&place_at(&work, &realm, &Loads::new(), now, &mut first)),
            Some("sandbox"),
            "work that says nothing of how long it runs may go where it names"
        );

        work.needs = Vec::new();
        work.lasts = Some(jiff::SignedDuration::from_hours(1));
        let Placement::Placed { machine, passed } =
            place_at(&work, &realm, &Loads::new(), now, &mut first)
        else {
            panic!("work that outlasts the sandbox goes elsewhere")
        };
        assert_eq!(machine, "skiff", "to the machine that stays");
        assert_eq!(
            passed,
            [Miss {
                machine: "sandbox".to_owned(),
                because: vec![Because::LeavesFirst(leaves), worker_refuses()],
            }],
            "saying when the sandbox leaves"
        );

        work.lasts = None;
        work.needs = vec![Need::Machine("sandbox".to_owned())];
        work.may_wait = true;
        assert!(
            matches!(
                place_at(&work, &realm, &Loads::new(), leaves, &mut first),
                Placement::Refused { .. }
            ),
            "once it has left, nothing goes there, and nothing waits for it"
        );
    }

    #[test]
    fn work_goes_to_a_worker_only_where_it_names_the_worker() {
        let mut cloud = machine("cloud", "linux", 60, &["aquifer", "internet"]);
        cloud.declared.as_mut().expect("declared").costs_money = true;
        let mut lent = machine("lent", "linux", 60, &["lent-net", "internet"]);
        let later = now() + jiff::SignedDuration::from_hours(8);
        lent.declared.as_mut().expect("declared").leaves_at = Some(later);
        let realm = [
            machine("skiff", "linux", 2, &["home", "internet"]),
            cloud,
            lent,
        ];
        let Placement::Placed {
            machine: chosen,
            passed,
        } = place(&needing(Vec::new()), &realm)
        else {
            panic!("work naming nothing goes to the person's own machine")
        };
        assert_eq!(
            chosen, "skiff",
            "never to a worker, however much room it has"
        );
        assert_eq!(
            passed,
            [
                Miss {
                    machine: "cloud".to_owned(),
                    because: vec![worker_refuses()],
                },
                Miss {
                    machine: "lent".to_owned(),
                    because: vec![worker_refuses()],
                },
            ],
            "each worker says it takes only work that names it"
        );
        let internet = needing(vec![Need::Reach("internet".to_owned())]);
        assert_eq!(
            placed(&place(&internet, &realm)),
            Some("skiff"),
            "a network the person's own machines reach doesn't name a worker"
        );
        for (need, worker) in [
            (Need::Machine("cloud".to_owned()), "cloud"),
            (Need::Reach("aquifer".to_owned()), "cloud"),
            (Need::Reach("lent-net".to_owned()), "lent"),
        ] {
            assert_eq!(
                placed(&place(&needing(vec![need.clone()]), &realm)),
                Some(worker),
                "work needing {need} names {worker}"
            );
        }
        let mut tolerant = needing(Vec::new());
        tolerant.tolerates = vec![WORKER.to_owned()];
        assert_eq!(
            placed(&place(&tolerant, &realm)),
            Some("skiff"),
            "tolerating isn't naming"
        );
        // A network the person's own machines reach now and then is theirs
        // too, whether or not its probe answers this minute.
        let mut laptop = machine("kestrel", "macos", 30, &[]);
        let declared = laptop.declared.as_mut().expect("declared");
        declared.probes.insert(
            "lent-net".to_owned(),
            Probe::Connect("192.0.2.1:22".to_owned()),
        );
        let with_laptop = [realm[0].clone(), realm[1].clone(), realm[2].clone(), laptop];
        assert!(
            matches!(
                place(
                    &needing(vec![Need::Reach("lent-net".to_owned())]),
                    &with_laptop
                ),
                Placement::Refused { .. }
            ),
            "a network a laptop probes for doesn't name the worker that reaches it"
        );
        // A machine that declares nothing is taken as one of the person's
        // own, as every machine was before workers were told apart.
        let mut quiet = machine("osprey", "linux", 90, &[]);
        quiet.declared = None;
        let with_quiet = [realm[0].clone(), quiet];
        assert_eq!(
            placed(&place(&needing(Vec::new()), &with_quiet)),
            Some("osprey"),
            "a machine that says nothing of itself isn't told a worker"
        );
        let workers_only = [realm[1].clone(), realm[2].clone()];
        assert!(
            matches!(
                place(&needing(Vec::new()), &workers_only),
                Placement::Refused { .. }
            ),
            "where only workers are left, work naming none goes nowhere"
        );
    }

    #[test]
    fn work_asked_on_a_worker_stays_there_unless_it_names_another() {
        let mut cloud = machine("cloud", "linux", 2, &["aquifer", "internet"]);
        cloud.declared.as_mut().expect("declared").costs_money = true;
        let mut lent = machine("lent", "linux", 60, &["lent-net"]);
        lent.declared.as_mut().expect("declared").costs_money = true;
        let realm = [
            cloud.clone(),
            machine("skiff", "linux", 60, &["home", "internet"]),
            lent,
        ];
        for need in [Vec::new(), vec![Need::Reach("internet".to_owned())]] {
            let work = asked_on(&needing(need.clone()), &cloud, &realm);
            assert_eq!(
                placed(&place(&work, &realm)),
                Some("cloud"),
                "work needing {need:?} asked on a worker runs there, never on the person's own"
            );
        }
        let elsewhere = asked_on(
            &needing(vec![Need::Reach("lent-net".to_owned())]),
            &cloud,
            &realm,
        );
        assert_eq!(
            placed(&place(&elsewhere, &realm)),
            Some("lent"),
            "unless it names another worker"
        );
        let at_home = needing(Vec::new());
        assert_eq!(
            asked_on(&at_home, &realm[1], &realm),
            at_home,
            "work asked on the person's own machine is placed as asked"
        );
    }

    #[test]
    fn a_machine_is_made_only_for_work_that_names_a_worker() {
        let offers = [offer("open", "0.0", None, &["aquifer"])];
        let realm = [machine("skiff", "linux", 0, &["home", "internet"])];
        let Err(misses) = to_make(&needing(Vec::new()), &offers, &realm) else {
            panic!("work naming nothing has no worker made for it")
        };
        assert_eq!(misses[0].because, [worker_refuses()], "saying why");
        assert!(
            to_make(
                &needing(vec![Need::Reach("internet".to_owned())]),
                &offers,
                &realm
            )
            .is_err(),
            "nor for work needing what the person's own machines reach"
        );
        assert_eq!(
            to_make(
                &needing(vec![Need::Reach("aquifer".to_owned())]),
                &offers,
                &realm
            )
            .map(|chosen| chosen.shape.as_str()),
            Ok("open"),
            "but for work needing a network only workers reach"
        );
    }

    /// An offer of `shape` on `linux` and `x86_64`, reaching `reach`, at `hourly`
    /// US dollars and living `lasts` seconds.
    fn offer(shape: &str, hourly: &str, lasts: Option<u64>, reach: &[&str]) -> Offer {
        let declared = machine(shape, "linux", 0, reach).declared;
        Offer {
            provider: "lender".to_owned(),
            shape: shape.to_owned(),
            facts: crate::offer::Shape {
                system: Some("linux".to_owned()),
                arch: Some("x86_64".to_owned()),
                declared,
                ..crate::offer::Shape::default()
            },
            costs: crate::offer::Costs {
                hourly: hourly.to_owned(),
                currency: "USD".to_owned(),
            },
            lasts,
        }
    }

    #[test]
    fn the_cheapest_offer_that_would_take_the_work_is_made() {
        let offers = [
            offer("large", "10.0", Some(7200), &["work"]),
            offer("small", "9.50", Some(3600), &["work"]),
            offer("open", "0.001", None, &[]),
        ];
        let mut work = needing(vec![Need::Reach("work".to_owned())]);
        assert_eq!(
            to_make(&work, &offers, &[]).map(|chosen| chosen.shape.as_str()),
            Ok("small"),
            "of the two that reach what it needs, the cheaper, by amount and not by how it is written"
        );

        work.lasts = Some(jiff::SignedDuration::from_hours(1));
        assert_eq!(
            to_make(&work, &offers, &[]).map(|chosen| chosen.shape.as_str()),
            Ok("large"),
            "not one that would end before the work does"
        );

        work.needs.push(Need::Accelerator(AcceleratorKind::Gpu));
        work.needs.push(Need::Machine("skiff".to_owned()));
        let Err(misses) = to_make(&work, &offers, &[]) else {
            panic!("no offer promises a gpu, and none is a machine already named");
        };
        assert_eq!(
            said(&misses[..2]),
            "large from lender hasn't said a gpu, isn't skiff; small from lender \
             would live only 1h, ending before the work, hasn't said a gpu, isn't skiff",
            "each offer says why not, a fact it doesn't promise meeting no need"
        );
    }

    #[test]
    fn what_a_machine_never_said_meets_no_need() {
        let mut quiet = machine("osprey", "macos", 16, &[]);
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

        let mut npu = machine("skiff", "linux", 8, &[]);
        npu.accelerators = Some(vec![Accelerator {
            kind: AcceleratorKind::Npu,
            driver: "intel_vpu".to_owned(),
        }]);
        assert_eq!(
            placed(&place(
                &needing(vec![Need::Accelerator(AcceleratorKind::Npu)]),
                &[npu]
            )),
            Some("skiff"),
            "an accelerator it lists meets the need"
        );
    }

    #[test]
    fn a_network_reached_now_and_then_is_met_while_its_probe_answers() {
        let mut laptop = machine("skiff", "linux", 8, &["internet"]);
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
            Some("skiff"),
            "a probe that answered meets it"
        );
        assert_eq!(
            placed(&place(
                &needing(vec![Need::Reach("internet".to_owned())]),
                &[laptop]
            )),
            Some("skiff"),
            "a network declared outright needs no probe"
        );
    }
}
