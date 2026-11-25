//! The swarm's teams, worked out from the sessions the node already holds,
//! so a reader sees the swarm as its teams without reading every session
//! and working them out itself.
//!
//! An orchestrator and the sessions answering to it are a team, led by the
//! session working on the orchestrator's own quest, and a team whose lead
//! answers to another orchestrator sits within that one's team. Each says
//! how its sessions stand, worst first, and the worst of the teams within
//! it rolls up into its own, so a quiet team never hides a stuck one. A
//! repository two teams' sessions work in is a bridge between them, and a
//! session hearing another team's group beside its own is a liaison.
//!
//! Nothing here reads anything: it folds what the node holds whenever its
//! sessions change.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde_json::{Value, json};

use super::glyph::{self, Choosing};
use crate::record::{Session, State};

/// The kind of a team's records.
pub const KIND: &str = "team";

/// How a session stands as its team counts it, worst first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Attention {
    /// It waits on its person.
    NeedsYou,
    /// It can't go on by itself: paused, or parked by a hold.
    Stuck,
    /// It runs a turn.
    Moving,
    /// It waits for something to do.
    Quiet,
}

impl Attention {
    /// Each, in the order a team counts them.
    const EACH: [Self; 4] = [Self::NeedsYou, Self::Stuck, Self::Moving, Self::Quiet];

    /// How `session` stands, or nothing for one that has ended or whose
    /// machine is out of reach, which a team doesn't count.
    fn of(session: &Session, parked: &HashSet<String>) -> Option<Self> {
        match session.state {
            State::Ended | State::Unreachable => None,
            _ if session.wants.is_some() => Some(Self::NeedsYou),
            _ if parked.contains(&session.id) || paused(session) => Some(Self::Stuck),
            State::Busy => Some(Self::Moving),
            State::Idle => Some(Self::Quiet),
        }
    }

    /// The word a record says it with.
    const fn word(self) -> &'static str {
        match self {
            Self::NeedsYou => "needs-you",
            Self::Stuck => "stuck",
            Self::Moving => "moving",
            Self::Quiet => "quiet",
        }
    }
}

/// Whether `session`'s presence says it is paused.
fn paused(session: &Session) -> bool {
    session
        .presence
        .get("paused")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The address of the quest `quest` names, its id's last part.
fn address_of(quest: &str) -> &str {
    quest.rsplit('-').next().unwrap_or(quest)
}

/// Each address a living session goes by beside its own, mapped to its
/// own, as the mesh reaches it by any of them. An orchestrator renamed
/// keeps its old address among its addresses, while the sessions it
/// spawned before still answer to the old one, so its team is one team
/// under its own address rather than two, one of them without a lead
/// (H2VYOB, once 37MNI3, 2026-11-14).
fn renamed<'a>(living: &[&'a Session]) -> BTreeMap<&'a str, &'a str> {
    let mut renamed = BTreeMap::new();
    for session in living {
        let others = session
            .presence
            .get("addresses")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|other| *other != session.address);
        for other in others {
            renamed.entry(other).or_insert(session.address.as_str());
        }
    }
    renamed
}

/// The name of the group an orchestrator's team hears.
fn group_of(team: &str) -> String {
    format!("orchestration.{}", team.to_lowercase())
}

/// The repositories `session` works in, by the groups it hears.
fn repos(session: &Session) -> impl Iterator<Item = &str> {
    session
        .groups
        .iter()
        .filter_map(|group| group.as_str().strip_prefix("repo."))
}

/// One team as it is being worked out.
#[derive(Default)]
struct Team<'a> {
    lead: Option<&'a Session>,
    members: Vec<&'a Session>,
}

