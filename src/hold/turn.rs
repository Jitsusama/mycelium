//! A holder's wait for its turn: a held program that resumes a session,
//! such as `pi --session <id>`, starts only once its node lets it, so a
//! restart's sessions come back a few at a time rather than all at once
//! (the node's `turns` says how they are let through).
//!
//! Whatever goes wrong goes the old way: a node that isn't there, can't
//! be reached, is too old to know turns, or restarts while the holder
//! waits lets the program start at once, so no session is ever left
//! waiting on a node that won't answer.

use std::iter::Peekable;
use std::path::Path;
use std::str::Chars;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::wire::Request;

/// The words before the session a command resumes.
const RESUMING: [&str; 2] = ["--session", "--session-id"];

/// The session `command` resumes, by id, wherever its words name one
/// after `--session` or `--session-id`, whole or within the script a
/// shell runs, as `sh -c "... exec pi --session <id>"` does. A word is
/// only ever read whole: one holding a person's words, as
/// `QUEST_WORKFLOW_FIRST_TEXT=fix --session ...` does, names nothing. A
/// session named by its log's path, as
/// `.../2026-11-14T22-23-37-209Z_<id>.jsonl`, is the id that path ends in.
#[must_use]
pub fn resumed(command: &[String]) -> Option<String> {
    trimmed(named(command).first()?)
}

/// Whether `command` resumes `session`: by the very word it names, or a
/// log's path ending in it, as `.../<time>Z_<session>.jsonl`, since
/// [`resumed`] reads an `_` or a `.` as a log path's, and an id may hold
/// either. So `pi --session a_b` resumes `a_b`, though [`resumed`] says
/// `b`. It errs toward a match, as `x_a` resuming `a` too, and as any
/// word the command names a session by, not only the first, since a
/// match only refuses a reopening, where a miss would start a second pi.
#[must_use]
pub fn resumes(command: &[String], session: &str) -> bool {
    named(command).into_iter().any(|word| {
        let stem = Path::new(&word).file_stem().and_then(|stem| stem.to_str());
        word == session
            || stem == Some(session)
            || stem.is_some_and(|stem| {
                stem.strip_suffix(session)
                    .is_some_and(|it| it.ends_with('_'))
            })
    })
}

/// Every session `command` names by `--session` or `--session-id`, under
/// any program or in a shell's `-c` script with its positional
/// parameters filled in: each such word, first to last, and the id that
/// word ends in where it may be a log's path, as [`resumed`] reads it.
/// It errs toward naming too many, as [`resumes`] does, since each is
/// only held to what stands. An id a script works out as it runs, or a
/// resume naming none, as `--continue`, is not found.
#[must_use]
pub fn sessions_named(command: &[String]) -> Vec<String> {
    let mut sessions = Vec::new();
    for word in named(command) {
        let id = trimmed(&word);
        for session in std::iter::once(word).chain(id) {
            if !session.is_empty() && !sessions.contains(&session) {
                sessions.push(session);
            }
        }
    }
    sessions
}

/// Each word [`sessions_named`] finds, as `command` writes it, never the
/// id a log's path ends in: the words pi may take for the start of an
/// id, which a path's tail never is.
#[must_use]
pub fn words_named(command: &[String]) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for word in named(command) {
        if !word.is_empty() && !words.contains(&word) {
            words.push(word);
        }
    }
    words
}

/// Each word naming a session `command` resumes, first to last, as
/// [`resumed`] finds the first, before it is read as a log's path.
fn named(command: &[String]) -> Vec<String> {
    let words: Vec<String> = command
        .iter()
        .enumerate()
        .flat_map(|(at, word)| {
            if is_script(command, at) {
                script_words(word, &command[at + 1..])
            } else {
                vec![word.clone()]
            }
        })
        .collect();
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    words
        .iter()
        .enumerate()
        .filter_map(|(at, word)| {
            if let Some((flag, id)) = word.split_once('=')
                && RESUMING.contains(&flag)
            {
                return Some(id);
            }
            RESUMING
                .contains(word)
                .then(|| words.get(at + 1).copied())
                .flatten()
        })
        .map(str::to_owned)
        .collect()
}

/// The id `named` gives, where it may be a log's path, as
/// `.../2026-11-14T22-23-37-209Z_<id>.jsonl` is.
fn trimmed(named: &str) -> Option<String> {
    let id = Path::new(named).file_stem()?.to_str()?.rsplit('_').next()?;
    (!id.is_empty() && !id.starts_with('-')).then(|| id.to_owned())
}

/// The shells whose `-c` takes a script, by their program's file name.
const SHELLS: [&str; 6] = ["sh", "bash", "zsh", "dash", "ksh", "mksh"];

/// What ends a word in a script outside quotes, besides a space.
const BETWEEN_WORDS: &str = ";&|()<>";

