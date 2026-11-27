//! The mycelium drawn for a terminal, in the swarm's own rows: every
//! network as a tree under its shape, one network with its cells, every
//! cell, and one cell with the networks it joined.
//!
//! A network's row is a team's row: its short name, its title dimmed after
//! it, and a column for each way its sessions stand, a count of none left
//! blank. A cell's line says how it stands first, so the eye runs down
//! one column for what needs its person.
//!
//! A mycelium of thousands has lists no screen holds, so a long list
//! folds: what needs its person or is stuck first, then the busiest few,
//! then one row for the rest, counting them. A view asked for `all` folds
//! nothing, and any one of what folded is seen by its name.
//!
//! An orchestration's title follows the mark its shape is drawn by, the
//! one the launcher and the map draw it by, where the theme has one.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;

use super::{Cell, Is, MACHINES, Mycelium, Network, Shape, Share};
use crate::swarm::{
    BOLD, DOING_AT, FAINT, GAP, Label, Look, NARROWEST, Standing, drawn_width, fit, header, padded,
    row, says_again, shell_word, unmarked, without_marks,
};

impl Shape {
    /// What the tree heads its networks with.
    const fn heading(self) -> &'static str {
        match self {
            Self::Orchestration => "orchestrations",
            Self::Project => "projects",
            Self::Repository => "repositories",
            Self::Tree => "trees",
            // Folders, not shared files: a session says it works in one,
            // and nothing here asks whether the share keeps it in step.
            Self::Files => "folders",
            Self::Group => "groups",
            Self::Machines => "machines",
            Self::Outside => "outside",
        }
    }

    /// What one network of it is called.
    const fn said(self) -> &'static str {
        match self {
            Self::Orchestration => "orchestration",
            Self::Project => "project",
            Self::Repository => "repository",
            Self::Tree => "tree",
            Self::Files => "folder",
            Self::Group => "group",
            Self::Machines => "the machines' network",
            Self::Outside => "outside source",
        }
    }
}

/// What a session cell is doing, or asks, beside its line.
fn doing(cell: &Cell) -> Option<&str> {
    cell.doing.as_deref()
}

/// The short name worth showing beside `network`'s title: none where it
/// has no title, or the short name only says it again, as `core-nix`
/// says core.nix.
fn shown_short(network: &Network) -> Option<&str> {
    let title = network.title.as_deref()?;
    let again = says_again(title, &network.short) || super::slug(title) == network.short;
    (!again).then_some(network.short.as_str())
}

/// `network`'s title, or its short name where it has none, after the
/// mark its shape is drawn by where `look`'s theme has one.
fn titled<'a>(network: &'a Network, look: &Look) -> Cow<'a, str> {
    let title = network.title.as_deref().unwrap_or(&network.short);
    let mark = network
        .glyph
        .as_deref()
        .and_then(|glyph| look.shapes.mark(glyph));
    mark.map_or(Cow::Borrowed(title), |mark| {
        Cow::Owned(format!("{mark} {title}"))
    })
}

/// How `network`'s row names it, its short name padded to `width`, then
/// `title`.
fn label<'a>(network: &'a Network, title: &'a str, width: usize) -> Label<'a> {
    Label {
        short: shown_short(network),
        width,
        title,
    }
}

