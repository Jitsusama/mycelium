//! The mycelium as its person reads it: cells, and the networks they join.
//!
//! A cell is a session or a machine. A network is a space cells share for
//! one concern, which a cell joins by carrying its shape, so a session
//! joins the networks of the work it does: each mesh group it hears, but
//! its own address, is one. An orchestration's network is a team of the
//! swarm, its title, short name and nesting the team's; a project's and a
//! repository's are a quest's and a repository's groups, counted only
//! where two cells or more share them, since otherwise each is one cell's
//! own; a named group's is its group; and the machines are cells of a
//! network of their own. A receptor, sensing one outside source for its
//! person, is a cell of that source's outside network, beyond the
//! machines.
//!
//! Everything here is built from the node's `team`, `session`,
//! `machine` and `receptor` records, as the swarm reads them, and says nothing back: the
//! one model every view renders, the terminal's in `shown` and a page's or
//! an agent's from the JSON lines of [`Mycelium::lines`].

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::swarm::{self, Standing};

mod kept;
mod made;
mod shown;
mod subjects;

pub use made::{FORMED, MOST_SUBJECTS, read, subjects};
pub use shown::{cell, cells, network, overview};
pub use subjects::Subjects;

/// The machines' own network's name.
pub const MACHINES: &str = "machines";

/// How many of a cell's shares are read, so a session writing its
/// presence can't make the node read thousands.
pub const SHARES_MOST: usize = 16;

/// How long a share's key may be, in bytes.
pub const SHARE_KEY_MOST: usize = 96;

/// How much of what a share is for is read, in characters: a title's
/// length, as an event's title is cut to.
pub const SHARE_FOR_MOST: usize = 120;

/// How long a share's folder may be, in bytes.
pub const SHARE_FOLDER_MOST: usize = 1024;

/// The most characters of a cell's `asked`: what the harness sends, with
/// room for its own mark where it cut.
pub const ASKED_MOST: usize = 4096;

/// The name of each network that forms where two cells meet, which the
/// trees and shares `presence` holds would make: a tree's, such as
/// `tree.github:harbour/oars:jsama/tides`, and a shared folder's, such
/// as `files.quest:QEST-20261112-HARBOR`. A tree saying no repository,
/// or neither branch nor commit, names none, nor does a share past its
/// bounds.
#[must_use]
pub fn formed_networks(presence: &Value) -> BTreeSet<String> {
    let trees = presence["trees"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tree| Tree::deserialize(tree).ok())
        .filter_map(|tree| tree.network());
    let shares = shares_of(presence).into_iter().map(|share| share.network());
    trees.chain(shares).collect()
}

/// The folders `presence` says it shares: of its first [`SHARES_MOST`],
/// each within its bounds, the first saying each key. A key is read in
/// lower case, as a network is picked by name whatever its case, so two
/// keys differing only in case are one folder; and what a share is for
/// is cut to [`SHARE_FOR_MOST`] characters, as a long title still names
/// a folder.
fn shares_of(presence: &Value) -> Vec<Share> {
    let mut shares: Vec<Share> = Vec::new();
    let said = presence["shares"].as_array().into_iter().flatten();
    for share in said.take(SHARES_MOST) {
        let Ok(mut share) = Share::deserialize(share) else {
            continue;
        };
        share.key.make_ascii_lowercase();
        // Cleaned whole before the cut, so what a terminal would act on is
        // gone wherever it was said, and the cut counts what is drawn.
        // A title is a name read and voted on, so it drops what draws
        // nothing too, as every peer's text on a page does; a folder is
        // a path, kept to the letter but for what would act on a terminal.
        share.about = swarm::unmarked(&share.about);
        share.folder = drawable(&share.folder);
        if let Some((cut, _)) = share.about.char_indices().nth(SHARE_FOR_MOST) {
            share.about.truncate(cut);
            // Cleaned again, so a cut falling after a space ends in none,
            // and one falling after a joiner leaves none joining nothing.
            share.about = swarm::unmarked(&share.about);
        }
        // Cleaning and a cut keep what was read, so a long `for` held in
        // every cell would still cost its whole length without this.
        share.about.shrink_to_fit();
        if share.fits() && shares.iter().all(|each| each.key != share.key) {
            shares.push(share);
        }
    }
    shares.shrink_to_fit();
    shares
}

