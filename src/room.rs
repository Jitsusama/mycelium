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
//! changes `v`.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The room a machine has, as its queue last said it.
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEPT_AT: i64 = 1_793_248_698;

    fn record(v: u64, heartbeat: i64) -> Vec<u8> {
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
        .to_string()
        .into_bytes()
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
            }),
            "its free memory in bytes, its pressure and what it would start"
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
}
