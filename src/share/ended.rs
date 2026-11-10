//! What an ended quest leaves on a machine that shared it: its scratch,
//! taken away once nothing on the machine works inside it.
//!
//! Concluding or retiring a quest takes its scratch away on the machine
//! where a session did it, but every other machine sharing the quest kept
//! its own copy, clones and data included, until someone removed it by
//! hand. Each machine's node now takes its own copy away once the quest
//! has ended at both ends of a link, as the harness does where it ended:
//! never while a process has its working folder or an open file inside,
//! since a session or a build still at work there would lose it.

use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

/// What on this machine works inside a folder. One implementation reads
/// `/proc`; a test gives its own.
pub trait Users: Send + Sync {
    /// Whether any process has its working folder, or a file open, inside
    /// `folder`.
    fn inside(&self, folder: &Path) -> bool;
}

/// The machine's processes: read from `/proc` on Linux, and asked of
/// `lsof` elsewhere, as on a Mac. Where neither answers, the folder counts
/// as in use, since a scratch kept costs only disk and one taken from a
/// session at work costs its work.
pub struct Processes;

impl Users for Processes {
    fn inside(&self, folder: &Path) -> bool {
        let Ok(real) = fs::canonicalize(folder) else {
            return false;
        };
        if cfg!(target_os = "linux") {
            in_proc(&real)
        } else {
            in_lsof(&real)
        }
    }
}

/// Whether `/proc` holds a process whose working folder or an open file is
/// inside `real`.
fn in_proc(real: &Path) -> bool {
    let Ok(pids) = fs::read_dir("/proc") else {
        return true;
    };
    pids.flatten()
        .filter(|pid| {
            pid.file_name()
                .to_string_lossy()
                .bytes()
                .all(|b| b.is_ascii_digit())
        })
        .any(|pid| {
            let at = pid.path();
            let cwd = fs::read_link(at.join("cwd")).is_ok_and(|cwd| cwd.starts_with(real));
            cwd || fs::read_dir(at.join("fd")).is_ok_and(|fds| {
                fds.flatten()
                    .any(|fd| fs::read_link(fd.path()).is_ok_and(|to| to.starts_with(real)))
            })
        })
}

/// Whether `lsof`, asked for every open file's name, working folders
/// included, names one inside `real`; or it couldn't say.
fn in_lsof(real: &Path) -> bool {
    let Ok(out) = Command::new("lsof").args(["-n", "-w", "-F", "n"]).output() else {
        return true;
    };
    // lsof exits 1 when some process couldn't be read, as another
    // account's can't, so what it printed is read whatever its status.
    out.stdout.is_empty()
        || String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix('n'))
            .any(|name| Path::new(name).starts_with(real))
}

/// How taking a folder away went.
#[derive(Debug)]
pub enum Removal {
    /// It's gone.
    Removed,
    /// There was none.
    Absent,
    /// A process works inside it, so it was left for a later try.
    InUse,
    /// It couldn't be removed, even made writable.
    Stuck(io::Error),
}

/// Takes `folder` away, unless something works inside it. A folder made
/// read-only, as Go's module cache is, is made writable so its files can
/// go.
pub fn take_away(folder: &Path, users: &dyn Users) -> Removal {
    if !folder.exists() {
        return Removal::Absent;
    }
    if users.inside(folder) {
        return Removal::InUse;
    }
    if fs::remove_dir_all(folder).is_ok() {
        return Removal::Removed;
    }
    match make_writable(folder).and_then(|()| fs::remove_dir_all(folder)) {
        Ok(()) => Removal::Removed,
        Err(error) => Removal::Stuck(error),
    }
}

/// Every folder under `folder`, and `folder`, writable by its owner.
fn make_writable(folder: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(folder, fs::Permissions::from_mode(0o700))?;
    }
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            make_writable(&entry.path())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a test says works inside every folder, or none.
    struct Said(bool);

    impl Users for Said {
        fn inside(&self, _: &Path) -> bool {
            self.0
        }
    }

    #[test]
    fn a_folder_nothing_works_in_is_taken_away_whole() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let scratch = dir.path().join("scratch");
        fs::create_dir_all(scratch.join("clone/.git")).expect("made");
        fs::write(scratch.join("clone/.git/HEAD"), "ref\n").expect("written");

        assert!(
            matches!(take_away(&scratch, &Said(false)), Removal::Removed),
            "the scratch is taken away"
        );
        assert!(!scratch.exists(), "and nothing of it is left");
        assert!(
            matches!(take_away(&scratch, &Said(false)), Removal::Absent),
            "a second try finds none"
        );
    }

    #[test]
    fn a_folder_something_works_in_is_left() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        fs::write(dir.path().join("notes.md"), "notes\n").expect("written");

        assert!(
            matches!(take_away(dir.path(), &Said(true)), Removal::InUse),
            "a folder in use is left"
        );
        assert!(dir.path().join("notes.md").exists(), "with its files");
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_folder_is_made_writable_to_go() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a temporary folder");
        let cache = dir.path().join("scratch/go/mod");
        fs::create_dir_all(&cache).expect("made");
        fs::write(cache.join("go.mod"), "module x\n").expect("written");
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o500)).expect("made read-only");

        assert!(
            matches!(
                take_away(&dir.path().join("scratch"), &Said(false)),
                Removal::Removed
            ),
            "a read-only folder goes too"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_file_held_open_inside_is_found_and_once_closed_is_not() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let held = fs::File::create(dir.path().join("log")).expect("made");
        assert!(
            Processes.inside(dir.path()),
            "this test's own open file is found"
        );

        drop(held);
        assert!(!Processes.inside(dir.path()), "and once closed, nothing is");
    }
}
