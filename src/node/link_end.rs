//! Whether the process asking a relayed start of this node may speak as
//! a link's end, so a session here can't claim a link proved what it
//! didn't.

use super::asker::{self, Processes};

/// Why [`asker::unclaimed_because`] tells an asker an agent's only for
/// want of a person here: nothing of a session or a fence marks it.
const UNMARKED: &str = "nothing marks it an agent's";

/// Whether process `asker`, which started at `started` as it connected,
/// may speak as the link's end that relays a start here: one the kernel
/// names, its forebears all read, that no pi, pi host or agent's unit
/// runs, and that no fence marks by privileges it can't gain or mounts
/// of its own. Neither mark is shed once set, so a session pi fenced as
/// this node's own account, which can open its socket, can't say a
/// link's key proved its relay, even forked away from the pi that
/// started it. On a machine no person works on, which tells every asker
/// an agent's, the kernel beneath is read alike.
///
/// It errs towards no: it asks the kernel's own folder alone, so a
/// machine whose processes are told some other way, as a Mac's are,
/// keeps no proof; it takes a barred asker as fenced even where the
/// node is barred too, though the judging of the person allows that;
/// and it reads the asker's start again after judging it, so an id
/// taken again by another process since it connected, which might be
/// unmarked, is never judged in its place.
#[must_use]
pub(super) fn speaks_for_a_link(
    asker: Option<i32>,
    started: Option<u64>,
    processes: &Processes,
) -> bool {
    let (Some(pid), Some(ticks), Processes::Folder(procs)) = (asker, started, processes.kernel())
    else {
        return false;
    };
    asker::unclaimed_because(asker, processes.kernel()) == UNMARKED
        && !asker::barred(procs, &pid.to_string())
        && asker::started(procs, pid) == Some(ticks)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    /// A stand-in for `/proc` holding each of `processes`, by its id,
    /// name, parent and control group, each started at [`began`].
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
            starts(procs.path(), *pid, began(*pid));
        }
        procs
    }

    /// When the stand-in's process `pid` started, in clock ticks.
    fn began(pid: i32) -> u64 {
        u64::try_from(pid).expect("a pid") * 10
    }

    /// Has process `pid` in `procs` say it started at `ticks`, as the
    /// twentieth field after its name in its `stat`.
    fn starts(procs: &Path, pid: i32, ticks: u64) {
        let before = "S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0";
        let stat = format!("{pid} (mycelium) {before} {ticks} 0 0\n");
        fs::write(procs.join(pid.to_string()).join("stat"), stat).expect("its stat is written");
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
        let speaks = |pid: i32, processes: &Processes| {
            speaks_for_a_link(Some(pid), Some(began(pid)), processes)
        };
        for processes in [&kernel, &unattended] {
            assert!(speaks(31, processes), "the link's end, under sshd");
            assert!(!speaks(41, processes), "a session below pi");
            assert!(
                !speaks(50, processes),
                "one a fence barred from gaining privileges, though pi is gone"
            );
            assert!(
                !speaks_for_a_link(None, None, processes),
                "one the kernel doesn't name"
            );
            assert!(!speaks(60, processes), "one nothing can be read of");
            assert!(
                !speaks_for_a_link(Some(31), None, processes),
                "one whose start wasn't read as it connected"
            );
        }
    }

    #[test]
    fn an_id_taken_again_since_it_connected_speaks_for_no_link() {
        let procs = procs(&[
            (30, "sshd", 1, "system.slice/sshd.service"),
            (31, "mycelium", 30, "system.slice/sshd.service"),
        ]);
        let kernel = Processes::Folder(procs.path().to_owned());
        // A fenced process connected as 31 and went, and a link's end
        // then took 31, unmarked, while the connection stayed open.
        let connected = began(31) - 1;
        assert!(
            !speaks_for_a_link(Some(31), Some(connected), &kernel),
            "the process judged isn't the one that connected"
        );
    }

    #[test]
    fn a_barred_asker_speaks_for_no_link_though_the_node_is_barred_too() {
        let procs = procs(&[
            (30, "sshd", 1, "system.slice/sshd.service"),
            (31, "mycelium", 30, "system.slice/sshd.service"),
        ]);
        fs::create_dir(procs.path().join("self")).expect("the node's folder is made");
        fs::write(procs.path().join("self").join("status"), "NoNewPrivs:\t1\n")
            .expect("the node is barred");
        barred(procs.path(), 31);
        let kernel = Processes::Folder(procs.path().to_owned());
        assert!(
            !speaks_for_a_link(Some(31), Some(began(31)), &kernel),
            "a node run barred, as a basin's exec runs it, keeps no proof from a barred asker"
        );
    }
}
