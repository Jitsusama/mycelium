//! What a machine's own configuration says of it: what can't be measured
//! from the machine, such as the networks work there reaches, what it
//! refuses, how many processors it is given where it shows more, or when
//! it leaves. Only the machine's built configuration declares it, as a
//! file it writes where every end of a link on the machine finds it;
//! nothing said at run time changes it. A machine whose configuration says
//! nothing, or says it in a shape this build doesn't read, declares
//! nothing known, never a guess.
//!
//! A refusal is a taint, as a cluster scheduler's are: work goes where
//! no taint it doesn't tolerate applies, and the machine itself enforces
//! it, since what is said here only saves work a trip.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What a machine's configuration says of it. Tests alone have a
/// default, declaring nothing at all: no reach, no taints, no fence,
/// which as a real machine's word would read as looser ground.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Default))]
pub struct Declared {
    /// The networks work there always reaches, such as `internet`.
    pub reach: Vec<String>,
    /// The networks work there reaches only some of the time, such as a
    /// laptop's `home`, each reached while its probe answers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub probes: BTreeMap<String, Probe>,
    /// What a session held there outlives.
    pub outlives: Vec<Outlives>,
    /// Whether work there costs money.
    pub costs_money: bool,
    /// What it refuses, or would rather not take.
    pub taints: Vec<Taint>,
    /// How many processors work there may use, where the machine shows more
    /// than it is given, as a virtual machine held to a share of its host
    /// does; the processors it lists where it says nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cores: Option<u32>,
    /// The command an agent session starts with there, by realm, such as
    /// `pi` in one and `pi-claude` in another, for whatever starts one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agents: BTreeMap<String, String>,
    /// When it leaves the realm, as a machine lent for a while does: it
    /// takes no work that would outlast it, and once it has left, it is
    /// no longer one of the realm's machines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaves_at: Option<jiff::Timestamp>,
    /// The machine it runs within, as a virtual machine runs in its host,
    /// whose power and room it has as its own (`within`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub within: Option<Within>,
    /// The machines that run within it, each told its power and room.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<Within>,
    /// The fences an agent session runs in there, by kind, each with the
    /// doors it opens; unknown where it says none, or says it in a shape
    /// this build doesn't read, so a carry ordered there is refused.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "fence_or_none"
    )]
    pub fence: Option<BTreeMap<Kind, Doors>>,
}

/// The fence an agent session runs in, as the harness names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// None: the session runs as its account does.
    None,
    /// The agent fence, open through the doors it names.
    Fence,
    /// The strict fence, shut but for the doors it names.
    Strict,
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Kind::None => "unfenced",
            Kind::Fence => "fenced",
            Kind::Strict => "strictly fenced",
        })
    }
}

/// What a fence lets a session reach: each by the name the machine's
/// configuration gives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Doors {
    /// The machines it may build on.
    pub builders: BTreeSet<String>,
    /// The forges it may push to and fetch from.
    pub forges: BTreeSet<String>,
    /// The secrets it may be released.
    pub secrets: BTreeSet<String>,
    /// The words a command is put after to run in this fence there, the
    /// first a program the Nix store holds; none where nothing runs in it
    /// for another machine or a command line, which refuses an agent's
    /// run (`launcher`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<String>,
}

/// A fence as declared, or none where it isn't in the shape this build
/// reads, which spoils only the fence, never the rest of what's declared.
fn fence_or_none<'de, D: serde::Deserializer<'de>>(
    said: D,
) -> Result<Option<BTreeMap<Kind, Doors>>, D::Error> {
    Ok(Option::<serde_json::Value>::deserialize(said)?
        .and_then(|fence| serde_json::from_value(fence).ok()))
}

/// One machine within another, as either declares it: the other machine,
/// and the file, shared between the two, in which the host says its power
/// and room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Within {
    /// The other machine's name.
    pub machine: String,
    /// The file the host says them in, as this machine reaches it.
    pub file: PathBuf,
}

impl Declared {
    /// What it declares as `realm` hears it: the command its agent
    /// sessions start with in that realm only, so no realm learns what
    /// another starts, or that the machine belongs to another.
    #[must_use]
    pub fn in_realm(mut self, realm: &str) -> Self {
        self.agents.retain(|named, _| named == realm);
        self
    }

