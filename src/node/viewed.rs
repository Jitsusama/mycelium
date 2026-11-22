//! The disk as the process asking the node sees it, so a sprout finds its
//! folder where the asker could reach it itself, never where only the node
//! can.
//!
//! An agent may run fenced, in a mount namespace that hides the person's
//! own folders, while the node sees them all. Were a sprout judged on the
//! node's disk, an agent could have the node start a session in a folder
//! it can't reach, so each path is first resolved inside the asker's root,
//! as the kernel shows it at `/proc/<pid>/root`, and links are kept inside
//! it (`RESOLVE_IN_ROOT`). A path counts only where the node's own path
//! names the same file, by its device and inode, since the session starts
//! in the node's namespace: the folder it starts in is the node's path for
//! what the asker named.
//!
//! A process id can be taken again once its process ends, so the asker is
//! trusted only while its start ticks are those read when it connected.

use std::fs;
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use rustix::fs::{Mode, OFlags, ResolveFlags};

use super::asker;
use crate::sprout::{Disk, ThisDisk, lexical};

/// The asker's root and where it is, opened while it's the process that
/// connected.
pub(super) struct Viewed {
    root: OwnedFd,
    cwd: PathBuf,
}

impl Viewed {
    /// The view of process `pid` in `procs`, which connected when it had
    /// started `ticks` after boot; refused where it has since gone, or its
    /// id is another's, or where it is can't be told.
    pub(super) fn of(procs: &Path, pid: i32, ticks: u64) -> Result<Self, String> {
        let process = procs.join(pid.to_string());
        let opened = |what: &str| {
            rustix::fs::open(
                process.join(what),
                OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| "the asker has gone, so its folders can't be told".to_owned())
        };
        let root = opened("root")?;
        // A kernel without openat2, or a filter refusing it, can't keep a
        // path inside the root, so the view is said unreadable as a whole
        // rather than every folder in it said missing.
        within(&root, Path::new("/"))
            .map_err(|_| "paths can't be kept within the asker's root here".to_owned())?;
        let cwd_opened = opened("cwd")?;
        let cwd = fs::read_link(process.join("cwd"))
            .map_err(|_| "the asker has gone, so its folders can't be told".to_owned())?;
        // Read after opening, so what was opened was the asker's own.
        match asker::started(procs, pid) {
            Some(started) if started == ticks => {}
            Some(_) => {
                return Err(
                    "the asker's id is another process's now, so its folders can't be told"
                        .to_owned(),
                );
            }
            None => return Err("the asker has gone, so its folders can't be told".to_owned()),
        }
        let there = rustix::fs::fstat(&cwd_opened)
            .map_err(|_| "where the asker is can't be told".to_owned())?;
        let same = fs::metadata(&cwd)
            .is_ok_and(|here| here.dev() == there.st_dev && here.ino() == there.st_ino);
        if !same {
            return Err("where the asker is can't be told".to_owned());
        }
        Ok(Self { root, cwd })
    }

    /// Where the asker is, as the node names it.
    pub(super) fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The node's own path for `folder` as the asker sees it, where both
    /// name the same file; none where the asker can't reach it, or the
    /// node's path is another file.
    fn here(&self, folder: &Path) -> Option<PathBuf> {
        let opened = within(&self.root, folder).ok()?;
        let there = rustix::fs::fstat(&opened).ok()?;
        let real = folder.canonicalize().ok()?;
        let here = fs::metadata(&real).ok()?;
        (here.dev() == there.st_dev && here.ino() == there.st_ino).then_some(real)
    }
}

/// Opens `folder` as if `root` were `/`: links and `..` can't leave it,
/// nor can the kernel's own links in /proc, which point past any root.
fn within(root: &OwnedFd, folder: &Path) -> rustix::io::Result<OwnedFd> {
    rustix::fs::openat2(
        root,
        folder,
        OFlags::PATH | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::IN_ROOT | ResolveFlags::NO_MAGICLINKS,
    )
}

impl Disk for Viewed {
    fn is_folder(&self, folder: &Path) -> bool {
        self.here(folder).is_some_and(|real| real.is_dir())
    }

    fn is_checkout(&self, folder: &Path) -> bool {
        self.here(&folder.join(".git")).is_some()
    }

    fn resolved(&self, folder: &Path) -> PathBuf {
        self.here(folder).unwrap_or_else(|| lexical(folder))
    }

