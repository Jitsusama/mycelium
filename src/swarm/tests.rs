//! The swarm drawn from the teams and sessions a node holds.
//!
//! Long because its cases draw one realm of teams and sessions,
//! built by the helpers at its top, at the widths a terminal or a pipe
//! gives, and a case split from them would build that realm again.

use serde_json::{Value, json};

use super::{
    LONGEST_RUN, Look, Swarm, WHOLE, aligned, drawn_width, fit, most_steps, overview, padded, team,
    typed, unmarked, within, wrapped,
};

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
                &json!({"worst": "moving", "members": ["s-coral"],
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
            // Not s-reef: a living session's address takes the short
            // name it cleans to, and Reef's short name is reef.
            session("s-coral", "Reef's one", "busy"),
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
    // Short names that clean alike go to neither, so two teams are drawn
    // alike only by titles that clean alike.
    let mut alike = swarm();
    alike.teams[1]["title"] = json!("Dock");
    alike.teams[0]["title"] = json!("Do\u{200B}ck");
    assert_eq!(
        team(&alike, "dock", &look()),
        Err("dock could be Dock (REEF01) or Dock (HARBOR)".to_owned()),
        "two drawn alike are each named by their address too, so one can be typed"
    );
    alike.teams[0]["key"] = json!("team:RE\u{1b}]52;c;eA==\u{7}EF");
    assert_eq!(
        team(&alike, "dock", &look()),
        Err("dock could be Dock (no address to type) or Dock (HARBOR)".to_owned()),
        "a peer's address is said only when it is one, so typing it reaches its team"
    );
    alike.teams[0]["key"] = json!("team:PAUSE");
    assert_eq!(
        team(&alike, "dock", &look()),
        Err("dock could be Dock (no address to type) or Dock (HARBOR)".to_owned()),
        "an address swarm reads as an act is not said, as typing it does no steer"
    );
    let mut hidden = swarm();
    hidden.teams[1]["swarm"] = json!("harbor");
    hidden.teams[0]["swarm"] = json!("har\u{200B}bor");
    assert!(
        team(&hidden, "HARBOR", &look()).is_ok(),
        "a short name cleaned to a team's address does not hide it"
    );
    // Nor is Reef drawn or offered by it, which typed would reach Harbour.
    let shown = overview(&hidden, &look());
    assert!(
        !shown
            .lines()
            .any(|line| line.contains("Reef") && line.contains("harbor")),
        "Reef was drawn by a name that reaches Harbour:\n{shown}"
    );
    // Two short names that clean alike go to neither, so Harbour may lose
    // its own to the one drawn like it, and is offered by its title.
    assert!(
        typed(&hidden)
            .iter()
            .filter(|name| *name == "harbor")
            .count()
            <= 1,
        "harbor was offered for a team besides Harbour"
    );
}

#[test]
fn the_hint_gives_only_an_example_a_shell_reads_as_one_name() {
    let hint = |swarm: &Swarm| {
        overview(swarm, &look())
            .lines()
            .last()
            .and_then(|line| line.rsplit(' ').next())
            .map(str::to_owned)
    };
    let mut acting = swarm();
    acting.teams[1]["swarm"] = json!("harbour;ls");
    assert_eq!(
        hint(&acting).as_deref(),
        Some("HARBOR"),
        "a short name a shell acts on is given as the team's address"
    );
    acting.teams[1]["swarm"] = json!("harbour tides");
    assert_eq!(
        hint(&acting).as_deref(),
        Some("harbour-tides"),
        "a short name's words are given joined, as one word swarm reads as a name"
    );
    // Every miss is gathered, so undoing any one rule names its case.
    let mut misread = Vec::new();
    // One swarm reads as other than a name is no short name at all, so
    // the team is given by its title, as one with none is.
    for (given, said) in [
        ("harbour pause", "a name with an act among its words"),
        (
            "harbour-pause",
            "or among its parts, which may be typed apart",
        ),
        ("--names", "a name swarm reads as an option"),
        ("-ojson", "or as its output"),
        ("team", "a name read as the person saying what follows"),
        // Resume, as no session is addressed by it, so the act is what
        // refuses it, not the address a cleaned name would take.
        ("re\u{200B}sume", "or as an act once it is cleaned"),
        (
            "re\u{3164}sume",
            "or once a filler that counts as a letter is",
        ),
    ] {
        acting.teams[1]["swarm"] = json!(given);
        if hint(&acting).as_deref() != Some("Harbour") {
            misread.push(said);
        }
    }
    // Kelp stays within it, so Harbour still leads, by a short name a
    // shell would act on, so its address is what the hint would give.
    acting.teams[1]["swarm"] = json!("harbour;ls");
    acting.teams[2]["within"] = json!("PAUSE");
    acting.teams[1]["key"] = json!("team:PAUSE");
    if hint(&acting).as_deref() != Some("<team>") {
        misread.push("an address swarm reads as an act");
    }
    assert!(
        misread.is_empty(),
        "each is given as the address, the title or no example: {misread:?}"
    );
    acting.teams[1]["key"] = json!("team:HAR BOR");
    acting.teams[2]["within"] = json!("HAR BOR");
    assert_eq!(
        hint(&acting).as_deref(),
        Some("<team>"),
        "and with no address to type, no example"
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

/// The look a pipe is drawn with: whole, never in colour.
fn whole() -> Look {
    Look {
        width: WHOLE,
        ..look()
    }
}

#[test]
fn drawn_whole_the_counts_line_up_after_the_widest_name_and_nothing_trails() {
    let shown = overview(&swarm(), &whole());
    let expected = [
        "work · 6 running · 1 needs you · 1 stuck",
        "",
        "              need you  stuck  moving  quiet",
        "Harbour                     1       1      1",
        "  Kelp               1",
        "Reef                                1",
        "on their own                        1",
        "",
        "see a team by any part of its name: mycelium --realm work swarm harbour",
    ];
    assert_eq!(
        shown.lines().collect::<Vec<_>>(),
        expected,
        "as wide as its widest name, not the terminal's room:\n{shown}"
    );
}

#[test]
fn a_short_name_that_is_another_s_address_is_never_drawn_or_offered() {
    // Typed, it would reach the session BUSY, not Reef.
    let mut swarm = swarm();
    swarm.teams[0]["swarm"] = json!("busy");
    let shown = overview(&swarm, &look());
    assert!(
        !shown.contains("busy"),
        "a page drew a name that reaches another:\n{shown}"
    );
    assert!(
        !typed(&swarm).contains(&"busy".to_owned()),
        "a shell was offered a name that reaches another"
    );
}

#[test]
fn a_team_s_own_address_as_its_short_name_is_drawn_only_where_its_lead_has_it() {
    // A team keyed by a working session's address, led by another: typed,
    // its short name would reach the session BUSY, not the team.
    let mut swarm = swarm();
    swarm.teams[1]["key"] = json!("team:BUSY");
    swarm.teams[2]["within"] = json!("BUSY");
    swarm.teams[1]["swarm"] = json!("busy");
    let shown = overview(&swarm, &look());
    assert!(
        !shown.contains("busy"),
        "a page drew a name that reaches the session BUSY:\n{shown}"
    );
    // Led by BUSY, the team is what typing its address reaches.
    swarm.teams[1]["lead"] = json!("s-busy");
    assert!(
        typed(&swarm).contains(&"busy".to_owned()),
        "a team led by the session of its address lost its short name"
    );
}

#[test]
fn a_lead_s_address_as_its_team_s_short_name_is_never_drawn_where_the_team_goes_by_another() {
    // Harbour goes by HARBOR, led by the session LEAD: typed, lead reaches
    // that session alone, so pausing it would leave the rest running.
    let mut swarm = swarm();
    swarm.teams[1]["swarm"] = json!("lead");
    let shown = overview(&swarm, &look());
    assert!(
        !shown
            .lines()
            .any(|line| line.trim_start().starts_with("lead ")),
        "a page drew a name that reaches the session LEAD alone:\n{shown}"
    );
    assert!(
        !typed(&swarm).contains(&"lead".to_owned()),
        "a shell was offered a name that reaches the session LEAD alone"
    );
}

#[test]
fn a_short_name_that_cleans_to_an_act_is_never_drawn() {
    // Drawn, it reads as resume, which swarm does rather than reach Harbour.
    let mut swarm = swarm();
    swarm.teams[1]["swarm"] = json!("re\u{200B}sume");
    let shown = overview(&swarm, &look());
    assert!(
        !shown
            .lines()
            .any(|line| line.trim_start().starts_with("resume ")),
        "a page drew an act as a team's name:\n{shown}"
    );
    assert!(
        !typed(&swarm).iter().any(|name| unmarked(name) == "resume"),
        "a shell was offered an act as a team's name"
    );
}

#[test]
fn a_short_name_is_drawn_as_it_is_typed_and_reaches_only_its_team() {
    // Drawn with its space, `tide plan` typed would reach `tide-plan`'s
    // team; a braille blank or a control draws like a space but parts no
    // words, so each is folded as a space is.
    for (given, drawn) in [
        ("harbour  tides", "harbour-tides"),
        ("harbour\u{2800}tides", "harbour-tides"),
        ("harbour\u{1}tides", "harbour-tides"),
        ("harbour\u{1b}tides", "harbour-tides"),
    ] {
        let mut swarm = swarm();
        swarm.teams[1]["swarm"] = json!(given);
        let shown = overview(&swarm, &look());
        assert!(
            !shown.contains(given) && typed(&swarm).iter().any(|name| name == drawn),
            "{given} was not drawn as {drawn}:\n{shown}"
        );
        for asked in [drawn.to_owned(), drawn.replace('-', " ")] {
            let found = team(&swarm, &asked, &look()).unwrap_or_default();
            assert!(
                found.starts_with("Harbour"),
                "{asked}, typed as {given} is drawn, missed Harbour: {found}"
            );
        }
    }
    // A name whose words, however parted, hold an act would be typed apart
    // as one: `reef pause` would pause Reef rather than show Harbour.
    for given in [
        "reef pause",
        "reef-pause",
        "reef\u{2800}pause",
        "reef\u{1b}pause",
        "--names",
    ] {
        let mut acting = swarm();
        acting.teams[1]["swarm"] = json!(given);
        let names = typed(&acting);
        assert!(
            !names
                .iter()
                .any(|name| name.contains("pause") || name.contains("names")),
            "{given} was given as Harbour's name: {names:?}"
        );
    }
    // A name worked out from a title is held in the same typed form, so a
    // control in a title's word is drawn and reached as a hyphen.
    let mut controlled = swarm();
    controlled.teams[1]["title"] = json!("Bootstrap tide\u{1b}plan tools");
    let found = team(&controlled, "tide-plan", &look()).unwrap_or_default();
    assert!(
        typed(&controlled).iter().any(|name| name == "tide-plan") && found.starts_with("Bootstrap"),
        "a title's controlled word was not drawn as typed: {:?}, {found}",
        typed(&controlled)
    );
    // And to the same rule on acts.
    let worked_out = "Bootstrap reef\u{2800}pause tools";
    let mut titled = swarm();
    titled.teams[1]["title"] = json!(worked_out);
    assert!(
        !typed(&titled)
            .iter()
            .any(|name| name != worked_out && name.contains("pause")),
        "a title's word was given as Harbour's name: {:?}",
        typed(&titled)
    );
    // A spaced name, a controlled one and a hyphened one are typed alike,
    // so go to neither.
    for spaced in ["tide plan", "tide\u{1b}plan"] {
        let mut twins = swarm();
        twins.teams[0]["swarm"] = json!("tide-plan");
        twins.teams[1]["swarm"] = json!(spaced);
        assert!(
            !typed(&twins).iter().any(|name| name.contains("tide")),
            "one typed name was given to two teams: {:?}",
            typed(&twins)
        );
    }
    // One that cleans to nothing would be drawn blank and typed as no word.
    let mut blank = swarm();
    blank.teams[1]["swarm"] = json!("\u{200B}");
    assert!(
        !typed(&blank).iter().any(|name| unmarked(name).is_empty()),
        "a blank was given as Harbour's name: {:?}",
        typed(&blank)
    );
}

#[test]
fn two_short_names_that_clean_alike_go_to_neither_and_swarm_names_say_them_cleaned() {
    // Drawn alike, typed they'd reach both, and the hint offered one.
    let mut both = swarm();
    both.teams[0]["swarm"] = json!("dock");
    both.teams[1]["swarm"] = json!("do\u{200B}ck");
    assert!(
        !typed(&both).iter().any(|name| unmarked(name) == "dock"),
        "two teams were given one name: {:?}",
        typed(&both)
    );
    // Alone, the name is kept and handed to a shell as a page draws it.
    let mut alone = swarm();
    alone.teams[1]["swarm"] = json!("do\u{200B}ck");
    assert!(
        typed(&alone).iter().any(|name| name == "dock"),
        "a name was handed to a shell otherwise than drawn: {:?}",
        typed(&alone)
    );
}

#[test]
fn drawn_whole_a_title_heavy_with_marks_is_never_cut() {
    // More marks on its cells than a cut keeps, which a terminal's row
    // would cut, yet a pipe keeps all of it.
    let mut swarm = swarm();
    let heavy = format!("Reef{}", "\u{301}".repeat(40));
    swarm.teams[0]["title"] = json!(heavy);
    let shown = overview(&swarm, &whole());
    assert!(
        shown.contains(&heavy),
        "a title was cut into a pipe:\n{shown}"
    );
}

#[test]
fn drawn_whole_a_long_name_is_never_cut_and_runs_on_alone() {
    let mut swarm = swarm();
    let long = "A reef whose name runs far past every column it was given";
    swarm.teams[0]["title"] = json!(long);
    let shown = overview(&swarm, &whole());
    let lines: Vec<&str> = shown.lines().collect();
    // Led by the short name a long title is given, one space between as
    // a line that runs on draws it, its counts on the next line, under
    // the column the other rows' counts are in, so its one is read as
    // moving.
    let named = format!("reef {long}");
    let at = lines
        .iter()
        .position(|line| *line == named)
        .unwrap_or_else(|| panic!("whole, the long name is drawn in full, alone:\n{shown}"));
    assert!(!shown.contains('…'), "whole, nothing is cut:\n{shown}");
    assert_eq!(
        lines[at + 1],
        format!("{}1", " ".repeat(36)),
        "its count sits under its header, moving:\n{shown}"
    );
    assert!(
        lines
            .iter()
            .filter(|line| !line.starts_with("reef"))
            .all(|line| line.chars().count() <= 80),
        "one long name pushed the other rows past 80 columns:\n{shown}"
    );
}

#[test]
fn a_title_holding_a_break_draws_on_its_one_line() {
    let mut broken = swarm();
    broken.teams[0]["title"] = json!("Re\n\nef");
    let mut spaced = swarm();
    spaced.teams[0]["title"] = json!("Re  ef");
    assert_eq!(
        overview(&broken, &whole()),
        overview(&spaced, &whole()),
        "a break in a title split the block its counts line up in"
    );
}

#[test]
fn drawn_whole_what_each_session_does_lines_up_after_the_widest_name() {
    let shown = team(&swarm(), "harb", &whole()).expect("Harbour is found by its name");
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(
        lines[3..],
        [
            "      need you  stuck  moving  quiet",
            "Kelp         1",
            "",
            "stuck      Paused one  paused",
            "moving     Busy one    running the tests",
            "quiet      Harbour's lead",
            "",
            "works in skiff beside Reef",
        ],
        "one team, whole:\n{shown}"
    );
}

#[test]
fn a_session_s_doing_draws_as_its_words_on_one_line() {
    let doing = |step: &str| {
        let mut swarm = swarm();
        swarm.sessions[2]["step"] = json!(step);
        swarm
    };
    let dirty = "running\u{fdd0} the\n\ntests\u{1b}[2J";
    for look in [look(), whole()] {
        let shown = team(&doing(dirty), "harb", &look).expect("Harbour is found");
        assert!(
            !shown.contains(['\u{1b}', '\u{fdd0}', '\u{fdd1}']),
            "a doing's escape or mark reached the page:\n{shown}"
        );
        assert_eq!(
            Ok(shown),
            team(&doing(&unmarked(dirty)), "harb", &look),
            "a doing's mark, break or escape drew otherwise than on one line at {}",
            look.width
        );
    }
}

#[test]
fn a_title_holding_a_mark_moves_no_column() {
    // Reef is named in two of Harbour's lines, so its marks would line
    // up with each other if either were kept.
    let bridged = || {
        let mut swarm = swarm();
        swarm.teams[1]["bridges"] = json!([{"repo": "skiff", "teams": ["REEF01"]},
                                          {"repo": "kelp", "teams": ["REEF01", "KELP01"]}]);
        swarm
    };
    let mut swarm = bridged();
    swarm.teams[0]["title"] = json!("Re\u{fdd0}e\u{fdd0}f");
    swarm.sessions[2]["title"] = json!("Busy\u{fdd1} one");
    // The head names the lead, and the line under it what landed, so a
    // mark kept in both would line one up with the other.
    for session in &mut swarm.sessions {
        if session["id"] == "s-lead" {
            session["title"] = json!("Harbour's\u{fdd0} lead");
        }
    }
    swarm.teams[1]["landed"]["what"] = json!("Hear\u{fdd0} the Tide");
    assert_eq!(
        overview(&swarm, &whole()),
        overview(&bridged(), &whole()),
        "a team's mark is dropped, so its count stays put"
    );
    assert_eq!(
        team(&swarm, "harb", &whole()),
        team(&bridged(), "harb", &whole()),
        "and in every line naming it, so none moves"
    );
}

#[test]
fn marks_line_up_within_a_block_and_each_kind_on_its_own() {
    assert_eq!(
        aligned("a\u{fdd0}  1\nlonger\u{fdd0}  2\n\nx\u{fdd1}  doing\nlong\u{fdd0}  3\n"),
        "a       1\nlonger  2\n\nx  doing\nlong  3",
        "padded to the block's widest, a blank line starting the next"
    );
    assert_eq!(
        aligned("été\u{fdd0} 1\nab\u{fdd0} 2   "),
        "été 1\nab  2",
        "counted in characters, not bytes, and nothing trails"
    );
    // Swarm's four headers take 32 columns, so a lead of 48 keeps them
    // within 80 and one of 49 doesn't.
    let counts = "  need you  stuck  moving  quiet";
    let (fits, runs_on) = ("x".repeat(48), "y".repeat(49));
    assert_eq!(
        aligned(&format!(
            "\u{fdd0}{counts}\n{fits}\u{fdd0}  1\n{runs_on}\u{fdd0}  2\nab\u{fdd0}\u{fdd0}  3"
        )),
        format!(
            "{pad}{counts}\n{fits}  1\n{runs_on}\n{pad}  2\nab{}  3",
            " ".repeat(46),
            pad = " ".repeat(48)
        ),
        "a lead that keeps the counts within 80 lines up, a longer one sets its counts \
         on the next line under the column, and a mark left over goes"
    );
    assert_eq!(
        aligned(&format!(
            "\u{fdd0}  need you  stuck  moving  quiet  dormant\n{fits}\u{fdd0}  1"
        )),
        format!("  need you  stuck  moving  quiet  dormant\n{fits}\n  1"),
        "with five headers, 41 columns, a lead of 48 runs on"
    );
}

#[test]
fn a_row_that_runs_on_leaves_no_gap_and_spaces_out_no_figure() {
    let counts = "  need you  stuck  moving  quiet";
    let pad = " ".repeat(48);
    // A lead of 48 sets the column, as the widest that fits; a run-on
    // row's lead never does.
    let (fits, none) = ("x".repeat(48), "z".repeat(49));
    assert_eq!(
        aligned(&format!(
            "\u{fdd0}{counts}\n{none}\u{fdd0}\n  Reef{}1{}x\u{fdd0}\n{fits}\u{fdd0}  1",
            " ".repeat(30),
            " ".repeat(20)
        )),
        format!("{pad}{counts}\n{none}\n  Reef 1 x\n{fits}  1"),
        "a long row with no counts stays on its line, leaving no blank one to end \
         the table early"
    );
    let spaced = format!("  Reef{}1{}x", " ".repeat(30), " ".repeat(20));
    assert_eq!(
        aligned(&format!(
            "\u{fdd0}{counts}\n{spaced}\u{fdd0}  2\n{fits}\u{fdd0}  1"
        )),
        format!("{pad}{counts}\n  Reef 1 x\n{pad}  2\n{fits}  1"),
        "a title that runs on draws each run of spaces as one, so it can't set a \
         figure under a header"
    );
    let blank = format!("  Reef{}1{}x", "\u{2800}".repeat(30), " ".repeat(20));
    assert_eq!(
        aligned(&format!(
            "\u{fdd0}{counts}\n{blank}\u{fdd0}  2\n{fits}\u{fdd0}  1"
        )),
        format!("{pad}{counts}\n  Reef 1 x\n{pad}  2\n{fits}  1"),
        "a run of braille blanks, drawn as spaces, is drawn as one space too"
    );
}

#[test]
fn a_title_s_own_leading_spaces_set_no_figure_under_a_header() {
    // Two teams of one title share its short name, so neither shows one,
    // and the title alone leads each row. A braille blank never counts
    // as an indent, so it isn't one of these.
    for space in [" ", "\u{3000}"] {
        let title = format!("{}1{}{}", space.repeat(36), " ".repeat(20), "x".repeat(30));
        let mut swarm = swarm();
        swarm.teams[0]["title"] = json!(title);
        swarm.teams[2]["title"] = json!(title);
        let shown = overview(&swarm, &whole());
        let rows: Vec<&str> = shown.lines().filter(|line| line.contains("1 xx")).collect();
        assert_eq!(
            rows.len(),
            2,
            "both titles' rows are drawn, each once:\n{shown}"
        );
        for row in rows {
            assert!(
                row.starts_with("1 x") || row.starts_with("  1 x"),
                "a title's own leading {space:?} passed for its row's indent:\n{shown}"
            );
        }
    }
}

#[test]
fn a_colour_s_escape_takes_no_column_where_marks_line_up() {
    assert_eq!(
        aligned("\u{1b}[1mab\u{1b}[0m\u{fdd0}  1\nabcd\u{fdd0}  2"),
        "\u{1b}[1mab\u{1b}[0m    1\nabcd  2",
        "a painted lead was padded by its characters rather than the columns it takes"
    );
}

#[test]
fn width_is_counted_in_the_cells_a_terminal_draws() {
    for (text, wide, what) in [
        ("tide", 4, "a letter takes one"),
        ("港湾", 4, "a Han character takes two"),
        ("e\u{301}", 1, "a combining accent takes none"),
        ("👩\u{200D}💻", 2, "an emoji joined from two takes two"),
        ("❤\u{FE0F}", 2, "a heart made an emoji takes two"),
        ("\u{1b}[1m港\u{1b}[0m", 2, "a colour's escape takes none"),
        ("👍\u{1F3FD}", 2, "a skin tone joins its hand"),
        ("\u{1F1E8}\u{1F1E6}", 2, "a flag's two letters take two"),
        ("1\u{FE0F}\u{20E3}", 2, "a keycap takes two"),
        (
            "👩\u{200D}❤\u{FE0F}\u{200D}👨",
            2,
            "a couple joined through a heart takes two",
        ),
        (
            "👩\u{1F3FD}\u{200D}💻",
            2,
            "a toned emoji joined on takes two",
        ),
        (
            "🏴\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F}",
            2,
            "a flag spelled in tags takes two",
        ),
        (
            "👩\u{200D}\u{3001}",
            4,
            "a wide mark joined to an emoji takes its own two",
        ),
    ] {
        assert_eq!(drawn_width(text), wide, "{what}: {text:?}");
    }
}

#[test]
fn a_letter_a_ligature_joins_still_takes_its_own_cell() {
    // Unicode's width rules count each of these ligatures narrower than
    // its letters, but a terminal draws each letter in a cell of its own.
    for (text, wide, what) in [
        ("\u{644}\u{627}", 2, "Arabic's lam-alef"),
        (
            "\u{644}\u{64E}\u{627}",
            2,
            "lam-alef with a vowel mark between",
        ),
        ("\u{5D0}\u{200D}\u{5DC}", 2, "Hebrew's alef-lamed"),
        (
            "\u{1780}\u{17D2}\u{1780}",
            2,
            "a Khmer letter and its subscript",
        ),
        (
            "\u{1781}\u{17D2}\u{1782}\u{17D2}\u{1783}",
            3,
            "a Khmer letter and a chain of two subscripts",
        ),
        ("\u{A4F9}\u{A4FC}", 2, "Lisu's tone letters"),
        ("\u{10C32}\u{200D}\u{10C03}", 2, "Old Turkic's ligature"),
        ("\u{1A15}\u{1A17}\u{200D}\u{1A10}", 2, "Buginese's ligature"),
        ("\u{2D4F}\u{200D}\u{2D3E}", 2, "Tifinagh's ligature"),
        (
            "👩\u{200D}\u{5D0}\u{200D}\u{5DC}",
            4,
            "a ligature joined on after an emoji",
        ),
        ("•\u{200D}\u{5D0}\u{200D}\u{5DC}", 3, "or after a sign"),
        (
            "\u{200D}\u{5D0}\u{200D}\u{5DC}",
            2,
            "or after a joiner alone",
        ),
        // An emoji after a letter's joiner counts in full, as Unicode's
        // widths count it.
        ("\u{5D0}\u{200D}👩", 3, "an emoji after a letter's joiner"),
        ("a\u{200D}👩", 3, "or after a Latin letter's"),
    ] {
        assert_eq!(drawn_width(text), wide, "{what}: {text:?}");
    }
    // So a cut to the room keeps no more than the room, and a pad fills it.
    let lam_alefs = "\u{644}\u{627}".repeat(10);
    assert_eq!(
        drawn_width(&fit(&lam_alefs, 10)),
        10,
        "{:?}",
        fit(&lam_alefs, 10)
    );
    assert_eq!(drawn_width(&padded("\u{644}\u{627}", 4)), 4);
    assert_eq!(
        within("\u{1780}\u{17D2}\u{1780}", 1),
        "\u{1780}",
        "a coeng left at the cut goes with its letter"
    );
}

#[test]
fn a_cut_or_pad_keeps_to_cells_and_never_splits_a_wide_character() {
    assert_eq!(
        fit("港湾港", 4),
        "港…",
        "a wide character that would pass the edge goes"
    );
    assert_eq!(fit("港湾", 4), "港湾", "what fits is kept whole");
    assert_eq!(padded("港", 4), "港  ", "padding counts the cells taken");
    assert_eq!(
        within("❤\u{FE0F}x", 1),
        "❤",
        "a selector that widens a heart past the edge is left out"
    );
    // A joiner makes two emoji one, narrower than its characters, so a
    // cut that counted them one at a time split it.
    let mut split = Vec::new();
    for (cut, kept, what) in [
        (
            within("👩\u{200D}💻", 2),
            "👩\u{200D}💻",
            "an emoji joined from two fits whole",
        ),
        (
            within("👍\u{1F3FD}ab", 3),
            "👍\u{1F3FD}a",
            "a skin tone keeps its hand",
        ),
        (
            within("\u{1b}[1mabc", 2),
            "\u{1b}[1mab",
            "an escape is never left half written",
        ),
        (
            within("\u{1b}[38;2;255;255;255mab", 1),
            "\u{1b}[38;2;255;255;255ma",
            "an escape longer than a cut's steps is one step",
        ),
        (
            within("ab👩\u{200D}💻", 2),
            "ab",
            "a joined emoji past the edge is left whole",
        ),
        (
            within("👩\u{200D}x", 2),
            "👩",
            "a joiner left at the cut goes",
        ),
        (
            within("港\u{200D}湾", 3),
            "港",
            "a joiner between two Han characters goes at the cut",
        ),
        (
            within("⌚\u{FE0E}x", 1),
            "⌚\u{FE0E}",
            "a start past the edge comes back within it once a text selector follows",
        ),
    ] {
        if cut != kept {
            split.push(format!("{what}: {cut:?}"));
        }
    }
    for (drawn, whole, what) in [
        (
            fit("👩\u{200D}💻 ab", 4),
            "👩\u{200D}💻…",
            "a cut keeps a joined emoji",
        ),
        (
            wrapped("👩\u{200D}💻", "", "", 3),
            "👩\u{200D}💻\n",
            "a wrap keeps a joined emoji on its line",
        ),
        (
            wrapped("👩\u{200D}💻", "", "", 1),
            "👩\u{200D}💻\n",
            "a line too narrow for it still takes it whole",
        ),
    ] {
        if drawn != whole {
            split.push(format!("{what}: {drawn:?}"));
        }
    }
    assert!(
        split.is_empty(),
        "a joined emoji or an escape was split: {split:?}"
    );
    assert_eq!(
        wrapped("港湾港 湾", "", "", 5),
        "港湾\n港 湾\n",
        "a word past a line is broken by cells, and words join by them"
    );
    assert_eq!(
        wrapped("港湾", "", "", 1),
        "港\n湾\n",
        "a line too narrow for a wide character still takes one"
    );
    assert_eq!(
        wrapped("abcd❤\u{FE0F} ef", "", "", 5),
        "abcd\n❤\u{FE0F} ef\n",
        "a heart that its selector widens past a line goes to the next with it"
    );
}

#[test]
fn a_peer_s_name_chained_from_emoji_counts_a_cell_pair_for_each_four() {
    // Unicode's widths count a joined run of any length as two cells, so
    // cleaned, a run keeps three joiners at most, as a family of four
    // does, and a cut takes the rest at the cells they count.
    let cells = <str as unicode_width::UnicodeWidthStr>::width;
    let chained = vec!["\u{1F469}"; 84].join("\u{200D}");
    let cleaned = unmarked(&chained);
    assert_eq!(cells(cleaned.as_str()), 2 * 84 / 4, "{cleaned:?}");
    assert!(
        fit(&cleaned, 20).ends_with('…'),
        "a name chained from emoji passed a cut whole"
    );
    // Four emoji joined as no sequence still count two cells and draw
    // eight, a bound the cap keeps rather than closes. A nonspacing mark
    // between them draws nothing, and a skin tone carries on the emoji
    // before it, so neither starts the count afresh, nor does a regional
    // indicator, paired as a flag or alone: a run holds four emoji at most, and each four cost two
    // cells, as Unicode's widths count them. Each emoji carries its mark,
    // so one that restarted the count would keep every joiner. The count
    // runs from the text's start, so a chain of flags, which nothing in
    // it restarts, keeps three joiners in all; a toned emoji after a
    // dropped joiner follows no joiner kept, so it starts a new run.
    let marked = "\u{1F469}\u{301}\u{200D}".repeat(20);
    let toned = "\u{1F469}\u{1F3FB}\u{200D}".repeat(20);
    let flagged = "\u{1F1E8}\u{1F1E6}\u{200D}".repeat(20);
    let kept = |text: &str| unmarked(text).matches('\u{200D}').count();
    assert_eq!(
        kept(&marked),
        15,
        "a mark between emoji is joined past, three joiners a run"
    );
    assert_eq!(kept(&toned), 15, "a toned emoji is a run's start");
    assert_eq!(
        kept(&flagged),
        3,
        "a flag's half started a run, or the count didn't run from the start"
    );
    assert_eq!(
        kept(
            "\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F469}\u{1F1E6}\u{200D}\u{1F469}"
        ),
        3,
        "a lone regional indicator started a run"
    );
    // A spacing mark takes a cell of its own, so it starts a run afresh,
    // as PROTOCOL says: the joiner after it is the run's first.
    assert_eq!(
        kept(
            "\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F469}\u{93E}\u{200D}\u{1F469}"
        ),
        4,
        "a spacing mark is a run's start"
    );
    // But one that extends a grapheme, as U+09BE does, unicode-width
    // gives no cell, so the run goes on past it, as PROTOCOL says.
    assert_eq!(
        kept(
            "\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F469}\u{9BE}\u{200D}\u{1F469}"
        ),
        3,
        "a mark extending a grapheme started a run"
    );
    // Nor does a conjoining jamo vowel, given no cell either.
    assert_eq!(
        kept(
            "\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F469}\u{1161}\u{200D}\u{1F469}"
        ),
        3,
        "a conjoining jamo vowel started a run"
    );
    // A non-joiner is never counted, so one after a run at the cap stays.
    let parted = "\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F469}\u{200C}\u{1F469}";
    assert_eq!(
        unmarked(parted),
        parted,
        "a non-joiner was capped as a joiner"
    );
    for (chain, emoji) in [
        (marked.as_str(), 20),
        (toned.as_str(), 20),
        (flagged.as_str(), 20),
    ] {
        let cleaned = unmarked(chain);
        // Counted directly too, since whether Unicode's widths carry a run
        // through a mark is theirs to say: each four emoji keep three.
        let joiners = cleaned.matches('\u{200D}').count();
        assert!(
            joiners <= 3 * emoji / 4 && cells(cleaned.as_str()) >= 2 * emoji / 4,
            "a mark, a skin tone or a flag let a run keep more than three joiners \
             ({joiners}): {cleaned:?}"
        );
    }
}

#[test]
fn a_noncharacter_in_a_chain_parts_no_run_of_joiners() {
    // A noncharacter goes before the joiners are judged, as PROTOCOL
    // cleans a `for`, so one between emoji, which unicode-width gives a
    // cell, can't start a run afresh and then leave it joined past the
    // cap once it's gone, nor part a joiner from the letter before it.
    let four = "\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F469}";
    let flags = "\u{1F1E8}\u{1F1E6}\u{200D}".repeat(3) + "\u{1F1E8}\u{1F1E6}";
    let kept = |text: &str| unmarked(text).matches('\u{200D}').count();
    for (parted, after) in [
        ("\u{FDD0}", "\u{1F3FB}\u{200D}"),
        ("\u{FDD0}", "\u{301}\u{200D}"),
        ("\u{FFFE}", "\u{1F3FB}\u{200D}"),
    ] {
        // Whole, not counted: one that started a run afresh keeps as many
        // joiners, but where the run should have been parted.
        let chain = format!("{four}{parted}{after}").repeat(10);
        assert_eq!(
            unmarked(&chain),
            unmarked(&chain.replace(parted, "")),
            "a noncharacter parted a run of joiners: {chain:?}"
        );
    }
    assert_eq!(
        kept(&format!("{flags}\u{FDD0}\u{200D}{flags}")),
        3,
        "a noncharacter started a chain of flags afresh"
    );
    assert_eq!(
        unmarked("\u{6E2F}\u{FDD0}\u{200D}\u{6E7E}"),
        "\u{6E2F}\u{200D}\u{6E7E}",
        "a noncharacter parted a joiner from the letter before it"
    );
}

#[test]
fn a_for_is_cleaned_by_what_protocol_lists() {
    // What draws nothing goes in the same pass as the joiners are
    // judged, as PROTOCOL says: before a joiner it is looked past, as
    // what is kept, and after one it parts it, as the very next
    // character left. A direction mark, gone before, as a noncharacter
    // is, parts nothing. A space past ASCII is a space, as PROTOCOL says
    // of White_Space, so a joiner after one goes, and one at an end is
    // trimmed. What draws nothing and what selects are PROTOCOL's own
    // lists: a Mongolian selector and an annotation control draw nothing,
    // both ends of the ideographic selectors select, and a non-joiner, as
    // a joiner, stands only between two. A selector follows the last
    // character kept, so what this pass drops before it parts nothing.
    // The cap on joiners in a run has its cases in the chain test above.
    for (said, cleaned) in [
        ("\u{1820}\u{180B}", "\u{1820}"),
        ("\u{1820}\u{FE00}", "\u{1820}\u{FE00}"),
        ("\u{845B}\u{E0100}", "\u{845B}\u{E0100}"),
        ("\u{845B}\u{E01EF}", "\u{845B}\u{E01EF}"),
        ("\u{845B}\u{E01F0}", "\u{845B}"),
        ("\u{6E2F}\u{200B}\u{FE0F}", "\u{6E2F}\u{FE0F}"),
        ("\u{6E2F}\u{200D}\u{FE0F}", "\u{6E2F}\u{FE0F}"),
        ("\u{6E2F}\u{FFF9}\u{6E7E}", "\u{6E2F}\u{6E7E}"),
        ("\u{6E2F}\u{200C}\u{6E7E}", "\u{6E2F}\u{200C}\u{6E7E}"),
        ("\u{6E2F}\u{200C}a", "\u{6E2F}a"),
        (
            "\u{6E2F}\u{3000}\u{200D}\u{6E7E}",
            "\u{6E2F}\u{3000}\u{6E7E}",
        ),
        ("\u{6E2F}\u{200D}\u{A0}\u{6E7E}", "\u{6E2F}\u{A0}\u{6E7E}"),
        ("\u{3000}\u{6E2F}\u{A0}", "\u{6E2F}"),
        (
            "\u{6E2F}\u{200D}\u{200E}\u{6E7E}",
            "\u{6E2F}\u{200D}\u{6E7E}",
        ),
        (
            "\u{6E2F}\u{200D}\u{202E}\u{6E7E}",
            "\u{6E2F}\u{200D}\u{6E7E}",
        ),
        (
            "\u{6E2F}\u{200B}\u{200D}\u{6E7E}",
            "\u{6E2F}\u{200D}\u{6E7E}",
        ),
        ("\u{6E2F}\u{200D}\u{200B}\u{6E7E}", "\u{6E2F}\u{6E7E}"),
        ("\u{6E2F}\u{200D}\u{AD}\u{6E7E}", "\u{6E2F}\u{6E7E}"),
        (
            "\u{6E2F}\u{200D}\u{FDD0}\u{6E7E}",
            "\u{6E2F}\u{200D}\u{6E7E}",
        ),
    ] {
        assert_eq!(unmarked(said), cleaned, "{said:?} cleaned unlike PROTOCOL");
    }
}

#[test]
fn a_cut_keeps_the_longest_start_that_fits_as_measured_whole() {
    // Measured whole by the cell count, a start at a time, as the cut
    // measured before it settled what it had walked.
    fn longest(text: &str, room: usize) -> &str {
        let mut kept = "";
        for (at, each) in text.char_indices() {
            let start = &text[..at + each.len_utf8()];
            if drawn_width(start) <= room {
                kept = start;
            }
        }
        kept.trim_end_matches(['\u{200D}', '\u{17D2}'])
    }
    let mut wrong = Vec::new();
    for text in [
        "❤\u{FE0F}x❤\u{FE0E}y",
        "⌚\u{FE0E}ab⌚c",
        "👩\u{200D}💻👩\u{200D}💻a👍\u{1F3FD}b",
        "🇨🇦🇺🇸 1\u{FE0F}\u{20E3}x",
        "لا ab لآ",
        "ꓹꓼ a ⵏ\u{2D7F}ⴾ b",
        "\u{1780}\u{17D2}\u{1780} \u{1781}\u{17D2}\u{1782}\u{17D2}\u{1783}",
        "港\u{200D}湾 e\u{301}\u{302}x",
        "a\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}b",
        // A selector just past the longest run of steps with no fresh
        // start, where a cut that settled between it and its heart would
        // count a cell less. Arabic letters hold the run, as accents can't:
        // a pile of them is cut at its steps to a cell first.
        &format!("{}\u{200D}❤\u{FE0F}x", "ب".repeat(LONGEST_RUN - 2)),
    ] {
        for room in 0..36 {
            let (cut, whole) = (within(text, room), longest(text, room));
            if cut != whole {
                wrong.push(format!("{text:?} to {room}: {cut:?}, not {whole:?}"));
            }
        }
    }
    // Past a run of steps with no fresh start the cut settles anyway,
    // which may count more than is drawn but never less.
    let joined = "👩\u{200D}".repeat(40);
    for room in 0..16 {
        let cut = within(&joined, room);
        if drawn_width(cut) > room {
            wrong.push(format!("a long joined run to {room} passed it"));
        }
        if room >= 2 && cut.is_empty() {
            wrong.push(format!("a long joined run to {room} kept nothing"));
        }
    }
    assert!(
        wrong.is_empty(),
        "cuts that weren't the longest start: {wrong:#?}"
    );
}

#[test]
fn a_long_text_costs_what_it_walks_into_a_pipe_or_not_and_a_pile_of_marks_is_cut() {
    use std::{sync::mpsc, thread, time::Duration};
    /// A cut by what it is, and whether it came out right.
    type Cut = (&'static str, fn() -> bool);
    // A peer's message may be 256 KiB on one line; measured to its end
    // at each start, cutting it takes minutes and wrapping it longer.
    // Into a pipe the room is the whole text, so neither its room nor a
    // cap on steps bounds it there.
    let budget = Duration::from_secs(5);
    // Each cut runs on a thread of its own, so one that costs what its
    // text does fails at the budget rather than holding the run for
    // hours; the test's process ends what it leaves running.
    let within_budget = |cut: fn() -> bool| {
        let (sent, heard) = mpsc::channel();
        thread::spawn(move || {
            let _ = sent.send(cut());
        });
        heard.recv_timeout(budget)
    };
    let cases: [Cut; 8] = [
        ("Han cut to 80", || {
            drawn_width(&fit(&"港".repeat(60_000), 80)) <= 80
        }),
        ("one word wrapped to 80", || {
            wrapped(&"a".repeat(20_000), "", "", 80).lines().count() == 250
        }),
        ("Han cut to its own width", || {
            let han = "港".repeat(60_000);
            within(&han, drawn_width(&han)) == han
        }),
        ("one word fitted whole, measured once", || {
            let word = "a".repeat(256 * 1024);
            fit(&word, WHOLE) == word
        }),
        ("one word wrapped whole", || {
            wrapped(&"a".repeat(256 * 1024), "  ", "    ", WHOLE)
                .lines()
                .count()
                == 1
        }),
        // Marks start nothing afresh, so only settling past a long run
        // keeps a cut through them from measuring them all again.
        ("a pile of marks wrapped whole", || {
            wrapped(&format!("a{}", "\u{301}".repeat(256 * 1024)), "", "", WHOLE)
                .lines()
                .count()
                == 1
        }),
        ("many words wrapped whole", || {
            wrapped(&"ab ".repeat(80_000), "", "", WHOLE)
                .lines()
                .count()
                == 1
        }),
        // Each emoji starts afresh, so a cut settles at each and counts
        // exactly: forty of them take eighty cells.
        ("joined emoji cut to 80", || {
            within(&"👩\u{200D}💻".repeat(20_000), 80).chars().count() == 40 * 3
        }),
    ];
    let slow: Vec<String> = cases
        .iter()
        .filter_map(|(what, cut)| match within_budget(*cut) {
            Ok(true) => None,
            Ok(false) => Some(format!("{what}: cut wrong")),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Some(format!("{what}: still cutting after {budget:?}"))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Some(format!("{what}: panicked")),
        })
        .collect();
    assert!(
        slow.is_empty(),
        "cuts that cost what their text does: {slow:#?}"
    );
    // A name buried in marks or joiners that draw nothing measures one
    // cell, so only the cap on steps keeps it from putting them all on
    // the line.
    for buried in [
        format!("a{}", "\u{301}".repeat(10_000)),
        format!("a{}", "\u{200D}".repeat(10_000)),
    ] {
        let kept = fit(&buried, 10);
        let count = kept.chars().count();
        assert!(
            kept.starts_with('a') && count <= most_steps(10 - 1) + 1,
            "a pile passed a cut whole, or its name went: {count} characters kept"
        );
    }
    // Wrapped, the rest of a pile goes too, rather than taking lines of
    // nothing that push the words after it out of what is shown.
    // Whichever of its word's letters the pile follows, so the word isn't
    // split at the pile's base.
    for word in ["a", "ab", "Reef"] {
        let wrap = wrapped(&format!("{word}{} b", "\u{301}".repeat(10_000)), "", "", 80);
        // Cut to the line's cap, it leaves no room for another step, so
        // the word after it goes down, and no line holds a mark but the
        // first.
        assert!(
            wrap.starts_with(word)
                && wrap.ends_with("\nb\n")
                && wrap.lines().skip(1).all(|line| !line.contains('\u{301}')),
            "a pile after {word} spread over lines: {} of them",
            wrap.lines().count()
        );
    }
    // A selector that doesn't fit still moves with its emoji to the next.
    assert_eq!(
        wrapped("abc\u{2764}\u{FE0F}", "", "", 4),
        "abc\n\u{2764}\u{FE0F}\n",
        "a selector was parted from its emoji, or dropped"
    );
    // A conjunct written with a joiner (a virama, then the joiner) is
    // no pile running on, so it moves down whole, its joiner kept.
    assert_eq!(
        wrapped("abc\u{915}\u{94D}\u{200D}\u{937}", "", "", 4),
        "abc\n\u{915}\u{94D}\u{200D}\u{937}\n",
        "a conjunct was split at its joiner, or its joiner dropped"
    );
}

#[test]
fn a_pile_of_marks_on_a_later_word_is_cut_as_one_on_the_first_is() {
    // A peer's question may wrap into a few lines and be shown as it
    // is, so a pile joined whole to the line before it would bury what
    // is beside it, as a pile on the text's first word can't.
    let wrap = wrapped(&format!("x Reef{} b", "\u{301}".repeat(10_000)), "", "", 80);
    let marks = wrap.matches('\u{301}').count();
    assert!(
        marks < most_steps(80)
            && wrap.starts_with('x')
            && wrap.contains("Reef")
            && wrap.ends_with("b\n"),
        "a pile after a later word passed whole, or a word went: {marks} marks on {:?}",
        wrap.lines()
            .map(|line| line.chars().count())
            .collect::<Vec<_>>()
    );
    // Nor do piles split over short words, each under the cap, add up
    // on one line past it.
    let split = wrapped(
        &format!("Reef{} ", "\u{301}".repeat(640)).repeat(16),
        "",
        "",
        80,
    );
    let most = split.lines().map(|line| line.chars().count()).max();
    assert!(
        split.matches("Reef").count() == 16 && most.is_some_and(|most| most <= most_steps(80)),
        "piles on short words added up on a line past the cap, or a word went: {most:?}"
    );
}

#[test]
fn a_wide_team_keeps_its_counts_in_line_cut_or_not() {
    // Measured by Unicode's widths directly, not by the code under test,
    // as nothing here is coloured.
    let cells = <str as unicode_width::UnicodeWidthStr>::width;
    let row = |title: &str| {
        let mut swarm = swarm();
        swarm.teams[0]["title"] = json!(title);
        let shown = overview(&swarm, &look());
        let first = title.chars().next().unwrap_or('R');
        shown
            .lines()
            .find(|line| line.starts_with(first) && line.trim_end().ends_with(" 1"))
            .map(cells)
    };
    let plain = row("Reef");
    for title in ["港湾港湾", &"港".repeat(40)] {
        assert!(
            plain.is_some() && row(title) == plain,
            "a wide title moved its counts: {title:?} drew {:?} cells, Reef {plain:?}",
            row(title)
        );
    }
}

#[test]
fn a_wide_title_lines_its_counts_up_by_cells() {
    assert_eq!(
        aligned("港湾\u{fdd0}  1\nab\u{fdd0}  2"),
        "港湾  1\nab    2",
        "a wide lead was padded by its characters rather than its cells"
    );
}

#[test]
fn a_doing_past_its_column_runs_on_under_it() {
    // What comes before a doing ends by column 48, so a lead of 48 lines
    // its doing up and one of 49 sets it on the next line.
    let (fits, runs_on) = ("x".repeat(48), "y".repeat(49));
    let pad = " ".repeat(48);
    assert_eq!(
        aligned(&format!(
            "{fits}\u{fdd1}  tide\nab\u{fdd1}  ebb\n{runs_on}\u{fdd1}  flood"
        )),
        format!(
            "{fits}  tide\nab{}  ebb\n{runs_on}\n{pad}  flood",
            " ".repeat(46)
        ),
        "a doing past column 48 runs on, under the others"
    );
    assert_eq!(
        aligned(&format!(
            "{runs_on}\u{fdd1}  flood\n{runs_on}z\u{fdd1}  ebb"
        )),
        format!("{runs_on}\n{pad}  flood\n{runs_on}z\n{pad}  ebb"),
        "where no lead fits, each doing runs on under column 48"
    );
    assert_eq!(
        aligned(&format!("{runs_on}\u{fdd1}")),
        runs_on,
        "with no doing, the line stays as it is"
    );
}

#[test]
fn a_short_name_holding_what_draws_nothing_moves_no_title() {
    // The widest short name sets the column, so a name counted before it
    // is cleaned would move every title; the narrower one is padded to
    // it, so a name padded before it is cleaned would move its own.
    let named = |harbour: &str, kelp: &str| {
        let mut swarm = swarm();
        swarm.teams[1]["swarm"] = json!(harbour);
        swarm.teams[2]["swarm"] = json!(kelp);
        swarm
    };
    let plain = overview(&named("dock", "ba"), &whole());
    assert!(
        plain.contains("\ndock  ") && plain.contains("ba    Kelp"),
        "both short names are shown, Kelp's padded to Harbour's:\n{plain}"
    );
    for look in [look(), whole()] {
        assert_eq!(
            overview(&named("do\u{200B}ck\u{fdd0}", "b\u{200B}a"), &look),
            overview(&named("dock", "ba"), &look),
            "a character a short name loses moved a title, or reached the hint, at {}",
            look.width
        );
    }
    // The name the page shows, as typed, finds its team.
    let dirty = named("do\u{200B}ck\u{fdd0}", "b\u{200B}a");
    for (typed, found) in [("dock", "Harbour"), ("BA", "Kelp")] {
        let shown = team(&dirty, typed, &look())
            .unwrap_or_else(|why| panic!("{typed}, as the page shows it, found nothing: {why}"));
        assert!(
            shown.contains(found),
            "{typed} found another team than {found}:\n{shown}"
        );
    }
}
