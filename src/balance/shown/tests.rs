//! The realm's balance drawn for a person, at each width it keeps to.

use super::*;

#[test]
fn wide_words_wrap_by_the_cells_they_take() {
    // Each word takes four cells, so three after `held:` fill twenty,
    // where counted by character five would.
    assert_eq!(
        wrapped("held", "held", &["港湾"; 6].join(" "), 20),
        "held: 港湾 港湾 港湾\n      港湾 港湾 港湾\n"
    );
}

fn machine(name: &str, load: f64, busy: usize, per_core: f64) -> Load {
    Load {
        machine: name.to_owned(),
        ready: true,
        memory: Some(0.5),
        stall: Some(0.1),
        processor: Some(load),
        load: Some(load),
        over: false,
        past: Vec::new(),
        lively: None,
        room: false,
        busy,
        idle: 3,
        busy_per_core: Some(per_core),
        age: Some(20),
    }
}

fn reading(machines: Vec<Load>, imbalance: Option<f64>, flags: Vec<Flag>) -> Reading {
    Reading {
        measures: super::super::MEASURES.to_owned(),
        at: "2026-11-16T12:30:00Z".to_owned(),
        machines,
        imbalance,
        flags,
        held: None,
    }
}

#[test]
fn an_imbalanced_realm_says_who_carries_more_and_what_follows() {
    let shown = said(
        &reading(
            vec![
                machine("kestrel", 0.9, 28, 1.75),
                machine("osprey", 0.4, 13, 1.08),
            ],
            Some(0.25),
            vec![Flag::Imbalanced],
        ),
        "work",
        100,
        false,
    );
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(lines[0], "work · 2 machines · loads spread 0.25, past 0.20");
    assert!(
        lines.iter().any(|line| line.starts_with("  kestrel")
            && line.contains("90%")
            && line.contains("1.75")),
        "{shown}"
    );
    assert!(
        shown.contains(
            "imbalanced: kestrel carries 90% and 1.75 busy sessions a core, \
             osprey 40% and 1.08;"
        ),
        "{shown}"
    );
    assert!(shown.contains("new work leans to osprey"), "{shown}");
    assert!(!shown.contains('\x1b'), "{shown}");
}

#[test]
fn a_realm_with_nothing_wrong_says_so() {
    let shown = said(
        &reading(
            vec![
                machine("kestrel", 0.3, 2, 0.25),
                machine("osprey", 0.35, 3, 0.4),
            ],
            Some(0.03),
            vec![],
        ),
        "home",
        100,
        false,
    );
    assert!(
        shown.starts_with("home · 2 machines · loads spread 0.03"),
        "{shown}"
    );
    assert!(
        shown.trim_end().ends_with(
            "nothing wrong: no machine over while another has room, none stranded, \
             every machine's facts fresh"
        ),
        "{shown}"
    );
}

#[test]
fn a_machine_not_ready_is_counted_apart_and_said_so() {
    let mut resting = machine("osprey", 0.6, 0, 0.0);
    resting.ready = false;
    let shown = said(
        &reading(
            vec![machine("kestrel", 0.9, 28, 1.75), resting],
            None,
            vec![],
        ),
        "work",
        100,
        false,
    );
    assert!(shown.starts_with("work · 2 machines, 1 ready\n"), "{shown}");
    assert!(
        shown
            .lines()
            .any(|line| line.starts_with("  osprey   not ready")),
        "{shown}"
    );
}

#[test]
fn each_flag_is_a_sentence_saying_what_follows() {
    let mut hot = machine("kestrel", 1.0, 30, 2.0);
    hot.over = true;
    let mut cool = machine("osprey", 0.2, 1, 0.1);
    cool.room = true;
    let shown = said(
        &reading(
            vec![hot, cool],
            Some(0.4),
            vec![
                Flag::HotWhileCool {
                    hot: "kestrel".to_owned(),
                    cool: "osprey".to_owned(),
                },
                Flag::Stranded {
                    machine: "kestrel".to_owned(),
                    spent: "processor".to_owned(),
                },
                Flag::Stale {
                    machine: "osprey".to_owned(),
                },
            ],
        ),
        "work",
        200,
        false,
    );
    for said in [
        "hot while cool: kestrel is over while osprey has room;",
        "stranded: kestrel's processors are spent while half its memory or more is free;",
        "stale: osprey's facts are 20 s old, too old to place by;",
    ] {
        assert!(shown.contains(said), "{said} in:\n{shown}");
    }
}

