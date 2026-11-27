//! The mycelium built from records as a node holds them, and drawn.

use serde_json::{Value, json};

use super::kept::{RECORD, answered, paths, pruned};
use super::made::asks;
use super::{Is, Mycelium, Shape, cell, cells, network, overview};
use crate::daemon::fields::{MOST_FIELDS, projected};
use crate::forage::one_line;
use crate::swarm::{ASKS_FIT, Look, Shapes, WHOLE, together};

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
    // Two cells hold one tree, so the tree's network is read too.
    for held in &mut sessions[..2] {
        held["presence"]["trees"] = json!([tides()]);
    }
    // Each starter a cell says is read from what the networks read.
    sessions[0]["orchestrated_by"] = json!("BUSY01");
    sessions[1]["started_by"] = json!("ASKS01");
    let kept = |records: &[Value]| -> Vec<Value> {
        records.iter().map(|each| pruned(each, &RECORD)).collect()
    };
    let lean = kept(&sessions);
    let whole = Mycelium::of("work", &teams, &sessions, &machines);
    assert!(
        whole.networks.iter().any(|each| each.shape == Shape::Tree),
        "the harbour's two cells share a tree"
    );
    let starter = |address: &str| {
        let cell = whole.cell(address).expect("its cell");
        (cell.started_by.clone(), cell.started_by_from)
    };
    assert_eq!(
        (starter("ASKS01"), starter("BUSY01")),
        (
            (Some("BUSY01".to_owned()), Some("presence")),
            (Some("ASKS01".to_owned()), Some("book"))
        ),
        "each cell says its starter"
    );
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
        let each = records
            .iter()
            .map(|record| projected(&record.to_string(), &fields));
        each.map(|sent| serde_json::from_str(sent.get()).expect("a projected record is JSON"))
            .collect()
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

/// A busy session `address`, numbered `n`, whose presence holds `trees`.
fn holding(address: &str, n: u8, trees: Value) -> Value {
    let mut held = session(address, n, "", "busy", &[]);
    held["presence"] = json!({});
    held["presence"]["trees"] = trees;
    held
}

/// Each tree network of `mycelium`: its name, short name and cells.
fn trees(mycelium: &Mycelium) -> Vec<(&str, &str, Vec<String>)> {
    mycelium
        .networks
        .iter()
        .filter(|each| each.shape == Shape::Tree)
        .map(|each| (each.name.as_str(), each.short.as_str(), each.cells.clone()))
        .collect()
}

/// A worktree of `github:harbour/oars` at `jsama/tides`.
fn tides() -> Value {
    json!({
        "key": "worktree-github-harbour-oars-jsama-tides",
        "repo": "github:harbour/oars",
        "branch": "jsama/tides",
    })
}

#[test]
fn a_tree_is_named_by_its_whole_repo_key_and_no_group_joins_one() {
    // Another owner's repository of the same name, at the same branch.
    let theirs = json!({
        "key": "worktree-github-other-oars-jsama-tides",
        "repo": "github:other/oars",
        "branch": "jsama/tides",
    });
    let mut posing = holding("TERN01", 3, json!([theirs]));
    posing["groups"] = json!(["tree.github:harbour/oars:jsama/tides"]);
    let sessions = [
        holding("ROWS01", 1, json!([tides()])),
        holding("OARS01", 2, json!([tides()])),
        posing,
    ];
    let mycelium = Mycelium::of("home", &[], &sessions, &[]);
    assert_eq!(
        trees(&mycelium),
        [(
            "tree.github:harbour/oars:jsama/tides",
            "oars@jsama/tides",
            vec!["ROWS01".to_owned(), "OARS01".to_owned()]
        )],
        "another owner's repo of the same name is another tree, and a group named as a \
         tree joins none"
    );
    assert_eq!(
        mycelium.cell("TERN01").expect("Tern").networks,
        Vec::<String>::new(),
        "so a tree held alone is no network"
    );
}

#[test]
fn a_tree_two_cells_formed_stays_a_network_while_one_holds_it() {
    let name = "tree.github:harbour/oars:jsama/tides";
    let formed = std::collections::BTreeSet::from([name.to_owned()]);
    let unformed = std::collections::BTreeSet::new();
    let one = || [holding("ROWS01", 1, json!([tides()]))];
    let made = |sessions: Vec<Value>, formed| {
        Mycelium::of_records("home", &[], sessions, &[], &[], &[], formed)
    };
    assert_eq!(
        trees(&made(one().to_vec(), &unformed)),
        [],
        "a tree one cell holds, never shared, is no network"
    );
    let kept = made(one().to_vec(), &formed);
    assert_eq!(
        trees(&kept),
        [(name, "oars@jsama/tides", vec!["ROWS01".to_owned()])],
        "formed once two held it, it stays while one does"
    );
    assert_eq!(
        kept.cell("ROWS01").expect("Rows").networks,
        [name.to_owned()],
        "and its cell is on it"
    );
    assert_eq!(
        trees(&made(Vec::new(), &formed)),
        [],
        "with no cell holding it, it is drawn no more"
    );
}

/// A busy session `address`, numbered `n`, whose presence says it
/// shares `shares`.
fn sharing(address: &str, n: u8, shares: &Value) -> Value {
    let mut held = session(address, n, "", "busy", &[]);
    held["presence"] = json!({"shares": shares});
    held
}

/// The folder its quest's scratch is on its machine, as one cell says
/// it: keyed by the quest, for what the quest is.
fn scratch(folder: &str) -> Value {
    json!({"key": "quest:QEST-20261112-HARBOR", "for": "Harbour Ships", "folder": folder})
}

/// The network of the folder [`scratch`] says, its key read in lower
/// case.
const HARBOUR: &str = "files.quest:qest-20261112-harbor";

/// Each shared files network of `mycelium`: its name, short name,
/// title and cells.
fn files(mycelium: &Mycelium) -> Vec<(&str, &str, Option<&str>, Vec<String>)> {
    mycelium
        .networks
        .iter()
        .filter(|each| each.shape == Shape::Files)
        .map(|each| {
            let title = each.title.as_deref();
            (
                each.name.as_str(),
                each.short.as_str(),
                title,
                each.cells.clone(),
            )
        })
        .collect()
}

