//! A reading of the realm's balance as a person reads it: a line saying
//! the realm, a row for each machine, and a sentence for each thing wrong
//! saying what it means and what follows.

use std::fmt::Write as _;

use super::{Flag, IMBALANCE, Load, Reading};
use crate::swarm::{BOLD, FAINT, RESET, drawn_width, padded, unmarked};

/// The table's columns after the machine's name and how it stands, each
/// as wide as its header or its widest figure.
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
    let mut head = String::new();
    let count = reading.machines.len();
    let noun = if count == 1 { "machine" } else { "machines" };
    let _ = write!(head, "{realm} · {count} {noun}");
    let ready = reading.machines.iter().filter(|one| one.ready).count();
    if ready < count {
        let _ = write!(head, ", {ready} ready");
    }
    match reading.imbalance {
        Some(spread) if spread > IMBALANCE => {
            let _ = write!(head, " · loads spread {spread:.2}, past {IMBALANCE:.2}");
        }
        Some(spread) => {
            let _ = write!(head, " · loads spread {spread:.2}");
        }
        None => {}
    }
    // The head keeps to the width as the table under it does, its later
    // lines hung under the first.
    let mut out = crate::swarm::wrapped(&head, "", "  ", width);
    out.push('\n');
    // A machine's name is its own to say, so it is held to one line and
    // counted as it is drawn.
    let rows: Vec<(&Load, String, [String; 8])> = reading
        .machines
        .iter()
        .map(|one| (one, unmarked(&one.machine), figures(one)))
        .collect();
    let named = rows
        .iter()
        .map(|(_, name, _)| drawn_width(name))
        .max()
        .unwrap_or(0);
    let wides: Vec<usize> = COLUMNS
        .iter()
        .enumerate()
        .map(|(at, column)| {
            rows.iter()
                .map(|(_, _, figures)| drawn_width(&figures[at]))
                .chain([column.len()])
                .max()
                .unwrap_or_default()
        })
        .collect();
    // As many columns as fit whole, in their order, as the machines' table
    // gives way, and a line naming the rest so none goes unsaid; a pipe
    // has every one.
    let mut reach = 2 + named + 2 + STANDS;
    let shown = wides
        .iter()
        .take_while(|wide| {
            reach += 2 + **wide;
            reach <= width
        })
        .count();
    let mut header = format!("  {:named$}  {:STANDS$}", "", "");
    for (column, wide) in COLUMNS.iter().zip(&wides).take(shown) {
        let _ = write!(header, "  {column:>wide$}");
    }
    // With no column fitting, the header would be a blank line.
    if shown > 0 {
        let _ = writeln!(out, "{}", paint(FAINT, header.trim_end()));
    }
    for (one, name, figures) in &rows {
        // Padded by the cells it takes, which a format's width doesn't
        // count, so a wide name keeps the columns after it in line.
        let mut row = format!("  {}  {:STANDS$}", padded(name, named), stands(one));
        for (figure, wide) in figures.iter().zip(&wides).take(shown) {
            let _ = write!(row, "  {figure:>wide$}");
        }
        let _ = writeln!(out, "{}", row.trim_end());
    }
    if shown < COLUMNS.len() {
        // Wrapped as the sentences are, so the line saying what didn't fit
        // fits itself.
        let label = "  left out to fit";
        for line in wrapped(label, label, &COLUMNS[shown..].join(", "), width).lines() {
            let _ = writeln!(out, "{}", paint(FAINT, line));
        }
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

/// One machine's figures, in the order of [`COLUMNS`].
fn figures(one: &Load) -> [String; 8] {
    [
        percent(one.load),
        percent(one.memory),
        percent(one.processor),
        percent(one.stall),
        one.busy.to_string(),
        one.idle.to_string(),
        one.busy_per_core
            .map_or_else(|| "-".to_owned(), |each| format!("{each:.2}")),
        one.age.map_or_else(|| "-".to_owned(), aged),
    ]
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
                 and each minute of this counts against place's weights",
                hot = unmarked(hot),
                cool = unmarked(cool),
            ),
        ),
        Flag::Stranded { machine, spent } if spent == "memory" => (
            "stranded",
            format!(
                "{machine}'s memory is nearly all used while its processors idle; \
                 work there waits on memory while the processors go unused",
                machine = unmarked(machine),
            ),
        ),
        Flag::Stranded { machine, .. } => (
            "stranded",
            format!(
                "{machine}'s processors are spent while half its memory or more is \
                 free; work there waits on processors while the memory goes unused",
                machine = unmarked(machine),
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
                     be slow or down, and placing there is placing blind",
                    machine = unmarked(machine),
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
        unmarked(&most.machine),
        percent(most.load),
        per_core(most, true),
        unmarked(&least.machine),
        percent(least.load),
        per_core(least, false),
        unmarked(&least.machine),
    )
}

/// A flag in a few words, for the flags held.
fn short(flag: &Flag) -> String {
    match flag {
        Flag::HotWhileCool { hot, cool } => {
            format!("{} hot while {} cool", unmarked(hot), unmarked(cool))
        }
        Flag::Stranded { machine, .. } => format!("{} stranded", unmarked(machine)),
        Flag::Stale { machine } => format!("{} stale", unmarked(machine)),
        Flag::Imbalanced => "imbalanced".to_owned(),
    }
}

/// `word: rest` wrapped to `width`, each later line under the first word
/// of `rest`; `drawn` is `word` as it is drawn, which may hold escapes.
fn wrapped(drawn: &str, word: &str, rest: &str, width: usize) -> String {
    let indent = drawn_width(word) + 2;
    let mut out = format!("{drawn}:");
    let mut column = indent - 1;
    let mut first = true;
    for each in rest.split_whitespace() {
        let wide = drawn_width(each);
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
mod tests;
