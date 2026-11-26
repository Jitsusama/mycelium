//! The room a machine has for more work, as its own queue says it. The
//! harness's machine queue starts every build and VM test by the memory
//! free and the pressure on the machine, and publishes what it starts
//! by. A node passes that on rather than judging room itself, so work is
//! placed on a machine by the measure that machine admits it by, and the
//! share its person's slices keep is never ours to set. A session, mostly
//! memory and a little processor, is judged by the memory free and the
//! pressure; what the queue would start now says only whether a build
//! or VM test sent there would start at once. A machine whose queue
//! nobody keeps, so on which no harness runs, has no room known.
//!
//! Only the record's `v`, `at`, `heartbeat`, `machine` and `room` are
//! read, which the harness keeps as a contract: a change to any of them
//! changes `v`, and a field added to them doesn't.
//!
//! Whether the machine is under pressure, for placing work there, is the
//! queue's own verdict, the limits it is past by name, never the queue's
//! numbers judged again here, which would drift from the queue's own
//! rules. Placement's
//! saturation is a different question, asked of sessions rather than of
//! the queue's builds: whether so much waits on the processors that a
//! session started there is slow, which no queue rule answers. The
//! machine's own node judges it across its measures and says it in its
//! room as `saturated`, which a queue's record never carries.
//!
//! A node easing its own machine asks a different question again: not
//! whether work would start there, but whether to carry a session off.
//! A queue's record is a file any session can write, so that verdict
//! comes only from the kernel's own readings, [`felt`], judged by
//! [`crate::balance`]'s copy of the queue's lines. Should the queue's
//! lines move, its copy moves with them by hand. Memory is judged by a
//! slower line besides, which a machine kept lively is held to: all work
//! stalled for part of the last minute. The ten seconds' stall alone
//! flaps on a machine whose memory is full. Memory free is no line of
//! its own, since what holds it may be no session's, so easing would
//! park sessions for what they never caused; the kernel's stall says
//! when the want of it slows work.
//!
//! [`Room`] and [`Limit`] say a queue's room; a room felt says the same
//! fields, judged by the node.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The room a machine has, as its queue last said it, or as its node
/// felt it ([`felt`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Room {
    /// When its queue said it, as an RFC 3339 time.
    pub said_at: String,
    /// The memory free for more work, in bytes: what the kernel says is
    /// available.
    pub memory_available: u64,
    /// How hard the machine works for what it has.
    pub pressure: Pressure,
    /// How many more of each kind of work its queue would start now.
    pub takes: Takes,
    /// The queue's limits the machine is at or past now, none when it is
    /// under none, or unknown from a queue too old to say.
    pub over: Option<Vec<Limit>>,
    /// Whether the machine's own node judges its processors saturated,
    /// over time rather than by this pressure alone (placement's
    /// `Saturation`): unknown from a queue's record as read, or from a
    /// node too old to judge, when placement judges the pressure itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saturated: Option<bool>,
}

/// A limit of the queue's that a machine can be past.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Limit {
    /// So much waits on memory that the queue starts no work.
    Memory,
    /// So much waits on a processor that the queue starts no build beside
    /// a VM test.
    Cpu,
}

/// The share of the last ten seconds in which some work waited on each,
/// in percent, as the kernel's pressure stall information says.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pressure {
    /// Waiting on a processor.
    pub cpu: f64,
    /// Waiting on memory.
    pub memory: f64,
    /// Waiting on disks.
    pub io: f64,
}

/// How many more of each kind of heavy work a queue would start now. None
/// means more would wait its turn, as it does while a VM test runs, not
/// that the machine is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Takes {
    /// Builds.
    pub builds: u32,
    /// Tests that run a virtual machine.
    pub vm_tests: u32,
}

/// How long a queue goes unwritten before nobody keeps it: the harness
/// writes it at least every 30 seconds, and gives it up after this.
const UNKEPT_SECONDS: i64 = 120;

/// The shape of the queue's record this reads.
const SHAPE: u64 = 1;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// The room the queue whose record is `queue` says this machine has, if
/// someone keeps it, as of `now` in epoch seconds.
#[must_use]
pub fn read(queue: &Path, now: i64) -> Option<Room> {
    said(&fs::read(queue).ok()?, now)
}

