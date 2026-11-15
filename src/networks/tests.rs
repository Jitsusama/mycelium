//! The mycelium built from records as a node holds them, and drawn.

use serde_json::{Value, json};

use super::kept::{RECORD, answered, paths, pruned};
use super::made::asks;
use super::{Is, Mycelium, Shape, cell, cells, network, overview};
use crate::daemon::fields::{MOST_FIELDS, projected};
use crate::swarm::{ASKS_FIT, Look, Shapes, together};

/// A look 80 columns wide, without colour, at 17:00 on Thursday
/// 12 November 2026, in UTC.
fn look() -> Look {
    Look {
        width: 80,
        colour: false,
        // UTC as jiff holds it, since a build has no zone database.
        now: "2026-11-12T17:00:00Z"
            .parse::<jiff::Timestamp>()
            .expect("a time")
            .to_zoned(jiff::tz::TimeZone::UTC),
        shapes: Shapes::default(),
    }
}

/// The same look, with a theme marking a triangle `^` and a square `#`.
fn marked() -> Look {
    let theme = json!({"marks": {
        "shapeTriangle": {"glyph": "^", "console": "A"},
        "shapeSquare": {"glyph": "#", "console": "S"},
    }});
    Look {
        shapes: Shapes::of(&theme, false),
        ..look()
    }
}

/// A session record of `address`, standing as `state`, hearing `groups`;
/// its id a UUID version 7 of 15:00 that day, `n` milliseconds on.
fn session(address: &str, n: u8, title: &str, state: &str, groups: &[&str]) -> Value {
    json!({
        "key": format!("session:{address}"),
        "id": format!("01a1d045-d5{:02x}-7000-8000-000000000000", 0x80 + n),
        "address": address,
        "title": title,
        "state": state,
        "machine": "skiff",
        "groups": groups,
        "quest": "QEST-20261112-HARBOR",
    })
}

/// `session` working on the quest of `short`, titled `title`.
fn on_quest(mut session: Value, short: &str, title: &str) -> Value {
    session["presence"] = json!({"questInfo": {"short": short, "title": title}});
    session
}

/// A crowd of `many` sessions in repositories of two each, the first
/// `asking` of them asking their person, the next few busy, the rest
/// idle.
fn crowd(many: u8, asking: u8) -> Mycelium {
    let sessions: Vec<Value> = (0..many)
        .map(|n| {
            let address = format!("CROWD{n:02}");
            let state = if n >= asking && n < asking + 3 {
                "busy"
            } else {
                "idle"
            };
            let repo = format!("repo.r{:02}", n / 2);
            let mut session = session(&address, n, &address, state, &[repo.as_str()]);
            if n < asking {
                session["wants"] = json!({"said": "Which way?"});
            }
            session
        })
        .collect();
    Mycelium::of("work", &[], &sessions, &[])
}

#[test]
fn a_long_list_of_cells_folds_to_who_needs_you_the_busiest_and_a_count() {
    let crowd = crowd(40, 2);
    let shown = cells(&crowd, &look(), false, &[]);
    let lines: Vec<&str> = shown.lines().collect();
    let asking = lines
        .iter()
        .filter(|line| line.starts_with("needs you"))
        .count();
    let moving = lines
        .iter()
        .filter(|line| line.starts_with("moving"))
        .count();
    let quiet = lines
        .iter()
        .filter(|line| line.starts_with("quiet"))
        .count();
    assert_eq!(
        (asking, moving, quiet),
        (2, 3, 2),
        "each needing its person, then the busiest five:\n{shown}"
    );
    assert!(
        lines.contains(&"and 33 more: 33 quiet"),
        "the rest counted on one line:\n{shown}"
    );
    assert_eq!(
        lines.last(),
        Some(&"some lists folded; see every one with: mycelium --realm work cells --all"),
        "and how to see them:\n{shown}"
    );
    let all = cells(&crowd, &look(), true, &[]);
    assert_eq!(
        all.lines().filter(|line| line.starts_with("quiet")).count(),
        35,
        "asked for all, nothing folds:\n{all}"
    );
    assert!(!all.contains("folded"), "{all}");
}

