//! The realm's networks as the daemon interface holds them: a record each,
//! made by the very model `mycelium networks` draws (`crate::networks`),
//! from the node's own team, session and machine records, so the map, the
//! command and every other reader read one model rather than each making
//! its own.
//!
//! A session's record is mostly what the networks never read, so each is
//! made for them lean, with only that of its presence, and let go once
//! its cell is made: ten thousand cells build within 100 ms and 30 MB.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde_json::Value;

use super::{json_of, millis_at};
use crate::daemon;
use crate::networks::{self, Mycelium};
use crate::record::Session;
use crate::swarm;

/// The kind of a network's records.
pub const KIND: &str = "network";

/// Every network the node's `teams` and `machines` records and its
/// `sessions` make, by key, each session standing as `standing` says.
/// Each record carries the fields of the network the networks view makes,
/// and is heard when the latest of its cells was, or its team where its
/// cells say nothing.
pub fn records(
    teams: &BTreeMap<String, Value>,
    sessions: &[Session],
    standing: impl Fn(&Session) -> Option<&'static str>,
    machines: &BTreeMap<String, Value>,
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
    let mycelium = Mycelium::of_each("", &teams, each, &machines);
    mycelium
        .networks
        .iter()
        .map(|network| {
            let latest = network
                .cells
                .iter()
                .chain([&network.name])
                .filter_map(|name| heard.get(name))
                .max()
                .copied()
                .unwrap_or_default();
            let record = daemon::record(KIND, &network.name, latest, json_of(network));
            (daemon::key(KIND, &network.name), record)
        })
        .collect()
}

/// `session`'s record as the networks read it, `standing` as it does:
/// its presence only what they read of it, and none of its landings.
fn lean(session: &Session, standing: Option<&str>) -> Value {
    let lean = Session {
        presence: Arc::new(networks::kept_presence(&session.presence)),
        landings: Vec::new(),
        ..session.clone()
    };
    let mut fields = json_of(&lean);
    if let (Some(standing), Some(fields)) = (standing, fields.as_object_mut()) {
        fields.insert("standing".to_owned(), standing.into());
    }
    fields
}