/// Every team among `sessions`, by its orchestrator's address: when it was
/// last heard, in milliseconds since the epoch by `heard`, and its fields.
/// A team is titled as its orchestrator's quest is among `quests`, by
/// address, else as its lead's window is, and goes by the name in the
/// swarm that quest gives it. Sessions `parked` here count as stuck.
pub fn teams(
    sessions: &[Session],
    quests: &BTreeMap<String, Value>,
    parked: &HashSet<String>,
    heard: impl Fn(&str) -> i64,
) -> Vec<(String, i64, Value)> {
    let living: Vec<&Session> = sessions
        .iter()
        .filter(|session| Attention::of(session, parked).is_some())
        .collect();
    let renamed = renamed(&living);
    // In upper case, as the mesh reaches an address in either: a session
    // saying its orchestrator in another case answers to the same team,
    // and no two teams differ by case alone, so neither takes the words
    // typed in lower case for the other.
    let own = |address: &str| -> String {
        let address = address.to_ascii_uppercase();
        match renamed.get(address.as_str()) {
            Some(own) => own.to_ascii_uppercase(),
            None => address,
        }
    };
    let owned: Vec<(Option<String>, Option<String>)> = living
        .iter()
        .map(|session| {
            (
                session.orchestrated_by.as_deref().map(own),
                session.quest.as_deref().map(|quest| own(address_of(quest))),
            )
        })
        .collect();
    let mut teams: BTreeMap<&str, Team> = BTreeMap::new();
    for (session, (team, _)) in living.iter().zip(&owned) {
        if let Some(team) = team.as_deref() {
            teams.entry(team).or_default().members.push(session);
        }
    }
    for (session, (_, quest)) in living.iter().zip(&owned) {
        let Some(quest) = quest.as_deref() else {
            continue;
        };
        if let Some(team) = teams.get_mut(quest) {
            team.lead.get_or_insert(session);
            if session.orchestrated_by.is_none() {
                team.members.push(session);
            }
        }
    }
    let within: BTreeMap<&str, &str> = teams
        .iter()
        .filter_map(|(id, team)| {
            let above = own(team.lead?.orchestrated_by.as_deref()?);
            let (above, _) = teams.get_key_value(above.as_str())?;
            (above != id).then_some((*id, *above))
        })
        .collect();
    let worst = rolled_up(&teams, &within, parked);
    let choosing: Vec<Choosing> = teams
        .iter()
        .map(|(id, team)| Choosing {
            id,
            // A session's id is a UUID version 7, so the least sorts first
            // by when it began.
            began: team
                .lead
                .iter()
                .chain(&team.members)
                .map(|session| session.id.as_str())
                .min()
                .unwrap_or_default(),
            within: within.get(id).copied(),
        })
        .collect();
    let glyphs = glyph::glyphs(&choosing);
    teams
        .iter()
        .map(|(id, team)| {
            let mut fields = fields(id, team, &teams, (quests, parked), (&within, &worst));
            fields["glyph"] = glyphs.get(id).copied().into();
            let last = team
                .members
                .iter()
                .map(|member| heard(&member.at))
                .max()
                .unwrap_or_default();
            ((*id).to_owned(), last, fields)
        })
        .collect()
}

/// The worst of each team's own sessions and of every team within it.
fn rolled_up<'a>(
    teams: &BTreeMap<&'a str, Team>,
    within: &BTreeMap<&'a str, &'a str>,
    parked: &HashSet<String>,
) -> BTreeMap<&'a str, Attention> {
    let mut worst: BTreeMap<&str, Attention> = teams
        .iter()
        .filter_map(|(id, team)| {
            let own = team
                .members
                .iter()
                .filter_map(|member| Attention::of(member, parked))
                .min()?;
            Some((*id, own))
        })
        .collect();
    // Each team's worst climbs to every team above it, stopping short of
    // one it has already passed, so a loop of orchestrators ends.
    for (id, own) in worst.clone() {
        let mut passed = BTreeSet::from([id]);
        let mut at = id;
        while let Some(&above) = within.get(at) {
            if !passed.insert(above) {
                break;
            }
            let theirs = worst.entry(above).or_insert(own);
            *theirs = (*theirs).min(own);
            at = above;
        }
    }
    worst
}

/// What team `id`'s record says beside its key.
fn fields(
    id: &str,
    team: &Team,
    teams: &BTreeMap<&str, Team>,
    (quests, parked): (&BTreeMap<String, Value>, &HashSet<String>),
    (within, worst): (&BTreeMap<&str, &str>, &BTreeMap<&str, Attention>),
) -> Value {
    let quest = quests.get(id);
    let title = quest
        .and_then(|quest| quest.get("title")?.as_str())
        .map(str::to_owned)
        .or_else(|| team.lead.and_then(|lead| lead.title.clone()))
        .unwrap_or_else(|| id.to_owned());
    let swarm = quest.and_then(|quest| quest.get("swarm")?.as_str());
    let counts: serde_json::Map<String, Value> = Attention::EACH
        .iter()
        .map(|each| {
            let count = team
                .members
                .iter()
                .filter(|member| Attention::of(member, parked) == Some(*each))
                .count();
            (each.word().to_owned(), count.into())
        })
        .collect();
    json!({
        "title": title,
        "swarm": swarm,
        "lead": team.lead.map(|lead| lead.id.clone()),
        "within": within.get(id),
        "worst": worst.get(id).map(|worst| worst.word()),
        "counts": counts,
        "members": team.members.iter().map(|member| &member.id).collect::<Vec<_>>(),
        "bridges": bridges(id, team, teams),
        "liaisons": liaisons(id, team, teams),
        "landed": landed(team),
    })
}

