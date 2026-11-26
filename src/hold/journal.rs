//! Where a holder says how each run of its program ended, so a program
//! that dies is never gone in silence. A held program's terminal is the
//! holder's own, and a window it showed in may have closed long before,
//! so what it said last can be read nowhere else. Forty pi sessions ended
//! within minutes of each other one night with no trace in either
//! journal, and why took an hour to find (2026-11-11).
//!
//! On a machine with systemd's journal, each end is an entry of its own,
//! with the fields `systemd` itself gives a unit's end (`EXIT_CODE`,
//! `EXIT_STATUS`) beside the hold's; elsewhere, or when the journal
//! won't take it, it is a line on the holder's standard error, which a
//! node's holder shares with the node, so it lands on the node's log.

use std::io::{self, Write};
use std::os::unix::net::UnixDatagram;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use super::holder::{PARKED, RELEASED, UNASKED, UNASKED_SIGNAL};

/// Where the journal takes entries in its own form.
const JOURNAL_SOCKET: &str = "/run/systemd/journal/socket";

/// How one run of a held program ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// It exited with this code.
    Exited(i32),
    /// It parked, to start again once woken.
    Parked,
    /// It quit though nobody asked it to, by its own code or, as a pi
    /// whose exit a name lookup held does, by [`UNASKED_SIGNAL`], and is
    /// started again unless it has quit so too often.
    Unasked {
        /// Whether it quit so by the signal rather than its code.
        by_signal: bool,
    },
    /// A signal ended it, leaving a core or not.
    Killed {
        /// The signal's number.
        signal: i32,
        /// Whether it left a core.
        core: bool,
    },
}

impl How {
    /// How a run that ended as `status` ended.
    #[must_use]
    pub fn of(status: ExitStatus) -> Self {
        match (status.code(), status.signal()) {
            (Some(PARKED), _) => Self::Parked,
            (Some(UNASKED), _) => Self::Unasked { by_signal: false },
            (None, Some(UNASKED_SIGNAL)) => Self::Unasked { by_signal: true },
            (Some(code), _) => Self::Exited(code),
            (None, signal) => Self::Killed {
                signal: signal.unwrap_or_default(),
                core: status.core_dumped(),
            },
        }
    }
}

/// One run of a held program ending, as its holder says it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ended {
    /// The hold's id.
    pub id: String,
    /// The realm it is held in.
    pub realm: String,
    /// The run's process.
    pub pid: u32,
    /// The program that ran, as it was named.
    pub program: String,
    /// How it ended.
    pub how: How,
}

impl Ended {
    /// What it says, in a line.
    #[must_use]
    pub fn line(&self) -> String {
        let how = match self.how {
            How::Exited(code) => format!("exited {code}"),
            How::Parked => format!("parked (exit {PARKED})"),
            How::Unasked { by_signal: false } => format!("quit unasked (exit {UNASKED})"),
            How::Unasked { by_signal: true } => format!(
                "quit unasked (signal {UNASKED_SIGNAL}, {})",
                signal_name(UNASKED_SIGNAL)
            ),
            How::Killed { signal, core } => format!(
                "was killed by signal {signal} ({}){}",
                signal_name(signal),
                if core { ", leaving a core" } else { "" }
            ),
        };
        format!(
            "held {} in {}: {} (pid {}) {how}",
            self.id,
            self.realm,
            one_line(&self.program),
            self.pid
        )
    }

    /// Its entry in the journal's own form: one field a line, in the
    /// words `systemd` gives a unit's end, warning of a failure and
    /// erring on a signal. A viewer's attach whose host released its
    /// conversation ([`RELEASED`]) ended as asked, so it is no failure.
    #[must_use]
    pub fn entry(&self) -> String {
        let fine = |code| code == 0 || code == i32::from(RELEASED);
        let (code, status, priority) = match self.how {
            How::Exited(code) => ("exited", code.to_string(), if fine(code) { 6 } else { 4 }),
            How::Parked => ("exited", PARKED.to_string(), 6),
            // A program quitting unasked gave up on something, so it warns,
            // whether or not it is started again.
            How::Unasked { by_signal: false } => ("exited", UNASKED.to_string(), 4),
            How::Unasked { by_signal: true } => {
                ("killed", signal_name(UNASKED_SIGNAL).to_owned(), 4)
            }
            How::Killed { signal, core } => (
                if core { "dumped" } else { "killed" },
                signal_name(signal).to_owned(),
                3,
            ),
        };
        [
            ("MESSAGE", self.line()),
            ("PRIORITY", priority.to_string()),
            ("SYSLOG_IDENTIFIER", "mycelium".to_owned()),
            ("MYCELIUM_HELD", one_line(&self.id)),
            ("MYCELIUM_REALM", one_line(&self.realm)),
            ("MYCELIUM_PROGRAM", one_line(&self.program)),
            ("MYCELIUM_PROGRAM_PID", self.pid.to_string()),
            ("EXIT_CODE", code.to_owned()),
            ("EXIT_STATUS", status),
        ]
        .iter()
        .flat_map(|(field, value)| [*field, "=", value.as_str(), "\n"])
        .collect()
    }
}