/// How wide the short names shown beside `networks` are, the widest's.
fn shorts_width<'a>(networks: impl Iterator<Item = &'a Network>) -> usize {
    networks
        .filter_map(shown_short)
        .map(|short| drawn_width(&unmarked(short)))
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

/// A machine as its row names it: whether it's ready and how busy, on
/// one line however its node put it.
fn machine_said(machine: &Cell) -> String {
    let mut said = machine
        .state
        .clone()
        .unwrap_or_else(|| "unheard".to_owned());
    if let Some(busy) = machine.busy {
        let _ = write!(said, " · {busy:.0}% busy");
    }
    unmarked(&said)
}

/// How a receptor stands, and what it brought in today, as its line in
/// the cells says it, on one line.
fn receptor_said(receptor: &Cell) -> String {
    let mut said = receptor
        .state
        .clone()
        .unwrap_or_else(|| "unheard".to_owned());
    if let Some(counts) = receptor.counts {
        let _ = write!(
            said,
            " · {} this hour · {} today, {} to you",
            counts.hour, counts.today, counts.addressed
        );
    }
    unmarked(&said)
}

/// What a builder is and how it stands, as its row says it: the files
/// naming it, its state, its systems and how many builds it takes, on
/// one line.
fn builder_said(builder: &Cell) -> String {
    let state = builder.state.as_deref().unwrap_or("unheard");
    let Some(builds) = &builder.builds else {
        return unmarked(state);
    };
    let jobs = if builds.jobs == 1 { "job" } else { "jobs" };
    unmarked(&format!(
        "{} · {state} · {} · {} {jobs}",
        builds.lines.join(", "),
        builds.systems.join(", "),
        builds.jobs
    ))
}

/// A builder in the few words a tree's row has room for: `lent` for a
/// lent one, as what costs to run, else the files naming it; then its
/// state.
fn builder_brief(builder: &Cell) -> String {
    let state = builder.state.as_deref().unwrap_or("unheard");
    match &builder.builds {
        Some(builds) if builds.lent => format!("lent · {state}"),
        Some(builds) => format!("{} · {state}", builds.lines.join(", ")),
        None => state.to_owned(),
    }
}

/// The builders of `mycelium`, drawn beside its machines.
fn builders(mycelium: &Mycelium) -> impl Iterator<Item = &Cell> {
    mycelium.cells.iter().filter(|cell| cell.is == Is::Builder)
}

/// How a long list folds: past how many it folds at all, how many of
/// those needing their person or stuck it shows then, and how many of the
/// rest, the busiest, before one row counts what folded.
struct Fold {
    past: usize,
    worrying: usize,
    busiest: usize,
}

/// How the networks under a heading, or within a network, fold.
const NETWORKS_FOLD: Fold = Fold {
    past: 12,
    worrying: 8,
    busiest: 4,
};

/// How a list of cells, or of machines, folds.
const CELLS_FOLD: Fold = Fold {
    past: 30,
    worrying: 20,
    busiest: 5,
};

/// Whether a view's long lists fold, and whether any has.
struct Folding {
    all: bool,
    any: std::cell::Cell<bool>,
}

impl Folding {
    /// Folding unless `all` is asked.
    const fn unless(all: bool) -> Self {
        Self {
            all,
            any: std::cell::Cell::new(false),
        }
    }

    /// `list`, in its order, split into what is shown and what folds, as
    /// `fold` says, `worrying` saying which need their person or are
    /// stuck.
    fn split<T>(
        &self,
        fold: &Fold,
        list: Vec<T>,
        worrying: impl Fn(&T) -> bool,
    ) -> (Vec<T>, Vec<T>) {
        if self.all || list.len() <= fold.past {
            return (list, Vec::new());
        }
        let (mut worried, mut others) = (0, 0);
        let (shown, folded): (Vec<T>, Vec<T>) = list.into_iter().partition(|each| {
            if worrying(each) {
                worried += 1;
                worried <= fold.worrying
            } else {
                others += 1;
                others <= fold.busiest
            }
        });
        if !folded.is_empty() {
            self.any.set(true);
        }
        (shown, folded)
    }

    /// The line saying how to see what folded, if anything did, by the
    /// `command` that drew it.
    fn hint(&self, look: &Look, command: &str) -> Option<String> {
        self.any.get().then(|| {
            look.paint(
                FAINT,
                &format!("some lists folded; see every one with: {command} --all"),
            )
        })
    }
}

/// Whether `network` holds a session needing its person or stuck.
fn network_worrying(network: &Network) -> bool {
    network.count(Standing::NeedsYou) + network.count(Standing::Stuck) > 0
}

/// Whether `cell` needs its person or is stuck.
fn cell_worrying(cell: &Cell) -> bool {
    matches!(cell.standing(), Some(Standing::NeedsYou | Standing::Stuck))
}

/// The counts of `networks` together, in the header's order.
fn counts_of(networks: &[&Network]) -> Vec<(Standing, u64)> {
    Standing::CELLS
        .into_iter()
        .map(|each| {
            (
                each,
                networks.iter().map(|network| network.count(each)).sum(),
            )
        })
        .collect()
}

/// The row counting `many` folded of what `kind` names, under rows whose
/// short names are `width` wide.
fn folded_row(
    look: &Look,
    depth: usize,
    width: usize,
    many: usize,
    kind: &str,
    counts: &[(Standing, u64)],
) -> String {
    let said = format!("and {many} more {kind}");
    let label = Label {
        short: Some(""),
        width,
        title: &said,
    };
    row(look, depth, &label, counts, false)
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
    let builders = builders(mycelium).count();
    if builders > 0 {
        let _ = write!(head, " and {builders} builders");
    }
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

/// How a tree is drawn: by whom, how wide its short names are, whether
/// each network's cells go under it, and how its lists fold; with the
/// networks within each network and the sessions each machine runs, found
/// once for the whole tree.
struct Drawing<'a> {
    mycelium: &'a Mycelium,
    look: &'a Look,
    width: usize,
    with_cells: bool,
    folding: &'a Folding,
    within: HashMap<&'a str, Vec<&'a Network>>,
    runs: HashMap<&'a str, Vec<&'a Cell>>,
}

impl<'a> Drawing<'a> {
    /// A tree of `mycelium`'s, its short names `width` wide.
    fn of(
        mycelium: &'a Mycelium,
        look: &'a Look,
        width: usize,
        with_cells: bool,
        folding: &'a Folding,
    ) -> Self {
        let mut within: HashMap<&str, Vec<&Network>> = HashMap::new();
        for network in &mycelium.networks {
            if let Some(above) = network.within.as_deref() {
                within.entry(above).or_default().push(network);
            }
        }
        let mut runs: HashMap<&str, Vec<&Cell>> = HashMap::new();
        for cell in &mycelium.cells {
            if let Some(machine) = cell.machine.as_deref() {
                runs.entry(machine).or_default().push(cell);
            }
        }
        Self {
            mycelium,
            look,
            width,
            with_cells,
            folding,
            within,
            runs,
        }
    }

    /// The networks within the one named `name`, in their order.
    fn within(&self, name: &str) -> Vec<&'a Network> {
        self.within.get(name).cloned().unwrap_or_default()
    }

    /// The rows of `networks`, set in by `depth`, as many as fold, then a
    /// row counting the rest.
    fn networks(&self, lines: &mut Vec<String>, networks: Vec<&'a Network>, depth: usize) {
        let (shown, folded) = self
            .folding
            .split(&NETWORKS_FOLD, networks, |each| network_worrying(each));
        for network in shown {
            self.network(lines, network, depth);
        }
        if !folded.is_empty() {
            let counts = counts_of(&folded);
            lines.push(folded_row(
                self.look,
                depth,
                self.width,
                folded.len(),
                "networks",
                &counts,
            ));
        }
    }

    /// `network`'s row, set in by `depth`, then its cells where asked, then
    /// the networks within it.
    fn network(&self, lines: &mut Vec<String>, network: &Network, depth: usize) {
        let title = titled(network, self.look);
        let label = label(network, &title, self.width);
        lines.push(row(self.look, depth, &label, &counts(network), false));
        if self.with_cells {
            let cells = network
                .cells
                .iter()
                .filter_map(|name| self.mycelium.cell(name));
            lines.extend(cell_lines(self.look, depth + 1, cells, self.folding));
        }
        self.networks(lines, self.within(&network.name), depth + 1);
    }

    /// Each machine's row, counting the sessions it runs, and those
    /// sessions under it where asked, as many as fold.
    fn machines(&self, lines: &mut Vec<String>, depth: usize) {
        let machines: Vec<(&Cell, &[&Cell])> = self
            .mycelium
            .cells
            .iter()
            .filter(|cell| cell.is == Is::Machine)
            .map(|machine| {
                let runs = self.runs.get(machine.name.as_str());
                (machine, runs.map_or(&[][..], Vec::as_slice))
            })
            .collect();
        let (shown, folded) = self
            .folding
            .split(&CELLS_FOLD, machines, |(machine, runs)| {
                machine.state.as_deref() != Some("ready")
                    || runs.iter().any(|cell| cell_worrying(cell))
            });
        for (machine, runs) in shown {
            let said = machine_said(machine);
            let label = Label {
                short: Some(&machine.name),
                width: self.width,
                title: &said,
            };
            lines.push(row(self.look, depth, &label, &counts_among(runs), false));
            if self.with_cells {
                lines.extend(cell_lines(
                    self.look,
                    depth + 1,
                    runs.iter().copied(),
                    self.folding,
                ));
            }
        }
        if !folded.is_empty() {
            let runs: Vec<&Cell> = folded
                .iter()
                .flat_map(|(_, runs)| runs.iter().copied())
                .collect();
            let counts = counts_among(&runs);
            lines.push(folded_row(
                self.look,
                depth,
                self.width,
                folded.len(),
                "machines",
                &counts,
            ));
        }
    }

    /// Each builder's row under its own heading, set in by `depth`, where
    /// there is one; a builder runs no session, so counts none.
    fn builders(&self, lines: &mut Vec<String>, depth: usize) {
        let mut builders = builders(self.mycelium).peekable();
        if builders.peek().is_none() {
            return;
        }
        let indent = "  ".repeat(depth.saturating_sub(1));
        lines.push(format!("{indent}{}", self.look.paint(BOLD, "builders")));
        for builder in builders {
            let said = builder_brief(builder);
            let label = Label {
                short: Some(&builder.name),
                width: self.width,
                title: &said,
            };
            lines.push(row(self.look, depth, &label, &counts_among(&[]), false));
        }
    }
}

