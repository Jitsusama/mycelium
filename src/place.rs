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
pub fn login_runtime() -> Option<PathBuf> {
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

/// The account's theme, where its built configuration writes it: the
/// marks and colours every surface of the account draws from.
pub fn theme() -> PathBuf {
    xdg("XDG_CONFIG_HOME", &[".config"])
        .join("jitsusama")
        .join("theme.json")
}

/// The node's record of which networks this machine's probes reached,
/// beside its sockets, for every realm's node and link to read.
pub fn reached() -> PathBuf {
    runtime().join("reached.json")
}

/// The folder every realm's node on this machine serves its socket in.
pub fn runtime() -> PathBuf {
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

/// The quests' waiting files on this machine, one a quest, where the
/// launcher keeps what each waits on its person for.
pub fn waiting() -> PathBuf {
    state_home().join("mission").join("waiting")
}

/// Where the harness's work broker on this machine cuts its work trees,
/// a folder a tree, and records the sessions holding each, and where pi
/// records the quest each session works.
pub fn trees() -> crate::node::Trees {
    let harness = state_home().join("pi").join("agentic-harness.pi");
    let work = harness.join("work");
    crate::node::Trees {
        root: work.join("trees"),
        records: work.join("cut"),
        sessions: harness.join("quest-workflow").join("sessions"),
    }
}

/// Where pi's hosts on this machine each keep their record of the
/// sessions they hold, a file named by the host's process and its start.
pub fn hosts() -> PathBuf {
    state_home()
        .join("pi")
        .join("agentic-harness.pi")
        .join("hosts")
}

/// Where pi keeps each session's log, in a folder of its working folder's:
/// under `PI_CODING_AGENT_DIR` where that is set, as pi reads it, else in
/// the home's `.pi/agent`.
pub fn logs() -> PathBuf {
    env::var_os("PI_CODING_AGENT_DIR")
        .map_or_else(
            || {
                env::home_dir()
                    .unwrap_or_else(env::temp_dir)
                    .join(".pi")
                    .join("agent")
            },
            PathBuf::from,
        )
        .join("sessions")
}

/// Where the harness marks each session it parks on this machine, a file
/// named by the session's id.
pub fn parked() -> PathBuf {
    state_home()
        .join("pi")
        .join("agentic-harness.pi")
        .join("park-workflow")
        .join("parked")
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

/// The name this machine's node goes by: `given`, as its `--machine`
/// says it, else the system's ([`named`]).
pub fn node_machine(given: Option<&str>) -> io::Result<String> {
    named(given, None, &nodename()).map_err(|why| io::Error::new(io::ErrorKind::InvalidInput, why))
}

/// The name a command on this machine goes by: `given`, as its
/// `--machine` says it, else the one its node recorded in `state`, else
/// the system's ([`named`]). A link the other end starts here is given
/// no `--machine`, so the record is what names it as its node is named.
pub fn machine(given: Option<&str>, state: &Path) -> io::Result<String> {
    let recorded = recorded_machine(state)?;
    named(given, recorded.as_deref(), &nodename())
        .map_err(|why| io::Error::new(io::ErrorKind::InvalidInput, why))
}

/// The system's name for this machine, its nodename.
fn nodename() -> String {
    rustix::system::uname()
        .nodename()
        .to_string_lossy()
        .into_owned()
}

/// The name a machine goes by: `given`, as `--machine` says it, else
/// `recorded`, as its node keeps it, else `system`'s, up to its first
/// dot. Whichever it is must be a mesh name, since the store names files
/// by it; one that isn't is refused with how to name the machine instead,
/// as a guest whose system calls it `(none)` must be.
fn named(given: Option<&str>, recorded: Option<&str>, system: &str) -> Result<String, String> {
    let mesh_name = |name: &str| crate::store::folders::safe(name).is_ok();
    if let Some(given) = given {
        return if mesh_name(given) {
            Ok(given.to_owned())
        } else {
            Err(format!(
                "--machine {given:?} is not a mesh name: use letters, digits, -, _ and @"
            ))
        };
    }
    if let Some(recorded) = recorded {
        return if mesh_name(recorded) {
            Ok(recorded.to_owned())
        } else {
            Err(format!(
                "this machine's node recorded its name as {recorded:?}, which is not a mesh \
                 name: pass --machine to name it"
            ))
        };
    }
    let short = system.split_once('.').map_or(system, |(short, _)| short);
    if mesh_name(short) {
        Ok(short.to_owned())
    } else {
        Err(format!(
            "this machine's system name, {short:?}, is not a mesh name, and nothing else \
             names it: pass --machine to name it"
        ))
    }
}

/// Where this machine's node, keeping its things in `state`, records the
/// name it goes by.
fn machine_record(state: &Path) -> PathBuf {
    state.join("machine")
}

/// Records in `state` that this machine's node goes by `name`, for every
/// command on the machine to go by it too. It is written whole, beside
/// its place and then moved there, so a command never reads half of it.
pub fn record_machine(state: &Path, name: &str) -> io::Result<()> {
    fs::create_dir_all(state)?;
    let record = machine_record(state);
    let staged = state.join(".machine.staged");
    fs::write(&staged, format!("{name}\n"))?;
    fs::rename(&staged, &record)
}

/// The name this machine's node recorded in `state`, if it has.
pub fn recorded_machine(state: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(machine_record(state)) {
        Ok(held) => Ok(Some(held.trim().to_owned()).filter(|name| !name.is_empty())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
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
    fn a_machine_goes_by_the_name_given_then_its_nodes_then_the_systems() {
        assert_eq!(
            named(Some("kestrel"), Some("skiff"), "harbour.local"),
            Ok("kestrel".to_owned()),
            "the name given is taken over any other"
        );
        assert_eq!(
            named(None, Some("skiff"), "harbour.local"),
            Ok("skiff".to_owned()),
            "with none given, the one its node recorded"
        );
        assert_eq!(
            named(None, None, "skiff.local"),
            Ok("skiff".to_owned()),
            "with nothing naming it, the system's, up to its first dot"
        );
    }

    #[test]
    fn a_system_name_that_is_no_mesh_name_is_refused_saying_to_name_it() {
        let refused = named(None, None, "(none)").expect_err("(none) names no machine");
        assert!(
            refused.contains("\"(none)\"") && refused.contains("--machine"),
            "the refusal says which name and how to give one: {refused}"
        );
        assert_eq!(
            named(Some("kestrel"), None, "(none)"),
            Ok("kestrel".to_owned()),
            "a machine named otherwise needs no system name"
        );
        let refused = named(Some("a/b"), None, "kestrel").expect_err("a/b names no machine");
        assert!(
            refused.contains("--machine") && refused.contains("\"a/b\""),
            "a name given is held to the same: {refused}"
        );
        let refused = named(None, Some("a b"), "kestrel").expect_err("a b names no machine");
        assert!(
            refused.contains("--machine"),
            "and so is one recorded: {refused}"
        );
    }

    #[test]
    fn a_node_records_its_name_for_every_command_on_its_machine() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        assert_eq!(
            recorded_machine(dir.path()).expect("nothing recorded reads"),
            None,
            "before its node starts, nothing is recorded"
        );
        record_machine(dir.path(), "kestrel").expect("its name is recorded");
        assert_eq!(
            recorded_machine(dir.path()).expect("the record reads"),
            Some("kestrel".to_owned()),
            "after, the name its node goes by"
        );
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
