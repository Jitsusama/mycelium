//! The realm's machines, and the builders this machine sends Nix builds
//! to, drawn for a person: a row each, cut to the terminal's width, with
//! a line beneath for what about one is worth a look. One machine or
//! builder by its name is a few lines of what it is.

use crate::builders::Builder;
use crate::disk::Band;
use crate::facts::{Machine, Readiness};
use crate::room::Limit;
use crate::swarm::{GAP, Look, fit, padded, wrapped};

/// How far a line's later lines are set in beneath its first.
const HANGING: &str = "  ";

/// How wide a label of one machine's facts is drawn, its widest.
const LABEL_WIDTH: usize = 8;

/// Bytes in a gigabyte, as disks and memory are sold.
const GB: f64 = 1_000_000_000.0;

/// What each machine's row says, column by column.
const MACHINE_COLUMNS: [&str; 7] = [
    "machine",
    "state",
    "busy",
    "memory free",
    "disk free",
    "power",
    "updated",
];

/// What each builder's row says, column by column.
const BUILDER_COLUMNS: [&str; 4] = ["builder", "state", "jobs", "systems"];

/// `machines`, then `builders`, drawn as `look` says: a heading, a row
/// each, and beneath its row what about each is worth a look.
#[must_use]
pub fn table(machines: &[Machine], builders: &[Builder], look: &Look) -> String {
    let mut out = String::new();
    if machines.is_empty() {
        out.push_str("No machine of the realm has said what it is.\n");
    } else {
        let rows: Vec<Vec<String>> = machines.iter().map(|each| row(each, look)).collect();
        out.push_str(&columns(&MACHINE_COLUMNS, &rows, look));
        let worth: Vec<String> = machines
            .iter()
            .flat_map(|each| worth_a_look(each, look))
            .collect();
        if !worth.is_empty() {
            out.push('\n');
            for line in worth {
                out.push_str(&wrapped(&line, "", HANGING, look.width));
            }
        }
    }
    if !builders.is_empty() {
        out.push('\n');
        let rows: Vec<Vec<String>> = builders.iter().map(builder_row).collect();
        out.push_str(&columns(&BUILDER_COLUMNS, &rows, look));
        for builder in builders {
            if let Some(said) = builder.said.as_deref().filter(|said| !said.is_empty()) {
                let said = format!("{}: {said}", builder.name);
                out.push_str(&wrapped(&said, "", HANGING, look.width));
            }
        }
    }
    out
}

/// `machine` said in a few lines, as `look` says: its row's facts each on
/// a line of their own, with what is worth a look about it after them.
#[must_use]
pub fn one(machine: &Machine, look: &Look) -> String {
    let what = [
        machine.model_name.as_deref().or(machine.model.as_deref()),
        machine.os.as_deref(),
        Some(machine.arch.as_str()),
    ];
    let cpu = [
        machine.cpu.model.clone(),
        machine.cpu.cores.map(|cores| format!("{cores} cores")),
        machine
            .cpu
            .busy
            .map(|busy| format!("{} busy", percent(busy))),
    ];
    let reaches: Vec<&str> = machine
        .reached
        .iter()
        .filter(|(_, reached)| **reached)
        .map(|(name, _)| name.as_str())
        .collect();
    let lines = [
        ("what", joined(what.iter().flatten().copied())),
        ("cpu", joined(cpu.iter().flatten().map(String::as_str))),
        ("memory", memory(machine)),
        ("disk", disk(machine)),
        ("power", power(machine)),
        ("reaches", reaches.join(", ")),
        ("updated", updated(machine, look)),
    ];
    let mut out = format!("{}  {}\n", machine.name, state(machine.state));
    out.push_str(&labelled(&lines, look));
    for line in worth_a_look(machine, look) {
        out.push_str(&wrapped(&line, HANGING, "    ", look.width));
    }
    out
}

/// `builder` said in a few lines, wrapped to `look`'s width.
#[must_use]
pub fn one_builder(builder: &Builder, look: &Look) -> String {
    let lines = [
        ("jobs", jobs(builder)),
        ("systems", builder.systems.join(", ")),
        ("supports", builder.supported.join(", ")),
        ("says", builder.said.clone().unwrap_or_default()),
    ];
    let mut out = format!("{}  {}\n", builder.name, builder.state);
    out.push_str(&labelled(&lines, look));
    out
}

