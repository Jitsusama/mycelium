//! How each session stands, as a launcher sorts them, and a digest of what
//! a shut launcher reads of it. A launcher had sorted every session itself
//! at each of the node's changes, about one a second, reading the park
//! markers and the boot beside them; the node says it once for every
//! reader instead, so a reader's idle cost is near nothing.

use std::collections::HashSet;
use std::fs;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::Path;

use serde_json::Value;

use crate::record::{Session, State};
use crate::store;

/// The boot this machine runs in: its id, where the system names one
/// (Linux), and when it began, in milliseconds since the epoch, where the
/// system says (Linux and macOS).
#[derive(Debug, Clone, Default)]
pub struct Boot {
    pub id: Option<String>,
    pub began: Option<i64>,
}

impl Boot {
    /// This machine's boot, read once, since a boot never changes while
    /// the node runs.
    pub fn this() -> Self {
        Self {
            id: store::folders::boot_id(),
            began: began(),
        }
    }
}

/// When this boot began, from the kernel's `btime` in `/proc/stat`, in
/// whole seconds, so a session heard in its first second counts as this
/// boot's.
#[cfg(target_os = "linux")]
fn began() -> Option<i64> {
    btime(&fs::read_to_string("/proc/stat").ok()?)?.checked_mul(1000)
}

/// When this boot began, from the kernel's `kern.boottime` as `sysctl`
/// says it, in whole seconds as on Linux. Without it a Mac counted no
/// ended session as this boot's, so none read as closed.
#[cfg(target_os = "macos")]
fn began() -> Option<i64> {
    let said = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "kern.boottime"])
        .output()
        .ok()?;
    boottime(&String::from_utf8_lossy(&said.stdout))?.checked_mul(1000)
}

/// Elsewhere the system says nothing this crate reads of when it booted.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn began() -> Option<i64> {
    None
}

/// The seconds since the epoch `/proc/stat`'s `btime` line gives.
#[cfg_attr(
    not(any(target_os = "linux", test)),
    expect(dead_code, reason = "read on Linux only, tested everywhere")
)]
fn btime(stat: &str) -> Option<i64> {
    stat.lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()
}

/// The seconds since the epoch `sysctl -n kern.boottime` gives, as
/// `{ sec = 1794333600, usec = 52016 } Tue Nov 10 18:00:00 2026`.
#[cfg_attr(
    not(any(target_os = "macos", test)),
    expect(dead_code, reason = "read on macOS only, tested everywhere")
)]
fn boottime(said: &str) -> Option<i64> {
    let (_, after) = said.split_once("sec =")?;
    after.split(',').next()?.trim().parse().ok()
}

/// The ids of the sessions parked here, by the markers the harness leaves
/// in `dir` as it parks one, each named by its session's id.
pub fn parked(dir: &Path) -> HashSet<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return HashSet::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| Some(path.file_stem()?.to_str()?.to_owned()))
        .filter(|id| !id.starts_with('.'))
        .collect()
}

/// How `session` stands: `running` while its process does, here or on a
/// machine heard of this boot; `parked` here, its process ended for its
/// holder to start again; `closed` here, ended this boot unparked; and
/// nothing for one passed over, ended elsewhere or before this boot.
/// Where the boot's start isn't known, nothing ended is closed, and a
/// session elsewhere runs however long ago it was heard.
pub fn standing(
    session: &Session,
    here: bool,
    parked: &HashSet<String>,
    boot: &Boot,
) -> Option<&'static str> {
    let this_boot = || {
        boot.began
            .is_some_and(|began| changed(session).is_some_and(|at| at >= began))
    };
    match session.state {
        State::Ended if here && parks(&session.presence, parked, boot) => Some("parked"),
        State::Ended if here && this_boot() => Some("closed"),
        State::Ended => None,
        _ if here || boot.began.is_none() || this_boot() => Some("running"),
        _ => None,
    }
}

/// When the session last changed, in milliseconds since the epoch, where
/// its record says.
fn changed(session: &Session) -> Option<i64> {
    session
        .at
        .parse::<jiff::Timestamp>()
        .ok()
        .map(jiff::Timestamp::as_millisecond)
}

