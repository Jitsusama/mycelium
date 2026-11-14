//! The mycelium as its person reads it: cells, and the networks they join.
//!
//! A cell is a session or a machine. A network is a space cells share for
//! one concern, which a cell joins by carrying its shape, so a session
//! joins the networks of the work it does: each mesh group it hears, but
//! its own address, is one. An orchestration's network is a team of the
//! swarm, its title, short name and nesting the team's; a project's is a
//! quest's group, counted only where two cells or more share it, since
//! otherwise it is one cell's own; a repository's and a named group's are
//! their groups; and the machines are cells of a network of their own.
//!
//! Everything here is built from the node's `team`, `session` and
//! `machine` records, as the swarm reads them, and says nothing back: the
//! one model every view renders, the terminal's in `shown` and a page's or
//! an agent's from the JSON lines of [`Mycelium::lines`].

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use crate::swarm::{self, Standing};

mod shown;

pub use shown::{cell, cells, network, overview};

/// The machines' own network's name.
pub const MACHINES: &str = "machines";

/// The mycelium of one realm, as its node holds it now.
#[derive(Debug, Default, Serialize)]
pub struct Mycelium {
    /// The realm it is, such as `work`.
    pub realm: String,
    /// Every network, in the order the tree draws them.
    pub networks: Vec<Network>,
    /// Every cell: the sessions running or dormant, then the machines.
    pub cells: Vec<Cell>,
}

/// What a network is for, which says where it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    /// An orchestrator and every session on a quest below it.
    Orchestration,
    /// A quest two cells or more work on.
    Project,
    /// A repository its cells work in.
    Repository,
    /// A group its cells named and joined.
    Group,
    /// The realm's machines.
    Machines,
}

impl Shape {
    /// Each, in the order the tree draws them.
    pub const EACH: [Self; 5] = [
        Self::Orchestration,
        Self::Project,
        Self::Repository,
        Self::Group,
        Self::Machines,
    ];

    /// The shape a group's name says, or none for a peer's own address.
    fn of_group(group: &str) -> Option<Self> {
        match group.split_once('.').map_or(group, |(first, _)| first) {
            "peer" => None,
            "orchestration" => Some(Self::Orchestration),
            "quest" => Some(Self::Project),
            "repo" => Some(Self::Repository),
            _ => Some(Self::Group),
        }
    }
}

/// One network, and the cells it holds.
#[derive(Debug, Clone, Serialize)]
pub struct Network {
    /// Always `network`, for a reader of mixed lines.
    pub kind: &'static str,
    /// Its name, as the mesh names its group, such as
    /// `orchestration.h2vyob`, `repo.world` or `machines`.
    pub name: String,
    /// What it is for.
    pub shape: Shape,
    /// What a person types to reach it, such as `grow` or `world`.
    pub short: String,
    /// What it is about, where something says.
    pub title: Option<String>,
    /// The network it branched from, by name.
    pub within: Option<String>,
    /// Its cells, by name.
    pub cells: Vec<String>,
    /// The machines its cells run on.
    pub machines: Vec<String>,
    /// How many of its sessions stand as each, by the word each is counted
    /// by: a machine's network counts the sessions its machines run.
    pub counts: BTreeMap<&'static str, u64>,
}

impl Network {
    /// The names a person may type to reach it.
    fn naming(&self) -> swarm::Naming<'_> {
        swarm::Naming {
            address: &self.name,
            short: Some(&self.short),
            title: self.title.as_deref().unwrap_or_default(),
        }
    }

    /// How many of its sessions stand as `each`.
    #[must_use]
    pub(crate) fn count(&self, each: Standing) -> u64 {
        self.counts.get(each.counted()).copied().unwrap_or_default()
    }

    /// How the worst of its sessions stand, none where it has none.
    fn worst(&self) -> Option<Standing> {
        Standing::CELLS
            .into_iter()
            .find(|each| self.count(*each) > 0)
    }

    /// How many sessions it counts at all.
    fn size(&self) -> u64 {
        self.counts.values().sum()
    }
}

/// What a cell is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Is {
    /// A pi working on a quest.
    Session,
    /// A machine of the realm.
    Machine,
}

