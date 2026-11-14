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
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

mod names;
pub mod steers;

pub(crate) use names::says_again;
pub use names::short_names;

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

/// How it is drawn: how many columns it may take, whether in colour, and
/// when now is, for the times it says.
pub struct Look {
    /// The terminal's width, in columns.
    pub width: usize,
    /// Whether to colour it, as a terminal asked to.
    pub colour: bool,
    /// Now, in the time zone the times are said in.
    pub now: jiff::Zoned,
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
    /// and none for one that has ended otherwise.
    pub(crate) fn of_cell(session: &Value) -> Option<Self> {
        let parked = json!("parked");
        if session["standing"] == parked || session["presence"]["state"] == parked {
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
const RESET: &str = "\x1b[0m";

/// The space between columns.
pub(crate) const GAP: &str = "  ";

/// How far each team in is set in from the one it is within.
const STEP: usize = 2;

/// The narrowest a name is cut to, however narrow the terminal.
pub(crate) const NARROWEST: usize = 16;

impl Look {
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
    let room = look
        .width
        .saturating_sub(counts_width(&columns) + depth * STEP)
        .max(NARROWEST);
    let (plain, lead) = label.short.map_or_else(
        || (label.title.to_owned(), room),
        |short| {
            (
                format!("{}{GAP}{}", padded(short, label.width), label.title),
                label.width + GAP.len(),
            )
        },
    );
    let name = padded(&fit(&plain, room), room);
    let head: String = name.chars().take(lead).collect();
    let tail: String = name.chars().skip(lead).collect();
    let mut line = format!(
        "{}{}{}",
        " ".repeat(depth * STEP),
        look.paint(if strong { BOLD } else { "" }, &head),
        look.paint(FAINT, &tail)
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
    lines.join("\n")
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
        let _ = write!(head, " · led by {}", title(lead));
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
    Ok(lines.join("\n"))
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
    let name_room = if doing.is_some() {
        (room / 2).max(NARROWEST)
    } else {
        room
    };
    let name = fit(title(session), name_room);
    let mut line = format!(
        "{}{GAP}{}",
        look.paint(standing.colour(), &padded(standing.said(), said)),
        padded(&name, name_room)
    );
    if let Some(doing) = doing {
        let left = room.saturating_sub(name_room + GAP.len()).max(NARROWEST);
        let _ = write!(line, "{GAP}{}", look.paint(FAINT, &fit(doing, left)));
    }
    line.trim_end().to_owned()
}

/// Where a team meets others: the repositories it works in beside them,
/// and its sessions hearing them too.
fn meetings(
    team: &Value,
    titled: &BTreeMap<&str, &str>,
    sessions: &BTreeMap<&str, &Value>,
) -> Vec<String> {
    let names = |teams: &Value| -> String {
        let names: Vec<&str> = teams
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|team| titled.get(team.as_str()?).copied())
            .collect();
        listed(&names)
    };
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
                title(session),
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

/// Reads the swarm from the node at `socket` in the daemon interface,
/// giving up on any read or write taking longer than `within`.
///
/// # Errors
///
/// When no node answers, or it answers in a way it shouldn't, or holds no
/// teams, as a node too old for them doesn't.
pub fn read(socket: &Path, realm: &str, within: Duration) -> io::Result<Swarm> {
    let mut answers = asked(
        socket,
        within,
        &[
            json!({"ask": "list", "kind": "team"}),
            json!({"ask": "list", "kind": "session"}),
        ],
    )?;
    let mut records = |id: u64| -> io::Result<Vec<Value>> {
        let answer = answers.remove(&id).unwrap_or_default();
        if let Some(why) = answer["error"]["message"].as_str() {
            return Err(io::Error::other(format!(
                "the node can't say its teams ({why}); it may need updating"
            )));
        }
        Ok(answer["records"].as_array().cloned().unwrap_or_default())
    };
    Ok(Swarm {
        realm: realm.to_owned(),
        teams: records(1)?,
        sessions: records(2)?,
    })
}

/// Asks the node at `socket` each of `asks` in the daemon interface,
/// after its hello, and gives its answers by their place in `asks`,
/// counting from one.
pub(crate) fn asked(
    socket: &Path,
    within: Duration,
    asks: &[Value],
) -> io::Result<BTreeMap<u64, Value>> {
    let mut writing = UnixStream::connect(socket).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("no node answers at {}: {error}", socket.display()),
        )
    })?;
    writing.set_read_timeout(Some(within))?;
    writing.set_write_timeout(Some(within))?;
    let mut reading = BufReader::new(writing.try_clone()?);
    writeln!(writing, "{}", json!({"id": 0, "ask": "hello"}))?;
    for (id, ask) in (1_u64..).zip(asks) {
        let mut ask = ask.clone();
        ask["id"] = id.into();
        writeln!(writing, "{ask}")?;
    }
    let wanted = (1_u64..)
        .zip(asks)
        .map(|(id, _)| id)
        .collect::<BTreeSet<u64>>();
    let mut answers: BTreeMap<u64, Value> = BTreeMap::new();
    let mut line = String::new();
    while !wanted.iter().all(|id| answers.contains_key(id)) {
        line.clear();
        if reading.read_line(&mut line)? == 0 {
            return Err(io::Error::other("the node hung up before it answered"));
        }
        let answer: Value = serde_json::from_str(&line).map_err(io::Error::other)?;
        if let Some(id) = answer["id"].as_u64().filter(|id| wanted.contains(id)) {
            answers.insert(id, answer);
        }
    }
    Ok(answers)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{Look, Swarm, overview, team, typed};

    /// A look 72 columns wide, without colour, at 17:00 on Thursday
    /// 12 November 2026, in UTC.
    fn look() -> Look {
        Look {
            width: 72,
            colour: false,
            // UTC as jiff holds it, since a build has no zone database.
            now: "2026-11-12T17:00:00Z"
                .parse::<jiff::Timestamp>()
                .expect("a time")
                .to_zoned(jiff::tz::TimeZone::UTC),
        }
    }

    /// A team record keyed by `address`.
    fn team_of(address: &str, title: &str, within: Option<&str>, fields: &Value) -> Value {
        let mut team = json!({
            "key": format!("team:{address}"),
            "kind": "team",
            "title": title,
            "within": within,
            "bridges": [],
            "liaisons": [],
            "landed": null,
        });
        if let (Some(team), Some(fields)) = (team.as_object_mut(), fields.as_object()) {
            team.extend(fields.clone());
        }
        team
    }

    /// A session record `id`, titled `title`, standing as `state`, its
    /// address its id's upper case without the `s-`.
    fn session(id: &str, title: &str, state: &str) -> Value {
        let address = id.trim_start_matches("s-").to_uppercase();
        json!({"key": format!("session:{id}"), "id": id, "address": address,
               "title": title, "state": state})
    }

    /// The harbour's swarm: Harbour with Kelp within it, Reef beside, and
    /// one session on its own.
    pub(super) fn swarm() -> Swarm {
        let mut asking = session("s-ask", "Kelp's question", "idle");
        asking["wants"] = json!({"kind": "question", "said": "Which tide?"});
        let mut paused = session("s-pause", "Paused one", "idle");
        paused["presence"] = json!({"paused": true});
        Swarm {
            realm: "work".to_owned(),
            teams: vec![
                team_of(
                    "REEF01",
                    "Reef",
                    None,
                    &json!({"worst": "moving", "members": ["s-reef"],
                           "counts": {"needs-you": 0, "stuck": 0, "moving": 1, "quiet": 0}}),
                ),
                team_of(
                    "HARBOR",
                    "Harbour",
                    None,
                    &json!({"worst": "needs-you", "lead": "s-lead",
                           "members": ["s-lead", "s-pause", "s-busy"],
                           "counts": {"needs-you": 0, "stuck": 1, "moving": 1, "quiet": 1},
                           "bridges": [{"repo": "skiff", "teams": ["REEF01"]}],
                           "landed": {"what": "Hear the Tide", "ref": "skiff#12",
                                      "at": "2026-11-12T15:54:00Z", "session": "s-busy"}}),
                ),
                team_of(
                    "KELP01",
                    "Kelp",
                    Some("HARBOR"),
                    &json!({"worst": "needs-you", "members": ["s-ask"],
                           "counts": {"needs-you": 1, "stuck": 0, "moving": 0, "quiet": 0},
                           "liaisons": [{"session": "s-ask", "teams": ["REEF01"]}]}),
                ),
            ],
            sessions: vec![
                session("s-lead", "Harbour's lead", "idle"),
                paused,
                {
                    let mut busy = session("s-busy", "Busy one", "busy");
                    busy["step"] = json!("running the tests");
                    busy
                },
                asking,
                session("s-reef", "Reef's one", "busy"),
                session("s-alone", "On its own", "busy"),
                session("s-gone", "Gone", "ended"),
            ],
        }
    }

    #[test]
    fn the_overview_lists_every_team_worst_first_nested_with_their_counts() {
        let shown = overview(&swarm(), &look());
        let expected = [
            "work · 6 running · 1 needs you · 1 stuck",
            "",
            "                                          need you  stuck  moving  quiet",
            "Harbour                                                 1       1      1",
            "  Kelp                                           1",
            "Reef                                                            1",
            "on their own                                                    1",
            "",
            "see a team by any part of its name: mycelium --realm work swarm harbour",
        ];
        assert_eq!(
            shown.lines().collect::<Vec<_>>(),
            expected,
            "teams needing you first, nested, counts under their header:\n{shown}"
        );
    }

    #[test]
    fn a_team_says_its_lead_landing_teams_within_sessions_and_meetings() {
        let shown = team(&swarm(), "harb", &look()).expect("Harbour is found by its name");
        let expected = [
            "Harbour · led by Harbour's lead",
            "landed Hear the Tide · skiff#12 · 15:54",
            "",
            "                                          need you  stuck  moving  quiet",
            "Kelp                                             1",
            "",
            "stuck      Paused one                      paused",
            "moving     Busy one                        running the tests",
            "quiet      Harbour's lead",
            "",
            "works in skiff beside Reef",
        ];
        assert_eq!(
            shown.lines().collect::<Vec<_>>(),
            expected,
            "one team, whole:\n{shown}"
        );
    }

    #[test]
    fn a_team_within_says_so_and_its_liaisons_are_named() {
        let shown = team(&swarm(), "KELP01", &look()).expect("Kelp is found by its address");
        let expected = [
            "Kelp · within Harbour",
            "",
            "needs you  Kelp's question                 Which tide?",
            "",
            "Kelp's question also hears Reef",
        ];
        assert_eq!(
            shown.lines().collect::<Vec<_>>(),
            expected,
            "within, and its liaison:\n{shown}"
        );
    }

    #[test]
    fn a_name_matching_none_or_several_is_said_plainly() {
        assert_eq!(
            team(&swarm(), "tide", &look()),
            Err("no team is named tide".to_owned()),
            "none"
        );
        assert_eq!(
            team(&swarm(), "r", &look()),
            Err("r could be reef or harbour".to_owned()),
            "several, each as it's typed, as the node lists them"
        );
    }

    /// Teams titled as people title them: two sharing Harbour, one a
    /// single word.
    fn titled_swarm() -> Swarm {
        let counted = |worst: &str, moving: u64, quiet: u64| {
            json!({"worst": worst, "members": [],
                   "counts": {"needs-you": 0, "stuck": 0, "moving": moving, "quiet": quiet}})
        };
        Swarm {
            realm: "work".to_owned(),
            teams: vec![
                team_of(
                    "DEPLOY",
                    "Deploy v3: How Harbour Ships",
                    None,
                    &counted("quiet", 0, 1),
                ),
                team_of(
                    "HARBR2",
                    "Harbour Part Deux: Closures on MicroVMs",
                    None,
                    &counted("moving", 2, 0),
                ),
                team_of("REEF01", "Reef", None, &counted("moving", 1, 0)),
            ],
            sessions: Vec::new(),
        }
    }

    #[test]
    fn each_team_leads_with_its_short_name_and_the_list_says_how_to_type_one() {
        let shown = overview(&titled_swarm(), &look());
        let expected = [
            "work · 0 running",
            "",
            "                                          need you  stuck  moving  quiet",
            "part-deux  Harbour Part Deux: Closures…                         2",
            "Reef                                                            1",
            "deploy     Deploy v3: How Harbour Ships                                1",
            "",
            "see a team by any part of its name: mycelium --realm work swarm part-deux",
        ];
        assert_eq!(
            shown.lines().collect::<Vec<_>>(),
            expected,
            "short names first, titles after them, Reef's own name once:\n{shown}"
        );
        assert_eq!(
            typed(&titled_swarm()),
            ["part-deux", "reef", "deploy"],
            "what a shell completes, in the list's order"
        );
    }

    #[test]
    fn a_short_name_finds_its_team_and_several_are_offered_by_theirs() {
        let swarm = titled_swarm();
        let found = team(&swarm, "Part-Deux", &look()).expect("found by its short name");
        assert!(
            found.starts_with("Harbour Part Deux"),
            "the team it names: {found}"
        );
        assert_eq!(
            team(&swarm, "harbour", &look()),
            Err("harbour could be deploy or part-deux".to_owned()),
            "each it could be, as it's typed"
        );
        let words = team(&swarm, "part deux", &look()).expect("found by its words");
        assert!(
            words.starts_with("Harbour Part Deux"),
            "a short name's words, spaced as typed, find it too: {words}"
        );
    }

    #[test]
    fn a_long_name_is_cut_with_an_ellipsis_and_counts_keep_their_columns() {
        let mut swarm = swarm();
        swarm.teams[0]["title"] =
            json!("A reef whose name runs far past every column it was given");
        let shown = overview(&swarm, &look());
        assert!(
            shown
                .lines()
                .any(|line| line
                    == "reef  A reef whose name runs far past e…                        1"),
            "cut to its room after its short name, its count still under moving:\n{shown}"
        );
    }

    #[test]
    fn times_say_the_day_once_they_are_not_today() {
        let look = look();
        assert_eq!(
            (
                look.when("2026-11-12T15:54:00Z"),
                look.when("2026-11-10T09:05:00Z"),
                look.when("2026-10-30T09:05:00Z"),
            ),
            (
                Some("15:54".to_owned()),
                Some("Tue 09:05".to_owned()),
                Some("30 Oct".to_owned())
            ),
            "today's time, this week's weekday, else the day"
        );
    }

    #[test]
    fn colour_marks_what_needs_you_and_what_is_stuck() {
        let mut look = look();
        look.colour = true;
        let shown = overview(&swarm(), &look);
        assert!(
            shown.contains("\x1b[33m1 needs you\x1b[0m")
                && shown.contains("\x1b[31m1 stuck\x1b[0m"),
            "needing you in yellow, stuck in red: {shown:?}"
        );
    }
}
