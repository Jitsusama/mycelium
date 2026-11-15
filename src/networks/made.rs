//! How the mycelium is made from the node's records: each session's cell
//! as it is read, the networks its cells join, titled and nested as the
//! swarm's teams, quests and repositories say, the machines' own, and an
//! outside network for each source the receptors sense.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};

use super::{Builds, Cell, EventLine, Is, MACHINES, Mycelium, Network, Places, Shape, Usage, kept};
use crate::receptor::{self, Counts, Source};
use crate::swarm::{self, Standing};

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

    /// The mycelium of `realm` that the node's `teams`, `sessions` and
    /// `machines` records make, each session's made its cell as it comes
    /// and let go, so thousands are never all held at once.
    #[must_use]
    pub fn of_each(
        realm: &str,
        teams: &[Value],
        sessions: impl IntoIterator<Item = Value>,
        machines: &[Value],
    ) -> Self {
        let mut read = Sessions::default();
        for session in sessions {
            read.add(&session);
        }
        Self::made(realm, teams, read, machines)
    }

    /// The same mycelium with the builders the node's `builders` records
    /// name beside its machines: cells of their own after the machines',
    /// and named in the machines' network as builders, never as its cells,
    /// since no session runs on one and no work is placed there.
    #[must_use]
    pub fn with_builders(mut self, builders: &[Value]) -> Self {
        let cells: Vec<Cell> = builders.iter().filter_map(builder_cell).collect();
        if cells.is_empty() {
            return self;
        }
        if let Some(machines) = self
            .networks
            .iter_mut()
            .find(|each| each.shape == Shape::Machines)
        {
            machines.builders = cells.iter().map(|cell| cell.name.clone()).collect();
        }
        self.cells.extend(cells);
        self.places = Places::of(&self.networks, &self.cells);
        self
    }

    /// The mycelium of `realm` that the node's `teams` and `machines`
    /// records make, with what its sessions' records made as each was read.
    fn made(realm: &str, teams: &[Value], sessions: Sessions, machines: &[Value]) -> Self {
        let Sessions {
            cells: mut sessions,
            quests,
            renamed,
            mut events,
        } = sessions;
        events.sort_by(|one, other| other.at.cmp(&one.at).then(one.cell.cmp(&other.cell)));
        // A team under an address its orchestrator was renamed from is one
        // a node too old to fold them still says; its sessions are drawn in
        // the orchestrator's own.
        let teams: Vec<Value> = teams
            .iter()
            .filter(|team| !renamed.contains_key(&swarm::address(team).to_lowercase()))
            .cloned()
            .collect();
        let teams = teams.as_slice();
        // Renamed in place, and the few a cell joins kept once each, as
        // copying every name of thousands of cells took a good part of the
        // build.
        for cell in &mut sessions {
            if !renamed.is_empty() {
                for name in &mut cell.networks {
                    if let Some(own) = owned_network(name, &renamed) {
                        *name = own;
                    }
                }
            }
            let mut kept = 0;
            for at in 0..cell.networks.len() {
                if !cell.networks[..kept].contains(&cell.networks[at]) {
                    cell.networks.swap(kept, at);
                    kept += 1;
                }
            }
            cell.networks.truncate(kept);
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
            events,
        }
    }

    /// This mycelium with the cells the node's `receptors` records make,
    /// each on the outside network of the source it senses. A record that
    /// names no source a receptor senses, or no machine, makes nothing.
    #[must_use]
    pub fn sensing(mut self, receptors: &[Value]) -> Self {
        let cells: Vec<Cell> = receptors.iter().filter_map(receptor_cell).collect();
        if cells.is_empty() {
            return self;
        }
        let mut sensing: BTreeMap<&str, Vec<&Cell>> = BTreeMap::new();
        for cell in &cells {
            for name in &cell.networks {
                sensing.entry(name).or_default().push(cell);
            }
        }
        let outside = sensing.into_iter().map(|(name, cells)| {
            let mut network = network_of(name, Shape::Outside, &cells);
            network.title = cells.first().and_then(|cell| cell.title.clone());
            network
        });
        let mut networks = std::mem::take(&mut self.networks);
        networks.extend(outside);
        self.networks = ordered(networks);
        self.cells.extend(cells);
        self.places = Places::of(&self.networks, &self.cells);
        self
    }
}