#[test]
fn a_folder_two_cells_share_by_key_is_a_network_named_for_what_it_is_for() {
    let mut posing = sharing("TERN01", 3, &json!([]));
    posing["groups"] = json!([
        "files.quest:QEST-20261112-HARBOR",
        "quest:qest-20261112-harbor",
        "files",
        "files.sync",
    ]);
    let sessions = [
        // Each machine keeps its own copy, so one key is one network
        // though their folders differ.
        sharing("ROWS01", 1, &json!([scratch("/home/rows/scratch")])),
        sharing("OARS01", 2, &json!([scratch("/Users/oars/scratch")])),
        posing,
        sharing(
            "KELP01",
            4,
            &json!([{"key": "quest:QEST-20261112-KELPED", "for": "Kelp", "folder": "/k"},
                    {"key": "quest:QEST-20261112-KELPSY", "for": 7, "folder": "/s"},
                    {"key": "quest:QEST-20261112-KELPOB", "for": {"a": [1, {"b": null}]},
                     "folder": "/o"},
                    {"key": "quest:QEST-20261112-KELPAR", "for": [true, -1.5],
                     "folder": "/a"},
                    {"key": "quest:QEST-20261112-KELPBO", "for": false, "folder": "/b"},
                    {"key": "quest:QEST-20261112-KELPNE", "for": -7, "folder": "/n"},
                    {"key": "quest:QEST-20261112-KELPFL", "for": 0.5, "folder": "/f"},
                    {"key": "quest:QEST-20261112-KELPNU", "for": null, "folder": "/u"}]),
        ),
    ];
    let mycelium = Mycelium::of("home", &[], &sessions, &[]);
    let name = HARBOUR;
    assert_eq!(
        files(&mycelium),
        [(
            name,
            "quest:qest-20261112-harbor",
            Some("Harbour Ships"),
            vec!["ROWS01".to_owned(), "OARS01".to_owned()]
        )],
        "one key two cells say is one network, titled by what it is for; one cell's own \
         is none, and a group named as one joins none"
    );
    let rows = mycelium.cell("ROWS01").expect("Rows");
    assert_eq!(
        (
            rows.networks.clone(),
            serde_json::to_value(&rows.shares).expect("shares")
        ),
        (
            vec![name.to_owned()],
            json!([{"key": "quest:qest-20261112-harbor", "for": "Harbour Ships",
                    "folder": "/home/rows/scratch"}])
        ),
        "its cells are on it, each saying its own folder"
    );
    assert_eq!(
        (
            mycelium.cell("TERN01").expect("Tern").networks.clone(),
            mycelium.cell("KELP01").expect("Kelp").networks.clone()
        ),
        (
            vec!["files".to_owned(), "files.sync".to_owned()],
            Vec::<String>::new()
        ),
        "a folder shared alone is no network; a group called files, or under it without a \
         key's colon, is a group; and one holding a colon, as no mesh group does, is none"
    );
    let drawn = overview(&mycelium, &look(), false, true);
    assert!(
        drawn.contains("folders") && drawn.contains("quest:qest-20261112-harbor"),
        "the shared folders are drawn under their heading: {drawn}"
    );
    let rows = cell(&mycelium, rows, &look());
    assert!(
        rows.contains("quest:qest-20261112-harbor") && !rows.contains("its own folders"),
        "a cell's shared folders are among its networks: {rows}"
    );
    let kelped = mycelium.cell("KELP01").expect("Kelp");
    assert_eq!(
        serde_json::to_value(kelped.shares.get(1..)).expect("shares"),
        json!([{"key": "quest:qest-20261112-kelpsy", "folder": "/s"},
               {"key": "quest:qest-20261112-kelpob", "folder": "/o"},
               {"key": "quest:qest-20261112-kelpar", "folder": "/a"},
               {"key": "quest:qest-20261112-kelpbo", "folder": "/b"},
               {"key": "quest:qest-20261112-kelpne", "folder": "/n"},
               {"key": "quest:qest-20261112-kelpfl", "folder": "/f"},
               {"key": "quest:qest-20261112-kelpnu", "folder": "/u"}]),
        "a share saying what it is for as anything but text (a number of each kind, a \
         truth, a list, a whole object or null) is kept, untitled, and its record leaves \
         out what it doesn't say"
    );
    let kelp = cell(&mycelium, kelped, &look());
    assert!(
        kelp.ends_with(
            "its own folders\n  quest:qest-20261112-kelped  Kelp\n  quest:qest-20261112-kelpsy\n  \
             quest:qest-20261112-kelpob\n  quest:qest-20261112-kelpar\n  \
             quest:qest-20261112-kelpbo\n  quest:qest-20261112-kelpne\n  \
             quest:qest-20261112-kelpfl\n  quest:qest-20261112-kelpnu"
        ),
        "a cell lists the folders it shares alone as its own, an untitled one's line \
         ending at its key: {kelp}"
    );
}

#[test]
fn a_shared_folder_is_titled_by_what_most_of_its_cells_say_it_is_for() {
    let titled = |address: &str, n: u8, about: &str| {
        let share = json!({"key": "quest:QEST-20261112-HARBOR", "for": about, "folder": "/q"});
        sharing(address, n, &json!([share]))
    };
    let title = |sessions: &[Value]| {
        let mycelium = Mycelium::of("home", &[], sessions, &[]);
        files(&mycelium)
            .first()
            .map(|(_, _, title, cells)| (title.map(str::to_owned), cells.clone()))
    };
    let renamed = [
        titled("ROWS01", 1, "Kelp"),
        titled("OARS01", 2, "Harbour Ships"),
        titled("KELP01", 3, "Harbour Ships"),
        titled("TERN01", 4, "Harbour Ships"),
        titled("SKIF01", 5, "Kelp"),
        titled("REEF01", 6, ""),
        titled("BUOY01", 7, ""),
        titled("DOCK01", 8, ""),
    ];
    let (most, cells) = title(&renamed).expect("a network");
    assert_eq!(
        (most.as_deref(), cells.len()),
        (Some("Harbour Ships"), 8),
        "what most say, though the earliest, the latest and the greatest say otherwise, \
         and those saying nothing, though on it, have no say"
    );
    let (untitled, _) =
        title(&[titled("ROWS01", 1, ""), titled("OARS01", 2, "")]).expect("a network");
    assert_eq!(untitled, None, "where none says, none");
    let even = [
        titled("ROWS01", 1, "Harbour"),
        titled("OARS01", 2, "Harbour Ships"),
    ];
    let (tied, _) = title(&even).expect("a network");
    assert_eq!(
        tied, None,
        "where two things are said as often, none, so no cell's order names it"
    );
    let split = [
        titled("ROWS01", 1, "Harbour"),
        titled("OARS01", 2, "Harbour"),
        titled("KELP01", 3, "Harbour Ships"),
        titled("TERN01", 4, "Harbour Ships"),
        titled("SKIF01", 5, "Kelp"),
    ];
    let (most_tied, _) = title(&split).expect("a network");
    assert_eq!(
        most_tied, None,
        "nor where the two said most are said as often, though a third is said less"
    );
}

#[test]
fn a_folder_two_cells_shared_stays_a_network_while_one_says_it_and_goes_once_faded() {
    let name = HARBOUR;
    let formed = std::collections::BTreeSet::from([name.to_owned()]);
    let faded = std::collections::BTreeSet::new();
    let one = || {
        vec![sharing(
            "ROWS01",
            1,
            &json!([scratch("/home/rows/scratch")]),
        )]
    };
    let made = |sessions: Vec<Value>, formed| {
        Mycelium::of_records("home", &[], sessions, &[], &[], &[], formed)
    };
    let kept = made(one(), &formed);
    assert_eq!(
        files(&kept),
        [(
            name,
            "quest:qest-20261112-harbor",
            Some("Harbour Ships"),
            vec!["ROWS01".to_owned()]
        )],
        "formed once two shared it, it stays while one does"
    );
    assert_eq!(
        kept.cell("ROWS01").expect("Rows").networks,
        [name.to_owned()],
        "and its cell is on it"
    );
    let gone = made(one(), &faded);
    assert_eq!(
        (
            files(&gone),
            gone.cell("ROWS01").expect("Rows").networks.clone()
        ),
        (Vec::new(), Vec::<String>::new()),
        "faded, it is gone from the networks and from its cell, though one says it"
    );
}

#[test]
fn a_folder_is_picked_by_its_key_and_a_group_called_files_by_its_name() {
    let mut grouped = sharing("TERN01", 3, &json!([]));
    grouped["groups"] = json!(["files", "files.sync"]);
    let sessions = [
        sharing("ROWS01", 1, &json!([scratch("/home/rows/scratch")])),
        sharing("OARS01", 2, &json!([scratch("/Users/oars/scratch")])),
        grouped,
    ];
    let mycelium = Mycelium::of("home", &[], &sessions, &[]);
    let picked = |asked: &str| mycelium.named_network(asked).map(|each| each.name.clone());
    assert_eq!(
        [
            picked("quest:QEST-20261112-HARBOR"),
            picked("files"),
            picked("files/sync"),
        ],
        [
            Ok(HARBOUR.to_owned()),
            Ok("files".to_owned()),
            Ok("files.sync".to_owned()),
        ],
        "a folder is picked by its key, whatever its case, and the groups beside it by theirs"
    );
    let drawn =
        network(&mycelium, "quest:qest-20261112-harbor", &look(), false).expect("found by its key");
    assert!(
        drawn
            .lines()
            .next()
            .is_some_and(|head| head.ends_with("Harbour Ships · folder quest:qest-20261112-harbor")),
        "its heading says it is a folder, by its key: {drawn}"
    );
}

