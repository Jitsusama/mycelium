//! Ending a held session for good, as its orchestrator or the person
//! asks: its holder and its program's process group are told to end, are
//! given a grace to save what they hold, and are killed past it, and the
//! hold's record and socket go with them.
//!
//! The program's whole group is told first, so what it started on its
//! terminal ends with it, and it has the whole grace: its holder, told
//! to end, would give it only its own few seconds. The holder is told
//! once the program has gone, or its grace is over. A record whose holder
//! no longer answers on its socket is stale, its pids perhaps another's
//! by now, so nothing it names is signalled.

use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal};
use serde::{Deserialize, Serialize};

use super::{again, gone, held, next, socket, steers, wake};

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

/// Ends the session held as `id` in `dir`: tells its program's process
/// group to end and waits as long as `grace` for it to go, tells its
/// holder to end, kills whatever is left, and takes away its record and
/// socket.
///
/// # Errors
///
/// When nothing is held as `id`, when its holder no longer answers (its
/// record and socket are then taken away, nothing signalled), or when
/// its record or socket can't be taken away.
pub fn end(dir: &Path, id: &str, grace: Duration) -> io::Result<Ending> {
    let Some(one) = held(dir).into_iter().find(|one| one.id == id) else {
        // A holder killed outright is no longer listed, but what it
        // left still says it holds the session.
        if left_behind(dir, id) {
            take_away(dir, id)?;
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "the holder of {id} has gone, so nothing is signalled; what it left is \
                     taken away"
                ),
            ));
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("nothing is held as {id}; `mycelium holds` lists what is"),
        ));
    };
    // The probe joins the holder as a window that sends nothing and
    // leaves at once.
    if std::os::unix::net::UnixStream::connect(socket(dir, id)).is_err() {
        take_away(dir, id)?;
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "the holder of {id} no longer answers, so nothing it named is signalled; \
                 its stale record is taken away"
            ),
        ));
    }
    let holder = pid(one.holder);
    // A parked program runs no process.
    let program = if one.parked { None } else { pid(one.pid) };
    tell(None, program, Signal::TERM);
    let gently = gone_within(None, program, grace);
    if gently {
        // A holder whose program has gone ends as soon as it's told.
        tell(holder, None, Signal::TERM);
    }
    let ending = if gently && gone_within(holder, None, KILLED_WITHIN) {
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
    take_away(dir, id)?;
    Ok(ending)
}

/// Whether a hold no holder runs for left its record or socket as `id`
/// in `dir`, `id` being one a holder could have: letters and digits
/// alone, so it names nothing outside the folder.
fn left_behind(dir: &Path, id: &str) -> bool {
    let an_id = (1..=64).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_alphanumeric());
    an_id && (dir.join(format!("{id}.json")).exists() || socket(dir, id).exists())
}

