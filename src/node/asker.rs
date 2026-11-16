//! Who asks the node to steer: whether the process at a client's end runs
//! as one of the machine's agents, which may say things to a session but
//! never as the person.
//!
//! Only the person's own login speaks as the person, but a steer names who
//! it is from itself, so any process of the person's account could claim
//! to be them. The kernel says which process is at a client's end; on
//! Linux its control group says which unit of the person's service
//! manager runs it, and the machine runs its agents in units of their own:
//! pi's hosts, each held session's scope, and the queues their commands
//! wait in. A process with pi among its forebears is an agent's too, as a
//! session started in a terminal rather than held is, and on a Mac, which
//! has no control groups, that is all there is to tell.
//!
//! This is a fence against habit and mistakes, not a boundary. Within one
//! account every check a process could make of another is forged by one
//! set on it: a command may move itself into a unit of its own, as
//! `systemd-run --user --scope` does, or fork twice so pi is no longer
//! among its forebears, or write a session's inbox itself. What keeps an
//! agent from the person's voice is what it can reach: commands fenced
//! away from the node's socket and the inboxes.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::steer::claims_the_person;

/// How the names of the units the machine runs its agents in begin, below
/// the person's service manager's own unit: pi's hosts, the scope each
/// held session runs in, and the queues agents' commands wait their turn
/// in, here or for a builder elsewhere. The rest of `agents.slice` is the
/// person's as much as theirs: their own terminals, the node and its
/// links run there.
const AGENTS_UNITS: [&str; 4] = [
    "agents.slice/pi-host-",
    "agents.slice/mycelium-pi-",
    "jobs.slice/",
    "offload.slice/",
];

/// The name pi's own process goes by.
const PI: &str = "pi";

/// How many forebears are looked through at most, so a chain the kernel
/// tells wrong can't hold a steer up for long.
const FOREBEARS: usize = 64;

/// Where a machine tells who runs each of its processes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Processes {
    /// Linux's `/proc`, or a stand-in for it: each process's control
    /// group, name and parent.
    Folder(PathBuf),
    /// What `ps` lists, as on a Mac, which has no `/proc`: each process's
    /// name and parent.
    Listed,
}

impl Processes {
    /// This machine's own.
    #[must_use]
    pub fn here() -> Self {
        if cfg!(target_os = "linux") {
            Self::Folder("/proc".into())
        } else {
            Self::Listed
        }
    }

    /// Process `pid` and each of its forebears in turn, as far as can be
    /// read, nearest first.
    fn lineage(&self, pid: i32) -> Vec<i32> {
        let walk = |process: &dyn Fn(i32) -> Option<i32>| {
            let mut lineage = vec![pid];
            let mut at = pid;
            while lineage.len() < FOREBEARS {
                match process(at) {
                    Some(parent) if parent > 1 => {
                        lineage.push(parent);
                        at = parent;
                    }
                    _ => break,
                }
            }
            lineage
        };
        match self {
            Self::Folder(procs) => walk(&|process| {
                let status =
                    fs::read_to_string(procs.join(process.to_string()).join("status")).ok()?;
                named(&status).map(|(_, parent)| parent)
            }),
            Self::Listed => {
                let listed = Command::new("/bin/ps")
                    .args(["-axo", "pid=,ppid=,comm="])
                    .output()
                    .ok()
                    .filter(|ran| ran.status.success())
                    .map(|ran| listing(&String::from_utf8_lossy(&ran.stdout)))
                    .unwrap_or_default();
                walk(&|process| listed.get(&process).map(|(_, parent)| *parent))
            }
        }
    }

    /// The session process `pid` says it runs for, as the harness tells
    /// each command it runs in its environment; a Mac's `ps` won't say.
    fn named_session(&self, pid: i32) -> Option<String> {
        let Self::Folder(procs) = self else {
            return None;
        };
        let environ = fs::read(procs.join(pid.to_string()).join("environ")).ok()?;
        environ.split(|byte| *byte == 0).find_map(|pair| {
            let value = pair.strip_prefix(b"PI_SESSION_ID=")?;
            String::from_utf8(value.to_vec()).ok()
        })
    }

