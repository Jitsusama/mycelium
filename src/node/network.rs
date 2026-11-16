//! The realm's networks as the daemon interface holds them: a record each,
//! made by the very model `mycelium networks` draws (`crate::networks`),
//! from the node's own team, session, machine, receptor and builder
//! records, so the map, the command and every other reader read one model
//! rather than each making its own. The node's cells (`cell`) are made by
//! the same.
//!
//! A session's record is mostly what the networks never read, so each is
//! made for them lean, with only that of its presence, and let go once
//! its cell is made, and the cells are let go before the records are
//! made: ten thousand cells build within 30 MB, in time that grows with
//! them, as `tests/networks.rs` shows.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Value};

use super::{json_of, millis_at};
use crate::daemon;
use crate::networks::{self, Mycelium};
use crate::receptor::Source;
use crate::record::Session;
use crate::swarm;

/// The kind of a network's records.
pub const KIND: &str = "network";

/// What the node's records hold: its `teams`, `machines`, `receptors` and
/// `builders` records, by key, and its `sessions`.
pub struct Holding<'a> {
    /// Its teams' records.
    pub teams: &'a BTreeMap<String, Value>,
    /// Its sessions.
    pub sessions: &'a [Session],
    /// Its machines' records.
    pub machines: &'a BTreeMap<String, Value>,
    /// Its receptors' records.
    pub receptors: &'a BTreeMap<String, Value>,
    /// Its builders' records.
    pub builders: &'a BTreeMap<String, Value>,
}

/// How a session stands, by the word the daemon interface says it with,
/// such as `running`, or none for one passed over.
pub type Standing<'a> = dyn Fn(&Session) -> Option<&'static str> + 'a;

/// Every network the node's records make, by key, each session standing
/// as `standing` says. Each record carries the fields of the network the
/// networks view makes, and is heard when the latest of its cells was, or
/// its team where its cells say nothing.
pub fn records(held: &Holding<'_>, standing: &Standing<'_>) -> BTreeMap<String, Value> {
    let Made {
        mut mycelium,
        heard,
    } = made(held, standing);
    // Only its networks are made records, so its cells are let go before
    // they are, never held beside them.
    let networks = std::mem::take(&mut mycelium.networks);
    drop(mycelium);
    networks
        .into_iter()
        .map(|network| {
            let latest = network
                .cells
                .iter()
                .chain([&network.name])
                .filter_map(|name| heard.get(name))
                .max()
                .copied()
                .unwrap_or_default();
            (
                daemon::key(KIND, &network.name),
                record(KIND, &network.name, latest, &network),
            )
        })
        .collect()
}

/// The realm's mycelium as the node's records make it, and when each
/// cell and each orchestration was last heard, by name.
pub(super) struct Made {
    /// The mycelium, as `mycelium networks` draws it.
    pub mycelium: Mycelium,
    /// When each cell and each orchestration's team was last heard, in
    /// milliseconds since the epoch, by name.
    pub heard: HashMap<String, i64>,
}

/// The mycelium `held` makes, each session standing as `standing` says,
/// made as `mycelium networks` makes it (`Mycelium::of_records`).
pub(super) fn made(held: &Holding<'_>, standing: &Standing<'_>) -> Made {
    let mut heard: HashMap<String, i64> = HashMap::new();
    let mut hear = |name: String, at: Option<i64>| {
        if let Some(at) = at {
            let latest = heard.entry(name).or_insert(at);
            *latest = (*latest).max(at);
        }
    };
    for team in held.teams.values() {
        let name = format!("orchestration.{}", swarm::address(team).to_lowercase());
        hear(name, team["heard"].as_i64());
    }
    for machine in held.machines.values() {
        let name = machine["name"].as_str().unwrap_or_default().to_owned();
        hear(name, machine["heard"].as_i64());
    }
    for receptor in held.receptors.values() {
        let source = receptor["source"].as_str().and_then(Source::named);
        if let (Some(source), Some(machine)) = (source, receptor["machine"].as_str()) {
            hear(
                format!("{}@{machine}", source.name()),
                receptor["heard"].as_i64(),
            );
        }
    }
    for session in held.sessions {
        hear(session.address.clone(), Some(millis_at(&session.at)));
    }
    let teams: Vec<Value> = held.teams.values().cloned().collect();
    let machines: Vec<Value> = held.machines.values().cloned().collect();
    let receptors: Vec<Value> = held.receptors.values().cloned().collect();
    let builders: Vec<Value> = held.builders.values().cloned().collect();
    let each = held
        .sessions
        .iter()
        .map(|session| lean(session, standing(session)));
    // The realm names none of its networks' or cells' fields.
    let mycelium = Mycelium::of_records("", &teams, each, &machines, &receptors, &builders);
    Made { mycelium, heard }
}

/// The record of `kind` keyed `name`, heard at `heard`, holding `made`'s
/// fields as the networks view says them. Its own `kind` is the
/// interface's, so is said once, not kept again under `source` as
/// another's would be.
pub(super) fn record(kind: &str, name: &str, heard: i64, made: &impl serde::Serialize) -> Value {
    let mut fields = json_of(made);
    if let Some(fields) = fields.as_object_mut() {
        fields.remove("kind");
    }
    daemon::record(kind, name, heard, fields)
}

/// `session`'s record as the networks read it, `standing` as it does:
/// only the fields they read, as a client listing sessions for them asks
/// for, and its presence only what they read of it. Made from the fields
/// themselves rather than the record whole, which at thousands of cells
/// took most of the build.
fn lean(session: &Session, standing: Option<&str>) -> Value {
    let mut fields = Map::new();
    let mut put = |key: &str, value: Value| {
        if !value.is_null() {
            fields.insert(key.to_owned(), value);
        }
    };
    put("id", session.id.as_str().into());
    put("address", session.address.as_str().into());
    put("machine", session.machine.as_str().into());
    put("title", session.title.as_deref().into());
    put("working_on", session.working_on.as_deref().into());
    put("groups", json_of(&session.groups));
    put("state", json_of(&session.state));
    put("step", session.step.as_deref().into());
    put("quest", session.quest.as_deref().into());
    put("wants", json_of(&session.wants));
    put("doing", json_of(&session.doing));
    put("presence", networks::kept_presence(&session.presence));
    put("standing", standing.into());
    Value::Object(fields)
}
