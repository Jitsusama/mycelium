//! How evenly the realm's machines carry its work, as `mycelium balance`
//! says it: each machine's load and whether it is over, how far the loads
//! spread, and the flags that say placement went wrong, read from what
//! every node already holds, so every node says the same.
//!
//! A machine's load is its dominant share, the largest of the memory it
//! uses, the share of its processors it spends and how long its work
//! stalls, each from 0 to 1, as Dominant Resource Fairness weighs a share.
//! Its processors count by how busy its cores are where its node says so,
//! and by pressure where that is more or the node doesn't say, since
//! pressure is zero until every core is busy. A machine is over past its
//! queue's memory limit, as placement counts it, or at
//! [`OVER_CPU_PRESSURE`]. A queue keeping a build from a VM test for want
//! of a processor is neither over nor has room: placement places sessions
//! and runs, never builds, so it may still place there, but would rather
//! not. [`MEASURES`] names this definition, `balance/3`; `balance/2`
//! called that queue over, `balance/1` counted the processors by pressure
//! alone, and a better one will be a new name, never this one changed.
//!
//! A reading flags, at once, a machine over while another has room (what
//! placement exists to prevent), a machine stranded (one resource spent,
//! the other idle), and facts too old to place by. Over time, a [`Watch`]
//! holds a flag once it is seen in most of the last readings and lets it
//! go only once it is seen in few, so a flag neither flickers nor lingers.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::facts::{Machine, Readiness};
use crate::record::{Session, State};
use crate::room::Limit;

mod shown;

pub use shown::said;

/// The name of these measures' definitions, said in every reading.
pub const MEASURES: &str = "balance/3";

/// Processor pressure, in percent, at which a machine is over: the
/// harness queue's own line, past which it starts no build beside a VM
/// test.
pub const OVER_CPU_PRESSURE: f64 = 70.0;

/// Memory pressure, in percent, at which no job starts in the harness
/// queue, so a memory stall of 1.
pub const OVER_MEMORY_PRESSURE: f64 = 10.0;

/// The most a machine with room may stall, 0 to 1.
pub const ROOM_STALL: f64 = 0.25;

/// The most of its processors a machine with room may spend, 0 to 1: a
/// quarter of its cores free.
pub const ROOM_PROCESSOR: f64 = 0.75;

/// The most memory a machine with room may use, 0 to 1.
pub const ROOM_MEMORY: f64 = 0.8;

/// Under this share of its memory used, a machine whose processors are
/// over is stranded: the memory can't be used for want of processors.
pub const STRANDED_MEMORY: f64 = 0.5;

/// From this share of its memory used, a machine whose processors idle
/// is stranded the other way: the processors can't be used for want of
/// memory.
pub const FULL_MEMORY: f64 = 0.95;

/// How old a machine's facts may be before placing by them is placing
/// blind: a link says them each minute, and the queue's room behind them
/// is up to 30 seconds older, so two and a half minutes is one missed.
pub const STALE_SECONDS: i64 = 150;

/// The spread of loads, as their standard deviation, past which the realm
/// is out of balance.
pub const IMBALANCE: f64 = 0.2;

/// How many readings a [`Watch`] judges a flag over.
pub const WINDOW: usize = 10;

/// In how many of the last [`WINDOW`] readings a flag must be seen to be
/// held.
pub const HELD_FROM: usize = 8;

/// In how few of the last [`WINDOW`] readings a held flag must be seen to
/// be let go.
pub const CLEARED_AT: usize = 2;

/// One reading of the realm's balance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    /// Which definitions these are.
    pub measures: String,
    /// When it was read, as an RFC 3339 time.
    pub at: String,
    /// Each machine, by name.
    pub machines: Vec<Load>,
    /// The standard deviation of the loads of machines that take work,
    /// none while fewer than two have a load known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imbalance: Option<f64>,
    /// What is wrong now.
    pub flags: Vec<Flag>,
    /// What a [`Watch`] holds wrong over the last readings, where one
    /// follows them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<Vec<Flag>>,
}