    /// Whether it is a worker: a machine lent or paid for, which work goes
    /// to but which isn't one of the person's own lasting machines, so
    /// nothing but the work is carried there.
    #[must_use]
    pub fn is_worker(&self) -> bool {
        self.costs_money
            || self.leaves_at.is_some()
            || self.reach.iter().any(|network| network == "aquifer")
    }

    /// Whether it has left by `now`.
    #[must_use]
    pub fn has_left(&self, now: jiff::Timestamp) -> bool {
        self.leaves_at.is_some_and(|leaves| leaves <= now)
    }
}

/// How a network is found reached, as the machine's configuration knows
/// best: by an address only that network serves, or by a test of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Probe {
    /// A connection that opens to this `host:port`.
    Connect(String),
    /// A program and its words, run with no shell, that exits 0. It suits
    /// a network no address tells apart, such as one known by its Wi-Fi
    /// or a tunnel's link, which only the machine itself can say.
    Run(Vec<String>),
}

/// Something a held session can outlive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outlives {
    /// The window it shows in closing.
    WindowClose,
    /// Its node restarting.
    NodeRestart,
    /// The machine sleeping.
    Sleep,
    /// The machine restarting.
    Reboot,
}

/// A refusal, by a key work tolerates it by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Taint {
    /// What work names to tolerate it, such as `person-works`.
    pub key: String,
    /// How hard it refuses.
    pub effect: Effect,
    /// Why, for a person reading where work went.
    pub why: String,
}

/// How hard a taint refuses work that doesn't tolerate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// The work never goes there.
    Never,
    /// The work goes there only when nowhere else will do.
    PreferNot,
}

/// What the configuration at `file` declares, or nothing known where it
/// declares nothing. A file in a shape this build doesn't read is told
/// of, since its configuration meant to say something.
#[must_use]
pub fn read(file: &Path) -> Option<Declared> {
    let text = match fs::read(file) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            eprintln!("what {} declares can't be read: {error}", file.display());
            return None;
        }
    };
    serde_json::from_slice(&text)
        .inspect_err(|error| {
            eprintln!("what {} declares isn't read: {error}", file.display());
        })
        .ok()
}

/// Where Nix keeps what it builds, which only Nix writes.
const STORE: &str = "/nix/store";

/// pi-fence's word for a run in a work tree shared to its asker.
const SHARED_TREE: &str = "--shared-tree";

/// The words that run a command in this machine's fence `kind`, as the
/// configuration at `file` declares them. They are taken only from a file
/// the agents can't write, since they say what the fence is: one in the
/// Nix store, or one that only `declarers` own, with nothing on its way
/// down from the root another may write. The first word must be a
/// program in the store too, so no agent can put another in its place.
/// They end with `--`, so nothing an ask says after them is read as the
/// launcher's own options, whatever the launcher. They never say
/// `--shared-tree`, nor any shortening of it, which would spare every run
/// a work tree pi-fence otherwise refuses: this node doesn't yet say which
/// trees are shared to whom, so it never asks that.
///
/// # Errors
///
/// When the file isn't sealed so, declares no words for `kind`, names a
/// program outside the store, doesn't end with `--`, or says
/// `--shared-tree`.
pub fn launcher(file: &Path, kind: Kind, declarers: &[u32]) -> Result<Vec<String>, String> {
    sealed(file, declarers).map_err(|why| {
        format!(
            "what {} declares can't say how a {kind} command runs: {why}",
            file.display()
        )
    })?;
    let runs = read(file)
        .and_then(|declared| declared.fence)
        .and_then(|mut fences| fences.remove(&kind))
        .map(|doors| doors.runs)
        .unwrap_or_default();
    let Some(program) = runs.first() else {
        return Err(format!(
            "this machine declares no way to run a command as its {kind} sessions run"
        ));
    };
    let found = fs::canonicalize(program)
        .map_err(|error| format!("its {kind} sessions' launcher {program} isn't there: {error}"))?;
    if !found.starts_with(STORE) {
        return Err(format!(
            "its {kind} sessions' launcher {program} isn't in the Nix store, so it isn't \
             trusted to fence"
        ));
    }
    if let Some(spare) = runs.iter().find(|word| spares_a_tree(word)) {
        return Err(format!(
            "its {kind} sessions' launcher in {} says {spare}, which spares a tree shared \
             to the asker; this node can't yet say which trees are shared, so it never asks \
             that: take it out of the declaration",
            file.display()
        ));
    }
    if runs.last().map(String::as_str) != Some("--") {
        return Err(format!(
            "its {kind} sessions' launcher in {} doesn't end with --, so what an ask says \
             after it could be read as the launcher's own options",
            file.display()
        ));
    }
    Ok(runs)
}

