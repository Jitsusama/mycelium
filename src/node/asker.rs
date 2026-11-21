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
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

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

    /// The session the host at process `host`, by its id and its start in
    /// ticks after boot, registered the spawner below it, in the lineage
    /// of `asker`, by its id and its start as read when the node took its
    /// connection, for,
    /// as [`Spawners::register`] took the host's word.
    ///
    /// A spawner's own process, its name or its command line, proves
    /// nothing: the host runs some commands an agent chose as its own
    /// children, which may run pi-spawner naming any session. So only
    /// the host's word counts, kept by the spawner's id and start. The
    /// host's and the asker's starts are read before and after, so a host
    /// gone and its id reused names nothing, nor does a connection held
    /// open past its process's end once another takes its id; and each
    /// process from the asker up started no earlier than its parent, so a
    /// forebear's id reused while the line was read breaks it.
    fn spawned_for(
        &self,
        asker: (i32, u64),
        host: (i32, u64),
        spawners: &Spawners,
    ) -> Option<String> {
        let Self::Folder(procs) = self else {
            return None;
        };
        // Read at each proof, not only as the host said it, since a spawner
        // taken while tracing was shut proves nothing once it opens.
        if tracing_open(procs).is_some() {
            return None;
        }
        let unchanged =
            || started(procs, host.0) == Some(host.1) && started(procs, asker.0) == Some(asker.1);
        let lineage = self.lineage(asker.0);
        let at = lineage.iter().position(|process| *process == host.0)?;
        let spawner = *lineage.get(at.checked_sub(1)?)?;
        let starts: Vec<u64> = lineage
            .get(..=at)?
            .iter()
            .map(|process| started(procs, *process))
            .collect::<Option<_>>()?;
        let in_order = starts.windows(2).all(|pair| pair[0] >= pair[1]);
        if !unchanged() || !in_order || starts.first() != Some(&asker.1) {
            return None;
        }
        let session = spawners.serves((spawner, starts[at - 1]), host)?;
        (started(procs, spawner) == Some(starts[at - 1]) && unchanged()).then_some(session)
    }

    /// Whether this node would take pi host `host`'s word for a spawner:
    /// only on a machine whose process folder it reads, neither a Mac nor
    /// one run unattended, and there only while none may trace another of
    /// its user and the host's inspector can't be opened. Where such a
    /// machine's node wouldn't, no spawner is ever said for the host, so
    /// what runs below one is told the host's one session, as before
    /// spawners were said; a Mac's sessions are told none.
    fn takes_word_of(&self, host: i32) -> bool {
        let Self::Folder(procs) = self else {
            return false;
        };
        tracing_open(procs).is_none() && inspector_open(procs, host).is_none()
    }

    /// Whether process `asker` runs below a `pi-spawner` that is a child of
    /// process `host`, read from the kernel, where only a process's own
    /// threads may change its name. None where it can't be told: the
    /// lineage, read again, no longer reaches the host, or that child's
    /// name can't be read. On a Mac or a machine run unattended, where no
    /// spawner is ever said, it is below none. It is asked only where this
    /// node takes the host's word, as [`Self::takes_word_of`] says what is
    /// told elsewhere.
    fn below_a_spawner(&self, asker: i32, host: i32) -> Option<bool> {
        let Self::Folder(procs) = self else {
            return Some(false);
        };
        let lineage = self.lineage(asker);
        let at = lineage.iter().position(|process| *process == host)?;
        let Some(child) = at.checked_sub(1).and_then(|below| lineage.get(below)) else {
            // The host asks for itself.
            return Some(false);
        };
        let status = fs::read_to_string(procs.join(child.to_string()).join("status")).ok()?;
        named(&status).map(|(name, _)| name == SPAWNER)
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
/// nothing past each command it runs. A pi host's environment and
/// commands are those of every session it holds, so a hosted session's
/// fence is the one its host's record names for it, as [`hosted_fence`]
/// reads it.
///
/// # Errors
///
/// Where who owns `session` can't be proved, or its process read, as on a
/// Mac, or its host's record names no fence for it, since then the fence
/// it runs in can't be told.
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
    let ticks = claim.start_ticks.ok_or_else(unproved)?;
    if let Some(host) = &claim.host {
        let beside = Beside {
            tree: &Tree::of(procs),
            owners: &owners,
            weighs_bubblewrap: true,
        };
        return hosted_fence(procs, hosts, host, pid, ticks, session, &beside);
    }
    shown_of(procs, &Tree::of(procs), pid, ticks, session)
}

