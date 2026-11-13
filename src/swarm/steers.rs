//! Steering the swarm as its person: saying something to a team, which
//! its lead hears, or to one session, and pausing or resuming a team's
//! sessions, those of the teams within it too, or one session's.
//!
//! Each is a steer the node takes to wherever its session runs, from the
//! person, so the harness reads it as the person's own words rather than
//! a peer's. A team is named as the swarm's overview names it; a session
//! by its address, or by its name when no team's matches.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::{Standing, Swarm, address, asked, either, listed, names, short_names, title};

/// Who a steer is from: the person, as the harness's steers name them.
const FROM: &str = "person";

/// What the person does to the swarm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    /// Says these words.
    Say(String),
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

/// The team or session `asked` names: a team by its address or its short
/// name, then a living session by its address, then a team by a part of
/// its name, then a living session by a part of its. Several are each
/// named as they're typed, a team by its short name, so one can be
/// picked.
fn named<'a>(swarm: &'a Swarm, asked: &str) -> Result<Named<'a>, String> {
    let living = || {
        swarm
            .sessions
            .iter()
            .filter(|session| Standing::of(session).is_some())
    };
    let shorts = short_names(&swarm.teams);
    if let Some(team) = swarm.teams.iter().find(|team| {
        address(team).eq_ignore_ascii_case(asked)
            || shorts
                .get(address(team))
                .is_some_and(|short| short.eq_ignore_ascii_case(asked))
    }) {
        return Ok(Named::Team(team));
    }
    if let Some(session) = living().find(|session| {
        session["address"]
            .as_str()
            .is_some_and(|address| address.eq_ignore_ascii_case(asked))
    }) {
        return Ok(Named::Session(session));
    }
    let lower = asked.to_lowercase();
    let matches = |record: &&Value| title(record).to_lowercase().contains(&lower);
    let teams: Vec<&Value> = swarm.teams.iter().filter(matches).collect();
    let mut found: Vec<Named<'_>> = if teams.is_empty() {
        living().filter(matches).map(Named::Session).collect()
    } else {
        teams.into_iter().map(Named::Team).collect()
    };
    if found.len() == 1
        && let Some(one) = found.pop()
    {
        return Ok(one);
    }
    match found.as_slice() {
        [] => Err(format!("nothing in the swarm is named {asked}")),
        many => {
            let typed: Vec<&str> = many
                .iter()
                .map(|one| match one {
                    Named::Team(record) => names::typed(record, &shorts),
                    Named::Session(record) => title(record),
                })
                .collect();
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
        name: title(session).to_owned(),
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
        Act::Say(words) => json!({"text": words, "from": FROM}),
        Act::Pause => json!({"act": "pause", "from": FROM}),
        Act::Resume => json!({"act": "resume", "from": FROM}),
    };
    let changes = |session: &Value| match act {
        Act::Say(_) => true,
        Act::Pause => !paused(session),
        Act::Resume => paused(session),
    };
    let (team, sessions) = match named(swarm, asked)? {
        Named::Session(session) if changes(session) => (None, vec![aimed(session, &steer)]),
        Named::Session(session) => {
            let name = title(session);
            return Err(match act {
                Act::Resume => format!("{name} isn't paused"),
                Act::Pause | Act::Say(_) => format!("{name} is paused already"),
            });
        }
        Named::Team(team) => {
            let name = title(team);
            let sessions: Vec<Aimed> = match act {
                Act::Say(_) => team["lead"]
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
                    Act::Say(_) => format!("{name} has no lead to say it to"),
                    Act::Pause => format!("every session of {name} is paused already"),
                    Act::Resume => format!("none of {name}'s sessions is paused"),
                });
            }
            (Some(name.to_owned()), sessions)
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
            Err(why) => missed.push(format!("{} wasn't told: {why}", aimed.name)),
        }
    }
    let mut lines = Vec::new();
    if !reached.is_empty() {
        let mut line = format!(
            "{} {}",
            match aim.act {
                Act::Say(_) => "said to",
                Act::Pause => "paused",
                Act::Resume => "resumed",
            },
            listed(&reached)
        );
        if let Some(team) = &aim.team {
            let _ = match aim.act {
                Act::Say(_) => write!(line, ", who leads {team}"),
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
mod tests {
    use serde_json::json;

    use super::{Act, Aim, Aimed, aim, told};
    use crate::swarm::tests::swarm;

    /// The addresses `aim` tells, in order.
    fn addresses(aim: &Aim) -> Vec<&str> {
        aim.sessions
            .iter()
            .map(|aimed| aimed.address.as_str())
            .collect()
    }

    #[test]
    fn words_to_a_team_go_to_its_lead_from_the_person() {
        let aimed = aim(&swarm(), Act::Say("Ship it".to_owned()), "harbour").expect("aimed");
        assert_eq!(
            aimed.sessions,
            [Aimed {
                address: "LEAD".to_owned(),
                name: "Harbour's lead".to_owned(),
                steer: json!({"text": "Ship it", "from": "person"}),
            }],
            "its lead is told, as the person"
        );
        assert_eq!(
            told(&aimed, &[Ok("skiff".to_owned())]),
            Ok("said to Harbour's lead, who leads Harbour".to_owned()),
            "and says who heard it"
        );
        assert_eq!(
            aim(&swarm(), Act::Say("Hello".to_owned()), "reef01"),
            Err("Reef has no lead to say it to".to_owned()),
            "a team with no lead is said to be one"
        );
    }

    #[test]
    fn a_pause_reaches_every_session_of_a_team_and_those_within_not_paused_already() {
        let aimed = aim(&swarm(), Act::Pause, "Harbour").expect("aimed");
        assert_eq!(
            addresses(&aimed),
            ["LEAD", "BUSY", "ASK"],
            "its own, then Kelp's within it, the paused one left"
        );
        assert_eq!(
            aimed.sessions[0].steer,
            json!({"act": "pause", "from": "person"}),
            "each told to pause, as the person"
        );
        assert_eq!(
            told(
                &aimed,
                &[
                    Ok("skiff".to_owned()),
                    Err("no session goes by BUSY now".to_owned()),
                    Ok("skiff".to_owned()),
                ]
            ),
            Err("paused Harbour's lead and Kelp's question, in Harbour\n\
                 Busy one wasn't told: no session goes by BUSY now"
                .to_owned()),
            "one not told is said, and why"
        );
    }

    #[test]
    fn a_resume_reaches_only_the_paused() {
        let aimed = aim(&swarm(), Act::Resume, "harbour").expect("aimed");
        assert_eq!(addresses(&aimed), ["PAUSE"], "only the paused one");
        assert_eq!(
            told(&aimed, &[Ok("skiff".to_owned())]),
            Ok("resumed Paused one, in Harbour".to_owned()),
            "said back by its name"
        );
        assert_eq!(
            aim(&swarm(), Act::Resume, "REEF01"),
            Err("none of Reef's sessions is paused".to_owned()),
            "a team with none paused says so"
        );
    }

    #[test]
    fn one_session_goes_by_its_address_or_its_name() {
        let aimed = aim(&swarm(), Act::Say("Hi".to_owned()), "alone").expect("aimed");
        assert_eq!(
            (addresses(&aimed), &aimed.team),
            (vec!["ALONE"], &None),
            "by its address"
        );
        assert_eq!(
            told(&aimed, &[Ok("skiff".to_owned())]),
            Ok("said to On its own".to_owned()),
            "said back by its name alone"
        );
        let busy = aim(&swarm(), Act::Pause, "busy one").expect("aimed");
        assert_eq!(addresses(&busy), ["BUSY"], "by a part of its name");
        assert_eq!(
            aim(&swarm(), Act::Pause, "paused one"),
            Err("Paused one is paused already".to_owned()),
            "a pause that changes nothing is refused"
        );
    }

    #[test]
    fn a_name_matching_nothing_or_several_is_said_plainly() {
        assert_eq!(
            aim(&swarm(), Act::Pause, "tide"),
            Err("nothing in the swarm is named tide".to_owned()),
            "none"
        );
        assert_eq!(
            aim(&swarm(), Act::Pause, "one"),
            Err("one could be Paused one, Busy one or Reef's one".to_owned()),
            "several living, the ended one left out"
        );
        assert_eq!(
            aim(&swarm(), Act::Pause, "e"),
            Err("e could be reef or kelp".to_owned()),
            "several teams, each by its short name"
        );
    }
}