/// The mycelium of one realm, as its node holds it now.
#[derive(Debug, Default, Serialize)]
pub struct Mycelium {
    /// The realm it is, such as `work`.
    pub realm: String,
    /// Every network, in the order the tree draws them.
    pub networks: Vec<Network>,
    /// Every cell: the sessions running or dormant, then the machines.
    pub cells: Vec<Cell>,
    /// What the sessions drawn say they did lately, newest first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventLine>,
    /// Where each network and each cell is among them, by its name, so a
    /// view of thousands finds each one at once.
    #[serde(skip)]
    places: Places,
}

/// Where each network and each cell of a mycelium is, by its name.
#[derive(Debug, Default)]
struct Places {
    networks: HashMap<String, usize>,
    cells: HashMap<String, usize>,
}

impl Places {
    /// Where each of `networks` and `cells` is. A builder isn't placed, so
    /// one named as a machine or a session never hides it, and is found by
    /// its name only where nothing else goes by it.
    fn of(networks: &[Network], cells: &[Cell]) -> Self {
        Self {
            networks: networks
                .iter()
                .enumerate()
                .map(|(place, each)| (each.name.clone(), place))
                .collect(),
            cells: cells
                .iter()
                .enumerate()
                .filter(|(_, each)| each.is != Is::Builder)
                .map(|(place, each)| (each.name.clone(), place))
                .collect(),
        }
    }
}

/// The one of `things` named `name`, where `places` says it is, or found
/// by looking through them where it says wrong, as it would of things
/// changed since.
fn placed<'a, T>(
    things: &'a [T],
    places: &HashMap<String, usize>,
    name: &str,
    named: impl Fn(&T) -> &str,
) -> Option<&'a T> {
    places
        .get(name)
        .and_then(|place| things.get(*place))
        .filter(|thing| named(thing) == name)
        .or_else(|| things.iter().find(|thing| named(thing) == name))
}

/// What a network is for, which says where it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    /// An orchestrator and every session on a quest below it.
    Orchestration,
    /// A quest two cells or more work on.
    Project,
    /// A repository its cells work in.
    Repository,
    /// A work tree two cells or more held at once, while one still does.
    Tree,
    /// A folder two cells or more shared at once, each in its own copy,
    /// while one still does.
    Files,
    /// A group its cells named and joined.
    Group,
    /// The realm's machines.
    Machines,
    /// An outside source the realm's receptors sense, such as Slack.
    Outside,
}

impl Shape {
    /// Each, in the order the tree draws them.
    pub const EACH: [Self; 8] = [
        Self::Orchestration,
        Self::Project,
        Self::Repository,
        Self::Tree,
        Self::Files,
        Self::Group,
        Self::Machines,
        Self::Outside,
    ];

    /// The shape a network's name says, as a group's or a tree's
    /// (`tree.<repo>:<branch>`), or none for a peer's own address.
    fn of_group(group: &str) -> Option<Self> {
        match group.split_once('.').map_or(group, |(first, _)| first) {
            "peer" => None,
            "orchestration" => Some(Self::Orchestration),
            "quest" => Some(Self::Project),
            "repo" => Some(Self::Repository),
            "tree" => Some(Self::Tree),
            // A folder's name holds its key's colon, which no group's can,
            // so a group called `files` or `files.sync` is a group.
            _ => Some(Self::Group),
        }
    }
}

/// One network, and the cells it holds.
#[derive(Debug, Clone, Serialize)]
pub struct Network {
    /// Always `network`, for a reader of mixed lines.
    pub kind: &'static str,
    /// Its name, as the mesh names its group, such as
    /// `orchestration.h2vyob`, `repo.world` or `machines`.
    pub name: String,
    /// What it is for.
    pub shape: Shape,
    /// What a person types to reach it, such as `grow` or `world`.
    pub short: String,
    /// What it is about, where something says.
    pub title: Option<String>,
    /// The shape it is drawn by, such as `square`, where the node chose
    /// one: only an orchestration's, as its team's record says.
    pub glyph: Option<String>,
    /// The network it branched from, by name.
    pub within: Option<String>,
    /// Its cells, by name.
    pub cells: Vec<String>,
    /// The machines its cells run on.
    pub machines: Vec<String>,
    /// The builders the machines' own network has beside its machines,
    /// by name, which are none of its `cells`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub builders: Vec<String>,
    /// How many of its sessions stand as each, by the word each is counted
    /// by: a machine's network counts the sessions its machines run.
    pub counts: BTreeMap<&'static str, u64>,
}

