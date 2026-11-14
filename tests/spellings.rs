//! The `mycelium` command's old spellings, each run beside the tree that
//! took it in: what one prints, to either output, and how it exits, the
//! other does byte for byte.

use std::process::{Command, Output};

/// Runs `mycelium --realm work` and `words` as a machine whose runtime
/// and state folders are in `dir`, with no node running.
fn run(dir: &std::path::Path, words: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mycelium"))
        .args(["--realm", "work"])
        .args(words.split(' '))
        .env("HOME", dir)
        .env("XDG_RUNTIME_DIR", dir)
        .env("XDG_STATE_HOME", dir)
        .output()
        .expect("mycelium runs")
}

#[test]
fn an_old_spelling_says_what_its_tree_says() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    for (old, new) in [
        ("held", "holds"),
        ("attach h0", "holds h0 attach"),
        ("attach h0", "hold h0 attach"),
        ("again h0 -- pi", "holds h0 again -- pi"),
        ("machine penelope", "machines penelope"),
        ("network grow", "networks grow"),
        ("network --json", "networks -o json"),
        ("cell QQ1UAK", "cells QQ1UAK"),
        ("swarm --json", "swarm -o json"),
    ] {
        let (old_said, new_said) = (run(dir.path(), old), run(dir.path(), new));
        assert_eq!(
            (&old_said.status, &old_said.stdout, &old_said.stderr),
            (&new_said.status, &new_said.stdout, &new_said.stderr),
            "{old} and {new}: {:?}",
            String::from_utf8_lossy(&old_said.stderr)
        );
    }
    assert_eq!(
        String::from_utf8_lossy(&run(dir.path(), "holds h0").stderr),
        "mycelium: nothing is held as h0; `mycelium holds` lists what is\n",
        "one held session, asked by an id nothing holds"
    );
}
