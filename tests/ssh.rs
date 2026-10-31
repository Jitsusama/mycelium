//! `mycelium ssh --from NAME`, run as a machine's pinned link key runs it:
//! what SSH says the key's client asked comes in `SSH_ORIGINAL_COMMAND`.

use std::process::{Command, Output};

/// Runs `mycelium --realm work ssh --from equilibrius` with mycelium's
/// folder and runtime folder in `dir`, as SSH runs it for a client asking
/// `asked`.
fn asking(dir: &std::path::Path, asked: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mycelium"))
        .args(["--realm", "work", "ssh", "--from", "equilibrius", "--state"])
        .arg(dir)
        .env("SSH_ORIGINAL_COMMAND", asked)
        .env("XDG_RUNTIME_DIR", dir)
        .output()
        .expect("mycelium runs")
}

#[test]
fn a_key_asking_anything_but_a_link_or_an_attach_is_refused_and_kept() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let refused = asking(dir.path(), "sh -c id");
    assert!(!refused.status.success(), "it is refused");
    assert_eq!(
        String::from_utf8_lossy(&refused.stderr),
        "mycelium: \"sh -c id\" isn't a link or an attach, all a link key may ask\n",
        "saying why, to the one who asked"
    );
    let kept = std::fs::read_to_string(dir.path().join("refused-keys.jsonl"))
        .expect("the refusal is kept");
    let line: serde_json::Value = serde_json::from_str(kept.trim()).expect("a JSON line");
    assert_eq!(
        (&line["from"], &line["asked"]),
        (
            &serde_json::json!("equilibrius"),
            &serde_json::json!("sh -c id")
        ),
        "whose key asked what: {kept}"
    );
}

#[test]
fn a_key_asking_to_attach_is_let_through_to_attach() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let attaching = asking(dir.path(), "mycelium --realm work attach 7bf12940");
    assert_eq!(
        String::from_utf8_lossy(&attaching.stderr),
        "mycelium: nothing is held as 7bf12940; `mycelium held` lists what is\n",
        "it attaches, and nothing is held by that id"
    );
    assert!(
        !dir.path().join("refused-keys.jsonl").exists(),
        "and nothing is kept as refused"
    );
}
