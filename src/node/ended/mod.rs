//! The sessions here that ended while their quest goes on, a record each,
//! so whoever orchestrates that quest can be told the work stopped.
//!
//! Seven sessions ended with their quests active on 16 Nov, one when pi
//! quit itself, and nothing told anyone: their orchestrator kept writing
//! to them, and the map drew them parked for hours. Only this node can
//! tell one has ended for good, since only it reads the park markers, the
//! boot, the carries it ordered and the turns of sessions coming back,
//! beside its own sessions, so it says so here, and the harness's mesh
//! side, which watches this kind, tells the orchestrator.
//!
//! A session counts once this node has seen it ended, unparked or parked
//! with nothing left to wake it, for [`SETTLED_AFTER`], so a park's marker
//! written a moment after its presence, or a holder back after a restart
//! and waiting its turn, is never told as an end. One carried from here
//! ended for the carry, and one waiting its turn is coming back, so
//! neither counts at all. It counts only while its quest, the one quest
//! the quests' record attaches it to, is active, names an orchestrator and
//! has no session running on it: work going on there goes on. It is kept a
//! day from when the node first saw it ended, and never once its presence
//! was last written over a day ago, so a node started again doesn't tell
//! last week's ends anew. The mesh side tells each end once, by its
//! session and `at`, however often it comes and goes here.
//!
//! The quests' record is a file any process of the person's account can
//! write, and a session's presence is its own word, so the record grants
//! nothing: it is a notice to an address, never an order, and its quest
//! and orchestrator come from the quests' record alone, the orchestrator
//! told only where it reads as a mesh address.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::{Value, json};

use crate::carry::kept::{Kept, State as CarryState};
use crate::record::{Session, State};

/// The kind of an ended session's records.
pub const KIND: &str = "ended";

/// The fields of an ended session's record that change while it lives:
/// none. Its quest and orchestrator move only as the quests' record does.
pub const LIVE: [&str; 0] = [];

/// How long a session that ended stays told, in milliseconds: a day, so a
/// session ended before a night's sleep is still told in the morning.
pub const KEPT_FOR: i64 = 24 * 60 * 60 * 1000;

/// How long a carry ordered from here stands for its session's end, in
/// milliseconds: an hour, past which a carry that runs nowhere, or a
/// session back from it that ended since, is work stopped. A session has
/// a quarter of an hour to leave for its carry, and its trees and quest
/// cross after, so half an hour would tell a slow carry as an end.
pub const CARRIED_WITHIN: i64 = 60 * 60 * 1000;

/// How long this node sees a session ended before it is told, in
/// milliseconds: ten minutes, past a park's marker following its presence
/// by moments and a machine's holders starting again after a reboot.
pub const SETTLED_AFTER: i64 = 10 * 60 * 1000;

/// The longest an orchestrator's address is told as, which a quest's six
/// letters fit.
const ADDRESS_MOST: usize = 16;

/// Why a session counts as ended: its process ended unparked, or it
/// parked with nothing left to wake it.
const ENDED: &str = "ended";
const PARKED_UNWAKEABLE: &str = "parked-unwakeable";

/// What this node has seen of sessions ending, kept while it runs: when
/// it first saw each ended.
#[derive(Debug, Default)]
pub struct Seen {
    since: HashMap<String, i64>,
}

/// How the node judges the sessions here: how each stands, and which are
/// coming back, carried from here or waiting a turn to resume.
pub struct Judged<'a> {
    /// The machine the node runs on.
    pub machine: &'a str,
    /// How a session stands, as [`super::standing::standing`] says.
    pub standing: &'a dyn Fn(&Session) -> Option<&'static str>,
    /// Sessions whose end here isn't their work stopping.
    pub coming_back: &'a HashSet<String>,
    /// How long an end is seen before it is told, in milliseconds:
    /// [`SETTLED_AFTER`], but where a test says.
    pub settled_after: i64,
    /// When a presence was last written, in milliseconds, where its time
    /// reads as one.
    pub written: &'a dyn Fn(&str) -> Option<i64>,
}

