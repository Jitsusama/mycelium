//! Steering the swarm as its person: saying something to a team, which
//! its lead hears, or to one session, and pausing or resuming a team's
//! sessions, those of the teams within it too, or one session's.
//!
//! Each is a steer the node takes to wherever its session runs, from the
//! person, so the harness reads it as the person's own words rather than
//! a peer's. A team is named as the swarm's overview names it; a session
//! by its address, or by its name when no team's matches.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::{
    Held, Standing, Swarm, address, asked, either, held, listed, names, shorts_of, title,
    told_apart, unmarked,
};

/// Who a steer is from: the person, as the harness's steers name them.
const FROM: &str = "person";

/// What the person does to the swarm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// Says these words: after the turn under way, or `now`, into it, as
    /// Joel chose for words to a working session (2026-11-12, "yes").
    Say {
        /// What is said.
        words: String,
        /// Whether it goes into the turn under way rather than after it.
        now: bool,
    },
    /// Pauses, so a session finishes its turn and waits.
    Pause,
    /// Resumes a paused session.
    Resume,
}

/// One session an act is told to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aimed {
    /// Its address, which the node steers it by.
    pub address: String,
    /// Its name, as it is said back.
    pub name: String,
    /// What it is told, as a steer without its time.
    pub steer: Value,
}

/// Where an act goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Aim {
    /// What it does.
    pub act: Act,
    /// The team it was named by, by its name, or none for one session.
    pub team: Option<String>,
    /// The sessions told, in the order they are said back.
    pub sessions: Vec<Aimed>,
}

/// What `asked` names: a team, or one session.
enum Named<'a> {
    Team(&'a Value),
    Session(&'a Value),
}

/// Whether a session says it is paused.
fn paused(session: &Value) -> bool {
    session["presence"]["paused"] == json!(true)
}

/// What a steer typed with `asked` does by address among the living
/// sessions, as [`held`] judges: left to a team where every session
/// holding it leads the team going by it, as an orchestrator's team goes
/// by its lead's; taken by the first where none does; refused where they
/// disagree. A team is keyed by what its members say orchestrates them,
/// and a session record is a peer's presence, so a peer could key a team
/// by a working session's address and lead it with a twin of that
/// session, or put a twin that leads nothing beside a team's lead; were
/// either side to win, the steer could reach the peer's session. Pages
/// keep short names by the same rule, so what they draw steers as shown.
pub(super) fn session_by_address<'a>(swarm: &'a Swarm, asked: &str) -> Held<&'a Value> {
    let living: Vec<&Value> = swarm
        .sessions
        .iter()
        .filter(|session| Standing::of(session).is_some())
        .collect();
    let said = |session: &&'a Value| {
        (
            session["address"].as_str().unwrap_or_default(),
            session["id"].as_str(),
        )
    };
    held(&swarm.teams, &living, said, asked).map(|session| *session)
}

/// Why a steer to `asked` is refused where the living sessions holding
/// it, `holders`, disagree on leading the team going by it: each is
/// named by its title and its id, as twins may be titled alike, so a
/// person can tell which is whose.
pub(super) fn refused(asked: &str, holders: &[&Value]) -> String {
    // Each is a peer's text, so each is held to one line.
    let named: Vec<String> = holders
        .iter()
        .map(|session| {
            format!(
                "{} ({})",
                unmarked(title(session)),
                unmarked(session["id"].as_str().unwrap_or("no id"))
            )
        })
        .collect();
    format!(
        "a steer to {} is refused: {} each say that address, and only some lead the team \
         going by it",
        unmarked(asked),
        listed(&named.iter().map(String::as_str).collect::<Vec<_>>())
    )
}

/// Whether a steer naming `asked` goes to `team` alone, as `named`
/// reads it with `shorts` as [`shorts_of`] gives them, and to no session
/// or other team.
pub(super) fn reaches(
    swarm: &Swarm,
    asked: &str,
    team: &Value,
    shorts: &BTreeMap<String, String>,
) -> bool {
    matches!(named(swarm, asked, shorts), Ok(Named::Team(found)) if std::ptr::eq(found, team))
}