    fn origin(&self, folder: &Path) -> Option<String> {
        ThisDisk.origin(&self.here(folder)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::declared::Kind;
    use crate::node::Processes;
    use crate::wire::By;
    use std::os::unix::fs::symlink;

    /// Writes in `procs` a process `pid` started `ticks` after boot, whose
    /// root is `root` and which stands in `cwd`.
    fn asking(procs: &Path, pid: i32, ticks: u64, root: &Path, cwd: &Path) {
        let process = procs.join(pid.to_string());
        fs::create_dir_all(&process).expect("its folder is made");
        let zeros = "0 ".repeat(17);
        fs::write(
            process.join("stat"),
            format!("{pid} (pi) S 1 {zeros}{ticks} 0 0\n"),
        )
        .expect("its stat is written");
        symlink(root, process.join("root")).expect("its root is linked");
        symlink(cwd, process.join("cwd")).expect("its cwd is linked");
    }

    fn checkout(at: &Path) {
        fs::create_dir_all(at.join(".git")).expect("the checkout is made");
    }

    #[test]
    fn an_asker_seeing_the_whole_disk_finds_its_checkout_there() {
        let place = tempfile::tempdir().expect("a place");
        let rows = place.path().join("src/rows");
        checkout(&rows);
        let procs = place.path().join("proc");
        asking(&procs, 41, 7, Path::new("/"), &rows);

        let viewed = Viewed::of(&procs, 41, 7).expect("the asker is seen");

        assert!(viewed.is_checkout(&rows));
        assert!(viewed.is_folder(&rows));
        assert_eq!(viewed.resolved(&rows), rows.canonicalize().expect("real"));
        assert_eq!(viewed.cwd(), rows);
    }

    #[test]
    fn a_folder_only_the_person_holds_is_no_folder_to_an_agent_that_cant_see_it() {
        let place = tempfile::tempdir().expect("a place");
        let persons = place.path().join("person/src/secret");
        checkout(&persons);
        // The agent's view holds a folder of its own at the same path,
        // as a fence's bind mounts might, but not the person's.
        let view = place.path().join("view");
        let own = view.join(persons.strip_prefix("/").expect("absolute"));
        fs::create_dir_all(&own).expect("its own is made");
        let procs = place.path().join("proc");
        asking(&procs, 42, 9, &view, &view);
        let viewed = Viewed::of(&procs, 42, 9).expect("the asker is seen");

        assert!(!viewed.is_checkout(&persons));
        assert!(!viewed.is_folder(&persons));
        assert!(!viewed.is_folder(&persons.join("..")));
        assert_eq!(viewed.resolved(&persons), persons);
    }

    #[test]
    fn a_link_in_the_askers_view_never_leads_out_of_it() {
        let place = tempfile::tempdir().expect("a place");
        let persons = place.path().join("person/rows");
        checkout(&persons);
        let view = place.path().join("view");
        // At the person's own path in the view, and absolute, so followed
        // as the node sees the disk it would reach the person's checkout.
        let linked = view.join(persons.strip_prefix("/").expect("absolute"));
        fs::create_dir_all(linked.parent().expect("a parent")).expect("the view is made");
        symlink(&persons, &linked).expect("linked");
        let procs = place.path().join("proc");
        asking(&procs, 45, 3, &view, &view);
        let viewed = Viewed::of(&procs, 45, 3).expect("the asker is seen");

        assert!(!viewed.is_checkout(&persons));
        assert!(!viewed.is_folder(&persons));
    }

    #[test]
    fn an_asker_whose_id_another_process_took_is_refused() {
        let place = tempfile::tempdir().expect("a place");
        let procs = place.path().join("proc");
        asking(&procs, 43, 11, Path::new("/"), place.path());

        let refused = Viewed::of(&procs, 43, 10).err();

        assert_eq!(
            refused.as_deref(),
            Some("the asker's id is another process's now, so its folders can't be told")
        );
    }

    fn sprout_in(folder: &Path) -> crate::wire::Sprout {
        crate::wire::Sprout {
            words: vec!["tidy".to_owned(), "the".to_owned(), "rows".to_owned()],
            folder: Some(folder.to_owned()),
            from: None,
            on: None,
            cwd: None,
        }
    }

    #[test]
    fn an_agent_naming_a_folder_only_the_person_holds_is_refused() {
        let place = tempfile::tempdir().expect("a place");
        let home = place.path().join("home");
        let persons = home.join("src/secret");
        checkout(&persons);
        let view = place.path().join("view");
        fs::create_dir_all(&view).expect("the view is made");
        let procs = place.path().join("proc");
        asking(&procs, 46, 5, &view, &view);
        let agent = By::Agent(Kind::Fence);

        let refused = super::super::sprouted(
            &sprout_in(&persons),
            agent,
            Some((46, 5)),
            &Processes::Folder(procs),
            &home,
            "osprey",
        );

        assert_eq!(
            refused,
            Err(format!("{} isn't a folder here", persons.display()))
        );
    }

    #[test]
    fn an_agent_seeing_the_folder_it_names_starts_there_as_the_node_names_it() {
        let place = tempfile::tempdir().expect("a place");
        let home = place.path().canonicalize().expect("real").join("home");
        let rows = home.join("src/rows");
        checkout(&rows);
        let procs = place.path().join("proc");
        asking(&procs, 47, 6, Path::new("/"), &rows);

        let start = super::super::sprouted(
            &sprout_in(&rows),
            By::Agent(Kind::Fence),
            Some((47, 6)),
            &Processes::Folder(procs),
            &home,
            "osprey",
        )
        .expect("it starts")
        .start;

        assert_eq!(start.folder, Some(PathBuf::from("src/rows")));
    }

    #[test]
    fn an_agents_own_word_for_where_it_is_counts_for_nothing_once_its_view_is_read() {
        let place = tempfile::tempdir().expect("a place");
        let home = place.path().canonicalize().expect("real").join("home");
        let rows = home.join("src/rows");
        let persons = home.join("src/persons");
        checkout(&rows);
        checkout(&persons);
        let procs = place.path().join("proc");
        asking(&procs, 48, 4, Path::new("/"), &rows);
        let said = crate::wire::Sprout {
            folder: None,
            cwd: Some(persons),
            ..sprout_in(&rows)
        };

        let start = super::super::sprouted(
            &said,
            By::Agent(Kind::Fence),
            Some((48, 4)),
            &Processes::Folder(procs),
            &home,
            "osprey",
        )
        .expect("it starts")
        .start;

        assert_eq!(start.folder, Some(PathBuf::from("src/rows")));
    }

    /// A process kept in a mount view of its own by bubblewrap, ended when
    /// the test is done with it.
    struct Fenced(std::process::Child);

    impl Drop for Fenced {
        fn drop(&mut self) {
            // One that ended already has nothing to kill or wait on.
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A process standing in `cwd` whose view of the disk is the node's
    /// with an empty folder laid over `hidden`, as a fence hides the
    /// person's own; none where bubblewrap can't make one, as in a build's
    /// sandbox. It says its id and waits on its input.
    fn fenced(hidden: &Path, cwd: &Path) -> Option<(Fenced, i32)> {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        let mut child = Command::new("bwrap")
            .args(["--dev-bind", "/", "/", "--tmpfs"])
            .arg(hidden)
            .arg("--chdir")
            .arg(cwd)
            .args(["sh", "-c", "echo $$; exec cat"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let out = child.stdout.take()?;
        let child = Fenced(child);
        let mut line = String::new();
        BufReader::new(out).read_line(&mut line).ok()?;
        let pid = line.trim().parse().ok()?;
        Some((child, pid))
    }

    /// The one thing the made-up /proc above can't show: that a path
    /// opened from a real process's /proc root walks that process's own
    /// mount namespace. Where bubblewrap can't run it says so and proves
    /// nothing.
    #[test]
    fn an_agent_in_a_real_mount_view_is_refused_what_it_hides_and_started_where_it_sees() {
        let place = tempfile::tempdir().expect("a place");
        let home = place.path().canonicalize().expect("real").join("home");
        let seen = home.join("src/rows");
        let hidden = home.join("src/secret");
        checkout(&seen);
        checkout(&hidden);
        let Some((_fenced, pid)) = fenced(&hidden, &seen) else {
            eprintln!("bubblewrap can't make a mount view here, so nothing is proved");
            return;
        };
        let procs = PathBuf::from("/proc");
        let ticks = asker::started(&procs, pid).expect("the fenced process has started");
        let viewed = Viewed::of(&procs, pid, ticks).expect("the fenced process is seen");
        assert!(viewed.is_checkout(&seen));
        assert!(!viewed.is_checkout(&hidden));
        assert!(!viewed.is_folder(&hidden));
        assert_eq!(viewed.cwd(), seen);
        let processes = Processes::Folder(procs);
        let sprout = |folder: &Path| {
            super::super::sprouted(
                &sprout_in(folder),
                By::Agent(Kind::Fence),
                Some((pid, ticks)),
                &processes,
                &home,
                "osprey",
            )
        };

        let refused = sprout(&hidden);
        let started = sprout(&seen).expect("it starts where it sees").start;

        assert_eq!(
            refused,
            Err(format!("{} isn't a folder here", hidden.display()))
        );
        assert_eq!(started.folder, Some(PathBuf::from("src/rows")));
    }

    #[test]
    fn an_asker_gone_is_refused() {
        let place = tempfile::tempdir().expect("a place");

        assert!(Viewed::of(&place.path().join("proc"), 44, 1).is_err());
    }
}