#[test]
fn cells_by_standing_are_only_those_standing_so() {
    let crowd = crowd(40, 2);
    let shown = cells(&crowd, &look(), false, &["needs-you".to_owned()]);
    let standings: Vec<&str> = shown
        .lines()
        .skip(2)
        .take_while(|line| !line.is_empty())
        .map(|line| line.split("  ").next().unwrap_or_default())
        .collect();
    assert_eq!(standings, ["needs you", "needs you"], "{shown}");
    let quiet = ["quiet".to_owned()];
    assert_eq!(
        crowd
            .cells
            .iter()
            .filter(|cell| cell.stands_as(&quiet))
            .count(),
        35
    );
}

#[test]
fn cells_by_a_standing_none_stand_as_say_so_plainly() {
    let crowd = crowd(3, 0);
    let asked = |standing: &[&str]| {
        let standing: Vec<String> = standing.iter().map(|&each| each.to_owned()).collect();
        cells(&crowd, &look(), false, &standing)
            .lines()
            .nth(2)
            .unwrap_or_default()
            .to_owned()
    };
    assert_eq!(
        (
            asked(&["needs-you", "stuck"]),
            asked(&["stuck"]),
            asked(&["needs-you", "stuck", "dormant"]),
        ),
        (
            "Nothing needs you and nothing is stuck.".to_owned(),
            "Nothing is stuck.".to_owned(),
            "Nothing needs you, nothing is stuck and nothing is dormant.".to_owned(),
        ),
        "in place of an empty table"
    );
}

#[test]
fn a_long_list_of_networks_folds_to_the_worrying_the_busiest_and_a_row_for_the_rest() {
    let crowd = crowd(40, 2);
    let shown = overview(&crowd, &look(), false, false);
    let repositories: Vec<&str> = shown
        .lines()
        .skip_while(|line| *line != "repositories")
        .skip(1)
        .take_while(|line| line.starts_with("  "))
        .collect();
    let names: Vec<&str> = repositories
        .iter()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(
        names,
        ["r00", "r01", "r02", "r03", "r04", "and"],
        "the one needing its person, the four busiest, then the rest:\n{shown}"
    );
    let folded = repositories.last().copied().unwrap_or_default();
    assert!(
        folded.trim_start().starts_with("and 15 more networks")
            && folded.trim_end().ends_with("30"),
        "the rest counted, their thirty quiet cells too:\n{shown}"
    );
    assert!(
        shown.ends_with(
            "some lists folded; see every one with: mycelium --realm work networks --all"
        ),
        "{shown}"
    );
    let all = overview(&crowd, &look(), false, true);
    assert!(!all.contains("more networks"), "{all}");
}

/// The harbour: Harbour's orchestration with Kelp within it, three
/// sessions of which one asks its person, one dormant, one ended, and
/// two machines.
fn mycelium() -> Mycelium {
    let [teams, sessions, machines] = records();
    Mycelium::of("work", &teams, &sessions, &machines)
}