/// One machine's load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Load {
    /// The machine.
    pub machine: String,
    /// Whether it takes work.
    pub ready: bool,
    /// The share of its memory in use, 0 to 1, where known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<f64>,
    /// How long its work stalls, 0 to 1: processor pressure over 100 or
    /// memory pressure over the queue's line, whichever is more.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stall: Option<f64>,
    /// How much of its processors are spent, 0 to 1: the more of its
    /// cores' use and its processor pressure over 100, or pressure alone
    /// where its node doesn't say its use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processor: Option<f64>,
    /// Its dominant share: the largest of `memory`, `processor` and
    /// `stall`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load: Option<f64>,
    /// Whether it is over: its queue says it is past its memory limit, or
    /// its processor pressure is at [`OVER_CPU_PRESSURE`].
    pub over: bool,
    /// Whether it has room for more: not over, its queue not short of a
    /// processor, stalling under [`ROOM_STALL`], its processors under
    /// [`ROOM_PROCESSOR`] spent and using under [`ROOM_MEMORY`] of its
    /// memory.
    pub room: bool,
    /// Its sessions running a turn.
    pub busy: usize,
    /// Its sessions waiting for something to do.
    pub idle: usize,
    /// Its busy sessions for each of its cores, where its cores are known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub busy_per_core: Option<f64>,
    /// How old its facts are, in seconds, where they say when.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age: Option<i64>,
}

/// Something wrong with how work is placed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "flag")]
pub enum Flag {
    /// `hot` is over while `cool` has room: what placement is for.
    HotWhileCool {
        /// The machine over.
        hot: String,
        /// A machine with room.
        cool: String,
    },
    /// One of `machine`'s resources is spent while the other idles.
    Stranded {
        /// The machine.
        machine: String,
        /// The resource spent: `processor` or `memory`.
        spent: String,
    },
    /// `machine`'s facts are too old to place by.
    Stale {
        /// The machine.
        machine: String,
    },
    /// The realm's loads spread past [`IMBALANCE`].
    Imbalanced,
}

/// Reads the realm's balance from `machines` and `sessions` as of `now`.
#[must_use]
pub fn read(machines: &[Machine], sessions: &[Session], now: jiff::Timestamp) -> Reading {
    let loads: Vec<Load> = machines
        .iter()
        .map(|machine| load(machine, sessions, now))
        .collect();
    let known: Vec<f64> = loads
        .iter()
        .filter(|one| one.ready)
        .filter_map(|one| one.load)
        .collect();
    let imbalance = (known.len() >= 2).then(|| deviation(&known));
    let mut flags = Vec::new();
    for hot in loads.iter().filter(|one| one.ready && one.over) {
        for cool in loads.iter().filter(|one| one.ready && one.room) {
            flags.push(Flag::HotWhileCool {
                hot: hot.machine.clone(),
                cool: cool.machine.clone(),
            });
        }
    }
    for one in loads.iter().filter(|one| one.ready) {
        if let Some(spent) = stranded(one, machines) {
            flags.push(Flag::Stranded {
                machine: one.machine.clone(),
                spent: spent.to_owned(),
            });
        }
        if one.age.is_some_and(|age| age > STALE_SECONDS) {
            flags.push(Flag::Stale {
                machine: one.machine.clone(),
            });
        }
    }
    if imbalance.is_some_and(|spread| spread > IMBALANCE) {
        flags.push(Flag::Imbalanced);
    }
    Reading {
        measures: MEASURES.to_owned(),
        at: now.to_string(),
        machines: loads,
        imbalance,
        flags,
        held: None,
    }
}