/// Each of `lines` said under its label, its words wrapped to `look`'s
/// width beneath where the first began, the empty ones left out.
fn labelled(lines: &[(&str, String)], look: &Look) -> String {
    lines
        .iter()
        .filter(|(_, said)| !said.is_empty())
        .map(|(label, said)| {
            let first = format!("{HANGING}{label:<LABEL_WIDTH$}{GAP}");
            let rest = " ".repeat(first.chars().count());
            wrapped(said, &first, &rest, look.width)
        })
        .collect()
}

/// `rows` under `headings`, each column as wide as its widest and as many
/// as fit `look`'s width whole, the first however narrow, every line cut
/// to it, the headings faint where it is drawn in colour.
fn columns(headings: &[&str], rows: &[Vec<String>], look: &Look) -> String {
    let widths: Vec<usize> = (0..headings.len())
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .chain(std::iter::once(headings[column].chars().count()))
                .max()
                .unwrap_or_default()
        })
        .collect();
    let mut reach = 0;
    let shown = widths
        .iter()
        .take_while(|width| {
            reach += if reach == 0 { 0 } else { GAP.len() } + **width;
            reach <= look.width
        })
        .count()
        .max(1);
    let line = |cells: Vec<&str>| {
        let drawn: Vec<String> = cells
            .iter()
            .zip(&widths)
            .take(shown)
            .map(|(cell, width)| padded(cell, *width))
            .collect();
        fit(drawn.join(GAP).trim_end(), look.width)
    };
    let mut out = look.paint(crate::swarm::FAINT, &line(headings.to_vec()));
    out.push('\n');
    for row in rows {
        out.push_str(&line(row.iter().map(String::as_str).collect()));
        out.push('\n');
    }
    out
}

/// `machine`'s row, in [`MACHINE_COLUMNS`]' order.
fn row(machine: &Machine, look: &Look) -> Vec<String> {
    vec![
        machine.name.clone(),
        state(machine.state).to_owned(),
        machine.cpu.busy.map(percent).unwrap_or_default(),
        memory(machine),
        disk(machine),
        power(machine),
        updated(machine, look),
    ]
}

/// `builder`'s row, in [`BUILDER_COLUMNS`]' order.
fn builder_row(builder: &Builder) -> Vec<String> {
    vec![
        builder.name.clone(),
        builder.state.clone(),
        jobs(builder),
        builder.systems.join(", "),
    ]
}

/// What about `machine` is worth a person's look, a line each: an update
/// that failed, a disk filling, or the queue's limits it is at.
fn worth_a_look(machine: &Machine, look: &Look) -> Vec<String> {
    let mut worth = Vec::new();
    let update = machine.updated.as_ref();
    if update.and_then(|update| update["step"].as_str()) == Some("failed") {
        let why = update
            .and_then(|update| update["why"].as_str())
            .unwrap_or("it said no more");
        let when = update
            .and_then(|update| update["at"].as_str())
            .and_then(|at| look.when(at))
            .map(|when| format!(" at {when}"))
            .unwrap_or_default();
        worth.push(format!("{}'s update failed{when}: {why}", machine.name));
    }
    match machine.disk_band {
        Some(Band::NearlyFull) => worth.push(format!("{}'s disk is nearly full", machine.name)),
        Some(Band::CriticallyFull) => worth.push(format!("{}'s disk is full", machine.name)),
        _ => {}
    }
    let over = machine.room.as_ref().and_then(|room| room.over.as_deref());
    if let Some(over) = over.filter(|over| !over.is_empty()) {
        let limits: Vec<&str> = over
            .iter()
            .map(|limit| match limit {
                Limit::Memory => "memory",
                Limit::Cpu => "processor",
            })
            .collect();
        worth.push(format!(
            "{} is at its queue's {} limit, so it starts no more heavy work",
            machine.name,
            limits.join(" and ")
        ));
    }
    worth
}