/// Whether `word` asks a launcher to spare a shared tree: `--shared-tree`,
/// with a value or without, or a shortening of it past `--s` that a
/// parser taking unique prefixes would read as it.
fn spares_a_tree(word: &str) -> bool {
    let name = word.split('=').next().unwrap_or(word);
    word.starts_with(SHARED_TREE) || (name.len() > "--s".len() && SHARED_TREE.starts_with(name))
}

/// What a node says as it starts: the file it reads this machine's
/// declaration from, and how each fence's agents run here, so a changed
/// file or launcher shows in its journal as well as in a restart.
#[must_use]
pub fn said_at_start(file: &Path, declarers: &[u32]) -> String {
    let runs = [Kind::Fence, Kind::Strict].map(|kind| match launcher(file, kind, declarers) {
        Ok(runs) => format!("{kind} agents run through {}", runs.join(" ")),
        Err(why) => format!("{kind} agents run nothing: {why}"),
    });
    format!(
        "reads what this machine declares from {}; {}",
        file.display(),
        runs.join("; ")
    )
}

/// What a node says as it starts of the realm's kit it was given in
/// `kit`: what it holds, which every session and run it starts finds first
/// on its path, and whether an account but `declarers` could change that,
/// since an agent among them could put its own program in a tool's place.
/// The verdict comes before the tools, and each is said escaped, so a
/// tool's name can't pass for the verdict.
#[must_use]
pub fn kit_said_at_start(kit: &Path, declarers: &[u32]) -> String {
    let Some(listed) = crate::facts::kit(kit) else {
        let why = fs::read_dir(kit.join("bin"))
            .err()
            .map_or_else(|| "it moved as it was read".to_owned(), |e| e.to_string());
        return format!(
            "puts the realm's kit at {} first on each path, but can't list its bin ({why}), so \
             names no kit",
            kit.display()
        );
    };
    // Each tool as well as the folder holding it, since a tool that is a
    // link is only as sealed as where it leads.
    let bin = kit.join("bin");
    let verdict = sealed(&bin, declarers)
        .and_then(|()| {
            listed
                .tools
                .iter()
                .try_for_each(|tool| sealed(&bin.join(tool), declarers))
        })
        .map_or_else(
            |why| format!("an agent could change what its tools are: {why}"),
            |()| "only its declarers can change its tools".to_owned(),
        );
    let tools: Vec<String> = listed
        .tools
        .iter()
        .map(|tool| tool.escape_debug().to_string())
        .collect();
    format!(
        "puts the realm's kit at {} first on each path; {verdict}; {} tools: {}",
        listed.path.escape_debug(),
        tools.len(),
        tools.join(" ")
    )
}

/// How many links a path may pass through before it is taken as a loop,
/// as Linux counts them.
const HOPS: usize = 40;

/// Whether no account but `declarers` can change what `file` names. The
/// path is walked as given, link by link, since a link is only as sealed as
/// what holds it: one an agent may repoint seals nothing, even where it
/// points into the store. Each folder on the way down from the root and
/// each link passed through belongs to one of them and lets no other
/// write, but for a sticky folder, where nobody else may move what isn't
/// theirs. Within the Nix store nothing changes, so nothing there is asked.
pub(crate) fn sealed(file: &Path, declarers: &[u32]) -> Result<(), String> {
    let given = std::path::absolute(file).map_err(|error| format!("it isn't there: {error}"))?;
    let mut at = PathBuf::from("/");
    trusted(
        &at,
        &fs::symlink_metadata(&at).map_err(|e| e.to_string())?,
        declarers,
    )?;
    let mut ahead = parts(&given);
    let mut hops = 0;
    while let Some(part) = ahead.pop() {
        if part == ".." {
            at.pop();
            continue;
        }
        let next = at.join(&part);
        let said = fs::symlink_metadata(&next)
            .map_err(|error| format!("{} can't be looked at: {error}", next.display()))?;
        if !(next.starts_with(STORE) && next != Path::new(STORE)) {
            trusted(&next, &said, declarers)?;
        }
        if said.file_type().is_symlink() {
            hops += 1;
            if hops > HOPS {
                return Err(format!("it passes through more than {HOPS} links"));
            }
            let target = fs::read_link(&next)
                .map_err(|error| format!("{} can't be read: {error}", next.display()))?;
            if target.is_absolute() {
                at = PathBuf::from("/");
            }
            ahead.extend(parts(&target));
        } else {
            at = next;
        }
    }
    Ok(())
}

