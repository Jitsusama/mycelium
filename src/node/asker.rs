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

use crate::declared::Kind;
use crate::steer::claims_the_person;
use crate::wire::By;

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

/// The name a pi host's process goes by where no agent's unit tells it,
/// as on a Mac, whose host launchd runs as the person's account, so its
/// conversations' commands are told by it as an agent's. Its runtime runs
/// it on a node of its own, kept in a folder of this name, so `ps` lists
/// it as that folder's `node` ([`listing`]).
const PI_HOST: &str = "pi-host";

/// How many forebears are looked through at most, so a chain the kernel
/// tells wrong can't hold a steer up for long.
const FOREBEARS: usize = 64;

/// What an unattended machine tells of whoever asks: an agent, since no
/// person works there.
const UNATTENDED: &str = "a process on a machine no person works on";

/// Where a machine tells who runs each of its processes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Processes {
    /// Linux's `/proc`, or a stand-in for it: each process's control
    /// group, name and parent.
    Folder(PathBuf),
    /// What `ps` lists, as on a Mac, which has no `/proc`: each process's
    /// name and parent.
    Listed,
    /// Those of a machine no person works on, as in a cell lent from a
    /// basin, which tells every process that asks as an agent's: the node
    /// there runs as fenced as the agents beside it, below no agent's
    /// unit, so nothing else would tell them from the person, who reaches
    /// it only over a link. What the kernel tells beyond who asks, such as
    /// when a process started, is read from those it holds.
    Unattended(Box<Processes>),
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

    /// These, on a machine no person works on.
    #[must_use]
    pub fn unattended(self) -> Self {
        match self {
            Self::Unattended(_) => self,
            kernel => Self::Unattended(Box::new(kernel)),
        }
    }

    /// Whether no person works on this machine.
    #[must_use]
    pub const fn is_unattended(&self) -> bool {
        matches!(self, Self::Unattended(_))
    }

    /// Where the kernel tells of these processes, whoever works here.
    #[must_use]
    pub fn kernel(&self) -> &Self {
        match self {
            Self::Unattended(kernel) => kernel.kernel(),
            kernel => kernel,
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
        match self.kernel() {
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
            Self::Unattended(_) => vec![pid],
        }
    }

    /// The session process `pid` says it runs for, as the harness tells
    /// each command it runs in its environment; a Mac's `ps` won't say.
    fn named_session(&self, pid: i32) -> Option<String> {
        let Self::Folder(procs) = self.kernel() else {
            return None;
        };
        began_with(procs, pid, b"PI_SESSION_ID=")
    }

    /// The session the host at process `host` started the spawner below
    /// it, in `asker`'s lineage, to run commands for, as the spawner's own
    /// environment names it. Only the host makes its direct children, and
    /// pi-spawner never replaces itself, so nothing it runs can change what
    /// it began with; a command the host started itself could, and names
    /// none here.
    fn spawned_for(&self, asker: i32, host: i32) -> Option<String> {
        let Self::Folder(procs) = self else {
            return None;
        };
        let lineage = self.lineage(asker);
        let at = lineage.iter().position(|process| *process == host)?;
        let spawner = *lineage.get(at.checked_sub(1)?)?;
        let status = fs::read_to_string(procs.join(spawner.to_string()).join("status")).ok()?;
        let (name, parent) = named(&status)?;
        (name == SPAWNER && parent == host).then_some(())?;
        began_with(procs, spawner, SPAWNED_FOR)
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
            Self::Unattended(_) => Told::Agent(UNATTENDED.to_owned()),
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
    let Processes::Folder(procs) = processes.kernel() else {
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

/// What the harness names a session's fence by, in the environment its
/// pi starts with.
const SANDBOX: &[u8] = b"PI_SANDBOX=";

/// The name the agent fence's bubblewrap runs as, below the pi whose
/// command it fences.
const BWRAP: &str = "bwrap";

/// How many processes are looked through at most for those below a pi,
/// so a machine running a great many can't hold a carry up for long.
const LOOKED_THROUGH: usize = 65_536;

/// The fence `session` shows it runs in, by what its owner's process
/// holds, proved as [`proved`] proves it, and never by the session's own
/// presence: `PI_SANDBOX` in the environment that process began with,
/// which pi never changes there and no command it fences can reach; and,
/// where that pi owns this session alone, a bubblewrap running below it,
/// which only the agent fence starts. The harness fences each command
/// rather than pi itself, so pi's own seccomp, `no_new_privs` and mount
/// namespace say nothing of it, and the strict fence's Landlock leaves
/// nothing past each command it runs.
///
/// # Errors
///
/// Where who owns `session` can't be proved, or its process read, as on a
/// Mac, since then the fence it runs in can't be told.
pub fn shown_fence(
    claims: &[Claim],
    processes: &Processes,
    hosts: &Path,
    logs: &Path,
    session: &str,
) -> Result<Kind, String> {
    let unproved = || {
        format!("who owns {session} can't be proved here, so the fence it runs in can't be told")
    };
    let Processes::Folder(procs) = processes.kernel() else {
        return Err(unproved());
    };
    let owners = proved(claims, processes, hosts, logs)?;
    let pid = owners
        .iter()
        .find_map(|(owned, pid)| (owned == session).then_some(*pid))
        .ok_or_else(unproved)?;
    // Proved, so claimed once, with when its process started.
    let claim = claims
        .iter()
        .find(|claim| claim.session == session)
        .ok_or_else(unproved)?;
    if claim.host.is_some() {
        return Err(format!(
            "{session} runs in a pi host, whose environment says nothing of each session's \
             fence, so the fence it runs in can't be told"
        ));
    }
    let ticks = claim.start_ticks.ok_or_else(unproved)?;
    shown_of(procs, &Tree::of(procs), pid, ticks, session)
}

/// The fence each session here shows it runs in, as [`shown_fence`] tells
/// it, for every session whose owner is proved and holds no other, read
/// with one look through the machine's processes. None where who owns
/// what can't be told.
pub fn shown_fences(
    claims: &[Claim],
    processes: &Processes,
    hosts: &Path,
    logs: &Path,
) -> std::collections::BTreeMap<String, Kind> {
    let Processes::Folder(procs) = processes.kernel() else {
        return std::collections::BTreeMap::new();
    };
    let Ok(owners) = proved(claims, processes, hosts, logs) else {
        return std::collections::BTreeMap::new();
    };
    let tree = Tree::of(procs);
    owners
        .into_iter()
        .filter_map(|(session, pid)| {
            let claim = claims.iter().find(|claim| claim.session == session)?;
            if claim.host.is_some() {
                return None;
            }
            let shown = shown_of(procs, &tree, pid, claim.start_ticks?, &session).ok()?;
            Some((session, shown))
        })
        .collect()
}

/// The fence pi `pid`, proved to own `session` alone as it started
/// `ticks` after boot, shows: `PI_SANDBOX` as its environment began, and
/// at least the ordinary fence where a bubblewrap runs below it in
/// `tree`. Read only while `pid` is still that process, since the
/// environment of a process that took its id after says nothing of it.
fn shown_of(
    procs: &Path,
    tree: &Tree,
    pid: i32,
    ticks: u64,
    session: &str,
) -> Result<Kind, String> {
    let environ = fs::read(procs.join(pid.to_string()).join("environ")).map_err(|_| {
        format!("{session}'s process can't be read, so the fence it runs in can't be told")
    })?;
    let began = environ
        .split(|byte| *byte == 0)
        .find_map(|pair| pair.strip_prefix(SANDBOX))
        .map_or(Kind::None, |value| match value.trim_ascii() {
            b"strict" => Kind::Strict,
            b"fence" => Kind::Fence,
            _ => Kind::None,
        });
    let fenced_below = tree.runs_below(pid, BWRAP);
    // Read again last, so what was read before was that process's.
    if started(procs, pid) != Some(ticks) {
        return Err(format!(
            "{session}'s process {pid} ended while it was read, so the fence it runs in can't \
             be told"
        ));
    }
    Ok(if fenced_below {
        began.max(Kind::Fence)
    } else {
        began
    })
}

/// The machine's processes, each by its id with its name and parent, as
/// one look through them found them.
struct Tree(Vec<(i32, String, i32)>);

impl Tree {
    /// The processes in `procs`, at most [`LOOKED_THROUGH`] of them.
    fn of(procs: &Path) -> Self {
        let Ok(entries) = fs::read_dir(procs) else {
            return Self(Vec::new());
        };
        Self(
            entries
                .flatten()
                .take(LOOKED_THROUGH)
                .filter_map(|entry| {
                    let child: i32 = entry.file_name().to_str()?.parse().ok()?;
                    let status = fs::read_to_string(entry.path().join("status")).ok()?;
                    let (named, parent) = named(&status)?;
                    Some((child, named.to_owned(), parent))
                })
                .collect(),
        )
    }

    /// Whether a process named `name` runs below process `pid`, at any
    /// depth.
    fn runs_below(&self, pid: i32, name: &str) -> bool {
        let mut below = vec![pid];
        let mut seen = std::collections::BTreeSet::from([pid]);
        while let Some(parent) = below.pop() {
            for (child, named, _) in self.0.iter().filter(|(_, _, of)| *of == parent) {
                if named == name {
                    return true;
                }
                if seen.insert(*child) {
                    below.push(*child);
                }
            }
        }
        false
    }
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
pub(super) fn started(procs: &Path, pid: i32) -> Option<u64> {
    let stat = fs::read_to_string(procs.join(pid.to_string()).join("stat")).ok()?;
    let (_, after) = stat.rsplit_once(')')?;
    after.split_whitespace().nth(19)?.parse().ok()
}

/// The value of the variable `key` names, its `=` included, in the
/// environment process `pid` in `procs` began with.
fn began_with(procs: &Path, pid: i32, key: &[u8]) -> Option<String> {
    let environ = fs::read(procs.join(pid.to_string()).join("environ")).ok()?;
    environ.split(|byte| *byte == 0).find_map(|pair| {
        let value = pair.strip_prefix(key)?;
        String::from_utf8(value.to_vec()).ok()
    })
}

/// The name the kernel gives the harness's pi-spawner, which a pi host
/// starts once for each of its sessions.
const SPAWNER: &str = "pi-spawner";

/// What names, in a pi-spawner's own environment, the session whose
/// commands it runs.
const SPAWNED_FOR: &[u8] = b"PI_SPAWNER_SESSION=";

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
/// it, as only the person frees a quest an orchestrator holds or carries
/// a quest in one act: what it may then `do`, as a person says it.
///
/// # Errors
///
/// When the asker is an agent's, or nothing can be told of it.
pub fn persons_own(asker: Option<i32>, processes: &Processes, does: &str) -> Result<(), String> {
    let only = format!("so it can't {does}: only the person's own login does");
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

/// Whether process `asker`, judged `by` as [`judge`] judges any asker,
/// claims and all, may relay a start as a linked node's: the node's own,
/// or one judged the person's, as a link's far end, a `mycelium link` an
/// ssh login runs, is. An agent's unit, a pi or a session's claim above
/// it, a fence's bar on gaining privileges or mounts of its own each
/// refuse it. On a machine no person works on every asker is judged an
/// agent's, since an agent's ssh login lands in a login's unit as the
/// person's does, so there the node's own process alone relays.
#[must_use]
pub fn relays(asker: Option<i32>, by: By) -> bool {
    by == By::Person
        || asker.is_some_and(|pid| u32::try_from(pid).is_ok_and(|pid| pid == std::process::id()))
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
    let (pid, sessions) = owning(asker, processes, owners)?;
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

/// The session whose work trees a run `asker` asks for may stand in, as
/// [`session_of`] tells who asks, but only where the process proved to
/// own it owns no other: a pi host's sessions share one process, and
/// which of them a command runs for is only what its environment says,
/// which any command there may set. So a host's session is never told
/// to a fence as the one a run stands for, whatever its command names.
///
/// Alone is counted over every claim here, proved or not, since a
/// session whose claim doesn't prove yet still shares its host's process:
/// the nearest of the asker and its forebears that any claim or owner
/// names must be named by exactly one, the claims naming the same host
/// and the sessions its host's own record lists counted with it, and
/// that one must be proved.
///
/// # Errors
///
/// When nothing names the asker, no session here claims it or its
/// forebears, the nearest claimed is claimed by more than one, or its
/// claim doesn't prove.
pub fn session_alone(
    asker: Option<i32>,
    processes: &Processes,
    owners: &[(String, i32)],
    claims: &[Claim],
    hosts: &Path,
) -> Result<String, String> {
    let Some(pid) = asker else {
        return Err(
            "nothing names the process that asks, so no session can be told to ask".to_owned(),
        );
    };
    let Some(process) = processes.lineage(pid).into_iter().find(|process| {
        claims.iter().any(|claim| claim.pid == Some(*process))
            || owners.iter().any(|(_, owner)| owner == process)
    }) else {
        return Err("this asks from no session this node knows".to_owned());
    };
    let keys: std::collections::BTreeSet<&str> = claims
        .iter()
        .filter(|claim| claim.pid == Some(process))
        .filter_map(|claim| claim.host.as_deref())
        .collect();
    let claimed = claims
        .iter()
        .filter(|claim| {
            claim.pid == Some(process)
                || claim
                    .host
                    .as_deref()
                    .is_some_and(|host| keys.contains(host))
        })
        .count();
    let sharing = claimed.max(hosted(hosts, process));
    let proved: Vec<&str> = owners
        .iter()
        .filter(|(_, owner)| *owner == process)
        .map(|(session, _)| session.as_str())
        .collect();
    // A host's sessions share its process, but each runs its commands
    // through a spawner of its own that the host names it to.
    let spawned = (!keys.is_empty())
        .then(|| processes.spawned_for(pid, process))
        .flatten()
        .filter(|session| proved.contains(&session.as_str()));
    if let Some(session) = spawned {
        return Ok(session);
    }
    match proved.as_slice() {
        [one] if sharing <= 1 => Ok((*one).to_owned()),
        [] => Err(format!(
            "this asks below process {process}, whose session none proves"
        )),
        _ => Err(format!(
            "this asks from a process {} sessions share, so none is proved alone",
            sharing.max(proved.len())
        )),
    }
}

/// The most sessions any pi host's record in `hosts` lists for process
/// `pid`, whenever it started, so a session its host holds counts toward
/// those sharing it before its own presence is written. A record of an
/// earlier process given the same id counts too, which only ever refuses.
fn hosted(hosts: &Path, pid: i32) -> usize {
    let prefix = format!("{pid}-");
    fs::read_dir(hosts)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&prefix))
        })
        .filter_map(|path| fs::read_to_string(path).ok())
        .filter_map(|text| serde_json::from_str::<Value>(&text).ok())
        .filter_map(|record| record.get("sessions")?.as_array().map(Vec::len))
        .max()
        .unwrap_or(0)
}

/// The asker's process and the sessions of `owners` owned by the nearest
/// of it and its forebears to own any.
fn owning<'a>(
    asker: Option<i32>,
    processes: &Processes,
    owners: &'a [(String, i32)],
) -> Result<(i32, Vec<&'a str>), String> {
    let Some(pid) = asker else {
        return Err(
            "nothing names the process that asks, so no session can be told to ask".to_owned(),
        );
    };
    processes
        .lineage(pid)
        .into_iter()
        .find_map(|process| {
            let sessions: Vec<&str> = owners
                .iter()
                .filter(|(_, owner)| *owner == process)
                .map(|(session, _)| session.as_str())
                .collect();
            (!sessions.is_empty()).then_some((pid, sessions))
        })
        .ok_or_else(|| "this asks from no session this node knows".to_owned())
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
        if name == PI_HOST {
            return Told::Agent(format!("pi's host {at}"));
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
/// path, spaces and all, so its name is what follows the last slash. A
/// `node` in a folder named [`PI_HOST`] is the pi host's own, and goes by
/// that name, so it is told from every other node program.
fn listing(listed: &str) -> HashMap<i32, (String, i32)> {
    listed
        .lines()
        .filter_map(|line| {
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (parent, command) = rest.trim_start().split_once(char::is_whitespace)?;
            let command = command.trim();
            let mut path = command.rsplit('/');
            let name = path.next().unwrap_or(command);
            let name = if name == "node" && path.next() == Some(PI_HOST) {
                PI_HOST
            } else {
                name
            };
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

/// The session whose cell a start `asker` asks counts toward: the one
/// claiming the nearest of the asker and its forebears that any claim
/// names, where only one does. A pi host's sessions all claim its one
/// process, so there the one the asker names its own (`PI_SESSION_ID`)
/// counts, if it is among them, and otherwise none. An agent sets its own
/// environment, so one there may name another of the host's sessions and
/// charge that cell rather than its own; the node's number still bounds
/// them all. A claim proves nothing, so this is a fair share, never a
/// fence.
#[must_use]
pub fn charged(asker: Option<i32>, processes: &Processes, claims: &[Claim]) -> Option<String> {
    let pid = asker?;
    let mut nearest: Vec<&str> = processes.lineage(pid).into_iter().find_map(|process| {
        let named: Vec<&str> = claims
            .iter()
            .filter(|claim| claim.pid == Some(process))
            .map(|claim| claim.session.as_str())
            .collect();
        (!named.is_empty()).then_some(named)
    })?;
    nearest.sort_unstable();
    nearest.dedup();
    if let [one] = nearest.as_slice() {
        return Some((*one).to_owned());
    }
    processes
        .named_session(pid)
        .filter(|named| nearest.contains(&named.as_str()))
}

/// Who process `asker` is, for a command it asks to run, as `processes`
/// tell it, where `above` is the fence each session claiming it says it
/// runs in, or none where that can't be read. It is an agent when it
/// runs as one, as [`Processes::tell`] says, or below a session, or it
/// was fenced as the node wasn't: it may gain no privileges where the
/// node may, or it sees other mounts. An agent runs in the strictest
/// fence above it, at least the agent fence, and strictly fenced where
/// none is claimed. An asker nothing can be told of is a strictly fenced
/// agent too, so a doubt only ever makes a run stricter.
///
/// Like [`may_say`], this fences habit, not an agent set on it.
#[must_use]
pub fn judge(asker: Option<i32>, processes: &Processes, above: &[Option<Kind>]) -> By {
    let fence = above
        .iter()
        .map(|kind| kind.unwrap_or(Kind::Strict))
        .max()
        .map_or(Kind::Strict, |kind| kind.max(Kind::Fence));
    let Some(pid) = asker else {
        return By::Agent(Kind::Strict);
    };
    if !above.is_empty() {
        return By::Agent(fence);
    }
    match processes.tell(pid) {
        Told::Person => {}
        Told::Agent(_) => return By::Agent(fence),
        Told::Untold => return By::Agent(Kind::Strict),
    }
    if let Processes::Folder(procs) = processes.kernel()
        && (gains_less(procs, pid) || mounts_otherwise(procs, pid))
    {
        return By::Agent(fence);
    }
    By::Person
}

/// Why [`unclaimed_because`] says an asker is an agent's where the
/// kernel names no process at it.
pub const UNNAMED: &str = "the kernel names no process at it";

/// Why [`unclaimed_because`] says an asker is an agent's where nothing
/// of its process can be read.
pub const UNTOLD: &str = "nothing can be read of it or a forebear, so it can't be told";

/// Why [`judge`] tells process `asker` an agent's where no session
/// claims it, as `processes` tell it, in words a refusal can say: the
/// step of the judgement that made it one.
#[must_use]
pub fn unclaimed_because(asker: Option<i32>, processes: &Processes) -> &'static str {
    let Some(pid) = asker else {
        return UNNAMED;
    };
    match processes.tell(pid) {
        Told::Agent(_) => return "an agent's unit, or pi or a pi host above it, runs it",
        Told::Untold => return UNTOLD,
        Told::Person => {}
    }
    if let Processes::Folder(procs) = processes.kernel() {
        if gains_less(procs, pid) {
            return "it is barred from gaining privileges, as a fence bars it";
        }
        if mounts_otherwise(procs, pid) {
            return "it sees mounts of its own, as a fence's process does";
        }
    }
    "nothing marks it an agent's"
}

/// Whether process `pid` may gain no privileges, as a fence sets, while
/// the node itself may.
fn gains_less(procs: &Path, pid: i32) -> bool {
    let barred = |process: &str| {
        fs::read_to_string(procs.join(process).join("status")).is_ok_and(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("NoNewPrivs:"))
                .is_some_and(|value| value.trim() == "1")
        })
    };
    barred(&pid.to_string()) && !barred("self")
}

/// Whether process `pid` sees other mounts than the node does, as one a
/// fence put in a namespace of its own does, or can't be told where the
/// node's own can.
fn mounts_otherwise(procs: &Path, pid: i32) -> bool {
    let mounts = |process: &str| fs::read_link(procs.join(process).join("ns").join("mnt")).ok();
    mounts("self").is_some_and(|own| mounts(&pid.to_string()) != Some(own))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
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
             \x20 706     1 /Applications/Google Chrome.app/Contents/MacOS/Google Chrome\n\
             \x20 710     1 /nix/store/abc-pi-host-runtime-1.5.0/libexec/pi-host/node\n\
             \x20 711   710 /bin/zsh\n\
             \x20 712   711 /Users/joel/.nix-profile/bin/mycelium\n\
             \x20 713     1 /nix/store/abc-node/bin/node\n\
             \x20 714   713 /Users/joel/.nix-profile/bin/mycelium\n",
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
            forebears(712, process),
            Told::Agent("pi's host 710".to_owned()),
            "a command a pi host's conversation ran is an agent's"
        );
        assert_eq!(
            forebears(714, process),
            Told::Person,
            "a host is known only by its own node, never as any node program"
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
            Err("this asks from no session this node knows".to_owned()),
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

    #[test]
    fn a_hosts_start_counts_toward_the_session_it_names_and_no_other() {
        let procs = procs(&[
            (80, "mycelium", 79, "jobs.slice/jobs.service"),
            (79, "bash", 78, "jobs.slice/jobs.service"),
            (
                78,
                "node",
                1,
                "agents.slice/pi-host-work-background.service",
            ),
            (70, "mycelium", 69, "agents.slice/pi-a.scope"),
            (69, "pi", 1, "agents.slice/pi-a.scope"),
        ]);
        let claims = [
            claim("s-first", 78, Some(1), Some("work")),
            claim("s-two", 78, Some(1), Some("work")),
            claim("s-alone", 69, Some(1), None),
        ];
        let processes = folder(procs.path());
        let environ = procs.path().join("80").join("environ");
        assert_eq!(
            charged(Some(80), &processes, &claims),
            None,
            "a host's command naming none counts toward none of its sessions"
        );
        fs::write(&environ, "PI_SESSION_ID=s-two\0").expect("written");
        assert_eq!(
            charged(Some(80), &processes, &claims).as_deref(),
            Some("s-two"),
            "the one it names, not the first claimed"
        );
        fs::write(&environ, "PI_SESSION_ID=s-alone\0").expect("written");
        assert_eq!(
            charged(Some(80), &processes, &claims),
            None,
            "never a session of another process, whatever it names"
        );
        assert_eq!(
            charged(Some(70), &processes, &claims).as_deref(),
            Some("s-alone"),
            "a session alone in its process counts as ever"
        );
        assert_eq!(charged(None, &processes, &claims), None);
    }

    #[test]
    fn no_session_of_a_hosts_is_proved_alone_whatever_its_command_names() {
        let procs = procs(&[
            (80, "mycelium", 79, "jobs.slice/jobs.service"),
            (79, "bash", 78, "jobs.slice/jobs.service"),
            (
                78,
                "node",
                1,
                "agents.slice/pi-host-work-background.service",
            ),
            (70, "mycelium", 69, "agents.slice/pi-a.scope"),
            (69, "pi", 1, "agents.slice/pi-a.scope"),
            (60, "mycelium", 59, "agents.slice/pi-a.scope"),
            (59, "pi", 69, "agents.slice/pi-a.scope"),
        ]);
        fs::write(
            procs.path().join("80").join("environ"),
            "PI_SESSION_ID=s-two\0",
        )
        .expect("its environment is written");
        let mut claims = vec![
            claim("s-one", 78, Some(1), Some("work")),
            claim("s-two", 78, Some(1), Some("work")),
            claim("s-alone", 69, Some(1), None),
        ];
        let mut owners = vec![
            ("s-one".to_owned(), 78),
            ("s-two".to_owned(), 78),
            ("s-alone".to_owned(), 69),
        ];
        let processes = folder(procs.path());
        let hosts = tempfile::tempdir().expect("a folder for hosts' records");
        let alone = |asker, claims: &[Claim], owners: &[(String, i32)]| {
            session_alone(asker, &processes, owners, claims, hosts.path())
        };
        assert!(
            alone(Some(80), &claims, &owners).is_err_and(|why| why.contains("2 sessions share")),
            "a host's command may name any session of its host, so none is proved"
        );
        owners.retain(|(session, _)| session != "s-two");
        assert!(
            alone(Some(80), &claims, &owners).is_err_and(|why| why.contains("2 sessions share")),
            "nor where only one of its sessions proves, since the others still share it"
        );
        claims[1].pid = Some(77);
        assert!(
            alone(Some(80), &claims, &owners).is_err_and(|why| why.contains("2 sessions share")),
            "nor where another session says the same host holds it by another process"
        );
        assert_eq!(
            alone(Some(70), &claims, &owners),
            Ok("s-alone".to_owned()),
            "a session its process owns alone is proved by the process"
        );
        let (one, one_owner) = (
            [claim("s-one", 78, Some(1), Some("work"))],
            [("s-one".to_owned(), 78)],
        );
        assert_eq!(
            alone(Some(80), &one, &one_owner),
            Ok("s-one".to_owned()),
            "a host holding one session alone is proved to ask for it"
        );
        fs::write(
            hosts.path().join("78-1.json"),
            json!({"key": "work", "sessions": [{"id": "s-one"}, {"id": "s-three"}],
                   "owner": {"pid": 78, "startTicks": 1}})
            .to_string(),
        )
        .expect("the host's record is written");
        assert!(
            alone(Some(80), &one, &one_owner).is_err_and(|why| why.contains("2 sessions share")),
            "nor where its own record lists a session no presence claims yet"
        );
        claims.push(claim("s-nested", 59, Some(1), None));
        assert!(
            alone(Some(60), &claims, &owners).is_err_and(|why| why.contains("none proves")),
            "nor through a nearer pi that claims a session it can't prove"
        );
        assert!(
            alone(None, &claims, &owners).is_err(),
            "nor where nothing names the asker"
        );
    }

    #[test]
    fn a_hosts_session_is_proved_by_the_spawner_the_host_started_for_it() {
        let host = "agents.slice/pi-host-work-background.service";
        let procs = procs(&[
            (78, "node", 1, host),
            (88, "pi-spawner", 78, "pi-spawner-b.scope"),
            (89, "bash", 88, "pi-spawner-b.scope"),
            (90, "mycelium", 89, "pi-spawner-b.scope"),
            (87, "pi-spawner", 78, "pi-spawner-a.scope"),
            (86, "mycelium", 87, "pi-spawner-a.scope"),
            (94, "pi-spawner", 89, "pi-spawner-b.scope"),
            (95, "mycelium", 94, "pi-spawner-b.scope"),
            (79, "bash", 78, host),
            (80, "mycelium", 79, host),
            (84, "pi-spawner", 78, "pi-spawner-c.scope"),
            (85, "mycelium", 84, "pi-spawner-c.scope"),
            (82, "pi-spawner", 78, "pi-spawner-d.scope"),
            (83, "mycelium", 82, "pi-spawner-d.scope"),
        ]);
        for (pid, environ) in [
            (88, "PI_QUEST=QEST-B\0PI_SPAWNER_SESSION=s-two\0"),
            (87, "PI_SPAWNER_SESSION=s-one\0"),
            (94, "PI_SPAWNER_SESSION=s-one\0"),
            (79, "PI_SPAWNER_SESSION=s-one\0PI_SESSION_ID=s-one\0"),
            (84, "PI_SPAWNER_SESSION=s-alone\0"),
            (82, "PI_SPAWNER_SESSION=s-three\0"),
        ] {
            fs::write(procs.path().join(pid.to_string()).join("environ"), environ)
                .expect("its environment is written");
        }
        let claims = [
            claim("s-one", 78, Some(1), Some("work")),
            claim("s-two", 78, Some(1), Some("work")),
            claim("s-three", 78, Some(1), Some("work")),
            claim("s-alone", 69, Some(1), None),
        ];
        let mut owners = vec![
            ("s-one".to_owned(), 78),
            ("s-two".to_owned(), 78),
            ("s-alone".to_owned(), 69),
        ];
        let processes = folder(procs.path());
        let hosts = tempfile::tempdir().expect("a folder for hosts' records");
        let alone = |asker, owners: &[(String, i32)]| {
            session_alone(Some(asker), &processes, owners, &claims, hosts.path())
        };
        assert_eq!(
            alone(90, &owners),
            Ok("s-two".to_owned()),
            "the host started that spawner for s-two, and nothing below it can say otherwise"
        );
        assert_eq!(
            alone(86, &owners),
            Ok("s-one".to_owned()),
            "and the other spawner for s-one"
        );
        assert_eq!(
            alone(95, &owners),
            Ok("s-two".to_owned()),
            "a spawner started below a command proves nothing: only the host's own child does"
        );
        assert!(
            alone(80, &owners).is_err_and(|why| why.contains("sessions share")),
            "a command the host started itself, rather than a spawner, may have set its \
             own environment, so it proves no session"
        );
        assert!(
            alone(85, &owners).is_err_and(|why| why.contains("sessions share")),
            "a spawner naming a session another process owns proves nothing here"
        );
        assert!(
            alone(83, &owners).is_err_and(|why| why.contains("sessions share")),
            "nor one naming a session its host claims but can't prove"
        );
        owners.retain(|(session, _)| session != "s-two");
        assert!(
            alone(90, &owners).is_err_and(|why| why.contains("sessions share")),
            "nor once the session it names no longer proves"
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

    /// Three pis, 58, 62 and 63, each in a scope of its own, started at
    /// 500, 900 and 950 ticks.
    fn three_pis() -> tempfile::TempDir {
        let procs = procs(&[
            (58, "pi", 1, "agents.slice/mycelium-pi-f3d9.scope"),
            (62, "pi", 1, "agents.slice/mycelium-pi-a1b2.scope"),
            (63, "pi", 1, "agents.slice/mycelium-pi-c3d4.scope"),
        ]);
        started(procs.path(), 58, "pi", 500);
        started(procs.path(), 62, "pi", 900);
        started(procs.path(), 63, "pi", 950);
        procs
    }

    #[test]
    fn an_owner_counts_only_where_it_holds_its_sessions_log_open() {
        let procs = three_pis();
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
        let procs = three_pis();
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

    /// A stand-in for `/proc` where pi 58 owns `s-own` and began with
    /// `environ`, with the processes `below` it, each `(pid, name,
    /// parent)`, answering it with pi's logs and its hosts' records.
    fn fenced_pi(
        environ: &str,
        below: &[(i32, &str, i32)],
    ) -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
        let mut processes = vec![(58, "pi", 1, "agents.slice/mycelium-pi-f3d9.scope")];
        processes.extend(below.iter().map(|(pid, name, parent)| {
            (*pid, *name, *parent, "agents.slice/mycelium-pi-f3d9.scope")
        }));
        let procs = procs(&processes);
        started(procs.path(), 58, "pi", 500);
        fs::write(procs.path().join("58").join("environ"), environ)
            .expect("its environment is written");
        let logs = tempfile::tempdir().expect("pi's logs");
        holds_log(procs.path(), 58, logs.path(), "s-own");
        let hosts = tempfile::tempdir().expect("no hosts");
        (procs, logs, hosts)
    }

    /// The fence `s-own` shows it runs in, as pi 58 owns it alone or, with
    /// `host`, for the host keyed so.
    fn shown(
        (procs, logs, hosts): &(tempfile::TempDir, tempfile::TempDir, tempfile::TempDir),
        host: Option<&str>,
    ) -> Result<Kind, String> {
        shown_fence(
            &[claim("s-own", 58, Some(500), host)],
            &folder(procs.path()),
            hosts.path(),
            logs.path(),
            "s-own",
        )
    }

    #[test]
    fn a_session_shows_the_fence_its_pi_was_started_in() {
        for (environ, kind) in [
            ("HOME=/home/jsama\0PI_SANDBOX=strict\0", Kind::Strict),
            ("PI_SANDBOX=fence\0PATH=/bin\0", Kind::Fence),
            ("PI_SANDBOX=off\0", Kind::None),
            ("HOME=/home/jsama\0", Kind::None),
        ] {
            assert_eq!(
                shown(&fenced_pi(environ, &[]), None),
                Ok(kind),
                "as its environment began: {environ:?}"
            );
        }
    }

    #[test]
    fn a_bubblewrap_below_a_lone_pi_shows_it_fenced() {
        let running = fenced_pi(
            "HOME=/home/jsama\0",
            &[(70, "bash", 58), (71, "bwrap", 70), (72, "cargo", 71)],
        );
        assert_eq!(
            shown(&running, None),
            Ok(Kind::Fence),
            "a command fenced below it, at any depth"
        );
        let strict = fenced_pi("PI_SANDBOX=strict\0", &[(71, "bwrap", 58)]);
        assert_eq!(
            shown(&strict, None),
            Ok(Kind::Strict),
            "never looser than it began"
        );
        let beside = fenced_pi("HOME=/home/jsama\0", &[(71, "bwrap", 1)]);
        assert_eq!(
            shown(&beside, None),
            Ok(Kind::None),
            "a bubblewrap of another's says nothing of it"
        );
    }

    #[test]
    fn a_session_whose_fence_cant_be_read_shows_none() {
        let unread = fenced_pi("PI_SANDBOX=strict\0", &[]);
        fs::remove_file(unread.0.path().join("58").join("environ")).expect("removed");
        assert_eq!(
            shown(&unread, None),
            Err("s-own's process can't be read, so the fence it runs in can't be told".to_owned()),
            "never guessed unfenced"
        );
        let unowned = fenced_pi("PI_SANDBOX=strict\0", &[]);
        assert_eq!(
            shown(&unowned, Some("work-background")),
            Err(
                "who owns s-own can't be proved here, so the fence it runs in can't be told"
                    .to_owned()
            ),
            "a host whose record doesn't hold it owns it not"
        );
    }

    /// Writes the record of the host `work-background`, run by pi 58, as
    /// holding `sessions`.
    fn hosts_in_58(hosts: &Path, sessions: &[&str]) {
        let sessions: Vec<Value> = sessions
            .iter()
            .map(|id| json!({"id": id, "state": "running"}))
            .collect();
        fs::write(
            hosts.join("58-500.json"),
            json!({"key": "work-background",
                   "owner": {"pid": 58, "startedAt": 1, "startTicks": 500},
                   "sessions": sessions})
            .to_string(),
        )
        .expect("its host's record is written");
    }

    #[test]
    fn a_hosted_session_shows_no_fence_of_its_own() {
        let hosted = fenced_pi("PI_SANDBOX=strict\0", &[(71, "bwrap", 58)]);
        hosts_in_58(hosted.2.path(), &["s-own"]);
        assert_eq!(
            shown(&hosted, Some("work-background")),
            Err(
                "s-own runs in a pi host, whose environment says nothing of each session's \
                 fence, so the fence it runs in can't be told"
                    .to_owned()
            ),
            "a host's environment and commands are those of every session it holds"
        );
    }

    #[test]
    fn a_process_that_ends_while_it_is_read_shows_no_fence() {
        let (procs, _logs, _hosts) = fenced_pi("PI_SANDBOX=strict\0", &[]);
        let tree = Tree::of(procs.path());
        assert_eq!(
            shown_of(procs.path(), &tree, 58, 500, "s-own"),
            Ok(Kind::Strict),
            "while it runs as it was proved to"
        );
        started(procs.path(), 58, "pi", 900);
        assert_eq!(
            shown_of(procs.path(), &tree, 58, 500, "s-own"),
            Err(
                "s-own's process 58 ended while it was read, so the fence it runs in can't be \
                 told"
                    .to_owned()
            ),
            "an environment read once its id was another's says nothing of it"
        );
    }

    #[test]
    fn the_fences_shown_are_those_of_each_session_proved_to_run_alone() {
        let (procs, logs, hosts) = fenced_pi("PI_SANDBOX=fence\0", &[]);
        hosts_in_58(hosts.path(), &["s-hosted"]);
        holds_log(procs.path(), 58, logs.path(), "s-hosted");
        let claims = [
            claim("s-own", 58, Some(500), None),
            claim("s-hosted", 58, Some(500), Some("work-background")),
            claim("s-ghost", 59, Some(1), None),
        ];
        let shown = |claims: &[Claim]| {
            shown_fences(claims, &folder(procs.path()), hosts.path(), logs.path())
        };
        assert_eq!(
            shown(&claims),
            BTreeMap::from([("s-own".to_owned(), Kind::Fence)]),
            "neither a hosted session nor one whose owner isn't proved"
        );
        let twice = [
            claim("s-own", 58, Some(500), None),
            claim("s-own", 58, Some(500), None),
        ];
        assert!(
            shown(&twice).is_empty(),
            "nor any where who owns a session can't be told"
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

    #[test]
    fn a_command_that_left_pi_behind_still_runs_as_an_agent() {
        // Each forked twice, so pi is no longer among its forebears.
        let procs = procs(&[
            (40, "sh", 1, "agents.slice/pi-host-work-attended.service"),
            (41, "sh", 1, "session.slice/quickshell.service"),
            (42, "sh", 1, "session.slice/quickshell.service"),
            (43, "sh", 1, "session.slice/quickshell.service"),
        ]);
        let processes = folder(procs.path());
        assert_eq!(
            judge(Some(40), &processes, &[]),
            By::Agent(Kind::Strict),
            "one an agent's unit holds, below no session, runs strictly fenced"
        );
        assert_eq!(
            unclaimed_because(Some(40), &processes),
            "an agent's unit, or pi or a pi host above it, runs it"
        );
        let status = procs.path().join("41").join("status");
        let said = fs::read_to_string(&status).expect("its status");
        fs::write(&status, format!("{said}NoNewPrivs:\t1\n")).expect("it is barred");
        assert_eq!(
            judge(Some(41), &processes, &[]),
            By::Agent(Kind::Strict),
            "one barred from gaining privileges, as a fence bars it, runs strictly fenced"
        );
        assert_eq!(
            unclaimed_because(Some(41), &processes),
            "it is barred from gaining privileges, as a fence bars it"
        );
        assert_eq!(
            judge(Some(42), &processes, &[Some(Kind::None), Some(Kind::Fence)]),
            By::Agent(Kind::Fence),
            "one below sessions runs in the strictest fence they claim, at least the agent fence"
        );
        assert_eq!(
            judge(Some(42), &processes, &[Some(Kind::Fence), None]),
            By::Agent(Kind::Strict),
            "a session whose fence can't be read claims the strict one"
        );
        assert_eq!(judge(Some(43), &processes, &[]), By::Person);
        assert_eq!(
            judge(None, &processes, &[]),
            By::Agent(Kind::Strict),
            "one the kernel doesn't name"
        );
        assert_eq!(
            unclaimed_because(None, &processes),
            "the kernel names no process at it"
        );
        assert_eq!(
            judge(Some(44), &processes, &[]),
            By::Agent(Kind::Strict),
            "one nothing can be read of"
        );
        assert_eq!(
            unclaimed_because(Some(44), &processes),
            "nothing can be read of it or a forebear, so it can't be told"
        );
    }

    #[test]
    fn a_start_is_relayed_only_from_the_node_or_a_process_judged_the_persons() {
        let procs = procs(&[
            (40, "mycelium", 39, "session.slice/sshd-login.scope"),
            (39, "sshd", 1, "session.slice/sshd-login.scope"),
            (
                41,
                "mycelium",
                2,
                "agents.slice/pi-host-work-attended.service",
            ),
            (42, "mycelium", 48, "session.slice/kitty-2830-4.scope"),
            (48, "pi", 1, "session.slice/kitty-2830-4.scope"),
            (43, "mycelium", 1, "session.slice/quickshell.service"),
        ]);
        fs::create_dir(procs.path().join("self")).expect("the node's folder is made");
        let status = procs.path().join("43").join("status");
        let said = fs::read_to_string(&status).unwrap_or_default();
        fs::write(&status, format!("{said}NoNewPrivs:\t1\n")).expect("it is barred");
        let kernel = folder(procs.path());
        let own = i32::try_from(std::process::id()).expect("a pid");
        let judged =
            |pid: Option<i32>, processes: &Processes| relays(pid, judge(pid, processes, &[]));
        assert!(
            judged(Some(40), &kernel),
            "a link's end an ssh login runs relays"
        );
        assert!(
            !judged(Some(40), &kernel.clone().unattended()),
            "but not where no person works, since an agent's ssh login lands so too"
        );
        assert!(
            !relays(Some(40), judge(Some(40), &kernel, &[None])),
            "nor one a session claims"
        );
        for processes in [kernel.clone(), kernel.unattended()] {
            assert!(judged(Some(own), &processes), "the node's own relays");
            assert!(!judged(Some(41), &processes), "an agent's unit doesn't");
            assert!(!judged(Some(42), &processes), "nor one pi runs");
            assert!(!judged(Some(43), &processes), "nor one a fence bars");
            assert!(!judged(None, &processes), "nor one nothing names");
        }
    }

    #[test]
    fn a_command_that_sees_other_mounts_than_the_node_runs_as_an_agent() {
        let procs = procs(&[
            (41, "sh", 1, "session.slice/quickshell.service"),
            (42, "sh", 1, "session.slice/quickshell.service"),
        ]);
        let mounts = |process: &str, mounts: &str| {
            let ns = procs.path().join(process).join("ns");
            fs::create_dir_all(&ns).expect("its folder is made");
            std::os::unix::fs::symlink(mounts, ns.join("mnt")).expect("its mounts are told");
        };
        mounts("self", "mnt:[4026531832]");
        mounts("41", "mnt:[4026532901]");
        mounts("42", "mnt:[4026531832]");
        let processes = folder(procs.path());
        assert_eq!(judge(Some(41), &processes, &[]), By::Agent(Kind::Strict));
        assert_eq!(judge(Some(42), &processes, &[]), By::Person);
    }

    #[test]
    fn where_no_person_works_the_one_taken_for_the_person_is_an_agent() {
        // The node itself runs barred from gaining privileges, as a
        // basin's exec runs it, so the asker barred too gains no less.
        let procs = procs(&[(41, "sh", 1, "session.slice/quickshell.service")]);
        fs::create_dir(procs.path().join("self")).expect("the node's folder is made");
        for process in ["41", "self"] {
            let status = procs.path().join(process).join("status");
            let said = fs::read_to_string(&status).unwrap_or_default();
            fs::write(&status, format!("{said}NoNewPrivs:\t1\n")).expect("it is barred");
        }
        let kernel = folder(procs.path());
        assert_eq!(
            judge(Some(41), &kernel, &[]),
            By::Person,
            "the kernel alone can't tell it from the person"
        );
        let unattended = kernel.clone().unattended();
        assert_eq!(
            judge(Some(41), &unattended, &[]),
            By::Agent(Kind::Strict),
            "where no person works, it runs strictly fenced"
        );
        assert_eq!(
            judge(Some(41), &unattended, &[Some(Kind::Fence)]),
            By::Agent(Kind::Fence),
            "a session claimed above it keeps its fence, as it would where a person works"
        );
        assert!(
            may_say(
                Some(41),
                &unattended,
                &json!({"act": "pause", "from": "person"})
            )
            .is_err_and(|why| why.contains(UNATTENDED)),
            "it can't speak as the person"
        );
        assert!(
            persons_own(Some(41), &unattended, "free a quest")
                .is_err_and(|why| why.contains(UNATTENDED)),
            "nor do what only the person's own login does"
        );
        assert_eq!(
            unattended.kernel(),
            &kernel,
            "what else the kernel tells is read as before"
        );
        assert_eq!(
            unattended.clone().unattended(),
            unattended,
            "said twice, it is said once"
        );
    }
}