/// The team or session `asked` names: a living session by its address,
/// as [`session_by_address`] finds it, or why a steer by it is refused
/// there, then a team by its address, then a team by its short name,
/// then a team by a part of its name, then a living session by a part
/// of its, with `shorts` as [`shorts_of`] gives them. An address comes
/// before a short name because a peer picks its team's short name, and
/// could otherwise pick another's address and take the words meant for
/// it, or clean to it and hide it. A team whose address is `asked` in
/// its own case comes before one keyed alike but for case. Several are
/// each named as they're typed, a team by its short name, so one can be
/// picked; two teams whose short names clean alike are several, as
/// `picked` finds them, so neither takes a steer meant for the other.
fn named<'a>(
    swarm: &'a Swarm,
    asked: &str,
    shorts: &BTreeMap<String, String>,
) -> Result<Named<'a>, String> {
    let living = || {
        swarm
            .sessions
            .iter()
            .filter(|session| Standing::of(session).is_some())
    };
    // A short name's words may be typed apart, as `part deux`. Short
    // names and titles are matched as the swarm's pages draw them,
    // cleaned on both sides, as `picked` matches them, so the name a page
    // shows steers the team it names.
    let joined = unmarked(&asked.split_whitespace().collect::<Vec<_>>().join("-"));
    match session_by_address(swarm, asked) {
        Held::Left => {}
        Held::One(session) => return Ok(Named::Session(session)),
        Held::Split(holders) => return Err(refused(asked, &holders)),
    }
    let exactly: Vec<Named<'_>> = swarm
        .teams
        .iter()
        .filter(|team| address(team) == asked)
        .map(Named::Team)
        .collect();
    let by_address: Vec<Named<'_>> = if exactly.is_empty() {
        swarm
            .teams
            .iter()
            .filter(|team| address(team).eq_ignore_ascii_case(asked))
            .map(Named::Team)
            .collect()
    } else {
        exactly
    };
    let whole: Vec<Named<'_>> = if by_address.is_empty() {
        swarm
            .teams
            .iter()
            .filter(|team| {
                shorts
                    .get(address(team))
                    .is_some_and(|short| unmarked(short).eq_ignore_ascii_case(&joined))
            })
            .map(Named::Team)
            .collect()
    } else {
        by_address
    };
    let mut found: Vec<Named<'_>> = if whole.is_empty() {
        let lower = unmarked(asked).to_lowercase();
        let matches = |record: &&Value| unmarked(title(record)).to_lowercase().contains(&lower);
        let teams: Vec<&Value> = swarm.teams.iter().filter(matches).collect();
        if teams.is_empty() {
            living().filter(matches).map(Named::Session).collect()
        } else {
            teams.into_iter().map(Named::Team).collect()
        }
    } else {
        whole
    };
    if found.len() == 1
        && let Some(one) = found.pop()
    {
        return Ok(one);
    }
    match found.as_slice() {
        [] => Err(format!("nothing in the swarm is named {asked}")),
        many => {
            // Each is a peer's text, so each is held to one line.
            // Two drawn alike are each given their address too, where
            // typing it steers the one it names. An act's word or `team`
            // isn't, since `swarm` reads the first act it finds as the
            // act, so typed before the act it would do another, and a
            // team's address is given only where a steer by it is left to
            // a team, a session's only where it takes that session.
            let typeable =
                |address: &str| crate::forage::is_address(address) && names::one_word(address);
            let typed = told_apart(
                &many
                    .iter()
                    .map(|one| match one {
                        Named::Team(record) => (
                            unmarked(names::typed(record, shorts)),
                            Some(address(record)).filter(|address| {
                                typeable(address)
                                    && matches!(session_by_address(swarm, address), Held::Left)
                            }),
                        ),
                        Named::Session(record) => (
                            unmarked(title(record)),
                            record["address"].as_str().filter(|address| {
                                typeable(address)
                                    && matches!(
                                        session_by_address(swarm, address),
                                        Held::One(found) if found["id"] == record["id"]
                                    )
                            }),
                        ),
                    })
                    .collect::<Vec<_>>(),
            );
            let typed: Vec<&str> = typed.iter().map(String::as_str).collect();
            Err(format!("{asked} could be {}", either(&typed)))
        }
    }
}

/// A team's living sessions, then those of the teams within it, however
/// deep, each once.
fn team_sessions<'a>(swarm: &'a Swarm, team: &Value) -> Vec<&'a Value> {
    let mut teams = vec![address(team).to_owned()];
    let mut next = 0;
    while let Some(above) = teams.get(next).cloned() {
        next += 1;
        for inner in &swarm.teams {
            let inner = address(inner);
            if team_within(swarm, inner) == Some(above.as_str())
                && !teams.iter().any(|known| known == inner)
            {
                teams.push(inner.to_owned());
            }
        }
    }
    let mut seen = BTreeSet::new();
    teams
        .iter()
        .filter_map(|each| swarm.teams.iter().find(|team| address(team) == each))
        .filter_map(|team| team["members"].as_array())
        .flatten()
        .filter_map(Value::as_str)
        .filter(|id| seen.insert(*id))
        .filter_map(|id| {
            swarm
                .sessions
                .iter()
                .find(|session| session["id"] == json!(id))
        })
        .filter(|session| Standing::of(session).is_some())
        .collect()
}

/// The team the team at `address` is within, if it says.
fn team_within<'a>(swarm: &'a Swarm, at: &str) -> Option<&'a str> {
    swarm
        .teams
        .iter()
        .find(|team| address(team) == at)
        .and_then(|team| team["within"].as_str())
}

/// The session told, with what it's told.
fn aimed(session: &Value, steer: &Value) -> Aimed {
    Aimed {
        address: session["address"].as_str().unwrap_or_default().to_owned(),
        // A session's title is a peer's text, said back to the person.
        name: unmarked(title(session)),
        steer: steer.clone(),
    }
}