/// Takes away the record and socket of the hold `id` in `dir`, and every
/// file its program spoke through, which a holder that ended removed
/// itself and one killed left: a wake written there after would land
/// where nothing reads it. Each is tried, and every one that couldn't
/// go is named.
fn take_away(dir: &Path, id: &str) -> io::Result<()> {
    let stuck: Vec<String> = [
        dir.join(format!("{id}.json")),
        socket(dir, id),
        wake(dir, id),
        again(dir, id),
        next(dir, id),
        dir.join(format!(".{id}.next")),
        steers(dir, id),
        gone(dir, id),
    ]
    .iter()
    .filter_map(|left| match fs::remove_file(left) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            Some(format!("{}: {error}", left.display()))
        }
        _ => None,
    })
    .collect();
    if stuck.is_empty() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "what the hold left couldn't all be taken away: {}",
        stuck.join("; ")
    )))
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
    use std::os::unix::net::UnixListener;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    use super::*;
    use crate::hold::Held;
    use crate::stand_in::Process;

    /// `program` run through `sh` in a process group of its own, as a
    /// holder starts its program on a terminal, once it says it's ready.
    fn program(program: &str) -> Process {
        let mut child = Command::new("sh")
            .args(["-c", program])
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("the program starts");
        let output = child.stdout.take().expect("its output");
        let running = Process::of(child);
        let mut said = String::new();
        BufReader::new(output)
            .read_line(&mut said)
            .expect("it says it's ready");
        assert_eq!(said.trim(), "ready");
        running
    }

    /// A holder that ends as it's told to.
    fn holder() -> Process {
        Process::start(Command::new("sleep").arg("1000"))
    }

    /// Writes the record and socket of a hold `id` in `dir`, held by
    /// `holder` and running `pid`, with the socket answering while what
    /// this gives back is kept.
    fn holds(dir: &Path, id: &str, holder: u32, pid: u32) -> UnixListener {
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
        UnixListener::bind(socket(dir, id)).expect("its socket answers")
    }

    /// Waits until nothing answers on `socket`. A process another test
    /// forks carries every open file until it runs its program, so a
    /// listener dropped here can answer a moment longer.
    fn unanswered(socket: &Path) {
        let until = Instant::now() + GRACE;
        while std::os::unix::net::UnixStream::connect(socket).is_ok() {
            assert!(Instant::now() < until, "{} still answers", socket.display());
            std::thread::sleep(LOOKS_EVERY);
        }
    }

    fn runs(pid: u32) -> bool {
        self::pid(pid).is_some_and(|it| rustix::process::test_kill_process(it).is_ok())
    }

    #[test]
    fn a_hold_told_to_end_ends_gently_and_leaves_nothing_behind() {
        let dir = tempfile::tempdir().expect("a folder");
        let told = dir.path().join("told");
        let program = program(&format!(
            "trap 'echo TERM > {}; exit 0' TERM; echo ready; while :; do sleep 0.05; done",
            told.display()
        ));
        let holding = holder();
        let (holder, program) = (holding.pid(), program.pid());
        let _answers = holds(dir.path(), "ending1", holder, program);

        assert_eq!(
            end(dir.path(), "ending1", GRACE).expect("it ends"),
            Ending::Gently
        );
        assert_eq!(
            fs::read_to_string(&told)
                .expect("the program was told")
                .trim(),
            "TERM",
            "the program was told to end, not killed"
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
        let running = program("trap '' TERM; echo ready; while :; do sleep 1; done");
        let holding = holder();
        let program = running.pid();
        let _answers = holds(dir.path(), "stubborn", holding.pid(), program);

        assert_eq!(
            end(dir.path(), "stubborn", Duration::from_millis(200)).expect("it ends"),
            Ending::Killed
        );
        assert!(!runs(program), "the program that ignored its end is killed");
        assert!(
            !dir.path().join("stubborn.json").exists() && !socket(dir.path(), "stubborn").exists(),
            "its record and socket go"
        );
    }

    #[test]
    fn a_stale_record_and_every_file_it_spoke_through_go_signalling_nothing() {
        let dir = tempfile::tempdir().expect("a folder");
        let (holding, running) = (holder(), program("echo ready; sleep 1000 & wait"));
        let (holder, program) = (holding.pid(), running.pid());
        drop(holds(dir.path(), "stale", holder, program));
        unanswered(&socket(dir.path(), "stale"));
        // A holder that no longer answers leaves what its program said
        // as it parked, and a wake written for it.
        let said = [
            wake(dir.path(), "stale"),
            again(dir.path(), "stale"),
            next(dir.path(), "stale"),
            steers(dir.path(), "stale"),
            gone(dir.path(), "stale"),
        ];
        for file in &said {
            fs::write(file, b"[]").expect("it is written");
        }

        let error = end(dir.path(), "stale", GRACE).expect_err("its holder doesn't answer");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(
            runs(holder) && runs(program),
            "what its pids name now is left alone"
        );
        assert!(
            !dir.path().join("stale.json").exists(),
            "its stale record goes"
        );
        assert!(
            said.iter().all(|file| !file.exists()),
            "and every file it spoke through, so none is found as though it were held"
        );
    }

    #[test]
    fn a_hold_whose_holder_was_killed_outright_leaves_nothing_once_ended() {
        let dir = tempfile::tempdir().expect("a folder");
        let mut ran = Command::new("true").spawn().expect("it starts");
        let holder = ran.id();
        ran.wait().expect("it ends");
        drop(holds(dir.path(), "killed1", holder, 0));
        let left = [
            dir.path().join("killed1.json"),
            wake(dir.path(), "killed1"),
            again(dir.path(), "killed1"),
            next(dir.path(), "killed1"),
            dir.path().join(".killed1.next"),
            steers(dir.path(), "killed1"),
            gone(dir.path(), "killed1"),
        ];
        for file in &left[1..] {
            fs::write(file, b"[]").expect("it is written");
        }
        assert!(
            socket(dir.path(), "killed1").exists() && held(dir.path()).is_empty(),
            "its socket is left, and nothing lists it as held"
        );

        let error = end(dir.path(), "killed1", GRACE).expect_err("its holder has gone");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(
            left.iter().all(|file| !file.exists()) && !socket(dir.path(), "killed1").exists(),
            "what a holder killed outright left goes once it is ended"
        );
    }

    // The usual end of a pi's hold: it parks, leaving its command and its
    // steers, and is then ended while it waits to be woken.
    #[test]
    fn a_parked_hold_ended_leaves_what_its_program_said_nowhere() {
        let dir = tempfile::tempdir().expect("a folder");
        let holding = holder();
        let holder = holding.pid();
        let _answers = holds(dir.path(), "parked1", holder, 0);
        let record = dir.path().join("parked1.json");
        let mut parked: Held =
            serde_json::from_slice(&fs::read(&record).expect("read")).expect("a record");
        parked.parked = true;
        fs::write(&record, serde_json::to_vec(&parked).expect("a record")).expect("written");
        let said = [again(dir.path(), "parked1"), steers(dir.path(), "parked1")];
        for file in &said {
            fs::write(file, b"[]").expect("it is written");
        }

        end(dir.path(), "parked1", GRACE).expect("it ends");
        assert!(!runs(holder), "its holder is gone");
        assert!(
            said.iter().all(|file| !file.exists()) && !record.exists(),
            "what it said as it parked goes with its record"
        );
    }

    #[test]
    fn an_id_naming_a_path_reaches_nothing_outside_the_folder() {
        let dir = tempfile::tempdir().expect("a folder");
        let folder = dir.path().join("work.held");
        fs::create_dir(&folder).expect("made");
        let outside = dir.path().join("kept.json");
        fs::write(&outside, b"{}").expect("it is written");
        let error = end(&folder, "../kept", GRACE).expect_err("no hold has that id");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(outside.exists(), "a file outside the folder is left");
    }

    #[test]
    fn nothing_held_is_said_to_be_so() {
        let dir = tempfile::tempdir().expect("a folder");
        let error = end(dir.path(), "nobody", GRACE).expect_err("nothing to end");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
