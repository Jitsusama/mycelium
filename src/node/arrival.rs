//! The cells carried sessions arrive with. A session carried here from
//! another machine runs under a new address, so without what its cell
//! brought it would stand as a cell nobody has heard of, its ties and
//! its name left behind. The machine it left puts its cell beside its
//! done file in the quest's handover ([`crate::carry::cell`]), and its
//! node asks this one, over their link, to start the session here. This
//! node notes each carry it so starts, reads the cell left for that
//! session in the folder the carry names, once, keeps it for a day
//! after the session last ran here, so one resumed later stands as it
//! again, and makes the session stand as its cell again: named
//! by it in the networks, its new address an alias of it, and the ties
//! it brought kept beside those this node makes, each for a day from
//! when it was heard, as long as a tie is told of. What is kept is held
//! in memory alone, so a node that restarts forgets it, and a session
//! resumed after stands under its own address.
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

use super::tie;
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

/// How many cells sessions arrived with are kept at once, those whose
/// sessions ran here longest ago let go first, as carries are, and
/// never one whose session runs here.
const KEPT_MOST: usize = CARRIES_MOST;

/// The strength a brought tie may have where this node holds none as
/// strong: a message's, heard as it arrived.
const STRONGEST_ALONE: f64 = 1.0;

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

/// A cell a session arrived with, and when its session last ran here,
/// in milliseconds since the epoch.
#[derive(Debug, Clone, PartialEq)]
struct Kept {
    /// The cell.
    brought: Brought,
    /// When its session last ran here.
    ran: i64,
    /// Whether its ties are held to [`bounded`]'s strength yet.
    bounded: bool,
}

/// A carry started here, waited on for its session to run here, and
/// what came of it so far.
#[derive(Debug)]
struct Waited {
    /// The carry.
    carry: Carried,
    /// Whether its session has run here.
    ran: bool,
    /// Whether a cell has stood for it.
    stood: bool,
}

/// The cells sessions that ran here arrived with, by session id, and
/// the carries started here they may arrive by.
#[derive(Debug, Default)]
pub(super) struct Arrivals {
    /// The cell each session arrived with.
    kept: Mutex<BTreeMap<String, Kept>>,
    /// The carry each session was started here by.
    carries: Mutex<BTreeMap<String, Waited>>,
}

impl Arrivals {
    /// Notes that a linked node asked this one to start the session
    /// `session` as `carry`, so its cell is read once it runs here, in
    /// place of any it arrived with before.
    pub(super) fn carried(&self, session: &str, carry: Carried) {
        self.kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(session);
        let mut carries = self.carries.lock().unwrap_or_else(PoisonError::into_inner);
        let waited = Waited {
            carry,
            ran: false,
            stood: false,
        };
        carries.insert(session.to_owned(), waited);
        while carries.len() > CARRIES_MOST {
            let earliest = carries
                .iter()
                .min_by_key(|(_, waited)| waited.carry.at)
                .map(|(session, _)| session.clone());
            if let Some(earliest) = earliest {
                carries.remove(&earliest);
            }
        }
    }