/// Whether the word `at` in `command` is a script a shell runs: the one
/// after its `-c`, alone or among other flags as `-lc` and `-lic` are,
/// where the program those flags follow is a shell. Another program's
/// `-c`, as pi's own, takes no script, so its next word stays whole.
fn is_script(command: &[String], at: usize) -> bool {
    let Some(before) = at.checked_sub(1) else {
        return false;
    };
    let flags = &command[before];
    let short = flags
        .strip_prefix('-')
        .is_some_and(|letters| letters.chars().all(|letter| letter.is_ascii_alphabetic()));
    if !short || !flags.contains('c') {
        return false;
    }
    command[..before]
        .iter()
        .rev()
        .find(|word| !word.starts_with('-'))
        .and_then(|program| Path::new(program).file_name()?.to_str())
        .is_some_and(|name| SHELLS.contains(&name))
}

/// The words a shell makes of `script`, its quotes taken off: what's
/// within quotes, `"..."`, `'...'` or bash's `$'...'`, or after a
/// backslash, stays within its word. A positional parameter, `$3` or
/// `${3}`, is what `given` holds there, `$0` first, as a script handed
/// its arguments reads them; nothing else is expanded.
fn script_words(script: &str, given: &[String]) -> Vec<String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    // The quote open: ', ", or $ for bash's $'...', whose backslash
    // escapes its next letter.
    let mut quote: Option<char> = None;
    let mut letters = script.chars().peekable();
    while let Some(letter) = letters.next() {
        let held = match quote {
            Some('\'') if letter == '\'' => {
                quote = None;
                None
            }
            Some('\'') => Some(letter.to_string()),
            Some(open) => match letter {
                '"' if open == '"' => {
                    quote = None;
                    None
                }
                '\'' if open == '$' => {
                    quote = None;
                    None
                }
                '\\' => escaped(&mut letters, open == '"'),
                '$' if open == '"' => Some(parameter(&mut letters, given)),
                _ => Some(letter.to_string()),
            },
            None => match letter {
                '\'' | '"' => {
                    quote = Some(letter);
                    Some(String::new())
                }
                '$' if letters.peek() == Some(&'\'') => {
                    letters.next();
                    quote = Some('$');
                    Some(String::new())
                }
                '\\' => escaped(&mut letters, false),
                '$' => Some(parameter(&mut letters, given)),
                _ if letter.is_whitespace() || BETWEEN_WORDS.contains(letter) => {
                    words.extend(word.take());
                    None
                }
                _ => Some(letter.to_string()),
            },
        };
        if let Some(held) = held {
            word.get_or_insert_with(String::new).push_str(&held);
        }
    }
    words.extend(word);
    words
}

/// What a backslash makes of the letter after it: nothing of a newline,
/// which only continues the line, else that letter. Within double
/// quotes (`quoted`) only `$`, `` ` ``, `"` and `\` are escaped; before
/// any other letter the backslash stays.
fn escaped(letters: &mut Peekable<Chars<'_>>, quoted: bool) -> Option<String> {
    match letters.next() {
        None | Some('\n') => None,
        Some(letter) if quoted && !"$`\"\\".contains(letter) => Some(format!("\\{letter}")),
        Some(letter) => Some(letter.to_string()),
    }
}

