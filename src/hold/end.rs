//! Ending a held session for good, as its orchestrator or the person
//! asks: its holder and its program's process group are told to end, are
//! given a grace to save what they hold, and are killed past it, and the
//! hold's record and socket go with them.
//!
//! The holder is told first, so a program that parks as it saves stays
//! down rather than waiting to be woken: a holder told to end tells its
//! program too, and ends with it. The program's whole group is told as
//! well, so what it started on its terminal ends with it.

use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal};
use serde::{Deserialize, Serialize};

use super::{held, socket};

/// How long a held program and its holder have, once told to end, to
/// save what they hold and go before they are killed.
pub const GRACE: Duration = Duration::from_secs(10);

/// How long what is killed is waited on to be gone, as the kernel takes
/// it away and its parent waits on it.
const KILLED_WITHIN: Duration = Duration::from_secs(2);

/// How often an end looks at whether they have gone.
const LOOKS_EVERY: Duration = Duration::from_millis(50);

/// Who asks an end, as the node it was first asked of judged them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Asker {
    /// The person's own login.
    Person,
    /// The session going by this address, as the node proved it asks.
    Session(String),
}

impl std::fmt::Display for Asker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Person => f.write_str("the person"),
            Self::Session(address) => f.write_str(address),
        }
    }
}

/// How a hold ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ending {
    /// Its holder and its program went within their grace.
    Gently,
    /// They were killed once their grace was over.
    Killed,
}

/// Whether `asker` may end the session held as `id`, whose session names
/// `orchestrator` as the one it answers to, if it names one: the person
/// may end any, and a session only one it orchestrates.
///
/// # Errors
///
/// When the asker is a session the held one doesn't answer to.
pub fn may(asker: &Asker, orchestrator: Option<&str>, id: &str) -> Result<(), String> {
    match asker {
        Asker::Person => Ok(()),
        Asker::Session(address)
            if orchestrator.is_some_and(|it| it.eq_ignore_ascii_case(address)) =>
        {
            Ok(())
        }
        Asker::Session(address) => Err(format!(
            "{address} doesn't orchestrate the session held as {id}, so it can't end it: \
             only its orchestrator or the person does"
        )),
    }
}

/// Ends the session held as `id` in `dir`: tells its holder and its
/// program's process group to end, waits as long as `grace` for both to
/// go, kills whatever is left, and takes away its record and socket.
///
/// # Errors
///
/// When nothing is held as `id`, or its record or socket can't be taken
/// away.
pub fn end(dir: &Path, id: &str, grace: Duration) -> io::Result<Ending> {
    let Some(one) = held(dir).into_iter().find(|one| one.id == id) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("nothing is held as {id}; `mycelium holds` lists what is"),
        ));
    };
    let holder = pid(one.holder);
    // A parked program runs no process.
    let program = pid(one.pid);
    tell(holder, program, Signal::TERM);
    let ending = if gone_within(holder, program, grace) {
        Ending::Gently
    } else {
        tell(holder, program, Signal::KILL);
        if !gone_within(holder, program, KILLED_WITHIN) {
            return Err(io::Error::other(format!(
                "what holds {id} was killed but hasn't gone within {KILLED_WITHIN:?}"
            )));
        }
        Ending::Killed
    };
    // A holder that ended removed these itself; one killed left them.
    for left in [dir.join(format!("{id}.json")), socket(dir, id)] {
        match fs::remove_file(&left) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    Ok(ending)
}

/// The process `raw` names, if it names one.
fn pid(raw: u32) -> Option<Pid> {
    i32::try_from(raw).ok().and_then(Pid::from_raw)
}

/// Sends `signal` to the holder, then to the program's process group, or
/// the program alone where it leads none.
fn tell(holder: Option<Pid>, program: Option<Pid>, signal: Signal) {
    // One already gone has nothing to be told.
    if let Some(holder) = holder {
        let _ = rustix::process::kill_process(holder, signal);
    }
    if let Some(program) = program
        && rustix::process::kill_process_group(program, signal).is_err()
    {
        let _ = rustix::process::kill_process(program, signal);
    }
}

