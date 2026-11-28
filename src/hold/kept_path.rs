//! Which folders of a plain run's path it keeps
//! ([`super::start::plain_run`]), judged by this machine's own mycelium,
//! as `mycelium kept-path`, named by where it is, never looked up on the
//! path it judges.
//!
//! A folder goes that the run's user could plant a program in: one in
//! the run's home, by name or by where it leads; one named relatively or
//! that can't be entered; one that user owns or could write; and one
//! under a folder it owns, or could write without the sticky bit, up to
//! `/`. Whether it could write a folder is read from the folder's owner,
//! group and mode, and from access(2) as well, never from access(2)
//! alone: a fence that binds a folder read-only makes access(2) say no,
//! though the same user can write it outside the fence, and a fence can
//! only ever take more off the path, never less.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use rustix::fs::{Access, FileType, Mode};
use rustix::process::Gid;

/// The subcommand of mycelium that judges a plain run's path.
pub const SUBCOMMAND: &str = "kept-path";

/// What a folder reads as to the process judging it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// The account that owns it.
    pub owner: u32,
    /// Its group.
    pub group: u32,
    /// Its mode, the sticky bit among it.
    pub mode: Mode,
    /// Whether access(2) says the judging process could write it, as an
    /// access list may grant where its mode doesn't, on a Mac especially.
    pub access_writes: bool,
}

/// The account a path is judged for: the judging process's own
/// effective user, its group and the others it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    /// Its effective user id.
    pub id: u32,
    /// Its effective group id.
    pub group: u32,
    /// The other groups it is in.
    pub groups: Vec<u32>,
}

impl User {
    /// The user this process runs as.
    #[must_use]
    pub fn this_process() -> Self {
        Self {
            id: rustix::process::geteuid().as_raw(),
            group: rustix::process::getegid().as_raw(),
            // None read is none trusted to hold it back, so a folder its
            // group could write is still taken by its other bits or by
            // access(2).
            groups: rustix::process::getgroups()
                .map(|groups| groups.into_iter().map(Gid::as_raw).collect())
                .unwrap_or_default(),
        }
    }

    /// Whether this user could write a folder reading as `folder`: root
    /// writes every one; any other by the owner's write bit where it owns
    /// it, the group's where the folder's group is one it is in, the
    /// others', or where access(2) says it could.
    fn could_write(&self, folder: &Reading) -> bool {
        let in_its_group = folder.group == self.group || self.groups.contains(&folder.group);
        self.id == 0
            || (folder.owner == self.id && folder.mode.contains(Mode::WUSR))
            || (in_its_group && folder.mode.contains(Mode::WGRP))
            || folder.mode.contains(Mode::WOTH)
            || folder.access_writes
    }

    /// Whether this user could plant a program in a folder reading as
    /// `folder`: it owns it, so could make it writable, or could write
    /// it.
    fn could_plant_in(&self, folder: &Reading) -> bool {
        folder.owner == self.id || self.could_write(folder)
    }

    /// Whether this user could put a folder of its own in place of one
    /// lying under a folder reading as `above`: it owns it, or could write
    /// it and it isn't sticky, as `/tmp` is, where it could rename only
    /// its own entries.
    fn could_replace_under(&self, above: &Reading) -> bool {
        above.owner == self.id || (self.could_write(above) && !above.mode.contains(Mode::SVTX))
    }
}

/// The folders a path names, as the judgement reads them.
pub trait Folders {
    /// Where `folder` leads, each link along it followed, where it is a
    /// folder the judging process can enter; none otherwise.
    fn leads(&self, folder: &Path) -> Option<PathBuf>;

    /// What `folder` reads as; none where it can't be read.
    fn reading(&self, folder: &Path) -> Option<Reading>;
}

/// The folders of this machine, as this process reads them.
#[derive(Debug, Clone, Copy, Default)]
pub struct Here;

impl Folders for Here {
    fn leads(&self, folder: &Path) -> Option<PathBuf> {
        let led = std::fs::canonicalize(folder).ok()?;
        let stat = rustix::fs::stat(&led).ok()?;
        let entered = FileType::from_raw_mode(stat.st_mode) == FileType::Directory
            && rustix::fs::access(&led, Access::EXEC_OK).is_ok();
        entered.then_some(led)
    }

