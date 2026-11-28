//! Placement: which machine of the realm work should go to, judged from
//! what each machine last said it is. Work names what it needs, what it
//! would prefer, and the taints it tolerates; the answer is the machine
//! it goes to, that it waits, or that it can't go anywhere, with each
//! machine's reasons.
//!
//! A machine's taints are those its configuration declares and those its
//! conditions set while they last: running on its battery, running on it
//! below the mark at which it is let sleep, which refuses all new work,
//! being past its queue's memory or processor limit, its processors
//! saturated, or its disk filling. A taint that refuses keeps work off a
//! machine. Those that qualify rank in tiers: heard lately and neither
//! past a limit of its queue's nor saturated, then past a limit, then
//! saturated, then those whose room hasn't been said lately, with a
//! machine its configuration keeps lively after every other heard one
//! with room or over, and past its ceiling after every heard one. Within a
//! tier, machines are ranked by the preferences each meets,
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
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::awake;
use crate::declared::Declared;
use crate::declared::{Effect, Lively, Taint};
use crate::disk::Band;
use crate::facts::{AcceleratorKind, HeldFor, Machine, Readiness};
use crate::offer::Offer;
use crate::room::{Limit, Room};

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
    /// The fence a session started for it declares it runs in, where its
    /// asker says: never looser than the agent fence, and only ever
    /// raising the node's judgement of an agent, never lowering it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "declared_or_none"
    )]
    pub fence: Option<DeclaredFence>,
}

/// The fence a start declares its session runs in, and what declared it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredFence {
    /// Which fence.
    pub kind: FenceKind,
    /// What declared it.
    pub from: FenceFrom,
}

/// A fence a start may declare: none looser than the agent fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FenceKind {
    /// The agent fence.
    Fence,
    /// The strict fence.
    Strict,
}

impl From<FenceKind> for crate::declared::Kind {
    fn from(kind: FenceKind) -> Self {
        match kind {
            FenceKind::Fence => Self::Fence,
            FenceKind::Strict => Self::Strict,
        }
    }
}

/// What declared a start's fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FenceFrom {
    /// The spawn that asked for the start.
    Spawn,
    /// The README of the quest it works on.
    Quest,
    /// Nothing: the fence the asker's harness runs it in by default,
    /// said for the record, which vouches for nothing.
    Default,
    /// A source this build doesn't know, as a newer asker may name one.
    /// What declared a fence moves nothing, since a declaration only
    /// ever raises, so its kind still counts.
    #[serde(other)]
    Unknown,
}

/// A declared fence in the shape this build reads, or none: one naming a
/// kind it doesn't know, as a newer asker or node may say it, is passed
/// over, as a node older than the field passes it over, rather than
/// refusing the ask or the line it is in. One naming a kind it knows
/// keeps it whatever it says of its source, a source left out, null or
/// not a word among them ([`FenceFrom::Unknown`]), since a source moves
/// nothing and a kind only ever raises.
///
/// # Errors
///
/// Only where what is said can't be read at all.
pub fn declared_or_none<'de, D: serde::Deserializer<'de>>(
    said: D,
) -> Result<Option<DeclaredFence>, D::Error> {
    let Some(declared) = Option::<serde_json::Value>::deserialize(said)? else {
        return Ok(None);
    };
    let Some(kind) = read_field::<FenceKind>(&declared, "kind") else {
        return Ok(None);
    };
    let from = read_field(&declared, "from").unwrap_or(FenceFrom::Unknown);
    Ok(Some(DeclaredFence { kind, from }))
}

/// `field` of `declared`, where it is there and reads as a `T`.
fn read_field<T: serde::de::DeserializeOwned>(
    declared: &serde_json::Value,
    field: &str,
) -> Option<T> {
    serde_json::from_value(declared.get(field)?.clone()).ok()
}

impl Work {
    /// Whether this work doesn't mind the taint keyed `key`.
    #[must_use]
    pub fn tolerating(&self, key: &str) -> bool {
        self.tolerates.iter().any(|tolerated| {
            tolerated == key || (tolerated == DISK_FILLING_BEFORE && key == DISK_FILLING)
        })
    }

    /// This work as it is asked of the machine already chosen for it,
    /// with only the needs and preferences its node can read, as
    /// [`Need::crosses`] says: a node that can't read one drops the link
    /// rather than answer, and once a machine is chosen a preference
    /// changes nothing.
    #[must_use]
    pub fn asked_there(mut self) -> Self {
        self.needs.retain(|need| need.crosses(true));
        self.prefers.retain(|need| need.crosses(false));
        self
    }
}

impl Need {
    /// Whether this need, `needed` or only preferred, goes with work asked
    /// of the machine already chosen for it. Each need is named here, so a
    /// new one says whether a node from before it, which can't read it,
    /// may have been chosen.
    fn crosses(&self, needed: bool) -> bool {
        match self {
            Self::Machine(_)
            | Self::System(_)
            | Self::Arch(_)
            | Self::Reach(_)
            | Self::Accelerator(_) => true,
            // Only a node that names its kit, and so reads a tool, can
            // have been chosen for a tool it needs; one preferred may
            // have chosen any node.
            Self::Tool(_) => needed,
            // Met here, by the machine's declaration or its make, while a
            // made machine's node may be too old to read it. The need for
            // itself that the machine's node adds names it as the worker
            // it is.
            Self::Cloud => false,
        }
    }
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
    /// A tool its realm's kit holds, by name, such as `gcloud`: a machine
    /// whose node names no kit hasn't said, as with any unsaid fact.
    Tool(String),
    /// A machine in the cloud: one linked that costs money, or one a
    /// provider makes for it. It names a worker, so a worker's refusal of
    /// work that names none doesn't refuse it.
    Cloud,
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
    /// It holds as many sessions as it holds at most, counting the starts
    /// placed there that it doesn't yet show.
    Full(u32),
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
            Need::Tool(tool) => write!(f, "tool {tool}"),
            Need::Cloud => write!(f, "a cloud machine"),
        }
    }
}

impl fmt::Display for Because {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Because::Lacks(Need::Machine(name)) => write!(f, "isn't {name}"),
            Because::Lacks(Need::Cloud) => write!(f, "isn't a cloud machine"),
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
            Because::Full(most) => write!(f, "holds the {most} sessions it holds at most"),
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

/// The key of the taint a machine whose home's disk is past healthy
/// carries: filling, nearly full or critically full.
pub const DISK_FILLING: &str = "disk-filling";

/// The key [`DISK_FILLING`] had before, by which work still tolerates it:
/// disk pressure is waiting on the disk, not how full it is.
pub const DISK_FILLING_BEFORE: &str = "disk-pressure";

/// The key of the taint a machine whose processors are saturated carries:
/// as its room's own node judged them (a [`Saturation`]), or at
/// [`SATURATED_PRESSURE`] or more where its room says no verdict. It goes after
/// every machine that isn't, so work goes to one that can run it at once,
/// and never refuses work, since every machine saturated still runs what
/// it must somewhere. A machine declaring it for itself ranks saturated
/// while its room is heard, as one declaring `memory-pressure` ranks over:
/// a key means one thing whoever sets it.
pub const CPU_SATURATED: &str = "cpu-saturated";

/// The processor pressure, in percent of the last ten seconds, at which a
/// machine's processors are saturated where its room says no verdict of
/// its node's, as an older node's doesn't: so much waits on them that a
/// session started there is slow for everyone on it.
pub const SATURATED_PRESSURE: f64 = 80.0;

/// The processor pressure at which a node judges its machine's processors
/// saturated, for a [`Saturation`].
pub const SATURATES_AT: f64 = 85.0;

/// The processor pressure a machine its node judges saturated must fall
/// under before it isn't. The band from here to [`SATURATES_AT`] sits
/// about [`SATURATED_PRESSURE`], so a machine is held saturated about as
/// often as the line alone holds it, while one whose ten-second reading
/// swings about the line, as it does under bursty work, isn't judged
/// saturated and then not with each reading, sending each ask elsewhere.
pub const UNSATURATES_UNDER: f64 = 75.0;

/// Whether a machine's processors are saturated, as its own node judges
/// them across its measures: saturated from [`SATURATES_AT`] until under
/// [`UNSATURATES_UNDER`], and between the two as it was last judged. Its
/// node says the verdict in its room, so every node places by the same
/// one and a replay reads what was decided.
#[derive(Debug, Default)]
pub struct Saturation(Mutex<bool>);

impl Saturation {
    /// Whether processors at `cpu` pressure are saturated, given what this
    /// judged last. A pressure that is no number changes nothing.
    pub fn judge(&self, cpu: f64) -> bool {
        let mut held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if cpu >= SATURATES_AT {
            *held = true;
        } else if cpu < UNSATURATES_UNDER {
            *held = false;
        }
        *held
    }

    /// Says this machine's verdict in `room`, its room as just measured.
    /// A host's room told to a machine within it keeps the host's verdict,
    /// where the host said one. No room, or a host's verdict, forgets this
    /// one's, so a reading of its own after either is never judged by one
    /// from before it.
    pub fn say(&self, room: Option<&mut Room>) {
        match room {
            Some(room) if room.saturated.is_none() => {
                room.saturated = Some(self.judge(room.pressure.cpu));
            }
            Some(_) | None => self.forget(),
        }
    }

