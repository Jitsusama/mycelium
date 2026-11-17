//! The sessions here that ended while their quest goes on, a record each,
//! so whoever orchestrates that quest can be told the work stopped.
//!
//! Seven sessions ended with their quests active on 16 Nov, one when pi
//! quit itself, and nothing told anyone: their orchestrator kept writing
//! to them, and the map drew them parked for hours. Only this node can
//! tell one has ended for good, since only it reads the park markers and
//! the boot beside its own sessions, so it says so here, and the
//! harness's mesh side, which watches this kind, tells the orchestrator.
//! A record names who orchestrates the quest by the quests' record alone:
//! it grants nothing, so a session's own word about its orchestrator is
//! never asked.
//!
//! A session counts while it ended in the last day, ended unparked or
//! parked with nothing left to wake it, and no session of the realm runs
//! on its quest, which would have taken the work up again.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Value, json};

use crate::record::{Session, State};

/// The kind of an ended session's records.
pub const KIND: &str = "ended";

/// The fields of an ended session's record that change while it lives:
/// none, since another end makes another record.
pub const LIVE: [&str; 0] = [];

/// How long a session that ended stays told, in milliseconds: a day, so a
/// session ended before a night's sleep is still told in the morning.
pub const KEPT_FOR: i64 = 24 * 60 * 60 * 1000;

/// Why a session counts as ended: its process ended unparked, or it
/// parked with nothing left to wake it.
const ENDED: &str = "ended";
const PARKED_UNWAKEABLE: &str = "parked-unwakeable";

/// Each session of `sessions` on `machine` that ended within
/// [`KEPT_FOR`] of `now`, as `standing` judges it, while its quest is
/// active by `quests` and has an orchestrator, and no session runs on it:
/// its id, when it ended, and its fields. `at` reads a session's last
/// change in milliseconds since the epoch.
pub fn ended(
    sessions: &[Session],
    machine: &str,
    standing: &dyn Fn(&Session) -> Option<&'static str>,
    quests: &[Value],
    (now, at): (i64, &dyn Fn(&str) -> i64),
) -> Vec<(String, i64, Value)> {
    let worked: HashSet<&str> = sessions
        .iter()
        .filter(|session| !matches!(session.state, State::Ended))
        .filter_map(|session| quest_of(session, quests))
        .collect();
    sessions
        .iter()
        .filter(|session| session.machine == machine && session.state == State::Ended)
        .filter(|session| standing(session) != Some("parked"))
        .filter_map(|session| {
            let ended = at(&session.at);
            if now - ended > KEPT_FOR {
                return None;
            }
            let quest = quest_of(session, quests)?;
            if worked.contains(quest) {
                return None;
            }
            let record = quests.iter().find(|each| id(each) == Some(quest))?;
            if record.get("status").and_then(Value::as_str) != Some("active") {
                return None;
            }
            let orchestrator = record.get("orchestrator").and_then(Value::as_str)?;
            let parked = session.presence.get("state").and_then(Value::as_str) == Some("parked");
            let fields = json!({
                "session": session.id,
                "address": session.address,
                "quest": quest,
                "orchestrator": orchestrator.to_ascii_uppercase(),
                "at": session.at,
                "why": if parked { PARKED_UNWAKEABLE } else { ENDED },
            });
            Some((session.id.clone(), ended, fields))
        })
        .collect()
}

/// The quest `session` works on: the one its record says, or else the one
/// whose record among `quests` attaches it.
fn quest_of<'a>(session: &'a Session, quests: &'a [Value]) -> Option<&'a str> {
    session.quest.as_deref().or_else(|| {
        quests
            .iter()
            .find(|quest| attaches(quest, &session.id))
            .and_then(id)
    })
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
