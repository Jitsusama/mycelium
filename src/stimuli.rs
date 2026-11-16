//! Stimuli: what a receptor says on its network when more is asked of its
//! person, and the table that says which sources may say anything.
//!
//! The table is the person's: which sources may speak, kept in a file
//! only root or the Nix store holds, so no agent can make a source louder.
//! A source the table doesn't name is silent, and so is every source when
//! the file isn't sealed so. The sender never sets its own strength.
//!
//! What is said is built from counts alone, since a receptor's record
//! holds nothing else: the source's name and digits, never a message's
//! text, its sender or its subject. It is always an fyi, so it starts no
//! session's turn and asks nothing; a session that hears it weighs it as a
//! fact, and it grants nothing, starts nothing and steers nothing. A
//! source says at most once in [`QUIET_FOR`], however often its count
//! climbs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use jiff::Timestamp;
use serde::Deserialize;

use crate::group::Group;
use crate::receptor::{Record, Source};
use crate::record::Intent;
use crate::wire::Draft;

/// The least time between two things one source says: ten minutes, the
/// budget of one interruption a control room allows its people.
pub const QUIET_FOR: i64 = 600;

/// The person's table of which sources may say what more is asked of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
    say: BTreeSet<Source>,
}

/// The table as its file holds it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Written {
    #[serde(default)]
    say: BTreeSet<Source>,
}

impl Table {
    /// A table in which every source is silent.
    #[must_use]
    pub fn silent() -> Self {
        Self::default()
    }

    /// The table `file` holds, taken only where no account but
    /// `declarers` can change it, as what a machine declares is
    /// (`declared`). Unnamed, unsealed, unreadable or malformed, every
    /// source is silent, and why is said, for the node to say as it
    /// starts.
    #[must_use]
    pub fn read(file: Option<&Path>, declarers: &[u32]) -> (Self, Option<String>) {
        let Some(file) = file else {
            return (Self::silent(), None);
        };
        let taken = crate::declared::sealed(file, declarers)
            .and_then(|()| fs::read(file).map_err(|error| error.to_string()))
            .and_then(|text| {
                serde_json::from_slice::<Written>(&text).map_err(|error| error.to_string())
            });
        match taken {
            Ok(written) => (Self { say: written.say }, None),
            Err(why) => (
                Self::silent(),
                Some(format!(
                    "every source is silent, since {} isn't taken: {why}",
                    file.display()
                )),
            ),
        }
    }

    /// Whether `source` may say what more is asked of its person.
    #[must_use]
    pub fn says(&self, source: Source) -> bool {
        self.say.contains(&source)
    }
}

/// The name a receptor cell is known by, as the cells kind lists it:
/// `github@kestrel`.
#[must_use]
pub fn cell(source: Source, machine: &str) -> String {
    format!("{}@{machine}", source.name())
}

/// Whether `from` names a receptor cell, of any machine, or could be read
/// as one: what only a receptor's own node may say as. The part before the
/// `@` counts as a source's name with its case, digits, punctuation and
/// spaces set aside, so `GitHub@kestrel` or `git-hub@kestrel` is one, and
/// any letter outside ASCII makes it one too, since a look-alike such as a
/// Cyrillic `і` would read as the cell to whoever hears it.
#[must_use]
pub fn speaks_as_receptor(from: &str) -> bool {
    from.split_once('@').is_some_and(|(said, _)| {
        let letters: String = said
            .chars()
            .filter(char::is_ascii_alphabetic)
            .map(|c| c.to_ascii_lowercase())
            .collect();
        !said.is_ascii() || Source::named(&letters).is_some()
    })
}

/// What each receptor last said, or was first seen at, so a count is said
/// only as it climbs.
#[derive(Debug, Default)]
pub struct Signals {
    /// The count asked of the person each source was last compared from.
    from: BTreeMap<Source, u32>,
    /// When each source last said anything.
    said_at: BTreeMap<Source, Timestamp>,
}

