//! The cells carried sessions arrive with. A session carried here from
//! another machine runs under a new address, so without what its cell
//! brought it would stand as a cell nobody has heard of, its ties and
//! its name left behind. The machine it left puts its cell beside its
//! done file in the quest's handover ([`crate::carry::cell`]); this node
//! reads it for each session running here, keeps it while the session
//! runs, and makes the session stand as its cell again: named by it in
//! the networks, its new address an alias of it, and the ties it
//! brought kept beside those this node makes, each for a day from when
//! it was heard, as long as a tie is told of.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use serde_json::Value;

use crate::carry::cell::{self, Brought};
use crate::carry::through;
use crate::daemon;
use crate::record::{Session, State};

/// How long a tie a cell brought is kept from when it was heard, in
/// milliseconds: a day, as a tie is told of.
const KEPT: i64 = 24 * 60 * 60 * 1000;

/// The cells sessions running here arrived with, by session id.
#[derive(Debug, Default)]
pub(super) struct Arrivals(Mutex<BTreeMap<String, Brought>>);

impl Arrivals {
    /// Reads the cell each of `sessions` running on `machine` with a
    /// quest arrived with, in its quest's handover in `scratch` from any
    /// of `machines`, by short name, but this one, `hostname`, and
    /// forgets each whose session no longer runs here. One whose file
    /// has gone is kept while its session runs. Answers whether what is
    /// kept changed.
    pub(super) fn read(
        &self,
        scratch: &Path,
        hostname: &str,
        machine: &str,
        machines: &BTreeSet<String>,
        sessions: &[Session],
    ) -> bool {
        let own = through::short_host(hostname);
        let from: Vec<&str> = machines
            .iter()
            .map(String::as_str)
            .filter(|one| *one != own && one_name(one))
            .collect();
        let here: Vec<&Session> = sessions
            .iter()
            .filter(|session| session.machine == machine && session.state != State::Ended)
            .collect();
        let mut kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let before = kept.clone();
        kept.retain(|id, _| here.iter().any(|session| session.id == *id));
        for session in &here {
            if let Some(brought) = left_for(scratch, &from, session) {
                kept.insert(session.id.clone(), brought);
            }
        }
        // A cell that is another live session's name, or that two
        // sessions here say, stands as none of them, so no session takes
        // the name, and the talk, of another cell.
        let taken: Vec<String> = kept
            .iter()
            .filter(|(id, brought)| {
                let other_is = sessions.iter().any(|other| {
                    other.id != **id
                        && other.state != State::Ended
                        && other.address.eq_ignore_ascii_case(&brought.cell)
                });
                let twice = kept
                    .iter()
                    .any(|(other, said)| other != *id && said.cell == brought.cell);
                other_is || twice
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &taken {
            kept.remove(id);
        }
        *kept != before
    }

    /// Each session's cell, by session id.
    fn cells(&self) -> BTreeMap<String, String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(session, brought)| (session.clone(), brought.cell.clone()))
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
    /// where this node holds none of it heard later. One said heard
    /// after `now` counts as heard `now`, so no time a file says puts a
    /// brought tie past this node's own for good.
    pub(super) fn ties(&self, made: &mut BTreeMap<String, daemon::Record>, now: i64) {
        let kept = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let brought = kept.values().flat_map(|brought| {
            let Brought { ties, talk, .. } = brought;
            ties.iter().chain(talk)
        });
        for record in brought {
            let (Some(key), Some(said)) = (record["key"].as_str(), record["heard"].as_i64()) else {
                continue;
            };
            let heard = said.min(now);
            if now.saturating_sub(heard) >= KEPT {
                continue;
            }
            let later = made.get(key).is_some_and(|held| held.heard() >= heard);
            if !later {
                let mut record = record.clone();
                record["heard"] = heard.into();
                made.insert(key.to_owned(), daemon::Record::from(record));
            }
        }
    }
}

/// The cell `session` arrived with, as the newest of its files in its
/// quest's handover from any of `machines` says, or none. The handover
/// is its sessions' to write, so no folder of it is read through a
/// link, and of two machines' files the one written last, the latest
/// carry's, is read.
fn left_for(scratch: &Path, machines: &[&str], session: &Session) -> Option<Brought> {
    let quest = session.quest.as_deref()?;
    if !one_name(quest) || !one_name(&session.id) {
        return None;
    }
    let handover = scratch.join(quest).join("handover");
    if !folder(&scratch.join(quest)) || !folder(&handover) {
        return None;
    }
    let file = format!("{}{}", session.id, cell::FILE);
    let mut left: Vec<(SystemTime, PathBuf)> = machines
        .iter()
        .map(|machine| handover.join(machine))
        .filter(|from| folder(from))
        .filter_map(|from| {
            let path = from.join(&file);
            let written = fs::symlink_metadata(&path).ok()?.modified().ok()?;
            Some((written, path))
        })
        .collect();
    // Newest first, and of two written at once by name, so which is
    // read never turns on the order the folder lists them in.
    left.sort_by(|one, other| other.0.cmp(&one.0).then_with(|| one.1.cmp(&other.1)));
    left.iter()
        .find_map(|(_, path)| cell::arrived(path, &session.id))
}

/// Whether `path` is a folder itself, never a link to one.
fn folder(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|met| met.is_dir())
}

