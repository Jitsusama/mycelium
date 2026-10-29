//! Where a node keeps things on its machine, by realm: its socket, its own
//! folder, the harness mesh's folders, the harness's quests and machine
//! queue, and what the machine is called.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::share::exchange::Places;

/// The socket the node for `realm` serves: in the login's runtime folder
/// where the system has one (Linux), otherwise in the account's own
/// temporary folder (macOS gives each account its own).
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
    env::var_os("XDG_RUNTIME_DIR").map(|dir| PathBuf::from(dir).join("jobs").join("queue.json"))
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
    env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(env::temp_dir, PathBuf::from)
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
