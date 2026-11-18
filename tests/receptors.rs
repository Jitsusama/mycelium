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

/// GitHub's receptor saying it reads, with `more` after its standing words.
fn github(dir: &Path, more: &[&str]) -> Output {
    let pid = std::process::id().to_string();
    let standing = [
        "github", "--state", "reading", "--every", "300", "--pid", &pid,
    ];
    let words: Vec<&str> = standing
        .iter()
        .copied()
        .chain(more.iter().copied())
        .collect();
    say(dir, &words)
}

/// The changes kept in `dir`'s GitHub items file, as `owner/name#N`.
fn kept(dir: &Path) -> Vec<String> {
    let file = dir.join("receptors").join("github.items.json");
    let written: serde_json::Value =
        serde_json::from_slice(&fs::read(file).expect("written")).expect("JSON");
    written["items"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|item| {
            format!(
                "{}#{}",
                item["repo"].as_str().expect("a repo"),
                item["number"]
            )
        })
        .collect()
}

#[test]
fn the_changes_asking_review_are_kept_whole_in_a_file_of_their_own() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let said = github(
        dir.path(),
        &[
            "--owner",
            "jsama",
            "--owner",
            "tern",
            "--item",
            "jsama/kestrel#12",
            "--item",
            "tern/skiff#3",
        ],
    );
    assert!(
        said.status.success(),
        "{}",
        String::from_utf8_lossy(&said.stderr)
    );
    let file = dir.path().join("receptors").join("github.items.json");
    assert!(
        String::from_utf8_lossy(&said.stdout).contains(&file.display().to_string()),
        "it says where"
    );
    let mode = fs::metadata(&file).expect("there").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "its owner's alone");
    assert_eq!(kept(dir.path()), ["jsama/kestrel#12", "tern/skiff#3"]);

    let said = github(dir.path(), &[]);
    assert!(
        said.status.success(),
        "{}",
        String::from_utf8_lossy(&said.stderr)
    );
    assert_eq!(
        kept(dir.path()),
        ["jsama/kestrel#12", "tern/skiff#3"],
        "no owners leave the items be"
    );

    let said = github(dir.path(), &["--owner", "jsama"]);
    assert!(
        said.status.success(),
        "{}",
        String::from_utf8_lossy(&said.stderr)
    );
    assert!(
        kept(dir.path()).is_empty(),
        "owners named with no item clear them"
    );
}

#[test]
fn a_change_that_isnt_an_owned_one_by_number_is_refused_and_nothing_written() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    for planted in [
        vec![
            "--owner",
            "jsama",
            "--item",
            "jsama/kestrel#12 Fix the sign-in",
        ],
        vec!["--owner", "jsama", "--item", "Fix the sign-in"],
        vec!["--owner", "jsama", "--item", "tern/skiff#3"],
        vec!["--owner", "jsama", "--item", "jsamax/kestrel#3"],
        vec!["--owner", "jsama", "--item", "jsama/kestrel#0"],
        vec!["--owner", "jsama", "--item", "jsama/kestrel#-4"],
        vec!["--owner", "jsama", "--item", "jsama/kestrel"],
        vec!["--item", "jsama/kestrel#12"],
    ] {
        let said = github(dir.path(), &planted);
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
    let pid = std::process::id().to_string();
    let said = say(
        dir.path(),
        &[
            "slack", "--state", "reading", "--every", "300", "--pid", &pid, "--owner", "jsama",
        ],
    );
    assert!(!said.status.success(), "only GitHub's receptor keeps items");
}

/// Calls' receptor saying it reads, with `more` after its standing words.
fn calls(dir: &Path, more: &[&str]) -> Output {
    let pid = std::process::id().to_string();
    let standing = [
        "calls", "--state", "reading", "--every", "60", "--pid", &pid,
    ];
    let words: Vec<&str> = standing
        .iter()
        .copied()
        .chain(more.iter().copied())
        .collect();
    say(dir, &words)
}

/// The events kept in `dir`'s calls items file, by id.
fn events(dir: &Path) -> Vec<String> {
    let file = dir.join("receptors").join("calls.items.json");
    let written: serde_json::Value =
        serde_json::from_slice(&fs::read(file).expect("written")).expect("JSON");
    written["items"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|item| item["event"].as_str().expect("an event").to_owned())
        .collect()
}

