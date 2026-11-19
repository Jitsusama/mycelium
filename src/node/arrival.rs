//! The cells carried sessions arrive with. A session carried here from
//! another machine runs under a new address, so without what its cell
//! brought it would stand as a cell nobody has heard of, its ties and
//! its name left behind. The machine it left puts its cell beside its
//! done file in the quest's handover ([`crate::carry::cell`]), and its
//! node asks this one, over their link, to start the session here. This
//! node notes each carry it so starts, reads the cell left for that
//! session in the folder the carry names, once, keeps it while the
//! session runs, and makes the session stand as its cell again: named
//! by it in the networks, its new address an alias of it, and the ties
//! it brought kept beside those this node makes, each for a day from
//! when it was heard, as long as a tie is told of.
//!
//! The handover is its sessions' to write, so a file there proves no
//! carry: only a session a linked node asked this one to start as a
//! carry is read for.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;

use crate::carry::cell::{self, Brought};
use crate::carry::through;
use crate::daemon;
use crate::record::{Session, State};

/// How long a tie a cell brought is kept from when it was heard, in
/// milliseconds: a day, as a tie is told of.
const KEPT: i64 = 24 * 60 * 60 * 1000;

/// How long a carry started here is waited on for its session to run
/// here, in milliseconds: an hour, well past any carry's arrival.
const WAITED: i64 = 60 * 60 * 1000;

/// How many carries started here are held at once, the earliest let go
/// first, so no linked node's starts grow what is held without bound.
const CARRIES_MOST: usize = 256;

/// A carry a linked node asked this one to start: the quest the session
/// was carried in, the folder of the quest's handover its machine left
/// it in, and when it was started, in milliseconds since the epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Carried {
    /// The quest it was carried in.
    pub(super) quest: String,
    /// The handover folder the machine it left wrote, as the carry names
    /// it.
    pub(super) from: String,
    /// When it was started.
    pub(super) at: i64,
}

/// The cells sessions running here arrived with, by session id, and the
/// carries started here they may arrive by.
#[derive(Debug, Default)]
pub(super) struct Arrivals {
    /// The cell each session arrived with.
    kept: Mutex<BTreeMap<String, Brought>>,
    /// The carry each session was started here by.
    carries: Mutex<BTreeMap<String, Carried>>,
}

impl Arrivals {
    /// Notes that a linked node asked this one to start the session
    /// `session` as `carry`, so its cell is read once it runs here.
    pub(super) fn carried(&self, session: &str, carry: Carried) {
        let mut carries = self.carries.lock().unwrap_or_else(PoisonError::into_inner);
        carries.insert(session.to_owned(), carry);
        while carries.len() > CARRIES_MOST {
            let earliest = carries
                .iter()
                .min_by_key(|(_, carry)| carry.at)
                .map(|(session, _)| session.clone());
            if let Some(earliest) = earliest {
                carries.remove(&earliest);
            }
        }
    }