    /// What is told of process `pid`.
    fn tell(&self, pid: i32) -> Told {
        match self {
            Self::Folder(procs) => {
                let cgroup = fs::read_to_string(procs.join(pid.to_string()).join("cgroup")).ok();
                if let Some(unit) = cgroup.as_deref().and_then(agents_unit) {
                    return Told::Agent(unit);
                }
                forebears(pid, |process| {
                    let status =
                        fs::read_to_string(procs.join(process.to_string()).join("status")).ok()?;
                    named(&status).map(|(name, parent)| (name.to_owned(), parent))
                })
            }
            Self::Listed => {
                let Some(listed) = Command::new("/bin/ps")
                    .args(["-axo", "pid=,ppid=,comm="])
                    .output()
                    .ok()
                    .filter(|ran| ran.status.success())
                else {
                    return Told::Untold;
                };
                let listed = listing(&String::from_utf8_lossy(&listed.stdout));
                forebears(pid, |process| listed.get(&process).cloned())
            }
        }
    }
}

/// Who owns a session, as its presence claims it, which the session
/// writes itself and so proves nothing until the kernel and its host
/// back it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The session's own id.
    pub session: String,
    /// The process it says owns it.
    pub pid: Option<i32>,
    /// When it says that process started, in the kernel's clock ticks
    /// since boot, which tells one process from a later one given its id.
    pub start_ticks: Option<u64>,
    /// The pi host it says holds it, by its key, when one does.
    pub host: Option<String>,
}

impl Claim {
    /// The claim `presence`, filed under the session id `filed`, makes, if
    /// it names its session. Pi files each presence under its own
    /// session's id, so one filed under another names no owner: it is
    /// forged, though it still counts toward a session claimed twice.
    #[must_use]
    pub fn of(filed: &str, presence: &Value) -> Option<Self> {
        let session = presence.get("session")?.as_str()?.to_owned();
        let owner = presence.get("owner").filter(|_| session == filed);
        let field = |key: &str| owner.and_then(|owner| owner.get(key));
        Some(Self {
            session,
            pid: field("pid")
                .and_then(Value::as_i64)
                .and_then(|pid| i32::try_from(pid).ok()),
            start_ticks: field("startTicks").and_then(Value::as_u64),
            host: field("host").and_then(Value::as_str).map(str::to_owned),
        })
    }
}

/// The owners `claims` prove, each session and its process, from records
/// no session writes. A claim counts only where its process started when
/// it says, as the kernel tells it, so a later process given the same id
/// isn't taken for it. One a pi host holds counts only where the host's
/// own record, in `hosts`, lists the session; one held by no host only
/// where its process is pi's own and no other session claims it. Either
/// way, its process must hold the session's log in `logs` open, as pi
/// does for each session it owns. Refused
/// outright when two claims name one session, as one forged to borrow
/// another's name would, or where the kernel can't say when a process
/// started, as on a Mac.
///
/// Like [`session_of`], this fences habit: a session's own process may
/// write another's presence, with its process's start, and open its log,
/// while that one isn't running.
///
/// # Errors
///
/// When a session is claimed twice, or no claim can be proved here.
pub fn proved(
    claims: &[Claim],
    processes: &Processes,
    hosts: &Path,
    logs: &Path,
) -> Result<Vec<(String, i32)>, String> {
    let Processes::Folder(procs) = processes else {
        return Err(
            "this machine can't say when a process started, so who owns a session can't be \
             proved and no carry is ordered"
                .to_owned(),
        );
    };
    let mut seen = std::collections::BTreeSet::new();
    if let Some(twice) = claims
        .iter()
        .find(|claim| !seen.insert(claim.session.as_str()))
    {
        return Err(format!(
            "two presences here claim the session {}, so who owns it can't be told and no \
             carry is ordered",
            twice.session
        ));
    }
    let logs = fs::canonicalize(logs).unwrap_or_else(|_| logs.to_owned());
    let unhosted = |pid: i32| {
        claims
            .iter()
            .filter(|claim| claim.host.is_none() && claim.pid == Some(pid))
            .count()
    };
    Ok(claims
        .iter()
        .filter_map(|claim| {
            let (pid, ticks) = (claim.pid?, claim.start_ticks?);
            if started(procs, pid)? != ticks {
                return None;
            }
            let held = match &claim.host {
                Some(host) => hosts_session(hosts, host, pid, ticks, &claim.session),
                None => named_pi(procs, pid) && unhosted(pid) == 1,
            };
            (held && holds_log(procs, pid, &logs, &claim.session))
                .then(|| (claim.session.clone(), pid))
        })
        .collect())
}

