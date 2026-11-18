//! The cells carried sessions arrive with. A session carried here from
//! another machine runs under a new address, so without what its cell
//! brought it would stand as a cell nobody has heard of, its ties and
//! its name left behind. The machine it left puts its cell beside its
//! done file in the quest's handover ([`crate::carry::cell`]); this node
//! reads it for each session running here, keeps it a day, as long as a
//! tie is told of, and makes the session stand as its cell again:
//! named by it in the networks, its new address an alias of it, and the
//! ties it brought kept beside those this node makes.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;

use crate::carry::cell::{self, Brought};
use crate::carry::through;
use crate::daemon;
use crate::record::Session;

/// How long a cell that arrived is kept, in milliseconds: a day, as a
/// tie is told of, so the file going from the handover, as the machine
/// it left takes it away once its carry has started, loses nothing.
const KEPT: i64 = 24 * 60 * 60 * 1000;

/// A cell a session arrived with, and when it was first read here.
#[derive(Debug, Clone, PartialEq)]
struct Arrival {
    /// What it brought.
    brought: Brought,
    /// When it was first read, in milliseconds since the epoch.
    at: i64,
}

/// The cells sessions running here arrived with, by session id.
#[derive(Debug, Default)]
pub(super) struct Arrivals(Mutex<BTreeMap<String, Arrival>>);

impl Arrivals {
    /// Reads the cell each of `sessions` running on `machine` with a
    /// quest arrived with, in any of `scratch`'s handovers for its quest
    /// but this machine's own, `hostname`'s, and forgets each kept past
    /// [`KEPT`] before `now`. Answers whether what is kept changed.
    pub(super) fn read(
        &self,
        scratch: &Path,
        hostname: &str,
        machine: &str,
        sessions: &[Session],
        now: i64,
    ) -> bool {
        let own = OsStr::new(through::short_host(hostname));
        let mut found = BTreeMap::new();
        for session in sessions.iter().filter(|session| session.machine == machine) {
            let Some(quest) = session.quest.as_deref() else {
                continue;
            };
            if !one_name(quest) || !one_name(&session.id) {
                continue;
            }
            let Ok(handovers) = fs::read_dir(scratch.join(quest).join("handover")) else {
                continue;
            };
            let file = format!("{}{}", session.id, cell::FILE);
            // By name, so which of two machines' cells is read never
            // turns on the order the folder lists them in.
            let mut left: Vec<_> = handovers
                .flatten()
                .filter(|from| from.file_name() != own)
                .map(|from| from.path())
                .collect();
            left.sort();
            if let Some(brought) = left
                .iter()
                .find_map(|from| cell::arrived(&from.join(&file), &session.id))
            {
                found.insert(session.id.clone(), brought);
            }
        }
        let mut kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let before = kept.clone();
        kept.retain(|_, arrival| now - arrival.at < KEPT);
        for (session, brought) in found {
            match kept.get_mut(&session) {
                Some(arrival) if arrival.brought == brought => {}
                Some(arrival) => arrival.brought = brought,
                None => {
                    kept.insert(session, Arrival { brought, at: now });
                }
            }
        }
        *kept != before
    }

