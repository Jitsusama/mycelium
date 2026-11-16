//! A reading of the realm's balance as a person reads it: a line saying
//! the realm, a row for each machine, and a sentence for each thing wrong
//! saying what it means and what follows.

use std::fmt::Write as _;

use super::{Flag, IMBALANCE, Load, Reading};
use crate::swarm::{BOLD, FAINT, RESET};

/// The table's columns after the machine's name and how it stands, each
/// as wide as its header.
const COLUMNS: [&str; 8] = [
    "load",
    "memory",
    "processors",
    "stall",
    "busy",
    "idle",
    "per core",
    "facts",
];

/// How wide the column saying how a machine stands is: its longest word.
const STANDS: usize = "not ready".len();

/// Up to how many seconds old facts are said in seconds, past which in
/// minutes.
const SECONDS_UP_TO: i64 = 120;

/// Draws `reading` of `realm` for a person, `width` columns wide, in
/// colour where `colour` says.
#[must_use]
pub fn said(reading: &Reading, realm: &str, width: usize, colour: bool) -> String {
    let paint = |style: &str, text: &str| {
        if colour {
            format!("{style}{text}{RESET}")
        } else {
            text.to_owned()
        }
    };
    let mut out = String::new();
    let count = reading.machines.len();
    let noun = if count == 1 { "machine" } else { "machines" };
    let _ = write!(out, "{realm} · {count} {noun}");
    let ready = reading.machines.iter().filter(|one| one.ready).count();
    if ready < count {
        let _ = write!(out, ", {ready} ready");
    }
    match reading.imbalance {
        Some(spread) if spread > IMBALANCE => {
            let _ = write!(out, " · loads spread {spread:.2}, past {IMBALANCE:.2}");
        }
        Some(spread) => {
            let _ = write!(out, " · loads spread {spread:.2}");
        }
        None => {}
    }
    out.push_str("\n\n");
    let named = reading
        .machines
        .iter()
        .map(|one| one.machine.chars().count())
        .max()
        .unwrap_or(0);
    let mut header = format!("  {:named$}  {:STANDS$}", "", "");
    for column in COLUMNS {
        let _ = write!(header, "  {column}");
    }
    let _ = writeln!(out, "{}", paint(FAINT, header.trim_end()));
    for one in &reading.machines {
        let figures = [
            percent(one.load),
            percent(one.memory),
            percent(one.processor),
            percent(one.stall),
            one.busy.to_string(),
            one.idle.to_string(),
            one.busy_per_core
                .map_or_else(|| "-".to_owned(), |each| format!("{each:.2}")),
            one.age.map_or_else(|| "-".to_owned(), aged),
        ];
        let mut row = format!("  {:named$}  {:STANDS$}", one.machine, stands(one));
        for (column, figure) in COLUMNS.iter().zip(figures) {
            let wide = column.len();
            let _ = write!(row, "  {figure:>wide$}");
        }
        let _ = writeln!(out, "{}", row.trim_end());
    }
    out.push('\n');
    let sentences: Vec<(&str, String)> = if reading.flags.is_empty() {
        vec![(
            "nothing wrong",
            "no machine over while another has room, none stranded, every machine's \
             facts fresh"
                .to_owned(),
        )]
    } else {
        reading
            .flags
            .iter()
            .map(|flag| sentence(flag, reading))
            .collect()
    };
    for (word, rest) in sentences {
        out.push_str(&wrapped(&paint(BOLD, word), word, &rest, width));
    }
    if let Some(held) = reading.held.as_ref().filter(|held| !held.is_empty()) {
        let named: Vec<String> = held.iter().map(short).collect();
        let rest = format!("{}, in most of the last ten minutes", named.join(", "));
        out.push_str(&wrapped(&paint(BOLD, "held"), "held", &rest, width));
    }
    out
}

/// A share from 0 to 1 as a whole percent, or a dash where unknown.
fn percent(share: Option<f64>) -> String {
    share.map_or_else(|| "-".to_owned(), |share| format!("{:.0}%", share * 100.0))
}

/// How old facts `seconds` old are, said shortly.
fn aged(seconds: i64) -> String {
    if seconds <= SECONDS_UP_TO {
        format!("{seconds} s")
    } else {
        format!("{} min", seconds / 60)
    }
}

