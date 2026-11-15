//! Asking the node for what the swarm, the networks and the steers read:
//! every ask in one write after the node's hello, and its answers read
//! back by their place among the asks.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::Swarm;

/// Reads the swarm from the node at `socket` in the daemon interface,
/// giving up on any read or write taking longer than `within`.
///
/// # Errors
///
/// When no node answers, or it answers in a way it shouldn't, or holds no
/// teams, as a node too old for them doesn't.
pub fn read(socket: &Path, realm: &str, within: Duration) -> io::Result<Swarm> {
    let mut answers = asked(
        socket,
        within,
        &[
            json!({"ask": "list", "kind": "team"}),
            json!({"ask": "list", "kind": "session"}),
        ],
    )?;
    let mut records = |id: u64| -> io::Result<Vec<Value>> {
        let answer = answers.remove(&id).unwrap_or_default();
        if let Some(why) = answer["error"]["message"].as_str() {
            return Err(io::Error::other(format!(
                "the node can't say its teams ({why}); it may need updating"
            )));
        }
        Ok(answer["records"].as_array().cloned().unwrap_or_default())
    };
    Ok(Swarm {
        realm: realm.to_owned(),
        teams: records(1)?,
        sessions: records(2)?,
    })
}

/// Asks the node at `socket` each of `asks` in the daemon interface,
/// after its hello, and gives its answers by their place in `asks`,
/// counting from one.
pub(crate) fn asked(
    socket: &Path,
    within: Duration,
    asks: &[Value],
) -> io::Result<BTreeMap<u64, Value>> {
    let mut reading = asking(socket, within, asks)?;
    let wanted = (1_u64..)
        .zip(asks)
        .map(|(id, _)| id)
        .collect::<BTreeSet<u64>>();
    let mut answers: BTreeMap<u64, Value> = BTreeMap::new();
    let mut line = String::new();
    while !wanted.iter().all(|id| answers.contains_key(id)) {
        line.clear();
        if reading.read_line(&mut line)? == 0 {
            return Err(io::Error::other("the node hung up before it answered"));
        }
        let answer: Value = serde_json::from_str(&line).map_err(io::Error::other)?;
        if let Some(id) = answer["id"].as_u64().filter(|id| wanted.contains(id)) {
            answers.insert(id, answer);
        }
    }
    Ok(answers)
}

/// Asks the node at `socket` each of `asks` in the daemon interface,
/// after its hello, each by its place in `asks` counting from one, and
/// gives the connection its answers come back on.
pub(crate) fn asking(
    socket: &Path,
    within: Duration,
    asks: &[Value],
) -> io::Result<BufReader<UnixStream>> {
    let mut writing = UnixStream::connect(socket).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("no node answers at {}: {error}", socket.display()),
        )
    })?;
    writing.set_read_timeout(Some(within))?;
    writing.set_write_timeout(Some(within))?;
    let reading = BufReader::new(writing.try_clone()?);
    // Every ask goes in one write. Written a piece at a time, the later
    // asks wait on a node still writing its first answer, which waits on
    // this reading it: neither moves until the timeout. One write is
    // taken whole only while it fits what the socket buffers, so asks
    // together are held under `ASKS_FIT`.
    let asked = together(asks);
    if asked.len() > ASKS_FIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} bytes of asks are past the {ASKS_FIT} a node takes in one write",
                asked.len()
            ),
        ));
    }
    writing.write_all(asked.as_bytes())?;
    Ok(reading)
}

/// The most bytes a connection's asks may hold together, so a node busy
/// writing its first answer can't leave the rest unread: what a Unix
/// stream buffers on macOS, the least of the systems a node runs on.
pub(crate) const ASKS_FIT: usize = 8 * 1024;

/// A hello and each of `asks`, a line each, as `asking` sends them: each
/// by its place in `asks` counting from one.
pub(crate) fn together(asks: &[Value]) -> String {
    let mut asked = format!("{}\n", json!({"id": 0, "ask": "hello"}));
    for (id, ask) in (1_u64..).zip(asks) {
        let mut ask = ask.clone();
        ask["id"] = id.into();
        asked.push_str(&ask.to_string());
        asked.push('\n');
    }
    asked
}
