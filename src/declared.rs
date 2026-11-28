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
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::hold::start::Kit;
use crate::placement::Need;

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
    /// How many sessions it holds at most, where its room would run short
    /// before its pressure showed it, as a small virtual machine with one
    /// worker home: a machine holding as many takes no more starts. Any
    /// number where it says none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions_at_most: Option<u32>,
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
    /// Whether pi there lays its own fence round each command it runs,
    /// as a Mac's pi can once it lays a sandbox profile round each, so a
    /// held start leaves pi to fence itself rather than starting it
    /// within the launcher, where it can't reach its own state.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pi_fences_itself: bool,
    /// Whether, as a worker, it takes carried sessions: a machine made to
    /// take load off the person's own, whose configuration lets a carry
    /// bring a session there with its quest. A worker saying nothing takes
    /// none, and one of the person's own machines takes them either way.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub takes_carries: bool,
    /// The basin it is one of, as each cell a basin makes says it, so a
    /// map can draw the basin once rather than every cell in it; none
    /// where it is no basin's, or says it in any shape but a name. It is
    /// the cell's own word, so it is only ever drawn, never weighed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "basin_or_none"
    )]
    pub basin: Option<String>,
    /// How lightly it is kept loaded, where it is a machine the person
    /// works at that the realm's other machines take new work from first:
    /// it ranks after every other machine heard lately with room, and past
    /// its ceiling after every machine heard lately. None where it is
    /// ranked as any machine is, and where it isn't in the shape this
    /// build reads.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "lively_or_none"
    )]
    pub kept_lively: Option<Lively>,
    /// What work begun here needs wherever placement sends it, by where
    /// it begins or what it names, as work in a checkout only some
    /// machines can work in needs a network only they reach. A rule not
    /// in the shape this build reads is left out, never the others or the
    /// rest of what's declared, so a link's end reading another machine's
    /// facts keeps the link.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "rules_read"
    )]
    pub needs_in: Vec<NeedsIn>,
}

/// The ceiling a machine kept lively ([`Declared::kept_lively`]) holds its
/// load to: past any part, it counts as past its room, so work goes to
/// any other machine heard lately before it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lively {
    /// The processor pressure it stays at or under, in percent of the
    /// last minute in which some work waited on a processor, machine-wide
    /// (its room's `cpu_minute`), or of the last ten seconds where its
    /// room says no minute.
    pub cpu: u32,
    /// The memory it keeps available at least, in bytes; none where it
    /// keeps no more than any machine does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_available: Option<u64>,
    /// The memory pressure it stays at or under, in percent of the last
    /// minute in which all work at once waited on memory, machine-wide
    /// (its room's `memory_full_minute`), as a whole percent; none where
    /// it holds to no such ceiling, and where it isn't a whole percent,
    /// which holds it to the rest of its ceiling rather than to none. A
    /// room that says no such minute is never past it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "memory_full_or_none"
    )]
    pub memory_full: Option<u32>,
    /// The disk pressure it stays at or under, in percent of the last
    /// minute in which all work at once waited on its disks, machine-wide
    /// or in the groups of `io_full_from` (its room's `io_full_minute`),
    /// as a whole percent; none where it holds to no such ceiling, and
    /// where it isn't a whole percent, as [`Lively::memory_full`] is. A
    /// room that says no such minute is never past it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "io_full_or_none"
    )]
    pub io_full: Option<u32>,
    /// The control groups, each a path below the kernel's root of them, a
    /// leading slash read the same, as
    /// `user.slice/user-1000.slice/user@1000.service/agents.slice`, whose
    /// disk pressure its node reads in place of the machine's, the worst
    /// of them over the last minute; none where it reads the
    /// machine's. Machine-wide, a task held back by a cap of its own
    /// counts as waiting on the disks, so a machine whose capped survey of
    /// its disks stalls alone reads past a ceiling its work never feels.
    /// A list in another shape is none, spoiling only itself. Where none
    /// of the groups says a minute its node reads the machine's, and where
    /// only some do, the worst of those; either is said on the node's log,
    /// naming each group saying none.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "groups_or_none"
    )]
    pub io_full_from: Vec<String>,
}

/// What work begun here needs, wherever placement sends it, where it
/// begins in one of `folders` or its words name one of `words`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeedsIn {
    /// The folders, each within home, as `src/rows` names `~/src/rows`,
    /// or whole; work begun in one or anywhere below it needs `needs`.
    /// Each is judged through its links, as the sprout's own folder is.
    #[serde(default)]
    pub folders: Vec<PathBuf>,
    /// The words, as a link's host, found in any case in what the work
    /// says, each standing on its own rather than inside a longer word.
    #[serde(default)]
    pub words: Vec<String>,
    /// What such work needs.
    pub needs: Vec<Need>,
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
    /// The doors another machine, by its name, may open beyond these for
    /// a session in this fence carried from here, or run or started there
    /// by an agent here, each allowed by the person; none where every door
    /// there must be no wider than here.
    /// The machine there still declares each door it opens, so what is
    /// allowed is said, never hidden by a narrower declaration.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub accepts: BTreeMap<String, Accepted>,
}