fn load(machine: &Machine, sessions: &[Session], now: jiff::Timestamp) -> Load {
    let room = machine.room.as_ref();
    let memory = room
        .zip(machine.memory.filter(|&total| total > 0))
        .map(|(room, total)| {
            #[expect(clippy::cast_precision_loss, reason = "bytes well under 2^52")]
            let share = 1.0 - room.memory_available as f64 / total as f64;
            share.clamp(0.0, 1.0)
        });
    let stall = room.map(|room| {
        (room.pressure.cpu / 100.0)
            .max(room.pressure.memory / OVER_MEMORY_PRESSURE)
            .clamp(0.0, 1.0)
    });
    let processor = room.map(|room| {
        let pressed = (room.pressure.cpu / 100.0).clamp(0.0, 1.0);
        machine.cpu.busy.map_or(pressed, |busy| busy.max(pressed))
    });
    let load = [memory, processor, stall]
        .into_iter()
        .flatten()
        .reduce(f64::max);
    let past = |limit: Limit| {
        room.and_then(|room| room.over.as_ref())
            .is_some_and(|over| over.contains(&limit))
    };
    let over =
        past(Limit::Memory) || room.is_some_and(|room| room.pressure.cpu >= OVER_CPU_PRESSURE);
    // A queue short of a processor for a build isn't over for the sessions
    // and runs placement places, but placement would rather not, so it has
    // no room either: nothing is eased onto it or flagged cool beside it.
    let has_room = !over
        && !past(Limit::Cpu)
        && stall.is_some_and(|stall| stall < ROOM_STALL)
        && processor.is_some_and(|processor| processor < ROOM_PROCESSOR)
        && memory.is_some_and(|memory| memory < ROOM_MEMORY);
    let count = |state: State| {
        sessions
            .iter()
            .filter(|one| one.machine == machine.name && one.state == state)
            .count()
    };
    let busy = count(State::Busy);
    #[expect(clippy::cast_precision_loss, reason = "session counts are small")]
    let busy_per_core = machine
        .cpu
        .cores
        .filter(|&cores| cores > 0)
        .map(|cores| busy as f64 / f64::from(cores));
    let age = machine
        .measured_at
        .parse::<jiff::Timestamp>()
        .ok()
        .map(|measured| now.duration_since(measured).as_secs());
    Load {
        machine: machine.name.clone(),
        ready: machine.state == Readiness::Ready,
        memory,
        stall,
        processor,
        load,
        over,
        room: has_room,
        busy,
        idle: count(State::Idle),
        busy_per_core,
        age,
    }
}

/// The resource `one` has spent while the other idles, if any.
fn stranded(one: &Load, machines: &[Machine]) -> Option<&'static str> {
    let memory = one.memory?;
    let cpu = machines
        .iter()
        .find(|machine| machine.name == one.machine)
        .and_then(|machine| machine.room.as_ref())
        .map(|room| room.pressure.cpu)?;
    if cpu >= OVER_CPU_PRESSURE && memory < STRANDED_MEMORY {
        Some("processor")
    } else if memory >= FULL_MEMORY && one.processor.unwrap_or(cpu / 100.0) < ROOM_STALL {
        Some("memory")
    } else {
        None
    }
}

/// The population standard deviation of `values`.
fn deviation(values: &[f64]) -> f64 {
    #[expect(clippy::cast_precision_loss, reason = "a handful of machines")]
    let count = values.len() as f64;
    let mean = values.iter().sum::<f64>() / count;
    (values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / count)
        .sqrt()
}

/// Readings over time, holding each flag seen in most of the last
/// [`WINDOW`] and letting it go once seen in few.
#[derive(Debug, Default)]
pub struct Watch {
    seen: VecDeque<Vec<Flag>>,
    held: BTreeSet<Flag>,
}

impl Watch {
    /// Takes `reading` in, answering the flags held now.
    pub fn take(&mut self, reading: &Reading) -> Vec<Flag> {
        self.seen.push_back(reading.flags.clone());
        while self.seen.len() > WINDOW {
            self.seen.pop_front();
        }
        let mut counts: BTreeMap<&Flag, usize> = BTreeMap::new();
        for flags in &self.seen {
            for flag in flags {
                *counts.entry(flag).or_default() += 1;
            }
        }
        self.held = counts
            .into_iter()
            .filter(|(flag, count)| {
                *count >= HELD_FROM || (self.held.contains(*flag) && *count > CLEARED_AT)
            })
            .map(|(flag, _)| flag.clone())
            .collect();
        self.held.iter().cloned().collect()
    }
}