/// Whether `name` names one entry of a folder, never a path.
fn one_name(name: &str) -> bool {
    Path::new(name).file_name() == Some(OsStr::new(name))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashMap};
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    use serde_json::{Value, json};

    use super::{Arrivals, KEPT};
    use crate::daemon;
    use crate::record::{Session, State};

    const QUEST: &str = "QEST-20261112-HARBOR";
    const NOW: i64 = 1_763_400_000_000;

    /// The realm's machines, by short name.
    fn realm() -> BTreeSet<String> {
        ["osprey", "skiff", "tern"].map(str::to_owned).into()
    }

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

    /// Puts `said` as the cell `session` brought from `from`'s handover,
    /// answering where.
    fn left(scratch: &Path, from: &str, session: &str, said: &Value) -> std::path::PathBuf {
        let handover = scratch.join(QUEST).join("handover").join(from);
        fs::create_dir_all(&handover).expect("a handover");
        let path = handover.join(format!("{session}.cell.json"));
        fs::write(&path, serde_json::to_vec(said).expect("said")).expect("written");
        path
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
        left(
            scratch.path(),
            "osprey",
            "s-elsewhere",
            &json!({"session": "s-elsewhere", "cell": "FARAWY"}),
        );
        left(
            scratch.path(),
            "kestrel",
            "s-outside",
            &json!({"session": "s-outside", "cell": "OUTSDE"}),
        );
        let arrivals = Arrivals::default();
        let mut sessions = [
            session("s-harbour", "stand1", "skiff"),
            session("s-kelp", "kelp01", "skiff"),
            session("s-elsewhere", "far001", "tern"),
            session("s-outside", "out001", "skiff"),
        ];
        assert!(arrivals.read(scratch.path(), "skiff.local", "skiff", &realm(), &sessions));
        assert_eq!(
            arrivals.cells(),
            BTreeMap::from([("s-harbour".to_owned(), "HARBOR".to_owned())]),
            "only what another of the realm's machines left, for a session running here"
        );
        assert!(
            !arrivals.read(scratch.path(), "skiff", "skiff", &realm(), &sessions),
            "read again unchanged, nothing changed"
        );
        fs::remove_dir_all(scratch.path().join(QUEST)).expect("taken away");
        assert!(!arrivals.read(scratch.path(), "skiff", "skiff", &realm(), &sessions));
        assert_eq!(arrivals.cells().len(), 1, "kept once its file is gone");
        sessions[0].state = State::Ended;
        assert!(arrivals.read(scratch.path(), "skiff", "skiff", &realm(), &sessions));
        assert!(arrivals.cells().is_empty(), "while its session runs here");
    }

    #[test]
    fn a_cell_another_live_session_goes_by_or_two_say_stands_as_none() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        left(scratch.path(), "tern", "s-harbour", &harbour());
        left(
            scratch.path(),
            "tern",
            "s-kelp",
            &json!({"session": "s-kelp", "cell": "REEF01"}),
        );
        left(
            scratch.path(),
            "tern",
            "s-twin",
            &json!({"session": "s-twin", "cell": "REEF01"}),
        );
        let arrivals = Arrivals::default();
        let mut sessions = [
            session("s-harbour", "stand1", "skiff"),
            session("s-kelp", "kelp01", "skiff"),
            session("s-twin", "twin01", "skiff"),
            session("s-real", "harbor", "tern"),
        ];
        arrivals.read(scratch.path(), "skiff", "skiff", &realm(), &sessions);
        assert!(
            arrivals.cells().is_empty(),
            "a live session's own name, in any case, and a name two say, are taken by none"
        );
        sessions[3].state = State::Ended;
        assert!(arrivals.read(scratch.path(), "skiff", "skiff", &realm(), &sessions));
        assert_eq!(
            arrivals.cells(),
            BTreeMap::from([("s-harbour".to_owned(), "HARBOR".to_owned())]),
            "once the cell's own session has ended, it arrives"
        );
    }

    #[test]
    fn the_newest_file_left_is_read_and_never_one_through_a_link() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        let older = left(
            scratch.path(),
            "osprey",
            "s-harbour",
            &json!({"session": "s-harbour", "cell": "OLDER1"}),
        );
        left(scratch.path(), "tern", "s-harbour", &harbour());
        let an_hour_ago = SystemTime::now() - Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(&older)
            .and_then(|file| file.set_modified(an_hour_ago))
            .expect("dated");
        let arrivals = Arrivals::default();
        let sessions = [session("s-harbour", "stand1", "skiff")];
        arrivals.read(scratch.path(), "skiff", "skiff", &realm(), &sessions);
        assert_eq!(
            arrivals.cells(),
            BTreeMap::from([("s-harbour".to_owned(), "HARBOR".to_owned())]),
            "the latest carry's"
        );

        let linked = tempfile::tempdir().expect("a scratch folder");
        let elsewhere = tempfile::tempdir().expect("another folder");
        left(elsewhere.path(), "tern", "s-harbour", &harbour());
        let handover = linked.path().join(QUEST).join("handover");
        fs::create_dir_all(&handover).expect("a handover");
        std::os::unix::fs::symlink(
            elsewhere.path().join(QUEST).join("handover").join("tern"),
            handover.join("tern"),
        )
        .expect("linked");
        let arrivals = Arrivals::default();
        assert!(
            !arrivals.read(linked.path(), "skiff", "skiff", &realm(), &sessions),
            "a machine's folder that is a link is never read through"
        );
    }

    #[test]
    fn an_arrived_session_stands_as_its_cell_and_its_address_names_it() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        left(scratch.path(), "tern", "s-harbour", &harbour());
        let arrivals = Arrivals::default();
        let mut sessions = vec![session("s-harbour", "stand1", "skiff")];
        Arc::make_mut(&mut sessions[0].presence)["addresses"] = json!(["STAND2"]);
        arrivals.read(scratch.path(), "skiff", "skiff", &realm(), &sessions);

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
        arrivals.read(scratch.path(), "skiff", "skiff", &realm(), &sessions);
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

        let mut made = BTreeMap::new();
        arrivals.ties(&mut made, NOW - 120_000);
        assert_eq!(
            made["tie:HARBOR~KELP01"].heard(),
            NOW - 120_000,
            "one said heard after now counts as heard now"
        );
    }

    #[test]
    fn a_session_or_quest_naming_a_path_is_never_read_for() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        left(scratch.path(), "tern", "s-harbour", &harbour());
        let arrivals = Arrivals::default();
        // A file where a session id naming a path would reach it, through
        // osprey's folder.
        left(
            scratch.path(),
            "tern",
            "s-wander",
            &json!({"session": "../tern/s-wander", "cell": "WANDER"}),
        );
        fs::create_dir_all(scratch.path().join(QUEST).join("handover").join("osprey"))
            .expect("osprey's folder");
        let wandering = session("../tern/s-wander", "stand1", "skiff");
        let mut pathed = session("s-harbour", "stand1", "skiff");
        pathed.quest = Some(format!("{QUEST}/."));
        let mut climbing = session("s-harbour", "stand1", "skiff");
        climbing.quest = Some(format!("{QUEST}/handover/tern/.."));
        assert!(!arrivals.read(
            scratch.path(),
            "skiff",
            "skiff",
            &realm(),
            &[wandering, pathed, climbing]
        ));
    }
}
