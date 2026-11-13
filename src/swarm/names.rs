//! Each team's short name, one or two words a person types to reach it,
//! worked out from the teams' titles alone, so it stays put as sessions
//! come and go. Joel, trying the swarm first: "how do i know what the
//! actual team names are? do I have to be exact?"
//!
//! A title's own short name comes first: the words before its colon, when
//! there are three at most, that no other team's title holds, a version
//! left out ("Deploy v3: Reimagine How Aquifer and Worldtree Ship" is
//! `deploy`, "Aquifer Part Deux: ..." is `part-deux`, since the other
//! holds Aquifer). Else a word written in lower case, as a name a person
//! writes that way is ("Bootstrap optimus to replace the work MacBook" is
//! `optimus`). Else its leading words up to its first small one, two at
//! most ("Quick Wins That Move Delta Toward tobi's Vision" is
//! `quick-wins`). A name two teams would share, or that's one of the
//! swarm's acts, goes to neither.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::{address, title};

/// The small words that name nothing, which a short name never holds.
const SMALL: [&str; 38] = [
    "a", "about", "an", "and", "as", "at", "be", "by", "for", "from", "how", "in", "into", "is",
    "it", "its", "of", "on", "or", "so", "than", "that", "the", "their", "this", "to", "toward",
    "towards", "up", "via", "vs", "was", "what", "when", "where", "which", "why", "with",
];

/// The swarm's acts, which a team named the same would hide.
const ACTS: [&str; 3] = ["say", "pause", "resume"];

/// The most words before a colon that still read as a name.
const NAME_WORDS: usize = 3;

/// A word of a title, as it's typed, and whether it was written in lower
/// case.
struct Word {
    typed: String,
    lower: bool,
}

/// The words of `text`, each trimmed of what isn't a letter, a digit or an
/// inner hyphen, a possessive left out, since nobody types one to name
/// something.
fn words(text: &str) -> Vec<Word> {
    text.split_whitespace()
        .filter(|word| !word.contains(['\'', '\u{2019}']))
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|word| !word.is_empty())
        .map(|word| Word {
            typed: word.to_lowercase(),
            lower: word.chars().next().is_some_and(char::is_lowercase),
        })
        .collect()
}

fn small(word: &Word) -> bool {
    SMALL.contains(&word.typed.as_str())
}

fn versioned(word: &Word) -> bool {
    word.typed.chars().any(|c| c.is_ascii_digit())
}

/// The short name `title` goes by, where `shared` says whether a word is
/// in another team's title too.
fn short(title: &str, shared: &dyn Fn(&str) -> bool) -> Option<String> {
    let own = |word: &&Word| !small(word) && !shared(&word.typed);
    if let Some((name, _)) = title.split_once(':') {
        let named = words(name);
        let kept: Vec<&str> = named
            .iter()
            .filter(own)
            .filter(|word| !versioned(word))
            .map(|word| word.typed.as_str())
            .collect();
        if named.len() <= NAME_WORDS && !kept.is_empty() {
            return Some(kept.join("-"));
        }
    }
    let all = words(title);
    if let Some(word) = all
        .iter()
        .filter(own)
        .find(|word| word.lower && word.typed.chars().count() >= 3)
    {
        return Some(word.typed.clone());
    }
    let leading: Vec<&str> = all
        .iter()
        .take_while(|word| !small(word))
        .filter(own)
        .filter(|word| !versioned(word))
        .take(2)
        .map(|word| word.typed.as_str())
        .collect();
    (!leading.is_empty()).then(|| leading.join("-"))
}

/// Each team's short name, by its address, for those that have one that's
/// theirs alone.
#[must_use]
pub fn short_names(teams: &[Value]) -> BTreeMap<String, String> {
    let held: Vec<(&str, BTreeSet<String>)> = teams
        .iter()
        .map(|team| {
            let typed = words(title(team)).into_iter().map(|word| word.typed);
            (address(team), typed.collect())
        })
        .collect();
    let named: Vec<(&str, String)> = teams
        .iter()
        .filter_map(|team| {
            let me = address(team);
            let shared = |word: &str| {
                held.iter()
                    .any(|(other, typed)| *other != me && typed.contains(word))
            };
            Some((me, short(title(team), &shared)?))
        })
        .filter(|(_, name)| !ACTS.contains(&name.as_str()))
        .collect();
    named
        .iter()
        .filter(|(_, name)| named.iter().filter(|(_, other)| other == name).count() == 1)
        .map(|(team, name)| ((*team).to_owned(), name.clone()))
        .collect()
}

/// What a person types to reach `team`: its short name, else its title.
pub fn typed<'a>(team: &'a Value, shorts: &'a BTreeMap<String, String>) -> &'a str {
    shorts
        .get(address(team))
        .map_or_else(|| title(team), String::as_str)
}

/// The short name worth showing beside `team`'s title: none where it only
/// says the title again, as `reef` says Reef.
pub fn shown<'a>(team: &Value, shorts: &'a BTreeMap<String, String>) -> Option<&'a str> {
    let short = shorts.get(address(team))?;
    let spelt: Vec<String> = words(title(team))
        .into_iter()
        .map(|word| word.typed)
        .collect();
    (spelt.join("-") != *short).then_some(short.as_str())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::short_names;

    #[test]
    fn each_team_goes_by_a_short_name_of_its_own() {
        let titles = [
            ("ACCELR", "Acceluary: The Acceleration Issue Tracker"),
            (
                "WEIGHT",
                "Find the Right Weight for Agent Sessions: pi or pi-durable",
            ),
            ("DELTAQ", "Quick Wins That Move Delta Toward tobi's Vision"),
            ("OPTIMS", "Bootstrap optimus to replace the work MacBook"),
            (
                "DEPLOY",
                "Deploy v3: Reimagine How Aquifer and Worldtree Ship",
            ),
            (
                "AQUIFR",
                "Aquifer Part Deux: Nix Closures on MicroVMs on Kubernetes on NixOS",
            ),
            ("REEF01", "Reef"),
        ];
        let teams: Vec<_> = titles
            .iter()
            .map(|(address, title)| json!({"key": format!("team:{address}"), "title": title}))
            .collect();
        let named = short_names(&teams);
        let said: Vec<(&str, &str)> = named
            .iter()
            .map(|(team, name)| (team.as_str(), name.as_str()))
            .collect();
        assert_eq!(
            said,
            [
                ("ACCELR", "acceluary"),
                ("AQUIFR", "part-deux"),
                ("DELTAQ", "quick-wins"),
                ("DEPLOY", "deploy"),
                ("OPTIMS", "optimus"),
                ("REEF01", "reef"),
                ("WEIGHT", "pi-durable"),
            ],
            "a name before a colon, else one written in lower case, else the leading words"
        );
    }

    #[test]
    fn a_name_two_teams_would_share_or_an_act_goes_to_neither() {
        let teams = [
            json!({"key": "team:ONE", "title": "Kelp Beds: the first"}),
            json!({"key": "team:TWO", "title": "Bring kelp-beds home"}),
            json!({"key": "team:SAY", "title": "Say"}),
        ];
        assert!(
            short_names(&teams).is_empty(),
            "kelp-beds twice, and an act: {:?}",
            short_names(&teams)
        );
    }
}