impl Network {
    /// The names a person may type to reach it.
    fn naming(&self) -> swarm::Naming<'_> {
        swarm::Naming {
            address: &self.name,
            short: Some(&self.short),
            title: self.title.as_deref().unwrap_or_default(),
        }
    }

    /// How many of its sessions stand as `each`.
    #[must_use]
    pub(crate) fn count(&self, each: Standing) -> u64 {
        self.counts.get(each.counted()).copied().unwrap_or_default()
    }

    /// How the worst of its sessions stand, none where it has none.
    fn worst(&self) -> Option<Standing> {
        Standing::CELLS
            .into_iter()
            .find(|each| self.count(*each) > 0)
    }

    /// How many sessions it counts at all.
    fn size(&self) -> u64 {
        self.counts.values().sum()
    }
}

/// What a cell is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Is {
    /// A pi working on a quest.
    Session,
    /// A machine of the realm.
    Machine,
    /// What senses an outside source for its person, intake only.
    Receptor,
    /// A builder this machine sends Nix builds to, which is no machine of
    /// the realm.
    Builder,
}

/// What a builder takes, and why it stands as it does.
#[derive(Debug, Clone, Serialize)]
pub struct Builds {
    /// The systems it builds for, such as `x86_64-linux`.
    pub systems: Vec<String>,
    /// How many builds it takes at once.
    pub jobs: u64,
    /// The machines files naming it, by name, such as `standing`.
    pub lines: Vec<String>,
    /// Whether it is lent, so never dialled.
    pub lent: bool,
    /// Why it stands as it does, where it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub said: Option<String>,
}

/// One cell: a session, a machine, a receptor or a builder.
#[derive(Debug, Clone, Serialize)]
pub struct Cell {
    /// Always `cell`, for a reader of mixed lines.
    pub kind: &'static str,
    /// Its name: a session's address, a machine's own name, a receptor's
    /// source and machine, such as `slack@kestrel`.
    pub name: String,
    /// What it is.
    #[serde(rename = "type")]
    pub is: Is,
    /// What it is called, where it says: a session's quest's title, where
    /// it works on one.
    pub title: Option<String>,
    /// A session's id, which stays the same as long as it lives.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// A session's window's title, where it is not its `title`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// How a session stands, by the word it is counted by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standing: Option<&'static str>,
    /// What a session is doing, or asks, where it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doing: Option<String>,
    /// What a session asks its person, in its own words. `doing` carries
    /// the same words for a session that asks, as the line a person reads;
    /// this is the ask alone, for a reader that draws it apart, so keep
    /// both.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asks: Option<String>,
    /// The whole text a session asks its person in, where its want says
    /// more than `asks`' one line: its paragraphs and lines kept, at most
    /// [`ASKED_MOST`] characters, so a reader can show the question whole.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asked: Option<String>,
    /// The repository a session works in, where it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The subjects its presence declares, as [`made::subjects`] keeps them.
    #[serde(skip_serializing_if = "Subjects::is_empty")]
    pub subjects: Subjects,
    /// What a session has spent today, as its presence says it.
    #[serde(skip_serializing_if = "Usage::is_empty")]
    pub usage: Usage,
    /// The networks a session leads, as each orchestration's team names
    /// its lead.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub leads: Vec<String>,
    /// The machine a session or a receptor runs on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// When a session began, from its id, so its place never moves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// The quest a session works on, by its id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quest: Option<String>,
    /// The cell that started a session, by its address: the one its
    /// machine's node's book says asked for its start, else the
    /// orchestrator its presence names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_by: Option<String>,
    /// Where `started_by` was read: `book` or `presence`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_by_from: Option<&'static str>,
    /// What a session's plan does next and what it chose last, as its
    /// presence's `doing` says: `plan`, `item`, `next`, `chose` and `at`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<Value>,
    /// The folder a session works in, as an absolute path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// How a machine is, such as `ready`, or a receptor, such as
    /// `reading`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// How busy a machine's processors are: the percentage, from 0 to 100,
    /// of the last ten seconds in which some task waited on one, as the
    /// kernel's pressure stall information says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub busy: Option<f64>,
    /// How many sessions a machine runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runs: Option<usize>,
    /// How many signals a receptor brought in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counts: Option<crate::receptor::Counts>,
    /// What a builder takes, boxed, as every session's cell carries
    /// none and ten thousand of them would hold its room for nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builds: Option<Box<Builds>>,
    /// The work trees a session holds now, as its presence says.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub trees: Vec<Tree>,
    /// The folders a session shares with others now, as its presence
    /// says.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shares: Vec<Share>,
    /// The networks it joined, by name.
    pub networks: Vec<String>,
}