/// The doors of each kind a session in this fence, carried from here or run
/// or started there by an agent here, may gain there.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accepted {
    /// The machines it may build on.
    #[serde(default)]
    pub builders: BTreeSet<String>,
    /// The forges it may push to and fetch from.
    #[serde(default)]
    pub forges: BTreeSet<String>,
    /// The secrets it may reach.
    #[serde(default)]
    pub secrets: BTreeSet<String>,
}

/// A fence as declared, or none where it isn't in the shape this build
/// reads, which spoils only the fence, never the rest of what's declared.
fn fence_or_none<'de, D: serde::Deserializer<'de>>(
    said: D,
) -> Result<Option<BTreeMap<Kind, Doors>>, D::Error> {
    Ok(Option::<serde_json::Value>::deserialize(said)?
        .and_then(|fence| serde_json::from_value(fence).ok()))
}

/// A ceiling as declared, or none where it isn't in the shape this build
/// reads, which spoils only the ceiling, never the rest of what's
/// declared.
fn lively_or_none<'de, D: serde::Deserializer<'de>>(said: D) -> Result<Option<Lively>, D::Error> {
    Ok(Option::<serde_json::Value>::deserialize(said)?
        .and_then(|lively| serde_json::from_value(lively).ok()))
}

/// The groups a ceiling's disk pressure is read from as declared, or none
/// where they aren't a list of names, so the ceiling's other parts stand
/// and its disk pressure is read machine-wide, said once on the node's log
/// so the list is seen to be left out.
fn groups_or_none<'de, D: serde::Deserializer<'de>>(said: D) -> Result<Vec<String>, D::Error> {
    Ok(Option::<serde_json::Value>::deserialize(said)?
        .and_then(|groups| {
            serde_json::from_value(groups)
                .inspect_err(|error| {
                    say_once(format!(
                        "reading a declared ceiling, its io_full_from isn't a list of \
                         groups, so its disks are read machine-wide: {error}"
                    ));
                })
                .ok()
        })
        .unwrap_or_default())
}

/// A ceiling's `memory_full` as declared, as [`whole_or_none`] reads it.
fn memory_full_or_none<'de, D: serde::Deserializer<'de>>(said: D) -> Result<Option<u32>, D::Error> {
    whole_or_none(said, "memory_full")
}

/// A ceiling's `io_full` as declared, as [`whole_or_none`] reads it.
fn io_full_or_none<'de, D: serde::Deserializer<'de>>(said: D) -> Result<Option<u32>, D::Error> {
    whole_or_none(said, "io_full")
}

/// A part of a ceiling as declared, or none where it isn't a whole
/// percent, which spoils only that part: a fraction such as 0.5 is easy to
/// write for a share the kernel says in hundredths, and dropping the whole
/// ceiling for it would rank the machine as any machine, ahead of others.
/// Each left out is said once on the node's log by its `name`, so the
/// part is seen not to hold.
fn whole_or_none<'de, D: serde::Deserializer<'de>>(
    said: D,
    name: &str,
) -> Result<Option<u32>, D::Error> {
    let part = Option::<serde_json::Value>::deserialize(said)?;
    Ok(part.and_then(|part| {
        serde_json::from_value(part)
            .inspect_err(|error| {
                say_once(not_whole(name, error));
            })
            .ok()
    }))
}

/// What a node says of a declared ceiling's part `name` that isn't a whole
/// percent, as `error` says why. It reads the files its peers declared as
/// well as its own, so it says it read a declared one.
fn not_whole(name: &str, error: &serde_json::Error) -> String {
    format!(
        "reading a declared ceiling, its {name} isn't a whole percent, \
         so that part is left out: {error}"
    )
}

/// Says `line` on the node's log unless it has said it already, and
/// whether it did. A declared file is read at every measure, every few
/// seconds, so a part that doesn't hold is said once for each way it
/// doesn't rather than at every read. What is said is kept while the node
/// runs: a few lines, one for each shape its declared files took.
pub fn say_once(line: String) -> bool {
    static SAID: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    let mut said = SAID.lock().unwrap_or_else(PoisonError::into_inner);
    if said.contains(&line) {
        return false;
    }
    eprintln!("{line}");
    said.insert(line)
}