/// The fence each session here shows it runs in, as [`shown_fence`] tells
/// it, for every session whose owner is proved and holds no other, read
/// with one look through the machine's processes. None where who owns
/// what can't be told. A bubblewrap below a pi host whose record names
/// another session isn't weighed here: what this look finds is kept as
/// the strictest each session was seen in, for good, and a sibling's
/// bubblewrap outliving its session, or running before it is proved,
/// would keep a session read fenced that isn't. A carry's own look
/// weighs it ([`shown_fence`]). Below a host whose record names this
/// session alone it is weighed, since no sibling can explain it, so a
/// record forged for a pi that hosts nothing is kept fenced.
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
    let beside = Beside {
        tree: &tree,
        owners: &owners,
        weighs_bubblewrap: false,
    };
    owners
        .iter()
        .filter_map(|(session, pid)| {
            let claim = claims.iter().find(|claim| &claim.session == session)?;
            let ticks = claim.start_ticks?;
            let shown = match &claim.host {
                Some(host) => hosted_fence(procs, hosts, host, *pid, ticks, session, &beside),
                None => shown_of(procs, &tree, *pid, ticks, session),
            };
            Some((session.clone(), shown.ok()?))
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
    let began = began_in(procs, pid, session)?;
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

/// The fence process `pid`, owning `session`, names in `PI_SANDBOX` as
/// its environment began: none where it names none or a word no fence
/// is.
fn began_in(procs: &Path, pid: i32, session: &str) -> Result<Kind, String> {
    let environ = fs::read(procs.join(pid.to_string()).join("environ")).map_err(|_| {
        format!("{session}'s process can't be read, so the fence it runs in can't be told")
    })?;
    Ok(environ
        .split(|byte| *byte == 0)
        .find_map(|pair| pair.strip_prefix(SANDBOX))
        .map_or(Kind::None, |value| match value.trim_ascii() {
            b"strict" => Kind::Strict,
            b"fence" => Kind::Fence,
            _ => Kind::None,
        }))
}

/// The most of a pi host's record read, so a record grown past any a
/// host writes holds no carry up.
const HOST_RECORD_MOST: u64 = 1 << 20;

/// The fence the pi host keyed `key`, run by process `pid` started
/// `ticks` after boot, names for `session` in its record in `hosts`, as
/// the harness writes it there and rewrites it as the session's fence
/// changes: `none`, `fence` or `strict`, and never looser than the
/// host's own `PI_SANDBOX`, so a record alone can't read a session out
/// of the fence its host began in. A record that names none for it,
/// or a word no harness writes, says nothing, so the fence can't be told,
/// never guessed looser; nor can a record read as [`host_record`]
/// refuses, or once `pid` is another process, read again last. Where
/// `beside` weighs it, a bubblewrap below the host reads it at least the
/// ordinary fence, as for a pi of its own, unless another session proved
/// on that host is named `fence` in the record, the one fence that starts
/// a bubblewrap, so a record forged for a pi that hosts nothing can't
/// read its session out of the fence below it. A record naming no
/// session but this one has it weighed whatever `beside` says, since
/// then no sibling's bubblewrap can be the one below.
fn hosted_fence(
    procs: &Path,
    hosts: &Path,
    key: &str,
    pid: i32,
    ticks: u64,
    session: &str,
    beside: &Beside<'_>,
) -> Result<Kind, String> {
    let untold = |why: &str| {
        format!(
            "{session} runs in a pi host whose record {why}, so the fence it runs in can't be told"
        )
    };
    let record = host_record(hosts, pid, ticks).map_err(|why| untold(&why))?;
    if record.get("key").and_then(Value::as_str) != Some(key)
        || record.pointer("/owner/pid").and_then(Value::as_i64) != Some(i64::from(pid))
        || record.pointer("/owner/startTicks").and_then(Value::as_u64) != Some(ticks)
    {
        return Err(untold("is another host's"));
    }
    let said = record
        .get("sessions")
        .and_then(Value::as_array)
        .and_then(|sessions| {
            sessions
                .iter()
                .find(|one| one.get("id").and_then(Value::as_str) == Some(session))
        })
        .ok_or_else(|| untold("doesn't hold it"))?;
    let kind = match said.get("fence").and_then(Value::as_str) {
        Some("none") => Kind::None,
        Some("fence") => Kind::Fence,
        Some("strict") => Kind::Strict,
        _ => return Err(untold("names no fence for it")),
    };
    let fenced_beside = record
        .get("sessions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|one| one.get("fence").and_then(Value::as_str) == Some("fence"))
        .filter_map(|one| one.get("id").and_then(Value::as_str))
        .any(|other| {
            other != session
                && beside
                    .owners
                    .iter()
                    .any(|(owned, by)| owned == other && *by == pid)
        });
    let alone = record
        .get("sessions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .all(|one| one.get("id").and_then(Value::as_str) == Some(session));
    let kind = kind.max(began_in(procs, pid, session)?);
    let weighs = beside.weighs_bubblewrap || alone;
    let kind = if weighs && !fenced_beside && beside.tree.runs_below(pid, BWRAP) {
        kind.max(Kind::Fence)
    } else {
        kind
    };
    // Read again last, so the record and environment read were that
    // process's.
    if started(procs, pid) != Some(ticks) {
        return Err(format!(
            "{session}'s pi host {pid} ended while its record was read, so the fence it runs \
             in can't be told"
        ));
    }
    Ok(kind)
}

/// What a hosted session's fence is weighed beside: the machine's
/// processes, and the sessions proved here with the process that owns
/// each.
struct Beside<'a> {
    tree: &'a Tree,
    owners: &'a [(String, i32)],
    /// Whether a bubblewrap below the host is weighed, as a carry's own
    /// look weighs it and the look kept for good doesn't, unless the
    /// host's record names no other session.
    weighs_bubblewrap: bool,
}

/// The text of a file in the hosts folder, opened without blocking and
/// read no further than any host writes, as [`host_record`] reads it,
/// so a pipe or an outsized file put there holds nothing up. None for
/// anything but a file, or one that can't be read.
fn host_file(path: &Path) -> Option<String> {
    use rustix::fs::{Mode, OFlags};
    use std::io::Read as _;
    let file = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()
    .map(fs::File::from)?;
    if !file.metadata().is_ok_and(|written| written.is_file()) {
        return None;
    }
    let mut text = String::new();
    file.take(HOST_RECORD_MOST).read_to_string(&mut text).ok()?;
    Some(text)
}

/// The record the pi host run by process `pid`, started `ticks` after
/// boot, keeps in `hosts` as `<pid>-<ticks>.json`, read only where
/// nothing but this account could have put it there: `hosts` reached
/// through no link, a folder of this account's that no group or other
/// account may write, and the record itself a file of this account's,
/// readable and writable by it alone, as the harness writes it, opened
/// without following a link. The harness names a session's fence in
/// its record only where no fenced command could have replaced that
/// record, so a record a command could have written names none and the
/// fence reads as untold.
fn host_record(hosts: &Path, pid: i32, ticks: u64) -> Result<Value, String> {
    use rustix::fs::{Mode, OFlags};
    use std::io::Read as _;
    use std::os::unix::fs::MetadataExt as _;
    let own = rustix::process::geteuid().as_raw();
    if fs::canonicalize(hosts).ok().as_deref() != Some(hosts) {
        return Err("folder is reached through a link, or isn't there".to_owned());
    }
    let folder = fs::symlink_metadata(hosts).map_err(|_| "folder can't be read".to_owned())?;
    if !folder.is_dir() || folder.uid() != own || folder.mode() & 0o022 != 0 {
        return Err("folder isn't this account's alone to write".to_owned());
    }
    // Opened without blocking, too, so one put there as a pipe holds
    // nothing up before it is seen to be no file.
    let file = rustix::fs::open(
        hosts.join(format!("{pid}-{ticks}.json")),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(fs::File::from)
    .map_err(|_| "can't be opened as a file".to_owned())?;
    let written = file.metadata().map_err(|_| "can't be read".to_owned())?;
    if !written.is_file() || written.uid() != own || written.mode() & 0o777 != 0o600 {
        return Err("isn't a file this account alone may read and write".to_owned());
    }
    let mut text = String::new();
    file.take(HOST_RECORD_MOST)
        .read_to_string(&mut text)
        .map_err(|_| "can't be read".to_owned())?;
    serde_json::from_str(&text).map_err(|_| "can't be read as a record".to_owned())
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

/// How many spawners are kept at most, so hosts that come and go can't
/// grow the node without end: those gone are let go first.
const SPAWNERS_KEPT: usize = 4096;

/// How long a reason a host's word was refused for goes unsaid once said,
/// so a host refused again later, after its unit was mended, is said again.
const REFUSAL_SAID_AGAIN: Duration = Duration::from_secs(60 * 60);

/// How many reasons a host's word was refused for are kept, all forgotten
/// once that many are.
const REFUSALS_KEPT: usize = 256;

/// Where the kernel says who may trace a process of the same user.
const PTRACE_SCOPE: &str = "sys/kernel/yama/ptrace_scope";

/// The node option that keeps SIGUSR1, which any process of the user may
/// send, from opening node's inspector.
const NO_SIGUSR1: &str = "--disable-sigusr1";

/// The variables that would run code inside a pi host as its node
/// starts, so a host given any of them is refused, but for a blank
/// `NODE_OPTIONS`, which gives no option: node's own
/// options, where a require that finds nothing beside it looks (as a
/// library's optional require does), the compile cache whose code node
/// runs as the host's modules, the loader's libraries, and the modules
/// OpenSSL's configuration loads. A host's unit clears each.
const LOADS_CODE: [&str; 8] = [
    "NODE_OPTIONS",
    "NODE_PATH",
    "NODE_COMPILE_CACHE",
    "LD_PRELOAD",
    "LD_AUDIT",
    "LD_LIBRARY_PATH",
    "OPENSSL_CONF",
    "OPENSSL_MODULES",
];

/// Whether a node option, its underscores read as dashes and its quotes
/// and escapes dropped, opens node's inspector or lets SIGUSR1 open it
/// again, as `--no-disable-sigusr1` does after [`NO_SIGUSR1`].
fn opens_inspector(option: &str) -> bool {
    let option = option.replace(['"', '\\'], "").replace('_', "-");
    option.starts_with("--inspect") || option.starts_with("--no-disable-sigusr1")
}

/// Why a process here might trace another of its user, if it might: a
/// pi host's word is taken, and a spawner it said proves its session,
/// only while the kernel lets a process trace none but its descendants.
fn tracing_open(procs: &Path) -> Option<&'static str> {
    let scope = fs::read_to_string(procs.join(PTRACE_SCOPE))
        .ok()
        .and_then(|scope| scope.trim().parse::<u8>().ok());
    match scope {
        None => Some("this machine doesn't say who may trace a process"),
        Some(0) => Some("any process here may trace another of its user"),
        Some(_) => None,
    }
}

/// Why another process of its user could run code inside the pi host at
/// `pid` through node's inspector, if it could: unless node's first
/// option is [`NO_SIGUSR1`], as a host's unit starts it, a SIGUSR1 opens
/// it, and an option on its command line may open it outright. The
/// first, since an option taking a value would take one later as its
/// own. Any `NODE_OPTIONS` but a blank one is refused, since besides
/// opening the inspector, `--require` or `--import` there runs code as
/// the host before it starts; a host's unit clears it. So is any
/// other [`LOADS_CODE`]
/// variable, which loads a library's or a module's code into the host as
/// it starts.
/// An environment that can't be read is taken as one that might hold
/// some.
fn inspector_open(procs: &Path, pid: i32) -> Option<String> {
    let process = procs.join(pid.to_string());
    let Ok(line) = fs::read(process.join("cmdline")) else {
        return Some(format!("process {pid}'s command line can't be read"));
    };
    let words: Vec<String> = line
        .split(|byte| *byte == 0)
        .skip(1)
        .map(|word| String::from_utf8_lossy(word).into_owned())
        .collect();
    let closed = words
        .first()
        .is_some_and(|word| word.replace('_', "-") == NO_SIGUSR1);
    if !closed {
        return Some(format!(
            "process {pid} runs node without {NO_SIGUSR1} as its first option, so any \
             process of its user may open its inspector"
        ));
    }
    let Ok(environ) = fs::read(process.join("environ")) else {
        return Some(format!(
            "process {pid}'s environment can't be read, so what it loads can't be told"
        ));
    };
    for name in LOADS_CODE {
        let prefix = format!("{name}=");
        let given = environ
            .split(|byte| *byte == 0)
            .filter_map(|pair| pair.strip_prefix(prefix.as_bytes()))
            // A blank NODE_OPTIONS gives node no option. A blank path is
            // another matter: node and the loader take " " as a folder
            // beside them, and the loader an empty entry as the working
            // folder.
            .any(|value| name != "NODE_OPTIONS" || !value.iter().all(u8::is_ascii_whitespace));
        if given {
            return Some(format!(
                "process {pid} is given {name}, which could run code in it; a pi host \
                 takes it from the shell or service manager that started it"
            ));
        }
    }
    words
        .iter()
        .any(|word| opens_inspector(word))
        .then(|| format!("process {pid} runs node with an option that opens its inspector"))
}

/// What a pi host said of a spawner it started: itself, by its id and
/// its start in clock ticks, and the session the spawner serves.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Registered {
    host: (i32, u64),
    session: String,
}

/// Each hosted session's spawners, by each spawner's id and start, as
/// their host said them, for as long as the node runs. A session may
/// have more than one, since a host starts another for a session's next
/// quest while the last finishes what it runs.
#[derive(Debug)]
pub struct Spawners {
    kept: Mutex<HashMap<(i32, u64), Registered>>,
    /// How many it keeps at most.
    room: usize,
    /// Why a host's word for a spawner was refused, by the reason with its
    /// digits aside, and when it was last said, kept apart from the runs'
    /// reasons so that no flood of these forgets those.
    refused: Mutex<HashMap<String, Instant>>,
}

impl Default for Spawners {
    fn default() -> Self {
        Self::with_room(SPAWNERS_KEPT)
    }
}

impl Spawners {
    /// None yet, with room for `room`.
    fn with_room(room: usize) -> Self {
        Self {
            kept: Mutex::default(),
            room,
            refused: Mutex::default(),
        }
    }

    /// Whether a host's word for a spawner refused for `why` at `now` is
    /// to be said: once for each reason, whichever processes it names, so
    /// speakers that come and go each say no more, and again once
    /// [`REFUSAL_SAID_AGAIN`] has passed, so a host refused again later
    /// is heard.
    pub fn first_refusal(&self, why: &str, now: Instant) -> bool {
        let kind: String = why.chars().filter(|c| !c.is_ascii_digit()).collect();
        let mut said = self.refused.lock().unwrap_or_else(PoisonError::into_inner);
        if said.len() >= REFUSALS_KEPT && !said.contains_key(&kind) {
            said.clear();
        }
        let due = said
            .get(&kind)
            .is_none_or(|at| now.saturating_duration_since(*at) >= REFUSAL_SAID_AGAIN);
        if due {
            said.insert(kind, now);
        }
        due
    }

    /// Takes the word of `host`, the process at the other end of the
    /// connection and its start as read when the node took it, that its child
    /// `spawner`, by its id and the start the host read before handing it
    /// any command, is the pi-spawner it started for `session`.
    ///
    /// The kernel names the host, so no command it runs can speak for it;
    /// and with ptrace limited to a process's own descendants, and node's
    /// inspector shut to SIGUSR1 and opened by none of its options, none
    /// can make the running host speak, so a machine where any process may
    /// trace another of its user is refused, as is a host whose inspector
    /// another could open. The host must prove `session` as its own, and
    /// the spawner must be its child, named pi-spawner, started when the
    /// host says. A spawner serves one session, and a session's spawners
    /// are one host's at a time, since a session proves under one host at
    /// a time: once it proves under another, that host's word is taken,
    /// though the first's spawners run on.
    ///
    /// That holds only while no command can change the host's code: one
    /// that can rewrite the harness a host loads, then end the host, has
    /// its code run as the host when the host starts again, which may say
    /// any spawner. So the word proves a session only where the host's
    /// sessions run their commands fenced from the harness's files, and
    /// where the host loads no extension its sessions can write, as a
    /// trusted project's own are today: no fence closes that.
    ///
    /// # Errors
    ///
    /// When any of that can't be shown, saying which.
    pub fn register(
        &self,
        host: (Option<i32>, Option<u64>),
        spawner: (i32, u64),
        session: &str,
        processes: &Processes,
        owners: &[(String, i32)],
        claims: &[Claim],
    ) -> Result<(), String> {
        let procs = match processes {
            Processes::Folder(procs) => procs,
            Processes::Unattended(_) => {
                return Err(
                    "this machine runs unattended, where every run is told its agent, so \
                     no spawner is told"
                        .into(),
                );
            }
            Processes::Listed => {
                return Err(
                    "this machine can't say when a process started, so no spawner is told".into(),
                );
            }
        };
        if let Some(why) = tracing_open(procs) {
            return Err(format!(
                "{why}, so a host's word can't be told from a command's"
            ));
        }
        let pid = host
            .0
            .ok_or("nothing names the process that says it, so no spawner is told")?;
        let ticks = host.1.ok_or_else(|| {
            format!("process {pid}'s start couldn't be read, so its spawner isn't told")
        })?;
        let hosts_it = owners
            .iter()
            .any(|(owned, owner)| owned == session && *owner == pid)
            && claims.iter().any(|claim| {
                claim.session == session
                    && claim.pid == Some(pid)
                    && claim.host.is_some()
                    && claim.start_ticks == Some(ticks)
            });
        if !hosts_it || started(procs, pid) != Some(ticks) {
            return Err(format!(
                "process {pid} is no pi host proving the session, so it can't say its spawner"
            ));
        }
        if let Some(why) = inspector_open(procs, pid) {
            return Err(format!("{why}, so its word can't be told from a command's"));
        }
        let status = fs::read_to_string(procs.join(spawner.0.to_string()).join("status")).map_err(
            |error| match error.kind() {
                std::io::ErrorKind::NotFound => format!("process {} has gone", spawner.0),
                kind => format!("process {} can't be read: {kind}", spawner.0),
            },
        )?;
        let child = named(&status).is_some_and(|(name, parent)| name == SPAWNER && parent == pid);
        if !child {
            return Err(format!(
                "process {} is no pi-spawner of the host's own",
                spawner.0
            ));
        }
        if started(procs, spawner.0) != Some(spawner.1) {
            return Err(format!(
                "process {} started at another time than the host says",
                spawner.0
            ));
        }
        // A spawner counts only while it and the host that said it run, so
        // one left behind by a host that ended holds nothing for long.
        let live = |spawner: &(i32, u64), said: &Registered| {
            started(procs, spawner.0) == Some(spawner.1)
                && started(procs, said.host.0) == Some(said.host.1)
        };
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let asked = Registered {
            host: (pid, ticks),
            session: session.to_owned(),
        };
        if let Some(said) = kept.get(&spawner) {
            return if *said == asked {
                Ok(())
            } else {
                Err(format!(
                    "process {} serves {} already",
                    spawner.0, said.session
                ))
            };
        }
        if kept.len() >= self.room {
            kept.retain(|spawner, said| live(spawner, said));
        }
        if kept.len() >= self.room {
            return Err("this node keeps as many spawners as it can".into());
        }
        kept.insert(spawner, asked);
        Ok(())
    }

    /// The session `host` registered `spawner` for, each by its id and
    /// start.
    fn serves(&self, spawner: (i32, u64), host: (i32, u64)) -> Option<String> {
        let kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        kept.get(&spawner)
            .filter(|said| said.host == host)
            .map(|said| said.session.clone())
    }
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
    let Some(record) = host_file(&hosts.join(format!("{pid}-{ticks}.json")))
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

/// The session whose work trees a run `asker` asks for, by its id and its
/// start as read when the node took its connection, may stand in, as
/// [`session_of`] tells
/// who asks, but only where the process proved to
/// own it owns no other: a pi host's sessions share one process, and
/// which of them a command runs for is only what its environment says,
/// which any command there may set. So where a host holds more than one
/// session, one is told to a fence only by the spawner the host said it
/// started for it, which the asker runs below (`Processes::spawned_for`,
/// [`Spawners`]), never by what its command names.
///
/// Below a spawner its host said, where tracing is shut when it asks and
/// the host proves a session, the spawner decides alone: its session is
/// told however many share the host. The host's command line and
/// environment are read as it says the spawner, not as a command asks,
/// and a saying refused leaves an earlier one standing; only code already
/// inside the host could open its inspector between. Elsewhere alone is
/// counted over
/// every claim here, proved or not, since a session whose claim doesn't
/// prove yet still shares its host's process: the nearest of the asker
/// and its forebears that any claim or owner names must be named by
/// exactly one, the claims naming the same host and the sessions its
/// host's own record lists counted with it, and that one must be proved.
///
/// # Errors
///
/// When nothing names the asker, no session here claims it or its
/// forebears, the nearest claimed is claimed by more than one, or its
/// claim doesn't prove; below a said spawner, when its session no longer
/// proves; and, where the host proves a session and this node takes its
/// word (`Processes::takes_word_of`), when the asker is below a spawner
/// it holds no word for, or whose line has changed since it was said or
/// since the node took the asker's connection, or its lineage can't be
/// read again, though the host proves just one.
pub fn session_alone(
    asker: (Option<i32>, Option<u64>),
    processes: &Processes,
    owners: &[(String, i32)],
    claims: &[Claim],
    hosts: &Path,
    spawners: &Spawners,
) -> Result<String, String> {
    let (Some(pid), asker_started) = asker else {
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
    // through a spawner of its own that the host said. The host's start
    // is as its proved claims say, since proving checked it.
    let host_started = claims
        .iter()
        .filter(|claim| claim.pid == Some(process) && claim.host.is_some())
        .filter(|claim| proved.contains(&claim.session.as_str()))
        .find_map(|claim| claim.start_ticks);
    // Below a spawner the host said, a command is told its session or
    // none, never another the host holds that proves alone.
    let spawned = host_started
        .zip(asker_started)
        .and_then(|(ticks, began)| processes.spawned_for((pid, began), (process, ticks), spawners));
    if let Some(session) = spawned {
        return if proved.contains(&session.as_str()) {
            Ok(session)
        } else {
            Err(format!(
                "this asks through a pi-spawner said for a session process {process} no longer \
                 proves"
            ))
        };
    }
    // One below a spawner whose word this node doesn't hold, as until the
    // host says it again once this node starts, is told none, though the
    // host holds only one: the spawner may serve a session gone since. So
    // is one whose lineage, read again, can't say whether it is below one.
    // A host that proves no session is told so instead, below.
    if !proved.is_empty() && !keys.is_empty() && processes.takes_word_of(process) {
        match processes.below_a_spawner(pid, process) {
            Some(false) => {}
            Some(true) => {
                return Err(format!(
                    "this asks below pi host process {process} through a pi-spawner this \
                     node can't tie to a session the host proves: it holds no word for \
                     the spawner, as where the host hasn't said it since this node \
                     started or its word was refused (the node's log says why), or a \
                     process on the line has changed since"
                ));
            }
            None => {
                return Err(format!(
                    "this asks below pi host process {process} by a lineage that, read \
                     again, can't say which of its pi-spawners it runs below"
                ));
            }
        }
    }
    let shared = sharing.max(proved.len());
    match proved.as_slice() {
        [one] if sharing <= 1 => Ok((*one).to_owned()),
        [] => Err(format!(
            "this asks below process {process}, whose session none proves"
        )),
        _ if !keys.is_empty() && processes.takes_word_of(process) => Err(format!(
            "this asks below pi host process {process}, which {shared} sessions share, \
             and below no pi-spawner this node ties to one of them"
        )),
        _ if !keys.is_empty() => Err(format!(
            "this asks below pi host process {process}, which {shared} sessions share, \
             and whose word for its pi-spawners this node can't take, as this \
             machine or that host stands"
        )),
        _ => Err(format!(
            "this asks from process {process}, which {shared} sessions share, so none is \
             proved alone"
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
        .filter_map(|path| host_file(&path))
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

    /// Writes, for process `pid` in `procs`, that it runs the pi host's
    /// script under node given `options`, in an environment holding
    /// `node_options` as its `NODE_OPTIONS` where any are given.
    fn runs_node(procs: &Path, pid: i32, options: &[&str], node_options: Option<&str>) {
        let process = procs.join(pid.to_string());
        let words: Vec<&str> = std::iter::once("node")
            .chain(options.iter().copied())
            .chain(["/x/scripts/pi-host.ts", "background"])
            .collect();
        fs::write(process.join("cmdline"), format!("{}\0", words.join("\0")))
            .expect("its command line is written");
        let environ = node_options.map_or_else(
            || "PATH=/bin\0".to_owned(),
            |options| format!("PATH=/bin\0NODE_OPTIONS={options}\0"),
        );
        fs::write(process.join("environ"), environ).expect("its environment is written");
    }

    /// How core starts a pi host's node: its inspector shut to SIGUSR1.
    const HOST_NODE: [&str; 2] = ["--disable-sigusr1", "--max-old-space-size=8192"];

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
        let alone = |asker: Option<i32>, claims: &[Claim], owners: &[(String, i32)]| {
            let began = asker.and_then(|pid| super::started(procs.path(), pid));
            session_alone(
                (asker, began),
                &processes,
                owners,
                claims,
                hosts.path(),
                &Spawners::default(),
            )
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

    /// Processes as a host holds them: host 78 and its spawners, each
    /// with what it runs, a pi 69 that is no host, and another host 79.
    /// Each starts after its parent, its ticks its id, but 86 and 83,
    /// which started later than any, and the hosts and the pi, at 1.
    fn a_host_and_its_spawners() -> tempfile::TempDir {
        let host = "agents.slice/pi-host-work-background.service";
        let processes = [
            (78, "node", 1, host),
            (
                79,
                "node",
                1,
                "agents.slice/pi-host-other-background.service",
            ),
            (98, "pi-spawner", 79, "pi-spawner-f.scope"),
            (88, "pi-spawner", 78, "pi-spawner-b.scope"),
            (89, "bash", 88, "pi-spawner-b.scope"),
            (90, "mycelium", 89, "pi-spawner-b.scope"),
            (87, "pi-spawner", 78, "pi-spawner-a.scope"),
            (86, "mycelium", 87, "pi-spawner-a.scope"),
            (94, "pi-spawner", 89, "pi-spawner-b.scope"),
            (95, "mycelium", 94, "pi-spawner-b.scope"),
            (96, "pi-spawner", 78, host),
            (97, "mycelium", 96, host),
            (82, "pi-spawner", 78, "pi-spawner-d.scope"),
            (83, "mycelium", 82, "pi-spawner-d.scope"),
            (85, "bash", 78, host),
            (69, "pi", 1, "agents.slice/pi-a.scope"),
            (70, "pi-spawner", 69, "pi-spawner-e.scope"),
            (71, "mycelium", 70, "pi-spawner-e.scope"),
        ];
        let procs = procs(&processes);
        // The host and the pi started first, each process after its parent.
        for (pid, name, ..) in processes {
            started(
                procs.path(),
                pid,
                name,
                u64::try_from(pid).expect("a small id"),
            );
        }
        started(procs.path(), 78, "node", 1);
        started(procs.path(), 79, "node", 1);
        started(procs.path(), 69, "pi", 1);
        for (pid, name) in [(86, "mycelium"), (83, "mycelium")] {
            started(procs.path(), pid, name, 200);
        }
        for host in [78, 79] {
            runs_node(procs.path(), host, &HOST_NODE, None);
        }
        traced_by_forebears(procs.path(), "1");
        procs
    }

    #[test]
    fn a_hosts_session_is_proved_by_the_spawner_the_host_said_it_started_for_it() {
        let procs = a_host_and_its_spawners();
        let claims = [
            claim("s-one", 78, Some(1), Some("work")),
            claim("s-two", 78, Some(1), Some("work")),
            claim("s-three", 78, Some(1), Some("work")),
            claim("s-alone", 69, Some(1), None),
        ];
        let owners = vec![
            ("s-one".to_owned(), 78),
            ("s-two".to_owned(), 78),
            ("s-alone".to_owned(), 69),
        ];
        let processes = folder(procs.path());
        let spawners = Spawners::default();
        let says =
            |host: i32, spawner: i32, ticks: u64, session: &str, owners: &[(String, i32)]| {
                spawners.register(
                    (Some(host), Some(1)),
                    (spawner, ticks),
                    session,
                    &processes,
                    owners,
                    &claims,
                )
            };
        assert_eq!(
            says(78, 88, 88, "s-two", &owners),
            Ok(()),
            "the host says its own child serves s-two"
        );
        assert_eq!(
            says(78, 87, 87, "s-one", &owners),
            Ok(()),
            "and another serves s-one"
        );
        assert_eq!(
            says(78, 88, 88, "s-two", &owners),
            Ok(()),
            "saying it again changes nothing"
        );
        let hosts = tempfile::tempdir().expect("a folder for hosts' records");
        let connected = |asker: i32, began, owners: &[(String, i32)], claims: &[Claim]| {
            session_alone(
                (Some(asker), began),
                &processes,
                owners,
                claims,
                hosts.path(),
                &spawners,
            )
        };
        let alone = |asker: i32, owners: &[(String, i32)], claims: &[Claim]| {
            connected(asker, super::started(procs.path(), asker), owners, claims)
        };
        let refused = |asker, owners: &[(String, i32)], claims: &[Claim]| {
            alone(asker, owners, claims)
                .is_err_and(|why| why.contains("or a process on the line has changed"))
        };
        assert_eq!(
            alone(90, &owners, &claims),
            Ok("s-two".to_owned()),
            "the host said that spawner serves s-two, and nothing below it can say otherwise"
        );
        assert_eq!(
            alone(86, &owners, &claims),
            Ok("s-one".to_owned()),
            "and the other s-one, so two sessions of one host are told apart"
        );
        assert_eq!(
            alone(95, &owners, &claims),
            Ok("s-two".to_owned()),
            "a spawner started below a command proves nothing: only the one the host said does"
        );
        assert!(
            refused(97, &owners, &claims),
            "a pi-spawner the host started but never said, as a command it ran itself may be, \
             proves nothing"
        );
        assert!(
            connected(90, Some(89), &owners, &claims)
                .is_err_and(|why| why.contains("or a process on the line has changed")),
            "a connection held open past its process's end proves nothing once another takes \
             its id below a spawner"
        );
        let mut unhosted = owners.clone();
        unhosted.push(("s-beside".to_owned(), 69));
        assert!(
            says(69, 70, 70, "s-alone", &unhosted).is_err_and(|why| why.contains("no pi host")),
            "a pi that is no host has no spawner to say"
        );
        assert!(
            alone(71, &unhosted, &claims).is_err_and(|why| why.contains("proved alone")),
            "so none of its shared sessions is proved by one"
        );
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one said spawner's life, each step changing what the last left"
    )]
    fn a_said_spawner_proves_nothing_once_it_or_its_line_or_its_session_is_gone() {
        let procs = a_host_and_its_spawners();
        let claims = [
            claim("s-one", 78, Some(1), Some("work")),
            claim("s-two", 78, Some(1), Some("work")),
        ];
        let mut owners = vec![("s-one".to_owned(), 78), ("s-two".to_owned(), 78)];
        let processes = folder(procs.path());
        let spawners = Spawners::default();
        let says = |spawner: i32, session: &str, owners: &[(String, i32)]| {
            let ticks = u64::try_from(spawner).expect("a small id");
            let at = (spawner, ticks);
            spawners.register(
                (Some(78), Some(1)),
                at,
                session,
                &processes,
                owners,
                &claims,
            )
        };
        assert_eq!(says(88, "s-two", &owners), Ok(()), "one for s-two");
        assert_eq!(says(87, "s-one", &owners), Ok(()), "one for s-one");
        let hosts = tempfile::tempdir().expect("a folder for hosts' records");
        let refused = |asker: i32, owners: &[(String, i32)], claims: &[Claim]| {
            let alone = session_alone(
                (Some(asker), super::started(procs.path(), asker)),
                &processes,
                owners,
                claims,
                hosts.path(),
                &spawners,
            );
            alone.is_err_and(|why| why.contains("or a process on the line has changed"))
        };
        assert!(
            !refused(90, &owners, &claims),
            "the said spawner proves its session"
        );
        let mut restarted = claims.clone();
        for claim in &mut restarted {
            claim.start_ticks = claim.start_ticks.map(|ticks| ticks + 1);
        }
        assert!(
            refused(90, &owners, &restarted),
            "nor once the claims name the host at a start other than the one that said it"
        );
        started(procs.path(), 78, "node", 2);
        assert!(
            refused(90, &owners, &claims),
            "nor once the host that said it has gone and its id been given another"
        );
        started(procs.path(), 78, "node", 1);
        started(procs.path(), 89, "bash", 91);
        assert!(
            refused(90, &owners, &claims),
            "nor where a forebear started after the asker, its id reused while the line was read"
        );
        // Each below the spawner started after it, so only its own start
        // tells it from the one said.
        for (pid, name, ticks) in [
            (88, "pi-spawner", 92),
            (89, "bash", 93),
            (90, "mycelium", 94),
        ] {
            started(procs.path(), pid, name, ticks);
        }
        assert!(
            refused(90, &owners, &claims),
            "nor once the spawner said has gone and its id been given another"
        );
        for (pid, name, ticks) in [
            (88, "pi-spawner", 88),
            (89, "bash", 89),
            (90, "mycelium", 90),
        ] {
            started(procs.path(), pid, name, ticks);
        }
        assert!(!refused(90, &owners, &claims), "and proves again as it was");
        traced_by_forebears(procs.path(), "0");
        assert!(
            session_alone(
                (Some(90), super::started(procs.path(), 90)),
                &processes,
                &owners,
                &claims,
                hosts.path(),
                &spawners,
            )
            .is_err_and(|why| why.contains("2 sessions share")
                && why.contains("whose word for its pi-spawners this node can't take")),
            "nor once any process may trace another of its user, though it was said \
             while none could: the host's sessions are told apart by nothing then"
        );
        traced_by_forebears(procs.path(), "1");
        assert_eq!(
            says(96, "s-two", &owners),
            Ok(()),
            "a host may say another spawner for a session that has one, as for its next quest"
        );
        owners.retain(|(session, _)| session != "s-one");
        let unproved = |claims: &[Claim]| {
            session_alone(
                (Some(86), super::started(procs.path(), 86)),
                &processes,
                &owners,
                claims,
                hosts.path(),
                &spawners,
            )
            .is_err_and(|why| why.contains("no longer proves"))
        };
        assert!(
            unproved(&claims),
            "nor once the session it serves no longer proves"
        );
        assert!(
            unproved(&[claims[1].clone()]),
            "nor is a command below it told the host's one other session, once that alone \
             proves"
        );
    }

    #[test]
    fn a_spawner_the_node_holds_no_word_for_proves_no_session_though_its_host_holds_one() {
        let procs = a_host_and_its_spawners();
        let claims = [claim("s-one", 78, Some(1), Some("work"))];
        let owners = vec![("s-one".to_owned(), 78)];
        let processes = folder(procs.path());
        let hosts = tempfile::tempdir().expect("a folder for hosts' records");
        let alone = |asker: i32| {
            session_alone(
                (Some(asker), super::started(procs.path(), asker)),
                &processes,
                &owners,
                &claims,
                hosts.path(),
                &Spawners::default(),
            )
        };
        assert!(
            alone(83).is_err_and(|why| why.contains("it holds no word for the spawner")),
            "a command below a spawner never said, as after this node restarts, is told no \
             session, though the host holds only one"
        );
        // Our council, round 8 (2026-11-19): the spawner was blamed
        // where the host proves no session at all.
        assert_eq!(
            session_alone(
                (Some(83), super::started(procs.path(), 83)),
                &processes,
                &[],
                &claims,
                hosts.path(),
                &Spawners::default(),
            ),
            Err("this asks below process 78, whose session none proves".to_owned()),
            "a command below a host that proves none of its sessions is told so"
        );
        let plain = [claim("s-one", 78, Some(1), None)];
        assert_eq!(
            session_alone(
                (Some(83), super::started(procs.path(), 83)),
                &processes,
                &owners,
                &plain,
                hosts.path(),
                &Spawners::default(),
            ),
            Ok("s-one".to_owned()),
            "a pi that is no host, below the pi-spawner it runs its own commands through, is \
             told its session, since no host says a spawner for it"
        );
        assert_eq!(
            alone(85),
            Ok("s-one".to_owned()),
            "the host's own child, as it starts once a spawner is lost, is told its one session"
        );
        assert_eq!(
            session_alone(
                (Some(83), super::started(procs.path(), 83)),
                &processes.clone().unattended(),
                &owners,
                &claims,
                hosts.path(),
                &Spawners::default(),
            ),
            Ok("s-one".to_owned()),
            "on a machine whose node takes no spawner's word, as one run unattended, a \
             command below a spawner is told its host's one session, as before spawners \
             were said"
        );
        assert_eq!(
            processes.below_a_spawner(83, 999),
            None,
            "a lineage that no longer reaches the host, as when it changed since it was first \
             read, says nothing of a spawner, rather than that there is none"
        );
        assert_eq!(
            processes.below_a_spawner(85, 78),
            Some(false),
            "the host's own child is below no spawner"
        );
        runs_node(procs.path(), 78, &["--max-old-space-size=8192"], None);
        assert_eq!(
            alone(83),
            Ok("s-one".to_owned()),
            "below a host whose inspector may be opened, whose word this node never takes, \
             a command is told its host's one session, as before spawners were said"
        );
        runs_node(procs.path(), 78, &HOST_NODE, None);
        traced_by_forebears(procs.path(), "0");
        assert_eq!(
            alone(83),
            Ok("s-one".to_owned()),
            "nor where any process may trace another, where no host's word is taken"
        );
    }

    #[test]
    fn a_session_takes_its_new_hosts_spawner_though_its_old_hosts_runs() {
        let procs = a_host_and_its_spawners();
        let processes = folder(procs.path());
        let spawners = Spawners::default();
        let first = [claim("s-one", 78, Some(1), Some("work"))];
        let first_owners = [("s-one".to_owned(), 78)];
        assert_eq!(
            spawners.register(
                (Some(78), Some(1)),
                (87, 87),
                "s-one",
                &processes,
                &first_owners,
                &first,
            ),
            Ok(()),
            "the first host's spawner"
        );
        let moved = [claim("s-one", 79, Some(1), Some("other"))];
        let moved_owners = [("s-one".to_owned(), 79)];
        assert_eq!(
            spawners.register(
                (Some(79), Some(1)),
                (98, 98),
                "s-one",
                &processes,
                &moved_owners,
                &moved,
            ),
            Ok(()),
            "once the session proves under another host, the old host's spawner holds it no \
             more"
        );
    }

    #[test]
    fn a_host_is_heard_only_of_its_own_spawner_for_a_session_it_proves() {
        let procs = a_host_and_its_spawners();
        let claims = [
            claim("s-one", 78, Some(1), Some("work")),
            claim("s-two", 78, Some(1), Some("work")),
        ];
        let owners = [("s-one".to_owned(), 78), ("s-two".to_owned(), 78)];
        let processes = folder(procs.path());
        let spawners = Spawners::default();
        let says = |host: i32, started: u64, spawner: i32, ticks: u64, session: &str| {
            spawners.register(
                (Some(host), Some(started)),
                (spawner, ticks),
                session,
                &processes,
                &owners,
                &claims,
            )
        };
        let refused = |said: Result<(), String>, why: &str| said.is_err_and(|it| it.contains(why));
        assert_eq!(says(78, 1, 88, 88, "s-two"), Ok(()), "its own spawner");
        assert!(
            refused(says(78, 1, 88, 88, "s-one"), "serves s-two"),
            "a spawner said for one session can't be said for another"
        );
        assert!(
            refused(says(78, 1, 94, 94, "s-one"), "no pi-spawner"),
            "the host can't say a pi-spawner that isn't its own child"
        );
        assert!(
            refused(says(78, 1, 85, 85, "s-one"), "no pi-spawner"),
            "nor a child of its own that isn't named pi-spawner"
        );
        assert!(
            refused(says(78, 1, 82, 81, "s-one"), "started at another time"),
            "nor one started other than when the host read it, its id given again since"
        );
        assert!(
            refused(says(78, 1, 82, 82, "s-three"), "no pi host"),
            "nor speak for a session it can't prove"
        );
        assert!(
            refused(says(87, 1, 88, 88, "s-two"), "no pi host"),
            "a process the host started can't speak for the host"
        );
        started(procs.path(), 78, "node", 2);
        assert!(
            refused(says(78, 1, 82, 82, "s-one"), "no pi host"),
            "nor a process that took the host's id since"
        );
        started(procs.path(), 78, "node", 1);
        assert_eq!(says(78, 1, 87, 87, "s-one"), Ok(()), "a spawner for s-one");
        assert!(
            refused(says(79, 1, 98, 98, "s-one"), "no pi host"),
            "another host's word for a session it doesn't prove is refused"
        );
        fs::remove_dir_all(procs.path().join("90")).ok();
        assert!(
            refused(says(78, 1, 90, 90, "s-one"), "has gone"),
            "nor a spawner that has gone"
        );
    }

    #[test]
    fn a_host_is_heard_only_where_the_kernel_names_it_and_tells_when_it_started() {
        let procs = a_host_and_its_spawners();
        let claims = [claim("s-one", 78, Some(1), Some("work"))];
        let owners = [("s-one".to_owned(), 78)];
        let processes = folder(procs.path());
        let spawners = Spawners::default();
        assert!(
            spawners
                .register(
                    (Some(78), None),
                    (82, 82),
                    "s-one",
                    &processes,
                    &owners,
                    &claims
                )
                .is_err_and(|why| why.contains("process 78's start couldn't be read")),
            "nor a host whose start the kernel named it without"
        );
        assert!(
            spawners
                .register(
                    (None, None),
                    (82, 82),
                    "s-one",
                    &processes,
                    &owners,
                    &claims
                )
                .is_err_and(|why| why.contains("nothing names the process")),
            "nor a speaker the kernel names not at all"
        );
        for (machine, why) in [
            (processes.clone().unattended(), "runs unattended"),
            (Processes::Listed, "can't say when a process started"),
        ] {
            assert!(
                spawners
                    .register(
                        (Some(78), Some(1)),
                        (82, 82),
                        "s-one",
                        &machine,
                        &owners,
                        &claims
                    )
                    .is_err_and(|it| it.contains(why)),
                "nor any host on a machine that {why}"
            );
        }
    }

    #[test]
    fn a_refusal_is_said_once_for_each_reason_until_an_hour_passes() {
        let spawners = Spawners::default();
        let now = Instant::now();
        assert!(
            spawners.first_refusal("process 78 is given NODE_PATH", now),
            "said the first time"
        );
        assert!(
            !spawners.first_refusal("process 4021 is given NODE_PATH", now),
            "not again for another process, so speakers that come and go say no more"
        );
        assert!(
            spawners.first_refusal("process 78 is given LD_AUDIT", now),
            "another reason is said"
        );
        assert!(
            spawners.first_refusal("process 91 is given NODE_PATH", now + REFUSAL_SAID_AGAIN),
            "and the first again an hour on, so a host refused later is heard"
        );
    }

    #[test]
    fn a_host_is_heard_only_where_no_other_process_can_make_it_speak() {
        let procs = a_host_and_its_spawners();
        let claims = [claim("s-one", 78, Some(1), Some("work"))];
        let owners = [("s-one".to_owned(), 78)];
        let processes = folder(procs.path());
        let spawners = Spawners::default();
        let refused = |why: &str| {
            spawners
                .register(
                    (Some(78), Some(1)),
                    (82, 82),
                    "s-one",
                    &processes,
                    &owners,
                    &claims,
                )
                .is_err_and(|it| it.contains(why))
        };
        let runs = |options: &[&str], node_options| {
            runs_node(procs.path(), 78, options, node_options);
        };
        runs(&["--max-old-space-size=8192"], None);
        assert!(
            refused("without --disable-sigusr1"),
            "a host whose inspector SIGUSR1 opens isn't heard, since any process of its user \
             may send it"
        );
        runs(&["--title", "--disable-sigusr1"], None);
        assert!(
            refused("without --disable-sigusr1 as its first option"),
            "nor one given it anywhere but first, where an option taking a value could \
             take it as its own"
        );
        runs(&["--disable_sigusr1", "--inspect=0"], None);
        assert!(
            refused("opens its inspector"),
            "nor one whose options open it outright"
        );
        runs(&["--disable-sigusr1", "--no_disable_sigusr1"], None);
        assert!(
            refused("opens its inspector"),
            "nor one whose options let SIGUSR1 open it again, written as node reads it"
        );
        runs(&["--disable-sigusr1", "\"--inspect\\_brk\""], None);
        assert!(
            refused("opens its inspector"),
            "nor one whose option opening it is quoted or escaped"
        );
        runs(&HOST_NODE, None);
        traced_by_forebears(procs.path(), "0");
        assert!(
            refused("may trace"),
            "nor any host where any process may trace another of its user"
        );
        traced_by_forebears(procs.path(), "x");
        assert!(
            refused("doesn't say who may trace"),
            "nor where the kernel's word on it can't be read"
        );
        fs::remove_file(procs.path().join(PTRACE_SCOPE)).expect("it goes");
        assert!(
            refused("doesn't say who may trace"),
            "nor where the kernel says nothing of it"
        );
        traced_by_forebears(procs.path(), "1");
        assert!(!refused(""), "and heard once it runs as core starts it");
    }

    #[test]
    fn a_host_is_heard_only_where_its_environment_loads_no_code() {
        let procs = a_host_and_its_spawners();
        let claims = [claim("s-one", 78, Some(1), Some("work"))];
        let owners = [("s-one".to_owned(), 78)];
        let processes = folder(procs.path());
        let spawners = Spawners::default();
        let refused = |why: &str| {
            spawners
                .register(
                    (Some(78), Some(1)),
                    (82, 82),
                    "s-one",
                    &processes,
                    &owners,
                    &claims,
                )
                .is_err_and(|it| it.contains(why))
        };
        runs_node(procs.path(), 78, &HOST_NODE, Some("--require /tmp/x.js"));
        assert!(
            refused("given NODE_OPTIONS"),
            "nor one given any NODE_OPTIONS, which could run code in it"
        );
        for loads in [
            "NODE_PATH=/tmp",
            "NODE_COMPILE_CACHE=/tmp",
            "LD_PRELOAD=/tmp/x.so",
            "LD_AUDIT=/tmp/x.so",
            "LD_LIBRARY_PATH=/tmp",
            "OPENSSL_CONF=/tmp/x.cnf",
            "OPENSSL_MODULES=/tmp",
            "NODE_PATH= ",
            "LD_LIBRARY_PATH=",
            "OPENSSL_CONF= ",
        ] {
            fs::write(
                procs.path().join("78").join("environ"),
                format!("PATH=/bin\0{loads}\0"),
            )
            .expect("its environment is written");
            assert!(
                refused("which could run code in it"),
                "nor one given {loads}, which loads code into it as it starts"
            );
            assert!(
                refused("a pi host takes it from the shell or service manager that started it"),
                "and the refusal says where a host gets {loads}, so a host started by \
                 hand isn't chased"
            );
        }
        runs_node(procs.path(), 78, &HOST_NODE, Some(" "));
        assert!(
            spawners
                .register(
                    (Some(78), Some(1)),
                    (82, 82),
                    "s-one",
                    &processes,
                    &owners,
                    &claims,
                )
                .is_ok(),
            "though one whose NODE_OPTIONS its unit cleared is heard"
        );
        fs::remove_file(procs.path().join("78").join("environ")).expect("it goes");
        assert!(
            refused("environment can't be read"),
            "nor one whose environment can't be read"
        );
    }

    #[test]
    fn a_full_node_makes_room_only_from_spawners_that_have_ended() {
        let procs = a_host_and_its_spawners();
        let claims = [
            claim("s-one", 78, Some(1), Some("work")),
            claim("s-two", 78, Some(1), Some("work")),
        ];
        let owners = [("s-one".to_owned(), 78), ("s-two".to_owned(), 78)];
        let processes = folder(procs.path());
        let spawners = Spawners::with_room(2);
        let says = |spawner: i32, session: &str| {
            let at = (spawner, u64::try_from(spawner).expect("a small id"));
            spawners.register(
                (Some(78), Some(1)),
                at,
                session,
                &processes,
                &owners,
                &claims,
            )
        };
        assert_eq!(says(88, "s-two"), Ok(()), "one");
        assert_eq!(says(87, "s-one"), Ok(()), "and another fill it");
        assert_eq!(
            says(88, "s-two"),
            Ok(()),
            "one said again is kept as it was"
        );
        assert!(
            says(82, "s-one").is_err_and(|why| why.contains("as many spawners as it can")),
            "a third is refused while both run"
        );
        started(procs.path(), 88, "pi-spawner", 92);
        assert_eq!(says(82, "s-one"), Ok(()), "and taken once one has ended");
        let moved = [claim("s-two", 79, Some(1), Some("other"))];
        let moved_owners = [("s-two".to_owned(), 79)];
        assert_eq!(
            spawners.register(
                (Some(79), Some(1)),
                (98, 98),
                "s-two",
                &processes,
                &moved_owners,
                &moved,
            ),
            Err("this node keeps as many spawners as it can".to_owned()),
            "a full node takes no more while each kept spawner and its host run"
        );
        started(procs.path(), 78, "node", 5);
        assert_eq!(
            spawners.register(
                (Some(79), Some(1)),
                (98, 98),
                "s-two",
                &processes,
                &moved_owners,
                &moved,
            ),
            Ok(()),
            "and makes room once the host that said them has ended, though its spawners run"
        );
    }

    /// Writes, in `procs`, who the kernel lets trace a process of the same
    /// user: `0` any of them, `1` only its forebears.
    fn traced_by_forebears(procs: &Path, scope: &str) {
        let file = procs.join(PTRACE_SCOPE);
        fs::create_dir_all(file.parent().expect("its folder")).expect("its folder is made");
        fs::write(file, scope).expect("the scope is written");
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
    /// holding `sessions`, each with the fence it names, if any, readable
    /// and writable by this account alone, as the harness writes it.
    fn hosts_in_58(hosts: &Path, sessions: &[(&str, Option<&str>)]) {
        use std::os::unix::fs::PermissionsExt as _;
        let sessions: Vec<Value> = sessions
            .iter()
            .map(|(id, fence)| match fence {
                Some(fence) => json!({"id": id, "state": "running", "fence": fence}),
                None => json!({"id": id, "state": "running"}),
            })
            .collect();
        let record = hosts.join("58-500.json");
        fs::write(
            &record,
            json!({"key": "work-background",
                   "owner": {"pid": 58, "startedAt": 1, "startTicks": 500},
                   "sessions": sessions})
            .to_string(),
        )
        .expect("its host's record is written");
        fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).expect("its own");
    }

    /// The fence `s-own`, held by the host `work-background` that pi 58
    /// runs, shows it runs in, its host's records read from `hosts`.
    fn shown_hosted(
        hosted: &(tempfile::TempDir, tempfile::TempDir, tempfile::TempDir),
        hosts: &Path,
    ) -> Result<Kind, String> {
        shown_hosted_beside(hosted, hosts, &[])
    }

    /// The fence `s-own` shows it runs in, as [`shown_hosted`], with the
    /// sessions `beside` it claimed on the same host too.
    fn shown_hosted_beside(
        (procs, logs, _): &(tempfile::TempDir, tempfile::TempDir, tempfile::TempDir),
        hosts: &Path,
        beside: &[&str],
    ) -> Result<Kind, String> {
        let claims: Vec<Claim> = std::iter::once("s-own")
            .chain(beside.iter().copied())
            .map(|session| claim(session, 58, Some(500), Some("work-background")))
            .collect();
        shown_fence(&claims, &folder(procs.path()), hosts, logs.path(), "s-own")
    }

    #[test]
    fn a_hosted_session_shows_the_fence_its_hosts_record_names() {
        // Unfenced as the host began, and a bubblewrap below it for
        // another session, so only its record says how this one runs.
        let hosted = fenced_pi("", &[(71, "bwrap", 58)]);
        holds_log(hosted.0.path(), 58, hosted.1.path(), "s-other");
        let hosts = fs::canonicalize(hosted.2.path()).expect("its real path");
        for (word, kind) in [
            ("none", Kind::None),
            ("fence", Kind::Fence),
            ("strict", Kind::Strict),
        ] {
            hosts_in_58(&hosts, &[("s-own", Some(word)), ("s-other", Some("fence"))]);
            assert_eq!(
                shown_hosted_beside(&hosted, &hosts, &["s-other"]),
                Ok(kind),
                "its own, as its record names it: {word}"
            );
        }
        // A bubblewrap below a host that holds no other session proved
        // fenced is this session's, as a record forged for a pi that
        // hosts nothing would read it.
        hosts_in_58(
            &hosts,
            &[("s-own", Some("none")), ("s-other", Some("fence"))],
        );
        assert_eq!(
            shown_hosted(&hosted, &hosts),
            Ok(Kind::Fence),
            "a fenced session beside it that none proves explains nothing below"
        );
        hosts_in_58(&hosts, &[("s-own", Some("none"))]);
        assert_eq!(
            shown_hosted(&hosted, &hosts),
            Ok(Kind::Fence),
            "nor does a record naming it alone"
        );
        hosts_in_58(
            &hosts,
            &[("s-own", Some("none")), ("s-other", Some("none"))],
        );
        assert_eq!(
            shown_hosted_beside(&hosted, &hosts, &["s-other"]),
            Ok(Kind::Fence),
            "nor one beside it named unfenced"
        );
        let untold = |why: &str| {
            Err(format!(
                "s-own runs in a pi host whose record {why}, so the fence it runs in can't be \
                 told"
            ))
        };
        hosts_in_58(&hosts, &[("s-own", None), ("s-other", Some("strict"))]);
        assert_eq!(
            shown_hosted(&hosted, &hosts),
            untold("names no fence for it"),
            "one an older harness wrote, or the host couldn't say, is never guessed"
        );
        hosts_in_58(&hosts, &[("s-own", Some("loose"))]);
        assert_eq!(
            shown_hosted(&hosted, &hosts),
            untold("names no fence for it"),
            "nor one a word no harness writes names"
        );
        hosts_in_58(&hosts, &[("s-own", Some("none"))]);
        let record = hosts.join("58-500.json");
        let written = fs::read_to_string(&record).expect("its record reads");
        // A record naming another key or owner proves no host owns the
        // session, so it is refused before its fence is weighed.
        for (whose, other) in [
            ("key", written.replace("work-background", "work-attended")),
            ("owner", written.replace("\"pid\":58", "\"pid\":59")),
        ] {
            assert_ne!(other, written, "the {whose} is rewritten");
            fs::write(&record, other).expect("its record is rewritten in place");
            assert_eq!(
                shown_hosted(&hosted, &hosts),
                Err(
                    "who owns s-own can't be proved here, so the fence it runs in can't be told"
                        .to_owned()
                ),
                "one naming another {whose} is never read as its own"
            );
        }
        let strict = fenced_pi("PI_SANDBOX=strict\0", &[]);
        let hosts = fs::canonicalize(strict.2.path()).expect("its real path");
        hosts_in_58(&hosts, &[("s-own", Some("none"))]);
        assert_eq!(
            shown_hosted(&strict, &hosts),
            Ok(Kind::Strict),
            "a host begun strict is never read looser, whatever its record names"
        );
    }

    #[test]
    fn only_a_fenced_session_proved_on_its_host_explains_a_bubblewrap_below_it() {
        // Pi 59 owns s-other alone, beside the host pi 58 runs, with a
        // bubblewrap below the host alone.
        let hosted = fenced_pi("", &[(71, "bwrap", 58), (59, "pi", 1)]);
        started(hosted.0.path(), 59, "pi", 600);
        holds_log(hosted.0.path(), 59, hosted.1.path(), "s-other");
        let hosts = fs::canonicalize(hosted.2.path()).expect("its real path");
        let claims = [
            claim("s-own", 58, Some(500), Some("work-background")),
            claim("s-other", 59, Some(600), None),
        ];
        let shown = || {
            shown_fence(
                &claims,
                &folder(hosted.0.path()),
                &hosts,
                hosted.1.path(),
                "s-own",
            )
        };
        hosts_in_58(
            &hosts,
            &[("s-own", Some("none")), ("s-other", Some("fence"))],
        );
        assert!(
            proved(&claims, &folder(hosted.0.path()), &hosts, hosted.1.path())
                .expect("its owners are proved")
                .contains(&("s-other".to_owned(), 59)),
            "s-other is proved on pi 59, so the next assertion weighs where it is proved"
        );
        assert_eq!(
            shown(),
            Ok(Kind::Fence),
            "a fenced session the record names, proved on another pi, explains nothing below"
        );
        let kept = || {
            shown_fences(&claims, &folder(hosted.0.path()), &hosts, hosted.1.path())
                .get("s-own")
                .copied()
        };
        // What the look kept for good finds leaves the bubblewrap out
        // where the record names a sibling, since a sibling's may
        // outlive its session there.
        assert_eq!(kept(), Some(Kind::None), "as its record names it");
        // A record naming the session alone leaves no sibling to explain
        // it, as one forged for a pi that hosts nothing would read.
        hosts_in_58(&hosts, &[("s-own", Some("none"))]);
        assert_eq!(
            kept(),
            Some(Kind::Fence),
            "a record naming it alone keeps the bubblewrap below"
        );
        // A strict session's fence is Landlock, which starts no
        // bubblewrap, so one named strict explains none either.
        let beside = fenced_pi("", &[(71, "bwrap", 58)]);
        holds_log(beside.0.path(), 58, beside.1.path(), "s-other");
        let hosts = fs::canonicalize(beside.2.path()).expect("its real path");
        hosts_in_58(
            &hosts,
            &[("s-own", Some("none")), ("s-other", Some("strict"))],
        );
        assert_eq!(
            shown_hosted_beside(&beside, &hosts, &["s-other"]),
            Ok(Kind::Fence),
            "a strict session beside it, proved there, explains nothing below"
        );
    }

    #[test]
    fn a_pipe_or_an_outsized_record_in_a_hosts_place_holds_nothing_up() {
        let unproved = Err(
            "who owns s-own can't be proved here, so the fence it runs in can't be told".to_owned(),
        );
        let hosted = fenced_pi("", &[]);
        let hosts = fs::canonicalize(hosted.2.path()).expect("its real path");
        let record = hosts.join("58-500.json");
        hosts_in_58(&hosts, &[("s-own", Some("none"))]);
        let written = fs::read_to_string(&record).expect("its record reads");
        // Blank space before the record, past the most any host writes,
        // leaves nothing but blank space in what is read.
        let padded = " ".repeat(usize::try_from(HOST_RECORD_MOST).expect("fits")) + &written;
        fs::write(&record, padded).expect("its record is rewritten in place");
        assert_eq!(
            shown_hosted(&hosted, &hosts),
            unproved,
            "a record grown past any a host writes is never read whole"
        );
        fs::remove_file(&record).expect("removed");
        let made = Command::new("mkfifo")
            .arg(&record)
            .status()
            .expect("mkfifo ran");
        assert!(made.success(), "mkfifo made no pipe: {made}");
        // Asked on a thread of its own, so a read that waits on the pipe
        // fails here rather than holding the suite up.
        let (tell, heard) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let shown = shown_hosted(&hosted, &hosts);
            tell.send(shown).ok();
        });
        assert_eq!(
            heard
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("a pipe in a record's place holds nothing up"),
            unproved,
            "and a pipe is no record"
        );
    }

    #[test]
    fn a_hosts_record_another_account_or_a_link_could_have_put_there_says_no_fence() {
        use std::os::unix::fs::PermissionsExt as _;
        let hosted = fenced_pi("", &[]);
        let hosts = fs::canonicalize(hosted.2.path()).expect("its real path");
        let record = hosts.join("58-500.json");
        let untold = |why: &str| {
            Err(format!(
                "s-own runs in a pi host whose record {why}, so the fence it runs in can't be \
                 told"
            ))
        };
        hosts_in_58(&hosts, &[("s-own", Some("none"))]);
        assert_eq!(shown_hosted(&hosted, &hosts), Ok(Kind::None), "as written");
        fs::set_permissions(&record, fs::Permissions::from_mode(0o644)).expect("opened");
        assert_eq!(
            shown_hosted(&hosted, &hosts),
            untold("isn't a file this account alone may read and write"),
            "a record others may read is none the harness wrote"
        );
        let elsewhere = tempfile::tempdir().expect("somewhere else");
        let planted = elsewhere.path().join("58-500.json");
        fs::rename(&record, &planted).expect("moved");
        fs::set_permissions(&planted, fs::Permissions::from_mode(0o600)).expect("its own");
        std::os::unix::fs::symlink(&planted, &record).expect("linked");
        assert_eq!(
            shown_hosted(&hosted, &hosts),
            untold("can't be opened as a file"),
            "nor one reached through a link"
        );
        fs::remove_file(&record).expect("unlinked");
        hosts_in_58(&hosts, &[("s-own", Some("none"))]);
        let linked = elsewhere.path().join("hosts");
        std::os::unix::fs::symlink(&hosts, &linked).expect("linked");
        assert_eq!(
            shown_hosted(&hosted, &linked),
            untold("folder is reached through a link, or isn't there"),
            "nor a folder put in the hosts' place as a link"
        );
        fs::set_permissions(&hosts, fs::Permissions::from_mode(0o775)).expect("opened");
        assert_eq!(
            shown_hosted(&hosted, &hosts),
            untold("folder isn't this account's alone to write"),
            "nor a folder its group may write"
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
        let hosts = fs::canonicalize(hosts.path()).expect("its real path");
        hosts_in_58(&hosts, &[("s-hosted", None), ("s-told", Some("strict"))]);
        holds_log(procs.path(), 58, logs.path(), "s-hosted");
        holds_log(procs.path(), 58, logs.path(), "s-told");
        let claims = [
            claim("s-own", 58, Some(500), None),
            claim("s-hosted", 58, Some(500), Some("work-background")),
            claim("s-told", 58, Some(500), Some("work-background")),
            claim("s-ghost", 59, Some(1), None),
        ];
        let shown =
            |claims: &[Claim]| shown_fences(claims, &folder(procs.path()), &hosts, logs.path());
        assert_eq!(
            shown(&claims),
            BTreeMap::from([
                ("s-own".to_owned(), Kind::Fence),
                ("s-told".to_owned(), Kind::Strict)
            ]),
            "a hosted session as its record names it, but neither one it names none for nor \
             one whose owner isn't proved"
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