/// Each repository team `id`'s sessions work in that another team's do
/// too, with those teams.
fn bridges(id: &str, team: &Team, teams: &BTreeMap<&str, Team>) -> Vec<Value> {
    let here: BTreeSet<&str> = team
        .members
        .iter()
        .flat_map(|member| repos(member))
        .collect();
    here.into_iter()
        .filter_map(|repo| {
            let also: Vec<&str> = teams
                .iter()
                .filter(|(other, _)| **other != id)
                .filter(|(_, other)| {
                    other
                        .members
                        .iter()
                        .any(|member| repos(member).any(|theirs| theirs == repo))
                })
                .map(|(other, _)| *other)
                .collect();
            (!also.is_empty()).then(|| json!({"repo": repo, "teams": also}))
        })
        .collect()
}

/// Each of team `id`'s sessions that hears another team's group too, with
/// those teams.
fn liaisons(id: &str, team: &Team, teams: &BTreeMap<&str, Team>) -> Vec<Value> {
    team.members
        .iter()
        .filter_map(|member| {
            let also: Vec<&str> = teams
                .keys()
                .filter(|other| **other != id)
                .filter(|other| {
                    let group = group_of(other);
                    member.groups.iter().any(|heard| heard.as_str() == group)
                })
                .copied()
                .collect();
            (!also.is_empty()).then(|| json!({"session": member.id, "teams": also}))
        })
        .collect()
}