#[test]
fn a_long_sentence_wraps_to_the_width_under_its_word() {
    let shown = said(
        &reading(
            vec![
                machine("kestrel", 0.9, 28, 1.75),
                machine("osprey", 0.4, 13, 1.08),
            ],
            Some(0.25),
            vec![Flag::Imbalanced],
        ),
        "work",
        60,
        false,
    );
    let sentences = shown
        .split("\n\n")
        .nth(2)
        .expect("sentences after the table");
    for line in sentences.lines() {
        assert!(drawn_width(line) <= 60, "{line:?} in:\n{shown}");
    }
    assert!(
        sentences.lines().nth(1).is_some_and(
            |line| line.starts_with("            ") && !line.starts_with("             ")
        ),
        "a sentence's later lines sit under its first word:\n{shown}"
    );
}

#[test]
fn in_colour_the_headers_are_faint_and_the_flags_strong() {
    let shown = said(
        &reading(vec![machine("kestrel", 0.3, 2, 0.25)], None, vec![]),
        "home",
        100,
        true,
    );
    assert!(shown.contains("\x1b[2m"), "{shown:?}");
    assert!(shown.contains("home · 1 machine"), "{shown}");
}

fn two() -> Reading {
    reading(
        vec![
            machine("kestrel", 0.9, 28, 1.75),
            machine("osprey", 0.4, 13, 1.08),
        ],
        Some(0.03),
        vec![],
    )
}

#[test]
fn at_eighty_columns_the_table_keeps_whole_columns_and_names_the_rest() {
    let shown = said(&two(), "work", 80, false);
    let long: Vec<&str> = shown
        .lines()
        .filter(|line| drawn_width(line) > 80)
        .collect();
    assert!(long.is_empty(), "lines past 80 columns: {long:?}\n{shown}");
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(
        lines[2..5],
        [
            "                      load  memory  processors  stall  busy  idle  per core",
            "  kestrel  no room     90%     50%         90%    10%    28     3      1.75",
            "  osprey   no room     40%     50%         40%    10%    13     3      1.08",
        ],
        "the facts column, which would pass 80, is left out whole:\n{shown}"
    );
    assert_eq!(
        lines[5], "  left out to fit: facts",
        "and the line under the table says so:\n{shown}"
    );
}

#[test]
fn a_column_is_kept_while_it_reaches_the_edge_and_no_further() {
    let at = |width| said(&two(), "work", width, false);
    assert!(
        at(82)
            .lines()
            .nth(2)
            .is_some_and(|line| line.ends_with("per core  facts"))
            && !at(82).contains("left out"),
        "at 82 the facts reach the edge, so they stay:\n{}",
        at(82)
    );
    assert!(
        at(81)
            .lines()
            .any(|line| line == "  left out to fit: facts"),
        "at 81 they'd pass it, so they're left out:\n{}",
        at(81)
    );
}

#[test]
fn with_no_column_fitting_no_blank_header_is_drawn() {
    let long = "a-machine-named-thirty-letters";
    let shown = said(
        &reading(vec![machine(long, 0.9, 28, 1.75)], None, vec![]),
        "work",
        40,
        false,
    );
    let lines: Vec<&str> = shown.lines().collect();
    assert!(
        lines[1].is_empty() && lines[2].starts_with(&format!("  {long}")),
        "a header with no column drew a blank line under the head:\n{shown}"
    );
}

#[test]
fn a_machine_s_name_reaches_the_table_and_sentences_as_one_line() {
    // Every sentence and the held line name each machine, so each
    // place a name is drawn is handed the dirty one.
    let realm = |hot: &str, cool: &str| {
        let flags = vec![
            Flag::HotWhileCool {
                hot: hot.to_owned(),
                cool: cool.to_owned(),
            },
            Flag::Stranded {
                machine: hot.to_owned(),
                spent: "memory".to_owned(),
            },
            Flag::Stranded {
                machine: cool.to_owned(),
                spent: "processors".to_owned(),
            },
            Flag::Stale {
                machine: cool.to_owned(),
            },
            Flag::Imbalanced,
        ];
        let mut realm = reading(
            vec![machine(hot, 0.9, 28, 1.75), machine(cool, 0.4, 13, 1.08)],
            Some(0.25),
            flags.clone(),
        );
        realm.held = Some(flags);
        realm
    };
    let pages = |hot: &str, cool: &str| {
        [40, 80, crate::swarm::WHOLE].map(|width| said(&realm(hot, cool), "work", width, false))
    };
    assert_eq!(
        pages("kes\u{200B}trel\u{fdd0}", "osp\u{200B}rey\u{fdd0}"),
        pages("kestrel", "osprey"),
        "what draws nothing, or a mark, in a machine's name moved a column"
    );
    let (dirty, cool) = ("kestrel\u{1b}]0;x\u{7}\r\nlater", "osprey\u{1b}[2J");
    let drawn = pages(dirty, cool);
    assert!(
        !drawn
            .iter()
            .any(|page| page.contains(['\u{1b}', '\u{7}', '\r'])),
        "a machine's escape reached the page: {drawn:?}"
    );
    assert_eq!(
        drawn,
        pages(
            &crate::forage::one_line(dirty),
            &crate::forage::one_line(cool)
        ),
        "a machine's escape or break drew otherwise than its name on one line"
    );
}

