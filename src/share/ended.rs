//! What an ended quest leaves on a machine that shared it: its scratch,
//! taken away once nothing on the machine works inside it.
//!
//! Concluding or retiring a quest takes its scratch away on the machine
//! where a session did it, but every other machine sharing the quest kept
//! its own copy, clones and data included, until someone removed it by
//! hand. Each machine's node now takes its own copy away once the quest
//! has ended at both ends of a link, as the harness does where it ended:
//! never while a process has its working folder or an open file inside,
//! since a session or a build still at work there would lose it. A copy
//! the machine moved aside beside itself before sharing, its own work, is
//! kept ([`take_away_keeping_aside`]); a session concluding the quest on
//! this machine still takes the whole scratch, that copy with it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
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
    /// It's gone but for the copies moved aside beside themselves, kept
    /// in it as this machine's own work.
    Kept(Vec<PathBuf>),
    /// It couldn't be removed, even made writable.
    Stuck(io::Error),
}

/// The name a copy of `name` moved aside beside itself at `stamp` takes:
/// it starts with a dot, so no end shares it.
#[must_use]
pub fn beside(name: &str, stamp: &str) -> String {
    format!(".{name}{BESIDE}{stamp}")
}

/// Whether `name` is a copy moved aside beside itself ([`beside`]).
#[must_use]
pub fn is_beside(name: &str) -> bool {
    name.starts_with('.') && name.contains(BESIDE)
}

/// What a copy moved aside beside itself holds in its name.
const BESIDE: &str = ".before-share-";

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
    match remove(folder) {
        Ok(()) => Removal::Removed,
        Err(error) => Removal::Stuck(error),
    }
}

/// Takes `folder` away as [`take_away`] does, but for the copies this
/// machine moved aside beside themselves in it before sharing
/// ([`is_beside`]): they are its own work, which no other machine holds,
/// so they are kept, and `folder` is left holding only them. A `folder`
/// that is a link is only unlinked, as [`take_away`] does, never emptied
/// where it leads. One that can't be read through, whole or an entry of
/// it, is stuck rather than taken away whole, since a copy aside may be
/// what couldn't be read.
pub fn take_away_keeping_aside(folder: &Path, users: &dyn Users) -> Removal {
    match fs::symlink_metadata(folder) {
        Ok(meta) if meta.is_dir() => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Removal::Absent,
        Err(error) => return Removal::Stuck(error),
        Ok(_) => return take_away(folder, users),
    }
    let paths: io::Result<Vec<PathBuf>> = fs::read_dir(folder).and_then(|entries| {
        entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect()
    });
    let paths = match paths {
        Ok(paths) => paths,
        Err(error) => return Removal::Stuck(error),
    };
    let (kept, rest): (Vec<PathBuf>, Vec<PathBuf>) = paths.into_iter().partition(|path| {
        path.file_name()
            .is_some_and(|name| is_beside(&name.to_string_lossy()))
    });
    if kept.is_empty() {
        return take_away(folder, users);
    }
    if users.inside(folder) {
        return Removal::InUse;
    }
    for path in rest {
        let gone = remove(&path).or_else(|error| {
            // A scratch made read-only keeps its entries, so it is made
            // writable, itself alone, and they are tried again.
            if error.kind() == io::ErrorKind::PermissionDenied {
                owner_writable(folder).and_then(|()| remove(&path))
            } else {
                Err(error)
            }
        });
        match gone {
            // Another link sweeping the same scratch took it first.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Removal::Stuck(error),
            Ok(()) => {}
        }
    }
    Removal::Kept(kept)
}

/// Removes `path`: a folder and all in it, made writable where it must be,
/// or anything else, a link included, by its own name alone, so nothing a
/// link leads to is opened or made writable.
fn remove(path: &Path) -> io::Result<()> {
    if !fs::symlink_metadata(path)?.is_dir() {
        return fs::remove_file(path);
    }
    if fs::remove_dir_all(path).is_ok() {
        return Ok(());
    }
    make_writable(path).and_then(|()| fs::remove_dir_all(path))
}