#[test]
fn a_cell_shares_at_most_sixteen_folders() {
    let keys = |sessions: &[Value]| -> Vec<String> {
        let mycelium = Mycelium::of("home", &[], sessions, &[]);
        let rows = mycelium.cell("ROWS01").expect("Rows");
        rows.shares.iter().map(|share| share.key.clone()).collect()
    };
    let many: Vec<Value> = (0..20)
        .map(|n| json!({"key": format!("quest:Q{n:02}"), "for": "Q", "folder": "/q"}))
        .collect();
    let first: Vec<String> = (0..16).map(|n| format!("quest:q{n:02}")).collect();
    assert_eq!(
        keys(&[sharing("ROWS01", 1, &json!(many))]),
        first,
        "the first sixteen it says, and no more"
    );
    let mut misfits: Vec<Value> = (0..16).map(|_| json!({"key": ""})).collect();
    misfits.push(json!({"key": "quest:late", "for": "Late", "folder": "/l"}));
    assert_eq!(
        keys(&[sharing("ROWS01", 1, &json!(misfits))]),
        Vec::<String>::new(),
        "the first sixteen it says are read, though none of them fits"
    );
}

/// Each folder network two cells make, both saying each of `said`, as
/// its short name and title: at most [`super::SHARES_MOST`] said, so
/// none is passed over for their count alone.
fn shared_by_two(said: &[Value]) -> Vec<(String, Option<String>)> {
    assert!(said.len() <= 16, "each is read: {}", said.len());
    let sessions = [
        sharing("ROWS01", 1, &json!(said)),
        sharing("OARS01", 2, &json!(said)),
    ];
    let mycelium = Mycelium::of("home", &[], &sessions, &[]);
    files(&mycelium)
        .iter()
        .map(|(_, short, title, _)| ((*short).to_owned(), title.map(str::to_owned)))
        .collect()
}

#[test]
fn a_share_past_its_keys_or_folders_bounds_is_passed_over() {
    let keys = [
        json!({"key": "", "for": "Empty", "folder": "/e"}),
        json!({"key": format!("quest:{}", "k".repeat(91)), "for": "Long", "folder": "/l"}),
        json!({"key": "quest:a.b", "for": "Dotted", "folder": "/d"}),
        json!({"key": "quest:a b", "for": "Spaced", "folder": "/s"}),
        json!({"key": ":quest", "for": "Colon first", "folder": "/c"}),
        json!({"key": "quest:", "for": "Of nothing", "folder": "/n"}),
        json!({"key": "harness", "for": "No kind", "folder": "/h"}),
    ];
    assert_eq!(
        shared_by_two(&keys),
        [],
        "a key past 96, holding a dot, a space or no colon, beginning with none of a \
         letter or digit, or saying nothing after its colon, is passed over"
    );
    let folders = [
        json!({"key": "quest:unfoldered", "for": "Nowhere", "folder": ""}),
        json!({"key": "quest:blank", "for": "Blank", "folder": "\t\u{202E} "}),
        json!({"key": "quest:said", "for": "Said"}),
        json!({"key": "quest:deep", "for": "Deep", "folder": "f".repeat(1025)}),
        json!({"key": "quest:wide", "for": "Wide", "folder": "é".repeat(513)}),
    ];
    assert_eq!(
        shared_by_two(&folders),
        [],
        "a share saying no folder, or none but what a terminal can't draw, or a folder \
         past 1024 bytes, though of 513 characters, is passed over"
    );
    let fits = format!("quest:{}", "k".repeat(90));
    let kept = [
        json!({"key": fits, "for": "Fits", "folder": "/f"}),
        json!({"key": "quest:roomy", "for": "Roomy", "folder": "é".repeat(512)}),
        json!({"key": "quest:twice", "for": "Twice", "folder": "/one"}),
        json!({"key": "QUEST:Twice", "for": "Again", "folder": "/two"}),
    ];
    let said = |short: &str, title: &str| (short.to_owned(), Some(title.to_owned()));
    assert_eq!(
        shared_by_two(&kept),
        [
            said(&fits, "Fits"),
            said("quest:roomy", "Roomy"),
            said("quest:twice", "Twice"),
        ],
        "a key of 96 letters, digits, colons, dashes and underscores is shared, in lower \
         case, and a folder of 1024 bytes is within bounds"
    );
}

#[test]
fn a_shares_title_is_cut_and_cleaned_to_draw_and_never_refuses_its_folder() {
    let long = |c: &str| c.repeat(121);
    let said = [
        json!({"key": "quest:titled", "for": long("t"), "folder": "/t"}),
        json!({"key": "quest:accented", "for": long("é"), "folder": "/a"}),
        json!({"key": "quest:cut", "for": format!("{}\t\u{202E}x", "c".repeat(119)),
               "folder": "/c"}),
        json!({"key": "quest:tabbed", "for": "Tab\there", "folder": "/t\nb"}),
        json!({"key": "quest:turned", "for": "fly\u{202E}nwod\u{2066}", "folder": "/t"}),
        json!({"key": "quest:lined", "for": "Lined\u{2028}up\u{2029}", "folder": "/l"}),
        json!({"key": "quest:joined", "for": "👩\u{200D}💻 Harbour", "folder": "/j"}),
        json!({"key": "quest:persian", "for": "می\u{200C}خواهم", "folder": "/p"}),
        json!({"key": "quest:hidden", "for": "Tide\u{200B} plan\u{FDD0}", "folder": "/h"}),
        json!({"key": "quest:unchars", "for": "\u{2800}Tide\u{FDD5} \u{FFFE}plan\u{1FFFF}",
               "folder": "/x"}),
        json!({"key": "quest:brailled", "for": "Tide\u{2800}\u{200D}\u{2800}", "folder": "/r"}),
        json!({"key": "quest:hinged", "for": format!("{}\u{200D}é", "é".repeat(119)),
               "folder": "/g"}),
        json!({"key": "quest:untitled", "for": "", "folder": "/u"}),
        json!({"key": "quest:unsaid", "folder": "/u"}),
        json!({"key": "quest:nulled", "for": null, "folder": "/n"}),
        json!({"key": "quest:blank", "for": "\u{202E}\t", "folder": "/b"}),
    ];
    let titled = |short: &str, title: &str| (short.to_owned(), Some(title.to_owned()));
    let untitled = |short: &str| (short.to_owned(), None);
    assert_eq!(
        shared_by_two(&said),
        [
            titled("quest:accented", &"é".repeat(120)),
            untitled("quest:blank"),
            titled("quest:brailled", "Tide"),
            titled("quest:cut", &"c".repeat(119)),
            titled("quest:hidden", "Tide plan"),
            titled("quest:hinged", &"é".repeat(119)),
            titled("quest:joined", "👩\u{200D}💻 Harbour"),
            titled("quest:lined", "Lined up"),
            untitled("quest:nulled"),
            titled("quest:persian", "می\u{200C}خواهم"),
            titled("quest:tabbed", "Tab here"),
            titled("quest:titled", &"t".repeat(120)),
            titled("quest:turned", "flynwod"),
            titled("quest:unchars", "Tide plan"),
            untitled("quest:unsaid"),
            untitled("quest:untitled"),
        ],
        "a title past 120 characters is cut to them, however many bytes each is, counting \
         what is drawn; a control character or separator in it is a space and a mark \
         turning its direction is gone, as is what draws nothing or a noncharacter, a braille \
         blank at either end too, and a cut ends in no space or lone joiner; the joiners an \
         emoji or a \
         script spells with stay; and a share saying \
         nothing or null it is for is kept, \
         untitled"
    );
    // Apart, as a cell reads no more than sixteen of what it shares.
    let khmer = format!("{}\u{17D2}\u{1780}", "\u{1780}".repeat(119));
    assert_eq!(
        shared_by_two(&[json!({"key": "quest:khmer", "for": khmer, "folder": "/k"})]),
        [titled("quest:khmer", &"\u{1780}".repeat(119))],
        "a cut ends in no lone Khmer coeng"
    );
}

