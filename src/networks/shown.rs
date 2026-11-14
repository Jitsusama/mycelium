//! The mycelium drawn for a terminal, in the swarm's own rows: every
//! network as a tree under its shape, one network with its cells, every
//! cell, and one cell with the networks it joined.
//!
//! A network's row is a team's row: its short name, its title dimmed after
//! it, and a column for each way its sessions stand, a count of none left
//! blank. A cell's line says how it stands first, so the eye runs down
//! one column for what needs its person.

use std::fmt::Write as _;

use super::{Cell, Is, MACHINES, Mycelium, Network, Shape};
use crate::swarm::{
    BOLD, FAINT, GAP, Label, Look, NARROWEST, Standing, fit, header, padded, row, says_again,
};

impl Shape {
    /// What the tree heads its networks with.
    const fn heading(self) -> &'static str {
        match self {
            Self::Orchestration => "orchestrations",
            Self::Project => "projects",
            Self::Repository => "repositories",
            Self::Group => "groups",
            Self::Machines => "machines",
        }
    }

    /// What one network of it is called.
    const fn said(self) -> &'static str {
        match self {
            Self::Orchestration => "orchestration",
            Self::Project => "project",
            Self::Repository => "repository",
            Self::Group => "group",
            Self::Machines => "the machines' network",
        }
    }
}

/// What a session cell is doing, or asks, beside its line.
fn doing(cell: &Cell) -> Option<&str> {
    cell.doing.as_deref()
}

/// The short name worth showing beside `network`'s title: none where it
/// has no title, or the short name only says it again.
fn shown_short(network: &Network) -> Option<&str> {
    let title = network.title.as_deref()?;
    (!says_again(title, &network.short)).then_some(network.short.as_str())
}

/// How `network`'s row names it, its short name padded to `width`.
fn label(network: &Network, width: usize) -> Label<'_> {
    Label {
        short: shown_short(network),
        width,
        title: network.title.as_deref().unwrap_or(&network.short),
    }
}

/// How wide the short names shown beside `networks` are, the widest's.
fn shorts_width<'a>(networks: impl Iterator<Item = &'a Network>) -> usize {
    networks
        .filter_map(shown_short)
        .map(|short| short.chars().count())
        .max()
        .unwrap_or_default()
}

/// `network`'s counts, in the header's order.
fn counts(network: &Network) -> Vec<(Standing, u64)> {
    Standing::CELLS
        .into_iter()
        .map(|each| (each, network.count(each)))
        .collect()
}

/// The counts of the sessions among `cells`, in the header's order.
fn counts_among(cells: &[&Cell]) -> Vec<(Standing, u64)> {
    Standing::CELLS
        .into_iter()
        .map(|each| {
            let many = cells
                .iter()
                .filter(|cell| cell.standing() == Some(each))
                .count();
            (each, u64::try_from(many).unwrap_or(u64::MAX))
        })
        .collect()
}

/// A machine as its row names it: whether it's ready and how busy.
fn machine_said(machine: &Cell) -> String {
    let mut said = machine
        .state
        .clone()
        .unwrap_or_else(|| "unheard".to_owned());
    if let Some(busy) = machine.busy {
        let _ = write!(said, " · {busy:.0}% busy");
    }
    said
}

/// How the realm stands in a line: its cells, networks and machines, and
/// how many sessions need their person or are stuck.
fn summary(mycelium: &Mycelium, look: &Look) -> String {
    let sessions = mycelium.cells.iter().filter(|cell| cell.is == Is::Session);
    let machines = mycelium
        .cells
        .iter()
        .filter(|cell| cell.is == Is::Machine)
        .count();
    let mut head = format!(
        "{} · {} sessions in {} networks on {machines} machines",
        mycelium.realm,
        sessions.clone().count(),
        mycelium.networks.len()
    );
    for each in [Standing::NeedsYou, Standing::Stuck] {
        let many = sessions
            .clone()
            .filter(|cell| cell.standing() == Some(each))
            .count();
        if many > 0 {
            let said = format!("{many} {}", each.said());
            let _ = write!(head, " · {}", look.paint(each.colour(), &said));
        }
    }
    look.paint(BOLD, &head)
}

/// How a tree is drawn: by whom, how wide its short names are, and
/// whether each network's cells go under it.
struct Drawing<'a> {
    mycelium: &'a Mycelium,
    look: &'a Look,
    width: usize,
    with_cells: bool,
}

