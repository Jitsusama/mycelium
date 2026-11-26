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
use unicode_normalization::UnicodeNormalization;
use unicode_security::mixed_script::AugmentedScriptSet;
use unicode_security::{
    GeneralSecurityProfile, RestrictionLevel, RestrictionLevelDetection, skeleton,
};

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
/// Reef. What draws on the letter before it is passed over, so a mark on
/// `pause` hides no act; and a name led by a hyphen may be typed as a
/// flag, as one a quest gives with a word led by hyphens is, as
/// [`flags`] says.
///
/// Each word is judged as Unicode's confusables draw it, by its skeleton,
/// so one spelt in letters drawn like an act's, as Cyrillic's dze, a and
/// u (U+0455, U+0430, U+0443) draw `say`, hides it as surely.
fn acts_for(name: &str) -> bool {
    let drawn: String = skeleton(&unmarked(name).to_lowercase())
        .filter(|each| each.is_alphanumeric() || !on_what_is_before(*each))
        .collect();
    let flagged = drawn.split(blank).any(|word| word.starts_with('-'));
    flagged
        || drawn
            .split(|each: char| blank(each) || each == '-')
            .any(|word| {
                ACTS.iter()
                    .chain(&["team"])
                    .any(|act| skeleton(act).eq(word.chars()))
            })
}

/// Whether a quest's own words for its team, `given`, hold one led by a
/// hyphen and then more than a blank: `kelp --names`, held as
/// `kelp---names`, typed as the quest wrote it would hand `swarm` the
/// flag `--names`, and a lone `--` ends its flags. A lookalike dash, as a
/// minus sign is, counts as a hyphen, since a person retyping the words
/// by eye types the keyboard's, though one pasting them hands no parser
/// a flag. A dash between words, as an en dash spaced between `Kelp` and
/// `Beds` is, flags nothing, and is kept.
fn flags(given: &str) -> bool {
    hyphens(&unmarked(given))
        .split(blank)
        .any(|word| word.starts_with('-') && word.chars().count() > 1)
}

/// Whether a person types `name`, in the composed form a keyboard types
/// it in, into a shell as it's drawn, and `swarm` reads it as drawn, as
/// Unicode's rules for identifiers (UTS #39) judge one: in ASCII a
/// letter, a digit, `-`, `_` or `.`, which no shell splits or acts on;
/// past ASCII, a letter, digit or mark its General Security Profile
/// allows, so no fullwidth, mathematical, circled or superscript form of
/// ASCII, no Roman numeral and no script kept only for scholars; a mark
/// only on a letter past ASCII, as a word's accents, viramas and tone
/// marks are, [`MOST_MARKS`] at most in a row; a joiner only after a
/// letter or mark; and its scripts no more mixed than Highly Restrictive
/// allows, one script, or ASCII beside Han, kana or Hangul, so a Latin
/// name with a Cyrillic or Greek letter drawn like its own, or a name
/// mixing two such scripts, isn't kept.
///
/// A hyphen bullet, as U+2043 is, a heavy minus or a box line, drawn as a
/// hyphen by many fonts, would join `reef` to `pause` unseen, and two
/// teams drawn as `tide-plan` would go by two names; so would a letter
/// drawn as a long dash, as the katakana prolonged mark and Han's one
/// are, kept only between Han, kana or Hangul. A shell reads `;`, `$`,
/// quotes and the like as other than the name. An emoji or a mark
/// standing alone can't be typed at all, nor a mark a keyboard has no
/// composed letter for on an ASCII one.
fn typable(name: &str) -> bool {
    let each: Vec<char> = name.chars().collect();
    let mut marks = 0;
    let kept = (0..each.len()).all(|at| {
        let (now, before) = (each[at], at.checked_sub(1).map(|before| each[before]));
        let on_a_letter = before.is_some_and(|before| before.is_alphabetic() || marks > 0);
        let kept = if now.is_ascii() {
            now.is_ascii_alphanumeric() || matches!(now, '-' | '_' | '.')
        } else if joiner(now) {
            on_a_letter
        } else if now.is_alphanumeric() {
            now.identifier_allowed()
                && (!long_dash(now)
                    || (before.is_some_and(cjk)
                        && each.get(at + 1).is_none_or(|after| cjk(*after))))
        } else {
            now.identifier_allowed()
                && on_what_is_before(now)
                && on_a_letter
                && marks < MOST_MARKS
                && !before.is_some_and(|before| before.is_ascii())
        };
        marks = if !now.is_alphanumeric() && on_what_is_before(now) && !joiner(now) {
            marks + 1
        } else {
            0
        };
        kept
    });
    let unjoined: String = each.iter().filter(|now| !joiner(**now)).collect();
    kept && matches!(
        unjoined.as_str().detect_restriction_level(),
        RestrictionLevel::ASCIIOnly
            | RestrictionLevel::SingleScript
            | RestrictionLevel::HighlyRestrictive
    )
}

