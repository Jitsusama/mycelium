//! The swarm drawn from the teams and sessions a node holds.

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
        shapes: super::Shapes::default(),
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
fn a_lead_titled_as_its_team_is_named_by_its_address() {
    let mut swarm = swarm();
    if let Some(lead) = swarm
        .sessions
        .iter_mut()
        .find(|each| each["id"] == "s-lead")
    {
        lead["presence"] = json!({"questInfo": {"title": "Harbour"}});
    }
    let shown = team(&swarm, "HARBOR", &look()).expect("Harbour is found by its address");
    assert_eq!(
        shown.lines().next(),
        Some("Harbour · led by LEAD"),
        "its quest's title would only repeat the team's:\n{shown}"
    );
}

#[test]
fn a_liaison_titled_as_the_team_it_hears_is_named_by_its_address() {
    let mut swarm = swarm();
    if let Some(asking) = swarm.sessions.iter_mut().find(|each| each["id"] == "s-ask") {
        asking["presence"] = json!({"questInfo": {"title": "Reef"}});
    }
    let shown = team(&swarm, "KELP01", &look()).expect("Kelp is found by its address");
    assert_eq!(
        shown.lines().last(),
        Some("ASK also hears Reef"),
        "its quest's title would only repeat the team it hears:\n{shown}"
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
    swarm.teams[0]["title"] = json!("A reef whose name runs far past every column it was given");
    let shown = overview(&swarm, &look());
    assert!(
        shown
            .lines()
            .any(|line| line == "reef  A reef whose name runs far past e…                        1"),
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
        shown.contains("\x1b[33m1 needs you\x1b[0m") && shown.contains("\x1b[31m1 stuck\x1b[0m"),
        "needing you in yellow, stuck in red: {shown:?}"
    );
}