    /// Each session's cell, by session id.
    fn cells(&self) -> BTreeMap<String, String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(session, arrival)| (session.clone(), arrival.brought.cell.clone()))
            .collect()
    }

    /// `sessions` as they stand as cells: each that arrived named by the
    /// cell it brought, its own address kept among its presence's, so
    /// the networks name it as before it was carried.
    pub(super) fn named(&self, sessions: &mut [Session]) {
        let cells = self.cells();
        for session in sessions {
            let Some(cell) = cells.get(&session.id) else {
                continue;
            };
            if session.address.eq_ignore_ascii_case(cell) {
                continue;
            }
            let address = std::mem::replace(&mut session.address, cell.clone());
            let presence = Arc::make_mut(&mut session.presence);
            if !presence.is_object() {
                *presence = Value::Object(serde_json::Map::new());
            }
            if !presence["addresses"].is_array() {
                presence["addresses"] = Value::Array(Vec::new());
            }
            if let Some(addresses) = presence["addresses"].as_array_mut() {
                addresses.push(Value::String(address));
            }
        }
    }

    /// `aliases`, as the tie kind names cells, with each session's
    /// address, and every alias of it, naming the cell it arrived with
    /// instead, as `sessions` say each session's address.
    pub(super) fn alias<'a>(
        &self,
        sessions: impl IntoIterator<Item = &'a Session>,
        aliases: &mut HashMap<String, String>,
    ) {
        let cells = self.cells();
        for session in sessions {
            let Some(cell) = cells.get(&session.id) else {
                continue;
            };
            let address = session.address.to_ascii_uppercase();
            for named in aliases.values_mut() {
                if *named == address {
                    named.clone_from(cell);
                }
            }
            aliases.insert(address, cell.clone());
            aliases.insert(cell.clone(), cell.clone());
        }
    }

    /// `made`, the tie records this node makes, with each tie and talk
    /// the cells that arrived brought heard within a day of `now`,
    /// where this node holds none of it heard later.
    pub(super) fn ties(&self, made: &mut BTreeMap<String, daemon::Record>, now: i64) {
        let kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let brought = kept.values().flat_map(|arrival| {
            let Brought { ties, talk, .. } = &arrival.brought;
            ties.iter().chain(talk)
        });
        for record in brought {
            let (Some(key), Some(heard)) = (record["key"].as_str(), record["heard"].as_i64())
            else {
                continue;
            };
            if now - heard >= KEPT {
                continue;
            }
            let later = made.get(key).is_some_and(|held| held.heard() >= heard);
            if !later {
                made.insert(key.to_owned(), daemon::Record::from(record.clone()));
            }
        }
    }
}