/// Whether process `pid` in `procs` holds open the log pi keeps of
/// `session` in `logs`, `<folder>/<time>_<session>.jsonl`, as the kernel
/// gives each open file's real path in its `fd` folder. Pi holds its
/// session's log open as it owns it, and a host each it hosts, so this
/// says the process means to own it, which a helper writing another's
/// presence by habit doesn't.
fn holds_log(procs: &Path, pid: i32, logs: &Path, session: &str) -> bool {
    let Ok(open) = fs::read_dir(procs.join(pid.to_string()).join("fd")) else {
        return false;
    };
    let named = format!("_{session}.jsonl");
    open.flatten()
        .filter_map(|file| fs::canonicalize(file.path()).ok())
        .any(|log| {
            log.parent().and_then(Path::parent) == Some(logs)
                && log
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(&named))
        })
}

/// When process `pid` started, in clock ticks since boot, as its `stat`
/// in `procs` tells it: the twentieth field after its name, which is in
/// brackets and may hold spaces or brackets itself.
fn started(procs: &Path, pid: i32) -> Option<u64> {
    let stat = fs::read_to_string(procs.join(pid.to_string()).join("stat")).ok()?;
    let (_, after) = stat.rsplit_once(')')?;
    after.split_whitespace().nth(19)?.parse().ok()
}

/// Whether process `pid` in `procs` is pi's own, by the name the kernel
/// gives it.
fn named_pi(procs: &Path, pid: i32) -> bool {
    fs::read_to_string(procs.join(pid.to_string()).join("status"))
        .is_ok_and(|status| named(&status).is_some_and(|(name, _)| name == PI))
}

/// Whether the pi host keyed `key`, at process `pid` started at `ticks`,
/// lists `session` among its own in its record in `hosts`: the file the
/// host keeps as `<pid>-<ticks>.json`, which pi itself trusts to tell
/// which sessions a host holds.
fn hosts_session(hosts: &Path, key: &str, pid: i32, ticks: u64, session: &str) -> bool {
    let Some(record) = fs::read_to_string(hosts.join(format!("{pid}-{ticks}.json")))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    else {
        return false;
    };
    record.get("key").and_then(Value::as_str) == Some(key)
        && record.pointer("/owner/pid").and_then(Value::as_i64) == Some(i64::from(pid))
        && record.pointer("/owner/startTicks").and_then(Value::as_u64) == Some(ticks)
        && record
            .get("sessions")
            .and_then(Value::as_array)
            .is_some_and(|sessions| {
                sessions
                    .iter()
                    .any(|one| one.get("id").and_then(Value::as_str) == Some(session))
            })
}

/// What a machine tells of the process that asks.
#[derive(Debug, PartialEq, Eq)]
enum Told {
    /// It runs as an agent, in this unit or below this pi.
    Agent(String),
    /// It is the person's own.
    Person,
    /// Nothing could be read of it, or of a forebear.
    Untold,
}

/// Whether `said` may be put to a session when process `asker` asks, as
/// `processes` tell it, answering whether its asker was seen to be the
/// person's own: what claims to be from the person is refused from an
/// agent, or from an asker the kernel names no process for, saying why,
/// and anything else goes. An asker whose process can't be read is taken
/// at its word here, but not seen, so what it says as the person crosses
/// no link.
pub fn may_say(asker: Option<i32>, processes: &Processes, said: &Value) -> Result<bool, String> {
    if !claims_the_person(said) {
        return Ok(false);
    }
    let Some(pid) = asker else {
        return Err(
            "nothing names the process that asks, so it can't speak as the person: \
                    only the person's own login says what they say"
                .to_owned(),
        );
    };
    match processes.tell(pid) {
        Told::Agent(unit) => Err(format!(
            "this asks from {unit}, an agent's, and an agent can't speak as the person: \
             only the person's own login says what they say"
        )),
        Told::Person => Ok(true),
        Told::Untold => Ok(false),
    }
}

/// Whether process `asker` is the person's own login, as `processes` tell
/// it, as only the person frees a quest an orchestrator holds.
///
/// # Errors
///
/// When the asker is an agent's, or nothing can be told of it.
pub fn persons_own(asker: Option<i32>, processes: &Processes) -> Result<(), String> {
    let only = "so it can't free a quest: only the person's own login does";
    let Some(pid) = asker else {
        return Err(format!("nothing names the process that asks, {only}"));
    };
    match processes.tell(pid) {
        Told::Person => Ok(()),
        Told::Agent(unit) => Err(format!("this asks from {unit}, an agent's, {only}")),
        Told::Untold => Err(format!(
            "nothing could be read of the process that asks, {only}"
        )),
    }
}