    /// Forgets the verdict, so the next is judged afresh.
    fn forget(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = false;
    }
}

/// Whether `room` is saturated: as its node judged it, or by the line
/// where a node too old to judge said none.
fn saturated(room: &Room) -> bool {
    room.saturated
        .unwrap_or(room.pressure.cpu >= SATURATED_PRESSURE)
}

/// The key of the taint a machine whose home's disk is critically full
/// carries, which takes no work: what it starts would fail to write.
pub const DISK_FULL: &str = "disk-full";

/// The key of the refusal an agent's work meets on a machine whose ground
/// is looser than where it asks, which would refuse to run it there.
pub const LOOSER_GROUND: &str = "looser-ground";

/// The key of the refusal an agent's run meets on a machine whose fence
/// for it names no way to run a command, which would refuse it there.
pub const RUNS_NOTHING: &str = "runs-nothing";

/// The key of the refusal an agent's held start meets on a Mac, whose
/// holder starts it outside any fence.
pub const LIFTS_FENCE: &str = "lifts-fence";

/// The key of the taint a machine past its queue's processor limit carries.
pub const CPU_PRESSURE: &str = "cpu-pressure";

/// The key of the taint a machine kept lively ([`Declared::kept_lively`])
/// carries past its ceiling, which ranks it over, as being past a limit
/// of its queue's does.
pub const KEPT_LIVELY: &str = "kept-lively";

/// How long a start placed counts against its machine at most, where its
/// room isn't said after it sooner.
pub const STARTS_COUNT_FOR: jiff::SignedDuration = jiff::SignedDuration::from_mins(5);

/// How long after a start its machine's room must be said to show it: a
/// room is said every few seconds here, sooner than a session begun can
/// register or load its machine, and a burst of spawns on 17 Nov showed
/// its sessions within about sixteen seconds of being asked.
pub const A_START_SHOWS_AFTER: jiff::SignedDuration = jiff::SignedDuration::from_secs(30);

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

/// What each start a core carries costs, while its machine's room doesn't
/// show it yet.
pub const SESSION_WEIGHT: f64 = 0.5;

/// What each session running a turn costs a core it runs on. What it
/// uses already shows in its machine's room, as processor and memory
/// taken, so this weighs only the burst it may make before the next
/// reading: at the start weight, 40 sessions mostly waiting on a model
/// cost a 16-core machine more than its whole processor headroom, and
/// sent work to a machine whose processors were busier.
pub const RUNNING_WEIGHT: f64 = 0.1;

/// What each taint that would rather not costs, beside those that say a
/// machine is over, which rank it after every machine that isn't.
pub const RATHER_NOT_WEIGHT: f64 = 1.0;

/// What a disk filling costs, beside one nearly or critically full, which
/// costs as any taint that would rather not: a guard clearing what has
/// waited a while is no reason to keep work off.
pub const FILLING_WEIGHT: f64 = 0.25;

/// What each part of a machine's room is worth to placing, and how near
/// the best a machine may be and still be drawn: the one set of knobs a
/// replay of kept placements varies to tune placing. A set read with
/// [`Weights::read`] takes the starting weight for each it leaves out, so a
/// candidate names only the knob it turns; one deserialized as a record's
/// does too, but for `running`, which it leaves none, as a record kept
/// before it was said was weighed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Weights {
    /// What a processor's whole headroom is worth.
    pub cpu: f64,
    /// What a machine's whole memory free is worth.
    pub memory: f64,
    /// What a machine's whole disk free is worth.
    pub disk: f64,
    /// What each start a core carries costs, and each session running
    /// where `running` says none.
    pub session: f64,
    /// What each session running a turn costs a core; none in weights
    /// kept before it was said, which weighed a session running as a
    /// start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running: Option<f64>,
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
        running: Some(RUNNING_WEIGHT),
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
        let mut read: Self = serde_json::from_str(said).map_err(|error| error.to_string())?;
        // A record's weights without `running` were weighed before it was
        // said, but a candidate's leave it to the starting one.
        if !fields.contains_key("running") {
            read.running = Self::STARTING.running;
        }
        Ok(read)
    }

    /// What weighed a decision kept before it said its weights: the
    /// starting ones of then, which weighed a session running as a start.
    fn before_said() -> Self {
        Self {
            running: None,
            ..Self::STARTING
        }
    }
}

impl Default for Weights {
    fn default() -> Self {
        Self::STARTING
    }
}