/// Where `act`, aimed at what `asked` names, goes: words to a team go to
/// its lead, and a pause or resume to each of its sessions it would
/// change, those of the teams within it included.
///
/// # Errors
///
/// When nothing, or more than one thing, goes by what was asked, or the
/// act would change none of the sessions it names.
pub fn aim(swarm: &Swarm, act: Act, asked: &str) -> Result<Aim, String> {
    let steer = match &act {
        Act::Say { words, now: false } => json!({"text": words, "from": FROM}),
        Act::Say { words, now: true } => json!({"text": words, "from": FROM, "now": true}),
        Act::Pause => json!({"act": "pause", "from": FROM}),
        Act::Resume => json!({"act": "resume", "from": FROM}),
    };
    let changes = |session: &Value| match act {
        Act::Say { .. } => true,
        Act::Pause => !paused(session),
        Act::Resume => paused(session),
    };
    let (team, sessions) = match named(swarm, asked, &shorts_of(swarm))? {
        Named::Session(session) if changes(session) => (None, vec![aimed(session, &steer)]),
        Named::Session(session) => {
            let name = unmarked(title(session));
            return Err(match act {
                Act::Resume => format!("{name} isn't paused"),
                Act::Pause | Act::Say { .. } => format!("{name} is paused already"),
            });
        }
        Named::Team(team) => {
            let name = unmarked(title(team));
            let sessions: Vec<Aimed> = match act {
                Act::Say { .. } => team["lead"]
                    .as_str()
                    .and_then(|lead| swarm.sessions.iter().find(|one| one["id"] == json!(lead)))
                    .filter(|lead| Standing::of(lead).is_some())
                    .map(|lead| aimed(lead, &steer))
                    .into_iter()
                    .collect(),
                Act::Pause | Act::Resume => team_sessions(swarm, team)
                    .into_iter()
                    .filter(|session| changes(session))
                    .map(|session| aimed(session, &steer))
                    .collect(),
            };
            if sessions.is_empty() {
                return Err(match act {
                    Act::Say { .. } => format!("{name} has no lead to say it to"),
                    Act::Pause => format!("every session of {name} is paused already"),
                    Act::Resume => format!("none of {name}'s sessions is paused"),
                });
            }
            (Some(name), sessions)
        }
    };
    Ok(Aim {
        act,
        team,
        sessions,
    })
}

/// Tells each session `aim` names, through the node at `socket`, giving
/// up on any read or write taking longer than `within`, the steer dated
/// `at`; each answer says the machine it was told on, or why it wasn't.
///
/// # Errors
///
/// When no node answers, or it answers in a way it shouldn't.
pub fn send(
    socket: &Path,
    within: Duration,
    aim: &Aim,
    at: &str,
) -> io::Result<Vec<Result<String, String>>> {
    let asks: Vec<Value> = aim
        .sessions
        .iter()
        .map(|aimed| {
            let mut steer = aimed.steer.clone();
            steer["at"] = at.into();
            json!({"ask": "steer", "address": aimed.address, "steer": steer})
        })
        .collect();
    let answers = asked(socket, within, &asks)?;
    Ok((1_u64..)
        .take(asks.len())
        .map(|id| {
            let answer = answers.get(&id).unwrap_or(&Value::Null);
            answer["steered"]["machine"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| {
                    answer["error"]["message"]
                        .as_str()
                        .unwrap_or("the node answered neither that it was told nor why not")
                        .to_owned()
                })
        })
        .collect())
}

/// What was done, as said back: who was told, then each that wasn't and
/// why, given each session's answer in the order `aim` names them.
///
/// # Errors
///
/// When any session wasn't told, saying who was too.
pub fn told(aim: &Aim, answers: &[Result<String, String>]) -> Result<String, String> {
    let mut reached = Vec::new();
    let mut missed = Vec::new();
    for (aimed, answer) in aim.sessions.iter().zip(answers) {
        match answer {
            Ok(_) => reached.push(aimed.name.as_str()),
            // The node's reason may carry another machine's words.
            Err(why) => missed.push(format!("{} wasn't told: {}", aimed.name, unmarked(why))),
        }
    }
    let mut lines = Vec::new();
    if !reached.is_empty() {
        let mut line = format!(
            "{} {}",
            match aim.act {
                Act::Say { now: false, .. } => "said to",
                Act::Say { now: true, .. } => "said now to",
                Act::Pause => "paused",
                Act::Resume => "resumed",
            },
            listed(&reached)
        );
        if let Some(team) = &aim.team {
            let _ = match aim.act {
                Act::Say { .. } => write!(line, ", who leads {team}"),
                Act::Pause | Act::Resume => write!(line, ", in {team}"),
            };
        }
        lines.push(line);
    }
    let all = missed.is_empty();
    let said = lines
        .into_iter()
        .chain(missed)
        .collect::<Vec<_>>()
        .join("\n");
    if all { Ok(said) } else { Err(said) }
}

#[cfg(test)]
mod tests;
