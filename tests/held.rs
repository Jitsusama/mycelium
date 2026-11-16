//! A holder told to end, as its scope's stop tells it at a shutdown, and
//! one whose window closed, which isn't told: the holder is a `mycelium`
//! process of its own, as a scope holds it, with a window joined to it as
//! a terminal's would be.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use mycelium::hold::{Said, socket};

/// How long anything here has to happen before the test gives up.
const PATIENCE: Duration = Duration::from_secs(20);

/// How long a holder may take to let its window go once its program has
/// said its last: a shutdown gives every scope five seconds in all, its
/// program's save included, and the holder's own share of that is only
/// letting its windows go. It's timed from the program's own last words
/// rather than from the signal, so a loaded machine's scheduling of the
/// program's end isn't counted against the holder.
const ENDS_WITHIN: Duration = Duration::from_millis(400);

/// A process that is stopped when the test is done with it.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        // One that ended already has nothing to stop.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A holder of `program` run as a machine whose runtime folder is under
/// `home`, answering once it says it holds.
fn holder(home: &Path, program: &str) -> Running {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home.join("r"))
        .expect("its runtime folder is made");
    let mut holder = Command::new(env!("CARGO_BIN_EXE_mycelium"))
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", home.join("r"))
        .env("XDG_STATE_HOME", home.join("s"))
        .args([
            "--realm", "harbour", "holder", "--id", "ending1", "--folder",
        ])
        .arg(home)
        .args(["--", "sh", "-c", program])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the holder starts");
    let mut said = String::new();
    BufReader::new(holder.stdout.take().expect("its standard output"))
        .read_line(&mut said)
        .expect("the holder says how it went");
    assert_eq!(said.trim(), "held", "the holder holds its program");
    Running(holder)
}

/// Reads `window` until it has shown `wanted`, failing past the test's
/// patience with what it did show.
fn sees(window: &mut UnixStream, wanted: &str) {
    window
        .set_read_timeout(Some(PATIENCE))
        .expect("the window's patience is set");
    let mut seen = String::new();
    let mut buffer = [0u8; 4096];
    while !seen.contains(wanted) {
        let read = window.read(&mut buffer).unwrap_or(0);
        assert!(
            read > 0,
            "the window never showed {wanted:?}; it showed {seen:?}"
        );
        seen.push_str(&String::from_utf8_lossy(&buffer[..read]));
    }
}

#[test]
fn a_holder_told_to_end_with_a_window_joined_ends_as_its_program_does() {
    let home = tempfile::tempdir().expect("a temporary home");
    // It ends the moment it's told, as a program with nothing left to save
    // does, saying so as its last words, once it has shown a key typed in
    // the window, so the window is known to be joined.
    let holder = holder(
        home.path(),
        "trap 'kill $!; echo bye; exit 0' TERM HUP INT; read typed; echo \"got $typed\"; \
         sleep 1000 & wait",
    );
    let held = home.path().join("r/mycelium/harbour.held");
    let mut window = UnixStream::connect(socket(&held, "ending1")).expect("a window joins");
    window
        .write_all(&Said::Typed(b"go\r".to_vec()).encode())
        .expect("the window types");
    sees(&mut window, "got go");

    let pid = rustix::process::Pid::from_raw(
        i32::try_from(holder.0.id()).expect("a process id fits a pid"),
    )
    .expect("the holder has a process id");
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)
        .expect("the holder is told to end, as its scope's stop tells it");
    let (ending, ends) = mpsc::channel();
    std::thread::spawn(move || {
        let mut holder = holder;
        let ended = holder.0.wait();
        // The test hears it only while it still waits.
        let _ = ending.send((ended, Instant::now()));
    });
    sees(&mut window, "bye");
    let said_its_last = Instant::now();
    let mut rest = Vec::new();
    window
        .read_to_end(&mut rest)
        .expect("the window is let go, its connection closed");
    let took = said_its_last.elapsed();
    assert!(
        took < ENDS_WITHIN,
        "the holder lets its window go as its program ends, not later: it took {took:?}"
    );
    let (ended, _) = ends.recv_timeout(PATIENCE).expect("the holder ends");
    let ended = ended.expect("the holder is waited on");
    assert!(ended.success(), "the holder ends as told: {ended:?}");
    assert!(
        !held.join("ending1.json").exists(),
        "its record is taken away once it has ended"
    );
}

/// A process known only by its id, as a shell's child is, killed when the
/// test is done with it, so a failing test leaves nothing running.
struct Killed(rustix::process::Pid);

impl Drop for Killed {
    fn drop(&mut self) {
        // One that ended already has nothing to kill.
        let _ = rustix::process::kill_process(self.0, rustix::process::Signal::KILL);
    }
}

