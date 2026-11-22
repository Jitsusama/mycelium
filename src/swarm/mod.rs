//! The swarm as a person reads it in a terminal: every team, worst first
//! and nested as they are, then one team's own sessions when asked, from
//! the node's `team` and `session` records.
//!
//! Each team is a line saying how many of its sessions need their person,
//! are stuck, move or wait, under one header, so the eye runs down a
//! column rather than reading words on every line. A team's name, never
//! its address, says which it is, its short name first where it has one
//! (names.rs), its title after it, dimmed, and a count of none is left
//! blank.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr as _;

use crate::forage::BRAILLE_BLANK;

mod asks;
mod names;
mod shapes;
pub mod steers;

#[cfg(test)]
pub(crate) use asks::{ASKS_FIT, together};
pub(crate) use asks::{asked, asking};
pub use asks::{read, records};
pub use names::short_names;
pub(crate) use names::{says_again, shell_word};
pub use shapes::Shapes;

/// The swarm as the node holds it.
#[derive(Debug, Default)]
pub struct Swarm {
    /// The realm it is, such as `work`.
    pub realm: String,
    /// Its `team` records.
    pub teams: Vec<Value>,
    /// Its `session` records.
    pub sessions: Vec<Value>,
}

/// How it is drawn: how many columns it may take, whether in colour, when
/// now is, for the times it says, and the marks it draws shapes by.
pub struct Look {
    /// The terminal's width, in columns.
    pub width: usize,
    /// Whether to colour it, as a terminal asked to.
    pub colour: bool,
    /// Now, in the time zone the times are said in.
    pub now: jiff::Zoned,
    /// The mark each orchestration's shape is drawn by.
    pub shapes: Shapes,
}

/// How a session stands, as a team counts it, worst first, and dormant
/// after them, as a cell is that parked to give back its memory, which a
/// team doesn't count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Standing {
    NeedsYou,
    Stuck,
    Moving,
    Quiet,
    Dormant,
}

impl Standing {
    /// Each a team counts, in the order the header says them.
    const EACH: [Self; 4] = [Self::NeedsYou, Self::Stuck, Self::Moving, Self::Quiet];

    /// Each a cell may stand as, in the order the header says them.
    pub(crate) const CELLS: [Self; 5] = [
        Self::NeedsYou,
        Self::Stuck,
        Self::Moving,
        Self::Quiet,
        Self::Dormant,
    ];

    /// The word a team record counts it by.
    pub(crate) const fn counted(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs-you",
            Self::Stuck => "stuck",
            Self::Moving => "moving",
            Self::Quiet => "quiet",
            Self::Dormant => "dormant",
        }
    }

    /// Its column's header, which is its column's width too.
    const fn header(self) -> &'static str {
        match self {
            Self::NeedsYou => "need you",
            Self::Stuck => "stuck",
            Self::Moving => "moving",
            Self::Quiet => "quiet",
            Self::Dormant => "dormant",
        }
    }

    /// The word a session's line says it with.
    pub(crate) const fn said(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs you",
            Self::Stuck => "stuck",
            Self::Moving => "moving",
            Self::Quiet => "quiet",
            Self::Dormant => "dormant",
        }
    }

    fn from_counted(word: &str) -> Option<Self> {
        Self::EACH.into_iter().find(|each| each.counted() == word)
    }

    /// How a session record stands as a cell: dormant where it parked,
    /// whether its process has ended yet or not, else as a team counts it,
    /// and none for one that has ended otherwise. Once the process of a
    /// session the node says runs `here` has ended, it's parked only as
    /// the node judges it, with something left to wake it; once nothing
    /// is, it has ended, whatever its presence still says. Of any other,
    /// its presence's word is all the node has, as it is of a record from
    /// a node that says no `here`.
    pub(crate) fn of_cell(session: &Value) -> Option<Self> {
        let parked = json!("parked");
        let judged = session["here"] == json!(true) && session["standing"] != json!("running");
        if session["standing"] == parked || !judged && session["presence"]["state"] == parked {
            return Some(Self::Dormant);
        }
        Self::of(session)
    }

    /// What a session standing so is doing, or asks, where it says.
    pub(crate) fn doing(self, session: &Value) -> Option<&str> {
        match self {
            Self::NeedsYou => session["wants"]["said"].as_str(),
            Self::Stuck => Some("paused"),
            Self::Moving | Self::Quiet => session["step"].as_str(),
            Self::Dormant => None,
        }
    }

    /// How a session record stands, or none for one that has ended.
    pub(crate) fn of(session: &Value) -> Option<Self> {
        let state = session["state"].as_str()?;
        if !matches!(state, "busy" | "idle") {
            return None;
        }
        if !session["wants"].is_null() {
            Some(Self::NeedsYou)
        } else if session["presence"]["paused"].as_bool() == Some(true) {
            Some(Self::Stuck)
        } else if state == "busy" {
            Some(Self::Moving)
        } else {
            Some(Self::Quiet)
        }
    }

    /// The colour a terminal says it in: needing you in yellow, stuck in
    /// red, quiet faint, so what to look at stands out before it's read.
    pub(crate) const fn colour(self) -> &'static str {
        match self {
            Self::NeedsYou => YELLOW,
            Self::Stuck => RED,
            Self::Moving => "",
            Self::Quiet | Self::Dormant => FAINT,
        }
    }
}

pub(crate) const BOLD: &str = "\x1b[1m";
pub(crate) const FAINT: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
pub(crate) const RESET: &str = "\x1b[0m";

