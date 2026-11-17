//! Whether what a served record waits on is already done, asked again each
//! time the record is shown, so nobody is asked for an act they already
//! did. A record says its act in `doneWhen`, `{"probe", ...}`, and the
//! probe it names answers done, not done, or unknown; only done takes the
//! record away, and a probe this node doesn't know, or one that can't
//! tell, keeps it.
//!
//! The steward asked Joel on 16 Nov to post a status he had posted six
//! minutes before (misses row 34): the wait was written once and never
//! looked at again. The probes read only this machine and the programs
//! the person signs in with, each held to [`PROBE_WITHIN`], and read the
//! record's own words for nothing but which repository, which number or
//! which tool, each checked before it reaches a program's arguments.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;

/// The field of a record that says what it waits on.
pub const FIELD: &str = "doneWhen";

/// The longest a probe may take before it is taken as unknown.
pub const PROBE_WITHIN: Duration = Duration::from_secs(5);

/// How long a verdict another program wrote on a record is believed, in
/// milliseconds: fifteen minutes, as it refreshes about every five.
pub const VERDICT_KEPT: i64 = 15 * 60 * 1000;

/// What a probe says of an act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// It is done, so nobody need be asked for it.
    Done,
    /// It isn't done yet.
    NotDone,
    /// The probe can't tell.
    Unknown,
}

/// What `record` says it waits on, where it says.
#[must_use]
pub fn act(record: &Value) -> Option<&Value> {
    record.get(FIELD).filter(|act| act.is_object())
}

/// Whether the act `when` says is done, as its probe answers on this
/// machine now.
pub async fn probe(when: &Value) -> Verdict {
    match when.get("probe").and_then(Value::as_str) {
        Some("rebooted") => rebooted(when, crate::store::folders::boot_id().as_deref()),
        Some("switch-done") => switched(when, &current_system()),
        Some("message-posted") => posted(when, jiff::Timestamp::now().as_millisecond()),
        Some("pr-merged") => match pr_view(when) {
            Some(asked) => merged(run("gh", &asked).await.as_ref()),
            None => Verdict::Unknown,
        },
        Some("signed-in") => match signing_in(when) {
            Some((program, asked)) => signed_in(run(program, &asked).await.as_ref()),
            None => Verdict::Unknown,
        },
        _ => Verdict::Unknown,
    }
}

/// Whether the machine started again since the boot `boot_id` names,
/// as the boot `now` is.
fn rebooted(when: &Value, now: Option<&str>) -> Verdict {
    match (when.get("boot_id").and_then(Value::as_str), now) {
        (Some(then), Some(now)) if then != now => Verdict::Done,
        (Some(_), Some(_)) => Verdict::NotDone,
        _ => Verdict::Unknown,
    }
}

/// The system this machine runs now, as its link says, where it has one.
fn current_system() -> Option<PathBuf> {
    std::fs::canonicalize("/run/current-system").ok()
}

/// Whether the machine runs the system `wanted` names, as `current` is.
fn switched(when: &Value, current: &Option<PathBuf>) -> Verdict {
    match (when.get("wanted").and_then(Value::as_str), current) {
        (Some(wanted), Some(current)) if Path::new(wanted) == current => Verdict::Done,
        (Some(_), Some(_)) => Verdict::NotDone,
        _ => Verdict::Unknown,
    }
}

/// What the `verdict` another program wrote on `when` says at `now`:
/// `{"done", "at"}`, believed for [`VERDICT_KEPT`] after its `at`.
fn posted(when: &Value, now: i64) -> Verdict {
    let Some(verdict) = when.get("verdict") else {
        return Verdict::Unknown;
    };
    let at = verdict
        .get("at")
        .and_then(Value::as_str)
        .and_then(|at| at.parse::<jiff::Timestamp>().ok())
        .map(jiff::Timestamp::as_millisecond);
    match (verdict.get("done").and_then(Value::as_bool), at) {
        (Some(done), Some(at)) if (0..=VERDICT_KEPT).contains(&(now - at)) => {
            if done {
                Verdict::Done
            } else {
                Verdict::NotDone
            }
        }
        _ => Verdict::Unknown,
    }
}

/// The words asking GitHub how the pull request `when` names stands, or
/// none where its repository isn't `owner/name` or its number isn't one.
fn pr_view(when: &Value) -> Option<Vec<String>> {
    if when.get("host").and_then(Value::as_str).unwrap_or("github") != "github" {
        return None;
    }
    let repo = when.get("repo").and_then(Value::as_str)?;
    let number = when.get("number").and_then(Value::as_u64)?;
    let (owner, name) = repo.split_once('/')?;
    let reads = |part: &str| {
        !part.is_empty()
            && !part.starts_with(['-', '.'])
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    };
    (reads(owner) && reads(name)).then(|| {
        [
            "pr",
            "view",
            &number.to_string(),
            "--repo",
            repo,
            "--json",
            "state",
            "--jq",
            ".state",
        ]
        .map(str::to_owned)
        .to_vec()
    })
}

/// What `gh pr view` said of a pull request's state.
fn merged(said: Option<&Said>) -> Verdict {
    match said {
        Some(Said { ok: true, out }) if out.trim() == "MERGED" => Verdict::Done,
        Some(Said { ok: true, .. }) => Verdict::NotDone,
        _ => Verdict::Unknown,
    }
}

/// The program and words that ask whether the person is signed in to the
/// tool `when` names, for the tools this node knows.
fn signing_in(when: &Value) -> Option<(&'static str, Vec<String>)> {
    let asked = |words: &[&str]| words.iter().map(|word| (*word).to_owned()).collect();
    match when.get("tool").and_then(Value::as_str)? {
        "gh" => Some(("gh", asked(&["auth", "status"]))),
        "gcloud" => Some(("gcloud", asked(&["auth", "print-access-token", "-q"]))),
        _ => None,
    }
}

/// Whether a sign-in probe found the person signed in: it ran, and said
/// so by succeeding.
fn signed_in(said: Option<&Said>) -> Verdict {
    match said {
        Some(Said { ok: true, .. }) => Verdict::Done,
        Some(Said { ok: false, .. }) => Verdict::NotDone,
        None => Verdict::Unknown,
    }
}

/// What a program said: whether it succeeded, and what it wrote.
#[derive(Debug)]
struct Said {
    ok: bool,
    out: String,
}

/// Runs `program` with `words`, nothing on its input, held to
/// [`PROBE_WITHIN`]; none where it can't run or doesn't end in time.
async fn run(program: &str, words: &[String]) -> Option<Said> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(words)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(PROBE_WITHIN, command.output())
        .await
        .ok()?
        .ok()?;
    Some(Said {
        ok: output.status.success(),
        out: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}

#[cfg(test)]
mod tests;