/// A work tree a session holds now, as its presence says: a worktree
/// checked out at a branch, or a snapshot pinned at a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tree {
    /// Its key, as its machine's work broker names it.
    pub key: String,
    /// The repository it is a tree of, by its key, such as
    /// `github:owner/name` or `local:name`.
    pub repo: String,
    /// The branch a worktree is checked out at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The commit a snapshot is pinned at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

impl Tree {
    /// Its repository's name: its key's last part, such as `oars` of
    /// `github:harbour/oars`.
    fn repo_name(&self) -> &str {
        self.repo.rsplit([':', '/']).next().unwrap_or(&self.repo)
    }

    /// The network it is once two cells or more hold it, and while one
    /// still does, on whichever machines: its repository's whole key and its branch or whole
    /// commit, such as `tree.github:harbour/oars:jsama/tides`, read up to
    /// the last colon, since a branch can't hold one. None for one saying
    /// neither its branch nor its commit.
    fn network(&self) -> Option<String> {
        let (repo, at) = self.place()?;
        Some(format!("tree.{repo}:{at}"))
    }

    /// Where it is: its repository's key and its branch or commit, alike
    /// for every tree of one network. None for one saying neither.
    fn place(&self) -> Option<(&str, &str)> {
        let at = self.branch.as_deref().or(self.commit.as_deref())?;
        Some((&self.repo, at))
    }

    /// What a person may type to reach that network, saying ever more of
    /// its repository, each to be taken where the one before is another
    /// tree's too: its name, such as `oars@jsama/tides`; its owner and
    /// name, such as `harbour/oars@jsama/tides`, or its whole key where
    /// it has no owner; then its whole key. A snapshot's commit is cut to
    /// seven, such as `skiff@6b88106`.
    fn shorts(&self) -> Option<[String; 3]> {
        let at = match (&self.branch, &self.commit) {
            (Some(branch), _) => branch.as_str(),
            (None, Some(commit)) => commit.get(..7).unwrap_or(commit),
            (None, None) => return None,
        };
        let owned = match self.repo.split_once(':') {
            Some((_, owned)) if owned.contains('/') => owned,
            _ => &self.repo,
        };
        Some([self.repo_name(), owned, &self.repo].map(|repo| format!("{repo}@{at}")))
    }
}

/// A folder a session works in with others, as its presence says: each
/// machine keeps its own copy, so the folder is known by its `key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Share {
    /// What it is known by on every machine, such as
    /// `quest:QEST-20261112-HARBOR`.
    pub key: String,
    /// What it is for, which titles its network; empty where it says
    /// nothing, or anything but text, so its network is untitled, and
    /// then left out of its cell's record.
    #[serde(
        rename = "for",
        default,
        deserialize_with = "said_or_none",
        skip_serializing_if = "String::is_empty"
    )]
    pub about: String,
    /// Where this session's copy is.
    pub folder: String,
}

/// What a share says it is for, or empty where it says null or anything
/// but text, as it says nothing, so no folder is refused for its title.
/// Anything else is passed over unbuilt, so a large one costs no copy.
fn said_or_none<'de, D: serde::Deserializer<'de>>(said: D) -> Result<String, D::Error> {
    use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};

    struct Said;
    impl<'de> Visitor<'de> for Said {
        type Value = String;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("what a share is for")
        }
        fn visit_str<E>(self, said: &str) -> Result<String, E> {
            Ok(said.to_owned())
        }
        fn visit_string<E>(self, said: String) -> Result<String, E> {
            Ok(said)
        }
        fn visit_bool<E>(self, _: bool) -> Result<String, E> {
            Ok(String::new())
        }
        fn visit_i64<E>(self, _: i64) -> Result<String, E> {
            Ok(String::new())
        }
        fn visit_u64<E>(self, _: u64) -> Result<String, E> {
            Ok(String::new())
        }
        fn visit_f64<E>(self, _: f64) -> Result<String, E> {
            Ok(String::new())
        }
        fn visit_unit<E>(self) -> Result<String, E> {
            Ok(String::new())
        }
        fn visit_none<E>(self) -> Result<String, E> {
            Ok(String::new())
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<String, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(String::new())
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<String, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(String::new())
        }
    }
    said.deserialize_any(Said)
}

