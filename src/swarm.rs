//! The swarm as a person reads it in a terminal: every team, worst first
//! and nested as they are, then one team's own sessions when asked, from
//! the node's `team` and `session` records.
//!
//! Each team is a line saying how many of its sessions need their person,
//! are stuck, move or wait, under one header, so the eye runs down a
//! column rather than reading words on every line. A team's name, never
//! its address, says which it is, and a count of none is left blank.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

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

/// How a session stands, as a team counts it, worst first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Standing {
    NeedsYou,
    Stuck,
    Moving,
    Quiet,
}

impl Standing {
    /// Each, in the order the header says them.
    const EACH: [Self; 4] = [Self::NeedsYou, Self::Stuck, Self::Moving, Self::Quiet];

    /// The word a team record counts it by.
    const fn counted(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs-you",
            Self::Stuck => "stuck",
            Self::Moving => "moving",
            Self::Quiet => "quiet",
        }
    }

    /// Its column's header, which is its column's width too.
    const fn header(self) -> &'static str {
        match self {
            Self::NeedsYou => "need you",
            Self::Stuck => "stuck",
            Self::Moving => "moving",
            Self::Quiet => "quiet",
        }
    }

    /// The word a session's line says it with.
    const fn said(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs you",
            Self::Stuck => "stuck",
            Self::Moving => "moving",
            Self::Quiet => "quiet",
        }
    }

    fn from_counted(word: &str) -> Option<Self> {
        Self::EACH.into_iter().find(|each| each.counted() == word)
    }

    /// How a session record stands, or none for one that has ended.
    fn of(session: &Value) -> Option<Self> {
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
    const fn colour(self) -> &'static str {
        match self {
            Self::NeedsYou => YELLOW,
            Self::Stuck => RED,
            Self::Moving => "",
            Self::Quiet => FAINT,
        }
    }
}

const BOLD: &str = "\x1b[1m";
const FAINT: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const RESET: &str = "\x1b[0m";

/// The space between columns.
const GAP: &str = "  ";

/// How far each team in is set in from the one it is within.
const STEP: usize = 2;

/// The narrowest a name is cut to, however narrow the terminal.
const NARROWEST: usize = 16;

impl Look {
    /// `text` in `colour`, where it is drawn in colour.
    fn paint(&self, colour: &str, text: &str) -> String {
        if self.colour && !colour.is_empty() && !text.trim().is_empty() {
            format!("{colour}{text}{RESET}")
        } else {
            text.to_owned()
        }
    }

