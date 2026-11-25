//! Programs the tests stand in for, put in place whole.
//!
//! A test that writes a program and then runs it races every other test
//! thread: one that forks while the file is still open for writing hands
//! its child that descriptor, and until the child runs its own program,
//! running the written one fails with "Text file busy". So a child
//! process writes it, and no descriptor open for writing it ever lives in
//! the tests' own process.
//!
//! And processes the tests stand up for a holder or its program, which
//! never outlive the test that started them ([`Process`]).

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// A process a test starts, in a process group of its own, which it and
/// every process it started are killed with as this drops, a failed
/// assert's unwinding included. One left running past a failed test kept
/// the output the build reads from open, so the build waited on it for as
/// long as it slept. It is waited on as it ends, as a holder's parent
/// waits on it, so it never lingers as a zombie that still seems to run.
pub struct Process(u32);

impl Process {
    /// Starts `command` with nothing to read, in a group of its own.
    ///
    /// # Panics
    ///
    /// When it can't be started, since a test without it proves nothing.
    pub fn start(command: &mut Command) -> Self {
        let child = command
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("the stand-in process starts");
        Self::of(child)
    }

    /// Takes on `child`, started in a process group of its own, to wait
    /// on and to kill with its group as this drops.
    pub fn of(mut child: std::process::Child) -> Self {
        let pid = child.id();
        std::thread::spawn(move || child.wait());
        Self(pid)
    }

    /// Its process id, which is its group's too.
    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.0
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let group = i32::try_from(self.0)
            .ok()
            .and_then(rustix::process::Pid::from_raw);
        if let Some(group) = group {
            // A group already gone, as a test that ended it leaves it, is
            // nothing to kill.
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
    }
}

/// Puts a program saying `text` at `path`, runnable, written by a child.
/// Its `chmod` is given no `--`, which a Mac's takes for a file's name;
/// `path` is a test's own, never one starting with a dash.
///
/// # Panics
///
/// When the child can't be started or doesn't put it in place, since a
/// test without its stand-in proves nothing.
pub fn program(path: &Path, text: &str) {
    let mut child = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("a shell puts the stand-in in place");
    child
        .stdin
        .take()
        .expect("the shell's input")
        .write_all(text.as_bytes())
        .unwrap_or_else(|error| {
            panic!("the stand-in {} is told the shell: {error}", path.display())
        });
    let put = child.wait().expect("the shell ends");
    assert!(put.success(), "the stand-in {} is in place", path.display());
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_test_failing_while_its_process_runs_leaves_nothing_holding_its_output() {
        let (mut read, write) = std::io::pipe().expect("a pipe");
        let failed = std::panic::catch_unwind(move || {
            let _sleeping = Process::start(
                Command::new("sh")
                    .args(["-c", "sleep 1000 & wait"])
                    .stdout(write),
            );
            panic!("a test fails while its process runs");
        });
        assert!(failed.is_err(), "the test failed");
        let (closed, ends) = mpsc::channel();
        std::thread::spawn(move || {
            let mut rest = Vec::new();
            let _ = closed.send(read.read_to_end(&mut rest).is_ok());
        });
        assert_eq!(
            ends.recv_timeout(Duration::from_secs(5)),
            Ok(true),
            "the output the build reads from closes within 5 s, its holders all killed"
        );
    }
}
