//! Whether the mycelium a process runs is still the one installed. A
//! node is restarted when its machine's configuration changes, but a
//! link's far end, started over SSH or by a provider's program, is not:
//! it would run the build it began with for as long as the link lasts,
//! and a newer build's facts or frames would never reach the other end.
//! So a far end watches what its search path names, and once that is
//! another build, it ends, for its dialler to start the new one.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How often a far end looks at what is installed.
const LOOK_EVERY: Duration = Duration::from_secs(60);

/// The `mycelium` that the search path `path` names first, with every
/// link on the way resolved, as a shell starting `mycelium` would run it.
#[must_use]
pub fn installed(path: Option<&OsStr>) -> Option<PathBuf> {
    env::split_paths(path?)
        .map(|folder| folder.join("mycelium"))
        .find(|candidate| candidate.is_file())
        .and_then(|found| fs::canonicalize(found).ok())
}

/// The build installed now, if it isn't `running`, the one this process
/// began as.
#[must_use]
pub fn newer(running: &Path, installed: Option<PathBuf>) -> Option<PathBuf> {
    installed.filter(|found| found != running)
}

/// The build this process began as, with every link resolved, read once
/// at its start: a link resolved later may name a newer build already.
#[must_use]
pub fn running() -> Option<PathBuf> {
    env::current_exe().and_then(fs::canonicalize).ok()
}

/// Waits until the search path names another build than `running`, and
/// says which. Never, where this process can't say what it runs.
pub async fn replaced(running: Option<PathBuf>) -> PathBuf {
    let Some(running) = running else {
        return std::future::pending().await;
    };
    let mut looking =
        tokio::time::interval_at(tokio::time::Instant::now() + LOOK_EVERY, LOOK_EVERY);
    loop {
        looking.tick().await;
        if let Some(newer) = newer(&running, installed(env::var_os("PATH").as_deref())) {
            return newer;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// A build named `name` in `store`, as an executable file.
    fn build(store: &Path, name: &str) -> PathBuf {
        let bin = store.join(name).join("bin");
        fs::create_dir_all(&bin).expect("a build's folder");
        let program = bin.join("mycelium");
        fs::write(&program, "#!/bin/sh\n").expect("a build");
        program
    }

    #[test]
    fn the_first_mycelium_the_search_path_names_is_the_one_installed() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let empty = dir.path().join("empty");
        fs::create_dir_all(&empty).expect("a folder");
        let first = build(dir.path(), "first");
        let second = build(dir.path(), "second");
        let path: OsString = env::join_paths([
            empty,
            first.parent().expect("its bin").to_owned(),
            second.parent().expect("its bin").to_owned(),
        ])
        .expect("a search path");
        assert_eq!(
            installed(Some(&path)),
            fs::canonicalize(&first).ok(),
            "a folder without one is passed over, and the first found is taken"
        );
        assert_eq!(installed(None), None, "no search path names nothing");
    }

    #[test]
    fn a_profile_moved_to_another_build_names_it() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let old = build(dir.path(), "old");
        let new = build(dir.path(), "new");
        let profile = dir.path().join("profile");
        std::os::unix::fs::symlink(old.parent().expect("its bin"), &profile).expect("linked");
        let path = profile.clone().into_os_string();
        let running = installed(Some(&path)).expect("the old build");

        assert_eq!(
            newer(&running, installed(Some(&path))),
            None,
            "while the profile names it, the build running is the one installed"
        );
        fs::remove_file(&profile).expect("unlinked");
        std::os::unix::fs::symlink(new.parent().expect("its bin"), &profile).expect("relinked");
        assert_eq!(
            newer(&running, installed(Some(&path))),
            fs::canonicalize(&new).ok(),
            "once it names another, that one is newer"
        );
    }
}
