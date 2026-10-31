//! A machine updating itself to its configuration's main, when asked.
//!
//! The updater is the configuration's own program, which the system runs
//! on a socket only the node's account may write. Mycelium knows it only
//! by that socket's contract: one line, `update`, answered by one JSON
//! line per step, each naming its `step`, until one that ends the update,
//! which is any step but `fetching`, `building` and `switching`. An ask
//! while an update runs joins it, so a flood of asks is one update.
//!
//! The updater also keeps its last ending in a file the account may read,
//! which the machine says as its `updated` fact. That, not what an asker
//! heard, is what is kept, since a switch restarts the node that asked.

use std::io;
use std::path::Path;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// The one ask the updater takes.
const ASK: &str = "update";

/// The steps an update passes through on its way to its ending.
const UNDER_WAY: [&str; 3] = ["fetching", "building", "switching"];

/// Whether `line` ends an update: a step, but none it passes through.
#[must_use]
pub fn ends(line: &Value) -> bool {
    line.get("step")
        .and_then(Value::as_str)
        .is_some_and(|step| !UNDER_WAY.contains(&step))
}

/// Asks the updater at `socket` to update the machine, telling `hear` each
/// step on the way, and gives the ending as the updater said it.
///
/// # Errors
///
/// When the updater can't be reached, says anything but a step, or stops
/// before its ending.
pub async fn ask(socket: &Path, mut hear: impl FnMut(&Value)) -> Result<Value, String> {
    let named = |error: io::Error| format!("{}: {error}", socket.display());
    let mut stream = UnixStream::connect(socket)
        .await
        .map_err(|error| format!("no updater answers at {}: {error}", socket.display()))?;
    stream
        .write_all(format!("{ASK}\n").as_bytes())
        .await
        .map_err(named)?;
    let mut lines = BufReader::new(stream).lines();
    while let Some(line) = lines.next_line().await.map_err(named)? {
        let said: Value = serde_json::from_str(&line)
            .map_err(|error| format!("the updater said {line:?}, not a step: {error}"))?;
        if ends(&said) {
            return Ok(said);
        }
        if said.get("step").and_then(Value::as_str).is_none() {
            return Err(format!("the updater said {line:?}, not a step"));
        }
        hear(&said);
    }
    Err("the updater stopped before saying how the update ended".to_owned())
}

/// The last update's ending, as the updater keeps it in `file`, or none
/// when it keeps none there or what is there is no ending.
#[must_use]
pub fn last(file: &Path) -> Option<Value> {
    let said: Value = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
    ends(&said).then_some(said)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::net::UnixListener;

    use super::*;

    /// An updater on `socket` that takes one ask and answers it with
    /// `lines`, then hangs up.
    fn updater(socket: &Path, lines: &[Value]) -> tokio::task::JoinHandle<String> {
        let listener = UnixListener::bind(socket).expect("listening");
        let said: Vec<String> = lines.iter().map(Value::to_string).collect();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("asked");
            let (reading, mut writing) = stream.into_split();
            let asked = BufReader::new(reading)
                .lines()
                .next_line()
                .await
                .expect("read")
                .unwrap_or_default();
            for line in said {
                writing
                    .write_all(format!("{line}\n").as_bytes())
                    .await
                    .expect("answered");
            }
            asked
        })
    }

    #[tokio::test]
    async fn an_update_is_heard_step_by_step_to_its_ending() {
        let dir = tempfile::tempdir().expect("a folder");
        let socket = dir.path().join("ask.sock");
        let done = json!({"step": "done", "commit": "c0ffee", "applied": "now",
            "restart": null, "restarted": []});
        let asked = updater(
            &socket,
            &[
                json!({"step": "fetching"}),
                json!({"step": "building", "commit": "c0ffee"}),
                done.clone(),
            ],
        );
        let mut heard = Vec::new();
        let ending = ask(&socket, |step| heard.push(step["step"].clone())).await;
        assert_eq!(ending, Ok(done), "the ending, as the updater said it");
        assert_eq!(
            heard,
            [json!("fetching"), json!("building")],
            "each step on the way is heard"
        );
        assert_eq!(asked.await.expect("ran"), "update", "the one ask is said");
    }

    #[tokio::test]
    async fn an_updater_that_stops_before_its_ending_is_said_to_have() {
        let dir = tempfile::tempdir().expect("a folder");
        let socket = dir.path().join("ask.sock");
        let _asked = updater(&socket, &[json!({"step": "switching", "commit": "c0ffee"})]);
        let ending = ask(&socket, |_| {}).await;
        assert_eq!(
            ending,
            Err("the updater stopped before saying how the update ended".to_owned()),
            "stopping is no ending"
        );
        let missing = ask(&dir.path().join("none.sock"), |_| {}).await;
        assert!(
            missing.is_err_and(|why| why.contains("none.sock")),
            "a machine with no updater says where it looked"
        );
    }

    #[test]
    fn the_last_ending_is_kept_and_nothing_else() {
        let dir = tempfile::tempdir().expect("a folder");
        let file = dir.path().join("last.json");
        assert_eq!(last(&file), None, "none before any update");
        let unchanged = json!({"step": "unchanged", "commit": "c0ffee"});
        std::fs::write(&file, unchanged.to_string()).expect("written");
        assert_eq!(last(&file), Some(unchanged), "the ending kept");
        std::fs::write(&file, json!({"step": "building"}).to_string()).expect("written");
        assert_eq!(last(&file), None, "a step on the way is no ending");
        std::fs::write(&file, "not json").expect("written");
        assert_eq!(last(&file), None, "nor is anything else");
    }
}