/// The most marks a short keeps in a row on one letter, as a syllable
/// stacks a vowel sign, a tone and one more at most, so a pile of them
/// can't draw over the letters around it.
const MOST_MARKS: usize = 3;

/// Whether `each` is a joiner or non-joiner, which Persian and Indic
/// words are spelt with, and which Unicode's profile allows only in
/// context, as after a letter.
const fn joiner(each: char) -> bool {
    matches!(each, '\u{200C}' | '\u{200D}')
}

/// Whether `each` is drawn as a long dash, as Unicode's confusables draw
/// the katakana prolonged mark, Han's one and Hangul's eu alike.
fn long_dash(each: char) -> bool {
    let drawn: String = skeleton(each.encode_utf8(&mut [0; 4])).collect();
    drawn == "\u{30FC}"
}

/// Whether `each` is of Han, kana or Hangul, among which a long dash is
/// a letter of the word; a character of every script, as a digit is,
/// isn't.
fn cjk(each: char) -> bool {
    let scripts = AugmentedScriptSet::for_char(each);
    (scripts.hanb || scripts.jpan || scripts.kore) && !scripts.is_all()
}

/// Whether `each` draws on what's before it, taking no cell of its own:
/// a mark, as an accent or a virama is, or a joiner.
fn on_what_is_before(each: char) -> bool {
    unicode_width::UnicodeWidthChar::width(each) == Some(0)
}

/// Whether `each` is drawn as a space between words.
fn blank(each: char) -> bool {
    each.is_whitespace() || each == BRAILLE_BLANK
}

/// `text` as it's drawn, for what a person asks for to meet a name or a
/// title by: cleaned, composed as a keyboard types an accented letter
/// (NFC), in lower case, and with each dash the keyboard's hyphen where
/// `folded`, as a page draws them alike. One form for both sides and for
/// every lookup, so a name asked as drawn finds what it's drawn from.
pub(crate) fn drawn_form(text: &str, folded: bool) -> String {
    let lower: String = unmarked(text).to_lowercase().nfc().collect();
    if folded { hyphens(&lower) } else { lower }
}

/// `text` with each dash the keyboard's hyphen, as a short is held, so
/// what a person asks for meets it however its dashes are spelt.
pub(crate) fn hyphens(text: &str) -> String {
    text.replace(dash, "-")
}

/// Whether `each` is a dash, as Unicode 16's Dash property says: a
/// hyphen, an en or em dash, a minus sign, and their small, fullwidth and
/// other scripts' forms, each read as a keyboard's hyphen. Taken from
/// PropList-16.0.0.txt, so a later Unicode's dashes are drawn as written.
fn dash(each: char) -> bool {
    matches!(
        each,
        '\u{2D}'
            | '\u{58A}'
            | '\u{5BE}'
            | '\u{1400}'
            | '\u{1806}'
            | '\u{2010}'..='\u{2015}'
            | '\u{2053}'
            | '\u{207B}'
            | '\u{208B}'
            | '\u{2212}'
            | '\u{2E17}'
            | '\u{2E1A}'
            | '\u{2E3A}'..='\u{2E3B}'
            | '\u{2E40}'
            | '\u{2E5D}'
            | '\u{301C}'
            | '\u{3030}'
            | '\u{30A0}'
            | '\u{FE31}'..='\u{FE32}'
            | '\u{FE58}'
            | '\u{FE63}'
            | '\u{FF0D}'
            | '\u{10D6E}'
            | '\u{10EAD}'
    )
}