/// The sessions with work in the queue kept at `queue`, waiting its turn
/// or running, as of `now`, in seconds since the epoch. None, where the
/// queue isn't kept: a record nobody keeps names work that may be long
/// gone.
#[must_use]
pub fn queued_by(queue: &Path, now: i64) -> BTreeSet<String> {
    fs::read(queue)
        .ok()
        .map(|record| owners(&record, now))
        .unwrap_or_default()
}

/// The sessions a queue's record says own work in it.
fn owners(record: &[u8], now: i64) -> BTreeSet<String> {
    #[derive(Deserialize)]
    struct Queue {
        v: u64,
        heartbeat: i64,
        #[serde(default)]
        jobs: Vec<Job>,
    }
    #[derive(Deserialize)]
    struct Job {
        owner: Owner,
    }
    #[derive(Deserialize)]
    struct Owner {
        session: String,
    }

    let Ok(queue) = serde_json::from_slice::<Queue>(record) else {
        return BTreeSet::new();
    };
    if queue.v != SHAPE || now.saturating_sub(queue.heartbeat) >= UNKEPT_SECONDS {
        return BTreeSet::new();
    }
    queue
        .jobs
        .into_iter()
        .map(|job| job.owner.session)
        .collect()
}

/// The room a queue's record says, unless it isn't one this reads or
/// nobody has kept it lately.
fn said(record: &[u8], now: i64) -> Option<Room> {
    #[derive(Deserialize)]
    struct Queue {
        v: u64,
        at: String,
        heartbeat: i64,
        machine: Machine,
        room: Left,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Machine {
        available_gib: f64,
        pressure: Pressure,
        over: Option<Vec<String>>,
    }
    #[derive(Deserialize)]
    struct Left {
        vms: u32,
        builds: u32,
    }

    let queue: Queue = serde_json::from_slice(record).ok()?;
    if queue.v != SHAPE || now.saturating_sub(queue.heartbeat) >= UNKEPT_SECONDS {
        return None;
    }
    let available = queue.machine.available_gib * GIB;
    Some(Room {
        said_at: queue.at,
        // A count of bytes, which a queue's few decimals of GiB can't
        // carry past; never negative.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a whole, non-negative count of bytes"
        )]
        memory_available: available.max(0.0).round() as u64,
        pressure: queue.machine.pressure,
        takes: Takes {
            builds: queue.room.builds,
            vm_tests: queue.room.vms,
        },
        // A limit a later queue names that this build doesn't know is
        // left out, rather than losing the rest of the room.
        over: queue.machine.over.map(|over| {
            over.iter()
                .filter_map(|limit| serde_json::from_value(limit.as_str().into()).ok())
                .collect()
        }),
        saturated: None,
    })
}

/// This machine's room as its own kernel says it, read from `procs`
/// (Linux's `/proc`, or a stand-in for it) and said `at`: the pressure
/// from `pressure/cpu`, `pressure/memory` and `pressure/io`, the memory
/// free from `meminfo`, and the limits it is past judged here by
/// [`crate::balance`]'s lines. It is what a node eases its machine by,
/// since a queue's record is a file any session can write, and none of
/// it is. A machine whose kernel says none of it, as a Mac's, has no room
/// felt, so its node never judges it over. It takes nothing, since a
/// session carried by it is no build or VM test.
#[must_use]
pub fn felt(procs: &Path, at: &str) -> Option<Room> {
    let read = |name: &str| fs::read_to_string(procs.join(name)).ok();
    felt_from(
        &read("pressure/cpu")?,
        &read("pressure/memory")?,
        &read("pressure/io")?,
        &read("meminfo")?,
        at,
    )
}

/// The room the kernel's `cpu`, `memory` and `io` pressure and its
/// `meminfo` say, `at`, or none where any can't be read.
fn felt_from(cpu: &str, memory: &str, io: &str, meminfo: &str, at: &str) -> Option<Room> {
    let pressure = Pressure {
        cpu: stalled(cpu)?,
        memory: stalled(memory)?,
        io: stalled(io)?,
    };
    let memory_available = kept(meminfo, "MemAvailable:")?;
    let mut over = Vec::new();
    if memory_over(memory)? {
        over.push(Limit::Memory);
    }
    if pressure.cpu >= crate::balance::OVER_CPU_PRESSURE {
        over.push(Limit::Cpu);
    }
    Some(Room {
        said_at: at.to_owned(),
        memory_available,
        pressure,
        takes: Takes {
            builds: 0,
            vm_tests: 0,
        },
        over: Some(over),
        saturated: None,
    })
}