/// Whether the holder and the program, its group included, have both
/// gone by the end of `grace`, looking every [`LOOKS_EVERY`].
fn gone_within(holder: Option<Pid>, program: Option<Pid>, grace: Duration) -> bool {
    let until = Instant::now() + grace;
    loop {
        let holds = holder.is_some_and(|it| rustix::process::test_kill_process(it).is_ok());
        let runs = program.is_some_and(|it| {
            rustix::process::test_kill_process_group(it).is_ok()
                || rustix::process::test_kill_process(it).is_ok()
        });
        if !holds && !runs {
            return true;
        }
        let now = Instant::now();
        if now >= until {
            return false;
        }
        std::thread::sleep(LOOKS_EVERY.min(until - now));
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    use super::*;
    use crate::hold::Held;

    /// A process that is waited on as it ends, as a holder's parent waits
    /// on it, so it never lingers as a zombie that still seems to run.
    fn reaped(mut child: Child) -> u32 {
        let pid = child.id();
        std::thread::spawn(move || child.wait());
        pid
    }

    /// `program` run through `sh` in a process group of its own, as a
    /// holder starts its program on a terminal, once it says it's ready.
    fn program(program: &str) -> u32 {
        let mut child = Command::new("sh")
            .args(["-c", program])
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("the program starts");
        let mut said = String::new();
        BufReader::new(child.stdout.take().expect("its output"))
            .read_line(&mut said)
            .expect("it says it's ready");
        assert_eq!(said.trim(), "ready");
        reaped(child)
    }

    /// A holder that ends as it's told to.
    fn holder() -> u32 {
        reaped(
            Command::new("sleep")
                .arg("1000")
                .spawn()
                .expect("the holder starts"),
        )
    }

    /// Writes the record and socket of a hold `id` in `dir`, held by
    /// `holder` and running `pid`.
    fn holds(dir: &Path, id: &str, holder: u32, pid: u32) {
        let record = Held {
            id: id.to_owned(),
            command: vec!["pi".to_owned()],
            folder: dir.to_owned(),
            holder,
            pid,
            parked: false,
            since: "2026-11-16T15:00:00Z".to_owned(),
            windows: Vec::new(),
        };
        fs::write(
            dir.join(format!("{id}.json")),
            serde_json::to_vec(&record).expect("a record"),
        )
        .expect("the record is written");
        fs::write(socket(dir, id), "").expect("its socket stands");
    }

    fn runs(pid: u32) -> bool {
        self::pid(pid).is_some_and(|it| rustix::process::test_kill_process(it).is_ok())
    }

    #[test]
    fn a_hold_told_to_end_ends_gently_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().expect("a folder");
        let (holder, program) = (holder(), program("echo ready; sleep 1000 & wait"));
        holds(dir.path(), "ending1", holder, program);

        assert_eq!(
            end(dir.path(), "ending1", GRACE).expect("it ends"),
            Ending::Gently
        );
        assert!(
            !runs(holder) && !runs(program),
            "its holder and its program are gone"
        );
        assert!(
            held(dir.path()).is_empty()
                && !dir.path().join("ending1.json").exists()
                && !socket(dir.path(), "ending1").exists(),
            "its record and socket are taken away"
        );
    }

    #[test]
    fn a_program_that_wont_end_is_killed_past_its_grace() {
        let dir = tempfile::tempdir().expect("a folder");
        let program = program("trap '' TERM; echo ready; while :; do sleep 1; done");
        let holder = holder();
        holds(dir.path(), "stubborn", holder, program);

        assert_eq!(
            end(dir.path(), "stubborn", Duration::from_millis(200)).expect("it ends"),
            Ending::Killed
        );
        assert!(!runs(program), "the program that ignored its end is killed");
        assert!(
            !dir.path().join("stubborn.json").exists(),
            "its record goes"
        );
    }

    #[test]
    fn nothing_held_is_said_to_be_so() {
        let dir = tempfile::tempdir().expect("a folder");
        let error = end(dir.path(), "nobody", GRACE).expect_err("nothing to end");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn only_the_person_or_the_sessions_orchestrator_may_end_it() {
        let person = Asker::Person;
        let orchestrator = Asker::Session("WCJQSG".to_owned());
        let stranger = Asker::Session("HARBOR".to_owned());
        assert_eq!(may(&person, None, "ab12"), Ok(()), "the person, any");
        assert_eq!(
            may(&orchestrator, Some("wcjqsg"), "ab12"),
            Ok(()),
            "its orchestrator, in any case"
        );
        assert_eq!(
            may(&stranger, Some("WCJQSG"), "ab12"),
            Err(
                "HARBOR doesn't orchestrate the session held as ab12, so it can't end it: \
                 only its orchestrator or the person does"
                    .to_owned()
            ),
            "anyone else is refused, saying why"
        );
        assert!(
            may(&stranger, None, "ab12").is_err(),
            "nor a session one that answers to nobody"
        );
    }
}