/// One cell: a session or a machine.
#[derive(Debug, Clone, Serialize)]
pub struct Cell {
    /// Always `cell`, for a reader of mixed lines.
    pub kind: &'static str,
    /// Its name: a session's address, a machine's own name.
    pub name: String,
    /// What it is.
    #[serde(rename = "type")]
    pub is: Is,
    /// What it is called, where it says.
    pub title: Option<String>,
    /// How a session stands, by the word it is counted by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standing: Option<&'static str>,
    /// What a session is doing, or asks, where it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doing: Option<String>,
    /// The machine a session runs on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// When a session began, from its id, so its place never moves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// The quest a session works on, by its id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quest: Option<String>,
    /// How a machine is, such as `ready`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// How busy a machine's processors are, as the share of the last ten
    /// seconds some task waited on one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub busy: Option<f64>,
    /// How many sessions a machine runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runs: Option<usize>,
    /// The networks it joined, by name.
    pub networks: Vec<String>,
}

impl Cell {
    /// The names a person may type to reach it.
    fn naming(&self) -> swarm::Naming<'_> {
        swarm::Naming {
            address: &self.name,
            short: None,
            title: self.title.as_deref().unwrap_or_default(),
        }
    }

    /// How a session stands; none for a machine.
    #[must_use]
    pub(crate) fn standing(&self) -> Option<Standing> {
        let word = self.standing?;
        Standing::CELLS
            .into_iter()
            .find(|each| each.counted() == word)
    }
}

impl Mycelium {
    /// The mycelium of `realm` that the node's `teams`, `sessions` and
    /// `machines` records make.
    #[must_use]
    pub fn of(realm: &str, teams: &[Value], sessions: &[Value], machines: &[Value]) -> Self {
        let quests = quest_titles(sessions);
        let mut sessions: Vec<Cell> = sessions.iter().filter_map(session_cell).collect();
        sessions.sort_by(|one, other| one.since.cmp(&other.since).then(one.name.cmp(&other.name)));
        let mut networks = joined(teams, &sessions, &quests);
        networks.push(machines_network(machines, &sessions));
        let names: BTreeSet<&str> = networks.iter().map(|each| each.name.as_str()).collect();
        for cell in &mut sessions {
            cell.networks.retain(|name| names.contains(name.as_str()));
        }
        let machines = machines.iter().map(|one| machine_cell(one, &sessions));
        let cells = sessions.iter().cloned().chain(machines).collect();
        Self {
            realm: realm.to_owned(),
            networks: ordered(networks),
            cells,
        }
    }

    /// Each network, then each cell, a JSON value each, as `-o json`
    /// prints them a line each.
    #[must_use]
    pub fn lines(&self) -> Vec<Value> {
        let networks = self.networks.iter().map(|each| json!(each));
        networks
            .chain(self.cells.iter().map(|each| json!(each)))
            .collect()
    }

    /// The network of this name.
    #[must_use]
    pub fn network(&self, name: &str) -> Option<&Network> {
        self.networks.iter().find(|each| each.name == name)
    }

    /// The cell of this name.
    #[must_use]
    pub fn cell(&self, name: &str) -> Option<&Cell> {
        self.cells.iter().find(|each| each.name == name)
    }

    /// The network `asked` names, by its name, its short name or a part of
    /// its title, as the swarm finds a team; or why none or several are,
    /// naming each of several by its name, so one can be picked.
    ///
    /// # Errors
    ///
    /// When no network, or more than one, goes by what was asked.
    pub fn named_network(&self, asked: &str) -> Result<&Network, String> {
        let mesh_spelt = asked.trim_start_matches('#').replace('/', ".");
        if mesh_spelt != asked {
            match swarm::picked(&self.networks, &mesh_spelt, Network::naming) {
                swarm::Picked::One(one) => return Ok(one),
                swarm::Picked::Several(many) => {
                    return Err(several(asked, &many, |each| &each.name));
                }
                swarm::Picked::None => {}
            }
        }
        match swarm::picked(&self.networks, asked, Network::naming) {
            swarm::Picked::One(one) => Ok(one),
            swarm::Picked::None => Err(format!("no network is named {asked}")),
            swarm::Picked::Several(many) => Err(several(asked, &many, |each| &each.name)),
        }
    }

    /// The cell `asked` names, by its name or a part of its title; or why
    /// none or several are.
    ///
    /// # Errors
    ///
    /// When no cell, or more than one, goes by what was asked.
    pub fn named_cell(&self, asked: &str) -> Result<&Cell, String> {
        match swarm::picked(&self.cells, asked, Cell::naming) {
            swarm::Picked::One(one) => Ok(one),
            swarm::Picked::None => Err(format!("no cell is named {asked}")),
            swarm::Picked::Several(many) => Err(several(asked, &many, |each| &each.name)),
        }
    }
}

/// That `asked` could be any of `many`, each by its `name`.
fn several<T>(asked: &str, many: &[&T], name: impl Fn(&T) -> &str) -> String {
    let names: Vec<&str> = many.iter().map(|each| name(each)).collect();
    format!("{asked} could be {}", swarm::either(&names))
}

