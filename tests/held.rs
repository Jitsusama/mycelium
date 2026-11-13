//! A holder told to end, as its scope's stop tells it at a shutdown: the
//! holder is a `mycelium` process of its own, as a scope holds it, with a
//! window joined to it as a terminal's would be.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use mycelium::hold::{Said, socket};

/// How long anything here has to happen before the test gives up.
const PATIENCE: Duration = Duration::from_secs(20);

/// How long a holder may take to end once its program has: a shutdown
/// gives every scope five seconds in all, its program's save included,
/// and the holder's own share of that is only letting its windows go.
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
    // does, once it has shown a key typed in the window, so the window is
    // known to be joined.
    let holder = holder(
        home.path(),
        "read typed; echo \"got $typed\"; exec sleep 1000",
    );
    let held = home.path().join("r/mycelium/harbour.held");
    let mut window = UnixStream::connect(socket(&held, "ending1")).expect("a window joins");
    window
        .write_all(&Said::Typed(b"go\r".to_vec()).encode())
        .expect("the window types");
    sees(&mut window, "got go");

    let told = Instant::now();
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
    let (ended, at) = ends.recv_timeout(PATIENCE).expect("the holder ends");
    let ended = ended.expect("the holder is waited on");
    let took = at - told;
    assert!(ended.success(), "the holder ends as told: {ended:?}");
    assert!(
        took < ENDS_WITHIN,
        "the holder lets its window go as its program ends, not later: it took {took:?}"
    );
    let mut rest = Vec::new();
    window
        .read_to_end(&mut rest)
        .expect("the window is let go, its connection closed");
    assert!(
        !held.join("ending1.json").exists(),
        "its record is taken away once it has ended"
    );
}
