//! The mycelium as its person reads it: cells, and the networks they join.
//!
//! A cell is a session or a machine. A network is a space cells share for
//! one concern, which a cell joins by carrying its shape, so a session
//! joins the networks of the work it does: each mesh group it hears, but
//! its own address, is one. An orchestration's network is a team of the
//! swarm, its title, short name and nesting the team's; a project's and a
//! repository's are a quest's and a repository's groups, counted only
//! where two cells or more share them, since otherwise each is one cell's
//! own; a named group's is its group; and the machines are cells of a
//! network of their own. A receptor, sensing one outside source for its
//! person, is a cell of that source's outside network, beyond the
//! machines.
//!
//! Everything here is built from the node's `team`, `session`,
//! `machine` and `receptor` records, as the swarm reads them, and says nothing back: the
//! one model every view renders, the terminal's in `shown` and a page's or
//! an agent's from the JSON lines of [`Mycelium::lines`].

use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use serde_json::{Value, json};

use crate::swarm::{self, Standing};

mod kept;
mod made;
mod shown;

pub use made::read;
use made::slug;
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
    /// What the sessions drawn say they did lately, newest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventLine>,
    /// Where each network and each cell is among them, by its name, so a
    /// view of thousands finds each one at once.
    #[serde(skip)]
    places: Places,
}

/// Where each network and each cell of a mycelium is, by its name.
#[derive(Debug, Default)]
struct Places {
    networks: HashMap<String, usize>,
    cells: HashMap<String, usize>,
}

impl Places {
    /// Where each of `networks` and `cells` is.
    fn of(networks: &[Network], cells: &[Cell]) -> Self {
        let placed = |names: Vec<&String>| {
            names
                .into_iter()
                .enumerate()
                .map(|(place, name)| (name.clone(), place))
                .collect()
        };
        Self {
            networks: placed(networks.iter().map(|each| &each.name).collect()),
            cells: placed(cells.iter().map(|each| &each.name).collect()),
        }
    }
}

/// The one of `things` named `name`, where `places` says it is, or found
/// by looking through them where it says wrong, as it would of things
/// changed since.
fn placed<'a, T>(
    things: &'a [T],
    places: &HashMap<String, usize>,
    name: &str,
    named: impl Fn(&T) -> &str,
) -> Option<&'a T> {
    places
        .get(name)
        .and_then(|place| things.get(*place))
        .filter(|thing| named(thing) == name)
        .or_else(|| things.iter().find(|thing| named(thing) == name))
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
    /// An outside source the realm's receptors sense, such as Slack.
    Outside,
}

