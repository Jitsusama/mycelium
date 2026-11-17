//! What a command says where nobody reads it: a failure whose words go
//! to a pipe nobody reads any more, as a caller's `2>&1 | head` leaves
//! behind, still ends as a failure saying nothing, never a panic.

use std::process::{Command, Stdio};

#[test]
fn a_failure_said_to_a_reader_that_left_still_ends_as_a_failure() {
    let home = tempfile::tempdir().expect("a home");
    let (reader, gone) = std::io::pipe().expect("a pipe");
    drop(reader);
    let ran = Command::new(env!("CARGO_BIN_EXE_mycelium"))
        .args(["--realm", "work", "--socket"])
        .arg(home.path().join("no-node.sock"))
        .arg("sessions")
        .env("HOME", home.path())
        .stdout(Stdio::null())
        .stderr(gone)
        .status()
        .expect("mycelium runs");
    assert_eq!(
        ran.code(),
        Some(1),
        "no node there is a failure, and saying so to nobody isn't a panic's 101"
    );
}