/// Every network as a tree under its shape, nested as each branched from
/// another, those needing their person first; with `with_cells`, each
/// network's cells under it. Each long list folds unless `all` is asked.
#[must_use]
pub fn overview(mycelium: &Mycelium, look: &Look, with_cells: bool, all: bool) -> String {
    let width = mycelium
        .cells
        .iter()
        .filter(|cell| matches!(cell.is, Is::Machine | Is::Builder))
        .map(|cell| drawn_width(&unmarked(&cell.name)))
        .chain([shorts_width(mycelium.networks.iter())])
        .max()
        .unwrap_or_default();
    let folding = Folding::unless(all);
    let drawing = Drawing::of(mycelium, look, width, with_cells, &folding);
    let mut lines = vec![
        summary(mycelium, look),
        String::new(),
        header(look, 0, &Standing::CELLS),
    ];
    for shape in Shape::EACH {
        // A realm whose machines sense nothing outside says nothing of it,
        // and a tree or a folder is a network only while two cells share
        // it, so none of their headings is drawn while no such network is.
        let any = mycelium.networks.iter().any(|each| each.shape == shape);
        if matches!(shape, Shape::Outside | Shape::Tree | Shape::Files) && !any {
            continue;
        }
        lines.push(look.paint(BOLD, shape.heading()));
        if shape == Shape::Machines {
            drawing.machines(&mut lines, 1);
            drawing.builders(&mut lines, 1);
            continue;
        }
        let tops = mycelium.networks.iter().filter(|each| {
            each.shape == shape
                && each
                    .within
                    .as_deref()
                    .is_none_or(|above| mycelium.network(above).is_none())
        });
        drawing.networks(&mut lines, tops.collect(), 1);
    }
    lines.push(String::new());
    // A short name comes from a peer's subject, so it is held to one line
    // and of marks, as on its row; and the example is copied into a
    // shell, so it is given only as one word a shell reads as one name, a
    // group's `/` and a tree's `@` among its characters, and else not.
    let example = mycelium
        .networks
        .first()
        .map(|each| unmarked(&each.short))
        .filter(|short| shell_word(short, &['/', '@']))
        .unwrap_or_else(|| "<network>".to_owned());
    lines.push(look.paint(
        FAINT,
        &format!(
            "see one by any part of its name: mycelium --realm {} networks {example}",
            mycelium.realm
        ),
    ));
    let cells = if with_cells { " --cells" } else { "" };
    let command = format!("mycelium --realm {} networks{cells}", mycelium.realm);
    lines.extend(folding.hint(look, &command));
    look.joined(&lines)
}