/// How `state` reads.
fn state(state: Readiness) -> &'static str {
    match state {
        Readiness::Ready => "ready",
        Readiness::Unreachable => "unreachable",
    }
}

/// `share`, 0 to 1, as a whole percentage.
fn percent(share: f64) -> String {
    format!("{:.0}%", share * 100.0)
}

/// `bytes` in gigabytes, whole once past ten.
fn gigabytes(bytes: u64) -> String {
    // A machine's bytes fit a float's 52 bits until 4 PB.
    #[expect(clippy::cast_precision_loss, reason = "a machine's bytes fit 52 bits")]
    let gb = bytes as f64 / GB;
    if gb >= 10.0 {
        format!("{gb:.0}")
    } else {
        format!("{gb:.1}")
    }
}

/// What of `machine`'s memory is free, of all it has.
fn memory(machine: &Machine) -> String {
    let free = machine.room.as_ref().map(|room| room.memory_available);
    match (free, machine.memory) {
        (Some(free), Some(all)) => format!("{} of {} GB", gigabytes(free), gigabytes(all)),
        (None, Some(all)) => format!("? of {} GB", gigabytes(all)),
        _ => String::new(),
    }
}

/// What of `machine`'s disk is free, of all it has, and how it fills.
fn disk(machine: &Machine) -> String {
    let filling = match machine.disk_band {
        Some(Band::Filling) => ", filling",
        _ => "",
    };
    match (machine.disk_free, machine.disk) {
        (Some(free), Some(all)) => {
            format!("{} of {} GB{filling}", gigabytes(free), gigabytes(all))
        }
        (None, Some(all)) => format!("? of {} GB", gigabytes(all)),
        _ => String::new(),
    }
}

/// Where `machine` draws its power from, and its charge where it has one.
fn power(machine: &Machine) -> String {
    match machine.power {
        Some(power) if power.on_battery => match power.charge {
            Some(charge) => format!("battery {charge}%"),
            None => "battery".to_owned(),
        },
        Some(power) => match power.charge {
            Some(charge) => format!("mains {charge}%"),
            None => "mains".to_owned(),
        },
        None => String::new(),
    }
}

/// How `machine`'s last update went, and when.
fn updated(machine: &Machine, look: &Look) -> String {
    let Some(update) = machine.updated.as_ref() else {
        return String::new();
    };
    let step = update["step"].as_str().unwrap_or("said");
    match update["at"].as_str().and_then(|at| look.when(at)) {
        Some(when) => format!("{step} {when}"),
        None => step.to_owned(),
    }
}

/// How many jobs `builder` takes at once, and whether it is lent.
fn jobs(builder: &Builder) -> String {
    let lent = if builder.lent { ", lent" } else { "" };
    format!("{}{lent}", builder.jobs)
}