/// A held program parked for quitting unasked too often, rather than
/// started again: said apart from its run's end, which reads as any
/// unasked quit's, since a terminal's holder has no log but the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TooOften {
    /// The hold's id.
    pub id: String,
    /// The realm it is held in.
    pub realm: String,
    /// The program that ran, as it was named.
    pub program: String,
    /// How many times it quit unasked, this last included.
    pub quits: usize,
    /// Within how long.
    pub within: Duration,
}

impl TooOften {
    /// What it says, in a line.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "held {} in {}: {} quit unasked {} times within {} s, so it is parked until a \
             key in its window starts it",
            self.id,
            self.realm,
            one_line(&self.program),
            self.quits,
            self.within.as_secs()
        )
    }

    /// Its entry in the journal's own form, warning, since a program left
    /// parked by its own quits needs someone to look.
    #[must_use]
    pub fn entry(&self) -> String {
        [
            ("MESSAGE", self.line()),
            ("PRIORITY", "4".to_owned()),
            ("SYSLOG_IDENTIFIER", "mycelium".to_owned()),
            ("MYCELIUM_HELD", one_line(&self.id)),
            ("MYCELIUM_REALM", one_line(&self.realm)),
            ("MYCELIUM_PROGRAM", one_line(&self.program)),
            ("MYCELIUM_PARKED", "too-often".to_owned()),
        ]
        .iter()
        .flat_map(|(field, value)| [*field, "=", value.as_str(), "\n"])
        .collect()
    }
}

/// `text` with no line breaks, which the journal's simple form can't
/// hold in a value.
fn one_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

/// A signal's name as `kill -l` says it, for those a program is likeliest
/// to end by, or a question mark.
fn signal_name(signal: i32) -> &'static str {
    // Their numbers differ between Linux and macOS, so they come from
    // the system's own.
    const USR1: i32 = rustix::process::Signal::USR1.as_raw();
    const USR2: i32 = rustix::process::Signal::USR2.as_raw();
    match signal {
        1 => "HUP",
        2 => "INT",
        3 => "QUIT",
        4 => "ILL",
        6 => "ABRT",
        7 => "BUS",
        8 => "FPE",
        9 => "KILL",
        11 => "SEGV",
        13 => "PIPE",
        14 => "ALRM",
        15 => "TERM",
        USR1 => "USR1",
        USR2 => "USR2",
        _ => "?",
    }
}

/// Where a holder says how each run of its program ended.
pub trait Journal: Send + Sync {
    /// Says `ended`.
    fn ended(&self, ended: &Ended);

    /// Says the program was parked for quitting unasked too often.
    fn too_often(&self, parked: &TooOften);
}

/// The machine's journal, or the holder's standard error where there is
/// none.
#[derive(Debug, Clone)]
pub struct SystemJournal {
    socket: PathBuf,
}

impl SystemJournal {
    /// The machine's own journal.
    #[must_use]
    pub fn here() -> Self {
        Self::at(Path::new(JOURNAL_SOCKET))
    }

    /// A journal taking entries on `socket`.
    #[must_use]
    pub fn at(socket: &Path) -> Self {
        Self {
            socket: socket.to_owned(),
        }
    }

    fn send(&self, entry: &str) -> io::Result<()> {
        UnixDatagram::unbound()?
            .send_to(entry.as_bytes(), &self.socket)
            .map(drop)
    }
}

impl Journal for SystemJournal {
    fn ended(&self, ended: &Ended) {
        if self.send(&ended.entry()).is_err() {
            // Nothing is left to say it to when even standard error is
            // gone.
            let _ = writeln!(io::stderr(), "mycelium: {}", ended.line());
        }
    }

