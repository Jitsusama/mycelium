//! `mycelium sprout` as its own process: asked from a home folder with no
//! repository named, it refuses before asking the node anything, so no
//! session is started where it would have none of its repository's skills.

use std::path::Path;
use std::process::Command;

/// `mycelium sprout` run from `cwd` with `args`, its home `home` and its
/// node's socket where no node listens.
fn sprout(home: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mycelium"))
        .args(["--realm", "work", "--socket"])
        .arg(home.join("no-node.sock"))
        .arg("sprout")
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", home.join("r"))
        .env("XDG_STATE_HOME", home.join("s"))
        .output()
        .expect("mycelium runs")
}

#[test]
fn a_sprout_from_home_is_refused_before_the_node_is_asked() {
    let home = tempfile::tempdir().expect("a home");
    let ran = sprout(home.path(), home.path(), &["make", "tea"]);
    let said = String::from_utf8_lossy(&ran.stderr);
    assert!(!ran.status.success(), "it refused: {said}");
    assert!(
        said.contains("--in <folder>"),
        "it says what to name: {said}"
    );
}

#[test]
fn a_home_folder_named_is_refused_too() {
    let home = tempfile::tempdir().expect("a home");
    let home_named = home.path().to_str().expect("a home said in words");
    let ran = sprout(
        home.path(),
        home.path(),
        &["--in", home_named, "make", "tea"],
    );
    let said = String::from_utf8_lossy(&ran.stderr);
    assert!(!ran.status.success(), "it refused: {said}");
    assert!(said.contains("home folder"), "it says why: {said}");
}

#[test]
fn a_sprout_with_no_words_is_refused() {
    let home = tempfile::tempdir().expect("a home");
    let ran = sprout(home.path(), home.path(), &[]);
    assert!(!ran.status.success(), "clap asks for the work");
}
