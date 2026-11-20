//! What `mycelium unanswered` prints, as its own process against a node
//! holding a hold: into a pipe, text for a person, nothing cut; with
//! `-o json`, or `--json` as every command's output takes it, the node's
//! records as JSON lines, as the command printed them before it drew for
//! a person.

use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::process::{Child, Stdio};
use std::time::Duration;

use process::{made, mycelium};
use serde_json::{Value, json};

mod process;

/// A hold's first line, wider than any terminal a test might run in, so a
/// pipe drawn as one would cut or wrap it.
const OPENING: &str = "merge nothing until I end this hold, which stands while the \
    builders catch up with every change sent to them today, and a little after";

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

#[test]
fn a_pipe_reads_text_and_json_is_asked_for() {
    let home = tempfile::tempdir().expect("a home");
    let home = home.path();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home.join("r"))
        .expect("its runtime folder is made");
    let messages = home.join("s/pi/agentic-harness.pi/mesh-workflow/messages");
    fs::create_dir_all(&messages).expect("the mesh's folder is made");
    let now = jiff::Timestamp::now();
    let id = format!("{}-KESTRL-1-647648e1", now.as_millisecond());
    let hold = json!({
        "v": 1,
        "id": id,
        "at": format!("{now:.3}"),
        "from": "KESTRL",
        "to": ["#all"],
        "text": format!("{OPENING}\nthe second line"),
        "intent": "hold",
        "pid": 1,
    });
    fs::write(messages.join(format!("{id}.json")), hold.to_string()).expect("the hold is put");

    let node = Running(
        mycelium(home, &["node", "--machine", "kestrel"])
            .stderr(Stdio::null())
            .spawn()
            .expect("the node starts"),
    );
    made(
        &home.join("r"),
        &home.join("r").join("mycelium").join("work.sock"),
        PATIENCE,
    );
    let printed = |args: &[&str]| {
        let ran = mycelium(home, args).output().expect("mycelium runs");
        assert!(ran.status.success(), "{args:?} failed: {ran:?}");
        String::from_utf8(ran.stdout).expect("UTF-8")
    };
    let text = printed(&["unanswered"]);
    let json = printed(&["unanswered", "-o", "json"]);
    let older = printed(&["unanswered", "--json"]);
    drop(node);

    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.first(), Some(&"1 hold stands"), "{text}");
    assert!(
        lines.get(1).is_some_and(|line| {
            line.contains("KESTRL holds #all, until ") && line.ends_with("  [647648e1]")
        }),
        "no line for the hold:\n{text}"
    );
    assert_eq!(
        lines.get(2),
        Some(&format!("    {OPENING} …").as_str()),
        "a pipe cut the opening, or drew it as a terminal would:\n{text}"
    );
    let records: Vec<Value> = json
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect();
    assert_eq!(records.len(), 1, "{json}");
    assert_eq!(records[0]["kind"], "unanswered", "{json}");
    assert_eq!(records[0]["key"], format!("unanswered:{id}"), "{json}");
    assert!(records[0]["until"].is_string(), "{json}");
    assert_eq!(older, json, "--json prints as -o json does");
}