impl Drawing<'_> {
    /// `network`'s row, set in by `depth`, then its cells where asked, then
    /// the networks within it.
    fn network(&self, lines: &mut Vec<String>, network: &Network, depth: usize) {
        let label = label(network, self.width);
        lines.push(row(self.look, depth, &label, &counts(network), false));
        if self.with_cells {
            let cells = network
                .cells
                .iter()
                .filter_map(|name| self.mycelium.cell(name));
            lines.extend(cell_lines(self.look, depth + 1, cells));
        }
        let within = self
            .mycelium
            .networks
            .iter()
            .filter(|other| other.within.as_deref() == Some(&network.name));
        for other in within {
            self.network(lines, other, depth + 1);
        }
    }

    /// Each machine's row, counting the sessions it runs, and those
    /// sessions under it where asked.
    fn machines(&self, lines: &mut Vec<String>, depth: usize) {
        for machine in self
            .mycelium
            .cells
            .iter()
            .filter(|cell| cell.is == Is::Machine)
        {
            let runs = self
                .mycelium
                .cells
                .iter()
                .filter(|cell| cell.machine.as_deref() == Some(machine.name.as_str()))
                .collect::<Vec<_>>();
            let said = machine_said(machine);
            let label = Label {
                short: Some(&machine.name),
                width: self.width,
                title: &said,
            };
            lines.push(row(self.look, depth, &label, &counts_among(&runs), false));
            if self.with_cells {
                lines.extend(cell_lines(self.look, depth + 1, runs.into_iter()));
            }
        }
    }
}

/// Every network as a tree under its shape, nested as each branched from
/// another, those needing their person first; with `with_cells`, each
/// network's cells under it.
#[must_use]
pub fn overview(mycelium: &Mycelium, look: &Look, with_cells: bool) -> String {
    let width = mycelium
        .cells
        .iter()
        .filter(|cell| cell.is == Is::Machine)
        .map(|cell| cell.name.chars().count())
        .chain([shorts_width(mycelium.networks.iter())])
        .max()
        .unwrap_or_default();
    let drawing = Drawing {
        mycelium,
        look,
        width,
        with_cells,
    };
    let mut lines = vec![
        summary(mycelium, look),
        String::new(),
        header(look, 0, &Standing::CELLS),
    ];
    for shape in Shape::EACH {
        lines.push(look.paint(BOLD, shape.heading()));
        if shape == Shape::Machines {
            drawing.machines(&mut lines, 1);
            continue;
        }
        let names: Vec<&str> = mycelium
            .networks
            .iter()
            .map(|each| each.name.as_str())
            .collect();
        let tops = mycelium.networks.iter().filter(|each| {
            each.shape == shape
                && each
                    .within
                    .as_deref()
                    .is_none_or(|above| !names.contains(&above))
        });
        for network in tops {
            drawing.network(&mut lines, network, 1);
        }
    }
    lines.push(String::new());
    let example = mycelium
        .networks
        .first()
        .map_or("<network>", |each| each.short.as_str());
    lines.push(look.paint(
        FAINT,
        &format!(
            "see one by any part of its name: mycelium --realm {} networks {example}",
            mycelium.realm
        ),
    ));
    lines.join("\n")
}

/// The lines of `cells`' sessions, set in by `depth`, worst first, then
/// in the order they began.
fn cell_lines<'a>(look: &Look, depth: usize, cells: impl Iterator<Item = &'a Cell>) -> Vec<String> {
    let mut sessions: Vec<(Standing, &Cell)> = cells
        .filter_map(|cell| Some((cell.standing()?, cell)))
        .collect();
    sessions.sort_by(|(one, a), (other, b)| one.cmp(other).then_with(|| a.since.cmp(&b.since)));
    let name_width = sessions
        .iter()
        .map(|(_, cell)| cell.name.chars().count())
        .max()
        .unwrap_or_default();
    sessions
        .iter()
        .map(|(standing, cell)| cell_line(look, depth, *standing, cell, name_width))
        .collect()
}

/// One session's line: how it stands, its name, its title, and what it is
/// doing, where it says.
fn cell_line(
    look: &Look,
    depth: usize,
    standing: Standing,
    cell: &Cell,
    name_width: usize,
) -> String {
    let said = Standing::CELLS
        .iter()
        .map(|each| each.said().len())
        .max()
        .unwrap_or_default();
    let indent = " ".repeat(depth * 2);
    let lead = indent.len() + said + GAP.len() + name_width + GAP.len();
    let room = look.width.saturating_sub(lead).max(NARROWEST);
    let doing = doing(cell);
    let title_room = if doing.is_some() {
        (room / 2).max(NARROWEST)
    } else {
        room
    };
    let title = fit(cell.title.as_deref().unwrap_or_default(), title_room);
    let mut line = format!(
        "{indent}{}{GAP}{}{GAP}{}",
        look.paint(standing.colour(), &padded(standing.said(), said)),
        padded(&cell.name, name_width),
        padded(&title, title_room)
    );
    if let Some(doing) = doing {
        let left = room.saturating_sub(title_room + GAP.len()).max(NARROWEST);
        let _ = write!(line, "{GAP}{}", look.paint(FAINT, &fit(doing, left)));
    }
    line.trim_end().to_owned()
}

