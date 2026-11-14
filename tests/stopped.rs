//! A node told to stop as its service manager tells it, by the terminate
//! signal, with a client still watching it: it ends its links, lets go of
//! what they carried and takes its socket away before it ends, and ends
//! well, though what its watchers hold is left for its process's end to
//! free.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};

/// How long the node has to start, and then to stop.
const PATIENCE: Duration = Duration::from_secs(30);

/// Waits up to `within` for `path` to be there, hearing each change under
/// `root`.
fn made(root: &Path, path: &Path, within: Duration) {
    let (telling, told) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(telling).expect("a watcher");
    watcher
        .watch(root, RecursiveMode::Recursive)
        .expect("the folder is watched");
    let deadline = Instant::now() + within;
    while !path.exists() {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "{} was never made", path.display());
        // A watch that fails or times out has the path looked at again.
        let _ = told.recv_timeout(left);
    }
}

#[test]
fn a_node_told_to_stop_while_watched_lets_go_and_ends_well() {
    let home = tempfile::tempdir().expect("a home");
    let home = home.path();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home.join("r"))
        .expect("its runtime folder is made");
    let said = home.join("said");
    let mut node = Command::new(env!("CARGO_BIN_EXE_mycelium"))
        .args(["--realm", "work", "node"])
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", home.join("r"))
        .env("XDG_CONFIG_HOME", home.join("c"))
        .env("XDG_STATE_HOME", home.join("s"))
        .env("XDG_DATA_HOME", home.join("d"))
        .env("XDG_CACHE_HOME", home.join("k"))
        .env_remove("INVOCATION_ID")
        .stdin(Stdio::null())
        .stderr(fs::File::create(&said).expect("somewhere to say"))
        .spawn()
        .expect("the node starts");
    let socket = home.join("r").join("mycelium").join("work.sock");
    made(&home.join("r"), &socket, PATIENCE);

    let mut watching = UnixStream::connect(&socket).expect("the node answers");
    watching
        .write_all(b"{\"Watch\":{\"groups\":\">\",\"recent\":0}}\n")
        .expect("a watch asked");
    let mut first = String::new();
    BufReader::new(&watching)
        .read_line(&mut first)
        .expect("the watch answered");
    assert!(
        first.contains("Watching"),
        "the watch wasn't answered: {first}"
    );

    let pid = rustix::process::Pid::from_child(&node);
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).expect("told to stop");
    let (ending, ended) = mpsc::channel::<ExitStatus>();
    std::thread::spawn(move || {
        let _ = ending.send(node.wait().expect("it ends"));
    });
    let status = ended
        .recv_timeout(PATIENCE)
        .expect("the node ends once told to stop");

    let said = fs::read_to_string(&said).unwrap_or_default();
    assert!(status.success(), "the node ended {status}: {said}");
    assert!(!socket.exists(), "the node left its socket behind");
    assert!(
        said.contains("stopped: its links ended in"),
        "the node didn't say how its stop went: {said}"
    );
}