/// A receptor's record as a cell, named for its source and its machine,
/// or none for one naming no source a receptor senses, or no machine.
fn receptor_cell(record: &Value) -> Option<Cell> {
    let source = Source::named(record["source"].as_str()?)?;
    let machine = record["machine"].as_str()?.to_owned();
    let text = |field: &str| record[field].as_str().map(str::to_owned);
    Some(Cell {
        kind: "cell",
        name: format!("{}@{machine}", source.name()),
        is: Is::Receptor,
        title: Some(source.title().to_owned()),
        id: None,
        window: None,
        standing: None,
        doing: None,
        asks: None,
        repo: None,
        usage: Usage::default(),
        leads: Vec::new(),
        machine: Some(machine),
        since: text("since"),
        quest: None,
        plan: None,
        cwd: None,
        state: text("state"),
        busy: None,
        runs: None,
        counts: serde_json::from_value::<Counts>(record["counts"].clone()).ok(),
        builds: None,
        networks: vec![source.network()],
    })
}

/// Reads the mycelium from the node at `socket` in the daemon interface,
/// giving up on any read or write taking longer than `within`.
///
/// # Errors
///
/// When no node answers, or it answers in a way it shouldn't, or holds no
/// teams, as a node too old for them doesn't.
pub fn read(socket: &Path, realm: &str, within: Duration) -> io::Result<Mycelium> {
    let asked = asks();
    let reading = swarm::asking(socket, within, &asked)?;
    // Teams, machines and receptors are few, but sessions are thousands,
    // so each is made its cell as it is read rather than all held first.
    let (mut teams, mut sessions, mut machines) = (Vec::new(), Sessions::default(), Vec::new());
    let (mut receptors, mut builders) = (Vec::new(), Vec::new());
    let answers = kept::answered(reading, BUILDERS_ASKED, |id, record| match id {
        1 => teams.push(record),
        2 => sessions.add(&record),
        3 => machines.push(record),
        RECEPTORS_ASKED => receptors.push(record),
        _ => builders.push(record),
    })?;
    // A node too old to hold receptors or know builders refuses their
    // asks, and is read for the rest; its mycelium senses nothing outside
    // and is drawn without builders.
    if let Some(why) = answers
        .iter()
        .filter(|(id, _)| **id != RECEPTORS_ASKED && **id != BUILDERS_ASKED)
        .find_map(|(_, answer)| answer["error"]["message"].as_str())
    {
        return Err(io::Error::other(format!(
            "the node can't say its networks ({why}); it may need updating"
        )));
    }
    Ok(Mycelium::made(realm, &teams, sessions, &machines)
        .sensing(&receptors)
        .with_builders(&builders))
}

/// Which of the networks' asks is for the node's receptors.
const RECEPTORS_ASKED: u64 = 4;

/// Which of the networks' asks is for the node's builders: the last, so
/// also how many there are.
const BUILDERS_ASKED: u64 = 5;

/// What the networks ask a node: its teams, sessions, machines,
/// receptors and builders, of only the fields read. A node sends only
/// those, and an older one, which sends every field whatever is asked, is
/// read for only those as it streams.
pub(super) fn asks() -> [Value; 5] {
    let fields = kept::paths(&kept::RECORD);
    let [teams, sessions, machines, receptors] = ["team", "session", "machine", receptor::KIND]
        .map(|kind| json!({"ask": "list", "kind": kind, "fields": fields}));
    let builders = json!({"ask": "list", "kind": "builder", "fields": BUILDER_FIELDS});
    [teams, sessions, machines, receptors, builders]
}

/// What the networks read of a builder's record.
const BUILDER_FIELDS: [&str; 7] = ["name", "state", "systems", "jobs", "lines", "lent", "said"];

