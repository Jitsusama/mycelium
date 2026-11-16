//! `mycelium receptors say` as a receptor runs it: what it leaves on disk,
//! and that it takes nothing but a source, a state, counts and times, so
//! nothing a receptor read can reach the file through it.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Output};

/// Runs `mycelium --realm work receptors say` and `words`, keeping its
/// state in `dir`, with no node running.
fn say(dir: &Path, words: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mycelium"))
        .args(["--realm", "work", "receptors", "say"])
        .args(words)
        .arg("--state-folder")
        .arg(dir)
        .env("HOME", dir)
        .env("XDG_RUNTIME_DIR", dir)
        .env("XDG_STATE_HOME", dir)
        .output()
        .expect("mycelium runs")
}

#[test]
fn a_receptor_says_how_it_stands_in_a_file_its_owners_alone() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let pid = std::process::id().to_string();
    let said = say(
        dir.path(),
        &[
            "slack",
            "--state",
            "reading",
            "--hour",
            "1",
            "--today",
            "4",
            "--addressed",
            "2",
            "--every",
            "300",
            "--pid",
            &pid,
        ],
    );
    assert!(
        said.status.success(),
        "{}",
        String::from_utf8_lossy(&said.stderr)
    );
    let folder = dir.path().join("receptors");
    let file = folder.join("slack.json");
    assert_eq!(
        String::from_utf8_lossy(&said.stdout).trim(),
        file.display().to_string(),
        "it says where"
    );
    let mode = |path: &Path| fs::metadata(path).expect("there").permissions().mode() & 0o777;
    assert_eq!((mode(&folder), mode(&file)), (0o700, 0o600));
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(&file).expect("written")).expect("JSON");
    let mut fields: Vec<&str> = record
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    // A process's start is known where /proc or libproc tells it, on Linux and
    // macOS, so only there is it kept; elsewhere the pid alone is judged.
    let mut expected = vec![
        "counts", "every", "pid", "read_at", "realm", "since", "source", "state",
    ];
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        expected.insert(7, "started");
    }
    assert_eq!(fields, expected, "counts and times alone");
    assert_eq!(
        (&record["source"], &record["realm"], &record["counts"]),
        (
            &serde_json::json!("slack"),
            &serde_json::json!("work"),
            &serde_json::json!({"hour": 1, "today": 4, "addressed": 2})
        )
    );
}

#[test]
fn what_a_receptor_read_never_reaches_its_file() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let pid = std::process::id().to_string();
    let standing = ["--every", "300", "--pid", pid.as_str()];
    for planted in [
        vec!["slack", "--state", "reading", "--channel", "general"],
        vec![
            "slack",
            "--state",
            "reading",
            "--subject",
            "Your sign-in code",
        ],
        vec!["slack", "--state", "reading", "--from", "someone"],
        vec!["#general", "--state", "reading"],
        vec!["slack", "--state", "replied"],
        vec!["slack", "--state", "reading", "--today", "many"],
    ] {
        let words: Vec<&str> = planted.iter().chain(&standing).copied().collect();
        let said = say(dir.path(), &words);
        assert!(
            !said.status.success(),
            "{planted:?} was taken: {}",
            String::from_utf8_lossy(&said.stdout)
        );
    }
    assert!(
        !dir.path().join("receptors").exists(),
        "nothing was written"
    );
}

#[test]
fn counts_that_cant_be_are_refused() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let pid = std::process::id().to_string();
    let said = say(
        dir.path(),
        &[
            "slack",
            "--state",
            "reading",
            "--today",
            "1",
            "--addressed",
            "5",
            "--every",
            "300",
            "--pid",
            &pid,
        ],
    );
    assert!(!said.status.success(), "more addressed than came in");
    assert!(!dir.path().join("receptors").join("slack.json").exists());
}

#[test]
fn a_process_that_isnt_running_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let mut ended = Command::new(env!("CARGO_BIN_EXE_mycelium"))
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("mycelium runs");
    ended.wait().expect("it ended");
    let pid = ended.id().to_string();
    let said = say(
        dir.path(),
        &["slack", "--state", "quiet", "--every", "300", "--pid", &pid],
    );
    assert!(!said.status.success(), "a process gone can't keep it");
    assert!(!dir.path().join("receptors").join("slack.json").exists());
}
