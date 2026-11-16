//! `mycelium attach --on MACHINE`, as a window on a session elsewhere runs
//! it: joined again each time SSH drops, ending as the join there ends or
//! once the node says the session has.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Puts `body` in place as the program `name` in `dir`, by a child
/// process, since another test's fork holding the file open for writing
/// would keep it from running.
fn program(dir: &Path, name: &str, body: &str) {
    let text = dir.join(format!("{name}.sh"));
    fs::write(&text, format!("#!/bin/sh\n{body}")).expect("the program is written");
    let put = Command::new("install")
        .args(["-m", "755"])
        .arg(&text)
        .arg(dir.join(name))
        .status()
        .expect("install runs");
    assert!(put.success(), "{name} is put in place");
}

/// An `ssh` in `dir` that keeps the words it was given and drops the
/// first `drops` times it runs, as SSH ends when its link does, then runs
/// its words past `-t -- MACHINE` as that machine would; and a `mycelium`
/// there that joins by saying so and ending 3.
fn stand_ins(dir: &Path, drops: u32) {
    program(
        dir,
        "ssh",
        &format!(
            "printf '%s\\n' \"$*\" >> '{asked}'\n\
             tries=$(($(wc -l < '{asked}')))\n\
             if [ \"$tries\" -le {drops} ]; then\n\
             echo 'Connection to skiff closed.' >&2\n\
             exit 255\n\
             fi\n\
             shift 3\n\
             exec \"$@\"\n",
            asked = dir.join("asked").display()
        ),
    );
    program(dir, "mycelium", "echo \"joined by $*\"\nexit 3\n");
}

/// What `mycelium --realm work attach ...` asked as `words`, with the
/// stand-ins in `dir` first on its `PATH` and its node at `node`.
fn attaching(dir: &Path, node: &Path, words: &[&str]) -> Command {
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_mycelium"));
    command
        .args(["--realm", "work", "--socket"])
        .arg(node)
        .arg("attach")
        .args(words)
        .env("PATH", path)
        .stdin(Stdio::null());
    command
}

/// The words SSH was given, a line each try.
fn asked(dir: &Path) -> String {
    fs::read_to_string(dir.join("asked")).expect("ssh was run")
}

#[test]
fn a_join_whose_link_drops_joins_again_and_ends_as_the_join_there_does() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    stand_ins(dir.path(), 2);
    let began = Instant::now();

    let joined: Output = attaching(
        dir.path(),
        &dir.path().join("no-node.sock"),
        &[
            "--on",
            "skiff",
            "--first-wait",
            "10",
            "5e55",
            "--",
            "ssh",
            "-t",
            "--",
            "skiff",
            "mycelium",
            "--realm",
            "work",
            "attach",
            "5e55",
        ],
    )
    .output()
    .expect("mycelium runs");

    assert_eq!(
        String::from_utf8_lossy(&joined.stderr),
        "Connection to skiff closed.\nReconnecting to skiff.\n\
         Connection to skiff closed.\nReconnecting to skiff.\n",
        "each drop says it reconnects, once"
    );
    assert_eq!(
        String::from_utf8_lossy(&joined.stdout),
        "joined by --realm work attach 5e55\n",
        "it joined on the third try"
    );
    assert_eq!(
        joined.status.code(),
        Some(3),
        "passing on how the join there ended"
    );
    assert_eq!(
        asked(dir.path()),
        "-t -- skiff mycelium --realm work attach 5e55\n".repeat(3),
        "each try runs the words it was given"
    );
    assert!(
        began.elapsed() < Duration::from_secs(3),
        "waiting as it was told to, not the second and two of a window's"
    );
}

/// Answers each ask at `socket` as a node listing `sessions` does.
fn node_listing(socket: &Path, sessions: &serde_json::Value) {
    let listener = UnixListener::bind(socket).expect("the node's socket");
    let reply = serde_json::json!({"Ok": {"Sessions": sessions}}).to_string();
    std::thread::spawn(move || {
        for client in listener.incoming() {
            let Ok(client) = client else { return };
            let mut asked = String::new();
            let _ = BufReader::new(&client).read_line(&mut asked);
            let _ = writeln!(&client, "{reply}");
        }
    });
}

#[test]
fn a_join_ends_well_once_the_node_says_the_session_has_ended() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    stand_ins(dir.path(), 1000);
    let node = dir.path().join("work.sock");
    node_listing(
        &node,
        &serde_json::json!([{
            "id": "0e50",
            "address": "HARBOR",
            "machine": "skiff",
            "title": "Harbour work",
            "groups": [],
            "state": "ended",
            "held": "5e55",
            "at": "2026-11-16T10:00:00Z",
        }]),
    );

    let joined = attaching(dir.path(), &node, &["--on", "skiff", "5e55"])
        .output()
        .expect("mycelium runs");

    assert_eq!(
        String::from_utf8_lossy(&joined.stderr),
        "Connection to skiff closed.\nHarbour work has ended.\n",
        "saying which session ended, and never reconnecting"
    );
    assert_eq!(
        joined.status.code(),
        Some(0),
        "an ended session ends it well"
    );
    assert_eq!(
        asked(dir.path()),
        "-t -- skiff mycelium --realm work attach 5e55\n",
        "with no words given, it joins over SSH to the machine by its name"
    );
}

#[test]
fn ctrl_c_while_waiting_to_join_again_ends_it() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    stand_ins(dir.path(), 1000);
    let mut joining = attaching(
        dir.path(),
        &dir.path().join("no-node.sock"),
        &["--on", "skiff", "--first-wait", "60000", "5e55"],
    )
    .stderr(Stdio::piped())
    .spawn()
    .expect("mycelium runs");
    let mut said = BufReader::new(joining.stderr.take().expect("its error output"));
    let mut line = String::new();
    while line != "Reconnecting to skiff.\n" {
        line.clear();
        assert_ne!(
            said.read_line(&mut line).expect("it says"),
            0,
            "it waits to join again"
        );
    }

    let pressed = Command::new("kill")
        .args(["-INT", &joining.id().to_string()])
        .status()
        .expect("kill runs");
    assert!(pressed.success(), "Ctrl+C is pressed");
    let ended = joining.wait().expect("it ends");

    assert_eq!(ended.code(), Some(130), "as a shell says Ctrl+C");
    assert_eq!(asked(dir.path()).lines().count(), 1, "never trying again");
}