/// `name` as a page draws it and a person types it: cleaned, so each
/// control is the space it's drawn as, in lower case, composed as a
/// keyboard types an accented letter (NFC), with each dash the
/// keyboard's hyphen, and with each run of what's drawn as a space one
/// hyphen, as a typed name's words are joined. So what a page draws is
/// what reaches it: `tide plan` drawn as said would be typed as
/// `tide-plan`, another team's; a braille blank or a control drawn like a
/// space would let `reef pause` through as one word; and a `reef-pause`
/// whose hyphen is a lookalike, as U+2011 is, would hide its act from the
/// check on each part, and miss the keyboard's `reef-pause` it seems to
/// be.
fn typed_form(name: &str) -> String {
    let lower: String = unmarked(name).to_lowercase().nfc().collect();
    hyphens(&lower)
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
            // In typed form, so two titles whose words differ only by
            // how a dash is spelt share that word.
            let typed = words(title(team))
                .into_iter()
                .map(|word| typed_form(&word.typed));
            (address(team), typed.collect())
        })
        .collect();
    let named: Vec<(&str, String)> = teams
        .iter()
        .filter_map(|team| {
            let me = address(team);
            let shared = |word: &str| {
                let word = typed_form(word);
                held.iter()
                    .any(|(other, typed)| *other != me && typed.contains(&word))
            };
            if let Some(name) = given(team) {
                let spelt = team
                    .get("swarm")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                return (!flags(spelt)).then_some((me, name));
            }
            // A worked-out name is held in its typed form too, as a
            // title's word may hold a braille blank or a control.
            Some((me, typed_form(&short(title(team), &shared)?)))
                .filter(|(_, name)| !chosen.contains(name))
        })
        // One that cleans to nothing would be drawn blank, and offered to
        // a shell as an empty word.
        .filter(|(_, name)| !acts_for(name) && typable(name) && !name.is_empty())
        .collect();
    named
        .iter()
        // Two that clean alike, or whose letters Unicode's confusables
        // draw alike, as a dotless i is drawn as an i, are drawn alike,
        // and typed reach both.
        .filter(|(_, name)| {
            let drawn: String = skeleton(&unmarked(name)).collect();
            named
                .iter()
                .filter(|(_, other)| skeleton(&unmarked(other)).eq(drawn.chars()))
                .count()
                == 1
        })
        .map(|(team, name)| ((*team).to_owned(), name.clone()))
        .collect()
}

/// The team whose quest gives it the name `asked` is typed as, where that
/// name was held back, so one asking for it is told why it reaches
/// nothing, rather than nothing at all, or a session whose title holds it.
pub(crate) fn held_back<'a>(
    teams: &'a [Value],
    shorts: &BTreeMap<String, String>,
    asked: &str,
) -> Option<&'a Value> {
    let asked = typed_form(asked);
    teams.iter().find(|team| {
        given(team).is_some_and(|name| name == asked) && !shorts.contains_key(address(team))
    })
}