    fn reading(&self, folder: &Path) -> Option<Reading> {
        let stat = rustix::fs::stat(folder).ok()?;
        Some(Reading {
            owner: stat.st_uid,
            group: stat.st_gid,
            mode: Mode::from_raw_mode(stat.st_mode),
            access_writes: rustix::fs::access(folder, Access::WRITE_OK).is_ok(),
        })
    }
}

/// A path judged: the folders it keeps, each by where it leads, and the
/// ones it drops, as they were given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Judged {
    /// The folders kept, in the path's order.
    pub kept: Vec<PathBuf>,
    /// The folders dropped, in the path's order, as the path named them.
    pub dropped: Vec<OsString>,
}

impl Judged {
    /// How the plain run's shell reads it, with no eval: the kept folders
    /// on one line, then the dropped ones on another, each joined by
    /// colons. A kept folder holds no colon or newline, since one that
    /// would is dropped; a dropped one holding a newline is written
    /// escaped, never raw.
    #[must_use]
    pub fn said(&self) -> Vec<u8> {
        let kept = self
            .kept
            .iter()
            .map(|folder| folder.as_os_str().as_bytes().to_vec());
        let dropped = self.dropped.iter().map(|folder| {
            let named = folder.as_bytes();
            if named.contains(&b'\n') {
                String::from_utf8_lossy(named)
                    .escape_debug()
                    .to_string()
                    .into_bytes()
            } else {
                named.to_vec()
            }
        });
        let mut said = kept.collect::<Vec<_>>().join(&b':');
        said.push(b'\n');
        said.extend(dropped.collect::<Vec<_>>().join(&b':'));
        said.push(b'\n');
        said
    }
}

/// `path` judged for this process's user, with `homes` (the home the run
/// is in and the `HOME` it is given) on this machine's folders.
#[must_use]
pub fn here(homes: [&OsStr; 2], path: &OsStr) -> Judged {
    judged(homes, path, &User::this_process(), &Here)
}

/// `path` judged for `user` on `folders`, with `homes` taken off it: each
/// folder named relatively, or that can't be entered, goes; each other
/// is named by where it leads, and goes where it, by name or by where it
/// leads, is in a home, where `user` could plant a program in it, or
/// where `user` could replace it from a folder above it, up to `/`. A
/// home that isn't a full path, or is `/`, takes nothing off. Empty
/// entries are passed over, named nowhere.
#[must_use]
pub fn judged(homes: [&OsStr; 2], path: &OsStr, user: &User, folders: &impl Folders) -> Judged {
    let homes = homes_of(homes, folders);
    let mut judged = Judged::default();
    for named in path.as_bytes().split(|&byte| byte == b':') {
        if named.is_empty() {
            continue;
        }
        let named = OsStr::from_bytes(named);
        match kept(Path::new(named), &homes, user, folders) {
            Some(led) => judged.kept.push(led),
            None => judged.dropped.push(named.to_owned()),
        }
    }
    judged
}

/// Each of `homes` that is a full path other than `/`, less one slash
/// it ends with, and where it leads, where it can be entered.
fn homes_of(homes: [&OsStr; 2], folders: &impl Folders) -> Vec<Vec<u8>> {
    let mut held = Vec::new();
    for home in homes {
        let named = home.as_bytes();
        if named.len() < 2 || !named.starts_with(b"/") {
            continue;
        }
        let named = named.strip_suffix(b"/").unwrap_or(named);
        held.push(named.to_vec());
        if let Some(led) = folders.leads(Path::new(OsStr::from_bytes(named))) {
            held.push(led.into_os_string().into_encoded_bytes());
        }
    }
    held
}

/// Where `folder` leads, where the path keeps it ([`judged`]).
fn kept(folder: &Path, homes: &[Vec<u8>], user: &User, folders: &impl Folders) -> Option<PathBuf> {
    if !folder.as_os_str().as_bytes().starts_with(b"/") {
        return None;
    }
    let led = folders.leads(folder)?;
    let shown = led.as_os_str().as_bytes();
    if shown.contains(&b':') || shown.contains(&b'\n') {
        return None;
    }
    let in_a_home = homes
        .iter()
        .any(|home| within(folder.as_os_str().as_bytes(), home) || within(shown, home));
    if in_a_home || user.could_plant_in(&folders.reading(&led)?) {
        return None;
    }
    for above in led.ancestors().skip(1) {
        if user.could_replace_under(&folders.reading(above)?) {
            return None;
        }
    }
    Some(led)
}