impl Share {
    /// Whether it is within its bounds: a key of letters, digits, colons,
    /// dashes and underscores, beginning with a letter or digit, so its
    /// network's name can't be cut anywhere but its first dot, and saying
    /// its kind, a colon and what it is of, as `quest:<id>`, so its short
    /// name is never another shape's, as no mesh group holds a colon;
    /// and its folder said. What it is for is held to one line, drops
    /// what draws nothing, is trimmed and cut to its bound, and its
    /// folder made [`drawable`], before, in [`shares_of`], so the titles
    /// voted on and those `-o json` prints are the cleaned ones.
    fn fits(&self) -> bool {
        let keyed = |byte: u8| byte.is_ascii_alphanumeric() || b":-_".contains(&byte);
        let key = self.key.as_bytes();
        key.first().is_some_and(u8::is_ascii_alphanumeric)
            && key.len() <= SHARE_KEY_MOST
            && key.iter().all(|byte| keyed(*byte))
            && self
                .key
                .split_once(':')
                .is_some_and(|(_, of)| !of.is_empty())
            && !self.folder.is_empty()
            && self.folder.len() <= SHARE_FOLDER_MOST
    }

    /// The network it is once two cells or more share it, and while one
    /// still does, on whichever machines: `files.<key>`.
    fn network(&self) -> String {
        format!("files.{}", self.key)
    }
}