/// The space between columns.
pub(crate) const GAP: &str = "  ";

/// How far each team in is set in from the one it is within.
const STEP: usize = 2;

/// The narrowest a name is cut to, however narrow the terminal.
pub(crate) const NARROWEST: usize = 16;

/// The width a pipe is drawn at: nothing is cut to it, and no column is
/// padded out to it, so a reader that greps or saves the text has all of
/// it, each table's columns as wide as what is in them; a row too long
/// to line up sets its counts on the next line, as [`aligned`] says.
pub const WHOLE: usize = usize::MAX;

/// Where a row's counts begin, as a table is drawn [`WHOLE`], for
/// [`aligned`] to line them up once every row is drawn: a Unicode
/// noncharacter, as no title has one once [`unmarked`].
pub(crate) const COUNTS_AT: &str = "\u{fdd0}";

/// Where a line's doing begins, as [`COUNTS_AT`] says its counts do.
pub(crate) const DOING_AT: &str = "\u{fdd1}";

/// Both marks, each a Unicode noncharacter, which no font draws and no
/// text from outside should hold, unlike the private-use characters a
/// theme's shape marks are drawn from.
const MARKS: [char; 2] = ['\u{fdd0}', '\u{fdd1}'];

/// `text` from a peer as a marked row draws it: on one line, as
/// [`one_line`](crate::forage::one_line) says, so a break in a title can't
/// split the block its counts line up in, with no noncharacter, so no
/// mark [`aligned`] reads and a title can't move a column, and with no
/// space or [`BRAILLE_BLANK`] of its own at either end, so a title's
/// leading spaces can't pass for a row's indent once it runs on and set
/// a figure under a header.
pub(crate) fn unmarked(text: &str) -> String {
    crate::forage::one_line(text)
        .chars()
        .filter(|each| !crate::forage::noncharacter(*each))
        .collect::<String>()
        .trim_matches(|each: char| each.is_whitespace() || each == BRAILLE_BLANK)
        .to_owned()
}

/// `text` with no mark left in it, for a page that lines nothing up.
pub(crate) fn without_marks(text: &str) -> String {
    text.replace(MARKS, "")
}

/// The columns a table lined up [`WHOLE`] keeps its counts within, as a
/// terminal of the narrowest common width shows them.
const KEPT_WITHIN: usize = 80;

/// The room kept after the widest lead for a doing, so what comes
/// before a doing ends by column 48 however long the titles, and the
/// doing begins after the gap that follows.
const DOING_ROOM: usize = 32;