    /// Reads, once, the cell each of `sessions` running on `machine`
    /// that a carry started here arrived with, in the folder of its
    /// quest's handover in `scratch` the carry named, and forgets each
    /// whose session hasn't run here for a day. Each tie it brought said heard
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
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        carries.retain(|id, waited| {
            let running = runs(id);
            waited.ran |= running;
            let held = running || now.saturating_sub(waited.carry.at) < WAITED;
            // A leaving machine names its folder by its hostname, and
            // this node reads the one its link names, so where the two
            // differ a carry's session runs without its cell, and only
            // this says so. Quoted, since a linked node said them.
            if !held && waited.ran && !waited.stood {
                let Carried { from, quest, .. } = &waited.carry;
                eprintln!(
                    "session {id:?}, carried from {from:?} in {quest:?}, ran here as no cell: \
                     none stood in its handover's folder {:?} while it ran",
                    through::short_host(from)
                );
            }
            held
        });
        let before = cells_of(&kept);
        kept.retain(|id, one| runs(id) || now.saturating_sub(one.ran) < KEPT);
        for session in &here {
            if let Some(one) = kept.get_mut(&session.id) {
                one.ran = now;
                continue;
            }
            let Some(waited) = carries.get_mut(&session.id) else {
                continue;
            };
            if let Some(brought) = left_for(scratch, &waited.carry, &session.id) {
                waited.stood = true;
                let brought = settled(brought, now);
                let one = Kept {
                    brought,
                    ran: now,
                    bounded: false,
                };
                kept.insert(session.id.clone(), one);
            }
        }
        drop(carries);
        // Only a cell whose session no longer runs here is let go, so
        // none that runs is read again each minute; those running are
        // bounded by the starts a node takes.
        while kept.len() > KEPT_MOST {
            let earliest = kept
                .iter()
                .filter(|(id, _)| !runs(id))
                .min_by_key(|(_, one)| one.ran)
                .map(|(id, _)| id.clone());
            let Some(earliest) = earliest else {
                break;
            };
            kept.remove(&earliest);
        }
        // A cell that another live session goes by, as its address or
        // any other, or that two sessions running here say, stands as
        // none of them, so no session takes the name, and the talk, of
        // another cell; one whose session ended never takes a live
        // one's.
        let taken: Vec<String> = kept
            .iter()
            .map(|(id, one)| (id, &one.brought))
            .filter(|(id, brought)| {
                let other_is = sessions.iter().any(|other| {
                    other.id != **id && other.state != State::Ended && goes_by(other, &brought.cell)
                });
                let twice = kept.iter().any(|(other, said)| {
                    other != *id
                        && runs(other)
                        && said.brought.cell.eq_ignore_ascii_case(&brought.cell)
                });
                other_is || twice
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &taken {
            kept.remove(id);
        }
        cells_of(&kept) != before
    }

    /// How many carries started here are waited on.
    #[cfg(test)]
    pub(super) fn carrying(&self) -> usize {
        self.carries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Each session's cell, by session id.
    fn cells(&self) -> BTreeMap<String, String> {
        cells_of(&self.kept.lock().unwrap_or_else(PoisonError::into_inner))
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
    /// where this node holds none of it heard later. A cell's file is
    /// its session's to write, so the ties each cell brought are held
    /// to [`bounded`]'s strength, once, the first time they are weighed
    /// here: from then on they fade from when each was heard, as every
    /// tie held here does, and say the same strength each time.
    pub(super) fn ties(&self, made: &mut BTreeMap<String, daemon::Record>, now: i64) {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        if kept.is_empty() {
            return;
        }
        let most = strongest_held(made, now);
        for one in kept.values_mut().filter(|one| !one.bounded) {
            one.brought.ties = bounded(&one.brought.ties, most, now).collect();
            one.bounded = true;
        }
        let brought = kept.values().flat_map(|one| {
            let Brought { ties, talk, .. } = &one.brought;
            ties.iter().chain(talk.iter()).cloned()
        });
        for record in brought {
            let (Some(key), Some(heard)) = (record["key"].as_str(), record["heard"].as_i64())
            else {
                continue;
            };
            if now.saturating_sub(heard) >= KEPT {
                continue;
            }
            let key = key.to_owned();
            let later = made.get(&key).is_some_and(|held| held.heard() >= heard);
            if !later {
                made.insert(key, daemon::Record::from(record));
            }
        }
    }
}

/// Each session's cell in `kept`, by session id: what changes what the
/// networks say, as when a session last ran doesn't.
fn cells_of(kept: &BTreeMap<String, Kept>) -> BTreeMap<String, String> {
    kept.iter()
        .map(|(session, one)| (session.clone(), one.brought.cell.clone()))
        .collect()
}

/// What strength `tie`, saying `strength` heard at `heard`, has at
/// `now`, faded as every reader of ties fades it.
fn at_now(strength: f64, heard: i64, now: i64) -> f64 {
    strength * tie::faded(now.saturating_sub(heard).max(0))
}

/// The strength of the strongest tie between two cells in `made`, at
/// `now`, or [`STRONGEST_ALONE`] where that is stronger.
fn strongest_held(made: &BTreeMap<String, daemon::Record>, now: i64) -> f64 {
    made.iter()
        .filter(|(key, _)| key.contains('~'))
        .filter_map(|(_, record)| {
            cell::strength(&record.value()).map(|one| at_now(one, record.heard(), now))
        })
        .fold(STRONGEST_ALONE, f64::max)
}

/// `ties`, the ties a cell brought, each saying a strength no tie could
/// have saying none, and all made weaker alike where its strongest at
/// `now` is past `most`, so it is `most`: no file makes a brought tie
/// stronger, as read at any time, than any this node holds, and its
/// ties keep their order.
fn bounded(ties: &[Value], most: f64, now: i64) -> impl Iterator<Item = Value> + '_ {
    let strongest = ties
        .iter()
        .filter_map(|tie| {
            let heard = tie["heard"].as_i64()?;
            Some(at_now(cell::strength(tie)?, heard, now))
        })
        .fold(0.0, f64::max);
    let scale = if strongest > most {
        most / strongest
    } else {
        1.0
    };
    ties.iter().map(move |tie| {
        let mut tie = tie.clone();
        if tie.get("strength").is_some() {
            tie["strength"] = (cell::strength(&tie).unwrap_or_default() * scale).into();
        }
        tie
    })
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

    use super::{Arrivals, CARRIES_MOST, Carried, KEPT, KEPT_MOST, WAITED};
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
        let later = NOW + KEPT - 1;
        assert!(!arrivals.read(scratch.path(), "skiff", &sessions, later));
        assert_eq!(arrivals.cells().len(), 1, "kept for a day after it ended");
        sessions[0].state = State::Idle;
        assert!(!arrivals.read(scratch.path(), "skiff", &sessions, later));
        assert_eq!(
            arrivals.cells().len(),
            1,
            "resumed within the day, it stands as its cell again"
        );
        sessions[0].state = State::Ended;
        assert!(!arrivals.read(scratch.path(), "skiff", &sessions, later + KEPT - 1));
        assert!(arrivals.read(scratch.path(), "skiff", &sessions, later + KEPT));
        assert!(
            arrivals.cells().is_empty(),
            "let go a day after its session last ran here"
        );
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
    fn a_cell_kept_past_its_session_gives_way_to_a_live_one_and_a_new_carry() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        left(scratch.path(), "tern", "s-harbour", &harbour());
        left(
            scratch.path(),
            "tern",
            "s-twin",
            &json!({"session": "s-twin", "cell": "HARBOR"}),
        );
        let arrivals = Arrivals::default();
        arrivals.carried("s-harbour", from("tern"));
        let mut sessions = vec![session("s-harbour", "stand1", "skiff")];
        assert!(arrivals.read(scratch.path(), "skiff", &sessions, NOW));
        left(
            scratch.path(),
            "tern",
            "s-harbour",
            &json!({"session": "s-harbour", "cell": "LATER1"}),
        );
        arrivals.carried("s-harbour", from("tern"));
        assert!(arrivals.read(scratch.path(), "skiff", &sessions, NOW + 1));
        assert_eq!(
            arrivals.cells(),
            BTreeMap::from([("s-harbour".to_owned(), "LATER1".to_owned())]),
            "carried here again while its cell is kept, it arrives with the cell it brings now"
        );
        sessions[0].state = State::Ended;
        left(
            scratch.path(),
            "tern",
            "s-twin",
            &json!({"session": "s-twin", "cell": "LATER1"}),
        );
        arrivals.carried("s-twin", from("tern"));
        sessions.push(session("s-twin", "twin01", "skiff"));
        assert!(arrivals.read(scratch.path(), "skiff", &sessions, NOW + 2));
        assert_eq!(
            arrivals.cells(),
            BTreeMap::from([("s-twin".to_owned(), "LATER1".to_owned())]),
            "a cell kept for an ended session gives way to a live session's"
        );
    }

    #[test]
    fn past_as_many_as_are_kept_the_cell_whose_session_ran_longest_ago_goes() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        let arrivals = Arrivals::default();
        let mut sessions = Vec::new();
        for at in 0..=KEPT_MOST {
            let id = format!("s-{at}");
            left(
                scratch.path(),
                "tern",
                &id,
                &json!({"session": id, "cell": format!("C{at:05}")}),
            );
            let when = NOW + i64::try_from(at).expect("a small count");
            let mut carried = from("tern");
            carried.at = when;
            arrivals.carried(&id, carried);
            sessions.push(session(&id, &format!("a{at:05}"), "skiff"));
            arrivals.read(scratch.path(), "skiff", &sessions, when);
            // Each ends once it stands, but the first runs on.
            if at > 0 {
                sessions[at].state = State::Ended;
            }
        }
        let cells = arrivals.cells();
        assert_eq!(cells.len(), KEPT_MOST, "at most {KEPT_MOST}");
        assert!(
            cells.contains_key("s-0"),
            "one whose session runs is never let go, however long ago it was read"
        );
        assert!(
            !cells.contains_key("s-1"),
            "the one whose session ran here longest ago goes"
        );
    }