/// The lines of `cells`' sessions, set in by `depth`, worst first, then
/// in the order they began, as many as fold, then a line counting the
/// rest by how they stand.
fn cell_lines<'a>(
    look: &Look,
    depth: usize,
    cells: impl Iterator<Item = &'a Cell>,
    folding: &Folding,
) -> Vec<String> {
    let mut sessions: Vec<(Standing, &Cell)> = cells
        .filter_map(|cell| Some((cell.standing()?, cell)))
        .collect();
    sessions.sort_by(|(one, a), (other, b)| one.cmp(other).then_with(|| a.since.cmp(&b.since)));
    let (sessions, folded) = folding.split(&CELLS_FOLD, sessions, |(standing, _)| {
        matches!(standing, Standing::NeedsYou | Standing::Stuck)
    });
    let name_width = sessions
        .iter()
        .map(|(_, cell)| drawn_width(&unmarked(&cell.name)))
        .max()
        .unwrap_or_default();
    let mut lines: Vec<String> = sessions
        .iter()
        .map(|(standing, cell)| cell_line(look, depth, *standing, cell, name_width))
        .collect();
    if !folded.is_empty() {
        let cells: Vec<&Cell> = folded.iter().map(|(_, cell)| *cell).collect();
        let counted: Vec<String> = counts_among(&cells)
            .into_iter()
            .filter(|(_, many)| *many > 0)
            .map(|(standing, many)| format!("{many} {}", standing.said()))
            .collect();
        let said = format!("and {} more: {}", folded.len(), counted.join(" · "));
        lines.push(format!(
            "{}{}",
            " ".repeat(depth * 2),
            look.paint(FAINT, &said)
        ));
    }
    lines
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
    let title = unmarked(cell.title.as_deref().unwrap_or_default());
    // Drawn whole, the title is as wide as it is, and what it does is
    // lined up with the others' once they're drawn.
    let title_room = if look.whole() {
        drawn_width(&title)
    } else if doing.is_some() {
        (room / 2).max(NARROWEST)
    } else {
        room
    };
    let mut line = format!(
        "{indent}{}{GAP}{}{GAP}{}",
        look.paint(standing.colour(), &padded(standing.said(), said)),
        padded(&unmarked(&cell.name), name_width),
        padded(&look.fitted(&title, title_room), title_room)
    );
    if let Some(doing) = doing {
        let left = room.saturating_sub(title_room + GAP.len()).max(NARROWEST);
        let _ = write!(
            line,
            "{}{GAP}{}",
            look.mark(DOING_AT),
            look.paint(FAINT, &fit(&unmarked(doing), left))
        );
    }
    line.trim_end().to_owned()
}