/// The rules of what work needs as declared, each one not in the shape
/// this build reads left out, which spoils only that rule, never the
/// others or the rest of what's declared. Each left out is said on the
/// node's log with why, so a misspelt rule, whose work would be placed
/// where it can't be done, is seen rather than dropped without a word.
fn rules_read<'de, D: serde::Deserializer<'de>>(said: D) -> Result<Vec<NeedsIn>, D::Error> {
    let rules = match serde_json::Value::deserialize(said)? {
        serde_json::Value::Array(rules) => rules,
        serde_json::Value::Null => Vec::new(),
        other => {
            eprintln!("what's declared of needs_in isn't a list, so it's left out: {other}");
            Vec::new()
        }
    };
    Ok(rules
        .into_iter()
        .enumerate()
        .filter_map(|(at, rule)| {
            serde_json::from_value(rule)
                .inspect_err(|error| {
                    eprintln!("needs_in rule {at} isn't read, so it's left out: {error}");
                })
                .ok()
        })
        .collect())
}

/// The longest name a basin is read by, a DNS label's.
const BASIN_LONGEST: usize = 63;

/// A basin as declared, or none where it isn't a name as a DNS label is
/// one, lower-case letters, digits and hyphens, at most 63 of them,
/// starting and ending with no hyphen, which spoils only the basin, never
/// the rest of what's declared.
fn basin_or_none<'de, D: serde::Deserializer<'de>>(said: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<serde_json::Value>::deserialize(said)?
        .and_then(|basin| basin.as_str().map(str::to_owned))
        .filter(|name| {
            (1..=BASIN_LONGEST).contains(&name.len())
                && !name.starts_with('-')
                && !name.ends_with('-')
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        }))
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
/// launcher's own options, so long as the launcher reads `--` so. They
/// never say `--shared-tree`, nor a shortening of it longer than `--s`,
/// as a word of its own, which would spare every run a work tree pi-fence otherwise
/// refuses: the node asks that only of a run it found in a tree shared
/// with its asker ([`sparing`]). A word is all it sees: a wrapper or a
/// script word saying it out of sight is the sealed declaration's to
/// keep out.
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
    let declares_none =
        || format!("this machine declares no way to run a command as its {kind} sessions run");
    let Some(program) = runs.first() else {
        return Err(declares_none());
    };
    let found = fs::canonicalize(program)
        .map_err(|error| format!("its {kind} sessions' launcher {program} isn't there: {error}"))?;
    if !found.starts_with(STORE) {
        return Err(format!(
            "its {kind} sessions' launcher {program} isn't in the Nix store, so it isn't \
             trusted to fence"
        ));
    }
    match unrunnable(&runs) {
        Some(Unrunnable::SparesATree(spare)) => Err(format!(
            "its {kind} sessions' launcher in {} says {spare}, which spares a tree shared \
             to the asker; only the node asks that, of a run it found in such a tree: take \
             it out of the declaration",
            file.display()
        )),
        Some(Unrunnable::Unended) => Err(format!(
            "its {kind} sessions' launcher in {} doesn't end with --, so what an ask says \
             after it could be read as the launcher's own options",
            file.display()
        )),
        // Words that run nothing never pass, though the guard above
        // already refuses them, since an empty launcher runs a command
        // unfenced.
        Some(Unrunnable::Nothing) => Err(declares_none()),
        None => Ok(runs),
    }
}

/// Why a fence's declared `runs` runs no agent's command, as its words
/// alone tell, so another machine can judge it from what that one said of
/// itself. Whether its program is in the Nix store only the machine itself
/// can tell ([`launcher`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unrunnable {
    /// It names no words at all.
    Nothing,
    /// A word of it, this one, asks the launcher to spare a shared tree.
    SparesATree(String),
    /// It doesn't end with `--`.
    Unended,
}

/// Why `runs`, as a fence declares it, runs no agent's command, or none
/// where its words would run one.
#[must_use]
pub fn unrunnable(runs: &[String]) -> Option<Unrunnable> {
    if runs.is_empty() {
        return Some(Unrunnable::Nothing);
    }
    if let Some(spare) = runs.iter().find(|word| spares_a_tree(word)) {
        return Some(Unrunnable::SparesATree(spare.clone()));
    }
    (runs.last().map(String::as_str) != Some("--")).then_some(Unrunnable::Unended)
}

/// Whether `word` asks a launcher to spare a shared tree: `--shared-tree`,
/// with a value or without, or a shortening of it past `--s` that a
/// parser taking unique prefixes would read as it.
fn spares_a_tree(word: &str) -> bool {
    let name = word.split('=').next().unwrap_or(word);
    word.starts_with(SHARED_TREE) || (name.len() > "--s".len() && SHARED_TREE.starts_with(name))
}