#[test]
fn a_key_said_twice_whatever_its_case_is_its_first_and_a_cut_title_keeps_no_more_room() {
    let said = [
        json!({"key": "quest:accented", "for": "é".repeat(121), "folder": "/a"}),
        json!({"key": "quest:twice", "for": "Twice", "folder": "/one"}),
        json!({"key": "QUEST:Twice", "for": "Again", "folder": "/two"}),
    ];
    let rows = Mycelium::of("home", &[], &[sharing("ROWS01", 1, &json!(said))], &[]);
    let rows = rows.cell("ROWS01").expect("Rows");
    assert_eq!(
        (
            rows.shares.len(),
            serde_json::to_value(rows.shares.last()).expect("share")
        ),
        (
            2,
            json!({"key": "quest:twice", "for": "Twice", "folder": "/one"})
        ),
        "a key said twice, whatever its case, is the first one alone"
    );
    assert!(
        rows.shares
            .iter()
            .all(|share| share.about.capacity() <= 240),
        "and a title cut keeps no more room than its 120 characters take"
    );
    for again in ["quest:twice", "QUEST:Twice"] {
        let twice = [
            json!({"key": "quest:twice", "for": "Twice", "folder": "/one"}),
            json!({"key": again, "for": "Twice", "folder": "/two"}),
        ];
        let alone = Mycelium::of("home", &[], &[sharing("ROWS01", 1, &json!(twice))], &[]);
        assert_eq!(
            files(&alone),
            [],
            "one cell saying a key twice, as {again}, is no network"
        );
    }
}

#[test]
fn two_trees_whose_short_names_clash_are_told_apart_by_their_owners() {
    let theirs = json!({
        "key": "worktree-github-other-oars-jsama-tides",
        "repo": "github:other/oars",
        "branch": "jsama/tides",
    });
    let ours = json!({
        "key": "worktree-local-oars-jsama-tides",
        "repo": "local:oars",
        "branch": "jsama/tides",
    });
    let sessions = [
        holding("ROWS01", 1, json!([tides(), ours])),
        holding("OARS01", 2, json!([tides(), ours])),
        holding("TERN01", 3, json!([theirs])),
        holding("KELP01", 4, json!([theirs])),
    ];
    let mycelium = Mycelium::of("home", &[], &sessions, &[]);
    let shorts: Vec<&str> = trees(&mycelium).iter().map(|tree| tree.1).collect();
    assert_eq!(
        shorts,
        [
            "harbour/oars@jsama/tides",
            "other/oars@jsama/tides",
            "local:oars@jsama/tides"
        ],
        "a short name two trees would share says the owner, or the whole key where \
         there is none"
    );
}

#[test]
fn a_tree_only_parked_cells_hold_is_dormant_and_after_a_live_one() {
    let parked = |address: &str, n: u8| {
        let mut napping = holding(address, n, json!([tides()]));
        napping["state"] = json!("ended");
        napping["standing"] = json!("parked");
        napping
    };
    let pinned = json!({
        "key": "snapshot-local-skiff-6b881064c2f1",
        "repo": "local:skiff",
        "commit": "6b881064c2f1d9a3e57b0c48f2e6a1d9c3b75e04",
    });
    let sessions = [
        parked("NAPS01", 1),
        parked("NAPS02", 2),
        parked("NAPS03", 3),
        holding("ROWS01", 4, json!([pinned])),
        holding("OARS01", 5, json!([pinned])),
    ];
    let mycelium = Mycelium::of("home", &[], &sessions, &[]);
    let counted: Vec<(&str, Vec<(&str, u64)>)> = mycelium
        .networks
        .iter()
        .filter(|each| each.shape == Shape::Tree)
        .map(|each| {
            let counts = each.counts.iter().filter(|(_, many)| **many > 0);
            let counts = counts.map(|(standing, many)| (*standing, *many)).collect();
            (each.short.as_str(), counts)
        })
        .collect();
    assert_eq!(
        counted,
        [
            ("skiff@6b88106", vec![("moving", 2)]),
            ("oars@jsama/tides", vec![("dormant", 3)]),
        ],
        "parked cells still hold their tree, which is dormant, after a live one however \
         many hold it"
    );
}

#[test]
fn a_tree_two_cells_hold_is_a_network_and_one_cells_own_is_on_the_cell() {
    let tides = tides();
    let pinned = json!({
        "key": "snapshot-local-skiff-6b881064c2f1",
        "repo": "local:skiff",
        "commit": "6b881064c2f1d9a3e57b0c48f2e6a1d9c3b75e04",
    });
    let sessions = [
        holding("ROWS01", 1, json!([tides, pinned])),
        holding("OARS01", 2, json!([tides])),
        holding(
            "KELP01",
            3,
            json!([pinned, {"key": "a tree saying nothing else"}]),
        ),
        holding(
            "NAPS01",
            4,
            json!([{
                "key": "snapshot-local-skiff-0226ece3409c",
                "repo": "local:skiff",
                "commit": "0226ece3409ce7f303fd08f070dc4fba6a288334",
            }]),
        ),
    ];
    let mycelium = Mycelium::of("home", &[], &sessions, &[]);
    assert_eq!(
        trees(&mycelium),
        [
            (
                "tree.github:harbour/oars:jsama/tides",
                "oars@jsama/tides",
                vec!["ROWS01".to_owned(), "OARS01".to_owned()]
            ),
            (
                "tree.local:skiff:6b881064c2f1d9a3e57b0c48f2e6a1d9c3b75e04",
                "skiff@6b88106",
                vec!["ROWS01".to_owned(), "KELP01".to_owned()]
            ),
        ],
        "a worktree or a snapshot two cells hold is a network, named by its repo's whole \
         key and its branch or whole commit; one cell's own is none"
    );
    let naps = mycelium.cell("NAPS01").expect("Naps");
    assert_eq!(
        (
            serde_json::to_value(&naps.trees).expect("trees"),
            naps.networks.clone()
        ),
        (
            json!([{
                "key": "snapshot-local-skiff-0226ece3409c",
                "repo": "local:skiff",
                "commit": "0226ece3409ce7f303fd08f070dc4fba6a288334",
            }]),
            Vec::<String>::new()
        ),
        "a tree one cell holds alone is shown on the cell, in no network"
    );
    let kelp = mycelium.cell("KELP01").expect("Kelp");
    assert_eq!(
        (kelp.trees.len(), kelp.networks.clone()),
        (
            1,
            vec!["tree.local:skiff:6b881064c2f1d9a3e57b0c48f2e6a1d9c3b75e04".to_owned()]
        ),
        "a tree saying no repo is passed over, and a shared one names its network"
    );
    let drawn = overview(&mycelium, &look(), false, true);
    assert!(
        drawn.contains("trees") && drawn.contains("oars@jsama/tides"),
        "the tree heads its shared trees: {drawn}"
    );
    let rows = cell(&mycelium, mycelium.cell("ROWS01").expect("Rows"), &look());
    assert!(
        rows.contains("oars@jsama/tides") && !rows.contains("its own trees"),
        "a cell's shared trees are among its networks: {rows}"
    );
    let naps = cell(&mycelium, naps, &look());
    assert!(
        naps.ends_with("its own trees\n  skiff@0226ece"),
        "a cell lists the trees it holds alone as its own: {naps}"
    );
}

#[test]
fn a_session_here_whose_process_ended_is_dormant_only_on_its_nodes_word() {
    // Seven sessions here ended with their quests active and nothing to wake
    // them, yet the map drew each dormant for hours, since their presences
    // still said parked (16 Nov). Here, only the node's own standing counts;
    // a session elsewhere has only its presence's word, which this node
    // can't check. A node that says no `here` is believed as before. Read
    // as the command reads a node, only the fields it asks for.
    let ended = |address: &str, n: u8, here: Option<bool>| {
        let mut record = session(address, n, "Ended", "ended", &["repo.oars"]);
        record["presence"] = json!({"state": "parked"});
        if let Some(here) = here {
            record["here"] = json!(here);
        }
        pruned(&record, &RECORD)
    };
    let mut wakes = ended("WAKES1", 1, Some(true));
    wakes["standing"] = json!("parked");
    // Parking, its process not yet gone: its presence's word stands.
    let mut parking = ended("LIVES1", 5, Some(true));
    parking["state"] = json!("idle");
    parking["standing"] = json!("running");
    let sessions = [
        wakes,
        ended("DEAD01", 2, Some(true)),
        ended("AWAY01", 3, Some(false)),
        ended("OLDER1", 4, None),
        parking,
    ];
    let mycelium = Mycelium::of("home", &[], &sessions, &[]);
    let cells: Vec<(&str, Option<&str>)> = mycelium
        .cells
        .iter()
        .map(|cell| (cell.name.as_str(), cell.standing))
        .collect();
    assert_eq!(
        cells,
        [
            ("WAKES1", Some("dormant")),
            ("AWAY01", Some("dormant")),
            ("OLDER1", Some("dormant")),
            ("LIVES1", Some("dormant")),
        ],
        "one here ended with nothing to wake it is no cell; one here still running, one \
         elsewhere and one from a node that says no here are dormant as their presences say"
    );
}