#[test]
fn in_a_narrow_terminal_a_wide_spread_s_head_wraps_within_its_edge() {
    let shown = said(
        &reading(
            vec![
                machine("kestrel", 0.9, 28, 1.75),
                machine("osprey", 0.4, 13, 1.08),
            ],
            Some(0.25),
            vec![],
        ),
        "work",
        40,
        false,
    );
    let lines: Vec<&str> = shown.lines().collect();
    assert_eq!(
        lines[..3],
        ["work · 2 machines · loads spread 0.25,", "  past 0.20", ""],
        "the head passed 40 columns, or hung its rest otherwise:\n{shown}"
    );
}

/// Names that fit: a name is drawn whole, so one past the width
/// passes it.
#[test]
fn in_a_narrow_terminal_no_line_passes_its_edge() {
    let shown = said(&two(), "work", 40, false);
    let long: Vec<&str> = shown
        .lines()
        .filter(|line| drawn_width(line) > 40)
        .collect();
    assert!(long.is_empty(), "lines past 40 columns: {long:?}\n{shown}");
    let lines: Vec<&str> = shown.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.starts_with("  left out to fit: "))
        .expect("a line says what was left out");
    assert_eq!(
        lines[at..at + 3],
        [
            "  left out to fit: processors, stall,",
            "                   busy, idle, per core,",
            "                   facts",
        ],
        "the columns left out wrap under the first one named:\n{shown}"
    );
}

#[test]
fn drawn_whole_every_column_is_there_and_none_is_said_left_out() {
    let shown = said(&two(), "work", crate::swarm::WHOLE, false);
    assert!(
        shown
            .lines()
            .nth(2)
            .is_some_and(|line| line.ends_with("per core  facts"))
            && shown
                .lines()
                .nth(3)
                .is_some_and(|line| line.ends_with("1.75   20 s")),
        "every column, the facts' age under its header:\n{shown}"
    );
    assert!(!shown.contains("left out"), "{shown}");
}

#[test]
fn drawn_whole_each_sentence_is_one_line() {
    let flagged = reading(
        vec![
            machine("kestrel", 0.9, 28, 1.75),
            machine("osprey", 0.4, 13, 1.08),
        ],
        Some(0.25),
        vec![
            Flag::Imbalanced,
            Flag::HotWhileCool {
                hot: "kestrel".to_owned(),
                cool: "osprey".to_owned(),
            },
            Flag::Stranded {
                machine: "kestrel".to_owned(),
                spent: "processor".to_owned(),
            },
        ],
    );
    // The sentences follow the table's blank line, the second.
    let sentences = |width: usize| -> Vec<String> {
        said(&flagged, "work", width, false)
            .split("\n\n")
            .skip(2)
            .flat_map(str::lines)
            .map(str::to_owned)
            .collect()
    };
    assert!(
        sentences(40).iter().any(|line| line.starts_with(' ')),
        "at 40 a sentence wraps: {:?}",
        sentences(40)
    );
    let whole = sentences(crate::swarm::WHOLE);
    assert!(
        whole.len() == 3 && whole.iter().all(|line| !line.starts_with(' ')),
        "whole, each of the three sentences is one line: {whole:?}"
    );
}

#[test]
fn a_wide_name_keeps_the_columns_in_line_and_within_the_edge() {
    // Twelve characters, each two cells wide, as a name in Han takes.
    let wide = "港湾港湾港湾港湾港湾港湾";
    let shown = said(
        &reading(
            vec![
                machine(wide, 0.9, 28, 1.75),
                machine("osprey", 0.4, 13, 1.08),
            ],
            None,
            vec![],
        ),
        "work",
        80,
        false,
    );
    // Measured by Unicode's widths directly, not by the code under test,
    // as nothing here is coloured.
    let cells = <str as unicode_width::UnicodeWidthStr>::width;
    let long: Vec<&str> = shown.lines().filter(|line| cells(line) > 80).collect();
    assert!(long.is_empty(), "lines past 80 cells: {long:?}\n{shown}");
    let stands = |name: &str| {
        shown
            .lines()
            .find(|line| line.starts_with(&format!("  {name}")))
            .and_then(|line| line.find("no room").map(|at| cells(&line[..at])))
    };
    assert!(
        stands(wide).is_some() && stands(wide) == stands("osprey"),
        "a wide name moved the columns after it:\n{shown}"
    );
    // The name takes 24 cells, so busy ends at 76 and idle would end
    // at 82; counted as 12 characters, idle and per core would fit.
    assert!(
        shown
            .lines()
            .any(|line| line.trim_end().ends_with(" stall  busy"))
            && shown
                .lines()
                .any(|line| line == "  left out to fit: idle, per core, facts"),
        "the columns kept were not those 80 cells hold:\n{shown}"
    );
}

