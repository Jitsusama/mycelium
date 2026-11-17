//! `mycelium sprout` as its own process: asked from a home folder, or
//! told to begin in one, it refuses before asking the node anything, so
//! no session is started where it would have none of its repository's
//! skills. Its home is a checkout here, as a home kept in git is, since
//! that is when a home could pass for a repository.

use std::path::Path;
use std::process::Command;

/// A home kept in git, with a folder of notes in it that isn't a
/// repository of its own.
fn home_in_git() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("a home");
    std::fs::create_dir(home.path().join(".git")).expect("home in git");
    std::fs::create_dir(home.path().join("notes")).expect("a folder in home");
    home
}

/// `mycelium sprout` run from `cwd` with `args`, its home `home` and its
/// node's socket where no node listens.
fn sprout(home: &Path, cwd: &Path, args: &[&str]) -> (bool, String) {
    let ran = Command::new(env!("CARGO_BIN_EXE_mycelium"))
        .args(["--realm", "work", "--socket"])
        .arg(home.join("no-node.sock"))
        .arg("sprout")
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", home.join("r"))
        .env("XDG_STATE_HOME", home.join("s"))
        .output()
        .expect("mycelium runs");
    (
        ran.status.success(),
        String::from_utf8_lossy(&ran.stderr).into_owned(),
    )
}

#[test]
fn a_sprout_from_home_or_a_folder_in_it_asks_which_repository() {
    let home = home_in_git();
    for cwd in [home.path().to_owned(), home.path().join("notes")] {
        let (ran, said) = sprout(home.path(), &cwd, &["make", "tea"]);
        assert!(!ran, "it refused from {}: {said}", cwd.display());
        assert!(
            said.contains("--in <folder>"),
            "it says what to name, not that the node is away: {said}"
        );
    }
}

#[test]
fn a_home_folder_named_is_refused_however_it_is_spelled() {
    let home = home_in_git();
    let notes = home.path().join("notes");
    let whole = home.path().to_str().expect("a home said in words");
    for named in [whole, "..", "../notes/.."] {
        let (ran, said) = sprout(home.path(), &notes, &["--in", named, "make", "tea"]);
        assert!(!ran, "it refused {named}: {said}");
        assert!(
            said.contains("home folder"),
            "it says why for {named}: {said}"
        );
    }
}

/// A sprout `--on` another machine is that machine's node's to judge, so
/// this machine's name, which only a sprout here needs, can't stop it:
/// its node's record of the name unreadable, it goes on to ask the node.
#[test]
fn a_sprout_on_another_machine_needs_no_name_for_this_one() {
    let home = home_in_git();
    let rows = home.path().join("src/rows");
    std::fs::create_dir_all(rows.join(".git")).expect("a checkout");
    let record = home.path().join("s/mycelium/machine");
    std::fs::create_dir_all(&record).expect("a record that can't be read");
    let folder = rows.to_str().expect("a folder said in words");

    let (ran, said) = sprout(
        home.path(),
        &rows,
        &["--on", "tern", "--in", folder, "make", "tea"],
    );

    assert!(!ran, "no node listens: {said}");
    assert!(
        !said.contains("Is a directory"),
        "it asked the node rather than reading this machine's name: {said}"
    );
}

#[test]
fn a_cell_it_came_from_that_isnt_one_is_refused() {
    let home = home_in_git();
    let (ran, said) = sprout(
        home.path(),
        home.path(),
        &["--from", "X; rm -rf ~", "make", "tea"],
    );
    assert!(!ran, "it refused: {said}");
    assert!(said.contains("isn't a cell"), "it says why: {said}");
}