/// The harbour's teams, sessions and machines, as the node lists them.
fn records() -> [Vec<Value>; 3] {
    let teams = vec![
        json!({"key": "team:HARBOR", "title": "Harbour Ships", "within": null,
               "lead": "01a1d045-d581-7000-8000-000000000000"}),
        json!({"key": "team:KELP01", "title": "Kelp: Tides", "within": "HARBOR",
               "glyph": "triangle"}),
    ];
    let mut asking = on_quest(
        session(
            "ASKS01",
            2,
            "π - Kelp's q… - skiff",
            "idle",
            &["orchestration.kelp01", "quest.harbor.kelp01", "repo.skiff"],
        ),
        "KELP01",
        "Kelp's question",
    );
    asking["wants"] = json!({"said": "Which tide?"});
    let mut busy = on_quest(
        session(
            "BUSY01",
            1,
            "Busy one",
            "busy",
            &[
                "orchestration.harbor",
                "quest.harbor",
                "repo.skiff",
                "peer.busy01",
            ],
        ),
        "HARBOR",
        "Harbour Ships",
    );
    busy["step"] = json!("running the tests");
    busy["presence"]["repo"] = json!("skiff");
    busy["presence"]["usage"] = json!({
        "context": {"limit": 1000, "used": 250},
        "pi.cost.usd": {"last": 0.05, "today": 2.5},
        "turns": {"last": {"at": "2026-11-12T16:59:00Z"}, "today": 12},
    });
    let mut parked = session(
        "NAPS01",
        3,
        "Napping one",
        "ended",
        &[
            "orchestration.harbor",
            "quest.harbor",
            "dock.crew",
            "repo.oars",
        ],
    );
    parked["standing"] = json!("parked");
    let gone = session("GONE01", 4, "Gone", "ended", &["repo.skiff"]);
    let machines = vec![
        json!({"name": "skiff", "state": "ready", "model_name": "Skiff 14",
               "room": {"pressure": {"cpu": 41.6}}}),
        json!({"name": "dinghy", "state": "ready"}),
    ];
    [teams, vec![asking, busy, parked, gone], machines]
}

#[test]
fn answers_are_read_by_their_ask_as_they_stream_in_and_a_hang_up_is_said() {
    let sent = concat!(
        r#"{"id":0,"hello":{"version":1}}"#,
        "\n",
        r#"{"id":2,"records":[{"address":"BUSY01","landings":[1,2]}]}"#,
        "\n",
        r#"{"records":[{"address":"LATE01"}],"id":1,"error":null}"#,
        "\n",
        r#"{"id":3,"error":{"message":"no such kind"}}"#,
        "\n",
    );
    let mut records = Vec::new();
    let answers = answered(sent.as_bytes(), 3, |id, record| records.push((id, record)))
        .expect("each answered");
    assert_eq!(answers[&3]["error"]["message"], "no such kind");
    assert_eq!(answers[&2].get("records"), None, "records aren't held");
    assert_eq!(
        records,
        [
            (2, json!({"address": "BUSY01"})),
            (1, json!({"address": "LATE01"}))
        ],
        "each record is handed on, pruned, as answering its ask, even before its id"
    );
    let hung_up = answered(sent.as_bytes(), 4, |_, _| ()).expect_err("four never answers");
    assert_eq!(hung_up.to_string(), "the node hung up before it answered");
}

#[test]
fn reading_only_what_the_networks_read_makes_the_same_mycelium() {
    let [teams, mut sessions, machines] = records();
    sessions[1]["landings"] = json!([{"ref": "world#1", "what": "A landing"}]);
    sessions[1]["presence"]["owner"] = json!({"pid": 1});
    let kept = |records: &[Value]| -> Vec<Value> {
        records.iter().map(|each| pruned(each, &RECORD)).collect()
    };
    let (lean, whole) = (kept(&sessions), mycelium());
    assert_eq!(lean[1].get("landings"), None);
    assert_eq!(lean[1]["presence"].get("owner"), None);
    let read = Mycelium::of("work", &kept(&teams), &lean, &kept(&machines));
    assert_eq!(read.lines(), whole.lines());

    let fields = paths(&RECORD);
    assert!(
        fields.len() <= MOST_FIELDS,
        "a node answers a list of {} fields: {fields:?}",
        fields.len()
    );
    let projected = |records: &[Value]| -> Vec<Value> {
        let each = records.iter().cloned();
        each.map(|record| projected(record, &fields)).collect()
    };
    let sent = Mycelium::of(
        "work",
        &projected(&teams),
        &projected(&sessions),
        &projected(&machines),
    );
    assert_eq!(
        sent.lines(),
        whole.lines(),
        "a node sending only those fields makes the same mycelium"
    );
}

