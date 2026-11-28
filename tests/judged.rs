//! A plain run's path judged by the `mycelium` command itself, as the
//! run's shell asks it, with no realm named: the unit tests' stand-in
//! judges without reading the words as the command does, so only this
//! shows that the command takes them.

use std::process::Command;

// Followup 566: the run's shell gives the judge an empty `HOME` where it
// has none, and a home may start with a hyphen; the command takes both
// as words, and says the folders kept and dropped as two lines.
#[test]
fn the_command_judges_a_path_as_a_plain_run_asks() {
    let made = tempfile::tempdir().expect("a temporary folder");
    let owned = std::fs::canonicalize(made.path()).expect("the folder's place");
    let path = format!("relative::{}", owned.display());
    let said = Command::new(env!("CARGO_BIN_EXE_mycelium"))
        .args(["kept-path", "--", "-a-home", ""])
        .arg(&path)
        .env("XDG_RUNTIME_DIR", &owned)
        .env("XDG_STATE_HOME", &owned)
        .output()
        .expect("mycelium runs");
    assert!(
        said.status.success(),
        "{}",
        String::from_utf8_lossy(&said.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&said.stdout),
        format!("\nrelative:{}\n", owned.display()),
        "a relative folder and one its user owns go, the empty one named nowhere"
    );
}