/// `text` drawn [`WHOLE`] with its marks lined up: in each block of
/// lines between blank ones, what comes before each mark is padded to
/// the widest of the block's that leaves what follows room within
/// [`KEPT_WITHIN`] (a block's widest counts, or [`DOING_ROOM`] for a
/// doing), counts and doings each on their own. A line whose lead is
/// wider, a pasted paragraph for a title, runs on alone and sets what
/// follows on the next line under the column, so one name moves no
/// other row's counts and no count sits under another's header; with
/// nothing after its mark, it stays on its line. A doing's block where
/// no lead fits sets its doings under the furthest column allowed. A
/// line that runs on has each run of spaces after its indent drawn as
/// one, its own gaps too, so a title can't space a figure out under a
/// header. A mark left over is dropped. A colour's escape takes no
/// column, should a pipe ever be drawn in colour.
pub(crate) fn aligned(text: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let blocks: Vec<(usize, usize)> = {
        let mut blocks = Vec::new();
        let mut start = 0;
        for (at, line) in lines.iter().enumerate() {
            if line.is_empty() {
                blocks.push((start, at));
                start = at + 1;
            }
        }
        blocks.push((start, lines.len()));
        blocks
    };
    for mark in [DOING_AT, COUNTS_AT] {
        for &(start, end) in &blocks {
            let block = &mut lines[start..end];
            let split = |line: &str| {
                line.find(mark).map(|at| {
                    let after = &line[at + mark.len()..];
                    (at, drawn_width(&line[..at]), drawn_width(after))
                })
            };
            let room = if mark == COUNTS_AT {
                block
                    .iter()
                    .filter_map(|line| split(line).map(|(_, _, after)| after))
                    .max()
                    .unwrap_or_default()
            } else {
                DOING_ROOM
            };
            let within = KEPT_WITHIN.saturating_sub(room);
            // A counts block always holds its header, whose lead fits, so
            // only a doings block can find none.
            let widest = block
                .iter()
                .filter_map(|line| split(line).map(|(_, before, _)| before))
                .filter(|before| *before <= within)
                .max()
                .unwrap_or(within);
            for line in block.iter_mut() {
                if let Some((at, before, _)) = split(line) {
                    let after = &line[at + mark.len()..];
                    *line = if before > widest && after.trim().is_empty() {
                        single_spaced(&line[..at])
                    } else if before > widest {
                        format!(
                            "{}\n{}{after}",
                            single_spaced(&line[..at]),
                            " ".repeat(widest)
                        )
                    } else {
                        format!("{}{}{after}", &line[..at], " ".repeat(widest - before))
                    };
                }
            }
        }
    }
    lines
        .iter()
        .flat_map(|line| line.split('\n'))
        .map(|line| line.replace(MARKS, "").trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `lead` as a line that runs on draws it: its indent kept, and each run
/// of spaces after it drawn as one, with none at its end. A
/// [`BRAILLE_BLANK`] counts as a space. The indent is the row's own, as
/// [`unmarked`] leaves a peer's text no space at its start.
fn single_spaced(lead: &str) -> String {
    let body = lead.trim_start();
    let indent = &lead[..lead.len() - body.len()];
    let words: Vec<&str> = body
        .split(|each: char| each.is_whitespace() || each == BRAILLE_BLANK)
        .filter(|word| !word.is_empty())
        .collect();
    format!("{indent}{}", words.join(" "))
}

/// The cells `text` takes in a terminal, as Unicode's East Asian Width
/// and emoji presentation say: two for a wide character or an emoji,
/// none for a joiner or a combining mark, one for the rest. A colour's
/// escape, which [`Look::paint`] writes and no peer's text keeps once
/// [`unmarked`], takes none.
pub(crate) fn drawn_width(text: &str) -> usize {
    let mut count = 0;
    let mut rest = text;
    while let Some(at) = rest.find("\u{1b}[") {
        count += rest[..at].width();
        let after = &rest[at + 2..];
        // An escape ends at its first letter or other final byte, each
        // one byte long.
        let end = after
            .find(|each: char| ('@'..='~').contains(&each))
            .map_or(after.len(), |end| end + 1);
        rest = &after[end..];
    }
    count + rest.width()
}

/// The longest start of `text` that takes at most `room` cells, as
/// [`drawn_width`] counts them, so a wide character that would straddle
/// the edge is left for what follows. A colour's escape is one step, so
/// a cut never leaves one half written, though it may leave a colour
/// open, which is why each caller paints after cutting. A joiner left at
/// the cut goes, as it would join nothing, and a pile of marks past
/// [`STEPS_PER_CELL`] steps to a cell is cut.
pub(crate) fn within(text: &str, room: usize) -> &str {
    // A selector widens the heart before it and a joiner makes two emoji
    // one, so a start can draw wider or narrower than its characters
    // summed. Unicode's width rules join a character to what's before it
    // only in the cases `starts_afresh` names, so the cells up to any
    // other are settled and only the run since is measured again: a cut
    // costs what it walks, even into a pipe, where the room is the whole
    // text, which may be a peer's message of 256 KiB. The one sequence
    // narrower than its characters, a text selector after an emoji,
    // narrows by a cell, so a start two cells past the room never comes
    // back within it and the walk stops there.
    let mut kept = 0;
    let (mut at, mut from, mut settled, mut since) = (0, 0, 0, 0);
    let mut before: Option<char> = None;
    for step in steps(text).take(most_steps(room)) {
        if let Some(each) = step.chars().next().filter(|_| !step.starts_with("\u{1b}[")) {
            if starts_afresh(before, each) || (since >= LONGEST_RUN && splits(before, each)) {
                settled += drawn_width(&text[from..at]);
                from = at;
                since = 0;
            }
            before = Some(each);
        }
        since += 1;
        at += step.len();
        let drawn = settled + drawn_width(&text[from..at]);
        if drawn <= room {
            kept = at;
        } else if drawn > room.saturating_add(2) {
            break;
        }
    }
    text[..kept].trim_end_matches('\u{200D}')
}

/// Each step a cut takes through `text`: a colour's escape whole, else
/// one character.
fn steps(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        let step = rest.strip_prefix("\u{1b}[").map_or_else(
            || rest.chars().next().map_or(0, char::len_utf8),
            |after| {
                2 + after
                    .find(|each: char| ('@'..='~').contains(&each))
                    .map_or(after.len(), |end| end + 1)
            },
        );
        let (taken, left) = rest.split_at(step);
        rest = left;
        (step > 0).then_some(taken)
    })
}

/// Whether no width rule of Unicode's joins `each` to what's before it:
/// it takes a cell of its own, isn't a modifier, a flag's half or a
/// letter of a script whose ligatures narrow, and follows no joiner.
fn starts_afresh(before: Option<char>, each: char) -> bool {
    let follows_a_joiner = matches!(before, Some('\u{200D}' | '\u{2D7F}' | '\u{17D2}' | '\r'));
    let joins_what_is_before = matches!(
        each,
        // Emoji modifiers and regional indicators.
        '\u{1F3FB}'..='\u{1F3FF}' | '\u{1F1E6}'..='\u{1F1FF}'
        // Arabic, where an alef joins the lam before it.
        | '\u{0600}'..='\u{08FF}' | '\u{FB50}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFF}'
        // Lisu's tone letters, Kirat Rai's vowels and Tifinagh's joiner.
        | '\u{A4F8}'..='\u{A4FF}' | '\u{16D40}'..='\u{16D7F}' | '\u{2D7F}'
    );
    !follows_a_joiner
        && !joins_what_is_before
        && unicode_width::UnicodeWidthChar::width(each).is_some_and(|cells| cells > 0)
}

/// Whether a run measured apart before `each` counts at least the cells
/// it draws: split anywhere else a sequence only counts more, so a cut
/// keeps less, but a selector split from what it selects counts less.
fn splits(before: Option<char>, each: char) -> bool {
    let selector = |each: char| matches!(each, '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}');
    !selector(each) || before.is_some_and(selector)
}

/// The most steps measured together before a cut settles them, which only
/// a run with no fresh start reaches: a pile of marks, or many emoji
/// joined with no space between. Anything Unicode joins is shorter.
const LONGEST_RUN: usize = 32;

/// The most steps a cut to `room` takes.
const fn most_steps(room: usize) -> usize {
    room.saturating_add(1).saturating_mul(STEPS_PER_CELL)
}

