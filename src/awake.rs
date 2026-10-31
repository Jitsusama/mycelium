//! Keeping a machine awake while it holds work. A laptop lent to the
//! realm sleeps when it sits idle, and every session held there stops
//! with it, though nobody there is using it. So while a node holds work,
//! it keeps its machine from sleeping when idle, and lets it once what
//! it holds is all idle, or once its battery runs low. A person shutting
//! the lid or asking it to sleep always wins: a laptop kept awake with
//! its lid shut would run hot in a bag.
//!
//! The machine is kept awake by a program of the system's own that holds
//! it so while it runs: `systemd-inhibit` on Linux, `caffeinate` on macOS.
//! That program runs a child reading the node's pipe, so the hold ends
//! with the node, however the node ends.

use crate::facts::Power;
use crate::hold::Held;
use crate::record::{Session, State};
use std::collections::BTreeSet;
use std::process::Stdio;
use tokio::process::{Child, Command};

/// The charge, in percent, below which a machine on battery is let sleep.
pub const LOW_CHARGE: u8 = 20;

/// Whether any of what is `held` here is at work, as `sessions` and the
/// sessions with work in the machine's queue, `queued`, say. A program
/// held that is no session, such as a build sent here, is at work while
/// it runs; a session is while it runs a turn or has work queued, and
/// not while it waits idle, which could keep a laptop awake for days.
#[must_use]
pub fn at_work(held: &[Held], sessions: &[Session], queued: &BTreeSet<String>) -> bool {
    held.iter().any(|one| {
        sessions
            .iter()
            .find(|session| session.held.as_deref() == Some(one.id.as_str()))
            .is_none_or(|session| session.state == State::Busy || queued.contains(&session.id))
    })
}

/// Whether a machine that holds work or not, as `holds_work` says, with
/// `power` as it reads, is to be kept awake. A machine whose charge it
/// can't read is kept, as one plugged in is.
#[must_use]
pub fn wanted(holds_work: bool, power: Option<&Power>) -> bool {
    holds_work && !power.is_some_and(low)
}

/// Whether a machine with `power` runs on its battery below the mark, so
/// is let sleep and takes no new work. One whose charge it can't read
/// isn't, as one plugged in isn't.
#[must_use]
pub fn low(power: &Power) -> bool {
    power.on_battery && power.charge.is_some_and(|charge| charge < LOW_CHARGE)
}

/// The words that keep a machine running `os`, as Rust names it, awake
/// while they run, the last of them a program reading until its input
/// ends. None where nothing here knows how.
#[must_use]
pub fn keeping(os: &str) -> Option<Vec<String>> {
    let words: &[&str] = match os {
        "linux" => &[
            "systemd-inhibit",
            "--what=idle",
            "--mode=block",
            "--who=mycelium",
            "--why=It holds sessions for the realm",
            "cat",
        ],
        // Idle sleep only: a Mac's lid always sleeps it.
        "macos" => &["/usr/bin/caffeinate", "-i", "/bin/cat"],
        _ => return None,
    };
    Some(words.iter().map(|word| (*word).to_owned()).collect())
}

/// A machine kept awake, or not.
pub struct Awake {
    /// The words that keep it awake, if any do here.
    words: Option<Vec<String>>,
    /// What keeps it awake now.
    keeper: Option<Child>,
    /// Whether keeping it awake failed since it was last let sleep, so it
    /// isn't tried again, and said again, every minute.
    failed: bool,
}

/// What a call to [`Awake::keep`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Kept {
    /// It began keeping the machine awake.
    Began,
    /// It already kept it awake, or tried and couldn't, or wasn't asked to.
    Stayed,
    /// It let the machine sleep.
    LetSleep,
}

impl Awake {
    /// A machine kept awake by `words`, as [`keeping`] gives them.
    #[must_use]
    pub const fn by(words: Option<Vec<String>>) -> Self {
        Self {
            words,
            keeper: None,
            failed: false,
        }
    }

    /// This machine, kept awake as its system does it.
    #[must_use]
    pub fn here() -> Self {
        Self::by(keeping(std::env::consts::OS))
    }