/// Whether `name` names one entry of a folder, never a path.
fn one_name(name: &str) -> bool {
    Path::new(name).file_name() == Some(OsStr::new(name))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;

    use serde_json::{Value, json};

    use super::{Arrivals, KEPT};
    use crate::daemon;
    use crate::record::Session;

    const QUEST: &str = "QEST-20261112-HARBOR";
    const NOW: i64 = 1_763_400_000_000;

    /// The session `id` going by `address` on `machine`, in [`QUEST`].
    fn session(id: &str, address: &str, machine: &str) -> Session {
        let mut session: Session = serde_json::from_value(json!({
            "id": id, "address": address, "machine": machine, "groups": [],
            "state": "busy",
            "at": "2026-11-17T17:20:00Z",
        }))
        .expect("a session");
        session.quest = Some(QUEST.to_owned());
        session
    }

    /// Puts `said` as the cell `session` brought from `from`'s handover.
    fn left(scratch: &Path, from: &str, session: &str, said: &Value) {
        let handover = scratch.join(QUEST).join("handover").join(from);
        fs::create_dir_all(&handover).expect("a handover");
        fs::write(
            handover.join(format!("{session}.cell.json")),
            serde_json::to_vec(said).expect("said"),
        )
        .expect("written");
    }

    fn harbour() -> Value {
        json!({
            "session": "s-harbour",
            "cell": "HARBOR",
            "ties": [{"key": "tie:HARBOR~KELP01", "kind": "tie", "heard": NOW - 60_000,
                      "strength": 2.0}],
            "talk": {"key": "tie:HARBOR", "kind": "tie", "heard": NOW - KEPT, "ties": 1},
        })
    }

    #[test]
    fn a_session_here_arrives_with_the_cell_another_machine_left_for_it() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        left(scratch.path(), "tern", "s-harbour", &harbour());
        left(
            scratch.path(),
            "skiff",
            "s-kelp",
            &json!({"session": "s-kelp", "cell": "KELP01"}),
        );
        let arrivals = Arrivals::default();
        let sessions = [
            session("s-harbour", "stand1", "skiff"),
            session("s-kelp", "kelp01", "skiff"),
            session("s-elsewhere", "far001", "tern"),
        ];
        assert!(arrivals.read(scratch.path(), "skiff.local", "skiff", &sessions, NOW));
        assert_eq!(
            arrivals.cells(),
            BTreeMap::from([("s-harbour".to_owned(), "HARBOR".to_owned())]),
            "only what another machine left, for a session running here"
        );
        assert!(
            !arrivals.read(scratch.path(), "skiff", "skiff", &sessions, NOW + 60_000),
            "read again unchanged, nothing changed"
        );
        fs::remove_dir_all(scratch.path().join(QUEST)).expect("taken away");
        assert!(!arrivals.read(scratch.path(), "skiff", "skiff", &sessions, NOW + KEPT - 1));
        assert_eq!(arrivals.cells().len(), 1, "kept once its file is gone");
        assert!(arrivals.read(scratch.path(), "skiff", "skiff", &sessions, NOW + KEPT));
        assert!(arrivals.cells().is_empty(), "for a day");
    }

    #[test]
    fn an_arrived_session_stands_as_its_cell_and_its_address_names_it() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        left(scratch.path(), "tern", "s-harbour", &harbour());
        let arrivals = Arrivals::default();
        let mut sessions = vec![session("s-harbour", "stand1", "skiff")];
        Arc::make_mut(&mut sessions[0].presence)["addresses"] = json!(["STAND2"]);
        arrivals.read(scratch.path(), "skiff", "skiff", &sessions, NOW);

        let mut aliases = HashMap::from([
            ("STAND1".to_owned(), "STAND1".to_owned()),
            ("STAND2".to_owned(), "STAND1".to_owned()),
            ("KELP01".to_owned(), "KELP01".to_owned()),
        ]);
        arrivals.alias(&sessions, &mut aliases);
        assert_eq!(
            aliases,
            HashMap::from([
                ("STAND1".to_owned(), "HARBOR".to_owned()),
                ("STAND2".to_owned(), "HARBOR".to_owned()),
                ("HARBOR".to_owned(), "HARBOR".to_owned()),
                ("KELP01".to_owned(), "KELP01".to_owned()),
            ]),
            "its address and every alias of it name its cell"
        );

        arrivals.named(&mut sessions);
        assert_eq!(sessions[0].address, "HARBOR");
        assert_eq!(
            sessions[0].presence["addresses"],
            json!(["STAND2", "stand1"]),
            "its own address kept among its others"
        );
    }

    #[test]
    fn the_ties_a_cell_brought_stand_until_a_day_past_or_heard_later_here() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        left(scratch.path(), "tern", "s-harbour", &harbour());
        let arrivals = Arrivals::default();
        let sessions = [session("s-harbour", "stand1", "skiff")];
        arrivals.read(scratch.path(), "skiff", "skiff", &sessions, NOW);
        let tie = |heard: i64| {
            daemon::Record::from(json!({"key": "tie:HARBOR~KELP01", "kind": "tie",
                                        "heard": heard, "strength": 0.5}))
        };

        let mut made = BTreeMap::new();
        arrivals.ties(&mut made, NOW);
        assert_eq!(
            made.keys().collect::<Vec<_>>(),
            ["tie:HARBOR~KELP01"],
            "its tie, and not its talk heard a day ago"
        );
        assert_eq!(made["tie:HARBOR~KELP01"].value()["strength"], json!(2.0));

        let mut made = BTreeMap::from([("tie:HARBOR~KELP01".to_owned(), tie(NOW))]);
        arrivals.ties(&mut made, NOW);
        assert_eq!(
            made["tie:HARBOR~KELP01"].value()["strength"],
            json!(0.5),
            "this node's, heard later, stands"
        );

        let mut made = BTreeMap::from([("tie:HARBOR~KELP01".to_owned(), tie(NOW - KEPT))]);
        arrivals.ties(&mut made, NOW);
        assert_eq!(
            made["tie:HARBOR~KELP01"].value()["strength"],
            json!(2.0),
            "the one brought, heard later, stands"
        );
    }

    #[test]
    fn a_session_or_quest_naming_a_path_is_never_read_for() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        left(scratch.path(), "tern", "s-harbour", &harbour());
        let arrivals = Arrivals::default();
        let mut wandering = session("../s-harbour", "stand1", "skiff");
        wandering.quest = Some(format!("{QUEST}/handover/tern/.."));
        let pathed = session("s-harbour", "stand1", "skiff");
        let mut pathed = pathed;
        pathed.quest = Some(format!("{QUEST}/."));
        assert!(!arrivals.read(scratch.path(), "skiff", "skiff", &[wandering, pathed], NOW));
    }
}