/// The most steps, a character or a colour's escape, a cut keeps for
/// each cell of its room. An emoji joined from seven characters takes
/// two cells, and writing that stacks its marks puts fewer than eight on
/// one, so only a pile meant to bury what's beside it loses any.
const STEPS_PER_CELL: usize = 8;

impl Look {
    /// Whether it is drawn [`WHOLE`], into a pipe.
    pub(crate) const fn whole(&self) -> bool {
        self.width == WHOLE
    }

    /// `mark` where it is drawn [`WHOLE`], for [`aligned`]; else nothing.
    pub(crate) const fn mark(&self, mark: &'static str) -> &'static str {
        if self.whole() { mark } else { "" }
    }

    /// `lines` as one text, their columns lined up where it is drawn
    /// [`WHOLE`].
    pub(crate) fn joined(&self, lines: &[String]) -> String {
        let text = lines.join("\n");
        if self.whole() { aligned(&text) } else { text }
    }

    /// `text` in `colour`, where it is drawn in colour.
    pub(crate) fn paint(&self, colour: &str, text: &str) -> String {
        if self.colour && !colour.is_empty() && !text.trim().is_empty() {
            format!("{colour}{text}{RESET}")
        } else {
            text.to_owned()
        }
    }

    /// When `at`, an RFC 3339 instant, was: its time today, its weekday
    /// and time this week, else its day.
    pub(crate) fn when(&self, at: &str) -> Option<String> {
        let at = at
            .parse::<jiff::Timestamp>()
            .ok()?
            .to_zoned(self.now.time_zone().clone());
        let days = (self.now.date() - at.date()).get_days();
        let format = match days {
            0 => "%H:%M",
            1..=6 => "%a %H:%M",
            _ => "%-d %b",
        };
        Some(at.strftime(format).to_string())
    }
}

/// `text` cut to `width` cells, as [`drawn_width`] counts them, its last
/// one an ellipsis where cut.
pub(crate) fn fit(text: &str, width: usize) -> String {
    // What draws within the width is kept whole, unless it piles more
    // steps on its cells than a cut keeps, joiners as much as marks.
    if drawn_width(text) <= width && steps(text).nth(most_steps(width)).is_none() {
        return text.to_owned();
    }
    format!("{}…", within(text, width.saturating_sub(1)).trim_end())
}

/// `text` wrapped at its spaces to `width` cells, as [`drawn_width`]
/// counts them, its first line after `first` and each after it after
/// `rest`. A word wider than a whole line's room, such as a path, is
/// broken across lines rather than passing it.
pub(crate) fn wrapped(text: &str, first: &str, rest: &str, width: usize) -> String {
    let room = |at: usize| {
        let indent = if at == 0 { first } else { rest };
        width.saturating_sub(drawn_width(indent)).max(1)
    };
    let mut lines: Vec<String> = Vec::new();
    // The cells the last line takes, added to as it grows rather than
    // measured again for each word, as no sequence spans a space, so a
    // long text drawn whole costs what it does once.
    let mut drawn = 0;
    for word in text.split_whitespace() {
        let cells = drawn_width(word);
        let joins = !lines.is_empty() && drawn + 1 + cells <= room(lines.len() - 1);
        if let Some(line) = lines.last_mut().filter(|_| joins) {
            line.push(' ');
            line.push_str(word);
            drawn += 1 + cells;
            continue;
        }
        let mut left = word;
        while !left.is_empty() {
            let mut taken = within(left, room(lines.len()));
            // A wide character, or an emoji joined from several, that a
            // line of one cell can't hold still takes a line of its own,
            // whole, so the word ends.
            if taken.is_empty() {
                taken = within(left, 2);
            }
            if taken.is_empty() {
                taken = &left[..left.chars().next().map_or(0, char::len_utf8)];
            }
            drawn = drawn_width(taken);
            lines.push(taken.to_owned());
            left = &left[taken.len()..];
        }
    }
    lines
        .iter()
        .enumerate()
        .fold(String::new(), |mut out, (at, line)| {
            let indent = if at == 0 { first } else { rest };
            // Writing to a String never fails.
            let _ = writeln!(out, "{indent}{line}");
            out
        })
}

/// `text` padded with spaces to `width` cells, as [`drawn_width`] counts
/// them.
pub(crate) fn padded(text: &str, width: usize) -> String {
    let short = width.saturating_sub(drawn_width(text));
    format!("{text}{}", " ".repeat(short))
}

/// How wide the counts' columns are together, their gaps before each.
fn counts_width(columns: &[Standing]) -> usize {
    columns
        .iter()
        .map(|each| GAP.len() + each.header().len())
        .sum()
}

/// A team's or a session's name as a person reads it: a session's
/// quest's title where it says one, rather than its window's.
pub(crate) fn title(record: &Value) -> &str {
    record["presence"]["questInfo"]["title"]
        .as_str()
        .or_else(|| record["title"].as_str())
        .or_else(|| record["working_on"].as_str())
        .unwrap_or("a session without a name")
}

/// The address a team record is keyed by.
pub(crate) fn address(team: &Value) -> &str {
    team["key"]
        .as_str()
        .and_then(|key| key.strip_prefix("team:"))
        .unwrap_or_default()
}

/// Each of the swarm's teams' short names, as [`shorts_beside`] keeps
/// them beside its living sessions.
pub(crate) fn shorts_of(swarm: &Swarm) -> BTreeMap<String, String> {
    let living = swarm
        .sessions
        .iter()
        .filter(|session| Standing::of(session).is_some())
        .filter_map(|session| Some((session["address"].as_str()?, session["id"].as_str())));
    shorts_beside(&swarm.teams, living)
}

/// Each team's short name, as [`short_names`] gives it, unless it cleans
/// to another team's address, or to the address of a session among
/// `living`, by its address and id, unless that session leads the team
/// and the team goes by that address too. A steer typed with such a name
/// reaches what has that address first, and a session leaves it to a
/// team only where both hold, as `session_by_address` asks, so a page
/// never draws, nor a hint offers, a name that reaches another.
pub(crate) fn shorts_beside<'a>(
    teams: &[Value],
    living: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
) -> BTreeMap<String, String> {
    let living: Vec<(&str, Option<&str>)> = living.into_iter().collect();
    let mut shorts = short_names(teams);
    shorts.retain(|team, short| {
        let short = unmarked(short);
        let lead = teams
            .iter()
            .find(|each| address(each) == team)
            .and_then(|each| each["lead"].as_str());
        let a_team = teams
            .iter()
            .map(address)
            .any(|other| other != team && other.eq_ignore_ascii_case(&short));
        let its_own = team.eq_ignore_ascii_case(&short);
        let a_session = living.iter().any(|(other, id)| {
            other.eq_ignore_ascii_case(&short) && !(its_own && id.is_some() && *id == lead)
        });
        !a_team && !a_session
    });
    shorts
}

