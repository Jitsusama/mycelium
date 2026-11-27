//! Each team's short name, one or two words a person types to reach it.
//! Joel, trying the swarm first: "how do i know what the actual team names
//! are? do I have to be exact?"
//!
//! A team goes by the name its quest gives it in the swarm, where it gives
//! one, since that is what its person calls it. Else its name is worked
//! out from the teams' titles alone, so it stays put as sessions come and
//! go. A title's own short name comes first: the words before its colon,
//! when there are three at most, that no other team's title holds, a
//! version left out ("Deploy v3: How Kelp and Beacon Ship" is `deploy`,
//! "Kelp Part Deux: ..." is `part-deux`, since the other holds Kelp). Else
//! a word written in lower case, as a name a person writes that way is
//! ("Bootstrap skiff to replace the old dinghy" is `skiff`). Else its
//! leading words up to its first small one, two at most ("Quick Wins That
//! Move Harbour Toward Lighthouse's Vision" is `quick-wins`). A name two
//! teams would share, or that reads as one of the swarm's acts or as
//! `team` once cleaned as a page draws it, goes to neither, and one
//! worked out goes to none where a quest gives it to its own.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::{address, title, unmarked};
use crate::forage::BRAILLE_BLANK;

/// The small words that name nothing, which a short name never holds.
const SMALL: [&str; 38] = [
    "a", "about", "an", "and", "as", "at", "be", "by", "for", "from", "how", "in", "into", "is",
    "it", "its", "of", "on", "or", "so", "than", "that", "the", "their", "this", "to", "toward",
    "towards", "up", "via", "vs", "was", "what", "when", "where", "which", "why", "with",
];

/// The swarm's acts, which a team named the same would hide.
const ACTS: [&str; 3] = ["say", "pause", "resume"];

/// Whether `name`, copied into a shell, reads as one word and nothing
/// more: letters, digits, `-`, `_`, `.` and any of `also`, which no
/// shell splits or acts on, led by a letter or digit, so no option is
/// read from it.
pub(crate) fn shell_word(name: &str, also: &[char]) -> bool {
    name.chars().next().is_some_and(char::is_alphanumeric)
        && name.chars().all(|each| {
            each.is_alphanumeric() || matches!(each, '-' | '_' | '.') || also.contains(&each)
        })
}

/// Whether `name`, copied after `swarm` into a shell, reads as one name
/// and nothing more: a [`shell_word`], and neither an act, which `swarm`
/// would do, nor `team`, which it reads as the person saying what
/// follows.
pub(super) fn one_word(name: &str) -> bool {
    let lower = name.to_lowercase();
    shell_word(name, &[]) && !ACTS.contains(&lower.as_str()) && lower != "team"
}

/// Whether `name`, cleaned as a page draws it, holds a word `swarm` reads
/// as other than a team's name: an act, `team` or a flag. Typed as a page
/// drew it, `reef pause` would pause Reef, and `pause` reach no team.
/// A short's words may be typed apart, so each part between hyphens
/// counts as a word too: `reef-pause`, typed as `reef pause`, would pause
/// Reef.
fn acts_for(name: &str) -> bool {
    let drawn = unmarked(name).to_lowercase();
    let flagged = drawn.split(blank).any(|word| word.starts_with('-'));
    flagged
        || drawn
            .split(|each: char| blank(each) || each == '-')
            .any(|word| ACTS.contains(&word) || word == "team")
}

/// Whether `each` is drawn as a space between words.
fn blank(each: char) -> bool {
    each.is_whitespace() || each == BRAILLE_BLANK
}

/// `name` as a page draws it and a person types it: cleaned, so each
/// control is the space it's drawn as, in lower case, and with each run
/// of what's drawn as a space one hyphen, as a typed name's words are
/// joined. So what a page draws is what reaches it: `tide plan` drawn as
/// said would be typed as `tide-plan`, another team's, and a braille
/// blank or a control drawn like a space would let `reef pause` through
/// as one word.
fn typed_form(name: &str) -> String {
    unmarked(name)
        .to_lowercase()
        .split(blank)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

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

/// The name `team`'s quest gives it in the swarm, in its [`typed_form`].
fn given(team: &Value) -> Option<String> {
    let name = typed_form(team.get("swarm")?.as_str()?);
    (!name.is_empty()).then_some(name)
}

/// Each team's short name, by its address, for those that have one that's
/// theirs alone.
#[must_use]
pub fn short_names(teams: &[Value]) -> BTreeMap<String, String> {
    let chosen: Vec<String> = teams.iter().filter_map(given).collect();
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
            if let Some(name) = given(team) {
                return Some((me, name));
            }
            // A worked-out name is held in its typed form too, as a
            // title's word may hold a braille blank or a control.
            Some((me, typed_form(&short(title(team), &shared)?)))
                .filter(|(_, name)| !chosen.contains(name))
        })
        // One that cleans to nothing would be drawn blank, and offered to
        // a shell as an empty word.
        .filter(|(_, name)| !acts_for(name) && !name.is_empty())
        .collect();
    named
        .iter()
        // Two that clean alike are drawn alike, and typed reach both.
        .filter(|(_, name)| {
            let drawn = unmarked(name);
            named
                .iter()
                .filter(|(_, other)| unmarked(other) == drawn)
                .count()
                == 1
        })
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
    (!says_again(title(team), short)).then_some(short.as_str())
}

/// Whether `short` only says `title` again, as `reef` says Reef, the
/// title's words in the [`typed_form`] a short is held in.
pub(crate) fn says_again(title: &str, short: &str) -> bool {
    let spelt: Vec<String> = words(title).into_iter().map(|word| word.typed).collect();
    typed_form(&spelt.join("-")) == short
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::short_names;

    #[test]
    fn each_team_goes_by_a_short_name_of_its_own() {
        let titles = [
            ("ACCELR", "Tidemark: The Acceleration Issue Tracker"),
            (
                "WEIGHT",
                "Find the Right Weight for Agent Sessions: io or io-durable",
            ),
            (
                "QUICKW",
                "Quick Wins That Move Harbour Toward Lighthouse's Vision",
            ),
            ("SKIFF1", "Bootstrap skiff to replace the old dinghy"),
            ("DEPLOY", "Deploy v3: Reimagine How Kelp and Beacon Ship"),
            (
                "KELP02",
                "Kelp Part Deux: Nix Closures on MicroVMs on Rafts",
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
                ("ACCELR", "tidemark"),
                ("DEPLOY", "deploy"),
                ("KELP02", "part-deux"),
                ("QUICKW", "quick-wins"),
                ("REEF01", "reef"),
                ("SKIFF1", "skiff"),
                ("WEIGHT", "io-durable"),
            ],
            "a name before a colon, else one written in lower case, else the leading words"
        );
    }

    #[test]
    fn a_name_a_quest_gives_its_team_wins_over_one_worked_out() {
        let teams = [
            json!({
                "key": "team:QUICKW",
                "title": "Quick Wins That Move the Reef",
                "swarm": "Harbour",
            }),
            json!({"key": "team:HARBOR", "title": "Harbour"}),
            json!({"key": "team:REEF01", "title": "Reef", "swarm": "say"}),
        ];
        let named = short_names(&teams);
        let said: Vec<(&str, &str)> = named
            .iter()
            .map(|(team, name)| (team.as_str(), name.as_str()))
            .collect();
        assert_eq!(
            said,
            [("QUICKW", "harbour")],
            "the quest's own name, typed in lower case, over the one the other's title \
             would give; and an act given as a name goes to none"
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
