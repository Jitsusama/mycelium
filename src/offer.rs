//! What a provider's program offers: the shapes of machine it can make,
//! each with what such a machine would be, what it costs an hour, and how
//! long one lasts. Asked as `PROGRAM offers`, it writes one JSON line a
//! shape and ends.
//!
//! An offer is the provider's promise, never a fact about a machine: no
//! machine takes another's word about itself, so an offer only decides
//! whether one is worth making, and once made, its own node says what it
//! is. Offers are asked for when wanted, never on a timer.

use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::declared::Declared;

/// How long a program has to say what it offers. Saying so reads its own
/// configuration or asks its provider once, so it takes seconds.
const OFFERS_WITHIN: Duration = Duration::from_secs(30);

/// One shape of machine a provider's program can make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    /// The program that offers it, as the node was given it.
    pub provider: String,
    /// The shape, in the program's own words, as it is asked to make one.
    pub shape: String,
    /// What a machine of this shape would be.
    pub facts: Shape,
    /// What one costs while it lives.
    pub costs: Costs,
    /// The longest one lives, in seconds, where its provider fixes it.
    pub lasts: Option<u64>,
}

/// What a machine of a shape would be, in the words its own node says it
/// in once it is made: anything left out is unknown, never a guess.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shape {
    /// The system it evaluates and builds for, such as `x86_64-linux`.
    #[serde(default)]
    pub system: Option<String>,
    /// How many processors it runs at once.
    #[serde(default)]
    pub cores: Option<u32>,
    /// Its memory, in bytes.
    #[serde(default)]
    pub memory: Option<u64>,
    /// Its disk, in bytes.
    #[serde(default)]
    pub disk: Option<u64>,
    /// What its configuration would declare of it.
    #[serde(default)]
    pub declared: Option<Declared>,
}

/// What a machine costs while it lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Costs {
    /// An hour's cost, as a decimal written out, such as `0.12`, so no
    /// amount is rounded on its way through.
    pub hourly: String,
    /// The currency, as its three-letter code, such as `USD`.
    pub currency: String,
}

/// One line of what a program says it offers, before it is known to be
/// whole.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Said {
    shape: String,
    #[serde(default)]
    facts: Shape,
    costs: Costs,
    #[serde(default)]
    lasts: Option<u64>,
}

/// Asks `program` what it offers.
///
/// # Errors
///
/// When it can't be run, ends with a failure or not within
/// [`OFFERS_WITHIN`], or says something that isn't an offer, saying which.
pub async fn asked(program: &str) -> Result<Vec<Offer>, String> {
    let mut command = tokio::process::Command::new(program);
    command
        .arg("offers")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let ran = tokio::time::timeout(OFFERS_WITHIN, command.output())
        .await
        .map_err(|_| {
            format!(
                "{program} offers said nothing within {}s",
                OFFERS_WITHIN.as_secs()
            )
        })?
        .map_err(|error| format!("{program} offers couldn't run: {error}"))?;
    if !ran.status.success() {
        let said = String::from_utf8_lossy(&ran.stderr);
        return Err(format!(
            "{program} offers ended {}: {}",
            ran.status,
            said.trim()
        ));
    }
    read(program, &String::from_utf8_lossy(&ran.stdout))
}

/// The offers `program` wrote as `text`, a JSON line each.
///
/// # Errors
///
/// When a line isn't an offer, or names a shape twice, saying which.
pub fn read(program: &str, text: &str) -> Result<Vec<Offer>, String> {
    let mut offers: Vec<Offer> = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let said: Said = serde_json::from_str(line)
            .map_err(|error| format!("{program} offered {line}, which isn't an offer: {error}"))?;
        if let Some(why) = unsound(&said) {
            return Err(format!("{program} offered {line}, but {why}"));
        }
        if offers.iter().any(|offer| offer.shape == said.shape) {
            return Err(format!("{program} offered {} twice", said.shape));
        }
        offers.push(Offer {
            provider: program.to_owned(),
            shape: said.shape,
            facts: said.facts,
            costs: said.costs,
            lasts: said.lasts,
        });
    }
    Ok(offers)
}

