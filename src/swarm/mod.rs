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

mod asks;
mod names;
mod shapes;
pub mod steers;

#[cfg(test)]
pub(crate) use asks::{ASKS_FIT, together};
pub(crate) use asks::{asked, asking};
pub use asks::{read, records};
pub(crate) use names::says_again;
pub use names::short_names;
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
/// it, each table's columns as wide as what is in them.
pub const WHOLE: usize = usize::MAX;

/// Where a row's counts begin, as a table is drawn [`WHOLE`], for
/// [`aligned`] to line them up once every row is drawn: a character of
/// the private use area, as no title has one once [`unmarked`].
pub(crate) const COUNTS_AT: &str = "\u{e000}";

/// Where a line's doing begins, as [`COUNTS_AT`] says its counts do.
pub(crate) const DOING_AT: &str = "\u{e001}";

/// `text` with no mark [`aligned`] reads, so a title can't move a column.
pub(crate) fn unmarked(text: &str) -> String {
    text.replace(['\u{e000}', '\u{e001}'], "")
}

/// `text` drawn [`WHOLE`] with its marks lined up: in each block of
/// lines between blank ones, what comes before each mark is padded to
/// the widest of the block's, counts and doings each on their own. A pipe
/// is never drawn in colour, so a character is a column.
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
            let Some(widest) = block
                .iter()
                .filter_map(|line| Some(line[..line.find(mark)?].chars().count()))
                .max()
            else {
                continue;
            };
            for line in block.iter_mut() {
                if let Some(at) = line.find(mark) {
                    let before = line[..at].chars().count();
                    let after = &line[at + mark.len()..];
                    *line = format!("{}{}{after}", &line[..at], " ".repeat(widest - before));
                }
            }
        }
    }
    lines
        .iter()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

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

/// `text` cut to `width` characters, its last one an ellipsis where cut.
pub(crate) fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// `text` wrapped at its spaces to `width` columns, its first line after
/// `first` and each after it after `rest`. A word wider than a whole line's
/// room, such as a path, is broken across lines rather than passing it.
pub(crate) fn wrapped(text: &str, first: &str, rest: &str, width: usize) -> String {
    let room = |at: usize| {
        let indent = if at == 0 { first } else { rest };
        width.saturating_sub(indent.chars().count()).max(1)
    };
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let joins = lines.last().is_some_and(|line| {
            line.chars().count() + 1 + word.chars().count() <= room(lines.len() - 1)
        });
        if let Some(line) = lines.last_mut().filter(|_| joins) {
            line.push(' ');
            line.push_str(word);
            continue;
        }
        let mut left: Vec<char> = word.chars().collect();
        while !left.is_empty() {
            let taken = room(lines.len()).min(left.len());
            lines.push(left.drain(..taken).collect());
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

/// `text` padded with spaces to `width` characters.
pub(crate) fn padded(text: &str, width: usize) -> String {
    let short = width.saturating_sub(text.chars().count());
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
        .map(|short| short.chars().count())
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
    let plain = unmarked(&label.short.map_or_else(
        || label.title.to_owned(),
        |short| format!("{}{GAP}{}", padded(short, label.width), label.title),
    ));
    // Drawn whole, the name is as wide as it is, and its counts are lined
    // up with the table's once it's drawn.
    let room = if look.whole() {
        plain.chars().count()
    } else {
        look.width
            .saturating_sub(counts_width(&columns) + depth * STEP)
            .max(NARROWEST)
    };
    let lead = label.short.map_or(room, |_| label.width + GAP.len());
    let name = padded(&fit(&plain, room), room);
    let head: String = name.chars().take(lead).collect();
    let tail: String = name.chars().skip(lead).collect();
    let mut line = format!(
        "{}{}{}{}",
        " ".repeat(depth * STEP),
        look.paint(if strong { BOLD } else { "" }, &head),
        look.paint(FAINT, &tail),
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
    let shorts = short_names(&swarm.teams);
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
    let example = first.map_or("<team>", |team| names::typed(team, &shorts));
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
/// the order the overview lists them, for a shell to complete.
#[must_use]
pub fn typed(swarm: &Swarm) -> Vec<String> {
    let shorts = short_names(&swarm.teams);
    ordered(swarm.teams.iter())
        .into_iter()
        .map(|team| names::typed(team, &shorts).to_owned())
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
    let shorts = short_names(&swarm.teams);
    let naming = |team: &'a Value| Naming {
        address: address(team),
        short: shorts.get(address(team)).map(String::as_str),
        title: title(team),
    };
    match picked(&swarm.teams, asked, naming) {
        Picked::One(team) => Ok(team),
        Picked::None => Err(format!("no team is named {asked}")),
        Picked::Several(many) => Err(format!(
            "{asked} could be {}",
            either(
                &many
                    .iter()
                    .map(|team| names::typed(team, &shorts))
                    .collect::<Vec<_>>()
            )
        )),
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
/// `naming` says each; or none, or each of several.
pub(crate) fn picked<'a: 'n, 'n, T>(
    things: &'a [T],
    asked: &str,
    naming: impl Fn(&'a T) -> Naming<'n>,
) -> Picked<'a, T> {
    // A short name's words may be typed apart, as `part deux`.
    let joined = asked.split_whitespace().collect::<Vec<_>>().join("-");
    let whole: Vec<&T> = things
        .iter()
        .filter(|thing| {
            let named = naming(thing);
            named.address.eq_ignore_ascii_case(asked)
                || named
                    .short
                    .is_some_and(|short| short.eq_ignore_ascii_case(&joined))
        })
        .collect();
    let lower = asked.to_lowercase();
    let found = if whole.is_empty() {
        things
            .iter()
            .filter(|thing| naming(thing).title.to_lowercase().contains(&lower))
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
    let mut lines = vec![look.paint(BOLD, &head)];
    if let Some(landed) = landed(team, look) {
        lines.push(look.paint(FAINT, &landed));
    }
    let inner = ordered(
        swarm
            .teams
            .iter()
            .filter(|other| other["within"] == json!(address(team))),
    );
    if !inner.is_empty() {
        let shorts = short_names(&swarm.teams);
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
        lines.extend(meets.iter().map(|line| look.paint(FAINT, line)));
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
        name.chars().count()
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