/// Reads the mycelium from the node at `socket` in the daemon interface,
/// giving up on any read or write taking longer than `within`.
///
/// # Errors
///
/// When no node answers, or it answers in a way it shouldn't, or holds no
/// teams, as a node too old for them doesn't.
pub fn read(socket: &Path, realm: &str, within: Duration) -> io::Result<Mycelium> {
    let mut answers = swarm::asked(
        socket,
        within,
        &[
            json!({"ask": "list", "kind": "team"}),
            json!({"ask": "list", "kind": "session"}),
            json!({"ask": "list", "kind": "machine"}),
        ],
    )?;
    let mut records = |id: u64| -> io::Result<Vec<Value>> {
        let answer = answers.remove(&id).unwrap_or_default();
        if let Some(why) = answer["error"]["message"].as_str() {
            return Err(io::Error::other(format!(
                "the node can't say its networks ({why}); it may need updating"
            )));
        }
        Ok(answer["records"].as_array().cloned().unwrap_or_default())
    };
    let (teams, sessions, machines) = (records(1)?, records(2)?, records(3)?);
    Ok(Mycelium::of(realm, &teams, &sessions, &machines))
}

/// A session record as a cell, or none for one that has ended.
fn session_cell(session: &Value) -> Option<Cell> {
    let standing = Standing::of_cell(session)?;
    let text = |field: &str| session[field].as_str().map(str::to_owned);
    let networks = session["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|group| Shape::of_group(group).is_some())
        .map(str::to_owned)
        .collect();
    Some(Cell {
        kind: "cell",
        name: text("address")?,
        is: Is::Session,
        title: Some(swarm::title(session).to_owned()),
        standing: Some(standing.counted()),
        doing: standing.doing(session).map(str::to_owned),
        machine: text("machine"),
        since: session["id"].as_str().and_then(begun),
        quest: text("quest"),
        state: None,
        busy: None,
        runs: None,
        networks,
    })
}

/// When the session of `id`, a UUID version 7, began: the milliseconds
/// since the epoch its first 48 bits hold.
fn begun(id: &str) -> Option<String> {
    let hex: String = id.chars().filter(|c| *c != '-').take(12).collect();
    let millis = i64::from_str_radix(&hex, 16).ok()?;
    let at = jiff::Timestamp::from_millisecond(millis).ok()?;
    Some(at.to_string())
}

/// A machine record as a cell, with how many of `sessions` it runs.
fn machine_cell(machine: &Value, sessions: &[Cell]) -> Cell {
    let name = machine["name"].as_str().unwrap_or_default().to_owned();
    let runs = sessions
        .iter()
        .filter(|cell| cell.machine.as_deref() == Some(name.as_str()))
        .count();
    Cell {
        kind: "cell",
        title: machine["model_name"].as_str().map(str::to_owned),
        is: Is::Machine,
        standing: None,
        doing: None,
        machine: None,
        since: None,
        quest: None,
        state: machine["state"].as_str().map(str::to_owned),
        busy: machine["room"]["pressure"]["cpu"].as_f64(),
        runs: Some(runs),
        networks: vec![MACHINES.to_owned()],
        name,
    }
}

/// How many of `cells` stand as each, by the word each is counted by.
fn counted<'a>(cells: impl Iterator<Item = &'a Cell>) -> BTreeMap<&'static str, u64> {
    let mut counts: BTreeMap<&'static str, u64> = Standing::CELLS
        .into_iter()
        .map(|each| (each.counted(), 0))
        .collect();
    for standing in cells.filter_map(|cell| cell.standing) {
        *counts.entry(standing).or_default() += 1;
    }
    counts
}

/// A network named `name`, of `shape`, holding `cells`.
fn network_of(name: &str, shape: Shape, cells: &[&Cell]) -> Network {
    let machines: BTreeSet<&str> = cells
        .iter()
        .filter_map(|cell| cell.machine.as_deref())
        .collect();
    let short = match shape {
        Shape::Group => name.replace('.', "/"),
        _ => name.rsplit('.').next().unwrap_or(name).to_owned(),
    };
    Network {
        kind: "network",
        name: name.to_owned(),
        shape,
        short,
        title: None,
        within: None,
        cells: cells.iter().map(|cell| cell.name.clone()).collect(),
        machines: machines.into_iter().map(str::to_owned).collect(),
        counts: counted(cells.iter().copied()),
    }
}