#[test]
fn every_group_but_an_address_is_a_network_and_a_lone_quest_or_repo_is_not() {
    let mycelium = mycelium();
    let names: Vec<(&str, Shape)> = mycelium
        .networks
        .iter()
        .map(|each| (each.name.as_str(), each.shape))
        .collect();
    assert_eq!(
        names,
        [
            ("orchestration.kelp01", Shape::Orchestration),
            ("orchestration.harbor", Shape::Orchestration),
            ("quest.harbor", Shape::Project),
            ("repo.skiff", Shape::Repository),
            ("dock.crew", Shape::Group),
            ("machines", Shape::Machines),
        ],
        "needing its person first within each shape; kelp01's quest and oars are one cell's own"
    );
    let kelp = mycelium.network("orchestration.kelp01").expect("Kelp");
    assert_eq!(
        (
            kelp.short.as_str(),
            kelp.title.as_deref(),
            kelp.within.as_deref(),
            kelp.glyph.as_deref()
        ),
        (
            "kelp",
            Some("Kelp: Tides"),
            Some("orchestration.harbor"),
            Some("triangle")
        ),
        "named, nested and shaped as the swarm's team is"
    );
    let harbour = mycelium.network("quest.harbor").expect("Harbour's quest");
    assert_eq!(
        (
            harbour.title.as_deref(),
            harbour.cells.clone(),
            harbour.count(crate::swarm::Standing::Dormant)
        ),
        (
            Some("Harbour Ships"),
            vec!["BUSY01".to_owned(), "NAPS01".to_owned()],
            1
        ),
        "a quest two cells share, titled as its quest, the parked one dormant"
    );
}

#[test]
fn cells_are_the_living_and_dormant_sessions_then_the_machines() {
    let mycelium = mycelium();
    let cells: Vec<(&str, Is, Option<&str>)> = mycelium
        .cells
        .iter()
        .map(|cell| (cell.name.as_str(), cell.is, cell.standing))
        .collect();
    assert_eq!(
        cells,
        [
            ("BUSY01", Is::Session, Some("moving")),
            ("ASKS01", Is::Session, Some("needs-you")),
            ("NAPS01", Is::Session, Some("dormant")),
            ("skiff", Is::Machine, None),
            ("dinghy", Is::Machine, None),
        ],
        "in the order each began, the ended one left out"
    );
    let busy = mycelium.cell("BUSY01").expect("the busy one");
    assert_eq!(
        (busy.since.as_deref(), busy.networks.len()),
        (Some("2026-11-12T15:00:00.001Z"), 3),
        "its start from its id, and no network for its own address"
    );
    let skiff = mycelium.cell("skiff").expect("skiff");
    assert_eq!(
        (skiff.busy, skiff.runs, skiff.networks.clone()),
        (Some(41.6), Some(3), vec!["machines".to_owned()]),
        "a machine says how busy it is and how many sessions it runs"
    );
}

#[test]
fn json_lines_say_each_network_then_each_cell() {
    let lines = mycelium().lines();
    assert_eq!(lines.len(), 11, "six networks and five cells");
    assert_eq!(
        lines[0],
        json!({"kind": "network", "name": "orchestration.kelp01",
               "shape": "orchestration", "short": "kelp", "title": "Kelp: Tides",
               "glyph": "triangle",
               "within": "orchestration.harbor", "cells": ["ASKS01"],
               "machines": ["skiff"],
               "counts": {"needs-you": 1, "stuck": 0, "moving": 0, "quiet": 0, "dormant": 0}}),
        "a network whole"
    );
    assert_eq!(
        lines[7],
        json!({"kind": "cell", "name": "ASKS01", "type": "session",
               "title": "Kelp's question",
               "id": "01a1d045-d582-7000-8000-000000000000",
               "window": "π - Kelp's q… - skiff", "standing": "needs-you",
               "doing": "Which tide?", "asks": "Which tide?", "machine": "skiff",
               "since": "2026-11-12T15:00:00.002Z", "quest": "QEST-20261112-HARBOR",
               "networks": ["orchestration.kelp01", "repo.skiff"]}),
        "a session cell whole"
    );
    assert_eq!(
        (&lines[6]["leads"], &lines[6]["repo"], &lines[6]["usage"]),
        (
            &json!(["orchestration.harbor"]),
            &json!("skiff"),
            &json!({"cost_today": 2.5, "context": {"limit": 1000, "used": 250},
                    "turns_today": 12, "last_turn": "2026-11-12T16:59:00Z"})
        ),
        "the busy one leads Harbour, works in skiff and says what it spent"
    );
}

