//! Whether the process asking a relayed start of this node may speak as
//! a link's end, so a session here can't claim a link proved what it
//! didn't.

use super::asker::{self, Processes};

/// Why [`asker::unclaimed_because`] tells an asker an agent's only for
/// want of a person here: nothing of a session or a fence marks it.
const UNMARKED: &str = "nothing marks it an agent's";

/// Whether process `asker` may speak as the link's end that relays a
/// start here: one the kernel names, its forebears all read, that no pi,
/// pi host or agent's unit runs, and that no fence marks by privileges
/// it can't gain or mounts of its own. Neither mark is shed once set, so
/// a session pi fenced as this node's own account, which can open its
/// socket, can't say a link's key proved its relay, even forked away
/// from the pi that started it. On a machine no person works on, which
/// tells every asker an agent's, the kernel beneath is read alike.
#[must_use]
pub(super) fn speaks_for_a_link(asker: Option<i32>, processes: &Processes) -> bool {
    asker::unclaimed_because(asker, processes.kernel()) == UNMARKED
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    /// A stand-in for `/proc` holding each of `processes`, by its id,
    /// name, parent and control group.
    fn procs(processes: &[(i32, &str, i32, &str)]) -> tempfile::TempDir {
        let procs = tempfile::tempdir().expect("a stand-in for /proc");
        for (pid, name, parent, group) in processes {
            let process = procs.path().join(pid.to_string());
            fs::create_dir(&process).expect("its folder is made");
            fs::write(
                process.join("status"),
                format!("Name:\t{name}\nState:\tS (sleeping)\nPPid:\t{parent}\n"),
            )
            .expect("its status is written");
            fs::write(process.join("cgroup"), format!("0::/{group}\n"))
                .expect("its group is written");
        }
        procs
    }

    fn barred(procs: &Path, pid: i32) {
        let status = procs.join(pid.to_string()).join("status");
        let said = fs::read_to_string(&status).expect("its status");
        fs::write(&status, format!("{said}NoNewPrivs:\t1\n")).expect("it is barred");
    }

    #[test]
    fn only_a_link_end_no_session_or_fence_marks_speaks_for_a_link() {
        let procs = procs(&[
            (30, "sshd", 1, "system.slice/sshd.service"),
            (31, "mycelium", 30, "system.slice/sshd.service"),
            (40, "pi", 1, "system.slice/mycelium.service"),
            (41, "mycelium", 40, "system.slice/mycelium.service"),
            // Forked away from its pi, so no pi is among its forebears.
            (50, "mycelium", 1, "system.slice/mycelium.service"),
        ]);
        barred(procs.path(), 50);
        let kernel = Processes::Folder(procs.path().to_owned());
        let unattended = Processes::Folder(procs.path().to_owned()).unattended();
        for processes in [&kernel, &unattended] {
            assert!(speaks_for_a_link(Some(31), processes), "the link's end, under sshd");
            assert!(!speaks_for_a_link(Some(41), processes), "a session below pi");
            assert!(
                !speaks_for_a_link(Some(50), processes),
                "one a fence barred from gaining privileges, though pi is gone"
            );
            assert!(!speaks_for_a_link(None, processes), "one the kernel doesn't name");
            assert!(!speaks_for_a_link(Some(60), processes), "one nothing can be read of");
        }
    }
}