/// The networks `sessions` joined, each team of the swarm's one among
/// them, with or without a cell, and a project's or a repository's only
/// where two cells share it: one cell's own is no network.
fn joined(teams: &[Value], sessions: &[Cell], quests: &BTreeMap<String, String>) -> Vec<Network> {
    let mut holding: BTreeMap<&str, Vec<&Cell>> = BTreeMap::new();
    for cell in sessions {
        for name in &cell.networks {
            holding.entry(name).or_default().push(cell);
        }
    }
    let orchestrated =
        |team: &Value| format!("orchestration.{}", swarm::address(team).to_lowercase());
    let team_names: Vec<String> = teams.iter().map(orchestrated).collect();
    for name in &team_names {
        holding.entry(name).or_default();
    }
    let mut networks: Vec<Network> = holding
        .iter()
        .filter_map(|(name, cells)| {
            let shape = Shape::of_group(name)?;
            let shared = !matches!(shape, Shape::Project | Shape::Repository) || cells.len() > 1;
            shared.then(|| network_of(name, shape, cells))
        })
        .collect();
    titled_by_teams(&mut networks, teams);
    titled_by_quests(&mut networks, quests);
    networks
}

/// Each quest's title by its short id in lower case, as the sessions
/// working on it say.
fn quest_titles(sessions: &[Value]) -> BTreeMap<String, String> {
    sessions
        .iter()
        .filter_map(|session| {
            let quest = &session["presence"]["questInfo"];
            let short = quest["short"].as_str()?.to_lowercase();
            Some((short, quest["title"].as_str()?.to_owned()))
        })
        .collect()
}

/// Each orchestration's network titled, short-named and nested as the
/// swarm's team of its address is.
fn titled_by_teams(networks: &mut [Network], teams: &[Value]) {
    let shorts = swarm::short_names(teams);
    let by_name: BTreeMap<String, &Value> = teams
        .iter()
        .map(|team| (swarm::address(team).to_lowercase(), team))
        .collect();
    for each in networks
        .iter_mut()
        .filter(|each| each.shape == Shape::Orchestration)
    {
        let Some(team) = by_name.get(&each.short) else {
            continue;
        };
        each.title = Some(swarm::title(team).to_owned());
        if let Some(short) = shorts.get(swarm::address(team)) {
            each.short.clone_from(short);
        }
        each.within = team["within"]
            .as_str()
            .map(str::to_lowercase)
            .filter(|above| by_name.contains_key(above))
            .map(|above| format!("orchestration.{above}"));
    }
}

/// Each project's network titled as its quest is, among `quests`, and
/// nested in the nearest network of a quest it is within.
fn titled_by_quests(networks: &mut [Network], quests: &BTreeMap<String, String>) {
    let projects: BTreeSet<String> = networks
        .iter()
        .filter(|each| each.shape == Shape::Project)
        .map(|each| each.name.clone())
        .collect();
    for each in networks
        .iter_mut()
        .filter(|each| each.shape == Shape::Project)
    {
        each.title = quests.get(&each.short).cloned();
        let mut above = each.name.as_str();
        each.within = loop {
            match above.rsplit_once('.') {
                Some((up, _)) if projects.contains(up) => break Some(up.to_owned()),
                Some((up, _)) => above = up,
                None => break None,
            }
        };
    }
}

/// The machines' own network, counting the sessions each runs.
fn machines_network(machines: &[Value], sessions: &[Cell]) -> Network {
    let names: Vec<String> = machines
        .iter()
        .filter_map(|machine| machine["name"].as_str().map(str::to_owned))
        .collect();
    Network {
        kind: "network",
        name: MACHINES.to_owned(),
        shape: Shape::Machines,
        short: MACHINES.to_owned(),
        title: Some("the realm's machines".to_owned()),
        within: None,
        cells: names.clone(),
        machines: names,
        counts: counted(sessions.iter()),
    }
}

/// `networks` in the order the tree draws them: by shape, then those
/// needing their person first, then the largest, then by name.
fn ordered(mut networks: Vec<Network>) -> Vec<Network> {
    networks.sort_by(|one, other| {
        one.shape
            .cmp(&other.shape)
            .then_with(|| rank(one).cmp(&rank(other)))
            .then_with(|| other.size().cmp(&one.size()))
            .then_with(|| one.name.cmp(&other.name))
    });
    networks
}

/// Where a network's worst session puts it, one with none last.
fn rank(network: &Network) -> usize {
    network
        .worst()
        .and_then(|worst| Standing::CELLS.iter().position(|each| *each == worst))
        .unwrap_or(Standing::CELLS.len())
}

#[cfg(test)]
mod tests;