#[test]
fn json_lines_say_each_cells_plan_and_folder_then_what_the_cells_did_newest_first() {
    let doing = json!({"plan": "PLAN-20261115-8CYRTK", "item": 9,
                       "next": "Serve the events", "at": "2026-11-15T15:50:00Z"});
    let mut busy = session("REEF01", 1, "Reef", "busy", &[]);
    busy["doing"] = doing.clone();
    busy["presence"] = json!({"cwd": "/home/kestrel/src/skiff"});
    busy["events"] = json!([
        {"at": "2026-11-15T15:40:00Z", "kind": "push",
         "where": "github:kestrel/skiff", "title": "Push the oars"},
        {"at": "2026-11-15T15:20:00Z", "kind": "commit",
         "where": "github:kestrel/skiff", "title": "Carve the oars"},
    ]);
    let mut idle = session("REEF02", 2, "Reef too", "idle", &[]);
    idle["events"] = json!([{"at": "2026-11-15T15:30:00Z", "kind": "stage",
                             "where": "QEST-20261112-HARBOR", "title": "build"}]);
    let mut ended = session("REEF03", 3, "Reef gone", "ended", &[]);
    ended["events"] = json!([{"at": "2026-11-15T15:45:00Z", "kind": "merge",
                              "where": "github:kestrel/skiff", "title": "Gone"}]);
    let mycelium = Mycelium::of("work", &[], &[busy, idle, ended], &[]);
    let reef = mycelium.cell("REEF01").expect("the busy cell");
    assert_eq!(
        (&reef.plan, reef.cwd.as_deref()),
        (&Some(doing), Some("/home/kestrel/src/skiff")),
        "a cell says its plan's line and the folder it works in"
    );
    assert_eq!(
        mycelium.cell("REEF02").map(|cell| (&cell.plan, &cell.cwd)),
        Some((&None, &None)),
        "and says neither where its presence doesn't"
    );
    let events: Vec<Value> = mycelium
        .lines()
        .into_iter()
        .filter(|line| line["kind"] == "event")
        .collect();
    assert_eq!(
        events.first(),
        Some(&json!({"kind": "event", "cell": "REEF01",
                     "session": "01a1d045-d581-7000-8000-000000000000",
                     "at": "2026-11-15T15:40:00Z", "what": "push",
                     "where": "github:kestrel/skiff", "title": "Push the oars"})),
        "an event is a line of its own, after the cells"
    );
    assert_eq!(
        events
            .iter()
            .map(|line| (&line["cell"], &line["what"]))
            .collect::<Vec<_>>(),
        [
            (&json!("REEF01"), &json!("push")),
            (&json!("REEF02"), &json!("stage")),
            (&json!("REEF01"), &json!("commit")),
        ],
        "newest first across the cells, none of a session that has ended"
    );
    assert_eq!(
        mycelium.lines().last().map(|line| line["kind"].clone()),
        Some(json!("event")),
        "the events follow every cell"
    );
}

#[test]
fn a_networks_ask_reads_what_a_cell_says_it_does_and_did() {
    let asked = paths(&RECORD);
    for field in ["doing", "events", "presence.cwd"] {
        assert!(
            asked.iter().any(|path| path == field),
            "{field} is asked of the node: {asked:?}"
        );
    }
}