/// The file in the balance folder holding the latest reading whole, for a
/// node to serve as a kind of its own.
pub const NOW_FILE: &str = "now.json";

/// Keeps `reading` in `folder` as a line of the day's file, `<day>.jsonl`,
/// which hindsight reads.
///
/// # Errors
///
/// When the day's file can't be written.
pub fn keep(folder: &Path, reading: &Reading, day: jiff::civil::Date) -> io::Result<()> {
    let mut line = serde_json::to_vec(reading).map_err(io::Error::other)?;
    line.push(b'\n');
    // Appended in one write, so two writers' lines can't interleave.
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(folder.join(format!("{day}.jsonl")))?
        .write_all(&line)
}

/// Keeps `reading` whole in `folder`'s [`NOW_FILE`], for a node to serve:
/// written to a file of this process's own, made new so no link there is
/// followed, synced, then renamed into place, so a reader never sees half
/// a reading, even with two followers on one folder or a crash between.
/// It stays once readings stop, so a reader judges how fresh it is by its
/// `at`.
///
/// # Errors
///
/// When it can't be written; nothing is left beside it then.
pub fn keep_latest(folder: &Path, reading: &Reading) -> io::Result<()> {
    let whole = serde_json::to_vec(reading).map_err(io::Error::other)?;
    let writing = folder.join(format!("{NOW_FILE}.{}.new", std::process::id()));
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&writing)
        .and_then(|mut file| {
            file.write_all(&whole)?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&writing, folder.join(NOW_FILE)));
    if written.is_err() {
        // Best effort: what is left is only a staging file, and the error
        // that made it is the one worth saying.
        let _ = fs::remove_file(&writing);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{self, Sources};
    use crate::room::{Limit, Pressure, Room, Takes};
    use serde_json::json;

    const NOW: &str = "2026-11-15T23:39:00Z";

    fn now() -> jiff::Timestamp {
        NOW.parse().expect("a time")
    }

    /// A ready machine of `cores` and `gib` GiB, with `free` GiB available
    /// and processor pressure `cpu`, measured `age` seconds ago.
    fn machine(name: &str, cores: u32, gib: u64, free: u64, cpu: f64, age: i64) -> Machine {
        let nowhere = Sources {
            home: "/".into(),
            queue: None,
            disk_guard: None,
            declared: None,
            reached: "/nowhere".into(),
            updated: "/nowhere".into(),
        };
        let mut machine = facts::measure(name, &nowhere);
        machine.state = Readiness::Ready;
        machine.cpu.cores = Some(cores);
        // As this machine's own cores are, never: a test reads the same
        // wherever it runs.
        machine.cpu.busy = None;
        machine.memory = Some(gib << 30);
        machine.measured_at = (now() - jiff::SignedDuration::from_secs(age)).to_string();
        machine.room = Some(Room {
            said_at: NOW.to_owned(),
            memory_available: free << 30,
            pressure: Pressure {
                cpu,
                memory: 0.0,
                io: 0.0,
            },
            takes: Takes {
                builds: 1,
                vm_tests: 1,
            },
            over: Some(if cpu >= OVER_CPU_PRESSURE {
                vec![Limit::Cpu]
            } else {
                Vec::new()
            }),
        });
        machine
    }

    fn session(id: &str, machine: &str, state: &str) -> Session {
        serde_json::from_value(json!({
            "id": id, "address": id, "machine": machine, "groups": [],
            "state": state, "at": NOW,
        }))
        .expect("a session")
    }

    fn of<'a>(reading: &'a Reading, name: &str) -> &'a Load {
        reading
            .machines
            .iter()
            .find(|one| one.machine == name)
            .expect("the machine")
    }

    #[test]
    fn a_machine_over_while_another_has_room_is_flagged() {
        // As on 15 November at 18:39: one machine stalled on its
        // processors, the other with every core to spare.
        let realm = [
            machine("kestrel", 16, 64, 21, 4.6, 20),
            machine("osprey", 12, 48, 41, 86.0, 40),
        ];
        let reading = read(&realm, &[], now());
        assert!(of(&reading, "osprey").over, "processor pressure 86 is over");
        assert!(
            of(&reading, "kestrel").room,
            "pressure 4.6 and a third free is room"
        );
        assert_eq!(
            reading.flags,
            [
                Flag::HotWhileCool {
                    hot: "osprey".to_owned(),
                    cool: "kestrel".to_owned(),
                },
                Flag::Stranded {
                    machine: "osprey".to_owned(),
                    spent: "processor".to_owned(),
                },
            ],
            "over while another has room, and stalled with its memory free"
        );
        assert_eq!(
            reading.measures, MEASURES,
            "a reading names its definitions"
        );
    }

    #[test]
    fn a_queue_short_of_a_processor_for_a_build_isnt_over() {
        // As the Mac was for 48 of 70 minutes on 16 November: its queue
        // kept a build from a VM test for want of a processor while its
        // pressure was low, and placement no longer counts that over.
        let mut short = machine("osprey", 12, 48, 30, 43.0, 0);
        if let Some(room) = short.room.as_mut() {
            room.over = Some(vec![Limit::Cpu]);
        }
        let mut full = machine("skiff", 8, 32, 1, 5.0, 0);
        if let Some(room) = full.room.as_mut() {
            room.over = Some(vec![Limit::Memory]);
        }
        let realm = [machine("kestrel", 16, 64, 40, 4.6, 0), short, full];
        let reading = read(&realm, &[], now());
        assert!(
            !of(&reading, "osprey").over,
            "a build's processor isn't over"
        );
        assert!(of(&reading, "skiff").over, "its queue's memory limit is");
        let hot: Vec<&Flag> = reading
            .flags
            .iter()
            .filter(|flag| matches!(flag, Flag::HotWhileCool { .. }))
            .collect();
        assert_eq!(
            hot,
            [&Flag::HotWhileCool {
                hot: "skiff".to_owned(),
                cool: "kestrel".to_owned(),
            }],
            "only the machine past its memory limit is hot"
        );
        assert_eq!(
            reading.measures, "balance/3",
            "a new definition, a new name"
        );
    }

    #[test]
    fn a_queue_short_of_a_processor_has_no_room_however_quiet() {
        let mut short = machine("osprey", 12, 48, 30, 5.0, 0);
        if let Some(room) = short.room.as_mut() {
            room.over = Some(vec![Limit::Cpu]);
        }
        let mut full = machine("skiff", 8, 32, 1, 5.0, 0);
        if let Some(room) = full.room.as_mut() {
            room.over = Some(vec![Limit::Memory]);
        }
        let reading = read(&[short, full], &[], now());
        let osprey = of(&reading, "osprey");
        assert!(
            !osprey.over && !osprey.room,
            "short of a processor: neither over nor roomy, {osprey:?}"
        );
        assert!(
            !reading
                .flags
                .iter()
                .any(|flag| matches!(flag, Flag::HotWhileCool { .. })),
            "nothing eases onto a queue short of a processor: {:?}",
            reading.flags
        );
    }

    #[test]
    fn processor_pressure_is_over_from_its_line() {
        let at = |cpu: f64| {
            let mut one = machine("osprey", 12, 48, 30, cpu, 0);
            if let Some(room) = one.room.as_mut() {
                room.over = Some(Vec::new());
            }
            of(&read(&[one], &[], now()), "osprey").over
        };
        assert!(!at(OVER_CPU_PRESSURE - 0.1), "under the line isn't over");
        assert!(at(OVER_CPU_PRESSURE), "at the line is over");
    }

    #[test]
    fn load_is_the_dominant_share_and_imbalance_its_spread() {
        let realm = [
            machine("kestrel", 16, 64, 16, 10.0, 0),
            machine("osprey", 12, 48, 36, 50.0, 0),
        ];
        let reading = read(&realm, &[], now());
        let kestrel = of(&reading, "kestrel");
        let osprey = of(&reading, "osprey");
        assert_eq!(kestrel.memory, Some(0.75), "48 of 64 GiB in use");
        assert_eq!(
            kestrel.load,
            Some(0.75),
            "memory dominates a light processor"
        );
        assert_eq!(
            osprey.load,
            Some(0.5),
            "pressure 50 dominates a quarter of memory"
        );
        assert_eq!(
            reading.imbalance,
            Some(0.125),
            "half the gap between 0.75 and 0.5"
        );
        assert!(
            !reading.flags.contains(&Flag::Imbalanced),
            "0.125 is within the line"
        );
        let wide = [
            machine("kestrel", 16, 64, 60, 0.0, 0),
            machine("osprey", 12, 48, 40, 90.0, 0),
        ];
        assert!(
            read(&wide, &[], now()).flags.contains(&Flag::Imbalanced),
            "0.9 against 0.06 is out of balance"
        );
    }

    #[test]
    fn cores_busy_but_not_waited_on_are_a_load() {
        // Pressure stays near zero until every core is busy, so a
        // machine nine tenths busy read as idle under balance/1.
        let mut kestrel = machine("kestrel", 16, 64, 60, 2.0, 0);
        kestrel.cpu.busy = Some(0.9);
        let mut skiff = machine("skiff", 12, 48, 44, 40.0, 0);
        skiff.cpu.busy = Some(0.1);
        let reading = read(&[kestrel, skiff], &[], now());
        let kestrel = of(&reading, "kestrel");
        assert_eq!(kestrel.processor, Some(0.9), "its cores' use");
        assert_eq!(kestrel.load, Some(0.9), "the processor dominates");
        assert!(!kestrel.room, "nine tenths busy has no room");
        assert_eq!(
            of(&reading, "skiff").processor,
            Some(0.4),
            "pressure past its use still counts"
        );
        let unsaid = machine("kestrel", 16, 64, 60, 30.0, 0);
        assert_eq!(
            of(&read(&[unsaid], &[], now()), "kestrel").processor,
            Some(0.3),
            "a node not saying its use counts by pressure"
        );
    }

    #[test]
    fn sessions_count_by_machine_and_by_core() {
        let realm = [machine("kestrel", 16, 64, 40, 0.0, 0)];
        let sessions = [
            session("a", "kestrel", "busy"),
            session("b", "kestrel", "busy"),
            session("c", "kestrel", "idle"),
            session("d", "kestrel", "ended"),
            session("e", "osprey", "busy"),
        ];
        let kestrel = read(&realm, &sessions, now()).machines.remove(0);
        assert_eq!(
            (kestrel.busy, kestrel.idle),
            (2, 1),
            "ended and elsewhere don't count"
        );
        assert_eq!(
            kestrel.busy_per_core,
            Some(0.125),
            "two busy on sixteen cores"
        );
    }

    #[test]
    fn stranded_and_stale_machines_are_flagged() {
        let realm = [
            machine("kestrel", 16, 64, 50, 80.0, 0),
            machine("osprey", 12, 48, 30, 0.0, 400),
        ];
        let flags = read(&realm, &[], now()).flags;
        assert!(
            flags.contains(&Flag::Stranded {
                machine: "kestrel".to_owned(),
                spent: "processor".to_owned(),
            }),
            "every core stalled with most memory free: {flags:?}"
        );
        assert!(
            flags.contains(&Flag::Stale {
                machine: "osprey".to_owned(),
            }),
            "facts four minutes old: {flags:?}"
        );
    }

    #[test]
    fn a_machine_out_of_reach_neither_has_room_nor_counts() {
        let mut gone = machine("osprey", 12, 48, 40, 0.0, 30);
        gone.state = Readiness::Unreachable;
        let realm = [machine("kestrel", 16, 64, 10, 90.0, 0), gone];
        let reading = read(&realm, &[], now());
        assert!(
            reading
                .flags
                .iter()
                .all(|flag| !matches!(flag, Flag::HotWhileCool { .. })),
            "no room out of reach"
        );
        assert_eq!(reading.imbalance, None, "one machine has no spread");
    }

    #[test]
    fn a_watch_holds_a_flag_seen_mostly_and_lets_it_go_once_rare() {
        let hot = [
            machine("kestrel", 16, 64, 40, 0.0, 0),
            machine("osprey", 12, 48, 40, 90.0, 0),
        ];
        let calm = [
            machine("kestrel", 16, 64, 40, 0.0, 0),
            machine("osprey", 12, 48, 40, 10.0, 0),
        ];
        let flag = Flag::HotWhileCool {
            hot: "osprey".to_owned(),
            cool: "kestrel".to_owned(),
        };
        let mut watch = Watch::default();
        let mut held = Vec::new();
        for _ in 0..HELD_FROM - 1 {
            held = watch.take(&read(&hot, &[], now()));
        }
        assert!(held.is_empty(), "not yet seen in most readings");
        held = watch.take(&read(&hot, &[], now()));
        assert!(held.contains(&flag), "held once seen in {HELD_FROM}");
        for _ in 0..WINDOW - CLEARED_AT - 1 {
            held = watch.take(&read(&calm, &[], now()));
        }
        assert!(
            held.contains(&flag),
            "still held while seen in more than {CLEARED_AT}"
        );
        held = watch.take(&read(&calm, &[], now()));
        assert!(held.is_empty(), "let go once seen in only {CLEARED_AT}");
    }

    #[test]
    fn each_reading_is_a_line_of_its_day_and_the_latest_whole() {
        let folder = tempfile::tempdir().expect("a folder");
        let day: jiff::civil::Date = "2026-11-16".parse().expect("a day");
        let first = read(&[machine("kestrel", 16, 64, 40, 0.0, 0)], &[], now());
        let mut second = read(&[machine("osprey", 12, 48, 40, 90.0, 0)], &[], now());
        second.held = Some(vec![Flag::Imbalanced]);
        for reading in [&first, &second] {
            keep(folder.path(), reading, day).expect("a line is kept");
            keep_latest(folder.path(), reading).expect("the latest is kept");
        }
        let lines =
            fs::read_to_string(folder.path().join("2026-11-16.jsonl")).expect("the day's file");
        let kept: Vec<Reading> = lines
            .lines()
            .map(|line| serde_json::from_str(line).expect("a reading a line"))
            .collect();
        assert_eq!(
            kept,
            [first, second.clone()],
            "each reading a line, in order"
        );
        let latest: Reading =
            serde_json::from_slice(&fs::read(folder.path().join(NOW_FILE)).expect("the latest"))
                .expect("the latest is a reading");
        assert_eq!(latest, second, "the latest is the last kept, whole");
        let mut names: Vec<String> = fs::read_dir(folder.path())
            .expect("the folder")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["2026-11-16.jsonl", NOW_FILE],
            "nothing half-written left"
        );
    }

    #[test]
    fn a_link_where_the_latest_is_staged_is_never_written_through() {
        let folder = tempfile::tempdir().expect("a folder");
        let elsewhere = folder.path().join("elsewhere");
        fs::write(&elsewhere, "untouched").expect("a file elsewhere");
        let staged = folder
            .path()
            .join(format!("{NOW_FILE}.{}.new", std::process::id()));
        std::os::unix::fs::symlink(&elsewhere, &staged).expect("a link");
        let reading = read(&[machine("kestrel", 16, 64, 40, 0.0, 0)], &[], now());
        keep_latest(folder.path(), &reading).expect_err("refused");
        assert_eq!(
            fs::read_to_string(&elsewhere).expect("the file elsewhere"),
            "untouched"
        );
        assert!(!folder.path().join(NOW_FILE).exists(), "nothing renamed");
    }
}
