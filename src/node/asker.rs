//! Who asks the node to steer: whether the process at a client's end runs
//! as one of the machine's agents, which may say things to a session but
//! never as the person.
//!
//! Only the person's own login speaks as the person, but a steer names who
//! it is from itself, so any process of the person's account could claim
//! to be them. The kernel says which process is at a client's end; its
//! control group says which unit of the person's service manager runs it,
//! and the machine runs its agents in units of their own: pi's hosts, each
//! held session's scope, and the queues their commands wait in. A process
//! with pi among its forebears is an agent's too, as a session started in
//! a terminal rather than held is.
//!
//! This keeps an agent from speaking as the person by habit or by mistake,
//! not one set on it: a process of the person's account may move itself
//! into any unit of theirs, or write a session's inbox itself.

use std::fs;
use std::path::Path;

use serde_json::Value;

/// How the names of the units the machine runs its agents in begin, below
/// the person's service manager's own unit: pi's hosts, the scope each
/// held session runs in, and the queues agents' commands wait their turn
/// in, here or for a builder elsewhere.
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

/// Whether `said` may be put to a session when process `asker` asks, as
/// `procs`, the machine's `/proc`, tells it: what claims to be from the
/// person is refused from an agent, saying why, and anything else goes.
/// An asker the kernel won't name, or a machine with no control groups to
/// read, as a Mac, is taken at its word, since nothing there tells an
/// agent apart.
pub fn may_say(asker: Option<i32>, procs: &Path, said: &Value) -> Result<(), String> {
    if !claims_the_person(said) {
        return Ok(());
    }
    let Some(pid) = asker else {
        return Ok(());
    };
    match agent(pid, procs) {
        Some(unit) => Err(format!(
            "this asks from {unit}, an agent's, and an agent can't speak as the person: \
             only the person's own login says what they say"
        )),
        None => Ok(()),
    }
}

/// Whether `said` claims to be from the person, as the harness reads a
/// steer's `from`: naming nobody, Joel, or the person, in any case.
fn claims_the_person(said: &Value) -> bool {
    let from = said.get("from").and_then(Value::as_str).unwrap_or("");
    from.is_empty() || from.eq_ignore_ascii_case("joel") || from.eq_ignore_ascii_case("person")
}

/// The agent's unit or process `pid` runs as, by its name, or none for a
/// process of the person's own or one that can't be told.
fn agent(pid: i32, procs: &Path) -> Option<String> {
    let cgroup = fs::read_to_string(procs.join(pid.to_string()).join("cgroup")).ok();
    if let Some(unit) = cgroup.as_deref().and_then(agents_unit) {
        return Some(unit);
    }
    let mut process = pid;
    for _ in 0..FOREBEARS {
        let status = fs::read_to_string(procs.join(process.to_string()).join("status")).ok()?;
        let (name, parent) = named(&status)?;
        if name == PI {
            return Some(format!("pi's process {process}"));
        }
        if parent <= 1 {
            return None;
        }
        process = parent;
    }
    None
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

#[cfg(test)]
mod tests {
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

    #[test]
    fn a_steer_claims_the_person_as_the_harness_reads_it() {
        for said in [
            json!({"act": "pause"}),
            json!({"act": "pause", "from": ""}),
            json!({"text": "hi", "from": "Joel"}),
            json!({"text": "hi", "from": "person"}),
            json!({"text": "hi", "from": 7}),
        ] {
            assert!(claims_the_person(&said), "{said} speaks as the person");
        }
        for said in [
            json!({"text": "leaving at six", "from": "machine"}),
            json!({"text": "hi", "from": "P9R1KI"}),
        ] {
            assert!(!claims_the_person(&said), "{said} doesn't");
        }
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

    #[test]
    fn an_agent_may_say_anything_but_as_the_person() {
        let procs = procs(&[(
            40,
            "mycelium",
            2,
            "agents.slice/pi-host-work-attended.service",
        )]);
        let refused = may_say(
            Some(40),
            procs.path(),
            &json!({"act": "pause", "from": "person"}),
        );
        assert_eq!(
            refused,
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
                procs.path(),
                &json!({"text": "your build is done", "from": "P9R1KI"})
            ),
            Ok(()),
            "an agent saying something as itself is let through"
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
            agent(50, procs.path()),
            Some("pi's process 48".to_owned()),
            "a command pi ran in a terminal is pi's"
        );
        assert_eq!(
            agent(47, procs.path()),
            None,
            "the person's own shell in that terminal is theirs"
        );
    }

    #[test]
    fn an_asker_nothing_tells_apart_is_taken_at_its_word() {
        let nowhere = Path::new("/nowhere/proc");
        let pause = json!({"act": "pause"});
        assert_eq!(may_say(Some(40), nowhere, &pause), Ok(()), "with no /proc");
        assert_eq!(
            may_say(None, nowhere, &pause),
            Ok(()),
            "with no process named"
        );
    }
}