#[test]
fn a_figure_wider_than_its_header_widens_its_column() {
    let mut many = machine("kestrel", 0.9, 12_345, 1.75);
    many.idle = 7;
    let shown = said(
        &reading(vec![many, machine("osprey", 0.4, 13, 1.08)], None, vec![]),
        "work",
        crate::swarm::WHOLE,
        false,
    );
    let lines: Vec<&str> = shown.lines().collect();
    let busy = |line: &str| line.find("busy").map(|at| at + "busy".len());
    assert!(
        lines[2].contains(" busy  idle")
            && lines[3].contains("12345     7")
            && lines[4].contains("   13     3")
            && busy(lines[2]) == lines[3].find("12345").map(|at| at + 5),
        "busy right-aligned over its widest figure, idle still under its own:\n{shown}"
    );
}

/// The row `shown` draws for `name`, failing with the whole table where
/// it draws none.
fn row<'a>(shown: &'a str, name: &str) -> &'a str {
    let named = format!("  {name} ");
    shown
        .lines()
        .find(|line| line.starts_with(&named))
        .unwrap_or_else(|| panic!("no row for {name}:\n{shown}"))
}

#[test]
fn a_machine_over_by_its_lively_ceiling_alone_says_so_and_why() {
    let mut queued = machine("kestrel", 0.9, 28, 1.75);
    queued.over = true;
    queued.past = vec![crate::room::Limit::Cpu];
    let mut lively = machine("tern", 0.45, 4, 0.25);
    lively.over = true;
    lively.lively = Some(
        "it is kept lively, at processor pressure 30 at most, and is at 45 over ten seconds"
            .to_owned(),
    );
    // Kept lively and stalled on its processors, so past its queue's
    // limit and its ceiling both.
    let mut both = machine("skiff", 0.95, 30, 1.9);
    both.over = true;
    both.past = vec![crate::room::Limit::Cpu];
    both.lively = Some(
        "it is kept lively, at processor pressure 30 at most, and is at 86 over ten seconds"
            .to_owned(),
    );
    let mut machines = vec![queued, lively, both];
    let shown = said(
        &reading(machines.clone(), None, vec![]),
        "work",
        crate::swarm::WHOLE,
        false,
    );
    assert!(
        row(&shown, "kestrel").starts_with("  kestrel  over ")
            && row(&shown, "skiff").starts_with("  skiff    over ")
            && row(&shown, "tern").starts_with("  tern     lively "),
        "past its queue's limit it stands over, whatever its ceiling:\n{shown}"
    );
    assert!(
        shown.contains(
            "lively: tern is over though its queue is past no limit, since it is kept \
             lively, at processor pressure 30 at most, and is at 45 over ten seconds"
        ),
        "the line under the table says why:\n{shown}"
    );
    assert!(
        !shown.contains("lively: kestrel") && !shown.contains("lively: skiff"),
        "one past its queue's limit needs no word on its ceiling:\n{shown}"
    );
    // One not ready stands by the column's longest word: were the column
    // as wide as the words it holds, the loads after it would move.
    let mut resting = machine("osprey", 0.3, 2, 0.5);
    resting.ready = false;
    machines.push(resting);
    let unready = said(
        &reading(machines, None, vec![]),
        "work",
        crate::swarm::WHOLE,
        false,
    );
    let load = |table: &str, name: &str| row(table, name).find('%');
    assert!(
        ["kestrel", "skiff", "tern", "osprey"]
            .into_iter()
            .all(|name| load(&unready, name) == load(&unready, "kestrel")),
        "every load sits in one column, beside a machine not ready too:\n{unready}"
    );
    assert_eq!(
        load(&shown, "kestrel"),
        load(&unready, "kestrel"),
        "the column is as wide as its longest word, though none stands so:\n{shown}\n{unready}"
    );
}