/// The names `path` walks through, last first, so the next is popped.
fn parts(path: &Path) -> Vec<std::ffi::OsString> {
    use std::path::Component;

    path.components()
        .rev()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name.to_owned()),
            Component::ParentDir => Some("..".into()),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => None,
        })
        .collect()
}

/// Whether `entry`, as `said` describes it, belongs to one of `declarers`
/// and lets no other write it. A link's own mode is never used, so only
/// its owner is asked.
fn trusted(entry: &Path, said: &fs::Metadata, declarers: &[u32]) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    if !declarers.contains(&said.uid()) {
        return Err(format!(
            "{} belongs to account {}, which isn't trusted to declare it",
            entry.display(),
            said.uid()
        ));
    }
    let sticky = said.is_dir() && said.mode() & 0o1000 != 0;
    if !said.file_type().is_symlink() && said.mode() & 0o022 != 0 && !sticky {
        return Err(format!(
            "{} may be written by others than its owner",
            entry.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// The accounts a test trusts to declare: root, its own, and the one a
    /// build's sandbox shows root as.
    fn declarers() -> Vec<u32> {
        vec![0, 65534, rustix::process::getuid().as_raw()]
    }

    /// Where `name` is found on the path, as the store holds it.
    fn stored(name: &str) -> PathBuf {
        std::env::split_paths(&std::env::var_os("PATH").expect("a path"))
            .map(|folder| folder.join(name))
            .find(|program| program.exists())
            .expect("the program is on the path")
    }

    /// A configuration whose `kind` fence runs a command after `runs`.
    fn runs_through(file: &Path, runs: &[&str]) {
        let declared = serde_json::json!({
            "reach": [], "outlives": [], "costs_money": false, "taints": [],
            "fence": {"strict": {"builders": [], "forges": [], "secrets": [],
                                  "runs": runs}}
        });
        fs::write(file, declared.to_string()).expect("the configuration is written");
    }

    #[test]
    fn a_node_says_as_it_starts_where_it_reads_and_how_each_fence_runs() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        let env = stored("env");
        let env = env.to_str().expect("a path in words");
        runs_through(&file, &[env, "FENCED=1", "--"]);
        assert_eq!(
            said_at_start(&file, &declarers()),
            format!(
                "reads what this machine declares from {}; fenced agents run nothing: this \
                 machine declares no way to run a command as its fenced sessions run; \
                 strictly fenced agents run through {env} FENCED=1 --",
                file.display()
            )
        );
        let own = rustix::process::getuid().as_raw();
        let refused = said_at_start(&file, &[0, 65534]);
        assert!(
            refused.contains(&format!(
                "; strictly fenced agents run nothing: what {} declares can't say how a \
                 strictly fenced command runs: ",
                file.display()
            )) && refused.ends_with(&format!(
                "belongs to account {own}, which isn't trusted to declare it"
            )),
            "and why one runs nothing: {refused}"
        );
    }

    #[test]
    fn a_fence_runs_a_command_only_as_a_sealed_file_says() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        let env = stored("env");
        let env = env.to_str().expect("a path in words");
        runs_through(&file, &[env, "FENCED=1", "--"]);
        assert_eq!(
            launcher(&file, Kind::Strict, &declarers()),
            Ok(vec![env.to_owned(), "FENCED=1".to_owned(), "--".to_owned()]),
            "a file only trusted accounts write says it"
        );
        assert_eq!(
            launcher(&file, Kind::Fence, &declarers()),
            Err(
                "this machine declares no way to run a command as its fenced sessions run"
                    .to_owned()
            ),
            "a fence it doesn't declare runs nothing"
        );
        let own = rustix::process::getuid().as_raw();
        let refused = launcher(&file, Kind::Strict, &[0, 65534]);
        assert!(
            refused.as_ref().is_err_and(|why| why.ends_with(&format!(
                "belongs to account {own}, which isn't trusted to declare it"
            ))),
            "a file the agents' own account writes says nothing: {refused:?}"
        );
        let shared = folder.path().join("shared");
        fs::create_dir(&shared).expect("a folder is made");
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o775)).expect("it is shared");
        let file = shared.join("declared.json");
        runs_through(&file, &[env]);
        let refused = launcher(&file, Kind::Strict, &declarers());
        assert!(
            refused
                .as_ref()
                .is_err_and(|why| why.ends_with("may be written by others than its owner")),
            "a file in a folder others write says nothing: {refused:?}"
        );
    }

    #[test]
    fn a_link_another_account_may_repoint_seals_nothing_even_into_the_store() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let in_store = fs::canonicalize(stored("env")).expect("the store holds it");
        let link = folder.path().join("declared.json");
        std::os::unix::fs::symlink(&in_store, &link).expect("the link is made");
        let own = rustix::process::getuid().as_raw();
        let refused = sealed(&link, &[0, 65534]);
        assert!(
            refused.as_ref().is_err_and(|why| why.ends_with(&format!(
                "belongs to account {own}, which isn't trusted to declare it"
            ))),
            "a store file reached through a folder the agents' account owns says nothing: \
             {refused:?}"
        );
        assert_eq!(
            sealed(&link, &declarers()),
            Ok(()),
            "the same link where only trusted accounts write says it"
        );
        assert_eq!(
            sealed(&in_store, &[0, 65534]),
            Ok(()),
            "a store file named as itself says it"
        );
    }

    #[test]
    fn every_hop_of_a_link_is_sealed_not_only_its_end() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let shared = folder.path().join("shared");
        fs::create_dir(&shared).expect("a folder is made");
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o775)).expect("it is shared");
        let in_store = fs::canonicalize(stored("env")).expect("the store holds it");
        let middle = shared.join("middle");
        std::os::unix::fs::symlink(&in_store, &middle).expect("the middle link is made");
        let link = folder.path().join("declared.json");
        std::os::unix::fs::symlink("shared/middle", &link).expect("the first link is made");
        let refused = sealed(&link, &declarers());
        assert!(
            refused
                .as_ref()
                .is_err_and(|why| why.ends_with("may be written by others than its owner")),
            "a link through a folder others write says nothing: {refused:?}"
        );
        let looping = folder.path().join("looping");
        std::os::unix::fs::symlink("looping", &looping).expect("the loop is made");
        assert!(
            sealed(&looping, &declarers()).is_err(),
            "a link to itself says nothing"
        );
    }

    #[test]
    fn a_node_says_what_its_kit_holds_and_whether_an_agent_could_change_it() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let kit = folder.path().join("kit");
        let unlisted = kit_said_at_start(&kit, &declarers());
        assert!(
            unlisted.ends_with("), so names no kit") && unlisted.contains("can't list its bin ("),
            "a kit with no bin to list is said to name none, and why: {unlisted}"
        );
        let bin = kit.join("bin");
        fs::create_dir_all(&bin).expect("its bin");
        for tool in ["gh", "gcloud", "say\nit"] {
            fs::write(bin.join(tool), "").expect("a tool");
            fs::set_permissions(bin.join(tool), fs::Permissions::from_mode(0o755))
                .expect("a tool only its owner writes");
        }
        let resolved = fs::canonicalize(&kit).expect("the kit resolves");
        let tools = "3 tools: gcloud gh say\\nit";
        assert_eq!(
            kit_said_at_start(&kit, &declarers()),
            format!(
                "puts the realm's kit at {} first on each path; only its declarers can change \
                 its tools; {tools}",
                resolved.display()
            ),
            "a kit only its declarers write is said as it is, a tool's odd name escaped"
        );
        fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o666))
            .expect("a tool anyone writes");
        assert_eq!(
            kit_said_at_start(&kit, &declarers()),
            format!(
                "puts the realm's kit at {} first on each path; an agent could change what its \
                 tools are: {} may be written by others than its owner; {tools}",
                resolved.display(),
                // Named as the walk reaches it, through any link above.
                resolved.join("bin").join("gh").display()
            ),
            "a tool anyone writes in a bin only its declarers write is said to be changeable"
        );
    }

    #[test]
    fn a_fence_never_spares_a_shared_tree_on_its_declarations_word() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        let env = stored("env");
        let env = env.to_str().expect("a path in words");
        for spare in ["--shared-tree", "--shared-tree=yes", "--shared", "--share"] {
            runs_through(&file, &[env, spare, "--"]);
            assert_eq!(
                launcher(&file, Kind::Strict, &declarers()),
                Err(format!(
                    "its strictly fenced sessions' launcher in {} says {spare}, which spares \
                     a tree shared to the asker; this node can't yet say which trees are \
                     shared, so it never asks that: take it out of the declaration",
                    file.display()
                )),
                "{spare} is refused"
            );
        }
        for kept in ["--kind", "--folder", "--s"] {
            runs_through(&file, &[env, kept, "--"]);
            assert!(
                launcher(&file, Kind::Strict, &declarers()).is_ok(),
                "{kept} spares nothing"
            );
        }
    }

    #[test]
    fn a_fence_ends_its_options_before_an_ask_begins() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        let env = stored("env");
        let env = env.to_str().expect("a path in words");
        for runs in [&[env, "FENCED=1"][..], &[env, "--", "FENCED=1"]] {
            runs_through(&file, runs);
            assert_eq!(
                launcher(&file, Kind::Strict, &declarers()),
                Err(format!(
                    "its strictly fenced sessions' launcher in {} doesn't end with --, so \
                     what an ask says after it could be read as the launcher's own options",
                    file.display()
                )),
                "{runs:?} is refused"
            );
        }
    }

    #[test]
    fn a_fence_runs_a_command_only_through_a_program_the_store_holds() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        let program = folder.path().join("fence");
        fs::write(&program, "#!/bin/sh\nexec \"$@\"\n").expect("the program is written");
        let program = program.to_str().expect("a path in words");
        runs_through(&file, &[program]);
        assert_eq!(
            launcher(&file, Kind::Strict, &declarers()),
            Err(format!(
                "its strictly fenced sessions' launcher {program} isn't in the Nix store, so \
                 it isn't trusted to fence"
            ))
        );
    }

    #[test]
    fn a_configuration_declares_what_cant_be_measured() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        fs::write(
            &file,
            r#"{"reach": ["home", "internet"],
                "outlives": ["window_close", "node_restart"],
                "costs_money": false,
                "taints": [{"key": "person-works", "effect": "prefer_not",
                            "why": "its person works here"}]}"#,
        )
        .expect("the configuration is written");
        assert_eq!(
            read(&file),
            Some(Declared {
                reach: vec!["home".to_owned(), "internet".to_owned()],
                probes: BTreeMap::new(),
                outlives: vec![Outlives::WindowClose, Outlives::NodeRestart],
                costs_money: false,
                taints: vec![Taint {
                    key: "person-works".to_owned(),
                    effect: Effect::PreferNot,
                    why: "its person works here".to_owned(),
                }],
                cores: None,
                agents: BTreeMap::new(),
                leaves_at: None,
                within: None,
                hosts: Vec::new(),
                fence: None,
            }),
            "it reads as declared"
        );
    }

    #[test]
    fn a_machine_that_costs_money_leaves_or_reaches_aquifer_is_a_worker() {
        let lasting = Declared {
            reach: vec!["home".to_owned(), "internet".to_owned()],
            probes: BTreeMap::new(),
            outlives: Vec::new(),
            costs_money: false,
            taints: Vec::new(),
            cores: None,
            agents: BTreeMap::new(),
            leaves_at: None,
            within: None,
            hosts: Vec::new(),
            fence: None,
        };
        assert!(
            !lasting.is_worker(),
            "a machine of the person's own is none"
        );
        for (worker, why) in [
            (
                Declared {
                    costs_money: true,
                    ..lasting.clone()
                },
                "it costs money",
            ),
            (
                Declared {
                    leaves_at: Some(jiff::Timestamp::UNIX_EPOCH),
                    ..lasting.clone()
                },
                "it leaves",
            ),
            (
                Declared {
                    reach: vec!["internet".to_owned(), "aquifer".to_owned()],
                    ..lasting.clone()
                },
                "it reaches aquifer",
            ),
        ] {
            assert!(worker.is_worker(), "a worker, as {why}");
        }
    }

    #[test]
    fn a_fence_reads_by_kind_and_one_misshapen_is_unknown() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        fs::write(
            &file,
            r#"{"reach": [], "outlives": [], "costs_money": false, "taints": [],
                "fence": {"strict": {"builders": ["remote"], "forges": [],
                                      "secrets": []}}}"#,
        )
        .expect("the configuration is written");
        let strict = Doors {
            builders: BTreeSet::from(["remote".to_owned()]),
            forges: BTreeSet::new(),
            secrets: BTreeSet::new(),
            runs: Vec::new(),
        };
        assert_eq!(
            read(&file).and_then(|declared| declared.fence),
            Some(BTreeMap::from([(Kind::Strict, strict)])),
            "each kind it runs reads with its doors"
        );
        fs::write(
            &file,
            r#"{"reach": ["internet"], "outlives": [], "costs_money": false,
                "taints": [], "fence": {"strict": {"builders": ["remote"]}}}"#,
        )
        .expect("the configuration is written");
        let declared = read(&file).expect("the rest still reads");
        assert_eq!(
            (declared.fence, declared.reach),
            (None, vec!["internet".to_owned()]),
            "a fence missing a door is unknown, and spoils nothing else"
        );
    }

    #[test]
    fn a_realm_hears_only_what_its_own_agent_sessions_start_with() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        fs::write(
            &file,
            r#"{"reach": [], "outlives": [], "costs_money": false, "taints": [],
                "agents": {"work": "pi", "personal": "pi-claude"}}"#,
        )
        .expect("the configuration is written");
        let declared = read(&file).expect("it reads");
        assert_eq!(
            declared.clone().in_realm("personal").agents,
            BTreeMap::from([("personal".to_owned(), "pi-claude".to_owned())]),
            "the personal realm hears its own command and nothing of work"
        );
        assert!(
            declared.in_realm("elsewhere").agents.is_empty(),
            "a realm the machine starts no agent in hears none"
        );
    }

    #[test]
    fn nothing_is_known_where_nothing_is_declared_as_this_build_reads_it() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        assert_eq!(read(&file), None, "no configuration declares nothing");
        fs::write(&file, r#"{"reach": ["home"]}"#).expect("written");
        assert_eq!(read(&file), None, "a part left out is never guessed");
        fs::write(
            &file,
            r#"{"reach": [], "outlives": ["moving"], "costs_money": false, "taints": []}"#,
        )
        .expect("written");
        assert_eq!(read(&file), None, "nor is a word this build doesn't know");
    }

    #[test]
    fn a_probe_connects_or_runs_a_command() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        fs::write(
            &file,
            r#"{"reach": ["internet"],
                "probes": {"home": {"connect": "192.0.2.1:53"},
                           "office": {"run": ["test", "-e", "/run/office"]}},
                "outlives": [], "costs_money": false, "taints": []}"#,
        )
        .expect("the configuration is written");
        let probes = read(&file).map(|declared| declared.probes);
        assert_eq!(
            probes,
            Some(BTreeMap::from([
                ("home".to_owned(), Probe::Connect("192.0.2.1:53".to_owned())),
                (
                    "office".to_owned(),
                    Probe::Run(vec![
                        "test".to_owned(),
                        "-e".to_owned(),
                        "/run/office".to_owned()
                    ])
                ),
            ])),
            "each probe reads as the kind it names"
        );
    }

    #[test]
    fn a_machine_given_a_share_of_its_host_says_how_many_processors() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        fs::write(
            &file,
            r#"{"reach": [], "outlives": [], "costs_money": true, "taints": [],
                "cores": 4}"#,
        )
        .expect("the configuration is written");
        assert_eq!(
            read(&file).and_then(|declared| declared.cores),
            Some(4),
            "the processors it is given read as declared"
        );
    }

    #[test]
    fn a_machine_lent_for_a_while_says_when_it_leaves() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        fs::write(
            &file,
            r#"{"reach": [], "outlives": [], "costs_money": true, "taints": [],
                "leaves_at": "2026-10-30T02:18:25Z"}"#,
        )
        .expect("the configuration is written");
        let declared = read(&file).expect("it reads");
        let leaves: jiff::Timestamp = "2026-10-30T02:18:25Z".parse().expect("a time");
        assert_eq!(declared.leaves_at, Some(leaves), "it reads as declared");
        assert!(
            !declared.has_left(leaves - jiff::SignedDuration::from_secs(1)),
            "it is still here a moment before"
        );
        assert!(
            declared.has_left(leaves),
            "and has left once its time comes"
        );

        fs::write(
            &file,
            r#"{"reach": [], "outlives": [], "costs_money": true, "taints": [],
                "leaves_at": "soon"}"#,
        )
        .expect("written");
        assert_eq!(
            read(&file),
            None,
            "a time this build can't read is never guessed"
        );
    }
}
