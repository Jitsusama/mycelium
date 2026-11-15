//! A holder's wait for its turn: a held program that resumes a session,
//! such as `pi --session <id>`, starts only once its node lets it, so a
//! restart's sessions come back a few at a time rather than all at once
//! (the node's `turns` says how they are let through).
//!
//! Whatever goes wrong goes the old way: a node that isn't there, can't
//! be reached, is too old to know turns, or restarts while the holder
//! waits lets the program start at once, so no session is ever left
//! waiting on a node that won't answer.

use std::path::Path;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::wire::Request;

/// The words before the session a command resumes.
const RESUMING: [&str; 2] = ["--session", "--session-id"];

/// The session `command` resumes, by id, wherever its words name one
/// after `--session` or `--session-id`, whole or within a shell's words,
/// as `sh -c "... exec pi --session <id>"` does. A session named by its
/// log's path, as `.../2026-11-14T22-23-37-209Z_<id>.jsonl`, is the id
/// that path ends in.
#[must_use]
pub fn resumed(command: &[String]) -> Option<String> {
    let words: Vec<&str> = command
        .iter()
        .flat_map(|word| word.split_whitespace())
        .map(|word| word.trim_matches(['"', '\'']))
        .collect();
    let named = words.iter().enumerate().find_map(|(at, word)| {
        if let Some((flag, id)) = word.split_once('=')
            && RESUMING.contains(&flag)
        {
            return Some(id);
        }
        RESUMING
            .contains(word)
            .then(|| words.get(at + 1).copied())
            .flatten()
    })?;
    let id = Path::new(named).file_stem()?.to_str()?.rsplit('_').next()?;
    (!id.is_empty() && !id.starts_with('-')).then(|| id.to_owned())
}

/// Waits until the node serving `socket` gives `session` its turn, or
/// says it can't, or can't be asked: in every case but the first, the
/// program starts at once, as it did before nodes gave turns.
pub async fn wait(socket: &Path, session: &str) {
    let Ok(stream) = UnixStream::connect(socket).await else {
        return;
    };
    let (reading, mut writing) = stream.into_split();
    let asked = Request::Turn {
        session: session.to_owned(),
    };
    let Ok(mut line) = serde_json::to_vec(&asked) else {
        return;
    };
    line.push(b'\n');
    if writing.write_all(&line).await.is_err() {
        return;
    }
    // Any line back is the turn: an older node's refusal too, since it
    // gives no turns. Its going away, as a node restarting does, ends the
    // wait the same way.
    let mut answer = String::new();
    let _ = BufReader::new(reading).read_line(&mut answer).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(all: &[&str]) -> Vec<String> {
        all.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn a_command_resumes_the_session_it_names() {
        let cases = [
            (words(&["pi", "--session", "01a1-ab"]), Some("01a1-ab")),
            (
                words(&["pi", "--session-id", "01a1-cd", "--model", "m"]),
                Some("01a1-cd"),
            ),
            (words(&["pi", "--session=01a1-ef"]), Some("01a1-ef")),
            (
                words(&[
                    "pi",
                    "--session",
                    "/h/.pi/agent/sessions/--h-src--/2026-11-14T22-23-37-209Z_01a1-gh.jsonl",
                ]),
                Some("01a1-gh"),
            ),
            (
                words(&[
                    "sh",
                    "-c",
                    "cd /h/src/harbour && QUEST=REEF01 exec pi --session \"01a1-ij\"",
                ]),
                Some("01a1-ij"),
            ),
            (words(&["pi"]), None),
            (words(&["pi", "--session"]), None),
            (words(&["pi", "--session", "--model"]), None),
        ];
        for (command, session) in cases {
            assert_eq!(
                resumed(&command).as_deref(),
                session,
                "{command:?} resumes {session:?}"
            );
        }
    }

    #[tokio::test]
    async fn no_node_means_no_wait() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            wait(&dir.path().join("work.sock"), "01a1-ab"),
        )
        .await
        .expect("a holder with no node to ask starts its program at once");
    }
}