/// The session that asks, by its own id, among `owners`, each session of
/// this machine's that runs and the process that owns it, as [`proved`]
/// proves them: the one whose process the asker is, or runs below. A pi host owns
/// many sessions in one process, so among those the asker names its own,
/// as the harness tells each command it runs (`PI_SESSION_ID`); it can
/// name only one its own process's forebear owns. Refused, saying why,
/// when nothing names the asker, it runs below no session, or it can't be
/// told which.
///
/// Like [`may_say`], this fences habit, not an agent set on it: a host's
/// command may name another session of its host.
///
/// # Errors
///
/// When no one session can be told to ask.
///
/// See [`claimed_above`] for the sessions that may limit what the asker
/// does, which needs no proof.
pub fn session_of(
    asker: Option<i32>,
    processes: &Processes,
    owners: &[(String, i32)],
) -> Result<String, String> {
    let Some(pid) = asker else {
        return Err(
            "nothing names the process that asks, so no session can be told to ask".to_owned(),
        );
    };
    let owning = processes.lineage(pid).into_iter().find_map(|process| {
        let sessions: Vec<&str> = owners
            .iter()
            .filter(|(_, owner)| *owner == process)
            .map(|(session, _)| session.as_str())
            .collect();
        (!sessions.is_empty()).then_some(sessions)
    });
    let Some(sessions) = owning else {
        return Err("this asks from no session this node knows, so it orders no carry".to_owned());
    };
    if let [one] = sessions.as_slice() {
        return Ok((*one).to_owned());
    }
    match processes.named_session(pid) {
        Some(named) if sessions.contains(&named.as_str()) => Ok(named),
        _ => Err(format!(
            "this asks from a process {} sessions share, and doesn't name which of them it is",
            sessions.len()
        )),
    }
}

/// What `process`, a lookup of a process's name and parent, tells of `pid`
/// by its forebears: an agent's below a process named pi, the person's
/// once the chain ends without one.
fn forebears(pid: i32, process: impl Fn(i32) -> Option<(String, i32)>) -> Told {
    let mut at = pid;
    for _ in 0..FOREBEARS {
        let Some((name, parent)) = process(at) else {
            return Told::Untold;
        };
        if name == PI {
            return Told::Agent(format!("pi's process {at}"));
        }
        if parent <= 1 {
            return Told::Person;
        }
        at = parent;
    }
    Told::Untold
}

/// The agent's unit a process runs in, as its `cgroup` file tells it, by
/// the unit's own name, or none for any other.
fn agents_unit(cgroup: &str) -> Option<String> {
    cgroup.lines().find_map(|line| {
        // Control groups v2 tell one line, its hierarchy 0 and no
        // controllers named: `0::<path>`.
        let path = line.strip_prefix("0::")?;
        let (_, below) = path.split_once(".service/").filter(|(manager, _)| {
            manager
                .rsplit('/')
                .next()
                .is_some_and(|unit| unit.starts_with("user@"))
        })?;
        AGENTS_UNITS
            .iter()
            .any(|unit| below.starts_with(unit))
            .then(|| below.rsplit('/').next().unwrap_or(below).to_owned())
    })
}

/// A process's name and its parent's id, as its `status` file tells them.
fn named(status: &str) -> Option<(&str, i32)> {
    let field = |key: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .map(str::trim)
    };
    Some((field("Name:")?, field("PPid:")?.parse().ok()?))
}

/// Each process's name and parent, by its id, as `ps -axo
/// pid=,ppid=,comm=` lists them: a Mac's `comm` is the program's whole
/// path, spaces and all, so its name is what follows the last slash.
fn listing(listed: &str) -> HashMap<i32, (String, i32)> {
    listed
        .lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (parent, command) = rest.trim_start().split_once(char::is_whitespace)?;
            let command = command.trim();
            let name = command.rsplit('/').next().unwrap_or(command);
            Some((pid.parse().ok()?, (name.to_owned(), parent.parse().ok()?)))
        })
        .collect()
}

