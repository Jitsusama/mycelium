//! Where a node keeps things on its machine, by realm: its socket, its own
//! folder, the harness mesh's folders, and what the machine is called.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The socket the node for `realm` serves: in the login's runtime folder
/// where the system has one (Linux), otherwise in the account's own
/// temporary folder (macOS gives each account its own).
pub fn socket(realm: &str) -> PathBuf {
    let base = env::var_os("XDG_RUNTIME_DIR").map_or_else(env::temp_dir, PathBuf::from);
    base.join("mycelium").join(format!("{realm}.sock"))
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
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::home_dir().map(|home| home.join(".local").join("state")))
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