/// Whether this test's own process ignores hangups, as one run under
/// `nohup` does: a shell it starts inherits that, and can't be ended by
/// one. Read where the kernel says it; a system that doesn't say is taken
/// not to ignore them.
fn hangups_ignored() -> bool {
    const HANGUP_BIT: u64 = 1;
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            let ignored = status
                .lines()
                .find_map(|line| line.strip_prefix("SigIgn:"))?;
            u64::from_str_radix(ignored.trim(), 16).ok()
        })
        .is_some_and(|ignored| ignored & HANGUP_BIT != 0)
}

/// Closing a window hangs up on the terminal it ran, as a terminal closing
/// one and an ssh that drops alike do: the hangup goes to the process
/// group in front, the shell and what it started. A holder started from
/// that terminal, as `holds add` starts one, leaves the terminal's group
/// and session as it starts (its `setsid`), so the hangup never reaches
/// it, and its program carries on, held, for another window to join (Joel,
/// 16 November: closing a window never stops its session, it only gives
/// back the room on the screen).
#[test]
fn a_holder_outlives_the_hangup_of_the_terminal_it_was_started_from() {
    let home = tempfile::tempdir().expect("a temporary home");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home.path().join("r"))
        .expect("its runtime folder is made");
    // The terminal's shell, in a group of its own as a terminal's job in
    // front is, waiting on the holder it started, so it says the holder's
    // id and ends when the holder does; its program answers each line.
    let mut terminal = Command::new("sh")
        .env("HOME", home.path())
        .env("XDG_RUNTIME_DIR", home.path().join("r"))
        .env("XDG_STATE_HOME", home.path().join("s"))
        .args([
            "-c",
            "\"$0\" --realm harbour holder --id hangup1 --folder \"$1\" -- sh -c \
             'while read typed; do echo \"got $typed\"; done' & echo \"holder $!\"; wait",
            env!("CARGO_BIN_EXE_mycelium"),
        ])
        .arg(home.path())
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the terminal's shell starts");
    let terminal_id = terminal.id();
    let mut lines = BufReader::new(terminal.stdout.take().expect("its standard output"));
    let terminal = Running(terminal);
    // The shell says the holder's id and the holder that it holds, in
    // whichever order they come.
    let (mut holder, mut holds) = (None, false);
    for _ in 0..2 {
        let mut line = String::new();
        lines
            .read_line(&mut line)
            .expect("the shell says how it went");
        match line.trim().strip_prefix("holder ") {
            Some(id) => holder = id.parse::<i32>().ok(),
            None => holds = line.trim() == "held",
        }
    }
    let holder = Killed(
        rustix::process::Pid::from_raw(holder.expect("the shell says the holder's id"))
            .expect("the holder has a process id"),
    );
    assert!(holds, "the holder holds its program");

    // A window joined, as the terminal's own was.
    let held = home.path().join("r/mycelium/harbour.held");
    let mut window = UnixStream::connect(socket(&held, "hangup1")).expect("a window joins");
    window
        .write_all(&Said::Typed(b"first\r".to_vec()).encode())
        .expect("the window types");
    sees(&mut window, "got first");

    let group = rustix::process::Pid::from_raw(
        i32::try_from(terminal_id).expect("a process id fits a pid"),
    )
    .expect("the terminal's shell has a process id");
    // Waited on below by its own thread, which a shell that stays would
    // hold for good, so it's killed by its id as well.
    let _shell = Killed(group);
    rustix::process::kill_process_group(group, rustix::process::Signal::HUP)
        .expect("the terminal is hung up on, as its window closing does");
    drop(window);
    let (hung, hangs) = mpsc::channel();
    std::thread::spawn(move || {
        let mut terminal = terminal;
        // The test hears it only while it still waits.
        let _ = hung.send(terminal.0.wait());
    });
    // A runner that ignores hangups passes that on to the shell, which
    // then stays, so only where they reach it is its end the proof that
    // the hangup landed.
    if !hangups_ignored() {
        let hung = hangs
            .recv_timeout(PATIENCE)
            .expect("the terminal's shell goes with its window")
            .expect("the terminal's shell is waited on");
        assert!(
            !hung.success(),
            "the hangup ended the terminal's shell: {hung:?}"
        );
    }

    assert!(
        rustix::process::test_kill_process(holder.0).is_ok(),
        "the holder still runs once its window has gone"
    );
    assert!(
        held.join("hangup1.json").exists(),
        "what is held is still listed once its window has gone"
    );
    let mut window =
        UnixStream::connect(socket(&held, "hangup1")).expect("another window joins it");
    window
        .write_all(&Said::Typed(b"again\r".to_vec()).encode())
        .expect("the window types");
    sees(&mut window, "got again");
}