/// The sessions of `claims` whose process is the asker or one of its
/// forebears, nearest first. Nothing is proved: a claim here can only
/// limit what the asker may do, never widen it, so one forged or stale
/// costs the asker, not the machine.
#[must_use]
pub fn claimed_above(asker: Option<i32>, processes: &Processes, claims: &[Claim]) -> Vec<String> {
    let Some(pid) = asker else {
        return Vec::new();
    };
    processes
        .lineage(pid)
        .into_iter()
        .flat_map(|process| {
            claims
                .iter()
                .filter(move |claim| claim.pid == Some(process))
                .map(|claim| claim.session.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;

    /// A control group under the person's service manager, as the kernel
    /// tells it.
    fn under_manager(below: &str) -> String {
        format!("0::/user.slice/user-1000.slice/user@1000.service/{below}\n")
    }

    #[test]
    fn an_agents_unit_is_told_by_where_the_machine_runs_agents() {
        for (below, unit) in [
            (
                "agents.slice/pi-host-work-background.service",
                "pi-host-work-background.service",
            ),
            (
                "agents.slice/mycelium-pi-f3d9.scope",
                "mycelium-pi-f3d9.scope",
            ),
            (
                "jobs.slice/jobs-work.slice/jobs-work-unranked.slice/work-12-3.scope",
                "work-12-3.scope",
            ),
            ("offload.slice/run-p40-i41.scope", "run-p40-i41.scope"),
        ] {
            assert_eq!(
                agents_unit(&under_manager(below)).as_deref(),
                Some(unit),
                "{below} is an agent's"
            );
        }
    }

    #[test]
    fn the_persons_own_units_are_no_agents() {
        for below in [
            "session.slice/quickshell.service",
            "background.slice/agents.service",
            "agents.slice/kitty-2830-4.scope",
            "app.slice/app-niri-slack-100507.scope",
        ] {
            assert_eq!(
                agents_unit(&under_manager(below)),
                None,
                "{below} is the person's"
            );
        }
        assert_eq!(
            agents_unit("0::/system.slice/jobs.slice/sshd.service\n"),
            None,
            "a unit outside the person's service manager is no agent's"
        );
    }

    /// A stand-in for `/proc` telling each of `processes`, by its id, its
    /// name, its parent and the control group it runs in.
    fn procs(processes: &[(i32, &str, i32, &str)]) -> tempfile::TempDir {
        let procs = tempfile::tempdir().expect("a stand-in for /proc");
        for (pid, name, parent, below) in processes {
            let process = procs.path().join(pid.to_string());
            fs::create_dir(&process).expect("its folder is made");
            fs::write(
                process.join("status"),
                format!("Name:\t{name}\nUmask:\t0022\nState:\tS (sleeping)\nPPid:\t{parent}\n"),
            )
            .expect("its status is written");
            fs::write(process.join("cgroup"), under_manager(below)).expect("its group is written");
        }
        procs
    }

    /// The stand-in for `/proc` at `procs`, as the node reads it.
    fn folder(procs: &Path) -> Processes {
        Processes::Folder(procs.to_owned())
    }

    #[test]
    fn an_agent_may_say_anything_but_as_the_person() {
        let procs = procs(&[
            (
                40,
                "mycelium",
                2,
                "agents.slice/pi-host-work-attended.service",
            ),
            (41, "quickshell", 1, "session.slice/quickshell.service"),
        ]);
        let pause = json!({"act": "pause", "from": "person"});
        assert_eq!(
            may_say(Some(40), &folder(procs.path()), &pause),
            Err(
                "this asks from pi-host-work-attended.service, an agent's, and an agent can't \
                 speak as the person: only the person's own login says what they say"
                    .to_owned()
            ),
            "an agent claiming the person is refused, saying why"
        );
        assert_eq!(
            may_say(
                Some(40),
                &folder(procs.path()),
                &json!({"text": "your build is done", "from": "P9R1KI"})
            ),
            Ok(false),
            "an agent saying something as itself is let through"
        );
        assert_eq!(
            may_say(Some(41), &folder(procs.path()), &pause),
            Ok(true),
            "the person's own launcher is seen to be theirs"
        );
    }

    #[test]
    fn a_process_pi_started_is_an_agents_wherever_it_runs() {
        let procs = procs(&[
            (50, "mycelium", 49, "agents.slice/kitty-2830-4.scope"),
            (49, "bash", 48, "agents.slice/kitty-2830-4.scope"),
            (48, "pi", 47, "agents.slice/kitty-2830-4.scope"),
            (47, "zsh", 1, "agents.slice/kitty-2830-4.scope"),
        ]);
        assert_eq!(
            folder(procs.path()).tell(50),
            Told::Agent("pi's process 48".to_owned()),
            "a command pi ran in a terminal is pi's"
        );
        assert_eq!(
            folder(procs.path()).tell(47),
            Told::Person,
            "the person's own shell in that terminal is theirs"
        );
    }

    #[test]
    fn a_mac_tells_pis_commands_by_what_ps_lists() {
        let listed = listing(
            "    1     0 /sbin/launchd\n\
             \x20 700     1 /Applications/kitty.app/Contents/MacOS/kitty\n\
             \x20 701   700 -zsh\n\
             \x20 702   701 pi\n\
             \x20 703   702 /bin/bash\n\
             \x20 704   703 /Users/joel/.nix-profile/bin/mycelium\n\
             \x20 705   701 /Users/joel/.nix-profile/bin/mycelium\n\
             \x20 706     1 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome\n",
        );
        let process = |pid: i32| listed.get(&pid).cloned();
        assert_eq!(
            forebears(704, process),
            Told::Agent("pi's process 702".to_owned()),
            "a command pi ran is pi's"
        );
        assert_eq!(
            forebears(705, process),
            Told::Person,
            "the person's own command in that terminal is theirs"
        );
        assert_eq!(
            listed.get(&706).map(|(name, _)| name.as_str()),
            Some("Google Chrome"),
            "a program's name may hold spaces"
        );
        assert_eq!(
            forebears(999, process),
            Told::Untold,
            "one gone tells nothing"
        );
    }

    #[test]
    fn the_session_that_asks_is_the_one_whose_process_it_runs_below() {
        let procs = procs(&[
            (60, "mycelium", 59, "jobs.slice/jobs.service"),
            (59, "bash", 58, "jobs.slice/jobs.service"),
            (58, "pi", 1, "agents.slice/mycelium-pi-f3d9.scope"),
            (61, "mycelium", 1, "session.slice/quickshell.service"),
        ]);
        let owners = [("s-own".to_owned(), 58), ("s-other".to_owned(), 70)];
        assert_eq!(
            session_of(Some(60), &folder(procs.path()), &owners),
            Ok("s-own".to_owned()),
            "a command a session ran is that session's"
        );
        assert_eq!(
            session_of(Some(61), &folder(procs.path()), &owners),
            Err("this asks from no session this node knows, so it orders no carry".to_owned()),
            "the person's own process is no session's"
        );
        assert_eq!(
            session_of(None, &folder(procs.path()), &owners),
            Err("nothing names the process that asks, so no session can be told to ask".to_owned()),
            "nor is a client the kernel names no process for"
        );
    }

    #[test]
    fn among_a_hosts_sessions_the_one_its_command_names_asks() {
        let procs = procs(&[
            (80, "mycelium", 79, "jobs.slice/jobs.service"),
            (79, "bash", 78, "jobs.slice/jobs.service"),
            (
                78,
                "node",
                1,
                "agents.slice/pi-host-work-background.service",
            ),
        ]);
        let environ = procs.path().join("80").join("environ");
        let owners = [
            ("s-one".to_owned(), 78),
            ("s-two".to_owned(), 78),
            ("s-three".to_owned(), 90),
        ];
        let told = || session_of(Some(80), &folder(procs.path()), &owners);
        assert!(
            told().is_err_and(|why| why.contains("2 sessions share")),
            "a host's command that names no session is no one's"
        );
        fs::write(
            &environ,
            "HOME=/home/joel\0PI_SESSION_ID=s-two\0PATH=/bin\0",
        )
        .expect("its environment is written");
        assert_eq!(told(), Ok("s-two".to_owned()), "the one it names");
        fs::write(&environ, "PI_SESSION_ID=s-three\0").expect("written");
        assert!(
            told().is_err(),
            "never a session of another process, whatever it names"
        );
    }

    /// Writes in `procs` that process `pid`, named `name`, started
    /// `ticks` after boot, as its `stat` file tells it.
    fn started(procs: &Path, pid: i32, name: &str, ticks: u64) {
        let process = procs.join(pid.to_string());
        fs::create_dir_all(&process).expect("its folder is made");
        let zeros = "0 ".repeat(17);
        fs::write(
            process.join("stat"),
            format!("{pid} ({name}) S 1 {zeros}{ticks} 0 0\n"),
        )
        .expect("its stat is written");
    }

    fn claim(session: &str, pid: i32, ticks: Option<u64>, host: Option<&str>) -> Claim {
        Claim {
            session: session.to_owned(),
            pid: Some(pid),
            start_ticks: ticks,
            host: host.map(str::to_owned),
        }
    }

    /// Opens, for process `pid` in `procs`, the log pi keeps of `session`
    /// in `logs`, as a link in its `fd` folder to the log's real path.
    fn holds_log(procs: &Path, pid: i32, logs: &Path, session: &str) {
        let log = logs
            .join("--home-joel--")
            .join(format!("2026-11-15T12-00-00-000Z_{session}.jsonl"));
        fs::create_dir_all(log.parent().expect("its folder")).expect("made");
        fs::write(
            &log,
            format!("{{\"type\":\"session\",\"id\":\"{session}\"}}\n"),
        )
        .expect("written");
        let fds = procs.join(pid.to_string()).join("fd");
        fs::create_dir_all(&fds).expect("made");
        let next = fs::read_dir(&fds).expect("read").count() + 3;
        std::os::unix::fs::symlink(&log, fds.join(next.to_string())).expect("linked");
    }

    fn owned(owners: &[(&str, i32)]) -> Vec<(String, i32)> {
        owners
            .iter()
            .map(|(session, pid)| ((*session).to_owned(), *pid))
            .collect()
    }

    #[test]
    fn an_owner_counts_only_where_it_holds_its_sessions_log_open() {
        let procs = procs(&[
            (58, "pi", 1, "agents.slice/mycelium-pi-f3d9.scope"),
            (62, "pi", 1, "agents.slice/mycelium-pi-a1b2.scope"),
            (63, "pi", 1, "agents.slice/mycelium-pi-c3d4.scope"),
        ]);
        started(procs.path(), 58, "pi", 500);
        started(procs.path(), 62, "pi", 900);
        started(procs.path(), 63, "pi", 950);
        let logs = tempfile::tempdir().expect("pi's logs");
        let elsewhere = tempfile::tempdir().expect("somewhere else");
        holds_log(procs.path(), 58, logs.path(), "s-own");
        holds_log(procs.path(), 62, logs.path(), "s-other");
        holds_log(procs.path(), 63, elsewhere.path(), "s-astray");
        let hosts = tempfile::tempdir().expect("no hosts");
        assert_eq!(
            proved(
                &[
                    claim("s-own", 58, Some(500), None),
                    claim("s-borrowed", 62, Some(900), None),
                    claim("s-astray", 63, Some(950), None),
                ],
                &folder(procs.path()),
                hosts.path(),
                logs.path()
            ),
            Ok(owned(&[("s-own", 58)])),
            "a pi holding another session's log, or a log outside pi's, owns nothing"
        );
    }

    #[test]
    fn an_owner_counts_only_where_its_process_started_when_it_says() {
        let procs = procs(&[
            (58, "pi", 1, "agents.slice/mycelium-pi-f3d9.scope"),
            (62, "pi", 1, "agents.slice/mycelium-pi-a1b2.scope"),
            (63, "pi", 1, "agents.slice/mycelium-pi-c3d4.scope"),
        ]);
        started(procs.path(), 58, "pi", 500);
        started(procs.path(), 62, "pi", 900);
        started(procs.path(), 63, "pi", 950);
        let logs = tempfile::tempdir().expect("pi's logs");
        holds_log(procs.path(), 58, logs.path(), "s-own");
        holds_log(procs.path(), 62, logs.path(), "s-stale");
        holds_log(procs.path(), 63, logs.path(), "s-untimed");
        let hosts = tempfile::tempdir().expect("no hosts");
        assert_eq!(
            proved(
                &[
                    claim("s-own", 58, Some(500), None),
                    claim("s-stale", 62, Some(800), None),
                    claim("s-untimed", 63, None, None),
                    claim("s-gone", 64, Some(100), None),
                ],
                &folder(procs.path()),
                hosts.path(),
                logs.path()
            ),
            Ok(owned(&[("s-own", 58)])),
            "a process started later under an old id, or one that says no start or is gone, \
             owns nothing"
        );
    }

    #[test]
    fn a_helper_claiming_anothers_session_owns_nothing() {
        let procs = procs(&[
            (58, "pi", 1, "agents.slice/mycelium-pi-f3d9.scope"),
            (60, "bash", 59, "jobs.slice/jobs.service"),
        ]);
        started(procs.path(), 58, "pi", 500);
        started(procs.path(), 60, "bash", 700);
        let logs = tempfile::tempdir().expect("pi's logs");
        holds_log(procs.path(), 58, logs.path(), "s-steward");
        holds_log(procs.path(), 58, logs.path(), "s-borrow");
        holds_log(procs.path(), 60, logs.path(), "s-steward");
        let hosts = tempfile::tempdir().expect("no hosts");
        let processes = folder(procs.path());
        assert_eq!(
            proved(
                &[
                    claim("s-steward", 58, Some(500), None),
                    claim("s-steward", 60, Some(700), None),
                ],
                &processes,
                hosts.path(),
                logs.path()
            ),
            Err(
                "two presences here claim the session s-steward, so who owns it can't be told \
                 and no carry is ordered"
                    .to_owned()
            ),
            "one forged beside the session's own refuses outright"
        );
        assert_eq!(
            proved(
                &[claim("s-steward", 60, Some(700), None)],
                &processes,
                hosts.path(),
                logs.path()
            ),
            Ok(Vec::new()),
            "and alone, a process that isn't pi owns no session no host holds"
        );
        assert_eq!(
            proved(
                &[
                    claim("s-steward", 58, Some(500), None),
                    claim("s-borrow", 58, Some(500), None),
                ],
                &processes,
                hosts.path(),
                logs.path()
            ),
            Ok(Vec::new()),
            "nor does a pi two sessions no host holds both claim"
        );
    }

    #[test]
    fn a_hosted_session_counts_only_where_its_hosts_record_lists_it() {
        let procs = procs(&[(
            78,
            "node",
            1,
            "agents.slice/pi-host-work-background.service",
        )]);
        started(procs.path(), 78, "node", 700);
        let hosts = tempfile::tempdir().expect("the hosts' records");
        fs::write(
            hosts.path().join("78-700.json"),
            json!({"key": "work-background",
                   "owner": {"pid": 78, "startedAt": 1, "startTicks": 700},
                   "sessions": [{"id": "s-one", "state": "running"},
                                {"id": "s-two", "state": "running"}]})
            .to_string(),
        )
        .expect("its record is written");
        let logs = tempfile::tempdir().expect("pi's logs");
        for session in ["s-one", "s-two", "s-three", "s-four"] {
            holds_log(procs.path(), 78, logs.path(), session);
        }
        let host = Some("work-background");
        assert_eq!(
            proved(
                &[
                    claim("s-one", 78, Some(700), host),
                    claim("s-two", 78, Some(700), host),
                    claim("s-three", 78, Some(700), host),
                    claim("s-four", 78, Some(700), Some("work-attended")),
                ],
                &folder(procs.path()),
                hosts.path(),
                logs.path()
            ),
            Ok(owned(&[("s-one", 78), ("s-two", 78)])),
            "a session its host doesn't list, or a host by another key, owns nothing"
        );
    }

    #[test]
    fn where_the_kernel_cant_say_when_a_process_started_nothing_is_proved() {
        let hosts = tempfile::tempdir().expect("no hosts");
        assert_eq!(
            proved(
                &[claim("s-own", 58, Some(500), None)],
                &Processes::Listed,
                hosts.path(),
                hosts.path()
            ),
            Err(
                "this machine can't say when a process started, so who owns a session can't \
                 be proved and no carry is ordered"
                    .to_owned()
            ),
            "as on a Mac"
        );
    }

    #[test]
    fn a_claim_is_read_from_its_presence() {
        let hosted = json!({"session": "s-one", "owner": {
            "pid": 78, "startedAt": 1, "startTicks": 700, "host": "work-background"}});
        assert_eq!(
            Claim::of("s-one", &hosted),
            Some(claim("s-one", 78, Some(700), Some("work-background")))
        );
        let ownerless = Claim {
            session: "s-one".to_owned(),
            pid: None,
            start_ticks: None,
            host: None,
        };
        assert_eq!(
            Claim::of("s-forged", &hosted),
            Some(ownerless.clone()),
            "one filed under another session's id names no owner"
        );
        assert_eq!(
            Claim::of("s-one", &json!({"session": "s-one", "owner": null})),
            Some(ownerless),
            "one that names no owner still counts toward a session claimed twice"
        );
        assert_eq!(Claim::of("s-one", &json!({"owner": {"pid": 1}})), None);
    }

    #[test]
    fn an_asker_nothing_names_cant_speak_as_the_person() {
        let pause = json!({"act": "pause"});
        assert_eq!(
            may_say(None, &folder(Path::new("/nowhere/proc")), &pause),
            Err(
                "nothing names the process that asks, so it can't speak as the person: \
                 only the person's own login says what they say"
                    .to_owned()
            ),
            "as a client of a network door would be"
        );
        assert_eq!(
            may_say(
                None,
                &folder(Path::new("/nowhere/proc")),
                &json!({"text": "hi", "from": "machine"})
            ),
            Ok(false),
            "though it may say what it likes as itself"
        );
    }

    #[test]
    fn an_asker_nothing_can_be_read_of_is_taken_at_its_word_but_not_seen() {
        assert_eq!(
            may_say(
                Some(40),
                &folder(Path::new("/nowhere/proc")),
                &json!({"act": "pause"})
            ),
            Ok(false)
        );
    }
}