/// An event's id as Google gives a recurring one's instance.
const EVENT: &str = "6kq3ehb1c4p0bt2oq1i5u4tq4c_20261118T150000Z";

#[test]
fn the_calls_summarized_are_kept_whole_by_their_events_alone() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let said = calls(dir.path(), &["--event", EVENT, "--event", "abc123"]);
    assert!(
        said.status.success(),
        "{}",
        String::from_utf8_lossy(&said.stderr)
    );
    let file = dir.path().join("receptors").join("calls.items.json");
    let mode = fs::metadata(&file).expect("there").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "its owner's alone");
    assert_eq!(events(dir.path()), [EVENT, "abc123"]);
    let text = fs::read_to_string(&file).expect("written");
    assert!(
        text.contains("\"reason\":\"summary-written\"") && !text.contains("repo"),
        "{text}"
    );

    let said = calls(dir.path(), &["--event", "abc123"]);
    assert!(
        said.status.success(),
        "{}",
        String::from_utf8_lossy(&said.stderr)
    );
    assert_eq!(events(dir.path()), ["abc123"], "an event not named stayed");
    let pid = std::process::id().to_string();
    let failing = say(
        dir.path(),
        &[
            "calls", "--state", "failing", "--every", "60", "--pid", &pid,
        ],
    );
    assert!(
        failing.status.success(),
        "{}",
        String::from_utf8_lossy(&failing.stderr)
    );
    assert_eq!(
        events(dir.path()),
        ["abc123"],
        "a failing say, which read nothing, cleared the events"
    );
    let said = calls(dir.path(), &[]);
    assert!(
        said.status.success(),
        "{}",
        String::from_utf8_lossy(&said.stderr)
    );
    assert!(events(dir.path()).is_empty(), "no event named left one");
}

#[test]
fn an_event_that_isnt_an_id_or_said_by_another_source_is_refused_and_nothing_written() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    for planted in [
        vec!["--event", "Pay review with Tern"],
        vec!["--event", ""],
        vec!["--event", "abc-def"],
        vec!["--event", EVENT, "--event", "abc/def"],
        vec!["--owner", "jsama"],
        vec!["--owner", "jsama", "--item", "jsama/kestrel#12"],
    ] {
        let said = calls(dir.path(), &planted);
        assert!(
            !said.status.success(),
            "{planted:?} was taken: {}",
            String::from_utf8_lossy(&said.stdout)
        );
    }
    for planted in [
        vec!["--event", EVENT],
        vec!["--owner", "jsama", "--event", EVENT],
    ] {
        let said = github(dir.path(), &planted);
        assert!(
            !said.status.success(),
            "GitHub's receptor named an event: {planted:?}"
        );
    }
    assert!(
        !dir.path().join("receptors").exists(),
        "nothing was written"
    );
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

#[test]
fn a_record_refused_keeps_none_of_the_items_named_beside_it() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let mut ended = Command::new(env!("CARGO_BIN_EXE_mycelium"))
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("mycelium runs");
    ended.wait().expect("it ended");
    let gone = ended.id().to_string();
    let here = std::process::id().to_string();
    let items = ["--owner", "jsama", "--item", "jsama/kestrel#12"];
    let empty = tempfile::tempdir().expect("a temporary folder");
    let kept_before = github(
        dir.path(),
        &["--owner", "jsama", "--item", "jsama/kestrel#11"],
    );
    assert!(kept_before.status.success(), "a sound say was refused");
    for (why, standing) in [
        (
            "its process is gone",
            vec!["--every", "300", "--pid", &gone],
        ),
        ("it reads too often", vec!["--every", "0", "--pid", &here]),
        (
            "it counts more addressed than it has",
            vec![
                "--every",
                "300",
                "--pid",
                &here,
                "--today",
                "1",
                "--addressed",
                "2",
            ],
        ),
    ] {
        let words: Vec<&str> = ["github", "--state", "reading"]
            .into_iter()
            .chain(standing)
            .chain(items)
            .collect();
        for folder in [empty.path(), dir.path()] {
            let said = say(folder, &words);
            assert!(!said.status.success(), "a record was taken though {why}");
        }
        assert!(
            !empty
                .path()
                .join("receptors")
                .join("github.items.json")
                .exists(),
            "items were kept beside a record refused because {why}"
        );
        assert_eq!(
            kept(dir.path()),
            ["jsama/kestrel#11"],
            "the items kept before were changed by a record refused because {why}"
        );
    }
}