/// How a machine stands, in a word.
fn stands(one: &Load) -> &'static str {
    if !one.ready {
        "not ready"
    } else if one.over {
        "over"
    } else if one.room {
        "room"
    } else {
        "no room"
    }
}

/// The sentence saying what `flag` means and what follows from it, as
/// its word and the rest.
fn sentence(flag: &Flag, reading: &Reading) -> (&'static str, String) {
    let load = |name: &str| reading.machines.iter().find(|one| one.machine == name);
    match flag {
        Flag::HotWhileCool { hot, cool } => (
            "hot while cool",
            format!(
                "{hot} is over while {cool} has room; new work should go to {cool}, \
                 and each minute of this counts against place's weights"
            ),
        ),
        Flag::Stranded { machine, spent } if spent == "memory" => (
            "stranded",
            format!(
                "{machine}'s memory is nearly all used while its processors idle; \
                 work there waits on memory while the processors go unused"
            ),
        ),
        Flag::Stranded { machine, .. } => (
            "stranded",
            format!(
                "{machine}'s processors are spent while half its memory or more is \
                 free; work there waits on processors while the memory goes unused"
            ),
        ),
        Flag::Stale { machine } => {
            let old = load(machine)
                .and_then(|one| one.age)
                .map_or_else(|| "unknown".to_owned(), aged);
            (
                "stale",
                format!(
                    "{machine}'s facts are {old} old, too old to place by; its link may \
                     be slow or down, and placing there is placing blind"
                ),
            )
        }
        Flag::Imbalanced => ("imbalanced", imbalanced(reading)),
    }
}

/// What an imbalanced realm means: which machine carries most, which
/// least, and where new work leans.
fn imbalanced(reading: &Reading) -> String {
    let mut known: Vec<&Load> = reading
        .machines
        .iter()
        .filter(|one| one.ready && one.load.is_some())
        .collect();
    known.sort_by(|a, b| b.load.unwrap_or(0.0).total_cmp(&a.load.unwrap_or(0.0)));
    let (Some(most), Some(least)) = (known.first(), known.last()) else {
        return format!("the machines' loads spread past {IMBALANCE:.2}");
    };
    let per_core = |one: &Load, long: bool| match (one.busy_per_core, long) {
        (Some(each), true) => format!(" and {each:.2} busy sessions a core"),
        (Some(each), false) => format!(" and {each:.2}"),
        (None, _) => String::new(),
    };
    format!(
        "{} carries {}{}, {} {}{}; place weighs headroom, so new work leans to {} \
         until the two even out",
        most.machine,
        percent(most.load),
        per_core(most, true),
        least.machine,
        percent(least.load),
        per_core(least, false),
        least.machine,
    )
}

/// A flag in a few words, for the flags held.
fn short(flag: &Flag) -> String {
    match flag {
        Flag::HotWhileCool { hot, cool } => format!("{hot} hot while {cool} cool"),
        Flag::Stranded { machine, .. } => format!("{machine} stranded"),
        Flag::Stale { machine } => format!("{machine} stale"),
        Flag::Imbalanced => "imbalanced".to_owned(),
    }
}

/// `word: rest` wrapped to `width`, each later line under the first word
/// of `rest`; `drawn` is `word` as it is drawn, which may hold escapes.
fn wrapped(drawn: &str, word: &str, rest: &str, width: usize) -> String {
    let indent = word.chars().count() + 2;
    let mut out = format!("{drawn}:");
    let mut column = indent - 1;
    let mut first = true;
    for each in rest.split_whitespace() {
        let wide = each.chars().count();
        if !first && column + 1 + wide > width {
            let _ = write!(out, "\n{:indent$}{each}", "");
            column = indent + wide;
        } else {
            out.push(' ');
            out.push_str(each);
            column += 1 + wide;
        }
        first = false;
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(name: &str, load: f64, busy: usize, per_core: f64) -> Load {
        Load {
            machine: name.to_owned(),
            ready: true,
            memory: Some(0.5),
            stall: Some(0.1),
            processor: Some(load),
            load: Some(load),
            over: false,
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
            &reading(vec![machine("kestrel", 0.9, 28, 1.75), resting], None, vec![]),
            "work",
            100,
            false,
        );
        assert!(shown.starts_with("work · 2 machines, 1 ready\n"), "{shown}");
        assert!(
            shown.lines().any(|line| line.starts_with("  osprey   not ready")),
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
            assert!(line.chars().count() <= 60, "{line:?} in:\n{shown}");
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
}