/// How many of a team's sessions stand as `each`.
fn count(team: &Value, each: Standing) -> u64 {
    team["counts"][each.counted()].as_u64().unwrap_or_default()
}

/// How many of a team's sessions it counts at all.
fn size(team: &Value) -> u64 {
    Standing::EACH.iter().map(|each| count(team, *each)).sum()
}

/// How the worst of a team stands.
fn worst(team: &Value) -> Standing {
    team["worst"]
        .as_str()
        .and_then(Standing::from_counted)
        .unwrap_or(Standing::Quiet)
}

/// How a row names what it counts: its short name, padded to `width` so
/// the titles after it line up, where it has one, then its title.
pub(crate) struct Label<'a> {
    pub(crate) short: Option<&'a str>,
    pub(crate) width: usize,
    pub(crate) title: &'a str,
}

impl<'a> Label<'a> {
    /// A row named only by `title`.
    pub(crate) const fn titled(title: &'a str) -> Self {
        Self {
            short: None,
            width: 0,
            title,
        }
    }

    /// `team`'s, its short name among `shorts` padded to `width`.
    fn of(team: &'a Value, shorts: &'a BTreeMap<String, String>, width: usize) -> Self {
        Self {
            short: names::shown(team, shorts),
            width,
            title: title(team),
        }
    }
}

/// How wide the short names shown beside `teams` are, the widest's.
fn shorts_width<'a>(
    teams: impl Iterator<Item = &'a Value>,
    shorts: &BTreeMap<String, String>,
) -> usize {
    teams
        .filter_map(|team| names::shown(team, shorts))
        .map(|short| drawn_width(&unmarked(short)))
        .max()
        .unwrap_or_default()
}

/// One row of counts under the header: what it counts, named by `label`
/// and set in by `depth`, then a column for each standing `counts` holds,
/// in its order, a count of none left blank.
pub(crate) fn row(
    look: &Look,
    depth: usize,
    label: &Label<'_>,
    counts: &[(Standing, u64)],
    strong: bool,
) -> String {
    let columns: Vec<Standing> = counts.iter().map(|(each, _)| *each).collect();
    // Each part is cleaned before it is padded, so a character a peer's
    // name loses can't move its title off the column.
    let plain = label.short.map_or_else(
        || unmarked(label.title),
        |short| {
            format!(
                "{}{GAP}{}",
                padded(&unmarked(short), label.width),
                unmarked(label.title)
            )
        },
    );
    // Drawn whole, the name is as wide as it is, and its counts are lined
    // up with the table's once it's drawn.
    let room = if look.whole() {
        drawn_width(&plain)
    } else {
        look.width
            .saturating_sub(counts_width(&columns) + depth * STEP)
            .max(NARROWEST)
    };
    let lead = label.short.map_or(room, |_| label.width + GAP.len());
    let name = padded(&fit(&plain, room), room);
    let head = within(&name, lead);
    let tail = &name[head.len()..];
    let mut line = format!(
        "{}{}{}{}",
        " ".repeat(depth * STEP),
        look.paint(if strong { BOLD } else { "" }, head),
        look.paint(FAINT, tail),
        look.mark(COUNTS_AT)
    );
    for &(each, count) in counts {
        let shown = if count == 0 {
            String::new()
        } else {
            count.to_string()
        };
        let cell = format!("{shown:>width$}", width = each.header().len());
        let _ = write!(line, "{GAP}{}", look.paint(each.colour(), &cell));
    }
    line.trim_end().to_owned()
}

/// The header over the counts of `columns`, lined up with them.
pub(crate) fn header(look: &Look, depth: usize, columns: &[Standing]) -> String {
    if look.whole() {
        let mut line = format!("{}{COUNTS_AT}", " ".repeat(depth * STEP));
        for &each in columns {
            let _ = write!(line, "{GAP}{}", each.header());
        }
        return line;
    }
    let room = look
        .width
        .saturating_sub(counts_width(columns) + depth * STEP)
        .max(NARROWEST);
    let mut line = " ".repeat(room + depth * STEP);
    for &each in columns {
        let _ = write!(line, "{GAP}{}", each.header());
    }
    look.paint(FAINT, &line)
}