/// A builder record as a cell, or none for one without a name.
fn builder_cell(builder: &Value) -> Option<Cell> {
    let name = builder["name"].as_str().filter(|name| !name.is_empty())?;
    let words = |field: &str| -> Vec<String> {
        builder[field]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    Some(Cell {
        kind: "cell",
        name: name.to_owned(),
        is: Is::Builder,
        title: Some("builder".to_owned()),
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
        plan: None,
        cwd: None,
        state: builder["state"].as_str().map(str::to_owned),
        busy: None,
        runs: None,
        builds: Some(Box::new(Builds {
            systems: words("systems"),
            jobs: builder["jobs"].as_u64().unwrap_or_default(),
            lines: words("lines"),
            lent: builder["lent"].as_bool().unwrap_or_default(),
            said: builder["said"].as_str().map(str::to_owned),
        })),
        counts: None,
        networks: vec![MACHINES.to_owned()],
    })
}

/// What a node's sessions make, read one by one: their cells, the titles
/// of the quests they work on, the addresses orchestrators were renamed
/// from, and what the cells drawn say they did.
#[derive(Default)]
struct Sessions {
    cells: Vec<Cell>,
    quests: BTreeMap<String, String>,
    renamed: BTreeMap<String, String>,
    events: Vec<EventLine>,
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
        let Some(cell) = session_cell(session) else {
            return;
        };
        self.events.extend(events_of(session, &cell));
        self.cells.push(cell);
    }
}

/// The events a session record says, each a line of `cell`'s.
fn events_of<'a>(session: &'a Value, cell: &'a Cell) -> impl Iterator<Item = EventLine> + 'a {
    session["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(move |event| {
            let text = |field: &str| event[field].as_str().map(str::to_owned);
            Some(EventLine {
                kind: "event",
                cell: cell.name.clone(),
                session: cell.id.clone()?,
                at: text("at")?,
                what: text("kind")?,
                place: text("where")?,
                title: text("title")?,
            })
        })
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
        plan: session
            .get("doing")
            .filter(|doing| doing.is_object())
            .cloned(),
        cwd: session["presence"]["cwd"].as_str().map(str::to_owned),
        state: None,
        busy: None,
        runs: None,
        counts: None,
        builds: None,
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
        plan: None,
        cwd: None,
        state: machine["state"].as_str().map(str::to_owned),
        busy: machine["room"]["pressure"]["cpu"].as_f64(),
        runs: Some(runs),
        counts: None,
        builds: None,
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
        builders: Vec::new(),
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
    titled_by_repos(&mut networks, sessions);
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

/// The network `name` under its orchestrator's own address, where it
/// names one the orchestrator was renamed from.
fn owned_network(name: &str, renamed: &BTreeMap<String, String>) -> Option<String> {
    name.strip_prefix("orchestration.")
        .and_then(|old| renamed.get(old))
        .map(|own| format!("orchestration.{own}"))
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

/// Each repository's network titled by the repository's own name, as its
/// cells say it, where that name is the one its group was made from:
/// `core.nix` for `repo.core-nix`. A name that reads as its short one
/// does, `skiff` for `repo.skiff`, would only say it twice, so titles
/// nothing.
///
/// A network's cells are in the order of `sessions`, so the first of them
/// naming it is the first session that does; each is looked up by name
/// rather than every session searched for each network, which at
/// thousands of cells took most of a second.
fn titled_by_repos(networks: &mut [Network], sessions: &[Cell]) {
    let mut repos: HashMap<&str, &str> = HashMap::new();
    for cell in sessions {
        if let Some(repo) = cell.repo.as_deref() {
            repos.entry(cell.name.as_str()).or_insert(repo);
        }
    }
    for each in networks
        .iter_mut()
        .filter(|each| each.shape == Shape::Repository)
    {
        each.title = each
            .cells
            .iter()
            .filter_map(|name| repos.get(name.as_str()).copied())
            .find(|repo| *repo != each.short && slug(repo) == each.short)
            .map(str::to_owned);
    }
}

/// The name a group gives `text`, as the mesh makes a repository's: in
/// lower case, each run of what isn't a letter or a digit one hyphen.
pub(crate) fn slug(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_alphanumeric() {
            slug.extend(c.to_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_owned()
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
        builders: Vec::new(),
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
