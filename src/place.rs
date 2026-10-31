//! Where a node keeps things on its machine, by realm: its socket, its own
//! folder, the harness mesh's folders, the harness's quests and machine
//! queue, and what the machine is called.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::share::exchange::Places;

/// The socket the node for `realm` serves, in the login's runtime folder
/// ([`login_runtime`]), or the temporary folder on a system with none.
pub fn socket(realm: &str) -> PathBuf {
    runtime().join(format!("{realm}.sock"))
}

/// The folder of the sessions held in `realm` on this machine, each a
/// holder's socket and record, beside the node's socket.
pub fn held(realm: &str) -> PathBuf {
    runtime().join(format!("{realm}.held"))
}

/// The record of the harness's machine queue, which says the room the
/// machine has for more work: in the login's runtime folder, so nowhere
/// on a system with none, as the harness keeps it.
pub fn queue() -> Option<PathBuf> {
    login_runtime().map(|dir| dir.join("jobs").join("queue.json"))
}

/// The status of the machine's disk guard, which says how full each disk
/// is: in the login's runtime folder, so nowhere on a system with none, as
/// the guard keeps it.
pub fn disk_guard() -> Option<PathBuf> {
    login_runtime().map(|dir| dir.join("disk-guard").join("status.json"))
}

/// The socket the machine's updater takes asks on, where the system's
/// configuration puts it ([`crate::update`]).
pub fn updater() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/var/run/update-to-main.sock")
    } else {
        PathBuf::from("/run/update-to-main/ask.sock")
    }
}

/// Where the machine's updater keeps its last update's ending, beside its
/// socket.
pub fn updated() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/var/run/update-to-main.last.json")
    } else {
        PathBuf::from("/run/update-to-main/last.json")
    }
}

/// The login's own runtime folder, as the harness finds it: what
/// `XDG_RUNTIME_DIR` names, or on macOS, which has none, the account's own
/// temporary folder as the system names it. Not `TMPDIR`, which the
/// harness moves for a session's quest. None on any other system without
/// one.
fn login_runtime() -> Option<PathBuf> {
    static DARWIN: OnceLock<Option<PathBuf>> = OnceLock::new();
    runtime_of(
        env::var_os("XDG_RUNTIME_DIR"),
        cfg!(target_os = "macos"),
        || DARWIN.get_or_init(darwin_temporary).clone(),
    )
}

/// The runtime folder `xdg` names, else on a Mac (`mac`) the one
/// `darwin` gives.
fn runtime_of(
    xdg: Option<OsString>,
    mac: bool,
    darwin: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    match xdg {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ if mac => darwin(),
        _ => None,
    }
}

/// The account's own temporary folder on macOS, as `getconf` says it, or
/// none if it won't.
fn darwin_temporary() -> Option<PathBuf> {
    let said = Command::new("/usr/bin/getconf")
        .arg("DARWIN_USER_TEMP_DIR")
        .output()
        .ok()?;
    let dir = String::from_utf8(said.stdout).ok()?;
    let dir = dir.trim();
    (said.status.success() && !dir.is_empty()).then(|| PathBuf::from(dir))
}

/// What this machine's configuration declares of it, where the account's
/// built configuration writes it.
pub fn declared() -> PathBuf {
    xdg("XDG_CONFIG_HOME", &[".config"])
        .join("mycelium")
        .join("declared.json")
}

/// The node's record of which networks this machine's probes reached,
/// beside its sockets, for every realm's node and link to read.
pub fn reached() -> PathBuf {
    runtime().join("reached.json")
}

fn runtime() -> PathBuf {
    login_runtime()
        .unwrap_or_else(env::temp_dir)
        .join("mycelium")
}

/// Mycelium's own folder on this machine: the realm it belongs to, and the
/// claims of the links that carry other machines' sessions here.
pub fn state() -> PathBuf {
    state_home().join("mycelium")
}

/// The harness mesh's folders on this machine, where the harness keeps
/// them.
pub fn mesh() -> PathBuf {
    state_home()
        .join("pi")
        .join("agentic-harness.pi")
        .join("mesh-workflow")
}

/// The sessions' steer inboxes on this machine, a folder an address, where
/// the harness reads them.
pub fn steer() -> PathBuf {
    state_home()
        .join("pi")
        .join("agentic-harness.pi")
        .join("steer")
}

/// Where this machine keeps shared quests, as the harness keeps them: the
/// quest store, the quests' scratch folders and the account's home, with
/// mycelium's own folder `state` for its ledgers.
pub fn shares(state: &Path) -> Places {
    Places {
        quests: xdg("XDG_DATA_HOME", &[".local", "share"])
            .join("pi")
            .join("agentic-harness.pi")
            .join("quest-workflow")
            .join("quests"),
        scratch: xdg("XDG_CACHE_HOME", &[".cache"])
            .join("pi")
            .join("agentic-harness.pi")
            .join("quest-workspace"),
        home: env::home_dir().unwrap_or_else(env::temp_dir),
        state: state.to_owned(),
    }
}

/// This machine's name, as the system gives it, up to its first dot.
pub fn machine() -> String {
    let name = rustix::system::uname()
        .nodename()
        .to_string_lossy()
        .into_owned();
    match name.split_once('.') {
        Some((short, _)) => short.to_owned(),
        None => name,
    }
}

/// Settles that this machine belongs to `realm`, recording it in `state`
/// the first time. A machine has one mesh, so it belongs to one realm: a
/// second realm would carry the first one's messages into it.
pub fn settle_realm(state: &Path, realm: &str) -> io::Result<()> {
    let marker = state.join("realm");
    match fs::read_to_string(&marker) {
        Ok(held) if held.trim() == realm => Ok(()),
        Ok(held) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "this machine belongs to the {} realm, so it can't join {realm}; {} says so",
                held.trim(),
                marker.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(state)?;
            fs::write(marker, format!("{realm}\n"))
        }
        Err(error) => Err(error),
    }
}

fn state_home() -> PathBuf {
    xdg("XDG_STATE_HOME", &[".local", "state"])
}

/// The folder an XDG variable names, or its default under the home folder.
fn xdg(variable: &str, default: &[&str]) -> PathBuf {
    env::var_os(variable)
        .map(PathBuf::from)
        .or_else(|| {
            env::home_dir().map(|home| default.iter().fold(home, |path, name| path.join(name)))
        })
        .unwrap_or_else(env::temp_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mac_keeps_its_runtime_folder_where_the_system_says() {
        let darwin = || Some(PathBuf::from("/var/folders/xb/T"));
        assert_eq!(
            runtime_of(None, true, darwin),
            Some(PathBuf::from("/var/folders/xb/T")),
            "a Mac, with no XDG_RUNTIME_DIR, has the account's temporary folder"
        );
        assert_eq!(
            runtime_of(Some("/run/user/501".into()), true, darwin),
            Some(PathBuf::from("/run/user/501")),
            "one that names a runtime folder keeps it"
        );
        assert_eq!(
            runtime_of(None, false, darwin),
            None,
            "another system without one has none"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_mac_names_its_own_temporary_folder() {
        let dir = darwin_temporary().expect("getconf names it");
        assert!(dir.is_dir(), "{} is a folder", dir.display());
    }

    #[test]
    fn a_machine_stays_in_the_realm_it_first_joined() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        settle_realm(dir.path(), "work").expect("the first realm is taken");
        settle_realm(dir.path(), "work").expect("and taken again");
        assert!(
            settle_realm(dir.path(), "personal").is_err(),
            "a second realm is refused"
        );
    }
}