/// `launcher`, words [`launcher`] gave, asking the fence to spare the work
/// tree its run stands in: `--shared-tree` before the `--` that ends them.
/// The node asks it only of a run it found, itself, in a tree of a quest
/// shared with the machine that asks. The `--` that ends them is the
/// fence's where the fence's program comes last, as every launcher core
/// declares; where another follows it, that one is told the option, and
/// either refuses it, running nothing, or spares nothing. Words with no
/// `--` are left as they are, sparing nothing, though [`launcher`] gives
/// none such.
#[must_use]
pub fn sparing(mut launcher: Vec<String>) -> Vec<String> {
    if let Some(end) = launcher.iter().rposition(|word| word == "--") {
        launcher.insert(end, SHARED_TREE.to_owned());
    }
    launcher
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

/// The realm's kit given in `kit`, as a node may carry it: carried, at the
/// folder it resolves to now, only where no account but `declarers` can
/// change the way to it, its `bin` or anything in that, and refused
/// otherwise, or where its `bin` can't be read. A kit goes first on the
/// path of every session and fenced run the node starts, other machines'
/// asks among them, so a tool an agent could replace would run as
/// everything that asks for it; refusing leaves each the path its login
/// profile gives. The folder carried is the one checked, so a link
/// repointed since changes nothing until the node starts again.
#[must_use]
pub fn kit_checked(kit: &Path, declarers: &[u32]) -> Kit {
    let refused = |why: String| Kit::Refused {
        given: kit.to_owned(),
        why,
    };
    let folder = match fs::canonicalize(kit) {
        Ok(folder) => folder,
        Err(error) => return refused(format!("it can't be resolved ({error})")),
    };
    // The way as given, link by link, since a link an agent may repoint
    // chooses the kit, even where it leads into the store.
    if let Err(why) = sealed(kit, declarers) {
        return refused(format!("an agent could change which kit it is: {why}"));
    }
    // A path splits at a colon, into folders nothing here checked.
    if folder.as_os_str().as_encoded_bytes().contains(&b':') {
        return refused(format!(
            "it resolves to {}, which holds a colon",
            folder.display()
        ));
    }
    let bin = folder.join("bin");
    let entries = match fs::read_dir(&bin).and_then(Iterator::collect::<io::Result<Vec<_>>>) {
        Ok(entries) => entries,
        Err(error) => return refused(format!("its bin can't be listed ({error})")),
    };
    // The folder and every entry in it, those the record leaves out among
    // them, since a listed tool may run a hidden one beside it, and a tool
    // that is a link is only as sealed as where it leads.
    match sealed(&bin, declarers).and_then(|()| {
        entries
            .iter()
            .try_for_each(|entry| sealed(&entry.path(), declarers))
    }) {
        Ok(()) => Kit::Carried(folder),
        Err(why) => refused(format!("an agent could change what its tools are: {why}")),
    }
}

/// What a node says as it starts of the realm's `kit`: what it holds,
/// which every session and run it starts finds first on its path, or that
/// it is left off the path of all it starts, why, and how it mends. Each
/// tool and why are said escaped, so no name can pass for the verdict.
#[must_use]
pub fn kit_said_at_start(kit: &Kit) -> String {
    match kit {
        Kit::Refused { given, why } => format!(
            "leaves the realm's kit at {} off the path of everything it starts, since {}; \
             it is checked as the node starts, so start it again once that is mended",
            given.display().to_string().escape_debug(),
            controls_escaped(why)
        ),
        Kit::Carried(folder) => {
            let tools: Vec<String> = crate::facts::kit(folder)
                .map(|listed| listed.tools)
                .unwrap_or_default()
                .iter()
                .map(|tool| tool.escape_debug().to_string())
                .collect();
            format!(
                "puts the realm's kit at {} first on each path, only its declarers able to \
                 change its tools; {} tools: {}",
                folder.display().to_string().escape_debug(),
                tools.len(),
                tools.join(" ")
            )
        }
    }
}

/// `text` with each control character escaped, so a name within it can't
/// start a line of its own, and the rest, such as its apostrophes, as is.
pub(crate) fn controls_escaped(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() {
                c.escape_debug().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
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

    /// A kit in `folder` of three tools only their owner writes, one of
    /// them oddly named, and the folder it resolves to.
    fn kit_of_three(folder: &Path) -> (PathBuf, PathBuf) {
        let kit = folder.join("kit");
        let bin = kit.join("bin");
        fs::create_dir_all(&bin).expect("its bin");
        for tool in ["gh", "gcloud", "say\nit"] {
            fs::write(bin.join(tool), "").expect("a tool");
            fs::set_permissions(bin.join(tool), fs::Permissions::from_mode(0o755))
                .expect("a tool only its owner writes");
        }
        let resolved = fs::canonicalize(&kit).expect("the kit resolves");
        (kit, resolved)
    }

    #[test]
    fn a_node_refuses_a_kit_it_cant_find_or_an_agent_could_point_elsewhere() {
        let temporary = tempfile::tempdir().expect("a temporary folder");
        let folder = temporary.path();
        let kit = folder.join("kit");
        let missing = kit_checked(&kit, &declarers());
        assert!(
            matches!(&missing, Kit::Refused { given, why }
                if *given == kit && why.starts_with("it can't be resolved (")),
            "a kit that isn't there is refused, and why: {missing:?}"
        );
        fs::create_dir(&kit).expect("a kit with no bin");
        let unlisted = kit_checked(&kit, &declarers());
        assert!(
            matches!(&unlisted, Kit::Refused { given, why }
                if *given == kit && why.starts_with("its bin can't be listed (")),
            "a kit with no bin to list is refused, and why: {unlisted:?}"
        );
        assert_eq!(
            kit_said_at_start(&unlisted),
            format!(
                "leaves the realm's kit at {} off the path of everything it starts, since its \
                 bin can't be listed ({}); it is checked as the node starts, so start it again \
                 once that is mended",
                kit.display(),
                fs::read_dir(kit.join("bin")).expect_err("no bin")
            ),
            "and says so as it starts, and how it mends"
        );
        fs::remove_dir(&kit).expect("the bare kit is taken away");
        let (kit, resolved) = kit_of_three(folder);
        // Given through a link, as a profile is.
        let profile = folder.join("profile");
        std::os::unix::fs::symlink(&kit, &profile).expect("a link to the kit");
        let carried = kit_checked(&profile, &declarers());
        assert_eq!(
            carried,
            Kit::Carried(resolved.clone()),
            "a kit only its declarers write is carried at the folder it was checked as"
        );
        // A link in a folder anyone writes may be repointed by an agent, at
        // a kit of its own choosing.
        let open = folder.join("open");
        fs::create_dir(&open).expect("a folder anyone writes");
        fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).expect("open to all");
        let repointable = open.join("profile");
        std::os::unix::fs::symlink(&kit, &repointable).expect("a link there to the kit");
        assert_eq!(
            kit_checked(&repointable, &declarers()),
            Kit::Refused {
                given: repointable.clone(),
                why: format!(
                    "an agent could change which kit it is: {} may be written by others than \
                     its owner",
                    // The check names the folder as it resolves, which
                    // differs where the temporary folder is reached
                    // through a link.
                    fs::canonicalize(&open)
                        .expect("the open folder resolves")
                        .display()
                ),
            },
            "a kit reached through a link an agent could repoint is refused"
        );
        // A colon where the kit resolves would split the path into folders
        // nothing checked.
        let coloned = folder.join("a:b");
        fs::create_dir_all(coloned.join("bin")).expect("a kit whose folder holds a colon");
        let linked = folder.join("coloned");
        std::os::unix::fs::symlink(&coloned, &linked).expect("a link to it");
        assert!(
            matches!(kit_checked(&linked, &declarers()), Kit::Refused { why, .. }
                if why.ends_with("which holds a colon")),
            "a kit resolving to a folder with a colon is refused"
        );
    }

    #[test]
    fn a_node_carries_a_kit_only_while_its_declarers_alone_write_each_tool() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let (kit, resolved) = kit_of_three(folder.path());
        let bin = kit.join("bin");
        let carried = kit_checked(&kit, &declarers());
        assert_eq!(
            carried,
            Kit::Carried(resolved.clone()),
            "a kit whose tools only its declarers write is carried"
        );
        assert_eq!(
            kit_said_at_start(&carried),
            format!(
                "puts the realm's kit at {} first on each path, only its declarers able to \
                 change its tools; 3 tools: gcloud gh say\\nit",
                resolved.display()
            ),
            "it is said as it is, a tool's odd name escaped"
        );
        // A hidden entry, which the record leaves out, still sits on the
        // path, and a listed tool may run it.
        fs::write(bin.join(".gh-wrapped"), "").expect("a hidden tool");
        fs::set_permissions(bin.join(".gh-wrapped"), fs::Permissions::from_mode(0o666))
            .expect("one anyone writes");
        assert!(
            matches!(kit_checked(&kit, &declarers()), Kit::Refused { why, .. }
                if why.contains(".gh-wrapped may be written by others")),
            "a hidden tool anyone writes refuses the kit too"
        );
        fs::remove_file(bin.join(".gh-wrapped")).expect("it is taken away");
        fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o666))
            .expect("a tool anyone writes");
        let why = format!(
            "an agent could change what its tools are: {} may be written by others than its \
             owner",
            // Named as the walk reaches it, from the folder it resolves to.
            resolved.join("bin").join("gh").display()
        );
        assert_eq!(
            kit_checked(&kit, &declarers()),
            Kit::Refused {
                given: kit.clone(),
                why: why.clone(),
            },
            "a tool anyone writes, in a bin only its declarers write, refuses the kit"
        );
        assert_eq!(
            kit_said_at_start(&kit_checked(&kit, &declarers())),
            format!(
                "leaves the realm's kit at {} off the path of everything it starts, since \
                 {why}; it is checked as the node starts, so start it again once that is \
                 mended",
                kit.display()
            ),
            "and the node says why as it starts"
        );
        // Why names what it found, so an odd name in it is escaped too.
        fs::set_permissions(bin.join("gh"), fs::Permissions::from_mode(0o755)).expect("gh mended");
        fs::set_permissions(bin.join("say\nit"), fs::Permissions::from_mode(0o666))
            .expect("an oddly named tool anyone writes");
        let said = kit_said_at_start(&kit_checked(&kit, &declarers()));
        assert!(
            !said.contains('\n') && said.contains("say\\nit may be written"),
            "no name in why can start a line of its own: {said}"
        );
    }

    #[test]
    fn a_fence_never_spares_a_shared_tree_on_its_declarations_word() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        let env = stored("env");
        let env = env.to_str().expect("a path in words");
        for spare in [
            "--shared-tree",
            "--shared-tree=yes",
            "--shared",
            "--share=yes",
            "--sh",
        ] {
            runs_through(&file, &[env, spare, "--"]);
            assert_eq!(
                launcher(&file, Kind::Strict, &declarers()),
                Err(format!(
                    "its strictly fenced sessions' launcher in {} says {spare}, which spares \
                     a tree shared to the asker; only the node asks that, of a run it found \
                     in such a tree: take it out of the declaration",
                    file.display()
                )),
                "{spare} is refused"
            );
        }
        for kept in ["--kind", "--folder", "--s", "--shard"] {
            runs_through(&file, &[env, kept, "--"]);
            assert!(
                launcher(&file, Kind::Strict, &declarers()).is_ok(),
                "{kept} spares nothing"
            );
        }
    }

    #[test]
    fn a_spare_is_asked_among_the_launchers_own_options_or_not_at_all() {
        let words = |words: &[&str]| {
            words
                .iter()
                .map(|&word| word.to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            sparing(words(&["/nix/store/pi-fence", "--kind", "strict", "--"])),
            words(&[
                "/nix/store/pi-fence",
                "--kind",
                "strict",
                "--shared-tree",
                "--"
            ])
        );
        assert_eq!(
            sparing(words(&[
                "/nix/store/env",
                "--",
                "/nix/store/pi-fence",
                "--kind",
                "strict",
                "--"
            ])),
            words(&[
                "/nix/store/env",
                "--",
                "/nix/store/pi-fence",
                "--kind",
                "strict",
                "--shared-tree",
                "--"
            ]),
            "the -- ending the launcher is pi-fence's, last as core declares it"
        );
        assert_eq!(
            sparing(words(&["/nix/store/pi-fence", "--kind", "strict"])),
            words(&["/nix/store/pi-fence", "--kind", "strict"]),
            "with no -- to end its options, nothing is the launcher's own"
        );
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
                pi_fences_itself: false,
                takes_carries: false,
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
                basin: None,
                sessions_at_most: None,
                kept_lively: None,
                needs_in: Vec::new(),
            }),
            "it reads as declared"
        );
    }

    #[test]
    fn a_machine_that_costs_money_leaves_or_reaches_aquifer_is_a_worker() {
        let lasting = Declared {
            pi_fences_itself: false,
            takes_carries: false,
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
            basin: None,
            sessions_at_most: None,
            kept_lively: None,
            needs_in: Vec::new(),
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
            accepts: BTreeMap::new(),
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

    #[test]
    fn a_machine_kept_lively_reads_its_ceiling_and_a_misshapen_one_spoils_only_it() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        let with = |lively: &str| {
            format!(
                r#"{{"reach": ["internet"], "outlives": [], "costs_money": false,
                    "taints": [], "kept_lively": {lively}}}"#
            )
        };
        // As core.nix writes it, with and without the memory it keeps and
        // the memory and disk pressure it holds to, and where it reads the
        // disks'.
        let (none, from): (&[&str], &[&str]) = (&[], &["agents.slice", "jobs.slice"]);
        for (said, memory_available, memory_full, io_full, io_full_from) in [
            (
                r#"{"cpu": 28, "memory_available": 25769803776}"#,
                Some(24 << 30),
                None,
                None,
                none,
            ),
            (r#"{"cpu": 28}"#, None, None, None, none),
            (
                r#"{"cpu": 28, "memory_available": 25769803776, "memory_full": 3}"#,
                Some(24 << 30),
                Some(3),
                None,
                none,
            ),
            (r#"{"cpu": 28, "io_full": 1}"#, None, None, Some(1), none),
            (
                r#"{"cpu": 28, "io_full": 1, "io_full_from": ["agents.slice", "jobs.slice"]}"#,
                None,
                None,
                Some(1),
                from,
            ),
        ] {
            fs::write(&file, with(said)).expect("written");
            let declared = read(&file).expect("it reads");
            assert_eq!(
                declared.kept_lively,
                Some(Lively {
                    cpu: 28,
                    memory_available,
                    memory_full,
                    io_full,
                    io_full_from: io_full_from.iter().map(ToString::to_string).collect(),
                }),
                "{said} reads as declared"
            );
            assert_eq!(
                serde_json::to_value(&declared).expect("it says itself")["kept_lively"],
                serde_json::from_str::<serde_json::Value>(said).expect("json"),
                "and is said again as it was"
            );
        }
        for misshapen in [
            "28",
            r#"{"cpu": 28.5}"#,
            r#"{"cpu": 0.28}"#,
            r#"{"cpu": "28"}"#,
            r#"{"cpu": -1}"#,
            r#"{"cpu": 28, "memory_available": "16G"}"#,
            r#"{"memory_available": 1024}"#,
        ] {
            fs::write(&file, with(misshapen)).expect("written");
            let spoiled = read(&file).expect("a misshapen ceiling spoils only itself");
            assert_eq!(
                spoiled.kept_lively, None,
                "{misshapen} keeps it lively to nothing"
            );
            assert_eq!(
                spoiled.reach,
                ["internet"],
                "{misshapen} leaves the rest of the declaration standing"
            );
        }
    }

    #[test]
    fn groups_to_read_disk_pressure_from_in_another_shape_spoil_only_themselves() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        for groups in [r#""agents.slice""#, "[1]", r#"{"agents": 1}"#, "null"] {
            fs::write(
                &file,
                format!(
                    r#"{{"reach": ["internet"], "outlives": [], "costs_money": false,
                        "taints": [], "kept_lively": {{"cpu": 28, "io_full": 1,
                        "io_full_from": {groups}}}}}"#
                ),
            )
            .expect("written");
            let lively = read(&file)
                .expect("it reads")
                .kept_lively
                .expect("still kept lively");
            assert_eq!(
                (lively.cpu, lively.io_full, lively.io_full_from.len()),
                (28, Some(1), 0),
                "groups of {groups} read its disks machine-wide, the rest standing"
            );
        }
    }

    #[test]
    fn a_pressure_ceiling_not_a_whole_percent_spoils_only_itself() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        for (name, kept) in [("memory_full", "io_full"), ("io_full", "memory_full")] {
            for part in ["0.5", "3.5", r#""3""#, "-1", "null"] {
                fs::write(
                    &file,
                    format!(
                        r#"{{"reach": ["internet"], "outlives": [], "costs_money": false,
                            "taints": [], "kept_lively": {{"cpu": 28,
                            "memory_available": 1024, "{kept}": 2, "{name}": {part}}}}}"#
                    ),
                )
                .expect("written");
                let lively = read(&file)
                    .expect("it reads")
                    .kept_lively
                    .expect("still kept lively");
                let (spoiled, other) = if name == "memory_full" {
                    (lively.memory_full, lively.io_full)
                } else {
                    (lively.io_full, lively.memory_full)
                };
                assert_eq!(
                    (lively.cpu, lively.memory_available, spoiled, other),
                    (28, Some(1024), None, Some(2)),
                    "a {name} of {part} holds it to the rest of its ceiling"
                );
                let written: serde_json::Value = serde_json::from_str(part).expect("json");
                if let Err(error) = serde_json::from_value::<Option<u32>>(written) {
                    assert!(
                        !say_once(not_whole(name, &error)),
                        "a {name} of {part} is said by its name, and said already"
                    );
                }
            }
        }
    }

    #[test]
    fn a_line_is_said_once_however_often_it_comes() {
        let line = "kestrel's ceiling, said for a test".to_owned();
        assert!(say_once(line.clone()), "a line is said the first time");
        assert!(!say_once(line), "and not again");
        assert!(
            say_once("osprey's ceiling, said for a test".to_owned()),
            "while another line is said in its turn"
        );
    }

    #[test]
    fn what_work_needs_reads_as_written_and_a_misshapen_rule_spoils_only_it() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        let with = |rules: &str| {
            format!(
                r#"{{"reach": ["internet"], "outlives": [], "costs_money": false,
                    "taints": [], "basin": "pond", "needs_in": {rules}}}"#
            )
        };
        let rule = r#"{"folders": ["zone"], "words": ["forge.example"],
            "needs": [{"reach": "inner"}, "cloud"]}"#;
        let read_as = vec![NeedsIn {
            folders: vec![PathBuf::from("zone")],
            words: vec!["forge.example".to_owned()],
            needs: vec![Need::Reach("inner".to_owned()), Need::Cloud],
        }];
        // As core.nix writes it, and said again as it was.
        fs::write(&file, with(&format!("[{rule}]"))).expect("written");
        let declared = read(&file).expect("it reads");
        assert_eq!(declared.needs_in, read_as, "it reads as declared");
        assert_eq!(
            serde_json::to_value(&declared).expect("it says itself")["needs_in"],
            serde_json::from_str::<serde_json::Value>(&format!("[{rule}]")).expect("json"),
            "and is said again as it was"
        );
        // Beside one this build can't read, which alone is left out.
        for misshapen in [
            r#"{"folders": ["zone"], "needs": [{"colour": "blue"}]}"#,
            r#"{"folders": ["zone"], "needs": "cloud"}"#,
            r#"{"folders": "zone", "needs": ["cloud"]}"#,
            r#"{"folders": ["zone"]}"#,
            r#""zone""#,
        ] {
            fs::write(&file, with(&format!("[{misshapen}, {rule}]"))).expect("written");
            let spoiled = read(&file).expect("a misshapen rule spoils only itself");
            assert_eq!(
                spoiled.needs_in, read_as,
                "{misshapen} is left out and the rule beside it kept"
            );
            assert_eq!(
                spoiled.basin.as_deref(),
                Some("pond"),
                "{misshapen} leaves the rest of the declaration standing"
            );
        }
        // Rules not in a list are none, the rest standing.
        for misshapen in [r#""zone""#, "7", "null", rule] {
            fs::write(&file, with(misshapen)).expect("written");
            let spoiled = read(&file).expect("misshapen rules spoil only themselves");
            assert!(spoiled.needs_in.is_empty(), "{misshapen} reads as no rule");
            assert_eq!(
                spoiled.reach,
                ["internet"],
                "{misshapen} leaves the rest standing"
            );
        }
    }

    #[test]
    fn a_cell_names_the_basin_it_is_one_of() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("declared.json");
        fs::write(
            &file,
            r#"{"reach": ["internet", "aquifer"], "outlives": [],
                "costs_money": true, "taints": [], "basin": "shoal"}"#,
        )
        .expect("the configuration is written");
        let declared = read(&file).expect("it reads");
        assert_eq!(
            declared.basin.as_deref(),
            Some("shoal"),
            "the basin reads as declared"
        );
        let longest = format!("b{}", "-1".repeat(31));
        for named in ["basin-1", "9", longest.as_str()] {
            fs::write(
                &file,
                format!(
                    r#"{{"reach": [], "outlives": [], "costs_money": false, "taints": [],
                        "basin": "{named}"}}"#
                ),
            )
            .expect("written");
            assert_eq!(
                read(&file).expect("it reads").basin.as_deref(),
                Some(named),
                "{named} is a basin's name"
            );
        }
        let said = serde_json::to_value(&declared).expect("it says itself");
        assert_eq!(
            said["basin"], "shoal",
            "and is said again when the declaration is written out"
        );
        for misshapen in [
            "7",
            r#"{"name": "shoal"}"#,
            r#""""#,
            r#""Shoal Basin""#,
            r#""Shoal""#,
            r#""shoal<script>""#,
            r#""-""#,
            r#""-shoal""#,
            r#""shoal-""#,
        ] {
            fs::write(
                &file,
                format!(
                    r#"{{"reach": ["internet"], "outlives": [], "costs_money": true,
                        "taints": [], "basin": {misshapen}}}"#
                ),
            )
            .expect("written");
            let spoiled = read(&file).expect("a misshapen basin spoils only itself");
            assert_eq!(spoiled.basin, None, "{misshapen} names no basin");
            assert!(
                spoiled.costs_money,
                "{misshapen} leaves the rest of the declaration standing"
            );
        }
        fs::write(
            &file,
            format!(
                r#"{{"reach": [], "outlives": [], "costs_money": false, "taints": [],
                    "basin": "{}"}}"#,
                "b".repeat(64)
            ),
        )
        .expect("written");
        assert_eq!(
            read(&file).expect("it reads").basin,
            None,
            "a basin's name past 63 characters names none"
        );
        fs::write(
            &file,
            r#"{"reach": [], "outlives": [], "costs_money": false, "taints": []}"#,
        )
        .expect("written");
        let lasting = read(&file).expect("it reads");
        assert_eq!(lasting.basin, None, "a machine of no basin names none");
        assert_eq!(lasting.kept_lively, None, "nor is it kept lively");
        assert!(
            serde_json::to_value(&lasting)
                .expect("it says itself")
                .get("basin")
                .is_none(),
            "and says nothing of one"
        );
    }
}
