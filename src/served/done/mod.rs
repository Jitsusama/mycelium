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

use std::path::Path;
use std::process::Stdio;
use std::sync::{Mutex, PoisonError};
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

/// What this machine is now, as the probes told at once read it.
#[derive(Debug, Clone, Copy)]
pub struct Machine<'a> {
    /// The boot it runs, as `/proc` names it, where it names one.
    pub boot: Option<&'a str>,
    /// The system it runs, its link followed, where it has one.
    pub system: Option<&'a Path>,
    /// Now, in milliseconds since the epoch.
    pub now: i64,
}

/// How an act's probe answers: told at once from what this machine holds,
/// or by asking a program, which takes time.
#[derive(Debug)]
pub enum Probe {
    /// Told at once.
    Told {
        /// What it says.
        verdict: Verdict,
        /// Whether it may say otherwise later with the record unchanged.
        may_change: bool,
    },
    /// Told by asking a program.
    Asked(Asking),
}

/// A program to ask whether an act is done, and how to read what it says.
#[derive(Debug)]
pub struct Asking {
    program: &'static str,
    words: Vec<String>,
    reads: fn(Option<&Said>) -> Verdict,
    /// Whether what it writes is read, so output nothing reads, such as
    /// a sign-in token, is never held.
    reads_output: bool,
    /// Whether a done it says stays done, so it need not be asked again.
    pub lasting: bool,
}

impl Asking {
    /// Asks it, held to [`PROBE_WITHIN`].
    pub async fn answer(&self) -> Verdict {
        (self.reads)(
            run(self.program, &self.words, self.reads_output)
                .await
                .as_ref(),
        )
    }
}

/// What `record` says it waits on, where it says.
#[must_use]
pub fn act(record: &Value) -> Option<&Value> {
    record.get(FIELD).filter(|act| act.is_object())
}

/// How the probe of the act `when` answers on `machine`.
#[must_use]
pub fn probe(when: &Value, machine: &Machine) -> Probe {
    let told = |verdict, may_change| Probe::Told {
        verdict,
        may_change,
    };
    match when.get("probe").and_then(Value::as_str) {
        // A machine started again starts its node again, so the boot it
        // reads never changes while it runs.
        Some("rebooted") => told(rebooted(when, machine.boot), false),
        Some("switch-done") => told(switched(when, machine.system), true),
        // Its verdict comes of age and goes unknown, so it changes too.
        Some("message-posted") => told(posted(when, machine.now), true),
        Some("pr-merged") => match pr_view(when) {
            Some(words) => Probe::Asked(Asking {
                program: "gh",
                words,
                reads: merged,
                reads_output: true,
                lasting: true,
            }),
            None => told(Verdict::Unknown, false),
        },
        // A sign-in can lapse, so a done one is asked again.
        Some("signed-in") => match signing_in(when) {
            Some((program, words)) => Probe::Asked(Asking {
                program,
                words,
                reads: signed_in,
                reads_output: false,
                lasting: false,
            }),
            None => told(Verdict::Unknown, false),
        },
        _ => told(Verdict::Unknown, false),
    }
}

/// Whether the machine started again since the boot `boot_id` names,
/// as the boot `now` is. Each is read as the 32 hex digits a boot is,
/// whatever its case, dashes or spaces, so one written another way is
/// never taken for another boot; one that isn't a boot can't tell.
fn rebooted(when: &Value, now: Option<&str>) -> Verdict {
    let then = when.get("boot_id").and_then(Value::as_str).and_then(boot);
    match (then, now.and_then(boot)) {
        (Some(then), Some(now)) if then != now => Verdict::Done,
        (Some(_), Some(_)) => Verdict::NotDone,
        _ => Verdict::Unknown,
    }
}

/// The boot `text` names, as its 32 hex digits in lower case.
fn boot(text: &str) -> Option<String> {
    let digits: String = text
        .trim()
        .chars()
        .filter(|char| *char != '-')
        .map(|char| char.to_ascii_lowercase())
        .collect();
    (digits.len() == 32 && digits.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(digits)
}

/// Whether the machine runs the system `wanted` names, as `current` is.
fn switched(when: &Value, current: Option<&Path>) -> Verdict {
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

/// The programs this node has said it can't run, each said once.
static UNRUN: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

/// Runs `program` with `words`, nothing on its input, held to
/// [`PROBE_WITHIN`], keeping what it writes only where `reads_output`;
/// none where it can't run or doesn't end in time. One that can't run is
/// said in the node's log, once, as every wait on it stays.
async fn run(program: &'static str, words: &[String], reads_output: bool) -> Option<Said> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(words)
        .stdin(Stdio::null())
        .stdout(if reads_output {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(PROBE_WITHIN, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            let mut said = UNRUN.lock().unwrap_or_else(PoisonError::into_inner);
            if !said.contains(&program) {
                said.push(program);
                eprintln!("{program} can't run, so waits that ask it stay: {error}");
            }
            return None;
        }
        Err(_) => return None,
    };
    Some(Said {
        ok: output.status.success(),
        out: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}

#[cfg(test)]
mod tests;