/// Teams in the order they are read: those needing you first, then the
/// largest, then by name.
fn ordered<'a>(teams: impl Iterator<Item = &'a Value>) -> Vec<&'a Value> {
    let mut teams: Vec<&Value> = teams.collect();
    teams.sort_by(|one, other| {
        worst(one)
            .cmp(&worst(other))
            .then_with(|| size(other).cmp(&size(one)))
            .then_with(|| title(one).cmp(title(other)))
    });
    teams
}

/// The counts a team's record holds, in the header's order.
fn counts_of(team: &Value) -> [(Standing, u64); 4] {
    Standing::EACH.map(|each| (each, count(team, each)))
}

/// Living sessions no team holds.
fn alone(swarm: &Swarm) -> Vec<&Value> {
    let held: BTreeSet<&str> = swarm
        .teams
        .iter()
        .filter_map(|team| team["members"].as_array())
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    swarm
        .sessions
        .iter()
        .filter(|session| Standing::of(session).is_some())
        .filter(|session| !session["id"].as_str().is_some_and(|id| held.contains(id)))
        .collect()
}

/// Every team, nested as they are, worst first, and how many sessions run
/// on their own.
#[must_use]
pub fn overview(swarm: &Swarm, look: &Look) -> String {
    let living: Vec<Standing> = swarm.sessions.iter().filter_map(Standing::of).collect();
    let mut head = format!("{} · {} running", swarm.realm, living.len());
    for each in [Standing::NeedsYou, Standing::Stuck] {
        let many = living.iter().filter(|one| **one == each).count();
        if many > 0 {
            let _ = write!(
                head,
                " · {}",
                look.paint(each.colour(), &format!("{many} {}", each.said()))
            );
        }
    }
    let mut lines = vec![
        look.paint(BOLD, &head),
        String::new(),
        header(look, 0, &Standing::EACH),
    ];
    let addresses: BTreeSet<&str> = swarm.teams.iter().map(address).collect();
    let mut within: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for team in &swarm.teams {
        let above = team["within"]
            .as_str()
            .filter(|above| addresses.contains(above));
        within
            .entry(above.unwrap_or_default())
            .or_default()
            .push(team);
    }
    let shorts = shorts_of(swarm);
    let shown = Shorts {
        width: shorts_width(swarm.teams.iter(), &shorts),
        by: &shorts,
    };
    let mut passed = BTreeSet::new();
    nest(&mut lines, look, &within, &shown, "", 0, &mut passed);
    let alone = alone(swarm);
    if !alone.is_empty() {
        let counts = Standing::EACH.map(|each| {
            let many = alone
                .iter()
                .filter(|one| Standing::of(one) == Some(each))
                .count();
            (each, u64::try_from(many).unwrap_or(u64::MAX))
        });
        lines.push(row(look, 0, &Label::titled("on their own"), &counts, false));
    }
    lines.push(String::new());
    // The first team on the list is the example, typed as a person would.
    let first = within
        .get("")
        .and_then(|top| ordered(top.iter().copied()).first().copied());
    // A chosen short name is a peer's text, and the example is copied
    // into a shell, so it is given only as a word a shell and `swarm`
    // read as one name, then as the team's address, and otherwise not.
    let example = first
        .and_then(|team| {
            let typed = unmarked(names::typed(team, &shorts));
            if names::one_word(&typed) {
                Some(typed)
            } else {
                typeable(address(team)).then(|| address(team).to_owned())
            }
        })
        .unwrap_or_else(|| "<team>".to_owned());
    lines.push(look.paint(
        FAINT,
        &format!(
            "see a team by any part of its name: mycelium --realm {} swarm {example}",
            swarm.realm
        ),
    ));
    look.joined(&lines)
}

/// Each team as a person types it, its short name where it has one, in
/// the order the overview lists them, for a shell to complete. Each is
/// cleaned as a page draws it, since a peer gives the name and a shell
/// may draw what it completes; matching cleans both sides, so the
/// cleaned name still reaches its team.
#[must_use]
pub fn typed(swarm: &Swarm) -> Vec<String> {
    let shorts = shorts_of(swarm);
    ordered(swarm.teams.iter())
        .into_iter()
        .map(|team| unmarked(names::typed(team, &shorts)))
        .collect()
}

/// The short names a row of a team is shown with, and how wide they are.
struct Shorts<'a> {
    by: &'a BTreeMap<String, String>,
    width: usize,
}

/// The teams within `above` as rows set in by `depth`, each followed by
/// those within it, never one already `passed`.
fn nest<'a>(
    lines: &mut Vec<String>,
    look: &Look,
    within: &BTreeMap<&str, Vec<&'a Value>>,
    shorts: &Shorts<'_>,
    above: &str,
    depth: usize,
    passed: &mut BTreeSet<&'a str>,
) {
    let Some(teams) = within.get(above) else {
        return;
    };
    for team in ordered(teams.iter().copied()) {
        if !passed.insert(address(team)) {
            continue;
        }
        let label = Label::of(team, shorts.by, shorts.width);
        lines.push(row(look, depth, &label, &counts_of(team), depth == 0));
        nest(
            lines,
            look,
            within,
            shorts,
            address(team),
            depth + 1,
            passed,
        );
    }
}