#[test]
fn a_repository_is_titled_by_the_name_its_group_was_made_from() {
    let in_repo = |address: &str, n: u8, repo: Option<&str>| {
        let mut cell = session(address, n, address, "idle", &["repo.skiff-rs", "repo.oars"]);
        cell["presence"] = json!({"repo": repo});
        cell
    };
    let mycelium = Mycelium::of(
        "work",
        &[],
        &[
            in_repo("REEF01", 1, None),
            in_repo("REEF02", 2, Some("Skiff.rs")),
            in_repo("REEF03", 3, Some("skiff.rs")),
        ],
        &[],
    );
    let titled = |name: &str| {
        let network = mycelium.network(name).expect("the repository");
        network.title.clone()
    };
    assert_eq!(
        (titled("repo.skiff-rs"), titled("repo.oars")),
        (Some("Skiff.rs".to_owned()), None),
        "the first cell's name its group's spells, and none where no cell's does"
    );
    let tree = overview(&mycelium, &look(), false, false);
    assert!(
        tree.contains("Skiff.rs") && !tree.contains("skiff-rs"),
        "drawn by its name, without the group's spelling of it again:\n{tree}"
    );
}

#[test]
fn the_tree_draws_each_shape_its_networks_nested_and_the_machines() {
    let shown = overview(&mycelium(), &look(), false, false);
    let expected = [
        "work · 3 sessions in 6 networks on 2 machines · 1 needs you",
        "",
        "                                         need you  stuck  moving  quiet  dormant",
        "orchestrations",
        "  Harbour Ships                                                1               1",
        "    kelp      Kelp: Tides                       1",
        "projects",
        "  harbor    Harbour Ships                                      1               1",
        "repositories",
        "  skiff                                         1              1",
        "groups",
        "  dock/crew                                                                    1",
        "machines",
        "  skiff     ready · 42% busy                    1              1               1",
        "  dinghy    ready",
        "",
        "see one by any part of its name: mycelium --realm work networks kelp",
    ];
    assert_eq!(
        shown.lines().collect::<Vec<_>>(),
        expected,
        "the tree:\n{shown}"
    );
}

#[test]
fn an_orchestration_wears_the_mark_its_shape_is_drawn_by() {
    let mycelium = mycelium();
    let tree = overview(&mycelium, &marked(), false, false);
    assert!(
        tree.lines()
            .any(|line| line == "    kelp      ^ Kelp: Tides                     1")
            && tree
                .lines()
                .any(|line| line.starts_with("  Harbour Ships ")),
        "Kelp's triangle before its title, and none on Harbour, which has no shape:\n{tree}"
    );
    let kelp = network(&mycelium, "kelp", &marked(), false).expect("Kelp");
    assert!(
        kelp.starts_with("^ Kelp: Tides · orchestration kelp"),
        "its heading wears it too:\n{kelp}"
    );
    let asking = mycelium.cell("ASKS01").expect("the asking session");
    let joined = cell(&mycelium, asking, &marked());
    assert!(
        joined.lines().any(|line| line.ends_with("^ Kelp: Tides")),
        "and so does the cell's list of its networks:\n{joined}"
    );
}

#[test]
fn with_cells_each_network_has_its_sessions_under_it() {
    let shown = overview(&mycelium(), &look(), true, false);
    let under_kelp = shown
        .lines()
        .skip_while(|line| !line.starts_with("    kelp "))
        .nth(1)
        .unwrap_or_default();
    assert!(
        under_kelp.starts_with("      needs you  ASKS01  Kelp's question ")
            && under_kelp.ends_with(" Which tide?"),
        "Kelp's session under Kelp:\n{shown}"
    );
}

#[test]
fn one_network_says_what_it_is_within_and_its_cells() {
    let shown = network(&mycelium(), "kelp", &look(), false).expect("found by its short name");
    let expected = [
        "Kelp: Tides · orchestration kelp · within Harbour Ships",
        "1 cell on skiff",
        "",
        "needs you  ASKS01  Kelp's question                 Which tide?",
    ];
    assert_eq!(
        shown.lines().collect::<Vec<_>>(),
        expected,
        "Kelp:\n{shown}"
    );
    let by_group =
        network(&mycelium(), "#dock/crew", &look(), false).expect("found as the mesh spells it");
    assert!(by_group.starts_with("dock/crew · group"), "{by_group}");
}