impl Signals {
    /// What this machine's receptors say, judged as `receptors` now stand,
    /// by `table`, at `now`. A receptor first seen says nothing, so a node
    /// that starts doesn't say what was already there. One whose count
    /// climbed says how far, once its last word is [`QUIET_FOR`] behind;
    /// until then the climb waits, compared from what was last said.
    pub fn after(
        &mut self,
        receptors: &BTreeMap<Source, Record>,
        table: &Table,
        machine: &str,
        now: Timestamp,
    ) -> Vec<Draft> {
        let mut drafts = Vec::new();
        for (source, record) in receptors {
            let asked = record.counts.addressed;
            let Some(was) = self.from.insert(*source, asked) else {
                continue;
            };
            if asked <= was || !table.says(*source) {
                continue;
            }
            let quiet = self
                .said_at
                .get(source)
                .is_none_or(|at| now.as_second() - at.as_second() >= QUIET_FOR);
            if !quiet {
                self.from.insert(*source, was);
                continue;
            }
            let Ok(network) = Group::new(source.network()) else {
                continue;
            };
            self.said_at.insert(*source, now);
            drafts.push(Draft {
                from: cell(*source, machine),
                groups: vec![network],
                intent: Some(Intent::Fyi),
                text: format!(
                    "{}: {asked} asked of you, up from {was}; {} today",
                    source.name(),
                    record.counts.today
                ),
                re: None,
                realm: None,
            });
        }
        drafts
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use crate::receptor::{Counts, State};

    fn at(text: &str) -> Timestamp {
        text.parse().expect("a time")
    }

    fn declarers() -> Vec<u32> {
        vec![0, 65534, rustix::process::getuid().as_raw()]
    }

    fn github(addressed: u32) -> BTreeMap<Source, Record> {
        BTreeMap::from([(
            Source::Github,
            Record {
                source: Source::Github,
                realm: "work".to_owned(),
                state: State::Reading,
                since: at("2026-11-16T14:00:00Z"),
                read_at: at("2026-11-16T14:00:00Z"),
                every: 300,
                pid: 4242,
                started: Some(700),
                counts: Counts {
                    hour: 1,
                    today: 3,
                    addressed,
                },
            },
        )])
    }

    fn loud() -> Table {
        Table {
            say: BTreeSet::from([Source::Github]),
        }
    }

    #[test]
    fn a_climbing_count_is_said_as_an_fyi_of_counts_on_its_network() {
        let mut signals = Signals::default();
        let now = at("2026-11-16T14:05:00Z");
        assert!(
            signals
                .after(&github(1), &loud(), "kestrel", now)
                .is_empty(),
            "a receptor first seen says nothing"
        );
        let said = signals.after(&github(2), &loud(), "kestrel", now);
        assert_eq!(said.len(), 1, "a climb is said once: {said:?}");
        let draft = &said[0];
        assert_eq!(draft.from, "github@kestrel", "as the receptor's cell");
        assert_eq!(
            draft.groups,
            vec![Group::new("outside.github").expect("a group")],
            "on its source's network alone"
        );
        assert_eq!(draft.intent, Some(Intent::Fyi), "an fyi, asking nothing");
        assert_eq!(draft.re, None, "answering nothing");
        assert_eq!(draft.realm, None, "crossing no realm");
        assert_eq!(
            draft.text, "github: 2 asked of you, up from 1; 3 today",
            "the source's name and counts, nothing read outside"
        );
    }

    #[test]
    fn a_count_that_holds_or_falls_says_nothing() {
        let mut signals = Signals::default();
        let now = at("2026-11-16T14:05:00Z");
        signals.after(&github(2), &loud(), "kestrel", now);
        assert!(
            signals
                .after(&github(2), &loud(), "kestrel", now)
                .is_empty(),
            "a count that holds says nothing"
        );
        assert!(
            signals
                .after(&github(1), &loud(), "kestrel", now)
                .is_empty(),
            "a count that falls says nothing"
        );
        assert_eq!(
            signals.after(&github(2), &loud(), "kestrel", now).len(),
            1,
            "and climbing again from there is said"
        );
    }

    #[test]
    fn a_source_the_table_leaves_out_is_silent() {
        let mut signals = Signals::default();
        let now = at("2026-11-16T14:05:00Z");
        signals.after(&github(1), &Table::silent(), "kestrel", now);
        assert!(
            signals
                .after(&github(5), &Table::silent(), "kestrel", now)
                .is_empty(),
            "a silent table says nothing"
        );
    }

    #[test]
    fn a_source_says_at_most_once_in_ten_minutes() {
        let mut signals = Signals::default();
        let first = at("2026-11-16T14:05:00Z");
        signals.after(&github(1), &loud(), "kestrel", first);
        assert_eq!(
            signals.after(&github(2), &loud(), "kestrel", first).len(),
            1
        );
        let soon = at("2026-11-16T14:14:59Z");
        assert!(
            signals
                .after(&github(3), &loud(), "kestrel", soon)
                .is_empty(),
            "within ten minutes, a climb waits"
        );
        let later = at("2026-11-16T14:15:00Z");
        let said = signals.after(&github(3), &loud(), "kestrel", later);
        assert_eq!(said.len(), 1, "once ten minutes pass, it is said: {said:?}");
        assert_eq!(
            said[0].text, "github: 3 asked of you, up from 2; 3 today",
            "from what was last said"
        );
    }

    #[test]
    fn only_a_sealed_table_is_taken_and_an_unnamed_one_is_silent() {
        assert_eq!(
            Table::read(None, &declarers()),
            (Table::silent(), None),
            "no table named is silent, quietly"
        );
        let folder = tempfile::tempdir().expect("a folder");
        let file = folder.path().join("stimuli.json");
        fs::write(&file, r#"{"say": ["github"]}"#).expect("written");
        assert_eq!(
            Table::read(Some(&file), &declarers()).0,
            loud(),
            "a sealed table is taken"
        );
        let (refused, why) = Table::read(Some(&file), &[0, 65534]);
        assert_eq!(refused, Table::silent(), "one another account owns isn't");
        assert!(
            why.is_some_and(|why| why.contains("isn't trusted")),
            "and why is said"
        );
        fs::set_permissions(&file, fs::Permissions::from_mode(0o666)).expect("opened");
        assert_eq!(
            Table::read(Some(&file), &declarers()).0,
            Table::silent(),
            "one any account may write isn't taken"
        );
    }

    #[test]
    fn a_table_naming_what_it_doesnt_know_is_silent() {
        let folder = tempfile::tempdir().expect("a folder");
        let file = folder.path().join("stimuli.json");
        for text in [
            r#"{"say": ["github"], "loudness": 11}"#,
            r#"{"say": ["fax"]}"#,
            "not json",
        ] {
            fs::write(&file, text).expect("written");
            assert_eq!(
                Table::read(Some(&file), &declarers()).0,
                Table::silent(),
                "{text} is silent"
            );
        }
    }

    #[test]
    fn a_name_that_could_be_read_as_a_receptors_is_one() {
        for name in [
            "GitHub@kestrel",
            "git-hub@kestrel",
            " slack @kestrel",
            "github\u{200b}@kestrel",
            "g\u{0456}thub@kestrel",
            "github@kestrel.",
        ] {
            assert!(
                speaks_as_receptor(name),
                "{name:?} reads as a receptor's cell but wasn't taken as one"
            );
        }
        for name in ["mycelium@kestrel", "WCJQSG", "joel"] {
            assert!(
                !speaks_as_receptor(name),
                "{name:?} was taken as a receptor's cell"
            );
        }
    }

    #[test]
    fn a_receptors_name_is_known_whatever_its_machine() {
        assert!(speaks_as_receptor("github@kestrel"), "a receptor of here");
        assert!(speaks_as_receptor("slack@skiff"), "a receptor of there");
        assert!(!speaks_as_receptor("mycelium@kestrel"), "the node's own");
        assert!(!speaks_as_receptor("joel"), "a person");
        assert!(!speaks_as_receptor("4S6QRI"), "a session");
    }
}