/// Whether the folder named `folder` is `home` or lies in it, by name.
fn within(folder: &[u8], home: &[u8]) -> bool {
    folder
        .strip_prefix(home)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(b"/"))
}

/// The program that judges a plain run's path: this one, by where it
/// leads, since the node that builds the run's words is this program.
/// None where it can't say where it is, and then the run refuses.
#[cfg(not(test))]
#[must_use]
pub fn judge() -> Option<PathBuf> {
    std::env::current_exe().and_then(std::fs::canonicalize).ok()
}

/// The program that judges a plain run's path in this crate's own unit
/// tests: a stand-in that runs this test program again as the judge
/// ([`tests::the_judges_stand_in`]), since the tests' program, unlike
/// mycelium, takes no subcommand.
#[cfg(test)]
#[must_use]
pub fn judge() -> Option<PathBuf> {
    Some(tests::the_judges_stand_in())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;
    use std::io::Write;
    use std::sync::OnceLock;

    /// The test that judges as the stand-in asks.
    const JUDGES: &str = "judges_as_its_stand_in_asks";

    /// A program that runs this test program as the judge: only the test
    /// [`JUDGES`], given the words a plain run gives mycelium, with what
    /// the test harness says thrown away and what the judge says on
    /// descriptor 3 put where its output goes. It is written beside this
    /// test program, once a run, so no temporary folder outlives it.
    pub(super) fn the_judges_stand_in() -> PathBuf {
        static MADE: OnceLock<PathBuf> = OnceLock::new();
        MADE.get_or_init(|| {
            let this = std::env::current_exe().expect("this test program");
            let name = this.file_name().expect("its name").to_string_lossy();
            let stand_in = this.with_file_name(format!("{name}.{SUBCOMMAND}"));
            let (_, tests) = module_path!().split_once("::").expect("in a crate");
            let quoted = |word: &str| format!("'{}'", word.replace('\'', r"'\''"));
            let script = format!(
                concat!(
                    "#!/bin/sh\n",
                    "exec {} --exact {} --quiet --test-threads=1 -- \"$@\" 3>&1 >/dev/null\n",
                ),
                quoted(&this.to_string_lossy()),
                quoted(&format!("{tests}::{JUDGES}")),
            );
            // Written apart and moved into place, so another test program
            // writing it at once never leaves it half written.
            let written =
                this.with_file_name(format!("{name}.{SUBCOMMAND}.{}", std::process::id()));
            fs::write(&written, script).expect("the stand-in is written");
            fs::set_permissions(
                &written,
                std::os::unix::fs::PermissionsExt::from_mode(0o755),
            )
            .expect("the stand-in runs");
            fs::rename(&written, &stand_in).expect("the stand-in is in place");
            stand_in
        })
        .clone()
    }

    /// Where a folder made here starts, as a Mac's temporary folder is
    /// behind a link.
    fn made() -> (tempfile::TempDir, PathBuf) {
        let made = tempfile::tempdir().expect("a temporary folder");
        let at = fs::canonicalize(made.path()).expect("the folder's place");
        (made, at)
    }

    // Followup 566: run as the stand-in, it is the judge, as mycelium's
    // subcommand is; run as a test, it judges a path through its stand-in,
    // so the stand-in the plain run tests use is shown to answer.
    #[test]
    fn judges_as_its_stand_in_asks() {
        let words: Vec<OsString> = std::env::args_os().collect();
        if let Some(at) = words.windows(2).position(|pair| pair[1] == SUBCOMMAND) {
            let given = &words[at + 2..];
            assert!(
                given.len() == 4 && given[0] == "--",
                "a plain run's words: {given:?}"
            );
            let said = here([given[1].as_os_str(), given[2].as_os_str()], &given[3]).said();
            fs::OpenOptions::new()
                .write(true)
                .open("/dev/fd/3")
                .and_then(|mut out| out.write_all(&said))
                .expect("what it judged is said");
            return;
        }
        let (_made, at) = made();
        let said = std::process::Command::new(the_judges_stand_in())
            .args([SUBCOMMAND, "--", "/nowhere/home", ""])
            .arg(format!("bin:{}", at.display()))
            .output()
            .expect("the stand-in runs");
        assert!(said.status.success(), "{said:?}");
        assert_eq!(
            String::from_utf8_lossy(&said.stdout),
            format!("\nbin:{}\n", at.display()),
            "a relative folder and one its user owns go"
        );
    }

    /// The folders of a machine, as a test lays them out: where each
    /// leads, and what each reads as.
    #[derive(Default)]
    struct Laid {
        leads: BTreeMap<PathBuf, PathBuf>,
        readings: BTreeMap<PathBuf, Reading>,
    }

    impl Laid {
        /// Lays `folder` out, leading to itself, reading as `reading`.
        fn with(mut self, folder: &str, reading: Reading) -> Self {
            self.leads.insert(folder.into(), folder.into());
            self.readings.insert(folder.into(), reading);
            self
        }
    }

    impl Folders for Laid {
        fn leads(&self, folder: &Path) -> Option<PathBuf> {
            self.leads.get(folder).cloned()
        }

        fn reading(&self, folder: &Path) -> Option<Reading> {
            self.readings.get(folder).copied()
        }
    }

    /// The user the judgement is for: kestrel, 1000, in group 100 and
    /// in 20 besides.
    fn kestrel() -> User {
        User {
            id: 1000,
            group: 100,
            groups: vec![20],
        }
    }

    /// A folder root owns, in root's group, with `mode`, that access(2)
    /// says kestrel can't write.
    fn roots(mode: rustix::fs::RawMode) -> Reading {
        Reading {
            owner: 0,
            group: 0,
            mode: Mode::from_raw_mode(mode),
            access_writes: false,
        }
    }

    /// The system's folders, which only root writes, with `folder` laid
    /// out under `/usr` as `reading`.
    fn system_with(folder: &str, reading: Reading) -> Laid {
        Laid::default()
            .with("/", roots(0o755))
            .with("/usr", roots(0o755))
            .with("/usr/bin", roots(0o755))
            .with(folder, reading)
    }

    /// Whether `folder`, laid out as `reading` under the system's, is
    /// kept on a path for `user`.
    fn keeps(user: &User, folder: &str, reading: Reading) -> bool {
        let laid = system_with(folder, reading);
        let path = format!("/usr/bin:{folder}");
        let judged = judged(
            [OsStr::new(""), OsStr::new("")],
            OsStr::new(&path),
            user,
            &laid,
        );
        assert_eq!(
            judged.kept.first().map(PathBuf::as_path),
            Some(Path::new("/usr/bin")),
            "the system's own folder is kept"
        );
        judged.kept.len() == 2
    }

    // Followup 566: the judgement reads the folder's own owner, group and
    // mode, so each way a user could write a folder is shown alone.
    #[test]
    fn a_folder_its_user_could_write_by_any_one_of_its_bits_goes() {
        let user = kestrel();
        assert!(
            keeps(&user, "/usr/tools", roots(0o775)),
            "a folder only root and its group write is kept"
        );
        for group in [100, 20] {
            let shared = Reading {
                group,
                ..roots(0o775)
            };
            assert!(
                !keeps(&user, "/usr/tools", shared),
                "a folder its group {group}, one the user is in, could write goes"
            );
        }
        assert!(
            !keeps(&user, "/usr/tools", roots(0o757)),
            "a folder any account could write goes"
        );
        let owned = Reading {
            owner: 1000,
            ..roots(0o555)
        };
        assert!(
            !keeps(&user, "/usr/tools", owned),
            "a folder its user owns goes, read-only, since its owner could make it writable"
        );
    }

    // Followup 566: the gap. A fence that binds a folder read-only makes
    // access(2) say its user can't write it, though outside the fence the
    // same user can, so a mode saying it can takes it off the path.
    #[test]
    fn a_folder_access_calls_read_only_goes_where_its_mode_says_its_user_could_write_it() {
        let bound = Reading {
            group: 100,
            ..roots(0o775)
        };
        assert!(!bound.access_writes, "access(2) says no");
        assert!(
            !keeps(&kestrel(), "/usr/tools", bound),
            "its group's write bit takes it off the path"
        );
    }

    // Followup 566: an access list may grant what the mode bits don't, as
    // on a Mac, so access(2) saying it could write a folder still takes it.
    #[test]
    fn a_folder_access_says_its_user_could_write_goes_whatever_its_mode() {
        let granted = Reading {
            access_writes: true,
            ..roots(0o755)
        };
        assert!(!keeps(&kestrel(), "/usr/tools", granted));
    }

    // Followup 566: an owner above could rename a folder away and put
    // its own in its place, but a sticky one, as /tmp is, only its own.
    #[test]
    fn a_folder_goes_under_one_its_user_could_write_unless_that_one_is_sticky() {
        let user = kestrel();
        let under = |above: Reading| {
            let laid = system_with("/usr/shared", above).with("/usr/shared/tools", roots(0o755));
            let path = OsStr::new("/usr/shared/tools");
            judged([OsStr::new(""), OsStr::new("")], path, &user, &laid)
        };
        assert_eq!(
            under(roots(0o1777)).kept,
            [PathBuf::from("/usr/shared/tools")],
            "under a sticky folder any account writes, as /tmp is, root's folder stays"
        );
        assert_eq!(
            under(roots(0o777)).dropped,
            [OsString::from("/usr/shared/tools")],
            "under one without the sticky bit, it goes"
        );
        let owned = Reading {
            owner: 1000,
            ..roots(0o1555)
        };
        assert_eq!(
            under(owned).dropped,
            [OsString::from("/usr/shared/tools")],
            "under one its user owns, sticky or not, it goes"
        );
    }

    // Followup 566: root could write any folder, so a run as root keeps
    // none, as the shell's own test said before.
    #[test]
    fn a_path_judged_for_root_keeps_no_folder() {
        let root = User {
            id: 0,
            group: 0,
            groups: Vec::new(),
        };
        let laid = system_with("/usr/tools", roots(0o555));
        let judged = judged(
            [OsStr::new(""), OsStr::new("")],
            OsStr::new("/usr/bin:/usr/tools"),
            &root,
            &laid,
        );
        assert_eq!(
            (judged.kept.len(), judged.dropped.len()),
            (0, 2),
            "{judged:?}"
        );
    }

    // Followup 566: the shell reads what the judge says as two lines,
    // with no eval, so no folder may break a line or a list.
    #[test]
    fn a_folder_that_would_break_what_the_judge_says_goes_and_is_never_said_raw() {
        let user = kestrel();
        let mut laid = system_with("/usr/to:ols", roots(0o755));
        laid.leads.insert("/usr/link".into(), "/usr/to:ols".into());
        let laid = laid.with("/usr/new\nline", roots(0o755));
        let judged = judged(
            [OsStr::new(""), OsStr::new("")],
            OsStr::new("/usr/bin:/usr/link:/usr/new\nline"),
            &user,
            &laid,
        );
        assert_eq!(
            String::from_utf8_lossy(&judged.said()),
            "/usr/bin\n/usr/link:/usr/new\\nline\n",
            "a folder leading to a name with a colon goes, and one named with a newline \
             is said escaped"
        );
    }

    // Followup 566: the real folders, as the judge reads them: one this
    // test's user made is its own, and goes; the system's stay.
    #[test]
    fn this_machines_folders_are_judged_by_who_owns_and_could_write_them() {
        let (_made, at) = made();
        let owned = at.join("owned");
        fs::create_dir(&owned).expect("a folder of its own");
        let sh = std::env::split_paths(&std::env::var_os("PATH").expect("a path"))
            .map(|folder| folder.join("sh"))
            .find(|program| program.is_file())
            .expect("sh is on the path");
        let system = fs::canonicalize(sh.parent().expect("the shell's folder"))
            .expect("the shell's folder's place");
        let path = [
            owned.as_os_str(),
            system.as_os_str(),
            OsStr::new("relative"),
        ]
        .map(|folder| folder.to_string_lossy().into_owned())
        .join(":");
        let judged = here([OsStr::new(""), OsStr::new("")], OsStr::new(&path));
        assert_eq!(
            judged,
            Judged {
                kept: vec![system],
                dropped: vec![owned.into_os_string(), "relative".into()],
            }
        );
    }
}