    fn too_often(&self, parked: &TooOften) {
        if self.send(&parked.entry()).is_err() {
            // As with an end, standard error is all that's left.
            let _ = writeln!(io::stderr(), "mycelium: {}", parked.line());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ended(how: How) -> Ended {
        Ended {
            id: "trial1".to_owned(),
            realm: "harbour".to_owned(),
            pid: 42,
            program: "pi".to_owned(),
            how,
        }
    }

    #[test]
    fn a_park_for_quitting_too_often_warns_naming_its_bound_and_what_wakes_it() {
        let parked = TooOften {
            id: "trial1".to_owned(),
            realm: "harbour".to_owned(),
            program: "pi\n--session tern".to_owned(),
            quits: 4,
            within: Duration::from_secs(60),
        };
        let entry = parked.entry();
        assert!(
            entry.contains("PRIORITY=4\n") && entry.contains("MYCELIUM_PARKED=too-often\n"),
            "it warns, marked as this park: {entry:?}"
        );
        assert!(
            entry.contains(
                "MESSAGE=held trial1 in harbour: pi --session tern quit unasked 4 times \
                 within 60 s, so it is parked until a key in its window starts it\n"
            ),
            "its line says the bound and the key, on one line: {entry:?}"
        );
    }

    #[test]
    fn each_end_is_said_in_the_words_systemd_gives_a_units_end() {
        let killed = ended(How::Killed {
            signal: 9,
            core: false,
        });
        assert_eq!(
            killed.line(),
            "held trial1 in harbour: pi (pid 42) was killed by signal 9 (KILL)",
            "a kill says its signal by name"
        );
        let entry = killed.entry();
        for field in [
            "PRIORITY=3\n",
            "EXIT_CODE=killed\n",
            "EXIT_STATUS=KILL\n",
            "MYCELIUM_HELD=trial1\n",
            "MYCELIUM_PROGRAM_PID=42\n",
        ] {
            assert!(entry.contains(field), "{field:?} in {entry:?}");
        }
        assert!(
            ended(How::Exited(3)).entry().contains("PRIORITY=6\n"),
            "a released viewer is no failure"
        );
        let failed = ended(How::Exited(1)).entry();
        assert!(
            failed.contains("EXIT_CODE=exited\nEXIT_STATUS=1\n") && failed.contains("PRIORITY=4\n"),
            "a failure warns: {failed:?}"
        );
        assert!(
            ended(How::Parked).line().ends_with("parked (exit 75)"),
            "a park says it parked"
        );
        let unasked = ended(How::of(ExitStatus::from_raw(UNASKED << 8)));
        assert!(
            unasked.line().ends_with("quit unasked (exit 79)")
                && unasked.entry().contains("EXIT_STATUS=79\n")
                && unasked.entry().contains("PRIORITY=4\n"),
            "an unasked quit says so, and warns"
        );
        let signalled = ended(How::of(ExitStatus::from_raw(UNASKED_SIGNAL)));
        assert!(
            signalled
                .line()
                .ends_with(&format!("quit unasked (signal {UNASKED_SIGNAL}, USR2)")),
            "one quitting so by its signal says so too: {}",
            signalled.line()
        );
        assert!(
            signalled
                .entry()
                .contains("EXIT_CODE=killed\nEXIT_STATUS=USR2\n")
                && signalled.entry().contains("PRIORITY=4\n"),
            "and warns as the code does, not erring as a kill: {}",
            signalled.entry()
        );
        assert_eq!(
            How::of(ExitStatus::from_raw(rustix::process::Signal::USR1.as_raw())),
            How::Killed {
                signal: rustix::process::Signal::USR1.as_raw(),
                core: false
            },
            "another signal is a kill"
        );
        let dumped = ended(How::Killed {
            signal: 11,
            core: true,
        });
        assert!(
            dumped
                .entry()
                .contains("EXIT_CODE=dumped\nEXIT_STATUS=SEGV\n"),
            "a core is said: {}",
            dumped.entry()
        );
        let odd = Ended {
            program: "pi\nrm".to_owned(),
            ..ended(How::Exited(0))
        };
        assert!(
            odd.entry().contains("MYCELIUM_PROGRAM=pi rm\n"),
            "a value keeps to its line: {}",
            odd.entry()
        );
    }

    #[test]
    fn an_end_reaches_the_journal_as_one_entry() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("journal");
        let journal = UnixDatagram::bind(&socket).expect("a journal listening");
        SystemJournal::at(&socket).ended(&ended(How::Exited(3)));
        let mut said = vec![0; 4096];
        let read = journal.recv(&mut said).expect("an entry");
        let entry = String::from_utf8_lossy(&said[..read]);
        assert_eq!(
            entry,
            ended(How::Exited(3)).entry(),
            "the whole entry, in one datagram"
        );
    }
}