/// The team `asked` names, by its address, its short name or a part of
/// its title, or why none or several are, naming each of several as it's
/// typed, so one can be picked.
fn named<'a>(swarm: &'a Swarm, asked: &str) -> Result<&'a Value, String> {
    let shorts = shorts_of(swarm);
    let naming = |team: &'a Value| Naming {
        address: address(team),
        short: shorts.get(address(team)).map(String::as_str),
        title: title(team),
    };
    match picked(&swarm.teams, asked, true, naming) {
        Picked::One(team) => Ok(team),
        Picked::None => Err(format!("no team is named {asked}")),
        Picked::Several(many) => {
            // Each is a peer's text, so it is held to one line.
            let named = told_apart(
                &many
                    .iter()
                    .map(|team| {
                        (
                            unmarked(names::typed(team, &shorts)),
                            Some(address(team)).filter(|address| typeable(address)),
                        )
                    })
                    .collect::<Vec<_>>(),
            );
            Err(format!(
                "{asked} could be {}",
                either(&named.iter().map(String::as_str).collect::<Vec<_>>())
            ))
        }
    }
}

/// The names a person may type for one thing to reach it.
pub(crate) struct Naming<'a> {
    /// What it is keyed by, matched whole.
    pub(crate) address: &'a str,
    /// The short name it goes by, matched whole, its words typed apart or
    /// joined by `-`.
    pub(crate) short: Option<&'a str>,
    /// Its title, matched by any part of it.
    pub(crate) title: &'a str,
}

/// What a name typed picked among things.
pub(crate) enum Picked<'a, T> {
    /// Nothing goes by it.
    None,
    /// The one thing that does.
    One(&'a T),
    /// Each of several that do, for the person to choose between.
    Several(Vec<&'a T>),
}

/// The one of `things` that `asked` names: by its address or its short
/// name whole, in either case, and else by a part of its title, as
/// `naming` says each; or none, or each of several. Where the address is
/// `first`, as a team's is, it comes before a short name, which a peer
/// picks and could clean to another's address to hide it; a network's
/// name is a group a peer may pick as well, so it isn't. An address
/// matched in its own case comes before one matched in another, so two
/// keyed alike but for case are each reached by the address told apart.
/// A short name and a title are matched as they are drawn, cleaned on
/// both sides, so what a page or hint shows finds what it names.
pub(crate) fn picked<'a: 'n, 'n, T>(
    things: &'a [T],
    asked: &str,
    first: bool,
    naming: impl Fn(&'a T) -> Naming<'n>,
) -> Picked<'a, T> {
    // A short name's words may be typed apart, as `part deux`.
    let joined = unmarked(&asked.split_whitespace().collect::<Vec<_>>().join("-"));
    let exactly: Vec<&T> = things
        .iter()
        .filter(|thing| naming(thing).address == asked)
        .collect();
    let by_address: Vec<&T> = if exactly.is_empty() {
        things
            .iter()
            .filter(|thing| naming(thing).address.eq_ignore_ascii_case(asked))
            .collect()
    } else {
        exactly
    };
    let by_short = |thing: &'a T| {
        naming(thing)
            .short
            .is_some_and(|short| unmarked(short).eq_ignore_ascii_case(&joined))
    };
    let whole: Vec<&T> = if first && !by_address.is_empty() {
        by_address
    } else {
        things
            .iter()
            .filter(|thing| {
                by_address.iter().any(|one| std::ptr::eq(*one, *thing)) || by_short(thing)
            })
            .collect()
    };
    let lower = unmarked(asked).to_lowercase();
    let found = if whole.is_empty() {
        things
            .iter()
            .filter(|thing| {
                unmarked(naming(thing).title)
                    .to_lowercase()
                    .contains(&lower)
            })
            .collect()
    } else {
        whole
    };
    match found.as_slice() {
        [] => Picked::None,
        [one] => Picked::One(one),
        _ => Picked::Several(found),
    }
}

