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
//! network of their own.
//!
//! Everything here is built from the node's `team`, `session` and
//! `machine` records, as the swarm reads them, and says nothing back: the
//! one model every view renders, the terminal's in `shown` and a page's or
//! an agent's from the JSON lines of [`Mycelium::lines`].

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};

use crate::swarm::{self, Standing};

mod kept;
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
    /// How busy a machine's processors are: the percentage, from 0 to 100,
    /// of the last ten seconds in which some task waited on one, as the
    /// kernel's pressure stall information says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub busy: Option<f64>,
    /// How many sessions a machine runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runs: Option<usize>,
    /// The networks it joined, by name.
    pub networks: Vec<String>,
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
    /// The mycelium of `realm` that the node's `teams`, `sessions` and
    /// `machines` records make.
    #[must_use]
    pub fn of(realm: &str, teams: &[Value], sessions: &[Value], machines: &[Value]) -> Self {
        let mut read = Sessions::default();
        for session in sessions {
            read.add(session);
        }
        Self::made(realm, teams, read, machines)
    }

    /// The mycelium of `realm` that the node's `teams` and `machines`
    /// records make, with what its sessions' records made as each was read.
    fn made(realm: &str, teams: &[Value], sessions: Sessions, machines: &[Value]) -> Self {
        let Sessions {
            cells: mut sessions,
            quests,
            renamed,
        } = sessions;
        // A team under an address its orchestrator was renamed from is one
        // a node too old to fold them still says; its sessions are drawn in
        // the orchestrator's own.
        let teams: Vec<Value> = teams
            .iter()
            .filter(|team| !renamed.contains_key(&swarm::address(team).to_lowercase()))
            .cloned()
            .collect();
        let teams = teams.as_slice();
        for cell in &mut sessions {
            let mut seen = BTreeSet::new();
            cell.networks = cell
                .networks
                .iter()
                .map(|name| owned_network(name, &renamed))
                .filter(|name| seen.insert(name.clone()))
                .collect();
        }
        sessions.sort_by(|one, other| one.since.cmp(&other.since).then(one.name.cmp(&other.name)));
        let mut networks = joined(teams, &sessions, &quests);
        networks.push(machines_network(machines, &sessions));
        let names: BTreeSet<&str> = networks.iter().map(|each| each.name.as_str()).collect();
        let leads = led(teams);
        for cell in &mut sessions {
            cell.networks.retain(|name| names.contains(name.as_str()));
            if let Some(led) = cell.id.as_deref().and_then(|id| leads.get(id)) {
                cell.leads.clone_from(led);
            }
        }
        let mut runs: HashMap<&str, usize> = HashMap::new();
        for machine in sessions.iter().filter_map(|cell| cell.machine.as_deref()) {
            *runs.entry(machine).or_default() += 1;
        }
        let machines: Vec<Cell> = machines
            .iter()
            .map(|one| machine_cell(one, &runs))
            .collect();
        // Extended in place, since collecting a chain would hold a second
        // copy of every cell while it is made.
        let mut cells = sessions;
        cells.extend(machines);
        let networks = ordered(networks);
        Self {
            realm: realm.to_owned(),
            places: Places::of(&networks, &cells),
            networks,
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

/// Reads the mycelium from the node at `socket` in the daemon interface,
/// giving up on any read or write taking longer than `within`.
///
/// # Errors
///
/// When no node answers, or it answers in a way it shouldn't, or holds no
/// teams, as a node too old for them doesn't.
pub fn read(socket: &Path, realm: &str, within: Duration) -> io::Result<Mycelium> {
    // A node sends only the fields read, and an older one, which sends
    // every field whatever is asked, is read for only those as it streams.
    let fields = kept::paths(&kept::RECORD);
    let asks = ["team", "session", "machine"]
        .map(|kind| json!({"ask": "list", "kind": kind, "fields": fields}));
    let reading = swarm::asking(socket, within, &asks)?;
    // Teams and machines are few, but sessions are thousands, so each is
    // made its cell as it is read rather than all held first.
    let (mut teams, mut sessions, mut machines) = (Vec::new(), Sessions::default(), Vec::new());
    let answers = kept::answered(reading, 3, |id, record| match id {
        1 => teams.push(record),
        2 => sessions.add(&record),
        _ => machines.push(record),
    })?;
    if let Some(why) = answers
        .values()
        .find_map(|answer| answer["error"]["message"].as_str())
    {
        return Err(io::Error::other(format!(
            "the node can't say its networks ({why}); it may need updating"
        )));
    }
    Ok(Mycelium::made(realm, &teams, sessions, &machines))
}

/// What a node's sessions make, read one by one: their cells, the titles
/// of the quests they work on, and the addresses orchestrators were
/// renamed from.
#[derive(Default)]
struct Sessions {
    cells: Vec<Cell>,
    quests: BTreeMap<String, String>,
    renamed: BTreeMap<String, String>,
}

impl Sessions {
    /// What `session` makes, added to what the sessions before it made.
    fn add(&mut self, session: &Value) {
        // Each quest's title by its short id in lower case, as the last
        // session working on it says.
        let quest = &session["presence"]["questInfo"];
        if let (Some(short), Some(title)) = (quest["short"].as_str(), quest["title"].as_str()) {
            self.quests.insert(short.to_lowercase(), title.to_owned());
        }
        renamed_by(session, &mut self.renamed);
        self.cells.extend(session_cell(session));
    }
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
    let title = swarm::title(session).to_owned();
    Some(Cell {
        kind: "cell",
        name: text("address")?,
        is: Is::Session,
        id: text("id"),
        window: text("title").filter(|window| *window != title),
        title: Some(title),
        standing: Some(standing.counted()),
        doing: standing.doing(session).map(str::to_owned),
        asks: session["wants"]["said"].as_str().map(str::to_owned),
        repo: session["presence"]["repo"].as_str().map(str::to_owned),
        usage: Usage::of(session),
        leads: Vec::new(),
        machine: text("machine"),
        since: session["id"].as_str().and_then(begun),
        quest: text("quest"),
        state: None,
        busy: None,
        runs: None,
        networks,
    })
}

/// The networks each session leads, by its id, as each team names its
/// lead.
fn led(teams: &[Value]) -> BTreeMap<String, Vec<String>> {
    let mut leads: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for team in teams {
        if let Some(lead) = team["lead"].as_str() {
            let network = format!("orchestration.{}", swarm::address(team).to_lowercase());
            leads.entry(lead.to_owned()).or_default().push(network);
        }
    }
    leads
}

/// When the session of `id`, a UUID version 7, began: the milliseconds
/// since the epoch its first 48 bits hold.
fn begun(id: &str) -> Option<String> {
    let hex: String = id.chars().filter(|c| *c != '-').take(12).collect();
    let millis = i64::from_str_radix(&hex, 16).ok()?;
    let at = jiff::Timestamp::from_millisecond(millis).ok()?;
    Some(at.to_string())
}

/// A machine record as a cell, with how many sessions it `runs` by name.
fn machine_cell(machine: &Value, runs: &HashMap<&str, usize>) -> Cell {
    let name = machine["name"].as_str().unwrap_or_default().to_owned();
    let runs = runs.get(name.as_str()).copied().unwrap_or_default();
    Cell {
        kind: "cell",
        title: machine["model_name"].as_str().map(str::to_owned),
        is: Is::Machine,
        id: None,
        window: None,
        standing: None,
        doing: None,
        asks: None,
        repo: None,
        usage: Usage::default(),
        leads: Vec::new(),
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
        glyph: None,
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

/// Each address `session`, if living, goes by beside its own, to its own,
/// both in lower case, added to `renamed` where an earlier session didn't
/// claim it, as the node's teams fold them (`node::team`): an orchestrator
/// renamed keeps its old address, and the sessions it spawned before still
/// hear the old one's group.
fn renamed_by(session: &Value, renamed: &mut BTreeMap<String, String>) {
    if !matches!(session["state"].as_str(), Some("busy" | "idle")) {
        return;
    }
    let Some(own) = session["address"].as_str() else {
        return;
    };
    let others = session["presence"]["addresses"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|other| *other != own);
    for other in others {
        renamed
            .entry(other.to_lowercase())
            .or_insert_with(|| own.to_lowercase());
    }
}

/// The network `name`, under its orchestrator's own address where it
/// names one the orchestrator was renamed from.
fn owned_network(name: &str, renamed: &BTreeMap<String, String>) -> String {
    name.strip_prefix("orchestration.")
        .and_then(|old| renamed.get(old))
        .map_or_else(|| name.to_owned(), |own| format!("orchestration.{own}"))
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
        each.glyph = team["glyph"].as_str().map(str::to_owned);
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
        glyph: None,
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