    /// Keeps the machine awake if `wanted`, and lets it sleep if not.
    pub fn keep(&mut self, wanted: bool) -> Kept {
        if !wanted {
            self.failed = false;
            return if self.let_sleep() {
                Kept::LetSleep
            } else {
                Kept::Stayed
            };
        }
        if let Some(keeper) = &mut self.keeper {
            match keeper.try_wait() {
                Ok(None) => return Kept::Stayed,
                Ok(Some(status)) => {
                    eprintln!("this machine may sleep with work held: its keeper ended, {status}");
                }
                Err(error) => {
                    eprintln!("this machine may sleep with work held: its keeper is lost: {error}");
                }
            }
            self.keeper = None;
            self.failed = true;
        }
        if self.failed {
            return Kept::Stayed;
        }
        let Some((program, args)) = self.words.as_ref().and_then(|words| words.split_first())
        else {
            return Kept::Stayed;
        };
        match Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(keeper) => {
                self.keeper = Some(keeper);
                Kept::Began
            }
            Err(error) => {
                eprintln!("this machine may sleep with work held: {program} didn't start: {error}");
                self.failed = true;
                Kept::Stayed
            }
        }
    }

    /// Ends what keeps the machine awake, if anything does, answering
    /// whether something did.
    fn let_sleep(&mut self) -> bool {
        let Some(mut keeper) = self.keeper.take() else {
            return false;
        };
        // Its input ending ends its reader, and with it the hold; it is
        // reaped once it has.
        drop(keeper.stdin.take());
        tokio::spawn(async move {
            // It ends either way; what it said on ending tells nothing.
            let _ = keeper.wait().await;
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    /// How long a test waits for a keeper to do what it says.
    const PATIENCE: Duration = Duration::from_secs(5);

    /// A program held here as `id`.
    fn held(id: &str) -> Held {
        serde_json::from_value(serde_json::json!({
            "id": id, "command": ["pi"], "folder": "/", "holder": 1, "pid": 2,
            "since": "2026-10-30T20:00:00Z"
        }))
        .expect("a held program")
    }

    /// Session `id`, held as `held`, standing as `state` says.
    fn session(id: &str, held: &str, state: &str) -> Session {
        serde_json::from_value(serde_json::json!({
            "id": id, "address": id, "machine": "penelope", "groups": [],
            "state": state, "held": held, "at": "2026-10-30T20:00:00Z"
        }))
        .expect("a session")
    }

    #[test]
    fn only_what_is_held_and_at_work_keeps_a_machine_awake() {
        let nothing = BTreeSet::new();
        let idle = [session("idle-one", "a", "idle")];
        assert!(
            !at_work(&[held("a")], &idle, &nothing),
            "a session held but waiting idle isn't at work"
        );
        assert!(
            at_work(
                &[held("a")],
                &idle,
                &BTreeSet::from(["idle-one".to_owned()])
            ),
            "one with work in its machine's queue is, though its turn has ended"
        );
        assert!(
            at_work(&[held("a")], &[session("busy-one", "a", "busy")], &nothing),
            "one running a turn is"
        );
        assert!(
            at_work(&[held("a"), held("b")], &idle, &nothing),
            "a program held that is no session, such as a build, is"
        );
        assert!(
            !at_work(&[], &idle, &nothing),
            "nothing held is nothing at work"
        );
    }

    /// Words that note in `said` when they begin and when their input
    /// ends, as a keeper's would.
    fn noting(said: &Path) -> Vec<String> {
        let said = said.display();
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!("echo began >> {said}; cat; echo ended >> {said}"),
        ]
    }

    /// Waits until `said` holds `lines`, answering what it holds.
    async fn until_said(said: &Path, lines: &str) -> String {
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            let now = fs::read_to_string(said).unwrap_or_default();
            if now == lines || tokio::time::Instant::now() > deadline {
                return now;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[test]
    fn a_machine_is_kept_from_sleeping_when_idle_and_never_when_told_to() {
        let linux = keeping("linux").expect("Linux keeps a machine awake");
        let what: Vec<&String> = linux
            .iter()
            .filter(|word| word.starts_with("--what="))
            .collect();
        assert_eq!(
            what,
            ["--what=idle"],
            "only idle sleep waits, never the lid or a person's suspend: {linux:?}"
        );
        let macos = keeping("macos").expect("macOS keeps a machine awake");
        assert_eq!(
            macos[1], "-i",
            "a Mac is kept from idle sleep alone: {macos:?}"
        );
        assert_eq!(
            keeping("plan9"),
            None,
            "nothing is kept where none knows how"
        );
    }

    #[test]
    fn a_machine_is_kept_awake_only_while_it_holds_work_and_has_the_charge() {
        let power = |on_battery, charge| Power { on_battery, charge };
        assert!(!wanted(false, None), "a machine holding nothing may sleep");
        assert!(
            wanted(true, None),
            "one holding work with no battery is kept"
        );
        assert!(
            wanted(true, Some(&power(false, Some(5)))),
            "one plugged in is kept, however low its charge"
        );
        assert!(
            wanted(true, Some(&power(true, Some(LOW_CHARGE)))),
            "one on battery at the mark is kept"
        );
        assert!(
            !wanted(true, Some(&power(true, Some(LOW_CHARGE - 1)))),
            "one on battery below the mark may sleep"
        );
        assert!(
            wanted(true, Some(&power(true, None))),
            "one whose charge can't be read is kept"
        );
    }

    #[tokio::test]
    async fn a_machine_is_kept_awake_once_and_let_sleep_when_asked() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let said = dir.path().join("said");
        let mut awake = Awake::by(Some(noting(&said)));

        assert_eq!(awake.keep(true), Kept::Began, "asked first, it begins");
        assert_eq!(
            until_said(&said, "began\n").await,
            "began\n",
            "the keeper runs"
        );
        assert_eq!(
            awake.keep(true),
            Kept::Stayed,
            "asked again, it keeps the one"
        );
        assert_eq!(
            awake.keep(false),
            Kept::LetSleep,
            "told no more, it lets go"
        );
        assert_eq!(
            until_said(&said, "began\nended\n").await,
            "began\nended\n",
            "and the keeper's input ends"
        );
        assert_eq!(
            awake.keep(false),
            Kept::Stayed,
            "with nothing kept, nothing to let go"
        );
    }

    #[tokio::test]
    async fn a_keeper_that_ends_unasked_is_not_started_again_until_let_go() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let said = dir.path().join("said");
        let words = Some(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!("echo began >> {}; exit 1", said.display()),
        ]);
        let mut awake = Awake::by(words);

        assert_eq!(awake.keep(true), Kept::Began, "asked first, it begins");
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while awake
            .keeper
            .as_mut()
            .is_some_and(|keeper| matches!(keeper.try_wait(), Ok(None)))
        {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the keeper never ended"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            awake.keep(true),
            Kept::Stayed,
            "a keeper the system refused isn't asked for again every minute"
        );
        awake.keep(false);
        assert_eq!(
            awake.keep(true),
            Kept::Began,
            "once let go, new work tries again"
        );
    }
}