/// The bytes `meminfo`'s line `name` says, as the kernel writes it in kB.
fn kept(meminfo: &str, name: &str) -> Option<u64> {
    meminfo.lines().find_map(|line| {
        let kib = line.strip_prefix(name)?.trim().strip_suffix("kB")?;
        kib.trim().parse::<u64>().ok()?.checked_mul(1024)
    })
}

/// Whether a machine is past its memory limit by the kernel's memory
/// `pressure`, by either of two lines: the queue's, some work stalled
/// [`OVER_MEMORY_PRESSURE`] of the last ten seconds; or all of it
/// stalled [`OVER_MEMORY_FULL`] of the last minute. The minute's stall
/// moves slowly, where the ten seconds' flaps: on 26 November a machine
/// whose memory was full for an hour read cool every few minutes by the
/// ten seconds alone, and each pair of cool readings started its spell
/// again, so it put no session to sleep. None where the `some` line
/// can't be read; a file with no `full` line, as a kernel may write, is
/// judged by its `some` line alone.
///
/// [`OVER_MEMORY_PRESSURE`]: crate::balance::OVER_MEMORY_PRESSURE
/// [`OVER_MEMORY_FULL`]: crate::balance::OVER_MEMORY_FULL
fn memory_over(pressure: &str) -> Option<bool> {
    let some = stalled(pressure)?;
    let full = stall(pressure, "full ", "avg60=").unwrap_or(0.0);
    Some(some >= crate::balance::OVER_MEMORY_PRESSURE || full >= crate::balance::OVER_MEMORY_FULL)
}

/// The share of the last ten seconds in which some task stalled, in
/// percent, as a pressure file's `some` line says it.
fn stalled(pressure: &str) -> Option<f64> {
    stall(pressure, "some ", "avg10=")
}

