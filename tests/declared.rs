//! A node given `--declared` reads what its machine declares from that
//! file alone, never from the account's own configuration, even where the
//! account's says more: its agents' commands could otherwise be run as a
//! file the account writes says. It says as it starts which file it reads,
//! so its journal shows a changed one.

use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::process::Child;
use std::time::Duration;

use process::{made, mycelium};

mod process;

/// How long the node has to start.
const PATIENCE: Duration = Duration::from_secs(30);

/// A node that is stopped once the test is done with it, however it ends.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A declaration tainting its machine with `key`, whose strict fence runs
/// a command through `runs`.
fn declaring(key: &str, runs: &[&str]) -> String {
    serde_json::json!({
        "reach": [], "outlives": [], "costs_money": false,
        "taints": [{"key": key, "effect": "prefer_not", "why": "it says so"}],
        "fence": {"strict": {"builders": [], "forges": [], "secrets": [], "runs": runs}}
    })
    .to_string()
}

#[test]
fn a_node_given_its_declaration_never_reads_the_accounts() {
    let home = tempfile::tempdir().expect("a home");
    let home = home.path();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home.join("r"))
        .expect("its runtime folder is made");
    let config = home.join("c").join("mycelium");
    fs::create_dir_all(&config).expect("its configuration's folder is made");
    fs::write(
        config.join("declared.json"),
        declaring("the-accounts-own", &["/bin/sh", "-c"]),
    )
    .expect("the account's declaration is written");
    let given = home.join("given.json");
    fs::write(&given, declaring("the-nodes-given", &[])).expect("the given one is written");
    let journal = home.join("journal");

    let node = Running(
        mycelium(
            home,
            &[
                "node",
                "--machine",
                "kestrel",
                "--declared",
                given.to_str().expect("a path"),
            ],
        )
        .stderr(fs::File::create(&journal).expect("its journal is made"))
        .spawn()
        .expect("the node starts"),
    );
    let socket = home.join("r").join("mycelium").join("work.sock");
    made(&home.join("r"), &socket, PATIENCE);
    // It says so before it listens, so the line is there once it does.
    let journal = fs::read_to_string(&journal).expect("its journal is read");
    assert!(
        journal.contains(&format!(
            "mycelium: reads what this machine declares from {}; ",
            given.display()
        )),
        "the node says which file it reads: {journal}"
    );
    let said = mycelium(home, &["machine"])
        .output()
        .expect("mycelium runs");
    drop(node);
    let said = String::from_utf8_lossy(&said.stdout);
    assert!(
        said.contains("the-nodes-given"),
        "the machine is as the given file declares: {said}"
    );
    assert!(
        !said.contains("the-accounts-own"),
        "nothing the account's own declares is read: {said}"
    );
}