/// The network `asked` names: what it is and is within, the machines its
/// cells run on, the networks within it, and its cells, worst first.
///
/// # Errors
///
/// When no network, or more than one, goes by what was asked.
pub fn network(mycelium: &Mycelium, asked: &str, look: &Look) -> Result<String, String> {
    let network = mycelium.named_network(asked)?;
    let mut head = network
        .title
        .clone()
        .unwrap_or_else(|| network.short.clone());
    let _ = write!(head, " · {} {}", network.shape.said(), network.short);
    if let Some(above) = network
        .within
        .as_deref()
        .and_then(|above| mycelium.network(above))
    {
        let _ = write!(
            head,
            " · within {}",
            above.title.as_deref().unwrap_or(&above.short)
        );
    }
    let mut lines = vec![look.paint(BOLD, &head)];
    let sessions = network.cells.iter().filter_map(|name| mycelium.cell(name));
    let many = sessions
        .clone()
        .filter(|cell| cell.is == Is::Session)
        .count();
    if network.shape != Shape::Machines {
        let on = if network.machines.is_empty() {
            String::new()
        } else {
            let machines: Vec<&str> = network.machines.iter().map(String::as_str).collect();
            format!(" on {}", crate::swarm::listed(&machines))
        };
        let cells = if many == 1 { "cell" } else { "cells" };
        lines.push(look.paint(FAINT, &format!("{many} {cells}{on}")));
    }
    let within: Vec<&Network> = mycelium
        .networks
        .iter()
        .filter(|other| other.within.as_deref() == Some(&network.name))
        .collect();
    let drawing = Drawing {
        mycelium,
        look,
        width: shorts_width(within.iter().copied()),
        with_cells: false,
    };
    if network.shape == Shape::Machines {
        lines.extend([String::new(), header(look, 0, &Standing::CELLS)]);
        drawing.machines(&mut lines, 0);
    } else if !within.is_empty() {
        lines.extend([String::new(), header(look, 0, &Standing::CELLS)]);
        for other in within {
            let label = label(other, drawing.width);
            lines.push(row(look, 0, &label, &counts(other), false));
        }
    }
    let cells = cell_lines(look, 0, sessions);
    if !cells.is_empty() {
        lines.push(String::new());
        lines.extend(cells);
    }
    Ok(lines.join("\n"))
}

/// Every cell: each session, worst first, then each machine.
#[must_use]
pub fn cells(mycelium: &Mycelium, look: &Look) -> String {
    let mut lines = vec![summary(mycelium, look), String::new()];
    lines.extend(cell_lines(look, 0, mycelium.cells.iter()));
    let machines: Vec<&Cell> = mycelium
        .cells
        .iter()
        .filter(|cell| cell.is == Is::Machine)
        .collect();
    if !machines.is_empty() {
        lines.extend([String::new(), look.paint(BOLD, MACHINES)]);
        let width = machines
            .iter()
            .map(|cell| cell.name.chars().count())
            .max()
            .unwrap_or_default();
        for machine in machines {
            let runs = machine.runs.unwrap_or_default();
            lines.push(format!(
                "  {}{GAP}{}",
                padded(&machine.name, width),
                look.paint(FAINT, &format!("{} · runs {runs}", machine_said(machine)))
            ));
        }
    }
    lines.push(String::new());
    lines.push(look.paint(
        FAINT,
        &format!(
            "see one by its name: mycelium --realm {} cells <name>",
            mycelium.realm
        ),
    ));
    lines.join("\n")
}

/// One session cell: how it stands and what it is doing, where it runs,
/// since when and on which quest, then each network it joined.
#[must_use]
pub fn cell(mycelium: &Mycelium, cell: &Cell, look: &Look) -> String {
    let title = cell.title.as_deref().unwrap_or_default();
    let mut lines = vec![look.paint(BOLD, &format!("{} · {title}", cell.name))];
    if let Some(standing) = cell.standing() {
        let mut said = look.paint(standing.colour(), standing.said());
        if let Some(doing) = doing(cell) {
            let _ = write!(said, " · {doing}");
        }
        lines.push(said);
    }
    let mut place = Vec::new();
    if let Some(machine) = &cell.machine {
        place.push(format!("on {machine}"));
    }
    if let Some(since) = cell.since.as_deref().and_then(|since| look.when(since)) {
        place.push(format!("since {since}"));
    }
    if let Some(quest) = &cell.quest {
        place.push(quest.clone());
    }
    if !place.is_empty() {
        lines.push(look.paint(FAINT, &place.join(" · ")));
    }
    let joined: Vec<&Network> = cell
        .networks
        .iter()
        .filter_map(|name| mycelium.network(name))
        .collect();
    if !joined.is_empty() {
        lines.extend([String::new(), look.paint(BOLD, "networks")]);
        let width = joined
            .iter()
            .map(|each| each.short.chars().count())
            .max()
            .unwrap_or_default();
        let shapes = joined
            .iter()
            .map(|each| each.shape.said().chars().count())
            .max()
            .unwrap_or_default();
        for each in joined {
            let mut line = format!(
                "  {}{GAP}{}",
                padded(&each.short, width),
                padded(each.shape.said(), shapes)
            );
            if let Some(title) = &each.title {
                let room = look
                    .width
                    .saturating_sub(line.chars().count() + GAP.len())
                    .max(NARROWEST);
                let _ = write!(line, "{GAP}{}", look.paint(FAINT, &fit(title, room)));
            }
            lines.push(line.trim_end().to_owned());
        }
    }
    lines.join("\n")
}