#[test]
fn a_cells_starter_is_its_machines_books_else_its_orchestrators_and_only_an_address() {
    let saying = |address: &str, n: u8, fields: Value| {
        let mut one = session(address, n, address, "busy", &[]);
        for (key, value) in fields.as_object().expect("fields") {
            one[key] = value.clone();
        }
        one
    };
    let sessions = [
        saying(
            "BOOK01",
            1,
            json!({"started_by": "OAR001", "orchestrated_by": "WCJQSG"}),
        ),
        saying("PRES01", 2, json!({"orchestrated_by": "WCJQSG"})),
        saying("NONE01", 3, json!({"orchestrated_by": "not an address"})),
        saying(
            "ODDS01",
            4,
            json!({"started_by": "not an address", "orchestrated_by": "WCJQSG"}),
        ),
        saying(
            "SELF01",
            5,
            json!({"started_by": "SELF01", "orchestrated_by": "SELF01"}),
        ),
        saying(
            "ALSO01",
            6,
            json!({
                "started_by": "ALSO02",
                "orchestrated_by": "ALSO02",
                "presence": {"addresses": ["ALSO01", "ALSO02"]},
            }),
        ),
        saying(
            "CASE01",
            7,
            json!({
                "started_by": "case01",
                "orchestrated_by": "case02",
                "presence": {"addresses": ["CASE01", "CASE02"]},
            }),
        ),
        saying(
            "OWNS01",
            8,
            json!({"started_by": "OWNS01", "orchestrated_by": "WCJQSG"}),
        ),
    ];
    let mycelium = Mycelium::of("work", &[], &sessions, &[]);
    let starter = |address: &str| {
        let cell = mycelium.cell(address).expect("its cell");
        (cell.started_by.as_deref(), cell.started_by_from)
    };
    assert_eq!(
        starter("BOOK01"),
        (Some("OAR001"), Some("book")),
        "the book's, over its orchestrator"
    );
    assert_eq!(
        starter("PRES01"),
        (Some("WCJQSG"), Some("presence")),
        "its orchestrator, with no book's"
    );
    assert_eq!(starter("NONE01"), (None, None), "no address, no starter");
    assert_eq!(
        starter("ODDS01"),
        (Some("WCJQSG"), Some("presence")),
        "a book's word that is no address gives way to its orchestrator"
    );
    assert_eq!(starter("SELF01"), (None, None), "no cell starts itself");
    assert_eq!(starter("ALSO01"), (None, None), "by any address it goes by");
    assert_eq!(starter("CASE01"), (None, None), "in any case");
    assert_eq!(
        starter("OWNS01"),
        (Some("WCJQSG"), Some("presence")),
        "a book naming the cell itself gives way to its orchestrator"
    );
    let line = mycelium
        .lines()
        .into_iter()
        .find(|line| line["name"] == "BOOK01")
        .expect("its line");
    assert_eq!(
        (&line["started_by"], &line["started_by_from"]),
        (&json!("OAR001"), &json!("book")),
        "as its line says: {line}"
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

/// The harbour's cell ASKS01 as its line says it, its want asking in
/// `asked`.
fn asking_whole(asked: &str) -> Value {
    let [teams, mut sessions, machines] = records();
    let asking = sessions
        .iter_mut()
        .find(|session| session["address"] == "ASKS01")
        .expect("the harbour's asking session");
    asking["wants"] = json!({"said": "Which tide?", "asked": asked});
    Mycelium::of("work", &teams, &sessions, &machines)
        .lines()
        .into_iter()
        .find(|line| line["name"] == "ASKS01")
        .expect("its cell")
}

#[test]
fn a_cell_carries_its_question_whole_its_lines_kept_and_nothing_that_steers() {
    let cell = asking_whole(
        "The ebb runs at six.\r\nThe flood at noon.\n\n\u{202E}Which tide?\u{1b}[2J \u{2028}1. ebb",
    );
    assert_eq!(
        (&cell["asks"], &cell["asked"]),
        (
            &json!("Which tide?"),
            &json!("The ebb runs at six.\nThe flood at noon.\n\nWhich tide? [2J \n1. ebb")
        ),
        "its paragraphs and lines kept, a return, an escape and a turn gone"
    );
    assert_eq!(
        asking_whole(" \n\u{200F} ").get("asked"),
        None,
        "nothing to read says none"
    );
}

#[test]
fn a_question_past_its_most_is_cut_and_says_so() {
    let cell = asking_whole(&"tide ".repeat(1000));
    let asked = cell["asked"].as_str().expect("its question");
    assert_eq!(
        (
            asked.chars().count() <= super::ASKED_MOST,
            asked.ends_with("tide [...]")
        ),
        (true, true),
        "{asked}"
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
        lines[9],
        json!({"kind": "cell", "name": "skiff", "type": "machine",
               "title": "Skiff 14", "state": "ready", "busy": 41.6, "runs": 3,
               "networks": ["machines"]}),
        "a machine cell whole"
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
    for field in ["doing", "events", "presence.cwd", "presence.trees"] {
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
fn the_hint_gives_only_an_example_a_shell_reads_as_one_name() {
    let hint = |short: &str| {
        let mut mycelium = mycelium();
        mycelium.networks[0].short = short.to_owned();
        let shown = overview(&mycelium, &look(), false, false);
        shown
            .lines()
            .last()
            .and_then(|line| line.rsplit(' ').next())
            .map(str::to_owned)
    };
    // Every miss is gathered, so undoing the rule names each case.
    let mut misread = Vec::new();
    for (given, said) in [
        ("kelp; curl x|sh", "a peer's short name a shell acts on"),
        ("kelp tides", "or splits"),
        ("--cells", "or networks reads as an option"),
    ] {
        if hint(given).as_deref() != Some("<network>") {
            misread.push(said);
        }
    }
    assert!(
        misread.is_empty(),
        "each is given as no example: {misread:?}"
    );
    assert_eq!(
        (hint("dock/crew"), hint("oars@jsama/tides")),
        (
            Some("dock/crew".to_owned()),
            Some("oars@jsama/tides".to_owned())
        ),
        "a group's and a tree's names are each one word"
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
fn an_orchestration_no_team_leads_is_named_as_its_quest_and_holds_its_cells() {
    // Two sessions of one quest hear its orchestration's group, while none
    // answers to it, so the node says no team of it.
    let sessions = [
        on_quest(
            session(
                "OSPREY",
                1,
                "Ship the Dive From the Cache",
                "busy",
                &["orchestration.osprey", "quest.osprey"],
            ),
            "OSPREY",
            "Ship the Dive From the Cache",
        ),
        on_quest(
            session(
                "TERN01",
                2,
                "Ship the Dive From the Cache",
                "busy",
                &["orchestration.osprey", "quest.osprey"],
            ),
            "OSPREY",
            "Ship the Dive From the Cache",
        ),
    ];
    let teams = [json!({"key": "team:KELP01", "title": "Kelp Beds"})];
    let mycelium = Mycelium::of("work", &teams, &sessions, &[]);
    let named: Vec<(&str, &str, Option<&str>, &[String])> = mycelium
        .networks
        .iter()
        .filter(|each| matches!(each.shape, Shape::Orchestration | Shape::Project))
        .map(|each| {
            (
                each.name.as_str(),
                each.short.as_str(),
                each.title.as_deref(),
                each.cells.as_slice(),
            )
        })
        .collect();
    let both = ["OSPREY".to_owned(), "TERN01".to_owned()];
    assert_eq!(
        named,
        [
            (
                "orchestration.osprey",
                "ship",
                Some("Ship the Dive From the Cache"),
                both.as_slice()
            ),
            (
                "orchestration.kelp01",
                "kelp-beds",
                Some("Kelp Beds"),
                &[][..]
            ),
            (
                "quest.osprey",
                "osprey",
                Some("Ship the Dive From the Cache"),
                both.as_slice()
            ),
        ],
        "the leaderless orchestration is still a network, named as its quest is"
    );
}

#[test]
fn a_network_with_no_leader_never_changes_a_teams_short_name() {
    // The real team's title shares its first word with the leaderless
    // quest's, so handing out short names together would move the team's.
    let teams = [json!({"key": "team:KELP01", "title": "Ship the Kelp Beds"})];
    let sessions = [
        on_quest(
            session(
                "OSPREY",
                1,
                "Ship the Dive",
                "busy",
                &["orchestration.osprey"],
            ),
            "OSPREY",
            "Ship the Dive From the Cache",
        ),
        // No quest of its address is known, so it has nothing truer to go by.
        session("SKIFFS", 2, "Skiff", "busy", &["orchestration.ternss"]),
    ];
    let mycelium = Mycelium::of("work", &teams, &sessions, &[]);
    let network = |name: &str| {
        mycelium
            .networks
            .iter()
            .find(|each| each.name == name)
            .map(|each| (each.short.clone(), each.title.clone()))
    };
    let swarm = crate::swarm::short_names(&teams);
    assert_eq!(
        network("orchestration.kelp01").map(|(short, _)| short),
        swarm.get("KELP01").cloned(),
        "the team keeps the short name the swarm steers it by"
    );
    let (short, title) = network("orchestration.osprey").expect("a network with no leader");
    assert_eq!(title.as_deref(), Some("Ship the Dive From the Cache"));
    assert_ne!(Some(&short), swarm.get("KELP01"), "never the team's name");
    assert_eq!(
        network("orchestration.ternss"),
        Some(("ternss".to_owned(), None)),
        "with no quest known, it goes by its address"
    );
}

#[test]
fn a_teams_short_name_that_is_a_sessions_address_is_never_shown() {
    // Typed after swarm, it would reach the session BUSY, not Kelp.
    let teams = [json!({"key": "team:KELP01", "title": "Kelp Beds", "swarm": "busy"})];
    let sessions = [session(
        "BUSY",
        1,
        "Busy one",
        "busy",
        &["orchestration.kelp01"],
    )];
    let mycelium = Mycelium::of("work", &teams, &sessions, &[]);
    let kelp = mycelium
        .networks
        .iter()
        .find(|each| each.name == "orchestration.kelp01")
        .expect("Kelp's network");
    assert_ne!(
        kelp.short, "busy",
        "networks showed a name that steers the session BUSY"
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
    // A group's name is a peer's to pick, as a short name is, so one
    // named like another's short name leaves the two to choose between.
    let mut grouped = self::mycelium();
    let mut group = grouped.networks[0].clone();
    group.name = "harbor".to_owned();
    group.short = "harbor".to_owned();
    group.title = None;
    grouped.networks.push(group);
    assert!(
        grouped
            .named_network("harbor")
            .is_err_and(|why| why.starts_with("harbor could be ") && why.contains("quest.harbor")),
        "a peer's group took a network's short name"
    );
}

#[test]
fn what_a_name_could_be_names_each_on_one_line() {
    let (cell, network) = ("ASKS\u{1b}]0;x\u{7}01", "quest.\u{200b}harbor\u{fdd0}");
    assert_eq!(
        super::several("as", &[&cell, &network], |each| *each),
        format!("as could be {} or quest.harbor", one_line(cell)),
        "a name it could be drew otherwise than on one line"
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
fn a_wide_cell_name_keeps_every_title_in_line() {
    let [teams, mut sessions, machines] = records();
    let asking = sessions
        .iter_mut()
        .find(|session| session["address"] == "ASKS01")
        .expect("the asking session");
    // Four characters, wider in cells than the others' six and narrower
    // counted by character, so either way of counting shows.
    asking["address"] = json!("港湾港湾");
    let mycelium = Mycelium::of("work", &teams, &sessions, &machines);
    let shown = cells(&mycelium, &look(), false, &[]);
    // Measured by Unicode's widths directly, not by the code under test,
    // as nothing here is coloured.
    let at = |title: &str| {
        shown.lines().find_map(|line| {
            line.find(title)
                .map(|at| <str as unicode_width::UnicodeWidthStr>::width(&line[..at]))
        })
    };
    assert!(
        at("Kelp's question").is_some() && at("Kelp's question") == at("Harbour Ships"),
        "a wide cell name moved its title:\n{shown}"
    );
}

#[test]
fn a_wide_machine_name_keeps_what_follows_in_line() {
    let [teams, sessions, mut machines] = records();
    machines[1]["name"] = json!("港湾港");
    let mycelium = Mycelium::of("work", &teams, &sessions, &machines);
    let shown = cells(&mycelium, &look(), false, &[]);
    // Measured by Unicode's widths directly, not by the code under test,
    // as nothing here is coloured.
    let at = |name: &str| {
        shown
            .lines()
            .find(|line| line.starts_with(&format!("  {name}")))
            .and_then(|line| {
                line.find("ready")
                    .map(|at| <str as unicode_width::UnicodeWidthStr>::width(&line[..at]))
            })
    };
    assert!(
        at("skiff").is_some() && at("skiff") == at("港湾港"),
        "a wide machine name moved what follows it:\n{shown}"
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
fn drawn_whole_the_tree_is_as_wide_as_its_widest_row_and_its_counts_line_up() {
    let shown = overview(&mycelium(), &whole(), false, false);
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(
        lines[2..15],
        [
            "                              need you  stuck  moving  quiet  dormant",
            "orchestrations",
            "  Harbour Ships                                     1               1",
            "    kelp      Kelp: Tides            1",
            "projects",
            "  harbor    Harbour Ships                           1               1",
            "repositories",
            "  skiff                              1              1",
            "groups",
            "  dock/crew                                                         1",
            "machines",
            "  skiff     ready · 42% busy         1              1               1",
            "  dinghy    ready",
        ],
        "the tree, whole:\n{shown}"
    );
}

#[test]
fn drawn_whole_a_cell_s_marks_and_breaks_move_no_column() {
    let drawn = |title: &str, step: &str| {
        let [teams, mut sessions, machines] = records();
        for session in &mut sessions {
            if session["address"] == "ASKS01" {
                session["presence"]["questInfo"]["title"] = json!(title);
            }
            if session["address"] == "BUSY01" {
                session["step"] = json!(step);
            }
        }
        let mycelium = Mycelium::of("work", &teams, &sessions, &machines);
        [
            overview(&mycelium, &whole(), true, false),
            cells(&mycelium, &whole(), false, &[]),
        ]
        .join("\n")
    };
    let clean = drawn("Kelp's question", "running the  tests");
    assert!(
        clean.contains("running the  tests"),
        "the step is drawn:\n{clean}"
    );
    assert_eq!(
        drawn(
            "Kelp's\u{fdd0} question\u{fdd1}",
            "running\u{fdd0} the\n\ntests"
        ),
        clean,
        "a mark or a break in a cell's title or doing moved a column or split a block"
    );
}

#[test]
fn drawn_whole_a_cell_s_title_keeps_every_mark_it_was_written_with() {
    // Into a pipe a reader that greps or saves the text has all of it,
    // so a title is drawn as written, though a cut would take a pile of
    // marks this long at any width.
    let title = format!("Kelp{}", "\u{301}".repeat(100));
    let [teams, mut sessions, machines] = records();
    let asking = sessions
        .iter_mut()
        .find(|session| session["address"] == "ASKS01")
        .expect("the asking session");
    asking["presence"]["questInfo"]["title"] = json!(&title);
    let mycelium = Mycelium::of("work", &teams, &sessions, &machines);
    let shown = cells(&mycelium, &whole(), false, &[]);
    assert!(
        shown.contains(&title),
        "a cell's title lost marks drawn whole:\n{shown}"
    );
}

#[test]
fn a_peer_s_escape_break_or_mark_reaches_no_page() {
    // Kelp's own title heads its page and names its network on the
    // others, so it is dirtied beside its session's.
    let pages = |(team, title): (&str, &str), step: &str, machine: (&str, &str), said: &str| {
        let [mut teams, mut sessions, mut machines] = records();
        teams[1]["title"] = json!(team);
        let mut lent = builders();
        lent[1]["said"] = json!(said);
        for session in &mut sessions {
            if session["address"] == "ASKS01" {
                session["presence"]["questInfo"]["title"] = json!(title);
            }
            if session["address"] == "BUSY01" {
                session["step"] = json!(step);
            }
        }
        machines[1]["name"] = json!(machine.0);
        machines[1]["state"] = json!(machine.1);
        let mycelium = Mycelium::of("work", &teams, &sessions, &machines).with_builders(&lent);
        let page = |name: &str| {
            let found = mycelium.cell(name).expect("the session is a cell");
            cell(&mycelium, found, &look())
        };
        let one = |name: &str| network(&mycelium, name, &whole(), false).expect("a network");
        [
            overview(&mycelium, &whole(), true, false),
            cells(&mycelium, &whole(), false, &[]),
            cells(&mycelium, &look(), false, &[]),
            page("ASKS01"),
            page("BUSY01"),
            one("kelp"),
            one("machines"),
        ]
    };
    let clean = pages(
        ("Kelp: Tides", "Kelp's question"),
        "running the tests",
        ("dinghy", "ready"),
        "nothing is lent",
    );
    assert_eq!(
        pages(
            (
                "Kelp: Ti\u{200b}des\u{fdd0}",
                "Kelp's\u{200b} question\u{fdd0}"
            ),
            "running\u{fdd1} the tests",
            ("ding\u{200b}hy\u{fdd0}", "ready"),
            "nothing\u{200b} is lent\u{fdd0}",
        ),
        clean,
        "what draws nothing, or a mark, in a peer's name, title, step or state moved a column"
    );
    // An escape's control goes and its visible rest stays, so these pages
    // are held to the same words set on one line, which may run on as
    // any long title does.
    let (team, title, step, state, said) = (
        "Kelp: Tides\u{1b}[2J\r\nnow",
        "Kelp's \u{1b}]0;tide\u{7}question",
        "running\u{1b}[2J the\n\ntests",
        "ready\r\n\nlater",
        "nothing\u{1b}]52;c;eA==\u{7} is\nlent",
    );
    let dirty = pages((team, title), step, ("dinghy", state), said);
    for page in &dirty {
        assert!(
            !page.contains(['\u{1b}', '\u{7}', '\r']),
            "a peer's escape reached the page:\n{page:?}"
        );
    }
    assert_eq!(
        dirty,
        pages(
            (&one_line(team), &one_line(title)),
            &one_line(step),
            ("dinghy", &one_line(state)),
            &one_line(said),
        ),
        "a peer's escape or break drew otherwise than its words on one line"
    );
}

#[test]
fn drawn_whole_with_cells_every_session_s_doing_shares_one_column_through_the_tree() {
    let shown = overview(&mycelium(), &whole(), true, false);
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(
        lines[2..25],
        [
            "                              need you  stuck  moving  quiet  dormant",
            "orchestrations",
            "  Harbour Ships                                     1               1",
            "    moving     BUSY01  Harbour Ships      running the tests",
            "    dormant    NAPS01  Napping one",
            "    kelp      Kelp: Tides            1",
            "      needs you  ASKS01  Kelp's question  Which tide?",
            "projects",
            "  harbor    Harbour Ships                           1               1",
            "    moving     BUSY01  Harbour Ships      running the tests",
            "    dormant    NAPS01  Napping one",
            "repositories",
            "  skiff                              1              1",
            "    needs you  ASKS01  Kelp's question    Which tide?",
            "    moving     BUSY01  Harbour Ships      running the tests",
            "groups",
            "  dock/crew                                                         1",
            "    dormant    NAPS01  Napping one",
            "machines",
            "  skiff     ready · 42% busy         1              1               1",
            "    needs you  ASKS01  Kelp's question    Which tide?",
            "    moving     BUSY01  Harbour Ships      running the tests",
            "    dormant    NAPS01  Napping one",
        ],
        "the tree with its cells, whole:\n{shown}"
    );
}

#[test]
fn drawn_whole_what_each_cell_does_lines_up_after_the_widest_title() {
    let shown = cells(&mycelium(), &whole(), false, &[]);
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(
        lines[2..5],
        [
            "needs you  ASKS01  Kelp's question  Which tide?",
            "moving     BUSY01  Harbour Ships    running the tests",
            "dormant    NAPS01  Napping one",
        ],
        "each cell, whole:\n{shown}"
    );
    let kelp = network(&mycelium(), "kelp", &whole(), false).expect("found by its short name");
    assert!(
        kelp.ends_with("\nneeds you  ASKS01  Kelp's question  Which tide?"),
        "one network's cells, whole:\n{kelp}"
    );
}

#[test]
fn drawn_whole_nothing_is_cut_and_no_line_trails() {
    let [teams, mut sessions, machines] = records();
    let long = "A question whose title runs far past every column a terminal gave it";
    for session in &mut sessions {
        if session["address"] == "ASKS01" {
            session["presence"]["questInfo"]["title"] = json!(long);
        }
    }
    let mycelium = Mycelium::of("work", &teams, &sessions, &machines);
    let drawn = [
        overview(&mycelium, &whole(), true, false),
        cells(&mycelium, &whole(), false, &[]),
    ]
    .join("\n");
    assert!(
        drawn.contains(long) && !drawn.contains('…'),
        "the long title whole:\n{drawn}"
    );
    assert!(
        drawn.lines().all(|line| line == line.trim_end()),
        "no line ends in spaces:\n{drawn}"
    );
    // The long title's row runs on: drawn single-spaced, its own gaps
    // too, with its doing on the next line in the column the others use.
    for page in [
        overview(&mycelium, &whole(), true, false),
        cells(&mycelium, &whole(), false, &[]),
    ] {
        let lines: Vec<&str> = page.lines().collect();
        let column = lines
            .iter()
            .find_map(|line| line.find("running the tests"))
            .expect("a doing in its column");
        let ran_on: Vec<usize> = (0..lines.len())
            .filter(|at| lines[*at].contains(long))
            .collect();
        assert!(!ran_on.is_empty(), "the long title is drawn:\n{page}");
        for at in ran_on {
            assert!(
                lines[at].ends_with(long) && !lines[at].trim_start().contains("  "),
                "{:?} ran on other than single-spaced, or kept its doing:\n{page}",
                lines[at]
            );
            assert_eq!(
                lines[at + 1],
                format!("{}Which tide?", " ".repeat(column)),
                "the doing of a row that ran on left the others' column:\n{page}"
            );
        }
    }
}

/// The builders the harbour's machine sends Nix builds to, as its node
/// lists them: one standing that answers, one lent that isn't running.
fn builders() -> Vec<Value> {
    vec![
        json!({"key": "builder:tern", "name": "tern", "systems": ["x86_64-linux"],
               "jobs": 1, "lines": ["standing"], "lent": false, "state": "answering"}),
        json!({"key": "builder:kestrel", "name": "kestrel",
               "systems": ["x86_64-linux", "aarch64-linux"], "jobs": 16,
               "lines": ["standing", "lent"], "lent": true, "state": "not running",
               "said": "nothing is lent"}),
    ]
}

#[test]
fn a_wide_builder_name_keeps_what_follows_in_line() {
    let mut lent = builders();
    lent[0]["name"] = json!("港湾港湾");
    let mycelium = mycelium().with_builders(&lent);
    let shown = cells(&mycelium, &look(), false, &[]);
    // Measured by Unicode's widths directly, not by the code under test,
    // as nothing here is coloured.
    let at = |name: &str, said: &str| {
        shown
            .lines()
            .find(|line| line.starts_with(&format!("  {name}")))
            .and_then(|line| {
                line.find(said)
                    .map(|at| <str as unicode_width::UnicodeWidthStr>::width(&line[..at]))
            })
    };
    assert!(
        at("kestrel", "standing").is_some()
            && at("kestrel", "standing") == at("港湾港湾", "standing"),
        "a wide builder name moved what follows it:\n{shown}"
    );
}

#[test]
fn builders_are_drawn_beside_the_machines_and_are_none_of_them() {
    let mycelium = mycelium().with_builders(&builders());
    let machines = mycelium.network("machines").expect("the machines' network");
    assert_eq!(
        (machines.cells.clone(), machines.builders.clone()),
        (
            vec!["skiff".to_owned(), "dinghy".to_owned()],
            vec!["tern".to_owned(), "kestrel".to_owned()]
        ),
        "builders beside the machines, never among them"
    );
    let kestrel = mycelium.cell("kestrel").expect("kestrel");
    assert_eq!(
        (kestrel.is, kestrel.state.as_deref(), kestrel.runs),
        (Is::Builder, Some("not running"), None)
    );
    assert_eq!(
        mycelium.lines().last(),
        Some(
            &json!({"kind": "cell", "name": "kestrel", "type": "builder",
                     "title": "builder", "state": "not running",
                     "builds": {"systems": ["x86_64-linux", "aarch64-linux"], "jobs": 16,
                                "lines": ["standing", "lent"], "lent": true,
                                "said": "nothing is lent"},
                     "networks": ["machines"]})
        )
    );
    let shown = overview(&mycelium, &look(), false, false);
    let tree: Vec<&str> = shown.lines().collect();
    assert_eq!(
        tree[0],
        "work · 3 sessions in 6 networks on 2 machines and 2 builders · 1 needs you"
    );
    assert_eq!(
        tree[14..19],
        [
            "  dinghy    ready",
            "builders",
            "  tern      standing · answering",
            "  kestrel   lent · not running",
            "",
        ],
        "the tree:\n{shown}"
    );
    let shown = cells(&mycelium, &look(), false, &[]);
    let listed: Vec<&str> = shown.lines().collect();
    assert_eq!(
        listed[8..13],
        [
            "  dinghy  ready · runs 0",
            "",
            "builders",
            "  tern     standing · answering · x86_64-linux · 1 job",
            "  kestrel  standing, lent · not running · x86_64-linux, aarch64-linux · 16 jobs · nothing is lent",
        ],
        "cells:\n{shown}"
    );
}

#[test]
fn a_builder_named_as_a_machine_hides_no_machine() {
    let named_so = [
        json!({"key": "builder:skiff", "name": "skiff", "systems": ["x86_64-linux"],
                          "jobs": 1, "lines": ["beside"], "lent": false, "state": "answering"}),
    ];
    let mycelium = mycelium().with_builders(&named_so);
    let skiff = mycelium.cell("skiff").expect("skiff");
    assert_eq!(skiff.is, Is::Machine, "the machine is found by its name");
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

/// A receptor's record as the node lists it, for `source` on `machine`.
fn receptor(source: &str, machine: &str, state: &str, today: u32) -> Value {
    json!({"key": format!("receptor:{source}"), "kind": "receptor", "heard": 1,
           "source": source, "realm": "work", "state": state,
           "since": "2026-11-15T19:00:00Z", "read_at": "2026-11-15T20:00:00Z",
           "every": 300, "pid": 4242,
           "counts": {"hour": 1, "today": today, "addressed": 2},
           "machine": machine})
}

#[test]
fn each_receptor_is_a_cell_on_its_source_s_outside_network() {
    let receptors = [
        receptor("slack", "skiff", "reading", 4),
        receptor("slack", "kestrel", "off", 0),
        receptor("mail", "skiff", "quiet", 0),
        receptor("#general", "skiff", "reading", 9),
        json!({"source": "mail", "state": "reading"}),
    ];
    let mycelium = mycelium().sensing(&receptors);
    let sensed: Vec<(&str, Option<&str>, Vec<String>)> = mycelium
        .cells
        .iter()
        .filter(|cell| cell.is == Is::Receptor)
        .map(|cell| {
            (
                cell.name.as_str(),
                cell.state.as_deref(),
                cell.networks.clone(),
            )
        })
        .collect();
    let on = |network: &str| vec![network.to_owned()];
    assert_eq!(
        sensed,
        [
            ("slack@skiff", Some("reading"), on("outside.slack")),
            ("slack@kestrel", Some("off"), on("outside.slack")),
            ("mail@skiff", Some("quiet"), on("outside.mail")),
        ],
        "one each, named by source and machine; none for a source no receptor senses, or no machine"
    );
    let slack = mycelium.network("outside.slack").expect("Slack's");
    assert_eq!(
        (slack.shape, slack.short.as_str(), slack.title.as_deref()),
        (Shape::Outside, "slack", Some("Slack")),
    );
    assert_eq!(slack.cells, ["slack@skiff", "slack@kestrel"]);
    assert_eq!(
        mycelium.networks.last().map(|each| each.shape),
        Some(Shape::Outside),
        "drawn beyond the machines"
    );
    let line = json!(mycelium.cell("slack@skiff").expect("Slack on skiff"));
    assert_eq!(
        line,
        json!({"kind": "cell", "name": "slack@skiff", "type": "receptor", "title": "Slack",
               "machine": "skiff", "since": "2026-11-15T19:00:00Z", "state": "reading",
               "counts": {"hour": 1, "today": 4, "addressed": 2},
               "networks": ["outside.slack"]}),
        "counts and times alone: nothing it read"
    );
}

#[test]
fn the_cells_say_each_receptor_and_what_it_brought_in() {
    let mycelium = mycelium().sensing(&[receptor("slack", "skiff", "reading", 4)]);
    let shown = cells(&mycelium, &look(), false, &[]);
    assert!(
        shown.contains("receptors\n  slack@skiff  reading · 1 this hour · 4 today, 2 to you"),
        "{shown}"
    );
    let one = cell(
        &mycelium,
        mycelium.cell("slack@skiff").expect("it"),
        &look(),
    );
    assert!(one.starts_with("slack@skiff · Slack\nreading · "), "{one}");
    let tree = overview(&mycelium, &look(), false, false);
    assert!(tree.contains("\noutside\n"), "{tree}");
}

#[test]
fn a_mycelium_sensing_nothing_is_the_same_mycelium() {
    assert_eq!(mycelium().sensing(&[]).lines(), mycelium().lines());
}

#[test]
fn a_receptor_cell_keeps_only_a_time_and_a_state_word_from_another_node() {
    let mut planted = receptor("slack", "skiff", "reading", 4);
    planted["since"] = json!("Re: your password reset code is 424242");
    planted["state"] = json!("the subject of a mail");
    let mycelium = mycelium().sensing(&[planted]);
    let cell = mycelium.cell("slack@skiff").expect("Slack on skiff");
    assert_eq!(
        (cell.since.as_deref(), cell.state.as_deref()),
        (None, None),
        "nothing vouches for what a receptor wrote, so a since that is no time and a \
         state that is no state's word are left out rather than drawn"
    );
}

#[test]
fn a_receptor_cells_since_is_drawn_as_the_time_it_names() {
    let mut offset = receptor("slack", "skiff", "quiet", 4);
    offset["since"] = json!("2026-11-15T14:00:00-05:00");
    let mycelium = mycelium().sensing(&[offset]);
    let cell = mycelium.cell("slack@skiff").expect("Slack on skiff");
    assert_eq!(
        (cell.since.as_deref(), cell.state.as_deref()),
        (Some("2026-11-15T19:00:00Z"), Some("quiet")),
        "a time with an offset is drawn as the time it names, and a state's word as itself"
    );
}

#[test]
fn a_cell_declares_plain_subjects_alone_each_bad_one_passed_over() {
    use super::made::{MOST_SUBJECTS, subjects};
    let declared = json!([
        " mycelium:hubs ",
        7,
        "",
        "a\u{202e}b",
        "zero\u{200b}width",
        "soft\u{00ad}hyphen",
        "marked\u{feff}",
        "tag\u{e0041}",
        "varied\u{fe0f}",
        "varied\u{e0100}",
        "mongolian\u{180b}",
        "mongolian\u{180f}",
        "filler\u{3164}",
        "joined\u{034f}",
        "line\u{2028}break",
        "bell\u{7}",
        "x".repeat(121),
        "x".repeat(120),
    ]);
    assert_eq!(
        subjects(&declared),
        vec!["mycelium:hubs".to_owned(), "x".repeat(120)],
        "only plain words of at most 120 characters are kept, trimmed"
    );
    let many: Vec<String> = (0..20).map(|n| format!("subject {n}")).collect();
    let kept = subjects(&json!(many));
    assert_eq!(kept.len(), MOST_SUBJECTS);
    assert_eq!(kept[0], "subject 0", "the first declared are kept");
    assert!(subjects(&json!("one")).is_empty(), "subjects are a list");
}
