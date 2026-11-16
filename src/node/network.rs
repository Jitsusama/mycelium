//! The realm's networks as the daemon interface holds them: a record each,
//! made by the very model `mycelium networks` draws (`crate::networks`),
//! from the node's own team, session and machine records, so the map, the
//! command and every other reader read one model rather than each making
//! its own.
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
use crate::record::Session;
use crate::swarm;

/// The kind of a network's records.
pub const KIND: &str = "network";

/// Every network the node's `teams`, `machines` and `builders` records
/// and its `sessions` make, by key, each session standing as `standing`
/// says.
/// Each record carries the fields of the network the networks view makes,
/// and is heard when the latest of its cells was, or its team where its
/// cells say nothing.
pub fn records(
    teams: &BTreeMap<String, Value>,
    sessions: &[Session],
    standing: impl Fn(&Session) -> Option<&'static str>,
    machines: &BTreeMap<String, Value>,
    builders: &BTreeMap<String, Value>,
) -> BTreeMap<String, Value> {
    let mut heard: HashMap<String, i64> = HashMap::new();
    let mut hear = |name: String, at: Option<i64>| {
        if let Some(at) = at {
            let latest = heard.entry(name).or_insert(at);
            *latest = (*latest).max(at);
        }
    };
    for team in teams.values() {
        let name = format!("orchestration.{}", swarm::address(team).to_lowercase());
        hear(name, team["heard"].as_i64());
    }
    for machine in machines.values() {
        let name = machine["name"].as_str().unwrap_or_default().to_owned();
        hear(name, machine["heard"].as_i64());
    }
    for session in sessions {
        hear(session.address.clone(), Some(millis_at(&session.at)));
    }
    let teams: Vec<Value> = teams.values().cloned().collect();
    let machines: Vec<Value> = machines.values().cloned().collect();
    let each = sessions
        .iter()
        .map(|session| lean(session, standing(session)));
    // The realm names none of its networks' fields.
    let builders: Vec<Value> = builders.values().cloned().collect();
    let mut mycelium = Mycelium::of_each("", &teams, each, &machines).with_builders(&builders);
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
            let mut fields = json_of(&network);
            // Its own `kind` is the interface's, so is said once, not kept
            // again under `source` as another's would be.
            if let Some(fields) = fields.as_object_mut() {
                fields.remove("kind");
            }
            let record = daemon::record(KIND, &network.name, latest, fields);
            (daemon::key(KIND, &network.name), record)
        })
        .collect()
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
    put("presence", networks::kept_presence(&session.presence));
    put("standing", standing.into());
    Value::Object(fields)
}