impl Shape {
    /// Each, in the order the tree draws them.
    pub const EACH: [Self; 6] = [
        Self::Orchestration,
        Self::Project,
        Self::Repository,
        Self::Group,
        Self::Machines,
        Self::Outside,
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
    /// The shape it is drawn by, such as `square`, where the node chose
    /// one: only an orchestration's, as its team's record says.
    pub glyph: Option<String>,
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
    /// What senses an outside source for its person, intake only.
    Receptor,
}

/// One cell: a session, a machine or a receptor.
#[derive(Debug, Clone, Serialize)]
pub struct Cell {
    /// Always `cell`, for a reader of mixed lines.
    pub kind: &'static str,
    /// Its name: a session's address, a machine's own name, a receptor's
    /// source and machine, such as `slack@kestrel`.
    pub name: String,
    /// What it is.
    #[serde(rename = "type")]
    pub is: Is,
    /// What it is called, where it says: a session's quest's title, where
    /// it works on one.
    pub title: Option<String>,
    /// A session's id, which stays the same as long as it lives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// A session's window's title, where it is not its `title`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// How a session stands, by the word it is counted by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standing: Option<&'static str>,
    /// What a session is doing, or asks, where it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doing: Option<String>,
    /// What a session asks its person, in its own words. `doing` carries
    /// the same words for a session that asks, as the line a person reads;
    /// this is the ask alone, for a reader that draws it apart, so keep
    /// both.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asks: Option<String>,
    /// The repository a session works in, where it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// What a session has spent today, as its presence says it.
    #[serde(skip_serializing_if = "Usage::is_empty")]
    pub usage: Usage,
    /// The networks a session leads, as each orchestration's team names
    /// its lead.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub leads: Vec<String>,
    /// The machine a session or a receptor runs on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// When a session began, from its id, so its place never moves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// The quest a session works on, by its id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quest: Option<String>,
    /// What a session's plan does next and what it chose last, as its
    /// presence's `doing` says: `plan`, `item`, `next`, `chose` and `at`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<Value>,
    /// The folder a session works in, as an absolute path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// How a machine is, such as `ready`, or a receptor, such as
    /// `reading`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// How busy a machine's processors are: the percentage, from 0 to 100,
    /// of the last ten seconds in which some task waited on one, as the
    /// kernel's pressure stall information says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub busy: Option<f64>,
    /// How many sessions a machine runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runs: Option<usize>,
    /// How many signals a receptor brought in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counts: Option<crate::receptor::Counts>,
    /// The networks it joined, by name.
    pub networks: Vec<String>,
}

/// One thing a session drawn says it did, as a line of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventLine {
    /// Always `event`, for a reader of mixed lines.
    pub kind: &'static str,
    /// The cell that did it, by its name.
    pub cell: String,
    /// Its session, by its id.
    pub session: String,
    /// When, as an RFC 3339 instant.
    pub at: String,
    /// What it did: `commit`, `push`, `pr`, `merge` or `stage`.
    pub what: String,
    /// Where: a repository by its key, or a quest by its id.
    #[serde(rename = "where")]
    pub place: String,
    /// What it is, in one short line.
    pub title: String,
}

/// What a session has spent today, as its presence says it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Usage {
    /// Its cost today, in US dollars.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_today: Option<f64>,
    /// How much of its context window it holds, as `{used, limit}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,
    /// How many turns it took today.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns_today: Option<u64>,
    /// When its last turn began.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_turn: Option<String>,
}

impl Usage {
    /// What the presence of `session` says it spent.
    fn of(session: &Value) -> Self {
        let usage = &session["presence"]["usage"];
        Self {
            cost_today: usage["pi.cost.usd"]["today"].as_f64(),
            context: Some(usage["context"].clone()).filter(Value::is_object),
            turns_today: usage["turns"]["today"].as_u64(),
            last_turn: usage["turns"]["last"]["at"].as_str().map(str::to_owned),
        }
    }

    /// Whether it says nothing.
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
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

    /// Whether it is a session standing as one of `standings` says, by
    /// the words its counts go by, such as `needs-you`.
    #[must_use]
    pub fn stands_as(&self, standings: &[String]) -> bool {
        self.standing
            .is_some_and(|standing| standings.iter().any(|each| each == standing))
    }
}

/// How a session may stand, by the words its counts go by, worst first.
pub const STANDINGS: [&str; 5] = ["needs-you", "stuck", "moving", "quiet", "dormant"];

impl Mycelium {
    /// Each network, then each cell, a JSON value each, as `-o json`
    /// prints them a line each.
    #[must_use]
    pub fn lines(&self) -> Vec<Value> {
        let networks = self.networks.iter().map(|each| json!(each));
        networks
            .chain(self.cells.iter().map(|each| json!(each)))
            .chain(self.events.iter().map(|each| json!(each)))
            .collect()
    }

    /// The network of this name.
    #[must_use]
    pub fn network(&self, name: &str) -> Option<&Network> {
        placed(&self.networks, &self.places.networks, name, |each| {
            &each.name
        })
    }

    /// The cell of this name.
    #[must_use]
    pub fn cell(&self, name: &str) -> Option<&Cell> {
        placed(&self.cells, &self.places.cells, name, |each| &each.name)
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

/// A session's `presence` with only what the networks read of it, so a
/// node making a session's record for them copies little of it.
#[must_use]
pub fn kept_presence(presence: &Value) -> Value {
    kept::pruned(presence, &kept::PRESENCE)
}

#[cfg(test)]
mod tests;