/// The network `asked` names: what it is and is within, the machines its
/// cells run on, the networks within it, and its cells, worst first.
///
/// # Errors
///
/// When no network, or more than one, goes by what was asked. Its long
/// lists fold unless `all` is asked.
pub fn network(mycelium: &Mycelium, asked: &str, look: &Look, all: bool) -> Result<String, String> {
    let network = mycelium.named_network(asked)?;
    let mut head = titled(network, look).into_owned();
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
    let mut lines = vec![look.paint(BOLD, &unmarked(&head))];
    let sessions = network.cells.iter().filter_map(|name| mycelium.cell(name));
    let many = sessions
        .clone()
        .filter(|cell| cell.is == Is::Session)
        .count();
    if network.shape != Shape::Machines {
        let on = if network.machines.is_empty() {
            String::new()
        } else {
            let machines: Vec<String> =
                network.machines.iter().map(|each| unmarked(each)).collect();
            let machines: Vec<&str> = machines.iter().map(String::as_str).collect();
            format!(" on {}", crate::swarm::listed(&machines))
        };
        let cells = if many == 1 { "cell" } else { "cells" };
        lines.push(look.paint(FAINT, &format!("{many} {cells}{on}")));
    }
    let folding = Folding::unless(all);
    let mut drawing = Drawing::of(mycelium, look, 0, false, &folding);
    let within = drawing.within(&network.name);
    drawing.width = shorts_width(within.iter().copied());
    if network.shape == Shape::Machines {
        lines.extend([String::new(), header(look, 0, &Standing::CELLS)]);
        drawing.machines(&mut lines, 0);
        drawing.builders(&mut lines, 0);
    } else if !within.is_empty() {
        lines.extend([String::new(), header(look, 0, &Standing::CELLS)]);
        let (shown, folded) = folding.split(&NETWORKS_FOLD, within, |each| network_worrying(each));
        for other in shown {
            let title = titled(other, look);
            let label = label(other, &title, drawing.width);
            lines.push(row(look, 0, &label, &counts(other), false));
        }
        if !folded.is_empty() {
            let counts = counts_of(&folded);
            lines.push(folded_row(
                look,
                0,
                drawing.width,
                folded.len(),
                "networks",
                &counts,
            ));
        }
    }
    let cells = cell_lines(look, 0, sessions, &folding);
    if !cells.is_empty() {
        lines.push(String::new());
        lines.extend(cells);
    }
    let command = format!("mycelium --realm {} networks {asked}", mycelium.realm);
    if let Some(hint) = folding.hint(look, &command) {
        lines.extend([String::new(), hint]);
    }
    Ok(look.joined(&lines))
}