    /// Reads, once, the cell each of `sessions` running on `machine`
    /// that a carry started here arrived with, in the folder of its
    /// quest's handover in `scratch` the carry named, and forgets each
    /// whose session no longer runs here. Each tie it brought said heard
    /// after `now` is kept as heard `now`. Answers whether what is kept
    /// changed.
    pub(super) fn read(
        &self,
        scratch: &Path,
        machine: &str,
        sessions: &[Session],
        now: i64,
    ) -> bool {
        let here: Vec<&Session> = sessions
            .iter()
            .filter(|session| session.machine == machine && session.state != State::Ended)
            .collect();
        let runs = |id: &str| here.iter().any(|session| session.id == id);
        let mut carries = self.carries.lock().unwrap_or_else(PoisonError::into_inner);
        carries.retain(|id, carry| runs(id) || now.saturating_sub(carry.at) < WAITED);
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let before = kept.clone();
        kept.retain(|id, _| runs(id));
        for session in &here {
            if kept.contains_key(&session.id) {
                continue;
            }
            let Some(carry) = carries.get(&session.id) else {
                continue;
            };
            if let Some(brought) = left_for(scratch, carry, &session.id) {
                kept.insert(session.id.clone(), settled(brought, now));
            }
        }
        drop(carries);
        // A cell that another live session goes by, as its address or
        // any other, or that two sessions here say, stands as none of
        // them, so no session takes the name, and the talk, of another
        // cell.
        let taken: Vec<String> = kept
            .iter()
            .filter(|(id, brought)| {
                let other_is = sessions.iter().any(|other| {
                    other.id != **id && other.state != State::Ended && goes_by(other, &brought.cell)
                });
                let twice = kept.iter().any(|(other, said)| {
                    other != *id && said.cell.eq_ignore_ascii_case(&brought.cell)
                });
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
        self.kept
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
    /// where this node holds none of it heard later.
    pub(super) fn ties(&self, made: &mut BTreeMap<String, daemon::Record>, now: i64) {
        let kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let brought = kept.values().flat_map(|brought| {
            let Brought { ties, talk, .. } = brought;
            ties.iter().chain(talk)
        });
        for record in brought {
            let (Some(key), Some(heard)) = (record["key"].as_str(), record["heard"].as_i64())
            else {
                continue;
            };
            if now.saturating_sub(heard) >= KEPT {
                continue;
            }
            let later = made.get(key).is_some_and(|held| held.heard() >= heard);
            if !later {
                made.insert(key.to_owned(), daemon::Record::from(record.clone()));
            }
        }
    }
}

/// `brought` with each tie and its talk said heard after `now`, when it
/// arrived, heard `now` instead, once, so no time a file says puts a
/// brought tie past this node's own for good.
fn settled(mut brought: Brought, now: i64) -> Brought {
    for record in brought.ties.iter_mut().chain(brought.talk.as_mut()) {
        if record["heard"].as_i64().is_some_and(|heard| heard > now) {
            record["heard"] = now.into();
        }
    }
    brought
}

/// Whether `session` goes by `name`, in any case: as its address, or as
/// any other address its presence says.
fn goes_by(session: &Session, name: &str) -> bool {
    session.address.eq_ignore_ascii_case(name)
        || session.presence["addresses"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .any(|address| address.eq_ignore_ascii_case(name))
}

/// The cell the session `id` arrived with by `carried`, as the file its
/// machine left in the handover says, or none: in the folder of the
/// machine the carry names, by its short name, as a leaving machine
/// names its folder. The handover is its sessions' to write, so no
/// folder of it is read through a link, nor any name that would leave
/// the folder it names.
fn left_for(scratch: &Path, carried: &Carried, id: &str) -> Option<Brought> {
    let machine = through::short_host(&carried.from);
    if ![carried.quest.as_str(), machine, id]
        .into_iter()
        .all(one_name)
    {
        return None;
    }
    let quest = scratch.join(&carried.quest);
    let handover = quest.join("handover");
    let from = handover.join(machine);
    if ![&quest, &handover, &from]
        .into_iter()
        .all(|path| folder(path))
    {
        return None;
    }
    cell::arrived(&from.join(format!("{id}{}", cell::FILE)), id)
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
    use std::collections::{BTreeMap, HashMap};
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;

    use serde_json::{Value, json};

    use super::{Arrivals, CARRIES_MOST, Carried, KEPT, WAITED};
    use crate::daemon;
    use crate::record::{Session, State};

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

    /// A carry of [`QUEST`] from `from`'s folder, started at [`NOW`].
    fn from(from: &str) -> Carried {
        Carried {
            quest: QUEST.to_owned(),
            from: from.to_owned(),
            at: NOW,
        }
    }

    /// Puts `said` as the cell `session` brought in `from`'s handover.
    fn left(scratch: &Path, from: &str, session: &str, said: &Value) {
        let handover = scratch.join(QUEST).join("handover").join(from);
        fs::create_dir_all(&handover).expect("a handover");
        let path = handover.join(format!("{session}.cell.json"));
        fs::write(&path, serde_json::to_vec(said).expect("said")).expect("written");
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
    fn only_a_session_a_carry_started_here_arrives_and_only_from_its_folder() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        left(scratch.path(), "tern", "s-harbour", &harbour());
        // Left as if carried, but no carry started it here.
        left(
            scratch.path(),
            "tern",
            "s-kelp",
            &json!({"session": "s-kelp", "cell": "KELP01"}),
        );
        // Carried from osprey, its file in tern's folder.
        left(
            scratch.path(),
            "tern",
            "s-astray",
            &json!({"session": "s-astray", "cell": "ASTRAY"}),
        );
        // Carried here, but running on tern.
        left(
            scratch.path(),
            "osprey",
            "s-elsewhere",
            &json!({"session": "s-elsewhere", "cell": "FARAWY"}),
        );
        let arrivals = Arrivals::default();
        // A Mac's machine leaves its folder by its short name.
        arrivals.carried("s-harbour", from("tern.local"));
        arrivals.carried("s-astray", from("osprey"));
        arrivals.carried("s-elsewhere", from("osprey"));
        let mut sessions = [
            session("s-harbour", "stand1", "skiff"),
            session("s-kelp", "kelp01", "skiff"),
            session("s-astray", "astray", "skiff"),
            session("s-elsewhere", "far001", "tern"),
        ];
        assert!(arrivals.read(scratch.path(), "skiff", &sessions, NOW));
        assert_eq!(
            arrivals.cells(),
            BTreeMap::from([("s-harbour".to_owned(), "HARBOR".to_owned())]),
            "only what the folder its carry named holds, for a session running here"
        );
        left(
            scratch.path(),
            "tern",
            "s-harbour",
            &json!({"session": "s-harbour", "cell": "LATER1"}),
        );
        assert!(
            !arrivals.read(scratch.path(), "skiff", &sessions, NOW),
            "its cell is read once, so a file written since changes nothing"
        );
        fs::remove_dir_all(scratch.path().join(QUEST)).expect("taken away");
        assert!(!arrivals.read(scratch.path(), "skiff", &sessions, NOW));
        assert_eq!(arrivals.cells().len(), 1, "kept once its file is gone");
        sessions[0].state = State::Ended;
        assert!(arrivals.read(scratch.path(), "skiff", &sessions, NOW));
        assert!(arrivals.cells().is_empty(), "while its session runs here");
    }

    #[test]
    fn a_carry_whose_session_never_runs_here_is_let_go() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        let arrivals = Arrivals::default();
        arrivals.carried("s-harbour", from("tern"));
        arrivals.read(scratch.path(), "skiff", &[], NOW + WAITED);
        left(scratch.path(), "tern", "s-harbour", &harbour());
        let sessions = [session("s-harbour", "stand1", "skiff")];
        assert!(
            !arrivals.read(scratch.path(), "skiff", &sessions, NOW + WAITED),
            "an hour on, its carry is forgotten"
        );

        let arrivals = Arrivals::default();
        for at in 0..=CARRIES_MOST {
            let mut carried = from("tern");
            carried.at = NOW + i64::try_from(at).expect("a small count");
            arrivals.carried(&format!("s-{at}"), carried);
        }
        let mut latest = from("tern");
        latest.at = NOW + 1_000;
        arrivals.carried("s-harbour", latest);
        assert!(
            arrivals.read(scratch.path(), "skiff", &sessions, NOW),
            "the earliest carry goes first once too many are held"
        );
        let carries = arrivals.carries.lock().expect("held");
        assert_eq!(carries.len(), CARRIES_MOST);
        assert!(!carries.contains_key("s-0") && !carries.contains_key("s-1"));
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
        left(
            scratch.path(),
            "tern",
            "s-alias",
            &json!({"session": "s-alias", "cell": "OTHER2"}),
        );
        let arrivals = Arrivals::default();
        for carried in ["s-harbour", "s-kelp", "s-twin", "s-alias"] {
            arrivals.carried(carried, from("tern"));
        }
        let mut other = session("s-other", "other1", "tern");
        Arc::make_mut(&mut other.presence)["addresses"] = json!(["other2"]);
        let mut sessions = [
            session("s-harbour", "stand1", "skiff"),
            session("s-kelp", "kelp01", "skiff"),
            session("s-twin", "twin01", "skiff"),
            session("s-alias", "alias1", "skiff"),
            session("s-real", "harbor", "tern"),
            other,
        ];
        arrivals.read(scratch.path(), "skiff", &sessions, NOW);
        assert!(
            arrivals.cells().is_empty(),
            "a live session's own name or another it goes by, in any case, and a name two \
             say, are taken by none"
        );
        sessions[4].state = State::Ended;
        assert!(arrivals.read(scratch.path(), "skiff", &sessions, NOW));
        assert_eq!(
            arrivals.cells(),
            BTreeMap::from([("s-harbour".to_owned(), "HARBOR".to_owned())]),
            "once the cell's own session has ended, it arrives"
        );
    }

    #[test]
    fn a_folder_reached_through_a_link_is_never_read() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        let elsewhere = tempfile::tempdir().expect("another folder");
        left(elsewhere.path(), "tern", "s-harbour", &harbour());
        let handover = scratch.path().join(QUEST).join("handover");
        fs::create_dir_all(&handover).expect("a handover");
        std::os::unix::fs::symlink(
            elsewhere.path().join(QUEST).join("handover").join("tern"),
            handover.join("tern"),
        )
        .expect("linked");
        let arrivals = Arrivals::default();
        arrivals.carried("s-harbour", from("tern"));
        let sessions = [session("s-harbour", "stand1", "skiff")];
        assert!(
            !arrivals.read(scratch.path(), "skiff", &sessions, NOW),
            "a machine's folder that is a link is never read through"
        );

        let linked = tempfile::tempdir().expect("a scratch folder");
        std::os::unix::fs::symlink(elsewhere.path().join(QUEST), linked.path().join(QUEST))
            .expect("linked");
        assert!(
            !arrivals.read(linked.path(), "skiff", &sessions, NOW),
            "nor a quest's folder that is a link"
        );
    }

    #[test]
    fn an_arrived_session_stands_as_its_cell_and_its_address_names_it() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        left(scratch.path(), "tern", "s-harbour", &harbour());
        let arrivals = Arrivals::default();
        arrivals.carried("s-harbour", from("tern"));
        let mut sessions = vec![session("s-harbour", "stand1", "skiff")];
        Arc::make_mut(&mut sessions[0].presence)["addresses"] = json!(["STAND2"]);
        arrivals.read(scratch.path(), "skiff", &sessions, NOW);

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
        arrivals.carried("s-harbour", from("tern"));
        let sessions = [session("s-harbour", "stand1", "skiff")];
        arrivals.read(scratch.path(), "skiff", &sessions, NOW);
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
    fn a_tie_said_heard_later_than_its_arrival_is_heard_then_and_ages_from_it() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        left(scratch.path(), "tern", "s-harbour", &harbour());
        let arrivals = Arrivals::default();
        arrivals.carried("s-harbour", from("tern"));
        let sessions = [session("s-harbour", "stand1", "skiff")];
        let arrived = NOW - 120_000;
        arrivals.read(scratch.path(), "skiff", &sessions, arrived);

        let mut made = BTreeMap::new();
        arrivals.ties(&mut made, NOW);
        assert_eq!(
            made["tie:HARBOR~KELP01"].heard(),
            arrived,
            "one said heard after it arrived counts as heard as it arrived"
        );
        let mut made = BTreeMap::from([(
            "tie:HARBOR~KELP01".to_owned(),
            daemon::Record::from(json!({"key": "tie:HARBOR~KELP01", "kind": "tie",
                                        "heard": NOW, "strength": 0.5})),
        )]);
        arrivals.ties(&mut made, NOW);
        assert_eq!(
            made["tie:HARBOR~KELP01"].value()["strength"],
            json!(0.5),
            "so this node's, heard since, stands"
        );
        let mut made = BTreeMap::new();
        arrivals.ties(&mut made, arrived + KEPT);
        assert!(made.is_empty(), "and it ages out a day after it arrived");
    }

    #[test]
    fn a_session_quest_or_folder_naming_a_path_is_never_read_for() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        left(scratch.path(), "tern", "s-harbour", &harbour());
        left(
            scratch.path(),
            "tern",
            "s-wander",
            &json!({"session": "../tern/s-wander", "cell": "WANDER"}),
        );
        fs::create_dir_all(scratch.path().join(QUEST).join("handover").join("osprey"))
            .expect("osprey's folder");
        let arrivals = Arrivals::default();
        arrivals.carried("../tern/s-wander", from("osprey"));
        arrivals.carried("s-harbour", from("osprey/../tern"));
        let mut pathed = from("tern");
        pathed.quest = format!("{QUEST}/handover/..");
        arrivals.carried("s-pathed", pathed);
        left(
            scratch.path(),
            "tern",
            "s-pathed",
            &json!({"session": "s-pathed", "cell": "PATHED"}),
        );
        let sessions = [
            session("../tern/s-wander", "stand1", "skiff"),
            session("s-harbour", "stand2", "skiff"),
            session("s-pathed", "stand3", "skiff"),
        ];
        assert!(!arrivals.read(scratch.path(), "skiff", &sessions, NOW));
    }
}