/// The share a pressure file's `line` says in its `field`, in percent.
fn stall(pressure: &str, line: &str, field: &str) -> Option<f64> {
    pressure
        .lines()
        .find_map(|said| said.strip_prefix(line))?
        .split_whitespace()
        .find_map(|said| said.strip_prefix(field))?
        .parse::<f64>()
        .ok()
        .filter(|share| share.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEPT_AT: i64 = 1_793_248_698;

    fn record(v: u64, heartbeat: i64) -> Vec<u8> {
        let mut record = older(v, heartbeat);
        record["machine"]["over"] = serde_json::json!(["memory", "swap"]);
        record.to_string().into_bytes()
    }

    /// As a queue wrote it before it said which limits it was past.
    fn older(v: u64, heartbeat: i64) -> serde_json::Value {
        serde_json::json!({
            "v": v,
            "at": "2026-10-29T04:38:18.818Z",
            "heartbeat": heartbeat,
            "machine": {
                "cores": 16,
                "availableGib": 23.5,
                "pressure": {"cpu": 44.37, "memory": 3.92, "io": 51.53},
                "vms": 7
            },
            "room": {"vms": 1, "builds": 8},
            "quiet": null,
            "jobs": [],
            "wasted": {"today": 0, "day": "2026-10-29", "minute": "2026-10-29T04:38"}
        })
    }

    #[test]
    fn a_kept_queue_says_whose_work_it_holds() {
        let mut kept = older(1, KEPT_AT);
        kept["jobs"] = serde_json::json!([
            {"id": "a", "state": "running", "owner": {"session": "busy-one", "pid": 1}},
            {"id": "b", "state": "waiting", "owner": {"session": "waiting-one", "pid": 2}}
        ]);
        let kept = kept.to_string().into_bytes();
        assert_eq!(
            owners(&kept, KEPT_AT + 30),
            BTreeSet::from(["busy-one".to_owned(), "waiting-one".to_owned()]),
            "a job running and one waiting its turn each name their session"
        );
        assert!(
            owners(&kept, KEPT_AT + UNKEPT_SECONDS).is_empty(),
            "a queue nobody keeps names no work"
        );
    }

    #[test]
    fn a_rooms_verdict_on_saturation_crosses_the_wire_only_when_said() {
        let mut room = said(&record(1, KEPT_AT), KEPT_AT + 30).expect("a room");
        let unsaid = serde_json::to_value(&room).expect("a room is json");
        assert!(
            unsaid.get("saturated").is_none(),
            "a room with no verdict says none, as an older node's does"
        );
        let older: Room = serde_json::from_value(unsaid).expect("an older room reads");
        assert_eq!(older.saturated, None, "and reads back as none");
        room.saturated = Some(true);
        let said_on = serde_json::to_string(&room).expect("a room is json");
        let heard: Room = serde_json::from_str(&said_on).expect("a room reads");
        assert_eq!(heard.saturated, Some(true), "a verdict said is heard");
    }

    #[test]
    fn a_kept_queue_says_the_room_it_starts_work_by() {
        assert_eq!(
            said(&record(1, KEPT_AT), KEPT_AT + 30),
            Some(Room {
                said_at: "2026-10-29T04:38:18.818Z".to_owned(),
                memory_available: 25_232_932_864,
                pressure: Pressure {
                    cpu: 44.37,
                    memory: 3.92,
                    io: 51.53,
                },
                takes: Takes {
                    builds: 8,
                    vm_tests: 1,
                },
                over: Some(vec![Limit::Memory]),
                saturated: None,
            }),
            "its free memory in bytes, its pressure, what it would start, and \
             the limits it is past that this build knows"
        );
        let older = older(1, KEPT_AT).to_string().into_bytes();
        assert_eq!(
            said(&older, KEPT_AT).and_then(|room| room.over),
            None,
            "a queue that doesn't say its limits leaves them unknown"
        );
    }

    #[test]
    fn no_room_is_known_from_a_queue_nobody_keeps() {
        assert_eq!(
            said(&record(1, KEPT_AT), KEPT_AT + UNKEPT_SECONDS),
            None,
            "a record unwritten past its keeping says what was, not what is"
        );
        assert_eq!(
            said(&record(2, KEPT_AT), KEPT_AT),
            None,
            "a shape this doesn't read is unknown, not guessed at"
        );
        assert_eq!(said(b"{", KEPT_AT), None, "nor is a torn record");
    }

    const FELT_AT: &str = "2026-11-17T23:50:00Z";

    /// A pressure file as Linux writes it, `some` at `avg10`.
    fn psi(avg10: f64) -> String {
        format!(
            "some avg10={avg10:.2} avg60=1.00 avg300=0.50 total=123456\n\
             full avg10=0.00 avg60=0.00 avg300=0.00 total=0\n"
        )
    }

    const MEMINFO: &str = "MemTotal:       65536000 kB\n\
                           MemFree:         1000000 kB\n\
                           MemAvailable:   41943040 kB\n";

    #[test]
    fn a_machine_its_own_kernel_says_is_pressed_is_over_its_processors() {
        let room =
            felt_from(&psi(86.0), &psi(1.5), &psi(40.0), MEMINFO, FELT_AT).expect("room felt");
        assert_eq!(room.over, Some(vec![Limit::Cpu]));
        assert_eq!(
            room.pressure,
            Pressure {
                cpu: 86.0,
                memory: 1.5,
                io: 40.0
            }
        );
        assert_eq!(room.memory_available, 41_943_040 * 1024);
        assert_eq!(room.said_at, FELT_AT);
        assert_eq!(
            room.takes,
            Takes {
                builds: 0,
                vm_tests: 0
            },
            "a session carried is no build"
        );
        assert_eq!(room.saturated, None, "saturation is placement's to judge");
    }

    #[test]
    fn memory_stalled_past_the_line_is_over_memory() {
        let room =
            felt_from(&psi(90.0), &psi(12.0), &psi(0.0), MEMINFO, FELT_AT).expect("room felt");
        assert_eq!(room.over, Some(vec![Limit::Memory, Limit::Cpu]));
    }

    #[test]
    fn over_starts_at_the_lines_themselves() {
        let over = |cpu: f64, memory: f64| {
            felt_from(&psi(cpu), &psi(memory), &psi(0.0), MEMINFO, FELT_AT)
                .and_then(|room| room.over)
                .expect("room felt")
        };
        assert_eq!(over(70.0, 0.0), vec![Limit::Cpu], "at the processor line");
        assert_eq!(over(69.99, 0.0), Vec::new(), "just under it");
        assert_eq!(over(0.0, 10.0), vec![Limit::Memory], "at the memory line");
        assert_eq!(over(0.0, 9.99), Vec::new(), "just under it");
    }

    /// A memory pressure file whose `full` line says `avg60`, with no
    /// work stalled in the last ten seconds.
    fn full(avg60: f64) -> String {
        format!(
            "some avg10=0.00 avg60=0.00 avg300=0.00 total=123456\n\
             full avg10=0.00 avg60={avg60:.2} avg300=0.00 total=0\n"
        )
    }

    // On 26 November a machine whose memory was full for an hour read cool
    // every few minutes by the ten seconds' stall alone, and each pair of
    // cool readings started its spell again.
    #[test]
    fn all_work_stalled_for_a_minute_is_over_memory_from_its_line() {
        let over = |avg60: f64| {
            felt_from(&psi(0.0), &full(avg60), &psi(0.0), MEMINFO, FELT_AT)
                .and_then(|room| room.over)
                .expect("room felt")
        };
        assert_eq!(over(5.0), vec![Limit::Memory], "at the line");
        assert_eq!(over(4.99), Vec::new(), "just under it");
    }

    // Our council on #488: a machine short of free memory for reasons no
    // session caused would park every session waiting there, and keep
    // them parked.
    #[test]
    fn a_machine_short_of_free_memory_but_unstalled_is_not_over() {
        let meminfo = "MemTotal: 67108864 kB\nMemAvailable: 1024 kB\n";
        let room =
            felt_from(&psi(0.0), &full(0.0), &psi(0.0), meminfo, FELT_AT).expect("room felt");
        assert_eq!(room.over, Some(Vec::new()));
    }

    #[test]
    fn a_pressure_file_with_no_full_line_is_judged_by_its_some_line() {
        let over = |some: f64| {
            let pressure = format!("some avg10={some:.2} avg60=0.00 avg300=0.00 total=1\n");
            felt_from(&psi(0.0), &pressure, &psi(0.0), MEMINFO, FELT_AT)
                .and_then(|room| room.over)
                .expect("room felt")
        };
        assert_eq!(over(10.0), vec![Limit::Memory], "over by its some line");
        assert_eq!(over(0.0), Vec::new(), "and cool by it");
    }

    #[test]
    fn a_calm_machine_is_over_nothing() {
        let room = felt_from(&psi(4.6), &psi(0.0), &psi(2.0), MEMINFO, FELT_AT).expect("room felt");
        assert_eq!(room.over, Some(Vec::new()));
    }

    #[test]
    fn what_the_kernel_doesnt_say_is_no_room() {
        assert_eq!(
            felt_from("", &psi(0.0), &psi(0.0), MEMINFO, FELT_AT),
            None,
            "no processor pressure"
        );
        assert_eq!(
            felt_from(&psi(1.0), &psi(0.0), &psi(0.0), "MemTotal: 1 kB\n", FELT_AT),
            None,
            "no memory free"
        );
        assert_eq!(
            felt_from(&psi(1.0), "", &psi(0.0), MEMINFO, FELT_AT),
            None,
            "no memory pressure"
        );
        assert_eq!(
            felt_from(&psi(1.0), &psi(0.0), "", MEMINFO, FELT_AT),
            None,
            "no I/O pressure"
        );
        for said in ["some avg10=high\n", "some avg10=nan\n", "some avg10=inf\n"] {
            assert_eq!(
                felt_from(said, &psi(0.0), &psi(0.0), MEMINFO, FELT_AT),
                None,
                "no number in {said:?}"
            );
        }
        assert_eq!(
            felt(Path::new("/nowhere/proc"), FELT_AT),
            None,
            "no kernel's files at all, as on a Mac"
        );
    }

    #[test]
    fn room_is_felt_from_the_kernels_own_files() {
        let procs = tempfile::tempdir().expect("a folder");
        fs::create_dir(procs.path().join("pressure")).expect("its pressure");
        for (name, avg10) in [("cpu", 86.0), ("memory", 0.0), ("io", 3.0)] {
            fs::write(procs.path().join("pressure").join(name), psi(avg10)).expect("written");
        }
        fs::write(procs.path().join("meminfo"), MEMINFO).expect("written");
        let room = felt(procs.path(), FELT_AT).expect("room felt");
        assert_eq!(room.over, Some(vec![Limit::Cpu]));
        fs::remove_file(procs.path().join("pressure").join("io")).expect("removed");
        assert_eq!(
            felt(procs.path(), FELT_AT),
            None,
            "a kernel that says only some of it says none"
        );
    }
}
