//! Where a node keeps things on its machine, by realm: its socket and the
//! harness mesh's folders, and what the machine is called.

use std::env;
use std::path::PathBuf;

/// The socket the node for `realm` serves: in the login's runtime folder
/// where the system has one (Linux), otherwise in the account's own
/// temporary folder (macOS gives each account its own).
pub fn socket(realm: &str) -> PathBuf {
    let base = env::var_os("XDG_RUNTIME_DIR").map_or_else(env::temp_dir, PathBuf::from);
    base.join("mycelium").join(format!("{realm}.sock"))
}

/// The harness mesh's folders on this machine, where the harness keeps
/// them.
pub fn mesh() -> PathBuf {
    let state = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::home_dir().map(|home| home.join(".local").join("state")))
        .unwrap_or_else(env::temp_dir);
    state
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