/// Whether a presence is parked: it says so, its marker is there, and it
/// was parked this boot. A park lasts only while its holder does, and no
/// holder outlives the boot it began in, so one parked before has nothing
/// left to wake it, whatever its marker says. A presence naming no boot
/// is taken as this one's.
fn parks(presence: &Value, parked: &HashSet<String>, boot: &Boot) -> bool {
    presence.get("state").and_then(Value::as_str) == Some("parked")
        && presence
            .get("session")
            .and_then(Value::as_str)
            .is_some_and(|id| parked.contains(id))
        && presence
            .pointer("/owner/bootId")
            .and_then(Value::as_str)
            .is_none_or(|began| boot.id.as_deref() == Some(began))
}

/// A presence's fields that only an open launcher shows, and that change
/// as the session works: its step, what it has used, when it was last
/// heard, and whether it works or listens and since when. Of Joel's 113
/// sessions' changes in 30 s on 2026-11-09, 21 of 26 moved nothing else.
const SHOWN_ONLY: [&str; 8] = [
    "at",
    "busy",
    "heard",
    "since",
    "state",
    "step",
    "stepDetail",
    "usage",
];

/// A digest of a session record's `fields` without what only an open
/// launcher shows, so a change whose records carry no new digest tells a
/// shut launcher nothing it reads. The record's own `at`, `step` and
/// `since` move as its presence's do; its `state` tells only whether it
/// runs, since working and listening are shown alone, and `standing`
/// tells the rest. Said as hex, since a reader in JavaScript loses a
/// number's last digits past 2^53.
pub fn told(fields: &Value) -> String {
    let mut kept = fields.clone();
    if let Some(kept) = kept.as_object_mut() {
        for name in ["at", "step", "since"] {
            kept.remove(name);
        }
        if let Some(state) = kept.get_mut("state")
            && matches!(state.as_str(), Some("busy" | "idle"))
        {
            *state = Value::from("running");
        }
        if let Some(presence) = kept.get_mut("presence").and_then(Value::as_object_mut) {
            for name in SHOWN_ONLY {
                presence.remove(name);
            }
        }
    }
    // Its fields serialize in order of name, so the same record always
    // reads the same.
    let mut hasher = DefaultHasher::new();
    kept.to_string().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::{Value, json};

    use super::{Boot, boottime, btime, standing, told};
    use crate::record::{Session, State};

    /// The boot the cases run in, begun at 18:00 on 2026-11-10.
    fn boot() -> Boot {
        Boot {
            id: Some("boot-now".to_owned()),
            began: Some(1_794_333_600_000),
        }
    }

    /// A session standing as `state`, last heard `at`, with `presence`.
    fn session(state: State, at: &str, presence: Value) -> Session {
        Session {
            id: "s-one".to_owned(),
            address: "ONE001".to_owned(),
            machine: "optimus".to_owned(),
            title: None,
            working_on: None,
            groups: Vec::new(),
            state,
            step: None,
            since: None,
            orchestrated_by: None,
            quest: None,
            wants: None,
            landings: Vec::new(),
            window: None,
            held: None,
            attach: None,
            host: None,
            conversation: None,
            at: at.to_owned(),
            presence: presence.into(),
        }
    }

    #[test]
    fn a_boot_begins_when_linux_or_a_mac_says_it_did() {
        let stat = "cpu  10 0 5 100\nbtime 1794333600\nprocesses 42\n";
        assert_eq!(
            btime(stat),
            Some(1_794_333_600),
            "Linux says it in /proc/stat"
        );
        let said = "{ sec = 1794333600, usec = 52016 } Tue Nov 10 18:00:00 2026\n";
        assert_eq!(
            boottime(said),
            Some(1_794_333_600),
            "a Mac says it as sysctl's kern.boottime"
        );
        assert_eq!(
            (btime(""), boottime(""), boottime("{ sec = soon }")),
            (None, None, None),
            "a boot no system said the start of has none"
        );
    }

    const BEFORE: &str = "2026-11-10T17:00:00Z";
    const SINCE: &str = "2026-11-10T19:00:00Z";

    fn napping(boot: &str) -> Value {
        json!({"session": "s-one", "state": "parked", "owner": {"bootId": boot}})
    }

    #[test]
    fn a_running_session_runs_here_whenever_heard_and_elsewhere_only_this_boot() {
        let none = HashSet::new();
        for state in [State::Busy, State::Idle, State::Unreachable] {
            let old = session(state, BEFORE, json!({}));
            assert_eq!(
                standing(&old, true, &none, &boot()),
                Some("running"),
                "{state:?} here"
            );
            assert_eq!(
                standing(&old, false, &none, &boot()),
                None,
                "{state:?} long gone"
            );
            let new = session(state, SINCE, json!({}));
            assert_eq!(
                standing(&new, false, &none, &boot()),
                Some("running"),
                "{state:?} elsewhere this boot"
            );
        }
        let unknown = Boot {
            began: None,
            ..boot()
        };
        let old = session(State::Busy, BEFORE, json!({}));
        assert_eq!(
            standing(&old, false, &none, &unknown),
            Some("running"),
            "elsewhere, however long ago, where the boot's start isn't known"
        );
    }

    #[test]
    fn an_ended_session_is_parked_with_its_marker_this_boot_or_closed_this_boot() {
        let marked = HashSet::from(["s-one".to_owned()]);
        let none = HashSet::new();
        let parked = session(State::Ended, BEFORE, napping("boot-now"));
        assert_eq!(
            standing(&parked, true, &marked, &boot()),
            Some("parked"),
            "parked"
        );
        assert_eq!(
            standing(&parked, true, &none, &boot()),
            None,
            "a park with no marker, heard before this boot, is passed over"
        );
        let unmarked = session(State::Ended, SINCE, napping("boot-now"));
        assert_eq!(
            standing(&unmarked, true, &none, &boot()),
            Some("closed"),
            "with no marker it closed"
        );
        let earlier = session(State::Ended, SINCE, napping("boot-then"));
        assert_eq!(
            standing(&earlier, true, &marked, &boot()),
            Some("closed"),
            "a park of an earlier boot has nothing to wake it"
        );
        let unnamed = json!({"session": "s-one", "state": "parked"});
        let unnamed = session(State::Ended, BEFORE, unnamed);
        assert_eq!(
            standing(&unnamed, true, &marked, &boot()),
            Some("parked"),
            "a park naming no boot is this boot's"
        );
        let ended = session(State::Ended, SINCE, json!({}));
        assert_eq!(
            standing(&ended, false, &marked, &boot()),
            None,
            "one ended elsewhere is passed over"
        );
        let unknown = Boot {
            began: None,
            ..boot()
        };
        assert_eq!(
            standing(&ended, true, &none, &unknown),
            None,
            "none is closed where the boot's start isn't known"
        );
    }

    #[test]
    fn the_digest_moves_only_with_what_a_shut_launcher_reads() {
        let fields = |state: &str, step: &str, busy: bool, wants: Value| {
            json!({
                "id": "s-one", "state": state, "step": step, "at": step, "since": step,
                "standing": "running", "window": {"id": 4},
                "presence": {"busy": busy, "step": step, "at": step, "usage": {"turns": 2},
                    "wants": wants, "cwd": "~/src/harbour"},
            })
        };
        let first = told(&fields("busy", "read", true, json!([])));
        assert_eq!(
            told(&fields("idle", "edit", false, json!([]))),
            first,
            "working, listening and each step are shown alone"
        );
        assert_ne!(
            told(&fields("busy", "read", true, json!([{"what": "a look"}]))),
            first,
            "what it wants is read shut"
        );
        assert_ne!(
            told(&fields("ended", "read", true, json!([]))),
            first,
            "and whether it runs"
        );
        let mut parked = fields("busy", "read", true, json!([]));
        parked["standing"] = json!("parked");
        assert_ne!(told(&parked), first, "and how it stands");
        assert_eq!(told(&parked).len(), 16, "said as sixteen hex digits");
    }
}