/// Why nothing answers `asked`, naming `team` where its quest gives it
/// that name and it's held back.
pub(crate) fn unnamed(asked: &str, team: Option<&Value>, nothing: &str) -> String {
    team.map_or_else(
        || format!("{nothing} {asked}"),
        |team| {
            format!(
                "{nothing} {asked}: {}'s quest gives it that name, held back as, typed or \
                 drawn, it reads as more than one name or as other than a name; ask for it by \
                 its title",
                unmarked(title(team))
            )
        },
    )
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
/// title's words in the [`typed_form`] a swarm short is held in, and
/// the short in its [`drawn_form`], folded as that form is, as a
/// network's short isn't held in it, so `Kelp-Beds` says Kelp Beds
/// again whatever its case or its dashes.
pub(crate) fn says_again(title: &str, short: &str) -> bool {
    let spelt: Vec<String> = words(title).into_iter().map(|word| word.typed).collect();
    typed_form(&spelt.join("-")) == drawn_form(short, true)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{says_again, short_names, typed_form};

    #[test]
    fn every_dash_is_typed_as_the_keyboard_s_hyphen_and_nothing_beside_one() {
        // Unicode 16's Dash property, as PropList-16.0.0.txt lists it, its
        // ranges written out, and the character either side of each entry.
        let dashes = [
            0x2D, 0x58A, 0x5BE, 0x1400, 0x1806, 0x2010, 0x2011, 0x2012, 0x2013, 0x2014, 0x2015,
            0x2053, 0x207B, 0x208B, 0x2212, 0x2E17, 0x2E1A, 0x2E3A, 0x2E3B, 0x2E40, 0x2E5D, 0x301C,
            0x3030, 0x30A0, 0xFE31, 0xFE32, 0xFE58, 0xFE63, 0xFF0D, 0x10D6E, 0x10EAD,
        ];
        let beside = [
            0x2C, 0x2E, 0x589, 0x58B, 0x5BD, 0x5BF, 0x13FF, 0x1401, 0x1805, 0x1807, 0x200F_u32,
            0x2016, 0x2052, 0x2054, 0x207A, 0x207C, 0x208A, 0x208C, 0x2211, 0x2213, 0x2E16, 0x2E18,
            0x2E19, 0x2E1B, 0x2E39, 0x2E3C, 0x2E3F, 0x2E41, 0x2E5C, 0x2E5E, 0x301B, 0x301D, 0x302F,
            0x3031, 0x309F, 0x30A1, 0xFE30, 0xFE33, 0xFE57, 0xFE59, 0xFE62, 0xFE64, 0xFF0C, 0xFF0E,
            0x10D6D, 0x10D6F, 0x10EAC, 0x10EAE,
        ];
        let typed = |code: u32| {
            let each = char::from_u32(code).expect("a character");
            typed_form(&format!("a{each}b"))
        };
        for code in dashes {
            assert_eq!(typed(code), "a-b", "U+{code:04X} is a dash");
        }
        // Each is kept as it is, but the direction mark, which a cleaning
        // drops, so none is folded to a hyphen or parts the words either.
        for code in beside {
            let each = char::from_u32(code).expect("a character");
            let kept = if code == 0x200F {
                "ab".to_owned()
            } else {
                format!("a{each}b").to_lowercase()
            };
            assert_eq!(typed(code), kept, "U+{code:04X} is no dash");
        }
    }

    #[test]
    fn a_short_saying_its_title_again_is_judged_however_its_dashes_are_spelt() {
        // A network's short isn't held in typed form, so both sides fold.
        assert!(says_again("Kelp\u{2011}Beds", "kelp\u{2011}beds"));
        assert!(says_again("Kelp\u{2011}Beds", "kelp-beds"));
        assert!(says_again("Kelp\u{2011}Beds", "Kelp-Beds"));
        assert!(!says_again("Kelp\u{2011}Beds", "kelp"));
    }

    #[test]
    fn two_titles_spelling_one_word_s_dash_apart_share_it() {
        let teams = [
            json!({"key": "team:ONE", "title": "Kelp\u{2011}Beds: first"}),
            json!({"key": "team:TWO", "title": "Kelp-Beds: second"}),
        ];
        let named = short_names(&teams);
        let said: Vec<(&str, &str)> = named
            .iter()
            .map(|(team, name)| (team.as_str(), name.as_str()))
            .collect();
        assert_eq!(
            said,
            [("ONE", "first"), ("TWO", "second")],
            "kelp-beds is in both titles, so neither takes it"
        );
    }

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