/// Every cell: each session, worst first, then each machine; or, where
/// `standing` names how some stand, only the sessions standing so. Its
/// long lists fold unless `all` is asked.
#[must_use]
pub fn cells(mycelium: &Mycelium, look: &Look, all: bool, standing: &[String]) -> String {
    let folding = Folding::unless(all);
    let mut lines = vec![summary(mycelium, look), String::new()];
    let mut sessions = mycelium
        .cells
        .iter()
        .filter(|cell| standing.is_empty() || cell.stands_as(standing))
        .peekable();
    if sessions.peek().is_none() && !standing.is_empty() {
        lines.push(nothing_standing(standing));
    } else {
        lines.extend(cell_lines(look, 0, sessions, &folding));
    }
    let machines: Vec<&Cell> = mycelium
        .cells
        .iter()
        .filter(|cell| cell.is == Is::Machine && standing.is_empty())
        .collect();
    let (machines, folded) = folding.split(&CELLS_FOLD, machines, |machine| {
        machine.state.as_deref() != Some("ready")
    });
    if !machines.is_empty() {
        lines.extend([String::new(), look.paint(BOLD, MACHINES)]);
        let width = machines
            .iter()
            .map(|cell| drawn_width(&unmarked(&cell.name)))
            .max()
            .unwrap_or_default();
        for machine in machines {
            let runs = machine.runs.unwrap_or_default();
            lines.push(format!(
                "  {}{GAP}{}",
                padded(&unmarked(&machine.name), width),
                look.paint(FAINT, &format!("{} · runs {runs}", machine_said(machine)))
            ));
        }
        if !folded.is_empty() {
            let said = format!("and {} more machines", folded.len());
            lines.push(format!("  {}", look.paint(FAINT, &said)));
        }
    }
    let builders: Vec<&Cell> = builders(mycelium).filter(|_| standing.is_empty()).collect();
    if !builders.is_empty() {
        lines.extend([String::new(), look.paint(BOLD, "builders")]);
        let width = builders
            .iter()
            .map(|cell| drawn_width(&unmarked(&cell.name)))
            .max()
            .unwrap_or_default();
        for builder in builders {
            let mut said = builder_said(builder);
            if let Some(why) = builder
                .builds
                .as_ref()
                .and_then(|builds| builds.said.as_ref())
            {
                let _ = write!(said, " · {}", unmarked(why));
            }
            lines.push(format!(
                "  {}{GAP}{}",
                padded(&unmarked(&builder.name), width),
                look.paint(FAINT, &said)
            ));
        }
    }
    let receptors: Vec<&Cell> = mycelium
        .cells
        .iter()
        .filter(|cell| cell.is == Is::Receptor && standing.is_empty())
        .collect();
    if !receptors.is_empty() {
        lines.extend([String::new(), look.paint(BOLD, "receptors")]);
        let width = receptors
            .iter()
            .map(|cell| drawn_width(&unmarked(&cell.name)))
            .max()
            .unwrap_or_default();
        for receptor in receptors {
            lines.push(format!(
                "  {}{GAP}{}",
                padded(&unmarked(&receptor.name), width),
                look.paint(FAINT, &receptor_said(receptor))
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
    let mut command = format!("mycelium --realm {} cells", mycelium.realm);
    if !standing.is_empty() {
        let _ = write!(command, " --standing {}", standing.join(","));
    }
    lines.extend(folding.hint(look, &command));
    look.joined(&lines)
}

/// That no session stands as `standing` says, as a person would say it:
/// `needs-you,stuck` is "Nothing needs you and nothing is stuck."
fn nothing_standing(standing: &[String]) -> String {
    let said: Vec<String> = standing
        .iter()
        .enumerate()
        .map(|(at, each)| {
            let nothing = if at == 0 { "Nothing" } else { "nothing" };
            match each.as_str() {
                "needs-you" => format!("{nothing} needs you"),
                other => format!("{nothing} is {other}"),
            }
        })
        .collect();
    match said.split_last() {
        Some((last, [])) => format!("{last}."),
        Some((last, rest)) => format!("{} and {last}.", rest.join(", ")),
        None => String::new(),
    }
}

/// One session cell: how it stands and what it is doing, where it runs,
/// since when and on which quest, then each network it joined.
#[must_use]
pub fn cell(mycelium: &Mycelium, cell: &Cell, look: &Look) -> String {
    // Everything a peer put here is held to one line, as a row is, so
    // none of it can draw an escape or a line of its own.
    let title = unmarked(cell.title.as_deref().unwrap_or_default());
    let name = unmarked(&cell.name);
    let mut lines = vec![look.paint(BOLD, &format!("{name} · {title}"))];
    if let Some(standing) = cell.standing() {
        let mut said = look.paint(standing.colour(), standing.said());
        if let Some(doing) = doing(cell) {
            let _ = write!(said, " · {}", unmarked(doing));
        }
        lines.push(said);
    }
    if cell.is == Is::Receptor {
        lines.push(receptor_said(cell));
    }
    let mut place = Vec::new();
    if let Some(machine) = &cell.machine {
        place.push(format!("on {}", unmarked(machine)));
    }
    if let Some(since) = cell.since.as_deref().and_then(|since| look.when(since)) {
        place.push(format!("since {since}"));
    }
    if let Some(quest) = &cell.quest {
        place.push(unmarked(quest));
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
            .map(|each| drawn_width(&unmarked(&each.short)))
            .max()
            .unwrap_or_default();
        let shapes = joined
            .iter()
            .map(|each| drawn_width(each.shape.said()))
            .max()
            .unwrap_or_default();
        for each in joined {
            let mut line = format!(
                "  {}{GAP}{}",
                padded(&unmarked(&each.short), width),
                padded(each.shape.said(), shapes)
            );
            if each.title.is_some() {
                let title = unmarked(&titled(each, look));
                let room = look
                    .width
                    .saturating_sub(drawn_width(&line) + GAP.len())
                    .max(NARROWEST);
                let _ = write!(line, "{GAP}{}", look.paint(FAINT, &fit(&title, room)));
            }
            lines.push(line.trim_end().to_owned());
        }
    }
    // A tree it shares is among its networks; one it holds alone is its own.
    let own: Vec<String> = cell
        .trees
        .iter()
        .filter(|tree| {
            tree.network()
                .is_none_or(|name| mycelium.network(&name).is_none())
        })
        .filter_map(|tree| tree.shorts().map(|[name, ..]| name))
        .collect();
    if !own.is_empty() {
        lines.extend([String::new(), look.paint(BOLD, "its own trees")]);
        lines.extend(own.iter().map(|short| format!("  {}", unmarked(short))));
    }
    // Likewise a folder it shares with no other cell, so one it says that
    // forms no network can still be seen to be said.
    let alone: Vec<&Share> = cell
        .shares
        .iter()
        .filter(|share| mycelium.network(&share.network()).is_none())
        .collect();
    if !alone.is_empty() {
        lines.extend([String::new(), look.paint(BOLD, "its own folders")]);
        let width = alone
            .iter()
            .map(|share| drawn_width(&share.key))
            .max()
            .unwrap_or_default();
        for share in alone {
            // An untitled folder's line ends at its key, with no gap
            // left after it for a title.
            if share.about.is_empty() {
                lines.push(format!("  {}", share.key));
                continue;
            }
            let line = format!("  {}{GAP}", padded(&share.key, width));
            let room = look.width.saturating_sub(drawn_width(&line)).max(NARROWEST);
            let about = look.paint(FAINT, &fit(&share.about, room));
            lines.push(format!("{line}{about}"));
        }
    }
    // A cell's page draws no table, so any mark left in it is a peer's.
    without_marks(&lines.join("\n"))
}
