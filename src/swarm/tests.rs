//! The swarm drawn from the teams and sessions a node holds.

use serde_json::{Value, json};

use super::{Look, Swarm, WHOLE, aligned, overview, team, typed};

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
fn a_colour_s_escape_takes_no_column_where_marks_line_up() {
    assert_eq!(
        aligned("\u{1b}[1mab\u{1b}[0m\u{fdd0}  1\nabcd\u{fdd0}  2"),
        "\u{1b}[1mab\u{1b}[0m    1\nabcd  2",
        "a painted lead was padded by its characters rather than the columns it takes"
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
}
