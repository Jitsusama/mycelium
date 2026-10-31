//! Machines made from offers. A node makes one only for a start that says
//! it may cost money and that no machine of the realm takes, asking the
//! provider's program `PROGRAM make SHAPE --lasting SECONDS`. The program
//! makes it, writes one JSON line saying where it is (`place`, as
//! `--via PROGRAM:PLACE` names it), the name its node will say for itself
//! (`machine`) and when it ends (`until`), and exits.
//!
//! Every machine made has an end: the provider ends it by `until` whatever
//! else happens, and the node releases it sooner once its work is done.
//! Each make and release is kept, a JSON line each, in the node's book
//! ([`Book`]), so what was made, for how long and at what rate can always
//! be read back and summed.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::offer::{Costs, Offer};

/// How long a program has to make a machine: a provider makes one in
/// seconds, and a minute or two when it is busy.
const MADE_WITHIN: Duration = Duration::from_secs(5 * 60);

/// A machine a provider made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Made {
    /// The program that made it.
    pub provider: String,
    /// Its shape, as offered.
    pub shape: String,
    /// Where it is, in the program's own words, as `--via` takes it.
    pub place: String,
    /// The name its node will say for itself once it links.
    pub machine: String,
    /// When it ends, whatever else happens.
    pub until: jiff::Timestamp,
    /// What it costs while it lives, as offered.
    pub costs: Costs,
}

/// What a program says it made.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Said {
    place: String,
    machine: String,
    until: jiff::Timestamp,
}

/// Asks `offer`'s program to make a machine of its shape, to live
/// `lasting`.
///
/// # Errors
///
/// When it can't be run, ends with a failure or not within
/// [`MADE_WITHIN`], or says something other than what it made, saying
/// which.
pub async fn make(offer: &Offer, lasting: Duration) -> Result<Made, String> {
    let program = &offer.provider;
    let mut command = tokio::process::Command::new(program);
    command
        .args(["make", &offer.shape, "--lasting"])
        .arg(lasting.as_secs().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let ran = tokio::time::timeout(MADE_WITHIN, command.output())
        .await
        .map_err(|_| {
            format!(
                "{program} make {} said nothing within {}s",
                offer.shape,
                MADE_WITHIN.as_secs()
            )
        })?
        .map_err(|error| format!("{program} make couldn't run: {error}"))?;
    if !ran.status.success() {
        let said = String::from_utf8_lossy(&ran.stderr);
        return Err(format!(
            "{program} make {} ended {}: {}",
            offer.shape,
            ran.status,
            said.trim()
        ));
    }
    let text = String::from_utf8_lossy(&ran.stdout);
    let said: Said = serde_json::from_str(text.trim()).map_err(|error| {
        format!(
            "{program} make {} said {}, which isn't what it made: {error}",
            offer.shape,
            text.trim()
        )
    })?;
    Ok(Made {
        provider: program.clone(),
        shape: offer.shape.clone(),
        place: said.place,
        machine: said.machine,
        until: said.until,
        costs: offer.costs.clone(),
    })
}

/// Where a node keeps each make and release, a JSON line each, newest
/// last.
#[derive(Debug, Clone)]
pub struct Book {
    path: PathBuf,
}

impl Book {
    /// A book kept at `path`.
    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// Keeps that `made` was made.
    ///
    /// # Errors
    ///
    /// When the book can't be written.
    pub fn made(&self, made: &Made) -> io::Result<()> {
        self.keep(&json!({ "at": jiff::Timestamp::now(), "made": made }))
    }

    /// Keeps that `made` was released, or let end, and why.
    ///
    /// # Errors
    ///
    /// When the book can't be written.
    pub fn released(&self, made: &Made, why: &str) -> io::Result<()> {
        self.keep(&json!({
            "at": jiff::Timestamp::now(),
            "released": { "machine": made.machine, "place": made.place, "why": why },
        }))
    }

    fn keep(&self, line: &serde_json::Value) -> io::Result<()> {
        if let Some(folder) = self.path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let mut book = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(book, "{line}")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::offer::Shape;

    /// A program at `dir`'s `provider` running `body`, put in place by a
    /// child so no test's fork holds it open.
    fn provider(dir: &std::path::Path, body: &str) -> String {
        let text = dir.join("provider.sh");
        fs::write(&text, format!("#!/bin/sh\n{body}\n")).expect("the provider is written");
        let program = dir.join("provider");
        let put = std::process::Command::new("install")
            .args(["-m", "755"])
            .arg(&text)
            .arg(&program)
            .status()
            .expect("install runs");
        assert!(put.success(), "the provider is put in place");
        program.display().to_string()
    }

    fn offer_of(provider: String) -> Offer {
        Offer {
            provider,
            shape: "scratch".to_owned(),
            facts: Shape::default(),
            costs: Costs {
                hourly: "0.12".to_owned(),
                currency: "USD".to_owned(),
            },
            lasts: Some(7200),
        }
    }

    #[tokio::test]
    async fn a_program_makes_a_machine_saying_where_it_is_and_when_it_ends() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let asked = dir.path().join("asked");
        let program = provider(
            dir.path(),
            &format!(
                "echo \"$@\" > {}\necho '{{\"place\": \"sbx-1\", \"machine\": \"lent-1\", \"until\": \"2026-10-31T20:00:00Z\"}}'",
                asked.display()
            ),
        );
        let made = make(&offer_of(program.clone()), Duration::from_secs(3600))
            .await
            .expect("it is made");
        assert_eq!(
            fs::read_to_string(&asked).expect("it was asked"),
            "make scratch --lasting 3600\n",
            "the program is asked for its shape and how long it lives"
        );
        assert_eq!(
            made,
            Made {
                provider: program,
                shape: "scratch".to_owned(),
                place: "sbx-1".to_owned(),
                machine: "lent-1".to_owned(),
                until: "2026-10-31T20:00:00Z".parse().expect("a time"),
                costs: offer_of(String::new()).costs,
            },
            "what was made, where, until when and at what rate"
        );

        let book = Book::at(dir.path().join("state").join("made.jsonl"));
        book.made(&made).expect("its making is kept");
        book.released(&made, "its work was done")
            .expect("its release is kept");
        let kept: Vec<serde_json::Value> = fs::read_to_string(dir.path().join("state/made.jsonl"))
            .expect("the book is there")
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON line"))
            .collect();
        assert_eq!(
            (
                &kept[0]["made"]["costs"]["hourly"],
                &kept[1]["released"]["why"]
            ),
            (&json!("0.12"), &json!("its work was done")),
            "each make and release is kept, a line each, in order"
        );
    }

    #[tokio::test]
    async fn a_program_that_makes_nothing_says_why() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let failing = provider(dir.path(), "echo 'quota exceeded' >&2\nexit 3");
        let error = make(&offer_of(failing), Duration::from_secs(60))
            .await
            .expect_err("nothing is made");
        assert!(
            error.ends_with("quota exceeded"),
            "it says what the program said: {error}"
        );

        let other = tempfile::tempdir().expect("a temporary folder");
        let vague = provider(other.path(), "echo '{\"place\": \"sbx-1\"}'");
        let error = make(&offer_of(vague), Duration::from_secs(60))
            .await
            .expect_err("nothing is taken as made");
        assert!(
            error.contains("isn't what it made"),
            "a machine with no name or end isn't taken as made: {error}"
        );
    }
}