/// `words` joined by commas, the empty ones left out.
fn joined<'a>(words: impl Iterator<Item = &'a str>) -> String {
    words
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::swarm::Shapes;

    fn look(width: usize) -> Look {
        Look {
            width,
            colour: false,
            // UTC as jiff holds it, since a build has no zone database.
            now: "2026-11-12T17:00:00Z"
                .parse::<jiff::Timestamp>()
                .expect("a time")
                .to_zoned(jiff::tz::TimeZone::UTC),
            shapes: Shapes::default(),
        }
    }

    fn kestrel() -> Machine {
        serde_json::from_value(json!({
            "name": "kestrel", "measured_at": "2026-11-12T16:59:58Z", "state": "ready",
            "os": "NixOS 26.11", "system": "linux", "arch": "x86_64",
            "model": "Framework Laptop 13", "model_name": "Laptop 13",
            "cpu": {"model": "AMD Ryzen 7", "cores": 16, "busy": 0.42},
            "memory": 64_000_000_000_u64, "disk": 1_000_000_000_000_u64,
            "disk_free": 160_000_000_000_u64, "disk_band": "filling",
            "accelerators": [], "power": {"on_battery": false, "charge": 76},
            "room": {"said_at": "2026-11-12T16:59:50Z", "memory_available": 20_500_000_000_u64,
                "pressure": {"cpu": 10.0, "memory": 2.0, "io": 0.0},
                "takes": {"builds": 2, "vm_tests": 1}, "over": ["memory"]},
            "declared": null, "reached": {"home": true, "far": false},
            "updated": {"step": "done", "at": "2026-11-12T16:29:10Z"},
        }))
        .expect("a machine")
    }

    fn osprey() -> Machine {
        serde_json::from_value(json!({
            "name": "osprey", "measured_at": "2026-11-12T16:59:12Z", "state": "ready",
            "os": "macOS 26.6", "system": "macos", "arch": "aarch64",
            "model": "Mac16,8", "model_name": "MacBook Pro",
            "cpu": {"model": "Apple M4 Pro", "cores": 12},
            "memory": 48_000_000_000_u64, "disk": 994_000_000_000_u64,
            "disk_free": 130_000_000_000_u64, "accelerators": null,
            "power": {"on_battery": true, "charge": 100}, "room": null, "declared": null,
            "updated": {"step": "failed", "at": "2026-11-12T16:29:02Z",
                "why": "building the system failed (exit status: 1)"},
        }))
        .expect("a machine")
    }

    fn skiff() -> Builder {
        serde_json::from_value(json!({
            "name": "skiff", "systems": ["x86_64-linux", "aarch64-linux"], "jobs": 4,
            "speed": 1.0, "supported": ["big-parallel"], "mandatory": [], "lines": ["standing"],
            "lent": true, "state": "unknown", "said": "it is reached through a jump host",
        }))
        .expect("a builder")
    }

    #[test]
    fn the_machines_are_a_row_each_then_whats_worth_a_look_then_the_builders() {
        let drawn = table(&[kestrel(), osprey()], &[skiff()], &look(120));
        assert_eq!(
            drawn,
            "machine  state  busy  memory free  disk free                power         updated\n\
             kestrel  ready  42%   20 of 64 GB  160 of 1000 GB, filling  mains 76%     done 16:29\n\
             osprey   ready        ? of 48 GB   130 of 994 GB            battery 100%  failed 16:29\n\
             \n\
             kestrel is at its queue's memory limit, so it starts no more heavy work\n\
             osprey's update failed at 16:29: building the system failed (exit status: 1)\n\
             \n\
             builder  state    jobs     systems\n\
             skiff    unknown  4, lent  x86_64-linux, aarch64-linux\n\
             skiff: it is reached through a jump host\n"
        );
    }

    #[test]
    fn no_line_passes_a_narrow_terminal() {
        let drawn = [
            table(&[kestrel(), osprey()], &[skiff()], &look(40)),
            one(&osprey(), &look(40)),
            one_builder(&skiff(), &look(40)),
        ]
        .concat();
        let long: Vec<&str> = drawn
            .lines()
            .filter(|line| line.chars().count() > 40)
            .collect();
        assert!(long.is_empty(), "lines past 40 columns: {long:?}");
    }

    #[test]
    fn a_narrow_terminal_shows_whole_columns_and_wraps_whats_worth_a_look() {
        assert_eq!(
            table(&[osprey()], &[], &look(40)),
            "machine  state  busy  memory free\n\
             osprey   ready        ? of 48 GB\n\
             \n\
             osprey's update failed at 16:29:\n\
             \x20 building the system failed (exit\n\
             \x20 status: 1)\n"
        );
    }

    #[test]
    fn one_machine_is_a_line_for_each_of_its_facts() {
        assert_eq!(
            one(&kestrel(), &look(100)),
            "kestrel  ready\n\
             \x20 what      Laptop 13, NixOS 26.11, x86_64\n\
             \x20 cpu       AMD Ryzen 7, 16 cores, 42% busy\n\
             \x20 memory    20 of 64 GB\n\
             \x20 disk      160 of 1000 GB, filling\n\
             \x20 power     mains 76%\n\
             \x20 reaches   home\n\
             \x20 updated   done 16:29\n\
             \x20 kestrel is at its queue's memory limit, so it starts no more heavy work\n"
        );
    }

    #[test]
    fn no_machine_is_said_in_words() {
        assert_eq!(
            table(&[], &[], &look(80)),
            "No machine of the realm has said what it is.\n"
        );
    }
}