/// `folder` itself writable by its owner, leaving what is in it be.
fn owner_writable(folder: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(folder, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Every folder under `folder`, and `folder`, writable by its owner.
fn make_writable(folder: &Path) -> io::Result<()> {
    owner_writable(folder)?;
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
    fn a_copy_moved_aside_beside_itself_is_kept_when_the_rest_goes() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let scratch = dir.path().join("scratch");
        let copy = scratch.join(beside("notes", "20261127T000000Z"));
        fs::create_dir_all(&copy).expect("made");
        fs::write(copy.join("old.md"), "this machine's own\n").expect("written");
        fs::create_dir_all(scratch.join("notes")).expect("made");
        fs::write(scratch.join("notes/plan.md"), "shared\n").expect("written");
        fs::write(scratch.join("loose.md"), "shared\n").expect("written");
        fs::write(scratch.join(".hidden"), "not a copy\n").expect("written");

        assert!(
            matches!(
                take_away_keeping_aside(&scratch, &Said(true)),
                Removal::InUse
            ),
            "a scratch in use is left"
        );
        let kept = take_away_keeping_aside(&scratch, &Said(false));

        assert!(
            matches!(&kept, Removal::Kept(kept) if kept.as_slice() == [copy.clone()]),
            "the copy is kept: {kept:?}"
        );
        assert_eq!(
            fs::read_to_string(copy.join("old.md")).expect("kept"),
            "this machine's own\n",
            "with all it held"
        );
        let left: Vec<_> = fs::read_dir(&scratch)
            .expect("read")
            .flatten()
            .map(|entry| entry.path())
            .collect();
        assert_eq!(left, [copy], "and nothing else is left");

        fs::remove_dir_all(scratch.join(beside("notes", "20261127T000000Z"))).expect("removed");
        assert!(
            matches!(
                take_away_keeping_aside(&scratch, &Said(false)),
                Removal::Removed
            ),
            "with no copy aside the scratch goes whole"
        );
        assert!(!scratch.exists());
    }

    // #586 round 2 F50: a scratch that is a link is only unlinked, as a
    // whole take-away does, so nothing where it leads is emptied.
    #[cfg(unix)]
    #[test]
    fn a_scratch_that_is_a_link_is_unlinked_and_what_it_leads_to_is_left() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let elsewhere = dir.path().join("elsewhere");
        let copy = elsewhere.join(beside("notes", "20261127T000000Z"));
        fs::create_dir_all(&copy).expect("made");
        fs::write(elsewhere.join("plan.md"), "not the scratch's\n").expect("written");
        let scratch = dir.path().join("scratch");
        std::os::unix::fs::symlink(&elsewhere, &scratch).expect("linked");

        let taken = take_away_keeping_aside(&scratch, &Said(false));

        assert!(matches!(taken, Removal::Removed), "unlinked: {taken:?}");
        assert!(fs::symlink_metadata(&scratch).is_err(), "the link is gone");
        assert!(
            elsewhere.join("plan.md").exists() && copy.exists(),
            "and what it led to is left whole"
        );
    }

    // #586 round 2 F27: a scratch that can't be read through may hold a
    // copy aside, so it's stuck rather than made writable and taken away
    // whole.
    #[cfg(unix)]
    #[test]
    fn a_scratch_that_cant_be_read_is_stuck_with_its_copy_aside_kept() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a temporary folder");
        let scratch = dir.path().join("scratch");
        let copy = scratch.join(beside("notes", "20261127T000000Z"));
        fs::create_dir_all(&copy).expect("made");
        fs::write(copy.join("old.md"), "this machine's own\n").expect("written");
        fs::set_permissions(&scratch, fs::Permissions::from_mode(0o300)).expect("unreadable");
        if fs::read_dir(&scratch).is_ok() {
            fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700)).expect("readable");
            println!("skipped: this user reads a folder whatever its mode");
            return;
        }

        let taken = take_away_keeping_aside(&scratch, &Said(false));
        fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700)).expect("readable");

        assert!(matches!(taken, Removal::Stuck(_)), "stuck: {taken:?}");
        assert_eq!(
            fs::read_to_string(copy.join("old.md")).expect("kept"),
            "this machine's own\n",
            "the copy aside is kept with all it held"
        );
    }

    #[test]
    fn only_a_dotted_name_holding_the_mark_is_a_copy_aside() {
        assert!(is_beside(&beside("notes", "20261127T000000Z")));
        assert!(!is_beside("notes.before-share-20261127T000000Z"));
        assert!(!is_beside(".notes"));
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

    #[cfg(unix)]
    #[test]
    fn a_read_only_scratch_keeping_a_copy_aside_is_made_writable_for_the_rest() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a temporary folder");
        let scratch = dir.path().join("scratch");
        let copy = scratch.join(beside("notes", "20261127T000000Z"));
        fs::create_dir_all(&copy).expect("made");
        fs::create_dir_all(scratch.join("bench")).expect("made");
        fs::write(scratch.join("run.md"), "a run\n").expect("written");
        fs::set_permissions(&scratch, fs::Permissions::from_mode(0o500)).expect("made read-only");

        let removal = take_away_keeping_aside(&scratch, &Said(false));

        assert!(matches!(removal, Removal::Kept(_)), "the rest goes");
        assert!(copy.is_dir(), "and the copy aside stays");
        assert!(
            !scratch.join("bench").exists() && !scratch.join("run.md").exists(),
            "with nothing else left"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_scratch_that_cant_be_unlinked_leaves_what_it_leads_to_as_it_was() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a temporary folder");
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir_all(elsewhere.join("inner")).expect("made");
        fs::set_permissions(elsewhere.join("inner"), fs::Permissions::from_mode(0o500))
            .expect("made read-only");
        let scratches = dir.path().join("scratch");
        fs::create_dir_all(&scratches).expect("made");
        let scratch = scratches.join("QEST-20261026-TIZK53");
        std::os::unix::fs::symlink(&elsewhere, &scratch).expect("linked");
        fs::set_permissions(&scratches, fs::Permissions::from_mode(0o500)).expect("locked");
        let unlocked = || {
            fs::set_permissions(&scratches, fs::Permissions::from_mode(0o755)).expect("unlocked");
            fs::set_permissions(elsewhere.join("inner"), fs::Permissions::from_mode(0o755))
                .expect("unlocked");
        };
        if fs::create_dir(scratches.join(".probe")).is_ok() {
            unlocked();
            eprintln!("skipped: a folder locked 0o500 is writable here, as it is to root");
            return;
        }

        let mode = |path: &Path| fs::metadata(path).expect("there").permissions().mode() & 0o777;
        let before = mode(&elsewhere);
        let removal = take_away_keeping_aside(&scratch, &Said(false));
        let (outer, inner) = (mode(&elsewhere), mode(&elsewhere.join("inner")));
        unlocked();

        assert!(matches!(removal, Removal::Stuck(_)), "the link is stuck");
        assert_eq!(inner, 0o500, "nothing it leads to is made writable");
        assert_eq!(outer, before, "not even where it leads");
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
        // A child another test forks while the file is open keeps a copy of
        // it until that child execs, since close-on-exec closes it only
        // then, so this waits out such a copy rather than asking once.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while Processes.inside(dir.path()) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!Processes.inside(dir.path()), "and once closed, nothing is");
    }
}