#[test]
fn an_orchestrator_renamed_draws_one_network_under_its_own_address() {
    // A node too old to fold the old address still says its team.
    let teams = [
        json!({"key": "team:KELP01", "title": "Kelp Beds"}),
        json!({"key": "team:HARBOR", "title": "HARBOR"}),
    ];
    let mut lead = session("KELP01", 1, "Kelp Beds", "busy", &["orchestration.kelp01"]);
    lead["presence"] = json!({"addresses": ["KELP01", "HARBOR", "EF5B865E"]});
    let sessions = [
        lead,
        session(
            "OLD001",
            2,
            "Spawned before",
            "idle",
            &["orchestration.harbor"],
        ),
        session(
            "NEW001",
            3,
            "Spawned since",
            "busy",
            &["orchestration.kelp01"],
        ),
    ];
    let mycelium = Mycelium::of("work", &teams, &sessions, &[]);
    let orchestrations: Vec<(&str, Option<&str>, &[String])> = mycelium
        .networks
        .iter()
        .filter(|each| each.shape == Shape::Orchestration)
        .map(|each| {
            (
                each.name.as_str(),
                each.title.as_deref(),
                each.cells.as_slice(),
            )
        })
        .collect();
    assert_eq!(
        orchestrations,
        [(
            "orchestration.kelp01",
            Some("Kelp Beds"),
            [
                "KELP01".to_owned(),
                "OLD001".to_owned(),
                "NEW001".to_owned()
            ]
            .as_slice()
        )],
        "one network, titled, holding those answering either address"
    );
    assert_eq!(
        mycelium.cell("OLD001").map(|cell| cell.networks.clone()),
        Some(vec!["orchestration.kelp01".to_owned()]),
        "a session spawned before the rename is in the orchestrator's own network"
    );
}

#[test]
fn a_name_matching_none_or_several_networks_is_said_plainly() {
    let mycelium = mycelium();
    assert_eq!(
        mycelium.named_network("ebb").map(|each| &each.name),
        Err("no network is named ebb".to_owned()),
        "none"
    );
    assert_eq!(
        mycelium
            .named_network("harbor")
            .map(|each| each.name.as_str()),
        Ok("quest.harbor"),
        "a short name picks its one network"
    );
    assert_eq!(
        mycelium.named_network("ships").map(|each| &each.name),
        Err("ships could be orchestration.harbor or quest.harbor".to_owned()),
        "several, each by its whole name"
    );
    assert_eq!(
        mycelium
            .named_network("quest/harbor")
            .map(|each| each.name.as_str()),
        Ok("quest.harbor"),
        "the whole name picks one"
    );
}

#[test]
fn every_cell_lists_worst_first_and_one_says_its_networks() {
    let mycelium = mycelium();
    let shown = cells(&mycelium, &look(), false, &[]);
    let expected = [
        "work · 3 sessions in 6 networks on 2 machines · 1 needs you",
        "",
        "needs you  ASKS01  Kelp's question                 Which tide?",
        "moving     BUSY01  Harbour Ships                   running the tests",
        "dormant    NAPS01  Napping one",
        "",
        "machines",
        "  skiff   ready · 42% busy · runs 3",
        "  dinghy  ready · runs 0",
        "",
        "see one by its name: mycelium --realm work cells <name>",
    ];
    assert_eq!(
        shown.lines().collect::<Vec<_>>(),
        expected,
        "cells:\n{shown}"
    );
    let one = mycelium.named_cell("busy01").expect("found by its name");
    let shown = cell(&mycelium, one, &look());
    let expected = [
        "BUSY01 · Harbour Ships",
        "moving · running the tests",
        "on skiff · since 15:00 · QEST-20261112-HARBOR",
        "",
        "networks",
        "  harbour-ships  orchestration  Harbour Ships",
        "  harbor         project        Harbour Ships",
        "  skiff          repository",
    ];
    assert_eq!(
        shown.lines().collect::<Vec<_>>(),
        expected,
        "one cell:\n{shown}"
    );
}

#[test]
fn the_networks_asks_fit_one_write_a_node_takes_whole() {
    let asked = together(&asks());

    assert!(
        asked.len() <= ASKS_FIT,
        "{} bytes asked, past {ASKS_FIT}: a node writing its first answer \
         would leave the rest unread",
        asked.len()
    );
}