/// The newest of what any of the team's sessions landed, and which did.
fn landed(team: &Team) -> Value {
    team.members
        .iter()
        .flat_map(|member| member.landings.iter().map(move |landing| (member, landing)))
        .max_by(|(_, one), (_, other)| one.at.cmp(&other.at))
        .map_or(Value::Null, |(member, landing)| {
            json!({
                "what": landing.what,
                "ref": landing.reference,
                "at": landing.at,
                "session": member.id,
            })
        })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use serde_json::{Value, json};

    use super::teams;
    use crate::group::Group;
    use crate::record::{Landing, Session, State, Want};

    /// A session `id` standing as `state`, answering to `orchestrator`, on
    /// `quest`, hearing `groups`.
    fn session(
        id: &str,
        state: State,
        (orchestrator, quest): (Option<&str>, Option<&str>),
        groups: &[&str],
    ) -> Session {
        Session {
            id: id.to_owned(),
            address: id.to_uppercase(),
            machine: "skiff".to_owned(),
            title: Some(format!("{id}'s window")),
            working_on: None,
            groups: groups
                .iter()
                .map(|group| Group::new(*group).expect("a group's name"))
                .collect(),
            state,
            step: None,
            since: None,
            orchestrated_by: orchestrator.map(str::to_owned),
            quest: quest.map(str::to_owned),
            wants: None,
            landings: Vec::new(),
            doing: None,
            events: Vec::new(),
            window: None,
            held: None,
            started_by: None,
            attach: None,
            host: None,
            conversation: None,
            at: "2026-11-12T22:00:00Z".to_owned(),
            presence: json!({}).into(),
        }
    }

    /// The teams of `sessions`, by id, titled as `titles` says, with
    /// `parked` parked.
    fn worked_out(
        sessions: &[Session],
        titles: &[(&str, &str)],
        parked: &[&str],
    ) -> BTreeMap<String, Value> {
        let quests = titles
            .iter()
            .map(|(id, title)| ((*id).to_owned(), json!({ "title": title })))
            .collect();
        let parked: HashSet<String> = parked.iter().map(|id| (*id).to_owned()).collect();
        teams(sessions, &quests, &parked, |_| 1)
            .into_iter()
            .map(|(id, _, fields)| (id, fields))
            .collect()
    }

    #[test]
    fn an_orchestrator_said_in_another_case_is_the_same_team() {
        // A peer saying its orchestrator in lower case would otherwise key a
        // team of its own beside Harbour's, taking what's typed in lower case.
        let sessions = [
            session(
                "lead",
                State::Idle,
                (None, Some("QEST-20261007-HARBOR")),
                &[],
            ),
            session("one", State::Busy, (Some("HARBOR"), None), &[]),
            session(
                "peer",
                State::Busy,
                (Some("harbor"), Some("QEST-x-harbor")),
                &[],
            ),
        ];
        let teams = worked_out(&sessions, &[("HARBOR", "Harbour")], &[]);
        assert_eq!(
            teams.keys().collect::<Vec<_>>(),
            ["HARBOR"],
            "one team, whatever the case its orchestrator is said in"
        );
        assert_eq!(
            (&teams["HARBOR"]["lead"], &teams["HARBOR"]["members"]),
            (&json!("lead"), &json!(["one", "peer", "lead"])),
            "led by the session on its quest, the peer only among its members"
        );
    }

    #[test]
    fn an_orchestrator_and_those_answering_it_are_a_team_titled_by_its_quest() {
        let sessions = [
            session(
                "lead",
                State::Idle,
                (None, Some("QEST-20261007-HARBOR")),
                &[],
            ),
            session("one", State::Busy, (Some("HARBOR"), None), &[]),
            session("two", State::Idle, (Some("HARBOR"), None), &[]),
            session("gone", State::Ended, (Some("HARBOR"), None), &[]),
            session("alone", State::Busy, (None, None), &[]),
        ];
        let teams = worked_out(&sessions, &[("HARBOR", "Harbour")], &[]);
        assert_eq!(
            teams.keys().collect::<Vec<_>>(),
            ["HARBOR"],
            "one team, and none for a session alone"
        );
        let team = &teams["HARBOR"];
        assert_eq!(
            (&team["title"], &team["lead"], &team["within"]),
            (&json!("Harbour"), &json!("lead"), &Value::Null),
            "titled by its quest, led by the session on it, within nothing"
        );
        assert_eq!(
            team["swarm"],
            Value::Null,
            "no name in the swarm where its quest gives none"
        );
        assert_eq!(
            team["counts"],
            json!({"needs-you": 0, "stuck": 0, "moving": 1, "quiet": 2}),
            "its lead and its two living sessions counted, the ended one not"
        );
        assert_eq!(
            team["members"],
            json!(["one", "two", "lead"]),
            "its members"
        );
        assert_eq!(team["worst"], json!("moving"), "the worst of them");
    }

    #[test]
    fn an_orchestrator_renamed_leads_one_team_with_those_answering_its_old_address() {
        let mut lead = session(
            "lead",
            State::Busy,
            (Some("REEF01"), Some("QEST-20261114-KELP01")),
            &[],
        );
        lead.address = "KELP01".to_owned();
        lead.presence = json!({"addresses": ["KELP01", "HARBOR", "EF5B865E"]}).into();
        let sessions = [
            session(
                "top",
                State::Idle,
                (None, Some("QEST-20261007-REEF01")),
                &[],
            ),
            lead,
            session("old", State::Busy, (Some("HARBOR"), None), &[]),
            session("new", State::Idle, (Some("KELP01"), None), &[]),
        ];
        let teams = worked_out(&sessions, &[("KELP01", "Kelp Beds")], &[]);
        assert_eq!(
            teams.keys().collect::<Vec<_>>(),
            ["KELP01", "REEF01"],
            "no team left under the old address"
        );
        let team = &teams["KELP01"];
        assert_eq!(
            (&team["title"], &team["lead"], &team["within"]),
            (&json!("Kelp Beds"), &json!("lead"), &json!("REEF01")),
            "titled and led as its own address, within its orchestrator's team"
        );
        assert_eq!(
            team["members"],
            json!(["old", "new"]),
            "those answering its old address and its new one alike"
        );
    }

    #[test]
    fn a_team_goes_by_the_name_its_quest_gives_it_in_the_swarm() {
        let sessions = [
            session(
                "lead",
                State::Idle,
                (None, Some("QEST-20261007-HARBOR")),
                &[],
            ),
            session("one", State::Busy, (Some("HARBOR"), None), &[]),
        ];
        let quests = BTreeMap::from([(
            "HARBOR".to_owned(),
            json!({"title": "Quick Wins That Move the Harbour", "swarm": "harbour"}),
        )]);
        let teams = teams(&sessions, &quests, &HashSet::new(), |_| 1);
        let named: Vec<(&str, &Value, &Value)> = teams
            .iter()
            .map(|(id, _, fields)| (id.as_str(), &fields["title"], &fields["swarm"]))
            .collect();
        assert_eq!(
            named,
            [(
                "HARBOR",
                &json!("Quick Wins That Move the Harbour"),
                &json!("harbour")
            )],
            "titled by its quest, and named in the swarm as its quest says"
        );
    }

    #[test]
    fn a_team_within_another_rolls_its_worst_up_into_it() {
        let mut asking = session("asks", State::Idle, (Some("KELP01"), None), &[]);
        asking.wants = Some(Want {
            kind: "question".to_owned(),
            since: "2026-11-12T21:50:00Z".to_owned(),
            said: "Which way?".to_owned(),
            asked: None,
        });
        let sessions = [
            session(
                "lead",
                State::Busy,
                (None, Some("QEST-20261007-HARBOR")),
                &[],
            ),
            session(
                "kelp",
                State::Busy,
                (Some("HARBOR"), Some("QEST-20261101-KELP01")),
                &[],
            ),
            asking,
        ];
        let teams = worked_out(&sessions, &[], &[]);
        let kelp = &teams["KELP01"];
        assert_eq!(
            (&kelp["within"], &kelp["lead"], &kelp["title"]),
            (&json!("HARBOR"), &json!("kelp"), &json!("kelp's window")),
            "within its lead's orchestrator, titled by its lead's window lacking a quest's"
        );
        assert_eq!(kelp["worst"], json!("needs-you"), "its own worst");
        let harbour = &teams["HARBOR"];
        assert_eq!(
            harbour["counts"],
            json!({"needs-you": 0, "stuck": 0, "moving": 2, "quiet": 0}),
            "only its own sessions counted"
        );
        assert_eq!(
            harbour["worst"],
            json!("needs-you"),
            "the worst of the team within rolls up"
        );
    }

    #[test]
    fn a_parked_or_paused_session_is_stuck() {
        let mut paused = session("paused", State::Idle, (Some("HARBOR"), None), &[]);
        paused.presence = json!({"paused": true}).into();
        let sessions = [
            paused,
            session("parked", State::Busy, (Some("HARBOR"), None), &[]),
            session("quiet", State::Idle, (Some("HARBOR"), None), &[]),
        ];
        let teams = worked_out(&sessions, &[], &["parked"]);
        assert_eq!(
            teams["HARBOR"]["counts"],
            json!({"needs-you": 0, "stuck": 2, "moving": 0, "quiet": 1}),
            "paused and parked both stuck"
        );
        assert_eq!(teams["HARBOR"]["lead"], Value::Null, "no session leads it");
    }

    #[test]
    fn a_repository_two_teams_share_is_a_bridge_and_a_session_hearing_both_a_liaison() {
        let sessions = [
            session(
                "one",
                State::Busy,
                (Some("HARBOR"), None),
                &["repo.reef", "repo.skiff", "orchestration.harbor"],
            ),
            session(
                "two",
                State::Busy,
                (Some("KELP01"), None),
                &["repo.reef", "orchestration.kelp01", "orchestration.harbor"],
            ),
        ];
        let teams = worked_out(&sessions, &[], &[]);
        assert_eq!(
            teams["HARBOR"]["bridges"],
            json!([{"repo": "reef", "teams": ["KELP01"]}]),
            "reef bridges the two; skiff is Harbour's alone"
        );
        assert_eq!(
            teams["HARBOR"]["liaisons"],
            json!([]),
            "none of Harbour's hears Kelp"
        );
        assert_eq!(
            teams["KELP01"]["liaisons"],
            json!([{"session": "two", "teams": ["HARBOR"]}]),
            "Kelp's session hearing Harbour too is a liaison"
        );
    }

    #[test]
    fn the_newest_landing_of_any_of_its_sessions_is_the_teams() {
        let landing = |at: &str, what: &str| Landing {
            at: at.to_owned(),
            reference: format!("reef#{}", what.len()),
            what: what.to_owned(),
            person: None,
        };
        let mut one = session("one", State::Idle, (Some("HARBOR"), None), &[]);
        one.landings = vec![landing("2026-11-12T20:00:00Z", "Older")];
        let mut two = session("two", State::Idle, (Some("HARBOR"), None), &[]);
        two.landings = vec![landing("2026-11-12T21:00:00Z", "Newest")];
        let teams = worked_out(&[one, two], &[], &[]);
        assert_eq!(
            teams["HARBOR"]["landed"],
            json!({"what": "Newest", "ref": "reef#6", "at": "2026-11-12T21:00:00Z", "session": "two"}),
            "the newest, and who landed it"
        );
    }

    #[test]
    fn each_team_says_the_shape_it_is_drawn_by() {
        let sessions = [
            session(
                "01a1",
                State::Busy,
                (None, Some("QEST-20261007-HARBOR")),
                &[],
            ),
            session(
                "01a2",
                State::Busy,
                (Some("HARBOR"), Some("QEST-20261101-KELP01")),
                &[],
            ),
            session("01a3", State::Idle, (Some("KELP01"), None), &[]),
        ];
        let teams = worked_out(&sessions, &[], &[]);
        assert_eq!(
            (&teams["HARBOR"]["glyph"], &teams["KELP01"]["glyph"]),
            (&json!("square"), &json!("triangle")),
            "both names hash to square, and Kelp, within Harbour and begun after it, steps on"
        );
    }
}