/// `said`, a folder's path, as a terminal can draw it on one line: each
/// control character and line or paragraph separator a space, and each
/// mark turning text's direction gone, so nothing it says acts on the
/// terminal or turns the text after it. Every other character stays, as
/// the path holds it.
fn drawable(said: &str) -> String {
    said.chars()
        .filter(|each| !crate::forage::turns_direction(*each))
        .map(|each| {
            if each.is_control() || matches!(each, '\u{2028}' | '\u{2029}') {
                ' '
            } else {
                each
            }
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// One thing a session drawn says it did, as a line of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventLine {
    /// Always `event`, for a reader of mixed lines.
    pub kind: &'static str,
    /// The cell that did it, by its name.
    pub cell: String,
    /// Its session, by its id.
    pub session: String,
    /// When, as an RFC 3339 instant.
    pub at: String,
    /// What it did: `commit`, `push`, `pr`, `merge` or `stage`.
    pub what: String,
    /// Where: a repository by its key, or a quest by its id.
    #[serde(rename = "where")]
    pub place: String,
    /// What it is, in one short line.
    pub title: String,
}

/// What a session has spent today, as its presence says it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Usage {
    /// Its cost today, in US dollars.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_today: Option<f64>,
    /// How much of its context window it holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Context>,
    /// How many turns it took today.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turns_today: Option<u64>,
    /// When its last turn began.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_turn: Option<String>,
}

impl Usage {
    /// What the presence of `session` says it spent.
    fn of(session: &Value) -> Self {
        let usage = &session["presence"]["usage"];
        Self {
            cost_today: usage["pi.cost.usd"]["today"].as_f64(),
            context: Context::of(&usage["context"]),
            turns_today: usage["turns"]["today"].as_u64(),
            last_turn: usage["turns"]["last"]["at"].as_str().map(str::to_owned),
        }
    }

    /// Whether it says nothing.
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// How much of its context window a session holds, in tokens, as its
/// presence says it: `{used, limit}`. Two numbers rather than the
/// presence's object as it came, which held about 650 bytes for each of
/// ten thousand cells.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Context {
    /// How many tokens it holds, where it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used: Option<u64>,
    /// How many it may hold, where it says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

impl Context {
    /// What `context`, a presence's, says, or none where it is no object.
    fn of(context: &Value) -> Option<Self> {
        context.is_object().then(|| Self {
            used: context["used"].as_u64(),
            limit: context["limit"].as_u64(),
        })
    }
}

impl Cell {
    /// The names a person may type to reach it.
    fn naming(&self) -> swarm::Naming<'_> {
        swarm::Naming {
            address: &self.name,
            short: None,
            title: self.title.as_deref().unwrap_or_default(),
        }
    }

    /// How a session stands; none for a machine.
    #[must_use]
    pub(crate) fn standing(&self) -> Option<Standing> {
        let word = self.standing?;
        Standing::CELLS
            .into_iter()
            .find(|each| each.counted() == word)
    }

    /// Whether it is a session standing as one of `standings` says, by
    /// the words its counts go by, such as `needs-you`.
    #[must_use]
    pub fn stands_as(&self, standings: &[String]) -> bool {
        self.standing
            .is_some_and(|standing| standings.iter().any(|each| each == standing))
    }
}

/// How a session may stand, by the words its counts go by, worst first.
pub const STANDINGS: [&str; 5] = ["needs-you", "stuck", "moving", "quiet", "dormant"];

impl Mycelium {
    /// Each network, then each cell, a JSON value each, as `-o json`
    /// prints them a line each.
    #[must_use]
    pub fn lines(&self) -> Vec<Value> {
        let networks = self.networks.iter().map(|each| json!(each));
        networks
            .chain(self.cells.iter().map(|each| json!(each)))
            .chain(self.events.iter().map(|each| json!(each)))
            .collect()
    }

    /// The network of this name.
    #[must_use]
    pub fn network(&self, name: &str) -> Option<&Network> {
        placed(&self.networks, &self.places.networks, name, |each| {
            &each.name
        })
    }

    /// The cell of this name.
    #[must_use]
    pub fn cell(&self, name: &str) -> Option<&Cell> {
        placed(&self.cells, &self.places.cells, name, |each| &each.name)
    }

    /// The network `asked` names, by its name, its short name or a part of
    /// its title, as the swarm finds a team; or why none or several are,
    /// naming each of several by its name, so one can be picked.
    ///
    /// # Errors
    ///
    /// When no network, or more than one, goes by what was asked.
    pub fn named_network(&self, asked: &str) -> Result<&Network, String> {
        let mesh_spelt = asked.trim_start_matches('#').replace('/', ".");
        if mesh_spelt != asked {
            match swarm::picked(&self.networks, &mesh_spelt, false, Network::naming) {
                swarm::Picked::One(one) => return Ok(one),
                swarm::Picked::Several(many) => {
                    return Err(several(asked, &many, |each| &each.name));
                }
                swarm::Picked::None => {}
            }
        }
        match swarm::picked(&self.networks, asked, false, Network::naming) {
            swarm::Picked::One(one) => Ok(one),
            swarm::Picked::None => Err(format!("no network is named {asked}")),
            swarm::Picked::Several(many) => Err(several(asked, &many, |each| &each.name)),
        }
    }

    /// The cell `asked` names, by its name or a part of its title; or why
    /// none or several are.
    ///
    /// # Errors
    ///
    /// When no cell, or more than one, goes by what was asked.
    pub fn named_cell(&self, asked: &str) -> Result<&Cell, String> {
        match swarm::picked(&self.cells, asked, false, Cell::naming) {
            swarm::Picked::One(one) => Ok(one),
            swarm::Picked::None => Err(format!("no cell is named {asked}")),
            swarm::Picked::Several(many) => Err(several(asked, &many, |each| &each.name)),
        }
    }
}

/// That `asked` could be any of `many`, each by its `name`, held to one
/// line, since a cell's name is a peer's text.
fn several<T>(asked: &str, many: &[&T], name: impl Fn(&T) -> &str) -> String {
    let names: Vec<String> = many
        .iter()
        .map(|each| swarm::unmarked(name(each)))
        .collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    format!("{asked} could be {}", swarm::either(&names))
}

/// A session's `presence` with only what the networks read of it, so a
/// node making a session's record for them copies little of it.
#[must_use]
pub fn kept_presence(presence: &Value) -> Value {
    kept::pruned(presence, &kept::PRESENCE)
}

#[cfg(test)]
mod tests;