/// What follows a `$` in a script: the positional parameter it names,
/// `$3` or `${3}`, from `given`, `$0` first, empty when unset; any other
/// `$` stays as it is.
fn parameter(letters: &mut Peekable<Chars<'_>>, given: &[String]) -> String {
    let at = match letters.peek() {
        Some(digit) if digit.is_ascii_digit() => {
            letters.next().and_then(|digit| digit.to_digit(10))
        }
        Some('{') => {
            // Only `${`, digits, then `}` names one; anything else in the
            // braces leaves the `$` as it is, and the letters unread.
            let mut ahead = letters.clone();
            ahead.next();
            let mut digits = String::new();
            while let Some(digit) = ahead.next_if(char::is_ascii_digit) {
                digits.push(digit);
            }
            if ahead.next() == Some('}') {
                *letters = ahead;
                digits.parse::<u32>().ok()
            } else {
                None
            }
        }
        _ => None,
    };
    match at {
        Some(at) => usize::try_from(at)
            .ok()
            .and_then(|at| given.get(at))
            .cloned()
            .unwrap_or_default(),
        None => "$".to_owned(),
    }
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

    // Round council-20261125T115239116-000008: a relayed start is held
    // to every session it names, by whatever program, in whatever shape.
    #[test]
    fn every_session_a_command_names_is_found_whatever_runs_it() {
        let cases: [(&[&str], &[&str]); 7] = [
            (&["pi", "--session", "s-1", "--thinking", "high"], &["s-1"]),
            (&["env", "A=b", "pi", "--session=s-1", "go"], &["s-1"]),
            (&["/nix/store/x-pi/bin/pi", "--session-id", "s-2"], &["s-2"]),
            (
                &["sh", "-c", "exec pi --session \"$1\"", "sh", "s-3"],
                &["s-3"],
            ),
            (
                &["pi", "--session", "/l/2026-11-14T22-23-37-209Z_s-4.jsonl"],
                &["/l/2026-11-14T22-23-37-209Z_s-4.jsonl", "s-4"],
            ),
            (&["pi", "--session", "a", "--session", "a"], &["a"]),
            (&["pi", "QUEST_WORKFLOW_FIRST_TEXT=fix --session s-5"], &[]),
        ];
        for (command, sessions) in cases {
            assert_eq!(
                sessions_named(&words(command)),
                words(sessions),
                "{command:?}"
            );
        }
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
        resumes(cases);
    }

    // Round council-20261125T054114584-000006 (F5): an id holding an
    // `_` or a `.` is read as a log path's, so its holder went unseen.
    #[test]
    fn a_command_resumes_an_id_holding_what_a_path_would() {
        for id in ["a_b", "a.b", "01a1-gh"] {
            assert!(
                super::resumes(&words(&["pi", "--session", id]), id),
                "pi --session {id} resumes {id}"
            );
            assert!(
                super::resumes(
                    &words(&["sh", "-c", &format!("exec pi --session={id}")]),
                    id
                ),
                "so does a script naming it"
            );
            let log =
                format!("/h/.pi/agent/sessions/--h-src--/2026-11-14T22-23-37-209Z_{id}.jsonl");
            assert!(
                super::resumes(&words(&["pi", "--session", &log]), id),
                "and a log's path ending in it (round 7's F1)"
            );
        }
        let log = "/h/.pi/agent/sessions/--h-src--/2026-11-14T22-23-37-209Z_01a1-gh.jsonl";
        assert!(
            super::resumes(&words(&["pi", "--session", log]), "01a1-gh"),
            "a log's path resumes the id it ends in"
        );
        assert!(
            !super::resumes(&words(&["pi", "--session", "a_b"]), "a"),
            "and nothing but what it names"
        );
        assert!(
            !super::resumes(&words(&["pi"]), "a_b"),
            "one naming none resumes none"
        );
        // Round council-20261125T103525593-000003: a session named after
        // another is resumed too, since pi may take the last.
        assert!(
            super::resumes(
                &words(&["sh", "-c", "tool --session-id q; exec pi --session x"]),
                "x"
            ),
            "a script naming one before the pi still resumes the pi's"
        );
        assert!(
            super::resumes(&words(&["pi", "--session", "d", "--session", "x"]), "x"),
            "and so does a command naming two"
        );
    }

    fn resumes<const N: usize>(cases: [(Vec<String>, Option<&str>); N]) {
        for (command, session) in cases {
            assert_eq!(
                resumed(&command).as_deref(),
                session,
                "{command:?} resumes {session:?}"
            );
        }
    }

    #[test]
    fn a_word_holding_a_persons_words_names_no_session() {
        let cases = [
            (
                words(&[
                    "env",
                    "QUEST_WORKFLOW_FIRST_TEXT=fix --session 01a1-no",
                    "pi",
                ]),
                None,
            ),
            (
                words(&[
                    "env",
                    "QUEST_WORKFLOW_FIRST_TEXT=fix --session 01a1-no",
                    "pi",
                    "--session",
                    "01a1-kl",
                ]),
                Some("01a1-kl"),
            ),
            (
                words(&[
                    "sh",
                    "-c",
                    "QUEST_WORKFLOW_FIRST_TEXT='fix --session 01a1-no' exec pi",
                ]),
                None,
            ),
            (
                words(&[
                    "sh",
                    "-c",
                    "T=\"say \\\"--session 01a1-no\\\"\" exec pi --session '01a1-mn'",
                ]),
                Some("01a1-mn"),
            ),
            (words(&["pi", "fix --session 01a1-no"]), None),
            // pi's own -c takes no script, so a person's words after it stay whole.
            (words(&["pi", "-c", "fix --session 01a1-no"]), None),
        ];
        resumes(cases);
    }

    #[test]
    fn a_shells_script_names_its_session_as_the_shell_reads_it() {
        let cases = [
            (
                words(&["/bin/bash", "-lc", "cd ~/src && exec pi --session 01a1-op"]),
                Some("01a1-op"),
            ),
            (
                words(&["zsh", "-lic", "T=\"it's --session 01a1-no\" exec pi"]),
                None,
            ),
            (
                words(&[
                    "bash",
                    "-c",
                    "T=$'don\\'t use --session 01a1-no now' exec pi",
                ]),
                None,
            ),
            // A carry's start names the session it resumes as its third argument.
            (
                words(&[
                    "sh",
                    "-c",
                    "pi-carry arrive \"$1\" --session \"${3}\" || exit; exec pi --session \"$3\"",
                    "pi-carry",
                    "QEST-1",
                    "tern",
                    "01a1-qr",
                ]),
                Some("01a1-qr"),
            ),
            (
                words(&["sh", "-c", "(cd x;exec pi --session 01a1-st)"]),
                Some("01a1-st"),
            ),
            (
                words(&["sh", "-c", "exec pi --session 01a1-uv>log"]),
                Some("01a1-uv"),
            ),
            (
                words(&["sh", "-c", "exec pi --session \\\n  01a1-wx"]),
                Some("01a1-wx"),
            ),
        ];
        resumes(cases);
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