/// Each session of `sessions` here that `seen` has held ended for
/// `judged`'s settling and not past [`KEPT_FOR`] at `now`, written within
/// [`KEPT_FOR`] too, while its quest by `quests` is active, has an
/// orchestrator and nobody running on it: its id, when the node first saw
/// it ended, and its fields. A session elsewhere runs while its own node
/// says it's busy or idle, however long since it last wrote.
pub fn ended(
    sessions: &[Session],
    judged: &Judged,
    quests: &[Value],
    now: i64,
    seen: &mut Seen,
) -> Vec<(String, i64, Value)> {
    let running = |session: &Session| {
        matches!(session.state, State::Busy | State::Idle)
            && (session.machine != judged.machine || (judged.standing)(session) == Some("running"))
    };
    // Each quest any running session is attached to, though others
    // attach it too: work going on anywhere it may be is work going on.
    let worked: HashSet<&str> = sessions
        .iter()
        .filter(|session| running(session))
        .flat_map(|session| {
            quests
                .iter()
                .filter(|quest| attaches(quest, &session.id))
                .filter_map(id)
        })
        .collect();
    let ending: Vec<&Session> = sessions
        .iter()
        .filter(|session| session.machine == judged.machine && session.state == State::Ended)
        .filter(|session| (judged.standing)(session) != Some("parked"))
        .filter(|session| !judged.coming_back.contains(&session.id))
        .collect();
    // A session seen again, or gone from the record, ends afresh next time.
    let still: HashSet<&str> = ending.iter().map(|session| session.id.as_str()).collect();
    seen.since.retain(|id, _| still.contains(id.as_str()));
    let mut told = Vec::new();
    for session in ending {
        let record = quest_of(&session.id, quests);
        if record
            .and_then(id)
            .is_some_and(|quest| worked.contains(quest))
        {
            // Its end counts from when the work stops again.
            seen.since.remove(&session.id);
            continue;
        }
        let since = *seen.since.entry(session.id.clone()).or_insert(now);
        let Some(written) = (judged.written)(&session.at) else {
            continue;
        };
        // A presence written ahead of the clock is as untold as an old one.
        if now - since < judged.settled_after
            || now - since > KEPT_FOR
            || (now - written).abs() > KEPT_FOR
        {
            continue;
        }
        let (Some(record), Some(address)) = (record, address_told(&session.address)) else {
            continue;
        };
        let Some(quest) = id(record) else {
            continue;
        };
        if record.get("status").and_then(Value::as_str) != Some("active") {
            continue;
        }
        let Some(orchestrator) = record
            .get("orchestrator")
            .and_then(Value::as_str)
            .and_then(address_told)
        else {
            continue;
        };
        let parked = session.presence.get("state").and_then(Value::as_str) == Some("parked");
        let fields = json!({
            "session": session.id,
            "address": address,
            "quest": quest,
            "orchestrator": orchestrator,
            "at": session.at,
            "why": if parked { PARKED_UNWAKEABLE } else { ENDED },
        });
        told.push((session.id.clone(), since, fields));
    }
    told
}

/// The one quest whose record among `quests` attaches `session`; none
/// where none or several do, as then whose it is can't be told.
fn quest_of<'a>(session: &str, quests: &'a [Value]) -> Option<&'a Value> {
    let mut attaching = quests.iter().filter(|quest| attaches(quest, session));
    match (attaching.next(), attaching.next()) {
        (Some(quest), None) => Some(quest),
        _ => None,
    }
}

/// Whether `quest`'s record attaches the session `id`.
fn attaches(quest: &Value, id: &str) -> bool {
    quest
        .get("sessions")
        .and_then(Value::as_array)
        .is_some_and(|sessions| sessions.iter().any(|one| one.as_str() == Some(id)))
}

/// A quest record's id.
fn id(quest: &Value) -> Option<&str> {
    quest.get("id").and_then(Value::as_str)
}

/// `said` as a mesh address, in capitals as the node reads addresses:
/// letters and digits, at most [`ADDRESS_MOST`]; else nothing.
fn address_told(said: &str) -> Option<String> {
    let address = said.to_ascii_uppercase();
    let reads = !address.is_empty()
        && address.len() <= ADDRESS_MOST
        && address
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit());
    reads.then_some(address)
}

/// The sessions whose end here is a carry rather than their work
/// stopping: each of `carries` ordered within [`CARRIED_WITHIN`] of `now`
/// that `judge` says neither refused nor came back. Only those carries
/// are judged, as judging reads their handovers.
pub fn carried(carries: &[Kept], judge: impl Fn(&Kept) -> CarryState, now: i64) -> HashSet<String> {
    carries
        .iter()
        .filter(|carry| now - carry.at.as_millisecond() <= CARRIED_WITHIN)
        .filter(|carry| !matches!(judge(carry), CarryState::Refused(_) | CarryState::Back))
        .map(|carry| carry.said("session").to_owned())
        .collect()
}

/// The sessions whose end here is a carry or a turn rather than their
/// work stopping: those `carried`, and each of `waiting`.
pub fn coming_back<'a>(
    mut carried: HashSet<String>,
    waiting: impl IntoIterator<Item = &'a str>,
) -> HashSet<String> {
    carried.extend(waiting.into_iter().map(str::to_owned));
    carried
}

/// The records `ended` makes, by key.
pub fn records(ended: Vec<(String, i64, Value)>) -> BTreeMap<String, Value> {
    ended
        .into_iter()
        .map(|(id, heard, fields)| {
            let record = crate::daemon::record(KIND, &id, heard, fields);
            (crate::daemon::key(KIND, &id), record)
        })
        .collect()
}

#[cfg(test)]
mod tests;
