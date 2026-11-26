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

use std::io;
use std::os::unix::net::UnixDatagram;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use super::holder::{PARKED, RELEASED, logs_to};

/// Where the journal takes entries in its own form.
const JOURNAL_SOCKET: &str = "/run/systemd/journal/socket";

/// How one run of a held program ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// It exited with this code.
    Exited(i32),
    /// It parked, to start again once woken.
    Parked,
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
    /// What it says, in a line, each name [`named`], so none can end its
    /// entry's `MESSAGE` and start a field of its own, or reach whoever
    /// reads it with a control.
    #[must_use]
    pub fn line(&self) -> String {
        let how = match self.how {
            How::Exited(code) => format!("exited {code}"),
            How::Parked => format!("parked (exit {PARKED})"),
            How::Killed { signal, core } => format!(
                "was killed by signal {signal} ({}){}",
                signal_name(signal),
                if core { ", leaving a core" } else { "" }
            ),
        };
        format!(
            "held {} in {}: {} (pid {}) {how}",
            named(&self.id),
            named(&self.realm),
            named(&self.program),
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
            ("MYCELIUM_HELD", named(&self.id)),
            ("MYCELIUM_REALM", named(&self.realm)),
            ("MYCELIUM_PROGRAM", named(&self.program)),
            ("MYCELIUM_PROGRAM_PID", self.pid.to_string()),
            ("EXIT_CODE", code.to_owned()),
            ("EXIT_STATUS", status),
        ]
        .iter()
        .flat_map(|(field, value)| [*field, "=", value.as_str(), "\n"])
        .collect()
    }
}

/// A name a hold was given, as the node's log and the journal both
/// write it: each control, backslash and character that doesn't show
/// escaped as Rust writes it, so none breaks a line, starts a field of
/// the journal's simple form or turns the text around it, and every
/// other letter, quotes included, as itself. The holder's quotes of a
/// program's output are escaped as bytes instead, as those needn't be
/// text.
pub(super) fn named(text: &str) -> String {
    text.chars()
        .fold(String::with_capacity(text.len()), |mut kept, letter| {
            if matches!(letter, '\'' | '"') {
                kept.push(letter);
            } else {
                kept.extend(letter.escape_debug());
            }
            kept
        })
}

/// A signal's name as `kill -l` says it, for those a program is likeliest
/// to end by, or a question mark.
fn signal_name(signal: i32) -> &'static str {
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
        _ => "?",
    }
}

/// Where a holder says how each run of its program ended.
pub trait Journal: Send + Sync {
    /// Says `ended`.
    fn ended(&self, ended: &Ended);
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
            // gone. Said as the holder's own lines are, in one write, so
            // another holder's can't split it.
            logs_to(&mut io::stderr().lock(), &said_instead(ended));
        }
    }
}

/// What standard error is told of `ended` when the journal can't be:
/// its line, each name in it written as the node's log writes it.
fn said_instead(ended: &Ended) -> String {
    format!("mycelium: {}", ended.line())
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
            odd.entry().contains("MYCELIUM_PROGRAM=pi\\nrm\n"),
            "a value keeps to its line: {}",
            odd.entry()
        );
        let named = Ended {
            id: "trial1\nPRIORITY=0".to_owned(),
            realm: "harbour\rX".to_owned(),
            ..ended(How::Exited(0))
        };
        assert!(
            named
                .entry()
                .starts_with("MESSAGE=held trial1\\nPRIORITY=0 in harbour\\rX: pi (pid 42)"),
            "a name keeps to the message's line: {}",
            named.entry()
        );
    }

    #[test]
    fn standard_error_is_told_an_end_escaped_in_one_line() {
        let odd = Ended {
            id: "ana's".to_owned(),
            program: "pi\x1b]0;caught\x07 \u{e9}".to_owned(),
            ..ended(How::Exited(1))
        };
        let mut told = Vec::new();
        logs_to(&mut told, &said_instead(&odd));
        assert_eq!(
            String::from_utf8(told).expect("text"),
            "mycelium: held ana's in harbour: pi\\u{1b}]0;caught\\u{7} \u{e9} (pid 42) exited 1\n",
            "its controls escaped, its letters and quotes as they are, its line end with it"
        );
        assert_eq!(
            odd.entry().lines().next(),
            Some(
                "MESSAGE=held ana's in harbour: pi\\u{1b}]0;caught\\u{7} \u{e9} (pid 42) exited 1"
            ),
            "the journal is given the same line, no control reaching it raw"
        );
    }

    #[test]
    fn a_name_is_written_one_way_with_nothing_in_it_turning_the_text() {
        assert_eq!(named("trial1"), "trial1", "a plain name is as it was given");
        assert_eq!(
            named("ana's \"pi\" \u{e9}"),
            "ana's \"pi\" \u{e9}",
            "quotes and letters as themselves"
        );
        assert_eq!(
            named("a\\b\nc\rd\x1b\u{202e}e"),
            "a\\\\b\\nc\\rd\\u{1b}\\u{202e}e",
            "a backslash, line breaks, a control and a right-to-left override escaped"
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