    /// When `at`, an RFC 3339 instant, was: its time today, its weekday
    /// and time this week, else its day.
    fn when(&self, at: &str) -> Option<String> {
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
fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{}…", kept.trim_end())
}

/// `text` padded with spaces to `width` characters.
fn padded(text: &str, width: usize) -> String {
    let short = width.saturating_sub(text.chars().count());
    format!("{text}{}", " ".repeat(short))
}

/// How wide the counts' columns are together, their gaps before each.
fn counts_width() -> usize {
    Standing::EACH
        .iter()
        .map(|each| GAP.len() + each.header().len())
        .sum()
}

/// A team's or a session's name as a person reads it.
fn title(record: &Value) -> &str {
    record["title"]
        .as_str()
        .or_else(|| record["working_on"].as_str())
        .unwrap_or("a session without a name")
}

/// The address a team record is keyed by.
fn address(team: &Value) -> &str {
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

/// One row of counts under the header: `name` set in by `depth`, then a
/// column for each standing, a count of none left blank.
fn row(look: &Look, depth: usize, name: &str, counts: [u64; 4], strong: bool) -> String {
    let room = look
        .width
        .saturating_sub(counts_width() + depth * STEP)
        .max(NARROWEST);
    let name = padded(&fit(name, room), room);
    let mut line = format!(
        "{}{}",
        " ".repeat(depth * STEP),
        look.paint(if strong { BOLD } else { "" }, &name)
    );
    for (each, count) in Standing::EACH.into_iter().zip(counts) {
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

/// The header over the counts, lined up with them.
fn header(look: &Look, depth: usize) -> String {
    let room = look
        .width
        .saturating_sub(counts_width() + depth * STEP)
        .max(NARROWEST);
    let mut line = " ".repeat(room + depth * STEP);
    for each in Standing::EACH {
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
fn counts_of(team: &Value) -> [u64; 4] {
    Standing::EACH.map(|each| count(team, each))
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
    let mut lines = vec![look.paint(BOLD, &head), String::new(), header(look, 0)];
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
    let mut passed = BTreeSet::new();
    nest(&mut lines, look, &within, "", 0, &mut passed);
    let alone = alone(swarm);
    if !alone.is_empty() {
        let counts = Standing::EACH.map(|each| {
            let many = alone
                .iter()
                .filter(|one| Standing::of(one) == Some(each))
                .count();
            u64::try_from(many).unwrap_or(u64::MAX)
        });
        lines.push(row(look, 0, "on their own", counts, false));
    }
    lines.push(String::new());
    lines.push(look.paint(FAINT, "mycelium swarm <team> shows one team's sessions"));
    lines.join("\n")
}

/// The teams within `above` as rows set in by `depth`, each followed by
/// those within it, never one already `passed`.
fn nest<'a>(
    lines: &mut Vec<String>,
    look: &Look,
    within: &BTreeMap<&str, Vec<&'a Value>>,
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
        lines.push(row(look, depth, title(team), counts_of(team), depth == 0));
        nest(lines, look, within, address(team), depth + 1, passed);
    }
}

/// The team `asked` names, by its address or a part of its name, or why
/// none or several are.
fn named<'a>(swarm: &'a Swarm, asked: &str) -> Result<&'a Value, String> {
    if let Some(team) = swarm
        .teams
        .iter()
        .find(|team| address(team).eq_ignore_ascii_case(asked))
    {
        return Ok(team);
    }
    let lower = asked.to_lowercase();
    let found: Vec<&Value> = swarm
        .teams
        .iter()
        .filter(|team| title(team).to_lowercase().contains(&lower))
        .collect();
    match found.as_slice() {
        [one] => Ok(*one),
        [] => Err(format!("no team is named {asked}")),
        many => Err(format!(
            "{asked} could be {}",
            listed(&many.iter().map(|team| title(team)).collect::<Vec<_>>())
        )),
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
        lines.extend([String::new(), header(look, 0)]);
        for other in inner {
            lines.push(row(look, 0, title(other), counts_of(other), false));
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
    let doing = match standing {
        Standing::NeedsYou => session["wants"]["said"].as_str(),
        Standing::Stuck => Some("paused"),
        Standing::Moving | Standing::Quiet => session["step"].as_str(),
    };
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
fn listed(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
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
    let mut writing = UnixStream::connect(socket).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("no node answers at {}: {error}", socket.display()),
        )
    })?;
    writing.set_read_timeout(Some(within))?;
    writing.set_write_timeout(Some(within))?;
    let mut reading = BufReader::new(writing.try_clone()?);
    for ask in [
        json!({"id": 0, "ask": "hello"}),
        json!({"id": 1, "ask": "list", "kind": "team"}),
        json!({"id": 2, "ask": "list", "kind": "session"}),
    ] {
        writeln!(writing, "{ask}")?;
    }
    let mut answers: BTreeMap<u64, Value> = BTreeMap::new();
    let mut line = String::new();
    while !(answers.contains_key(&1) && answers.contains_key(&2)) {
        line.clear();
        if reading.read_line(&mut line)? == 0 {
            return Err(io::Error::other("the node hung up before it answered"));
        }
        let answer: Value = serde_json::from_str(&line).map_err(io::Error::other)?;
        if let Some(id) = answer["id"].as_u64() {
            answers.insert(id, answer);
        }
    }
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

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{Look, Swarm, overview, team};

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

    /// A session record `id`, titled `title`, standing as `state`.
    fn session(id: &str, title: &str, state: &str) -> Value {
        json!({"key": format!("session:{id}"), "id": id, "title": title, "state": state})
    }

    /// The harbour's swarm: Harbour with Kelp within it, Reef beside, and
    /// one session on its own.
    fn swarm() -> Swarm {
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
            "mycelium swarm <team> shows one team's sessions",
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
            Err("r could be Reef and Harbour".to_owned()),
            "several, as the node lists them"
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
                    == "A reef whose name runs far past every c…                        1"),
            "cut to its room, its count still under moving:\n{shown}"
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