/// Why `said` can't be taken as an offer, if it can't.
fn unsound(said: &Said) -> Option<&'static str> {
    if said.shape.trim().is_empty() {
        return Some("it names no shape");
    }
    let hourly = &said.costs.hourly;
    let (whole, part) = hourly.split_once('.').unwrap_or((hourly, "0"));
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(whole) || !digits(part) {
        return Some("its hourly cost isn't a decimal written out, such as 0.12");
    }
    let currency = &said.costs.currency;
    if currency.len() != 3 || !currency.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Some("its currency isn't a three-letter code, such as USD");
    }
    if said.lasts == Some(0) {
        return Some("a machine that lasts no time can't be made");
    }
    None
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const SCRATCH: &str = r#"{"shape": "scratch", "facts": {"system": "x86_64-linux", "cores": 4, "memory": 8589934592}, "costs": {"hourly": "0.12", "currency": "USD"}, "lasts": 7200}"#;

    /// A provider program at `dir`/provider running `body`, put in place by
    /// a child process, since another test's fork holding the file open
    /// for writing would keep it from running.
    fn provider(dir: &std::path::Path, body: &str) -> String {
        let text = dir.join("provider.sh");
        let program = dir.join("provider");
        fs::write(&text, format!("#!/bin/sh\n{body}")).expect("the provider is written");
        let put = std::process::Command::new("install")
            .args(["-m", "755"])
            .arg(&text)
            .arg(&program)
            .status()
            .expect("install runs");
        assert!(put.success(), "the provider is put in place");
        program.display().to_string()
    }

    #[test]
    fn an_offer_says_its_shape_facts_cost_and_how_long_it_lasts() {
        let offers = read("aquifer-link", &format!("{SCRATCH}\n\n")).expect("read");
        assert_eq!(
            offers,
            vec![Offer {
                provider: "aquifer-link".to_owned(),
                shape: "scratch".to_owned(),
                facts: Shape {
                    system: Some("x86_64-linux".to_owned()),
                    cores: Some(4),
                    memory: Some(8_589_934_592),
                    ..Shape::default()
                },
                costs: Costs {
                    hourly: "0.12".to_owned(),
                    currency: "USD".to_owned(),
                },
                lasts: Some(7200),
            }],
            "one offer a line, what it leaves out unknown, blank lines let be"
        );
    }

    #[test]
    fn what_isnt_a_whole_offer_is_refused_saying_why() {
        for (line, why) in [
            (r#"{"shape": "scratch"}"#, "missing field `costs`"),
            (
                r#"{"shape": "scratch", "costs": {"hourly": "0.12", "currency": "USD"}, "size": 4}"#,
                "unknown field `size`",
            ),
            (
                r#"{"shape": "scratch", "facts": {"gpus": 1}, "costs": {"hourly": "0.12", "currency": "USD"}}"#,
                "unknown field `gpus`",
            ),
            (
                r#"{"shape": " ", "costs": {"hourly": "0.12", "currency": "USD"}}"#,
                "names no shape",
            ),
            (
                r#"{"shape": "scratch", "costs": {"hourly": "1e-3", "currency": "USD"}}"#,
                "isn't a decimal written out",
            ),
            (
                r#"{"shape": "scratch", "costs": {"hourly": ".5", "currency": "USD"}}"#,
                "isn't a decimal written out",
            ),
            (
                r#"{"shape": "scratch", "costs": {"hourly": "0.12", "currency": "usd"}}"#,
                "three-letter code",
            ),
            (
                r#"{"shape": "scratch", "costs": {"hourly": "0.12", "currency": "USD"}, "lasts": 0}"#,
                "lasts no time",
            ),
        ] {
            let refused = read("aquifer-link", line).expect_err(line);
            assert!(
                refused.contains(why),
                "{line} is refused for {why}: {refused}"
            );
        }
        let twice = read("aquifer-link", &format!("{SCRATCH}\n{SCRATCH}\n")).expect_err("twice");
        assert!(
            twice.contains("offered scratch twice"),
            "a shape is offered once: {twice}"
        );
    }

    #[tokio::test]
    async fn a_program_is_asked_what_it_offers_and_a_failure_says_why() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let args = dir.path().join("args");
        let program = provider(
            dir.path(),
            &format!(
                "printf '%s\\n' \"$@\" > {}\necho '{SCRATCH}'\n",
                args.display()
            ),
        );
        let offers = asked(&program).await.expect("it offers");
        assert_eq!(
            offers
                .iter()
                .map(|offer| offer.shape.as_str())
                .collect::<Vec<_>>(),
            ["scratch"],
            "what it wrote is what it offers"
        );
        assert_eq!(
            fs::read_to_string(&args).expect("the provider was asked"),
            "offers\n",
            "the program is asked only what it offers"
        );

        let failing = tempfile::tempdir().expect("a temporary folder");
        let program = provider(failing.path(), "echo 'no login' >&2\nexit 3\n");
        let refused = asked(&program).await.expect_err("it fails");
        assert!(
            refused.contains("exit status: 3") && refused.contains("no login"),
            "a failure says how it ended and what the program said: {refused}"
        );
    }
}
