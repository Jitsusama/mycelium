//! Whether a carried session's start on the other machine stood, rather
//! than only being held there.
//!
//! On 27 November a carry to a room said "1 session started on <room>,
//! held" and closed, though its held start ended at once with 127: the
//! room's path had no `pi-carry`, so no pi ever ran there, and its done
//! file was marked started, so no later carry would start it again. The
//! session's work stood still for half an hour before anyone saw.
//!
//! So a carry asks first, as a plain run on that machine, whether the
//! commands the start runs are on its path, and fails the start plainly
//! where they aren't, leaving its done file unmarked. The start itself
//! stays word for word what [`super::arriving`] gives, since both nodes
//! read that shape as a carry's arrival: it raises the start's fence to
//! the one the session ran in and brings the session's cell. Once
//! started, it waits for each session's presence to cross from there,
//! for [`STANDS_WITHIN`] at most for all of them together, and says
//! those it never saw as not yet seen running rather than failing them,
//! since a slow machine may still bring them up.

use std::fmt::Write as _;
use std::time::Duration;

use super::{Done, Hands, PI_CARRY, runs, session_id};
use crate::placement::Work;
use crate::wire::{Run, Started};

/// How long a carry waits, all told, for its started sessions' presences
/// to cross from the machine they went to: time for pi to start and say
/// it is there, which takes seconds, with room for a loaded machine.
pub const STANDS_WITHIN: Duration = Duration::from_secs(120);

/// What the program a carried session starts with is named.
const PI: &str = "pi";

/// The exit code a shell gives a command it can't find, which the check
/// gives too, so its failure reads as the start's would have.
const NOT_FOUND: i32 = 127;

/// The check that each command named after it is on the path, saying the
/// first that isn't on its standard error and ending as a shell would
/// had it run it.
const CHECK: &str =
    r#"for one in "$@"; do command -v "$one" >/dev/null || { echo "$one" >&2; exit 127; }; done"#;

/// How a run on another machine ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    /// Its exit code, or 128 and the signal that ended it.
    pub code: i32,
    /// What it wrote to its standard error, at most its last 64 KiB.
    pub err: Vec<u8>,
}

/// The run asking whether the commands `one`'s start on the machine
/// `work` needs would be found there: `pi-carry` alone for the quest's
/// own trees, which start no session, and pi as well for a session.
#[must_use]
pub fn check(quest: &str, one: &Done, work: Work) -> Run {
    let mut command = ["sh", "-c", CHECK, "sh", PI_CARRY]
        .map(str::to_owned)
        .to_vec();
    if one.session != quest {
        command.push(PI.to_owned());
    }
    Run {
        work,
        folder: None,
        command,
        // The node judges who asks, and where from.
        by: None,
        from: None,
        asked_by: None,
    }
}

/// Whether `address`'s start on `to` may go ahead, as `exit`, the run of
/// [`check`], says: where it ended other than at once, why it can't, said
/// whole for the carry's line.
///
/// # Errors
///
/// When the check ended with any code but 0, naming the code and the
/// command it found missing, or the last line it said.
pub fn checked(address: &str, to: &str, exit: &Exit) -> Result<(), String> {
    if exit.code == 0 {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&exit.err);
    let last = said
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty());
    let mut why = format!(
        "{address} didn't start on {to}: it would end at once with code {}",
        exit.code
    );
    // Writing to a string can't fail.
    let _ = match last {
        Some(missing) if exit.code == NOT_FOUND => {
            write!(why, ", as {missing} isn't on {to}'s path")
        }
        Some(line) => write!(why, ": {line}"),
        None => Ok(()),
    };
    Err(why)
}

/// A session started held on the other machine, not yet seen there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landed {
    /// The address it went by here.
    pub address: String,
    /// Its done file, marked started once it is seen or the wait ends.
    pub one: Done,
    /// Its start there.
    pub started: Started,
}

/// Which of `landed` were seen running on `to`, in their order: each
/// whose session's presence came from there, by its id, its address or
/// the hold it was started in. It looks again until every one is seen,
/// [`STANDS_WITHIN`] has gone by for them all, or the hands have no
/// patience left. A presence that can't be read counts as none yet.
pub fn seen(hands: &mut impl Hands, to: &str, landed: &[Landed]) -> Vec<bool> {
    let mut seen = vec![false; landed.len()];
    if landed.is_empty() {
        return seen;
    }
    let began = hands.now();
    loop {
        let sessions = hands.sessions().unwrap_or_default();
        let there = sessions.iter().filter(|one| one.machine == to && runs(one));
        for session in there {
            for (at, one) in landed.iter().enumerate() {
                seen[at] |= session_id(session) == Some(one.one.session.as_str())
                    || session.id == one.one.session
                    || session.address == one.address
                    || session.held.as_deref() == Some(one.started.held.as_str());
            }
        }
        let waited = hands.now().saturating_duration_since(began);
        if seen.iter().all(|one| *one) || waited >= STANDS_WITHIN || !hands.wait() {
            return seen;
        }
    }
}