/// The team `asked` names: what it is within and led by, what it landed
/// last, the teams within it, its sessions worst first, and where it
/// meets other teams.
///
/// # Errors
///
/// When no team, or more than one, goes by what was asked.
pub fn team(swarm: &Swarm, asked: &str, look: &Look) -> Result<String, String> {
    let team = named(swarm, asked)?;
    let titled: BTreeMap<&str, &str> = swarm
        .teams
        .iter()
        .map(|team| (address(team), title(team)))
        .collect();
    let sessions: BTreeMap<&str, &Value> = swarm
        .sessions
        .iter()
        .filter_map(|session| Some((session["id"].as_str()?, session)))
        .collect();
    let mut head = title(team).to_owned();
    if let Some(lead) = team["lead"].as_str().and_then(|lead| sessions.get(lead)) {
        let _ = write!(head, " · led by {}", beside(lead, &[title(team)]));
    }
    if let Some(above) = team["within"].as_str().and_then(|above| titled.get(above)) {
        let _ = write!(head, " · within {above}");
    }
    // Peer text off the table's rows is unmarked too, since a mark in a
    // title here would line up with another's in its block.
    let mut lines = vec![look.paint(BOLD, &unmarked(&head))];
    if let Some(landed) = landed(team, look) {
        lines.push(look.paint(FAINT, &unmarked(&landed)));
    }
    let inner = ordered(
        swarm
            .teams
            .iter()
            .filter(|other| other["within"] == json!(address(team))),
    );
    if !inner.is_empty() {
        let shorts = shorts_of(swarm);
        let width = shorts_width(inner.iter().copied(), &shorts);
        lines.extend([String::new(), header(look, 0, &Standing::EACH)]);
        for other in inner {
            let label = Label::of(other, &shorts, width);
            lines.push(row(look, 0, &label, &counts_of(other), false));
        }
    }
    let mut members: Vec<(Standing, &Value)> = team["members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|id| sessions.get(id.as_str()?))
        .filter_map(|session| Some((Standing::of(session)?, *session)))
        .collect();
    members.sort_by(|(one, a), (other, b)| one.cmp(other).then_with(|| title(a).cmp(title(b))));
    if !members.is_empty() {
        lines.push(String::new());
        lines.extend(
            members
                .iter()
                .map(|(standing, session)| member(look, *standing, session)),
        );
    }
    let meets = meetings(team, &titled, &sessions);
    if !meets.is_empty() {
        lines.push(String::new());
        lines.extend(meets.iter().map(|line| look.paint(FAINT, &unmarked(line))));
    }
    Ok(look.joined(&lines))
}

/// What a team landed last, and when.
fn landed(team: &Value, look: &Look) -> Option<String> {
    let landed = &team["landed"];
    let what = landed["what"].as_str()?;
    let mut line = format!("landed {what}");
    if let Some(reference) = landed["ref"].as_str() {
        let _ = write!(line, " · {reference}");
    }
    if let Some(when) = landed["at"].as_str().and_then(|at| look.when(at)) {
        let _ = write!(line, " · {when}");
    }
    Some(line)
}

/// One session of a team: how it stands, its name, and what it asks, or
/// is doing, where it says.
fn member(look: &Look, standing: Standing, session: &Value) -> String {
    let said = Standing::EACH
        .iter()
        .map(|each| each.said().len())
        .max()
        .unwrap_or_default();
    let doing = standing.doing(session);
    let room = look.width.saturating_sub(said + GAP.len());
    let name = unmarked(title(session));
    // Drawn whole, the name is as wide as it is, and what it does is lined
    // up with the others' once they're drawn.
    let name_room = if look.whole() {
        drawn_width(&name)
    } else if doing.is_some() {
        (room / 2).max(NARROWEST)
    } else {
        room
    };
    let mut line = format!(
        "{}{GAP}{}",
        look.paint(standing.colour(), &padded(standing.said(), said)),
        padded(&fit(&name, name_room), name_room)
    );
    if let Some(doing) = doing {
        let left = room.saturating_sub(name_room + GAP.len()).max(NARROWEST);
        let _ = write!(
            line,
            "{}{GAP}{}",
            look.mark(DOING_AT),
            look.paint(FAINT, &fit(&unmarked(doing), left))
        );
    }
    line.trim_end().to_owned()
}

/// A session's name beside the teams titled `others`: its title, or its
/// address where its title is one of theirs. A lead's quest is most often
/// its team's own, and then its title would only repeat the team's.
fn beside<'a>(session: &'a Value, others: &[&str]) -> &'a str {
    let titled = title(session);
    if others.contains(&titled) {
        session["address"].as_str().unwrap_or(titled)
    } else {
        titled
    }
}

/// Where a team meets others: the repositories it works in beside them,
/// and its sessions hearing them too.
fn meetings(
    team: &Value,
    titled: &BTreeMap<&str, &str>,
    sessions: &BTreeMap<&str, &Value>,
) -> Vec<String> {
    let heard = |teams: &Value| -> Vec<&str> {
        teams
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|team| titled.get(team.as_str()?).copied())
            .collect()
    };
    let names = |teams: &Value| listed(&heard(teams));
    let bridges = team["bridges"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|bridge| {
            format!(
                "works in {} beside {}",
                bridge["repo"].as_str().unwrap_or_default(),
                names(&bridge["teams"])
            )
        });
    let liaisons = team["liaisons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|liaison| {
            let session = sessions.get(liaison["session"].as_str()?)?;
            Some(format!(
                "{} also hears {}",
                beside(session, &heard(&liaison["teams"])),
                names(&liaison["teams"])
            ))
        });
    bridges.chain(liaisons).collect()
}

/// `names` as a sentence lists them: one, two joined by and, or several
/// with commas and a last and.
pub(crate) fn listed(names: &[&str]) -> String {
    joined(names, "and")
}

/// Each of several choices by the name it's drawn with, and by its
/// address too where another draws alike, so a refusal offering both
/// still names something to type for each. An address is a peer's text
/// too, so each is given only where typing it, where the refusal is
/// answered, reaches the one it names, as its caller judges, and is
/// `None` where it doesn't.
pub(crate) fn told_apart(named: &[(String, Option<&str>)]) -> Vec<String> {
    let alike = |name: &str| {
        named
            .iter()
            .filter(|(other, _)| other.eq_ignore_ascii_case(name))
            .count()
            > 1
    };
    named
        .iter()
        .map(|(name, address)| match address {
            _ if !alike(name) => name.clone(),
            Some(address) => format!("{name} ({address})"),
            None => format!("{name} (no address to type)"),
        })
        .collect()
}

/// Whether a peer's `address` can be typed alone after `swarm` to reach
/// what it names: letters and digits, as a cell's is, and one word
/// `swarm` reads as a name, not `PAUSE` or `TEAM`.
fn typeable(address: &str) -> bool {
    crate::forage::is_address(address) && names::one_word(address)
}

/// `names` as a sentence offers a choice of them: two joined by or, or
/// several with commas and a last or.
pub(crate) fn either(names: &[&str]) -> String {
    joined(names, "or")
}

fn joined(names: &[&str], last_by: &str) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [first @ .., last] => format!("{} {last_by} {last}", first.join(", ")),
    }
}

#[cfg(test)]
mod tests;