/// What `taint`, which would rather not have the work, costs `machine`.
fn costs(machine: &Machine, taint: &Taint, weights: &Weights) -> f64 {
    if taint.key == DISK_FILLING && machine.disk_band == Some(Band::Filling) {
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
    let running = weights.running.unwrap_or(weights.session);
    let carried = (running * f64::from(load.sessions) + weights.session * f64::from(load.placed))
        / f64::from(cores);
    weights.cpu * cpu + weights.memory * memory + weights.disk * disk - carried - rather_not
}

/// Whether `machine`'s room was said within [`ROOM_HEARD_WITHIN`] of `now`,
/// either way: a room dated further ahead than that is no more heard than
/// one as far behind, or a clock set wrong would keep it heard, and so
/// saturated, for good.
fn heard(machine: &Machine, now: jiff::Timestamp) -> bool {
    said_at(machine).is_some_and(|said| now.duration_since(said).abs() <= ROOM_HEARD_WITHIN)
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
/// its machine until that machine says its room [`A_START_SHOWS_AFTER`]
/// it or later, as its room then shows it, or for [`STARTS_COUNT_FOR`] at
/// most, and is then forgotten.
#[derive(Debug, Default)]
pub struct Starts {
    placed: Vec<(String, jiff::Timestamp)>,
}

impl Starts {
    /// Keeps that a start was placed on `machine` at `at`.
    pub fn placed(&mut self, machine: &str, at: jiff::Timestamp) {
        self.placed.push((machine.to_owned(), at));
    }

    /// Forgets the start kept as placed on `machine` at `at`, which it
    /// refused before it began, so it weighs there no longer. One already
    /// forgotten, as its machine's room showed it, is left so, rather than
    /// another start's load forgotten in its place.
    pub fn unplaced(&mut self, machine: &str, at: jiff::Timestamp) {
        let this = |(one, when): &(String, jiff::Timestamp)| one == machine && *when == at;
        if let Some(kept) = self.placed.iter().position(this) {
            self.placed.remove(kept);
        }
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
                .is_some_and(|said| said.duration_since(*at) >= A_START_SHOWS_AFTER);
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
    /// What weighed them; the starting weights of then where a kept
    /// decision predates saying so.
    #[serde(default = "Weights::before_said")]
    pub weights: Weights,
}

/// How one machine was judged for work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a field of the placement log apart, so a record kept before one was \
              judged still reads, and replay and hindsight read each by name"
)]
pub struct Judgement {
    /// The machine.
    pub machine: String,
    /// Whether it could take the work at all.
    pub qualifies: bool,
    /// Whether it was past a limit of its queue's the work minds.
    pub over: bool,
    /// Whether its room hadn't been said lately.
    pub unheard: bool,
    /// Whether its processors were saturated, as its room said lately.
    /// Records kept before it was judged read as not.
    #[serde(default)]
    pub saturated: bool,
    /// Whether it was kept lively, so ranked after machines that weren't,
    /// and not where the work tolerated [`KEPT_LIVELY`]. Records kept
    /// before it was judged read as not.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub kept_lively: bool,
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

/// Whether `decision` placed its work on a machine heard lately but past
/// its room, over a limit of its queue's or saturated, or on one kept
/// lively, which the person would have take work only where nothing else
/// does. Machines are ranked by those tiers first, so then every heard
/// machine that takes it is past its room or kept lively, and a machine
/// made for it would have more. One unheard isn't judged past it, as
/// nothing says so.
#[must_use]
pub fn placed_past_room(decision: &Decision) -> bool {
    let Placement::Placed { machine, .. } = &decision.placement else {
        return false;
    };
    decision.judged.iter().any(|one| {
        one.machine == *machine && !one.unheard && (one.over || one.saturated || one.kept_lively)
    })
}

/// Whether `decision` placed its work on a machine heard lately and within
/// its room, neither over a limit of its queue's nor saturated: one that
/// says it takes more, where one silent may be stuck.
#[must_use]
pub fn placed_with_room(decision: &Decision) -> bool {
    let Placement::Placed { machine, .. } = &decision.placement else {
        return false;
    };
    decision
        .judged
        .iter()
        .any(|one| one.machine == *machine && !one.unheard && !one.over && !one.saturated)
}

/// Each of `machines` that holds as many sessions as it holds at most,
/// refused for it: its live sessions, as `holding` counts them by
/// machine, and the starts `loads` says were placed there that it doesn't
/// yet show. How many it holds at most is its own configuration's word,
/// or, where that says none, what `promised` says the offer it was made
/// of promised; a machine neither caps takes any number, as before.
///
/// Live sessions are counted rather than a load's busy ones, since an
/// idle session holds its memory and its worker home all the same. Where
/// the machine's node says it holds more running than `holding` knows of
/// ([`Machine::holds`]), that is counted instead, as a start whose pi
/// says nothing of itself, a fenced worker's, is a session there all the
/// same. The larger is taken rather than their sum, since a held session
/// that says it is one is counted in both.
#[must_use]
pub fn full(
    machines: &[Machine],
    holding: &BTreeMap<String, u32>,
    loads: &Loads,
    promised: &BTreeMap<String, u32>,
) -> Vec<Miss> {
    full_keeping(machines, holding, loads, promised, &BTreeSet::new())
}

/// As [`full`], each machine named in `keeping` full one session short of
/// the most it holds, and said to hold that many at most, so the last is
/// kept for the person whose machine it is.
#[must_use]
pub fn full_keeping(
    machines: &[Machine],
    holding: &BTreeMap<String, u32>,
    loads: &Loads,
    promised: &BTreeMap<String, u32>,
    keeping: &BTreeSet<String>,
) -> Vec<Miss> {
    machines
        .iter()
        .filter_map(|machine| {
            let most = machine
                .declared
                .as_ref()
                .and_then(|declared| declared.sessions_at_most)
                .or_else(|| promised.get(&machine.name).copied())?;
            let most = most.saturating_sub(u32::from(keeping.contains(&machine.name)));
            let held = holding
                .get(&machine.name)
                .copied()
                .unwrap_or(0)
                .max(machine.holds.unwrap_or(0));
            let placed = loads.get(&machine.name).map_or(0, |load| load.placed);
            (held.saturating_add(placed) >= most).then(|| Miss {
                machine: machine.name.clone(),
                because: vec![Because::Full(most)],
            })
        })
        .collect()
}

/// Places `work` among `machines`, each carrying its `loads`, saying how
/// each was judged.
#[must_use]
pub fn decide(work: &Work, machines: &[Machine], loads: &Loads) -> Decision {
    decide_refusing(work, machines, loads, &[])
}

/// As [`decide`], each machine named in `refused` refused for its reasons
/// besides its own, whatever the work tolerates, as one whose ground is
/// looser than an agent asking stands on. Each still counts as one of the
/// realm's machines, as where the person's lasting machines reach.
#[must_use]
pub fn decide_refusing(
    work: &Work,
    machines: &[Machine],
    loads: &Loads,
    refused: &[Miss],
) -> Decision {
    let mut draw = |many: usize| {
        // Each `RandomState` is seeded afresh, so its hash of nothing is a
        // fresh draw.
        let draw = RandomState::new().hash_one(());
        usize::try_from(draw % u64::try_from(many.max(1)).unwrap_or(1)).unwrap_or(0)
    };
    let now = jiff::Timestamp::now();
    decide_at(
        work,
        machines,
        loads,
        refused,
        now,
        &mut draw,
        &Weights::STARTING,
    )
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
    decide_at(work, machines, loads, &[], now, pick, &Weights::STARTING).placement
}

/// Decides where `work` would go among `machines` as of `now`, weighed by
/// `weights`, choosing the best of those near it rather than at random, so
/// replaying a kept placement says the same each time.
#[must_use]
pub fn decide_as(
    work: &Work,
    machines: &[Machine],
    loads: &Loads,
    refused: &[Miss],
    now: jiff::Timestamp,
    weights: &Weights,
) -> Decision {
    decide_at(work, machines, loads, refused, now, &mut |_| 0, weights)
}

/// Decides where `work` goes among `machines` as of `now`, weighed by
/// `weights`, each named in `refused` refused besides, with `pick`
/// choosing which of the machines near the best, given how many, takes it.
fn decide_at(
    work: &Work,
    machines: &[Machine],
    loads: &Loads,
    refused: &[Miss],
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
            // A room made for agents to share is paid for by being up, so
            // it takes work that names none of the workers.
            if one.declared.as_ref().is_some_and(Declared::is_worker)
                && one.held_for != HeldFor::Agents
                && !names_worker(work, &lasting)
                && !work.needs.contains(&Need::Machine(one.name.clone()))
            {
                judged.hard.push(worker_refuses());
            }
            judged.hard.extend(
                refused
                    .iter()
                    .filter(|miss| miss.machine == one.name)
                    .flat_map(|miss| miss.because.iter().cloned()),
            );
            judged
        })
        .collect();
    // Machines that qualify, by their tier first, then by how many
    // preferences each meets, earliest most, then by the headroom it has,
    // then by name. Of the best and those as good but for headroom within
    // the band, one at random, so asks alike spread rather than herd.
    let mut ranked: Vec<usize> = (0..judged.len())
        .filter(|&at| judged[at].hard.is_empty())
        .collect();
    ranked.sort_by(|&one, &other| {
        let (one, other) = (&judged[one], &judged[other]);
        rank(one)
            .cmp(&rank(other))
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
                rank(one) == rank(best)
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
            saturated: one.saturated,
            kept_lively: !matches!(one.kept, Kept::Not),
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

/// Which tier `one` ranks in, first first: heard and neither over nor
/// saturated, heard and over, heard and saturated, then unheard. A
/// saturated machine goes after one only over, since over here is often
/// a build queue short of a processor, which says nothing of sessions,
/// while saturated is every session there waiting. An unheard machine
/// goes last of all, since a room nobody has said lately can't show it has
/// any, and its headroom, from its disk alone, isn't comparable with a
/// room's.
const fn tier(one: &Judged) -> u8 {
    match (one.unheard, one.saturated, one.over) {
        (false, false, false) => 0,
        (false, false, true) => 1,
        (false, true, _) => 2,
        (true, _, _) => 3,
    }
}

/// Where `one` ranks, first first: by its [`tier`], but a machine kept
/// lively within its ceiling and its queue's limits after every other
/// heard machine with room and before those over, whatever limit they
/// are over: one within its ceiling is light by the ceiling's own
/// measure, which keeps it short of every limit it holds to, while one
/// over has already passed one of its own; within its ceiling but over a limit of its
/// queue's, after those over too and before those saturated; and past
/// its ceiling or saturated after every other heard machine, so other
/// machines take what they can before it. Unheard, it ranks with the
/// unheard, since an unheard room shows no room for work.
const fn rank(one: &Judged) -> u8 {
    let tier = tier(one);
    match one.kept {
        Kept::Not => tier * 2,
        _ if tier == 3 => 6,
        Kept::Within if tier == 0 => 1,
        Kept::Within if tier == 1 => 3,
        _ => 5,
    }
}

/// Whether a machine is kept lively, as the work judging it sees it.
#[derive(Debug, Clone, Copy)]
enum Kept {
    /// Not kept lively, or kept so but the work tolerates
    /// [`KEPT_LIVELY`], so ranked as any machine.
    Not,
    /// Kept lively and within its ceiling.
    Within,
    /// Kept lively and past its ceiling.
    Past,
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
    let named = names_worker(work, &lasting_reach(machines));
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
        && work.lasts.is_some_and(|lasts| lives < lasts)
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
        // What a machine not yet made would carry isn't offered yet.
        Need::Accelerator(_) | Need::Tool(_) => None,
        // Whatever a provider makes is in the cloud.
        Need::Cloud => Some(true),
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
    for taint in taints.filter(|taint| !work.tolerating(&taint.key)) {
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
        saturated: false,
        kept: Kept::Not,
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
            "it is past its memory limit, so it starts no work for want of memory",
        ));
    }
    if past(Limit::Cpu) {
        taints.push(condition(
            CPU_PRESSURE,
            "it is past its processor limit, so it starts no build beside a VM test",
        ));
    }
    if let Some(lively) = machine
        .declared
        .as_ref()
        .and_then(|said| said.kept_lively.as_ref())
        && let Some(room) = &machine.room
        && let Some(why) = past_lively(lively, room)
    {
        taints.push(condition(KEPT_LIVELY, &why));
    }
    if let Some(room) = &machine.room
        && saturated(room)
    {
        let cpu = room.pressure.cpu;
        taints.push(Taint {
            key: CPU_SATURATED.to_owned(),
            effect: Effect::PreferNot,
            why: if room.saturated == Some(true) && (UNSATURATES_UNDER..SATURATES_AT).contains(&cpu)
            {
                // Rounded down, so a pressure just under where it
                // saturates never reads as that pressure.
                let under = cpu.floor();
                format!(
                    "its processors are saturated still, at pressure {under:.0}, \
                     until it falls under {UNSATURATES_UNDER:.0}"
                )
            } else {
                format!("its processors are saturated, at pressure {cpu:.0}")
            },
        });
    }
    let full = match machine.disk_band {
        Some(Band::Filling) => Some("filling"),
        Some(Band::NearlyFull) => Some("nearly full"),
        Some(Band::CriticallyFull) => Some("critically full"),
        Some(Band::Healthy) | None => None,
    };
    if let Some(full) = full {
        taints.push(condition(
            DISK_FILLING,
            &format!("its disk guard says its disk is {full}"),
        ));
    }
    if machine.disk_band == Some(Band::CriticallyFull) {
        taints.push(Taint {
            key: DISK_FULL.to_owned(),
            effect: Effect::Never,
            why: format!(
                "its disk guard says its disk is critically full, so what it starts would \
                 fail to write (work that frees it may tolerate {DISK_FULL})"
            ),
        });
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
    /// Whether its processors are saturated, as its room said lately.
    saturated: bool,
    /// Whether it is kept lively, so ranked after machines that aren't.
    kept: Kept,
}

/// Why a machine kept lively to `lively` is past its ceiling, as `room`
/// says, if it is.
pub(crate) fn past_lively(lively: &Lively, room: &Room) -> Option<String> {
    // Over the last minute where its node says it, so a burst of a few
    // seconds never crosses the ceiling; over ten from a node too old.
    let (cpu, over) = room
        .cpu_minute
        .map_or((room.pressure.cpu, "ten seconds"), |minute| {
            (minute, "a minute")
        });
    if cpu > f64::from(lively.cpu) {
        return Some(format!(
            "it is kept lively, at processor pressure {} at most, and is at {:.0} over {over}",
            lively.cpu,
            // Rounded up, so a pressure just past the ceiling never
            // reads as the ceiling.
            cpu.ceil()
        ));
    }
    // Over the last minute alone, since memory stalls that hold every task
    // at once are what the person at the machine feels as it stopping.
    if let (Some(most), Some(full)) = (lively.memory_full, room.memory_full_minute)
        && full > f64::from(most)
    {
        return Some(format!(
            "it is kept lively, at memory pressure {most} at most, and is at {:.1} over a minute",
            // Rounded up to a tenth, as the processor's is to a whole, less
            // a hair first, since tenths in binary land just above
            // themselves (1.1 times 10 is past 11) and would read a tenth
            // high; the kernel says hundredths, so the hair never reaches
            // the ceiling.
            ((full * 10.0) - 1e-9).ceil() / 10.0
        ));
    }
    // Over the last minute alone too: disk stalls that hold every task at
    // once stop the machine as surely as memory's do.
    if let (Some(most), Some(full)) = (lively.io_full, room.io_full_minute)
        && full > f64::from(most)
    {
        return Some(format!(
            "it is kept lively, at disk pressure {most} at most, and is at {:.1} over a minute",
            // Rounded as memory's is, a hair less first.
            ((full * 10.0) - 1e-9).ceil() / 10.0
        ));
    }
    lively
        .memory_available
        .filter(|&least| room.memory_available < least)
        .map(|least| {
            format!(
                "it is kept lively, with {} MiB available at least, and has {}",
                least >> 20,
                room.memory_available >> 20
            )
        })
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
/// reach, as `aquifer` is, which names the workers that do, or needs a
/// cloud machine ([`Need::Cloud`]). Work names a worker otherwise only by
/// needing it by name: tolerating its refusal
/// isn't naming it, nor is needing a system, an architecture or an
/// accelerator, so work that says nothing of where it goes never goes to
/// a machine that costs money, leaves, or holds what only it was lent.
fn names_worker(work: &Work, lasting: &BTreeSet<&str>) -> bool {
    work.needs.iter().any(|need| match need {
        Need::Reach(network) => !lasting.contains(network.as_str()),
        Need::Cloud => true,
        _ => false,
    })
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
    let names_one = names_worker(&work, &lasting_reach(machines))
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
        if work.tolerating(&taint.key) {
            continue;
        }
        match taint.effect {
            Effect::Never => hard.push(Because::Refuses(taint)),
            Effect::PreferNot => rather_not.push(taint),
        }
    }
    let is_over =
        |taint: &&Taint| [MEMORY_PRESSURE, CPU_PRESSURE, KEPT_LIVELY].contains(&taint.key.as_str());
    // Over is read from the room even where it is unheard, and harmlessly:
    // a machine unheard ranks with those over either way.
    let over = rather_not.iter().any(|taint| is_over(&taint));
    let lately = heard(machine, now);
    // A room unheard says nothing of its processors now, so it neither
    // ranks the machine saturated nor is given as why it was passed.
    if !lately {
        rather_not.retain(|taint| taint.key != CPU_SATURATED);
    }
    let is_saturated = |taint: &&Taint| taint.key == CPU_SATURATED;
    // Saturation ranks a machine rather than costing it a taint's weight,
    // as being over does. Its processor pressure still weighs in its
    // headroom, so of two saturated machines the less pressed goes first.
    let weighed = rather_not
        .iter()
        .filter(|taint| !is_over(taint) && !is_saturated(taint))
        .map(|taint| costs(machine, taint, weights))
        .sum();
    let saturated = rather_not.iter().any(|taint| is_saturated(&taint));
    let kept = match machine
        .declared
        .as_ref()
        .and_then(|said| said.kept_lively.as_ref())
    {
        Some(_) if work.tolerating(KEPT_LIVELY) => Kept::Not,
        Some(_) if rather_not.iter().any(|taint| taint.key == KEPT_LIVELY) => Kept::Past,
        Some(_) => Kept::Within,
        None => Kept::Not,
    };
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
        saturated,
        kept,
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
        Need::Tool(tool) => machine.kit.as_ref().map(|kit| kit.holds(tool)),
        Need::Cloud => machine
            .declared
            .as_ref()
            .map(|declared| declared.costs_money),
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
            pressed(machine("tern", "linux", 32, &[]), 75.0),
            machine("osprey", "macos", 32, &[]),
        ];
        let work = needing(vec![Need::System("linux".to_owned())]);
        let decision = decide_at(
            &work,
            &realm,
            &Loads::new(),
            &[],
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
        // all free and it carries four starts on its eight cores.
        let realm = [
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
        let work = needing(Vec::new());
        let starting = decide_as(&work, &realm, &loads, &[], now(), &Weights::STARTING);
        assert_eq!(placed(&starting.placement), Some("skiff"));
        assert_eq!(starting.weights, Weights::STARTING);
        let heavier = Weights {
            session: 1.0,
            ..Weights::STARTING
        };
        let decided = decide_as(&work, &realm, &loads, &[], now(), &heavier);
        assert_eq!(
            placed(&decided.placement),
            Some("kestrel"),
            "starts costing twice as much outweigh the busier processor"
        );
        assert_eq!(decided.weights, heavier, "a decision says what weighed it");
    }

    #[test]
    fn a_running_session_weighs_less_than_a_start_its_room_doesnt_show() {
        let kestrel = machine("kestrel", "linux", 32, &[]);
        let weighed = |load| headroom(&kestrel, load, 0.0, true, &Weights::STARTING);
        let costs = |load| weighed(Load::default()) - weighed(load);
        let running = costs(Load {
            sessions: 8,
            placed: 0,
        });
        assert!(
            (running - RUNNING_WEIGHT).abs() < 1e-9,
            "a session running on each of its eight cores costs the running weight: {running}"
        );
        let started = costs(Load {
            sessions: 0,
            placed: 8,
        });
        assert!(
            (started - SESSION_WEIGHT).abs() < 1e-9,
            "a start on each costs the session weight, as its room shows none of it: {started}"
        );
        assert!(
            running < started,
            "a running session costs less than a start: {running} against {started}"
        );
    }

    #[test]
    fn a_cool_machine_running_many_sessions_is_chosen_over_a_pressed_one() {
        // Many sessions in a turn, mostly waiting on a model, on a machine
        // whose processors are cool, against few on one that is pressed.
        let mut cool = pressed(machine("kestrel", "linux", 32, &[]), 10.0);
        cool.cpu.cores = Some(16);
        let hot = pressed(machine("osprey", "linux", 32, &[]), 70.0);
        let loads = Loads::from([
            (
                "kestrel".to_owned(),
                Load {
                    sessions: 48,
                    placed: 0,
                },
            ),
            (
                "osprey".to_owned(),
                Load {
                    sessions: 10,
                    placed: 0,
                },
            ),
        ]);
        let decide = |weights: &Weights| {
            let decided = decide_at(
                &needing(Vec::new()),
                &[cool.clone(), hot.clone()],
                &loads,
                &[],
                now(),
                &mut first,
                weights,
            );
            placed(&decided.placement).map(str::to_owned)
        };
        assert_eq!(
            decide(&Weights::STARTING).as_deref(),
            Some("kestrel"),
            "what the sessions use already shows in its room"
        );
        assert_eq!(
            decide(&Weights::before_said()).as_deref(),
            Some("osprey"),
            "weighed as starts, they counted twice and sent the work to the pressed one"
        );
    }

    #[test]
    fn a_declared_fence_is_read_off_the_wire_and_none_can_be_declared() {
        let work: Work = serde_json::from_value(serde_json::json!({
            "needs": [], "fence": {"kind": "fence", "from": "spawn"},
        }))
        .expect("work");
        assert_eq!(
            work.fence,
            Some(DeclaredFence {
                kind: FenceKind::Fence,
                from: FenceFrom::Spawn,
            })
        );
        let unsaid: Work = serde_json::from_value(serde_json::json!({"needs": []})).expect("work");
        assert_eq!(unsaid.fence, None, "an older asker's work declares nothing");
        assert!(
            serde_json::to_value(&unsaid)
                .expect("said")
                .get("fence")
                .is_none(),
            "and says nothing of it to an older node"
        );
        // No start declares itself unfenced, nor in a fence this build
        // doesn't know, so each is passed over, judged as an older node
        // judges it, rather than refusing the start.
        let read = |fence: serde_json::Value| -> Option<DeclaredFence> {
            let work: Work =
                serde_json::from_value(serde_json::json!({"needs": [], "fence": fence}))
                    .expect("the work still reads");
            work.fence
        };
        for fence in [
            serde_json::json!({"kind": "none", "from": "quest"}),
            serde_json::json!({"kind": "sealed", "from": "spawn"}),
        ] {
            assert_eq!(read(fence.clone()), None, "{fence} is passed over");
        }
        // A source this build doesn't know moves nothing, so a strict
        // kind it names still raises, as it does with its source left
        // out, null or not a word.
        for fence in [
            serde_json::json!({"kind": "strict", "from": "orchestrator"}),
            serde_json::json!({"kind": "strict"}),
            serde_json::json!({"kind": "strict", "from": null}),
            serde_json::json!({"kind": "strict", "from": 5}),
        ] {
            assert_eq!(
                read(fence.clone()),
                Some(DeclaredFence {
                    kind: FenceKind::Strict,
                    from: FenceFrom::Unknown,
                }),
                "{fence} still raises"
            );
        }
    }

    #[test]
    fn weights_kept_before_running_was_said_weigh_a_running_session_as_a_start() {
        let kept: Weights = serde_json::from_str(r#"{"session": 0.5}"#).expect("weights");
        assert_eq!(kept.running, None, "a record's weights without it say none");
        let kestrel = machine("kestrel", "linux", 32, &[]);
        let weighed = |load| headroom(&kestrel, load, 0.0, true, &kept);
        let running = weighed(Load::default())
            - weighed(Load {
                sessions: 8,
                placed: 0,
            });
        assert!(
            (running - SESSION_WEIGHT).abs() < 1e-9,
            "and weigh each session running as a start, as placing did then: {running}"
        );
        let unsaid = serde_json::json!({"placement": {"refused": {"misses": []}}, "judged": []});
        let decision: Decision = serde_json::from_value(unsaid).expect("a decision");
        assert_eq!(
            decision.weights.running, None,
            "as does a decision kept before any weights were said"
        );
        assert_eq!(
            Weights::read(r#"{"session": 1.0}"#).map(|read| read.running),
            Ok(Some(RUNNING_WEIGHT)),
            "weights a replay reads take the starting one for it where left out"
        );
        assert_eq!(
            Weights::read(r#"{"running": 0.3}"#).map(|read| read.running),
            Ok(Some(0.3)),
            "and keep the one they name"
        );
        assert_eq!(
            Weights::read(r#"{"running": null}"#),
            Err("running is null, not a number".to_owned()),
            "a replay can't ask for the weighing of before by saying none"
        );
    }

    #[test]
    fn weights_read_as_json_start_from_the_starting_ones() {
        let read: Weights = serde_json::from_str(r#"{"session": 1.0}"#).expect("weights");
        assert_eq!(
            read,
            Weights {
                session: 1.0,
                running: None,
                ..Weights::STARTING
            }
        );
        let kept = serde_json::json!({"placement": {"refused": {"misses": []}}, "judged": []});
        let decision: Result<Decision, _> = serde_json::from_value(kept);
        assert!(
            decision.is_ok_and(|decision| decision.weights
                == Weights {
                    running: None,
                    ..Weights::STARTING
                }),
            "a decision kept before weights were said was weighed by the starting ones of then"
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
                saturated: None,
                cpu_minute: None,
                memory_full_minute: None,
                io_full_minute: None,
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

    /// A small machine made from an offer, as `machine` says it, holding
    /// `most` sessions at most by its own word where given.
    fn capped(name: &str, most: Option<u32>) -> Machine {
        let mut one = machine(name, "NixOS", 12, &["cloud"]);
        if let Some(declared) = one.declared.as_mut() {
            declared.sessions_at_most = most;
        }
        one
    }

    #[test]
    fn a_machine_holding_its_most_sessions_is_refused_for_it() {
        let machines = [capped("wren", Some(1)), capped("tern", Some(2))];
        let holding = BTreeMap::from([("wren".to_owned(), 1), ("tern".to_owned(), 1)]);
        assert_eq!(
            full(&machines, &holding, &Loads::new(), &BTreeMap::new()),
            vec![Miss {
                machine: "wren".to_owned(),
                because: vec![Because::Full(1)],
            }],
            "wren holds its one, while tern has room for a second"
        );
        let refused = full(&machines, &holding, &Loads::new(), &BTreeMap::new());
        let work = needing(vec![Need::Reach("cloud".to_owned())]);
        let decision = decide_refusing(&work, &machines, &Loads::new(), &refused);
        assert_eq!(
            placed(&decision.placement),
            Some("tern"),
            "the start goes where there is room"
        );
        let both = BTreeMap::from([("wren".to_owned(), 1), ("tern".to_owned(), 2)]);
        let refused = full(&machines, &both, &Loads::new(), &BTreeMap::new());
        assert!(
            matches!(
                decide_refusing(&work, &machines, &Loads::new(), &refused).placement,
                Placement::Refused { .. }
            ),
            "with both full it goes nowhere, so a machine is made for it"
        );
    }

    #[test]
    fn a_start_placed_and_not_yet_shown_counts_toward_a_machines_most() {
        let machines = [capped("wren", Some(1))];
        let loads = Loads::from([(
            "wren".to_owned(),
            Load {
                sessions: 0,
                placed: 1,
            },
        )]);
        assert_eq!(
            full(&machines, &BTreeMap::new(), &loads, &BTreeMap::new()).len(),
            1,
            "a burst of starts doesn't all land before the first shows"
        );
    }

    // KP0L6A (2026-11-23): S99IA2's second cloud slot was placed on a VM
    // promised one session, eight minutes after the first, whose fenced
    // pi said nothing of itself.
    #[test]
    fn a_session_its_node_holds_fills_a_machine_though_nothing_says_it_is_one() {
        let mut wren = capped("wren", None);
        wren.holds = Some(1);
        let promised = BTreeMap::from([("wren".to_owned(), 1)]);
        assert_eq!(
            full(&[wren.clone()], &BTreeMap::new(), &Loads::new(), &promised),
            vec![Miss {
                machine: "wren".to_owned(),
                because: vec![Because::Full(1)],
            }],
            "the one its node holds fills it, long after its start is forgotten"
        );
        let mut roomy = capped("wren", Some(2));
        roomy.holds = Some(1);
        let holding = BTreeMap::from([("wren".to_owned(), 1)]);
        assert!(
            full(&[roomy], &holding, &Loads::new(), &BTreeMap::new()).is_empty(),
            "a held session that says it is one is counted once, not twice"
        );
        wren.holds = None;
        assert!(
            full(&[wren], &BTreeMap::new(), &Loads::new(), &promised).is_empty(),
            "a node too old to say counts only the sessions known, as before"
        );
    }

    #[test]
    fn a_machine_kept_for_its_person_is_full_one_short_of_its_most() {
        let machines = [capped("wren", None), capped("tern", None)];
        let promised = BTreeMap::from([("wren".to_owned(), 4), ("tern".to_owned(), 4)]);
        let holding = BTreeMap::from([("wren".to_owned(), 3), ("tern".to_owned(), 3)]);
        let keeping = BTreeSet::from(["wren".to_owned()]);
        assert_eq!(
            full_keeping(&machines, &holding, &Loads::new(), &promised, &keeping),
            vec![Miss {
                machine: "wren".to_owned(),
                because: vec![Because::Full(3)],
            }],
            "its last is kept, though a machine not kept takes its fourth"
        );
        assert!(
            full(&machines, &holding, &Loads::new(), &promised).is_empty(),
            "and for its person, it takes the fourth"
        );
    }

    #[test]
    fn an_offers_promised_most_caps_a_machine_saying_none_of_its_own() {
        let machines = [capped("wren", None), capped("tern", Some(3))];
        let holding = BTreeMap::from([("wren".to_owned(), 1), ("tern".to_owned(), 1)]);
        let promised = BTreeMap::from([("wren".to_owned(), 1), ("tern".to_owned(), 1)]);
        assert_eq!(
            full(&machines, &holding, &Loads::new(), &promised),
            vec![Miss {
                machine: "wren".to_owned(),
                because: vec![Because::Full(1)],
            }],
            "wren is capped by its offer, and tern's own word comes before its offer's"
        );
        assert!(
            full(&machines, &holding, &Loads::new(), &BTreeMap::new()).is_empty(),
            "a machine nothing caps takes any number, as before"
        );
    }

    pub(crate) fn needing(needs: Vec<Need>) -> Work {
        Work {
            needs,
            prefers: Vec::new(),
            tolerates: Vec::new(),
            may_wait: false,
            lasts: None,
            fence: None,
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
        tolerant.tolerates = vec!["disk-filling".to_owned()];
        assert_eq!(
            placed(&place(&tolerant, &realm)),
            Some("skiff"),
            "unless the work doesn't mind"
        );
        let mut before = needing(Vec::new());
        before.tolerates = vec!["disk-pressure".to_owned()];
        assert_eq!(
            placed(&place(&before, &realm)),
            Some("skiff"),
            "nor does work minding it by the name it had before"
        );
        skiff.disk_band = None;
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[skiff, tern])),
            Some("skiff"),
            "a disk no guard bands is no reason to pass a machine by"
        );
    }

    #[test]
    fn a_disk_past_healthy_is_said_as_full_as_its_guard_says() {
        let mut skiff = machine("skiff", "linux", 30, &[]);
        for (band, said) in [
            (Band::Filling, "filling"),
            (Band::NearlyFull, "nearly full"),
            (Band::CriticallyFull, "critically full"),
        ] {
            skiff.disk_band = Some(band);
            let why = taints(&skiff)
                .into_iter()
                .find(|taint| taint.key == "disk-filling")
                .map(|taint| taint.why);
            assert_eq!(
                why.as_deref(),
                Some(format!("its disk guard says its disk is {said}").as_str()),
                "a disk {said} is said as {said}"
            );
        }
    }

    #[test]
    fn a_machine_whose_disk_is_critically_full_takes_no_work() {
        let mut skiff = machine("skiff", "linux", 30, &[]);
        skiff.disk_band = Some(Band::CriticallyFull);
        let lone = [skiff.clone()];
        assert_eq!(
            placed(&place(&needing(Vec::new()), &lone)),
            None,
            "work started on a full disk fails to write, so it waits instead"
        );
        let mut filling = needing(Vec::new());
        filling.tolerates = vec![DISK_FILLING.to_owned()];
        assert_eq!(
            placed(&place(&filling, &lone)),
            None,
            "minding no filling disk isn't minding no full one"
        );
        filling.tolerates = vec!["disk-pressure".to_owned()];
        assert_eq!(
            placed(&place(&filling, &lone)),
            None,
            "nor is it by the filling disk's key before"
        );
        let mut full = needing(Vec::new());
        full.tolerates = vec![DISK_FILLING.to_owned(), DISK_FULL.to_owned()];
        assert_eq!(
            placed(&place(&full, &lone)),
            Some("skiff"),
            "unless the work says it minds neither"
        );
        skiff.disk_band = Some(Band::NearlyFull);
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[skiff])),
            Some("skiff"),
            "a disk nearly full only weighs against its machine"
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
    fn a_saturated_machine_is_never_chosen_while_another_is_not() {
        let kestrel = pressed(machine("kestrel", "macos", 60, &[]), 85.0);
        let osprey = pressed(machine("osprey", "linux", 8, &[]), 40.0);
        let decision = decide_at(
            &needing(Vec::new()),
            &[kestrel.clone(), osprey.clone()],
            &Loads::new(),
            &[],
            now(),
            &mut first,
            &Weights::STARTING,
        );
        let chances: Vec<(&str, bool, f64)> = decision
            .judged
            .iter()
            .map(|one| (one.machine.as_str(), one.saturated, one.chance))
            .collect();
        assert_eq!(
            chances,
            [("kestrel", true, 0.0), ("osprey", false, 1.0)],
            "a saturated machine is never drawn beside one that isn't, however near its room"
        );
        let Placement::Placed {
            machine: chosen,
            passed,
        } = decision.placement
        else {
            panic!("one takes it");
        };
        assert_eq!(
            chosen, "osprey",
            "however much memory a machine has, processors saturated send work elsewhere"
        );
        assert_eq!(
            said(&passed),
            "kestrel would rather not: its processors are saturated, at pressure 85",
            "and it says why"
        );
        let busier = pressed(osprey.clone(), 90.0);
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[kestrel.clone(), busier])),
            Some("kestrel"),
            "every machine saturated, the work still goes to the one with most room"
        );
        let at_the_line = pressed(osprey.clone(), SATURATED_PRESSURE);
        assert_eq!(
            placed(&place(
                &needing(Vec::new()),
                &[kestrel.clone(), at_the_line]
            )),
            Some("kestrel"),
            "a machine at the line itself is saturated"
        );
        let over_only = over(osprey.clone(), &[Limit::Cpu]);
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[kestrel.clone(), over_only])),
            Some("osprey"),
            "a machine only over goes before one saturated"
        );
        let mut tolerant = needing(Vec::new());
        tolerant.tolerates.push(CPU_SATURATED.to_owned());
        assert_eq!(
            placed(&place(&tolerant, &[kestrel, osprey])),
            Some("kestrel"),
            "work that minds no saturation weighs it as any other room"
        );
    }

    /// `machine` kept lively at processor pressure `cpu` at most, with
    /// `memory_available` bytes kept available where given.
    fn lively(mut machine: Machine, cpu: u32, memory_available: Option<u64>) -> Machine {
        machine.declared.as_mut().expect("declared").kept_lively = Some(Lively {
            cpu,
            memory_available,
            memory_full: None,
            io_full: None,
            io_full_from: Vec::new(),
        });
        machine
    }

    #[test]
    fn a_machine_kept_lively_takes_work_only_after_every_other_heard_with_room() {
        // The one kept lively has far the most room, and is still passed.
        let kestrel = lively(pressed(machine("kestrel", "linux", 60, &[]), 5.0), 28, None);
        let osprey = pressed(machine("osprey", "macos", 8, &[]), 40.0);
        let mut agents = needing(Vec::new());
        agents.fence = Some(DeclaredFence {
            kind: FenceKind::Fence,
            from: FenceFrom::Spawn,
        });
        for work in [needing(Vec::new()), agents] {
            let decision = decide_as(
                &work,
                &[kestrel.clone(), osprey.clone()],
                &Loads::new(),
                &[],
                now(),
                &Weights::STARTING,
            );
            assert_eq!(
                placed(&decision.placement),
                Some("osprey"),
                "another machine with room goes first, for the person's work and an agent's"
            );
            let lively: Vec<(&str, bool, bool)> = decision
                .judged
                .iter()
                .map(|one| (one.machine.as_str(), one.kept_lively, one.over))
                .collect();
            assert_eq!(
                lively,
                [("kestrel", true, false), ("osprey", false, false)],
                "and its record says which was kept lively, within its ceiling"
            );
        }
        let decision = judged_now(
            &needing(Vec::new()),
            &[kestrel.clone(), over(osprey.clone(), &[Limit::Memory])],
        );
        assert_eq!(
            placed(&decision.placement),
            Some("kestrel"),
            "but one kept lively within its ceiling goes before one over a limit, swamped"
        );
        let small = lively(pressed(machine("wren", "linux", 2, &[]), 27.0), 28, None);
        let decision = judged_now(
            &needing(Vec::new()),
            &[small, over(osprey.clone(), &[Limit::Cpu])],
        );
        assert_eq!(
            placed(&decision.placement),
            Some("wren"),
            "by its rank, not its room, and before one over only its queue's processor limit"
        );
        assert!(
            placed_past_room(&decision),
            "and counts as past room, so the person's start goes to the cloud before it"
        );
        let decision = judged_now(
            &needing(Vec::new()),
            &[
                over(kestrel.clone(), &[Limit::Memory]),
                over(osprey.clone(), &[Limit::Memory]),
            ],
        );
        assert_eq!(
            placed(&decision.placement),
            Some("osprey"),
            "both over a limit, the one not kept lively goes first"
        );
        let saturated = pressed(osprey.clone(), 90.0);
        let decision = judged_now(&needing(Vec::new()), &[kestrel.clone(), saturated]);
        assert_eq!(
            placed(&decision.placement),
            Some("kestrel"),
            "but one kept lively within its ceiling goes before one saturated"
        );
        assert!(
            placed_past_room(&decision),
            "and counts as past room, so the person's start still goes to the cloud first"
        );
        let mut unheard = osprey.clone();
        unheard.room.as_mut().expect("a room").said_at = "2026-10-29T05:50:00Z".to_owned();
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[kestrel.clone(), unheard])),
            Some("kestrel"),
            "and before one unheard, whose room shows nothing"
        );
        let wren = lively(pressed(machine("wren", "linux", 60, &[]), 10.0), 28, None);
        let tern = pressed(machine("tern", "macos", 8, &[]), 60.0);
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[wren, tern, kestrel])),
            Some("tern"),
            "of two kept lively and one not, the one not goes first however pressed"
        );
    }

    #[test]
    fn a_machine_kept_lively_past_its_ceiling_goes_after_every_heard_but_never_refuses() {
        let kestrel = lively(
            pressed(machine("kestrel", "linux", 60, &[]), 30.0),
            28,
            None,
        );
        let osprey = pressed(machine("osprey", "macos", 8, &[]), 90.0);
        let decision = decide_as(
            &needing(Vec::new()),
            &[kestrel.clone(), osprey],
            &Loads::new(),
            &[],
            now(),
            &Weights::STARTING,
        );
        let Placement::Placed {
            machine: chosen,
            passed,
        } = &decision.placement
        else {
            panic!("one takes it");
        };
        assert_eq!(
            chosen, "osprey",
            "past its ceiling, it goes after one saturated"
        );
        assert_eq!(
            said(passed),
            "kestrel would rather not: it is kept lively, at processor pressure 28 at most, \
             and is at 30 over ten seconds",
            "and says why"
        );
        let alone = decide_as(
            &needing(Vec::new()),
            std::slice::from_ref(&kestrel),
            &Loads::new(),
            &[],
            now(),
            &Weights::STARTING,
        );
        assert_eq!(
            placed(&alone.placement),
            Some("kestrel"),
            "where nothing else takes it, it still does, rather than the start failing"
        );
        assert!(
            placed_past_room(&alone),
            "and is past its room, so the person's start may go to the cloud"
        );
        let short = lively(
            pressed(machine("kestrel", "linux", 12, &[]), 5.0),
            28,
            Some(16 << 30),
        );
        let decision = decide_as(
            &needing(Vec::new()),
            &[short],
            &Loads::new(),
            &[],
            now(),
            &Weights::STARTING,
        );
        assert!(
            placed_past_room(&decision),
            "with less memory available than it keeps, it is past its ceiling too"
        );
        let mut tolerant = needing(Vec::new());
        tolerant.tolerates.push(KEPT_LIVELY.to_owned());
        let decision = decide_as(
            &tolerant,
            std::slice::from_ref(&kestrel),
            &Loads::new(),
            &[],
            now(),
            &Weights::STARTING,
        );
        assert!(
            !placed_past_room(&decision),
            "work tolerating its ceiling counts it within its room"
        );
    }

    #[test]
    fn a_machine_kept_lively_is_judged_by_its_ceiling_and_tolerated_as_any() {
        let saturated = pressed(machine("osprey", "macos", 8, &[]), 90.0);
        let chosen_of = |one: Machine, other: &Machine, work: &Work| {
            let decision = judged_now(work, &[one, other.clone()]);
            (
                placed(&decision.placement).map(str::to_owned),
                said_of(&decision),
            )
        };
        let at = |cpu: f64| lively(pressed(machine("kestrel", "linux", 60, &[]), cpu), 28, None);
        let anything = needing(Vec::new());
        for (kestrel, why) in [
            (at(28.0), "at its ceiling it is within it"),
            (
                over(at(5.0), &[Limit::Cpu]),
                "over its queue's limit, within its ceiling",
            ),
        ] {
            let (chosen, _) = chosen_of(kestrel, &saturated, &anything);
            assert_eq!(
                chosen.as_deref(),
                Some("kestrel"),
                "{why}, before one saturated"
            );
        }
        let mut unheard = pressed(machine("osprey", "macos", 8, &[]), 40.0);
        unheard.room.as_mut().expect("a room").said_at = "2026-10-29T05:50:00Z".to_owned();
        let (chosen, _) = chosen_of(at(30.0), &unheard, &anything);
        assert_eq!(
            chosen.as_deref(),
            Some("kestrel"),
            "past its ceiling, it still goes before one unheard"
        );
        let keeping = |free| {
            lively(
                pressed(machine("kestrel", "linux", free, &[]), 5.0),
                28,
                Some(16 << 30),
            )
        };
        let (chosen, _) = chosen_of(keeping(20), &saturated, &anything);
        assert_eq!(
            chosen.as_deref(),
            Some("kestrel"),
            "with more memory available than it keeps, it is within its ceiling"
        );
        let osprey = pressed(machine("osprey", "macos", 8, &[]), 40.0);
        assert_eq!(
            chosen_of(keeping(12), &osprey, &anything),
            (
                Some("osprey".to_owned()),
                "kestrel would rather not: it is kept lively, with 16384 MiB available at \
                 least, and has 12288"
                    .to_owned()
            ),
            "with less, it is past it, and says why"
        );
        let mut tolerant = needing(Vec::new());
        tolerant.tolerates.push(KEPT_LIVELY.to_owned());
        let decision = judged_now(&tolerant, &[at(30.0), osprey]);
        assert_eq!(
            placed(&decision.placement),
            Some("kestrel"),
            "work tolerating its ceiling ranks it as any machine, roomiest first"
        );
        assert!(
            decision.judged.iter().all(|one| !one.kept_lively),
            "and records it as not kept lively"
        );
    }

    #[test]
    fn a_machine_kept_lively_is_judged_by_its_minute_where_its_node_says_one() {
        let saturated = pressed(machine("osprey", "macos", 8, &[]), 90.0);
        let anything = needing(Vec::new());
        let over_a_minute = |seconds: f64, minute: f64| {
            let mut kestrel = lively(
                pressed(machine("kestrel", "linux", 60, &[]), seconds),
                28,
                None,
            );
            kestrel.room.as_mut().expect("a room").cpu_minute = Some(minute);
            judged_now(&anything, &[kestrel, saturated.clone()])
        };
        let burst = over_a_minute(60.0, 20.0);
        assert_eq!(
            placed(&burst.placement),
            Some("kestrel"),
            "a burst of seconds over the ceiling, within it over a minute, is within it"
        );
        let steady = over_a_minute(5.0, 29.4);
        assert_eq!(
            placed(&steady.placement),
            Some("osprey"),
            "a minute past the ceiling is past it, however calm the last seconds"
        );
        assert_eq!(
            said_of(&steady),
            "kestrel would rather not: it is kept lively, at processor pressure 28 at most, \
             and is at 30 over a minute",
            "and says which it read"
        );
        assert_eq!(
            placed(&over_a_minute(60.0, 28.0).placement),
            Some("kestrel"),
            "a minute at the ceiling itself is within it"
        );
    }

    #[test]
    fn a_machine_kept_lively_is_past_its_ceiling_once_memory_stalls_all_its_work() {
        let saturated = pressed(machine("osprey", "macos", 8, &[]), 90.0);
        let anything = needing(Vec::new());
        let stalled = |most: Option<u32>, full: Option<f64>| {
            let mut kestrel = lively(pressed(machine("kestrel", "linux", 60, &[]), 5.0), 28, None);
            kestrel
                .declared
                .as_mut()
                .and_then(|said| said.kept_lively.as_mut())
                .expect("kept lively")
                .memory_full = most;
            kestrel.room.as_mut().expect("a room").memory_full_minute = full;
            judged_now(&anything, &[kestrel, saturated.clone()])
        };
        let past = stalled(Some(3), Some(3.04));
        assert_eq!(
            placed(&past.placement),
            Some("osprey"),
            "a minute past its memory ceiling is past it, however light its processors"
        );
        assert_eq!(
            said_of(&past),
            "kestrel would rather not: it is kept lively, at memory pressure 3 at most, \
             and is at 3.1 over a minute",
            "and says which it read"
        );
        assert_eq!(
            said_of(&stalled(Some(1), Some(1.1))),
            "kestrel would rather not: it is kept lively, at memory pressure 1 at most, \
             and is at 1.1 over a minute",
            "a tenth the kernel said reads as itself, never a tenth high"
        );
        for (most, full, why) in [
            (
                Some(3),
                Some(3.0),
                "a minute at the ceiling itself is within it",
            ),
            (
                Some(3),
                None,
                "a room saying no minute, as an older node's, is within it",
            ),
            (
                None,
                Some(40.0),
                "a machine holding to no memory ceiling is never past one",
            ),
        ] {
            assert_eq!(
                placed(&stalled(most, full).placement),
                Some("kestrel"),
                "{why}"
            );
        }
    }

    #[test]
    fn a_machine_kept_lively_is_past_its_ceiling_once_its_disks_stall_all_its_work() {
        let saturated = pressed(machine("osprey", "macos", 8, &[]), 90.0);
        let anything = needing(Vec::new());
        let stalled = |most: Option<u32>, full: Option<f64>| {
            let mut kestrel = lively(pressed(machine("kestrel", "linux", 60, &[]), 5.0), 28, None);
            kestrel
                .declared
                .as_mut()
                .and_then(|said| said.kept_lively.as_mut())
                .expect("kept lively")
                .io_full = most;
            let room = kestrel.room.as_mut().expect("a room");
            room.io_full_minute = full;
            // Memory stalls past where a memory ceiling would sit, which
            // a ceiling naming only the disks never reads.
            room.memory_full_minute = Some(40.0);
            judged_now(&anything, &[kestrel, saturated.clone()])
        };
        let past = stalled(Some(1), Some(8.64));
        assert_eq!(
            placed(&past.placement),
            Some("osprey"),
            "a minute past its disk ceiling is past it, however light its processors"
        );
        assert_eq!(
            said_of(&past),
            "kestrel would rather not: it is kept lively, at disk pressure 1 at most, \
             and is at 8.7 over a minute",
            "and says which it read"
        );
        for (most, full, why) in [
            (
                Some(1),
                Some(1.0),
                "a minute at the ceiling itself is within it",
            ),
            (
                Some(1),
                None,
                "a room saying no minute, as an older node's or a Mac's, is within it",
            ),
            (
                None,
                Some(40.0),
                "a machine holding to no disk ceiling is never past one",
            ),
        ] {
            assert_eq!(
                placed(&stalled(most, full).placement),
                Some("kestrel"),
                "{why}"
            );
        }
    }

    #[test]
    fn a_room_made_for_agents_takes_work_naming_no_worker_ahead_of_a_machine_kept_lively() {
        let kestrel = lively(
            pressed(machine("kestrel", "linux", 60, &[]), 20.0),
            28,
            None,
        );
        let mut wren = machine("wren", "linux", 30, &[]);
        wren.declared.as_mut().expect("declared").costs_money = true;
        let anything = needing(Vec::new());
        let chosen_of = |wren: &Machine| {
            placed(&judged_now(&anything, &[kestrel.clone(), wren.clone()]).placement)
                .map(str::to_owned)
        };
        assert_eq!(
            chosen_of(&wren).as_deref(),
            Some("kestrel"),
            "a machine that costs money, not made for agents, takes none of it"
        );
        wren.held_for = HeldFor::Agents;
        assert_eq!(
            chosen_of(&wren).as_deref(),
            Some("wren"),
            "a room made for agents takes it, ahead of a machine kept lively"
        );
        assert_eq!(
            placed(&judged_now(&needing(vec![Need::Cloud]), &[kestrel.clone(), wren]).placement),
            Some("wren"),
            "and is in the cloud still"
        );
    }

    /// How `work` is decided among `machines`, as the tests' clock says now.
    fn judged_now(work: &Work, machines: &[Machine]) -> Decision {
        decide_as(
            work,
            machines,
            &Loads::new(),
            &[],
            now(),
            &Weights::STARTING,
        )
    }

    /// Why each machine `decision` passed was passed, as [`said`] says it.
    fn said_of(decision: &Decision) -> String {
        match &decision.placement {
            Placement::Placed { passed, .. } => said(passed),
            Placement::Pending { misses } | Placement::Refused { misses } => said(misses),
        }
    }

    #[test]
    fn saturation_holds_through_the_band_between_its_lines() {
        let band = Saturation::default();
        let judged: Vec<bool> = [84.0, 85.0, 80.0, 75.0, 74.0, 84.0, 85.0]
            .into_iter()
            .map(|cpu| band.judge(cpu))
            .collect();
        assert_eq!(
            judged,
            [false, true, true, true, false, false, true],
            "saturated at 85 or more, and saturated still until under 75"
        );
        assert!(
            band.judge(f64::NAN),
            "a pressure that is no number changes nothing"
        );
    }

    #[test]
    fn a_node_says_its_verdict_in_its_room_unless_its_host_did() {
        let room = |cpu: f64, saturated: Option<bool>| {
            let mut machine = pressed(machine("kestrel", "macos", 60, &[]), cpu);
            let mut room = machine.room.take().expect("a room");
            room.saturated = saturated;
            room
        };
        let band = Saturation::default();
        let mut hot = room(90.0, None);
        band.say(Some(&mut hot));
        assert_eq!(hot.saturated, Some(true), "its own room says its verdict");
        let mut eased = room(80.0, None);
        band.say(Some(&mut eased));
        assert_eq!(eased.saturated, Some(true), "and holds it through the band");
        let mut hosts = room(80.0, Some(false));
        band.say(Some(&mut hosts));
        assert_eq!(
            hosts.saturated,
            Some(false),
            "a host's verdict told to a machine within it stands"
        );
        let mut own = room(80.0, None);
        band.say(Some(&mut own));
        assert_eq!(
            own.saturated,
            Some(false),
            "and forgets its own, so its own after the host's isn't judged by one from before"
        );
        band.say(Some(&mut room(90.0, Some(true))));
        let mut quiet = room(80.0, None);
        band.say(Some(&mut quiet));
        assert_eq!(
            quiet.saturated,
            Some(false),
            "nor takes the host's verdict for its own once the host goes quiet"
        );
        band.say(Some(&mut room(90.0, None)));
        band.say(None);
        let mut after = room(80.0, None);
        band.say(Some(&mut after));
        assert_eq!(
            after.saturated,
            Some(false),
            "a measure with no room forgets the verdict, so one after a \
             silence isn't judged by one from before it"
        );
    }

    #[test]
    fn a_room_says_whether_it_is_saturated_over_its_pressure() {
        let unjudged = pressed(machine("kestrel", "macos", 60, &[]), 78.0);
        let osprey = pressed(machine("osprey", "linux", 8, &[]), 40.0);
        assert_eq!(
            placed(&place(
                &needing(Vec::new()),
                &[unjudged.clone(), osprey.clone()]
            )),
            Some("kestrel"),
            "under the line, a room that says nothing isn't saturated"
        );
        let mut held = unjudged;
        held.room.as_mut().expect("a room").saturated = Some(true);
        let Placement::Placed {
            machine: chosen,
            passed,
        } = place(&needing(Vec::new()), &[held.clone(), osprey.clone()])
        else {
            panic!("one takes it");
        };
        assert_eq!(
            chosen, "osprey",
            "a machine its node still judges saturated goes after, under where it saturates"
        );
        assert_eq!(
            said(&passed),
            "kestrel would rather not: its processors are saturated still, at pressure 78, \
             until it falls under 75",
            "and it says why"
        );
        let mut hot = pressed(machine("kestrel", "macos", 60, &[]), 85.0);
        hot.room.as_mut().expect("a room").saturated = Some(true);
        let Placement::Placed { passed, .. } = place(&needing(Vec::new()), &[hot, osprey.clone()])
        else {
            panic!("one takes it");
        };
        assert_eq!(
            said(&passed),
            "kestrel would rather not: its processors are saturated, at pressure 85",
            "from where it saturates, it is saturated, not still"
        );
        let mut eased = pressed(machine("kestrel", "macos", 60, &[]), 85.0);
        eased.room.as_mut().expect("a room").saturated = Some(false);
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[eased, osprey.clone()])),
            Some("kestrel"),
            "a room its node says isn't saturated isn't, whatever its pressure"
        );
        let unsaid = pressed(machine("kestrel", "macos", 60, &[]), 82.0);
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[unsaid, osprey])),
            Some("osprey"),
            "a room from a node too old to say is judged by the line"
        );
    }

    #[test]
    fn a_machine_unheard_goes_after_one_heard_saturated() {
        let kestrel = pressed(machine("kestrel", "macos", 60, &[]), 90.0);
        let mut osprey = machine("osprey", "linux", 60, &[]);
        osprey.room = None;
        let Placement::Placed {
            machine: chosen,
            passed,
        } = place(&needing(Vec::new()), &[kestrel.clone(), osprey])
        else {
            panic!("one takes it");
        };
        assert_eq!(
            chosen, "kestrel",
            "a room saturated but heard says more than one never said"
        );
        assert_eq!(passed[0].because, [Because::Unheard], "osprey says why");

        let tern = pressed(machine("tern", "linux", 2, &[]), 75.0);
        let mut ahead = kestrel.clone();
        ahead.room.as_mut().expect("a room").said_at = "2026-10-29T06:10:30Z".to_owned();
        assert_eq!(
            placed(&place(&needing(Vec::new()), &[ahead, tern.clone()])),
            Some("tern"),
            "a room dated ten minutes ahead is no more heard than one ten behind"
        );
        let mut stale = kestrel;
        stale.room.as_mut().expect("a room").said_at = "2026-10-29T05:50:00Z".to_owned();
        let decision = decide_at(
            &needing(Vec::new()),
            &[stale, tern],
            &Loads::new(),
            &[],
            now(),
            &mut first,
            &Weights::STARTING,
        );
        assert_eq!(placed(&decision.placement), Some("tern"));
        assert!(
            !decision.judged[0].saturated,
            "a room said ten minutes ago says nothing of its processors now"
        );
        let Placement::Placed { passed, .. } = decision.placement else {
            panic!("tern takes it");
        };
        assert_eq!(
            passed[0].because,
            [Because::Unheard],
            "and is passed for being unheard, not for an old pressure"
        );
    }

    #[test]
    fn a_machine_over_a_limit_is_never_chosen_while_another_is_not() {
        let kestrel = over(
            pressed(machine("kestrel", "macos", 60, &[]), 75.0),
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
        let tern = pressed(machine("tern", "linux", 2, &[]), 75.0);
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
        // Crowding kestrel, so the tie its name settles can't pass for it.
        let crowded = machine("kestrel", "linux", 32, &[]);
        let loads = Loads::from([(
            "kestrel".to_owned(),
            Load {
                sessions: 8,
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
            Some("skiff"),
            "the fewer sessions"
        );

        let mut filling = machine("tern", "linux", 56, &[]);
        filling.disk_band = Some(Band::Filling);
        let healthy = pressed(machine("osprey", "linux", 4, &[]), 75.0);
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
        // Pressed short of saturated, so the two rank in one tier and
        // only their headroom tells them apart.
        busy.cpu.busy = Some(0.5);
        let pressed_quiet = pressed(machine("skiff", "linux", 32, &[]), 79.0);
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
            &[],
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
        tern.disk_band = Some(Band::NearlyFull);
        assert_eq!(
            placed(&place(
                &needing(Vec::new()),
                &[osprey.clone(), tern.clone()]
            )),
            Some("osprey"),
            "a disk nearly full costs as any taint that would rather not"
        );
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
    fn a_start_its_machine_refused_counts_there_no_longer() {
        // Kestrel's room shows the first start, not the second.
        let mut kestrel = machine("kestrel", "linux", 32, &[]);
        kestrel.room.as_mut().expect("a room").said_at = "2026-10-29T06:00:40Z".to_owned();
        let skiff = machine("skiff", "linux", 32, &[]);
        let first: jiff::Timestamp = "2026-10-29T06:00:05Z".parse().expect("a time");
        let second = first + jiff::SignedDuration::from_secs(45);
        let mut starts = Starts::default();
        starts.placed("kestrel", first);
        starts.placed("kestrel", second);
        starts.placed("skiff", first);
        starts.unplaced("kestrel", first);
        starts.unplaced("tern", first);
        starts.unplaced("skiff", second);
        let later = second + jiff::SignedDuration::from_secs(10);
        let loads = starts.loads(&[kestrel, skiff], [], later);
        assert_eq!(
            loads["kestrel"].placed, 1,
            "the refused start is forgotten, the one its room doesn't show still counts"
        );
        assert_eq!(
            loads["skiff"].placed, 1,
            "a start never placed at that time forgets nothing"
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
        kestrel.room.as_mut().expect("a room").said_at = "2026-10-29T06:00:09Z".to_owned();
        assert_eq!(
            starts.loads(std::slice::from_ref(&kestrel), sessions, later)["kestrel"].placed,
            1,
            "a room said seconds after it is too soon to show a session begun"
        );
        kestrel.room.as_mut().expect("a room").said_at = "2026-10-29T06:00:35Z".to_owned();
        assert_eq!(
            starts.loads(std::slice::from_ref(&kestrel), sessions, later)["kestrel"].placed,
            0,
            "and no longer once its room is said long enough after it"
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
    fn a_machine_refused_still_counts_where_the_persons_machines_reach() {
        let mut cloud = machine("cloud", "linux", 60, &["home", "internet"]);
        cloud.declared.as_mut().expect("declared").costs_money = true;
        let realm = [
            machine("kestrel", "linux", 2, &["internet"]),
            machine("osprey", "linux", 60, &["home", "internet"]),
            cloud,
        ];
        let looser = Miss {
            machine: "osprey".to_owned(),
            because: vec![Because::Refuses(Taint {
                key: LOOSER_GROUND.to_owned(),
                effect: Effect::Never,
                why: "its ground is looser".to_owned(),
            })],
        };
        let mut tolerant = needing(vec![Need::Reach("home".to_owned())]);
        tolerant.tolerates.push(LOOSER_GROUND.to_owned());
        let Placement::Refused { misses } =
            decide_refusing(&tolerant, &realm, &Loads::new(), &[looser]).placement
        else {
            panic!("nowhere takes it");
        };
        let said = said(&misses);
        assert!(
            said.contains("osprey refuses: its ground is looser"),
            "the refused machine says why, whatever the work tolerates: {said}"
        );
        assert!(
            said.contains("cloud refuses"),
            "and a worker still refuses work for a network the person's refused machine \
             reaches: {said}"
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

    // Work placed past every heard machine's room is placed where a
    // machine made for it would have more; not where one has room, nor
    // where nothing heard says.
    #[test]
    fn work_is_placed_past_room_only_where_every_heard_machine_is_past_it() {
        let work = needing(Vec::new());
        let decided = |machines: &[Machine]| {
            decide_as(
                &work,
                machines,
                &Loads::new(),
                &[],
                now(),
                &Weights::STARTING,
            )
        };
        let past = |machines: &[Machine]| placed_past_room(&decided(machines));
        let room = |machines: &[Machine]| placed_with_room(&decided(machines));
        let mut over = pressed(machine("kestrel", "linux", 60, &[]), 20.0);
        over.room.as_mut().expect("a room").over = Some(vec![Limit::Cpu]);
        let saturated = pressed(machine("osprey", "linux", 60, &[]), 95.0);
        let roomy = pressed(machine("tern", "linux", 60, &[]), 20.0);
        assert!(past(&[over.clone()]), "over its room");
        assert!(past(std::slice::from_ref(&saturated)), "saturated");
        assert!(past(&[over.clone(), saturated]), "both past it");
        assert!(!past(&[over.clone(), roomy.clone()]), "one has room");
        assert!(room(&[over.clone(), roomy]), "and takes it");
        // Silent, under another name, and not over, so only its silence
        // keeps it from reading as room.
        let mut unheard = pressed(machine("wren", "linux", 60, &[]), 20.0);
        unheard.room.as_mut().expect("a room").said_at = "2026-10-28T06:00:00Z".to_owned();
        assert!(
            !past(std::slice::from_ref(&unheard)),
            "nothing heard lately says so"
        );
        assert!(
            !room(std::slice::from_ref(&unheard)),
            "nor that it has room"
        );
        assert!(
            past(&[over.clone(), unheard.clone()]),
            "one heard past its room goes before one silent"
        );
        assert!(
            !room(&[over.clone(), unheard]),
            "and silence never reads as room"
        );
        assert!(!room(&[over]), "over its room");
        assert!(!past(&[]) && !room(&[]), "placed nowhere");
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

        work.lasts = Some(jiff::SignedDuration::from_hours(2));
        assert_eq!(
            to_make(&work, &offers, &[]).map(|chosen| chosen.shape.as_str()),
            Ok("large"),
            "not one that would end before the work does, and one living exactly as long will do"
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
    fn work_needing_the_cloud_goes_only_to_a_machine_that_costs_money() {
        let mut made = machine("wren", "linux", 4, &["work"]);
        made.declared.as_mut().expect("declared").costs_money = true;
        let realm = [machine("skiff", "linux", 30, &["work"]), made];
        let cloud = needing(vec![Need::Cloud]);
        let Placement::Placed { machine, passed } = place(&cloud, &realm) else {
            panic!("a machine that costs money is in the cloud")
        };
        assert_eq!(
            machine, "wren",
            "to the one that costs money, though skiff has more room"
        );
        assert_eq!(
            said(&passed),
            "skiff isn't a cloud machine",
            "the person's own machine isn't the cloud"
        );

        assert_eq!(
            placed(&place(&needing(Vec::new()), &realm)),
            Some("skiff"),
            "work that says nothing still never goes to one that costs money"
        );
    }

    #[test]
    fn work_needing_the_cloud_is_made_a_machine_naming_no_network() {
        let offers = [offer("large", "1.16", Some(43_200), &["work"])];
        let mut work = needing(vec![Need::Cloud]);
        assert_eq!(
            to_make(&work, &offers, &[]).map(|chosen| chosen.shape.as_str()),
            Ok("large"),
            "whatever a provider makes is in the cloud, and the need names it"
        );

        work.lasts = Some(jiff::SignedDuration::from_hours(12));
        assert_eq!(
            to_make(&work, &offers, &[]).map(|chosen| chosen.shape.as_str()),
            Ok("large"),
            "one that lives exactly as long as the work will do"
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
    fn a_tool_is_met_only_where_the_realms_kit_holds_it() {
        let kit = |tools: &[&str]| facts::Kit {
            path: "/nix/store/kit".to_owned(),
            tools: tools.iter().map(|&tool| tool.to_owned()).collect(),
            refused: None,
        };
        let mut carrying = machine("kestrel", "linux", 8, &[]);
        // Out of order, as a record read from the wire may be.
        carrying.kit = Some(kit(&["git", "gh", "gcloud"]));
        let mut lacking = machine("osprey", "linux", 32, &[]);
        lacking.kit = Some(kit(&["gh", "git"]));
        let unsaid = machine("tern", "linux", 32, &[]);
        let gcloud = || needing(vec![Need::Tool("gcloud".to_owned())]);
        let Placement::Placed { machine, passed } =
            place(&gcloud(), &[lacking.clone(), unsaid.clone(), carrying])
        else {
            panic!("the machine whose kit holds it takes it")
        };
        assert_eq!(machine, "kestrel", "for all it has less room");
        assert_eq!(
            passed,
            [
                Miss {
                    machine: "osprey".to_owned(),
                    because: vec![Because::Lacks(Need::Tool("gcloud".to_owned()))],
                },
                Miss {
                    machine: "tern".to_owned(),
                    because: vec![Because::Unknown(Need::Tool("gcloud".to_owned()))],
                },
            ],
            "a kit without it lacks it, and no kit hasn't said"
        );
        assert_eq!(
            Because::Lacks(Need::Tool("gcloud".to_owned())).to_string(),
            "lacks tool gcloud",
            "a miss says it in words"
        );
        assert_eq!(
            serde_json::to_value(Need::Tool("gcloud".to_owned())).expect("a need says itself"),
            serde_json::json!({"tool": "gcloud"}),
            "as PROTOCOL.md says it"
        );
    }

    #[test]
    fn work_asked_of_its_chosen_machine_prefers_no_tool_but_still_needs_one() {
        let gcloud = Need::Tool("gcloud".to_owned());
        let linux = Need::System("linux".to_owned());
        let work = Work {
            prefers: vec![gcloud.clone(), linux.clone()],
            ..needing(vec![gcloud.clone()])
        };
        let there = work.asked_there();
        assert_eq!(
            there.prefers,
            [linux],
            "a node too old to read a tool may have been preferred"
        );
        assert_eq!(
            there.needs,
            [gcloud],
            "only a node naming its kit can meet a need for a tool"
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
