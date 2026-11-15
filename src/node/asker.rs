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
use std::path::PathBuf;
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

/// The session that asks, by its own id, among `owners`, each session of
/// this machine's that runs and the process that owns it, as its presence
/// says: the one whose process the asker is, or runs below. A pi host owns
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
