//! A quest shared and waited for, as the harness's spawn runs it: the
//! command prints one JSON line however the wait ends, and exits non-zero
//! unless the machine holds the quest.

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

const QUEST: &str = "QEST-20261115-5XF89V";

/// `mycelium share` on kestrel, with no node, its folders under `home`.
fn share(home: &Path, args: &[&str]) -> (Output, Value) {
    let ran = Command::new(env!("CARGO_BIN_EXE_mycelium"))
        .args(["--realm", "work", "share", QUEST, "--machine", "kestrel"])
        .args(args)
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", home.join("r"))
        .env("XDG_STATE_HOME", home.join("s"))
        .env("XDG_DATA_HOME", home.join("d"))
        .env("XDG_CACHE_HOME", home.join("k"))
        .stdin(Stdio::null())
        .output()
        .expect("mycelium runs");
    let line: Value = serde_json::from_slice(&ran.stdout).unwrap_or_else(|error| {
        panic!(
            "no JSON line ({error}): {}",
            String::from_utf8_lossy(&ran.stdout)
        )
    });
    (ran, line)
}

#[test]
fn a_share_to_a_machine_gives_it_the_quest_and_to_no_name_is_refused() {
    let home = tempfile::tempdir().expect("a home");
    let home = home.path();
    let record = home
        .join("d/pi/agentic-harness.pi/quest-workflow/quests")
        .join(QUEST);
    fs::create_dir_all(&record).expect("its record made");
    fs::write(record.join("README.md"), "# Shared\n").expect("its README written");
    let given = home.join("s/mycelium/given");

    let (ran, line) = share(home, &["--to", "skiff", "--wait", "--within", "1"]);
    assert_eq!(line["reason"], "no-link", "with no node: {line}");
    assert!(!ran.status.success(), "and so not held");
    assert!(
        given.join("skiff").join(QUEST).exists(),
        "a share to skiff gives it the quest, so a worker there is served it"
    );

    let (ran, line) = share(home, &["--to", "../tern", "--wait", "--within", "1"]);
    assert!(!ran.status.success(), "a share to no machine's name fails");
    assert_eq!(line["reason"], "refused", "{line}");
    assert!(
        !home.join("s/mycelium/tern").exists(),
        "and writes nothing outside the gifts' folder"
    );
}

#[test]
fn a_wait_says_why_in_a_json_line_and_fails_unless_held() {
    let home = tempfile::tempdir().expect("a home");
    let home = home.path();
    let record = home
        .join("d/pi/agentic-harness.pi/quest-workflow/quests")
        .join(QUEST);
    fs::create_dir_all(&record).expect("its record made");
    fs::write(record.join("README.md"), "# Shared\n").expect("its README written");

    let (ran, line) = share(home, &["--to", "kestrel", "--wait"]);
    assert!(!ran.status.success(), "this machine is refused");
    assert_eq!(line["held"], false);
    assert_eq!(line["reason"], "refused", "{line}");

    let (ran, line) = share(home, &["--to", "skiff", "--wait", "--within", "1"]);
    assert!(!ran.status.success(), "with no node, no link reaches skiff");
    assert_eq!(line["reason"], "no-link", "{line}");
    assert_eq!(line["files"], 1, "{line}");
    assert_eq!(line["holding"], 0, "{line}");
    assert_eq!(line["lacking"][0], "record/README.md", "{line}");
    assert!(
        String::from_utf8_lossy(&ran.stderr).contains("no link reaches skiff from kestrel"),
        "why is said on stderr too"
    );
}