    #[test]
    fn past_as_many_as_are_kept_none_whose_session_runs_is_let_go() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        let arrivals = Arrivals::default();
        let mut sessions = Vec::new();
        for at in 0..=KEPT_MOST {
            let id = format!("s-{at}");
            left(
                scratch.path(),
                "tern",
                &id,
                &json!({"session": id, "cell": format!("C{at:05}")}),
            );
            // The first stand before the last are carried, so the
            // carries waited on, as many as are kept too, let go only
            // those already standing.
            let mut carried = from("tern");
            carried.at = if at < KEPT_MOST { NOW - 1 } else { NOW };
            arrivals.carried(&id, carried);
            sessions.push(session(&id, &format!("a{at:05}"), "skiff"));
            if at + 1 == KEPT_MOST {
                arrivals.read(scratch.path(), "skiff", &sessions, NOW - 1);
            }
        }
        // Every one runs, so a cell is let go only for want of one
        // whose session has stopped.
        arrivals.read(scratch.path(), "skiff", &sessions, NOW);
        assert_eq!(
            arrivals.cells().len(),
            KEPT_MOST + 1,
            "none whose session runs is let go, past {KEPT_MOST} too"
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
        // As every reader of ties reads them: faded to now.
        let brought_now = |made: &BTreeMap<String, daemon::Record>| {
            let record = &made["tie:HARBOR~KELP01"];
            let strength = record.value()["strength"].as_f64().expect("a strength");
            super::at_now(strength, record.heard(), NOW)
        };

        let mut made = BTreeMap::new();
        arrivals.ties(&mut made, NOW);
        assert_eq!(
            made.keys().collect::<Vec<_>>(),
            ["tie:HARBOR~KELP01"],
            "its tie, and not its talk heard a day ago"
        );
        assert!(
            (brought_now(&made) - 1.0).abs() < 1e-9,
            "as strong now as a message, since this node holds none stronger: {}",
            brought_now(&made)
        );

        let mut made = BTreeMap::from([("tie:HARBOR~KELP01".to_owned(), tie(NOW))]);
        arrivals.ties(&mut made, NOW);
        assert_eq!(
            made["tie:HARBOR~KELP01"].value()["strength"],
            json!(0.5),
            "this node's, heard later, stands"
        );

        let mut made = BTreeMap::from([("tie:HARBOR~KELP01".to_owned(), tie(NOW - KEPT))]);
        arrivals.ties(&mut made, NOW);
        assert!(
            (brought_now(&made) - 1.0).abs() < 1e-9,
            "the one brought, heard later, stands: {}",
            brought_now(&made)
        );
    }

