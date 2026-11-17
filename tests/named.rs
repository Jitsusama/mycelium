//! A machine named by its node's `--machine`: the node records the name
//! as it starts, and a command run there with no `--machine`, as the
//! other end of a link starts `mycelium link`, goes by the same name
//! rather than the system's.

use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::process::{Child, Stdio};
use std::time::Duration;

use process::{made, mycelium};

mod process;
use serde_json::Value;

/// How long the node has to start.
const PATIENCE: Duration = Duration::from_secs(30);

const QUEST: &str = "QEST-20261115-17G3UY";

/// A node that is stopped once the test is done with it, however it ends.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_command_goes_by_the_name_its_node_was_given() {
    let home = tempfile::tempdir().expect("a home");
    let home = home.path();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home.join("r"))
        .expect("its runtime folder is made");
    let record = home
        .join("d/pi/agentic-harness.pi/quest-workflow/quests")
        .join(QUEST);
    fs::create_dir_all(&record).expect("its record made");
    fs::write(record.join("README.md"), "# Named\n").expect("its README written");

    let node = Running(
        mycelium(home, &["node", "--machine", "kestrel"])
            .stderr(Stdio::null())
            .spawn()
            .expect("the node starts"),
    );
    let socket = home.join("r").join("mycelium").join("work.sock");
    made(&home.join("r"), &socket, PATIENCE);
    let named = home.join("s").join("mycelium").join("machine");
    assert_eq!(
        fs::read_to_string(&named).ok().as_deref(),
        Some("kestrel\n"),
        "the node records the name it was given"
    );

    let shared = mycelium(home, &["share", QUEST, "--to", "kestrel", "--wait"])
        .output()
        .expect("mycelium runs");
    drop(node);
    let line: Value = serde_json::from_slice(&shared.stdout).unwrap_or_else(|error| {
        panic!(
            "no JSON line ({error}): {}",
            String::from_utf8_lossy(&shared.stdout)
        )
    });
    assert_eq!(line["reason"], "refused", "{line}");
    assert!(
        line["why"]
            .as_str()
            .is_some_and(|why| why.contains("kestrel is this machine")),
        "a share with no --machine goes by the node's name: {line}"
    );
}
