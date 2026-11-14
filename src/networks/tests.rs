//! The mycelium built from records as a node holds them, and drawn.

use serde_json::{Value, json};

use super::{Is, Mycelium, Shape, cell, cells, network, overview};
use crate::swarm::Look;

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

/// The harbour: Harbour's orchestration with Kelp within it, three
/// sessions of which one asks its person, one dormant, one ended, and
/// two machines.
fn mycelium() -> Mycelium {
    let teams = [
        json!({"key": "team:HARBOR", "title": "Harbour Ships", "within": null}),
        json!({"key": "team:KELP01", "title": "Kelp: Tides", "within": "HARBOR"}),
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
    let machines = [
        json!({"name": "skiff", "state": "ready", "model_name": "Skiff 14",
               "room": {"pressure": {"cpu": 41.6}}}),
        json!({"name": "dinghy", "state": "ready"}),
    ];
    Mycelium::of("work", &teams, &[asking, busy, parked, gone], &machines)
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
            kelp.within.as_deref()
        ),
        ("kelp", Some("Kelp: Tides"), Some("orchestration.harbor")),
        "named and nested as the swarm's team is"
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
               "within": "orchestration.harbor", "cells": ["ASKS01"],
               "machines": ["skiff"],
               "counts": {"needs-you": 1, "stuck": 0, "moving": 0, "quiet": 0, "dormant": 0}}),
        "a network whole"
    );
    assert_eq!(
        lines[7],
        json!({"kind": "cell", "name": "ASKS01", "type": "session",
               "title": "Kelp's question", "standing": "needs-you",
               "doing": "Which tide?", "machine": "skiff",
               "since": "2026-11-12T15:00:00.002Z", "quest": "QEST-20261112-HARBOR",
               "networks": ["orchestration.kelp01", "repo.skiff"]}),
        "a session cell whole"
    );
}

#[test]
fn the_tree_draws_each_shape_its_networks_nested_and_the_machines() {
    let shown = overview(&mycelium(), &look(), false);
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
fn with_cells_each_network_has_its_sessions_under_it() {
    let shown = overview(&mycelium(), &look(), true);
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
    let shown = network(&mycelium(), "kelp", &look()).expect("found by its short name");
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
        network(&mycelium(), "#dock/crew", &look()).expect("found as the mesh spells it");
    assert!(by_group.starts_with("dock/crew · group"), "{by_group}");
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
    let shown = cells(&mycelium, &look());
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
