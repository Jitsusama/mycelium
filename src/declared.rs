//! What a machine's own configuration says of it: what can't be measured
//! from the machine, such as the networks work there reaches or what it
//! refuses. Only the machine's built configuration declares it, as a file
//! it writes where every end of a link on the machine finds it; nothing
//! said at run time changes it. A machine whose configuration says
//! nothing, or says it in a shape this build doesn't read, declares
//! nothing known, never a guess.
//!
//! A refusal is a taint, as a cluster scheduler's are: work goes where
//! no taint it doesn't tolerate applies, and the machine itself enforces
//! it, since what is said here only saves work a trip.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// What a machine's configuration says of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declared {
    /// The networks work there reaches, such as `home` or `internet`.
    pub reach: Vec<String>,
    /// What a session held there outlives.
    pub outlives: Vec<Outlives>,
    /// Whether work there costs money.
    pub costs_money: bool,
    /// What it refuses, or would rather not take.
    pub taints: Vec<Taint>,
}

/// Something a held session can outlive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outlives {
    /// The window it shows in closing.
    WindowClose,
    /// Its node restarting.
    NodeRestart,
    /// The machine sleeping.
    Sleep,
    /// The machine restarting.
    Reboot,
}

/// A refusal, by a key work tolerates it by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Taint {
    /// What work names to tolerate it, such as `person-works`.
    pub key: String,
    /// How hard it refuses.
    pub effect: Effect,
    /// Why, for a person reading where work went.
    pub why: String,
}

/// How hard a taint refuses work that doesn't tolerate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// The work never goes there.
    Never,
    /// The work goes there only when nowhere else will do.
    PreferNot,
}

/// What the configuration at `file` declares, or nothing known where it
/// declares nothing. A file in a shape this build doesn't read is told
/// of, since its configuration meant to say something.
#[must_use]
pub fn read(file: &Path) -> Option<Declared> {
    let text = match fs::read(file) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            eprintln!("what {} declares can't be read: {error}", file.display());
            return None;
        }
    };
    serde_json::from_slice(&text)
        .inspect_err(|error| {
            eprintln!("what {} declares isn't read: {error}", file.display());
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configuration_declares_what_cant_be_measured() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        fs::write(
            &file,
            r#"{"reach": ["home", "internet"],
                "outlives": ["window_close", "node_restart"],
                "costs_money": false,
                "taints": [{"key": "person-works", "effect": "prefer_not",
                            "why": "its person works here"}]}"#,
        )
        .expect("the configuration is written");
        assert_eq!(
            read(&file),
            Some(Declared {
                reach: vec!["home".to_owned(), "internet".to_owned()],
                outlives: vec![Outlives::WindowClose, Outlives::NodeRestart],
                costs_money: false,
                taints: vec![Taint {
                    key: "person-works".to_owned(),
                    effect: Effect::PreferNot,
                    why: "its person works here".to_owned(),
                }],
            }),
            "it reads as declared"
        );
    }

    #[test]
    fn nothing_is_known_where_nothing_is_declared_as_this_build_reads_it() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        assert_eq!(read(&file), None, "no configuration declares nothing");
        fs::write(&file, r#"{"reach": ["home"]}"#).expect("written");
        assert_eq!(read(&file), None, "a part left out is never guessed");
        fs::write(
            &file,
            r#"{"reach": [], "outlives": ["moving"], "costs_money": false, "taints": []}"#,
        )
        .expect("written");
        assert_eq!(read(&file), None, "nor is a word this build doesn't know");
    }
}
