//! What the tests that run `mycelium` as a process of their own share:
//! the command with its folders under a home of the test's own, and a
//! wait for a path the process makes.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};

/// Waits up to `within` for `path` to be there, hearing each change under
/// `root`.
pub fn made(root: &Path, path: &Path, within: Duration) {
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

/// `mycelium` run with `args`, its folders under `home`.
pub fn mycelium(home: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mycelium"));
    command
        .args(["--realm", "work"])
        .args(args)
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", home.join("r"))
        .env("XDG_CONFIG_HOME", home.join("c"))
        .env("XDG_STATE_HOME", home.join("s"))
        .env("XDG_DATA_HOME", home.join("d"))
        .env("XDG_CACHE_HOME", home.join("k"))
        .env_remove("INVOCATION_ID")
        .stdin(Stdio::null());
    command
}