    #[test]
    fn no_tie_a_cell_brought_is_stronger_than_the_strongest_held_here() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        let tie = |other: &str, strength: f64| {
            json!({"key": format!("tie:HARBOR~{other}"), "kind": "tie",
                   "heard": NOW - 60_000, "strength": strength})
        };
        let mut boasting = harbour();
        boasting["ties"] = json!([tie("KELP01", 1e9), tie("REEF01", 5e8), tie("TIDE01", -4.0)]);
        left(scratch.path(), "tern", "s-harbour", &boasting);
        let arrivals = Arrivals::default();
        arrivals.carried("s-harbour", from("tern"));
        let sessions = [session("s-harbour", "stand1", "skiff")];
        arrivals.read(scratch.path(), "skiff", &sessions, NOW);

        let held = json!({"key": "tie:OSPREY~TERN01", "kind": "tie", "heard": NOW,
                          "strength": 8.0});
        let mut made =
            BTreeMap::from([("tie:OSPREY~TERN01".to_owned(), daemon::Record::from(held))]);
        arrivals.ties(&mut made, NOW);
        // As every reader of ties reads them: faded to now.
        let at_now = |key: &str| {
            let record = &made[key];
            record.value()["strength"].as_f64().unwrap_or_default()
                * super::tie::faded(NOW - record.heard())
        };
        assert!(
            (at_now("tie:HARBOR~KELP01") - 8.0).abs() < 1e-9,
            "its strongest is as strong now as the strongest here: {}",
            at_now("tie:HARBOR~KELP01")
        );
        assert!(
            (at_now("tie:HARBOR~REEF01") - 4.0).abs() < 1e-9,
            "and the rest weaker alike, so they keep their order: {}",
            at_now("tie:HARBOR~REEF01")
        );
        assert_eq!(
            made["tie:HARBOR~TIDE01"].value().get("strength"),
            None,
            "one saying a strength no tie could have says none"
        );
    }

    #[test]
    fn no_tie_a_cell_brought_is_stronger_now_than_one_held_here_since_faded() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        let mut fresh = harbour();
        fresh["ties"] = json!([{"key": "tie:HARBOR~KELP01", "kind": "tie",
                                "heard": NOW, "strength": 8.0}]);
        left(scratch.path(), "tern", "s-harbour", &fresh);
        let arrivals = Arrivals::default();
        arrivals.carried("s-harbour", from("tern"));
        let sessions = [session("s-harbour", "stand1", "skiff")];
        arrivals.read(scratch.path(), "skiff", &sessions, NOW);

        // As strong as heard, but heard two half-lives ago, so a quarter
        // as strong now.
        let held = json!({"key": "tie:OSPREY~TERN01", "kind": "tie",
                          "heard": NOW - 2 * super::tie::HALF_LIFE, "strength": 8.0});
        let mut made =
            BTreeMap::from([("tie:OSPREY~TERN01".to_owned(), daemon::Record::from(held))]);
        arrivals.ties(&mut made, NOW);
        assert_eq!(
            made["tie:HARBOR~KELP01"].value()["strength"],
            json!(2.0),
            "weighed as it stands now, as the strongest here does"
        );

        // Weighed again two half-lives on, with nothing held stronger
        // than a message: bounded once, it says the same and so fades,
        // as a quarter of what it was.
        let later = NOW + 2 * super::tie::HALF_LIFE;
        let mut made = BTreeMap::new();
        arrivals.ties(&mut made, later);
        let record = &made["tie:HARBOR~KELP01"];
        assert_eq!(
            record.value()["strength"],
            json!(2.0),
            "bounded when first weighed, never again"
        );
        let faded = 2.0 * super::tie::faded(later - record.heard());
        assert!(
            (faded - 0.5).abs() < 1e-9,
            "so it fades as any tie held here: {faded}"
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
