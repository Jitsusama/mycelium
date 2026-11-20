//! Forages: each living cell's purpose, with the research, plans and
//! audits it keeps for it, read from the quest the cell works on.
//!
//! A forage belongs to its cell and goes with it: a cell of this machine
//! that works on a quest has one, keyed by the cell's address, and it is
//! gone when the cell is. Each node serves its own cells' forages, read
//! from this machine's quest store, so nothing new crosses a link; a quest
//! is never a record of its own, since it isn't a network or a lasting
//! unit but what one cell's purpose is written in.
//!
//! A quest's record is a folder: its `README.md`, whose front matter says
//! its parent and status and whose Summary's first sentence of its own
//! words is its purpose, and its documents under `plans/`, `research/`,
//! `briefs/` and `reports/`, each with front matter saying its id, kind
//! and stage. What it waits on its person for is kept apart, in a file of
//! its own in the waiting folder, named by the last part of its id, less
//! those the launcher keeps beside it as done. A
//! plan can grow past 400 KB, so a document is read only when its length
//! or time changed since it was last read, and a quest only when a watch
//! on its folders hears a change, or on the slow round that catches what
//! a watch missed. What a reader asks is answered from what was read.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::io::{self, BufRead as _, BufReader, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

/// The kind of a forage's records in the daemon interface.
pub const KIND: &str = "forage";

/// The folders a quest keeps its documents in.
pub const DOCUMENTS: [&str; 4] = ["plans", "research", "briefs", "reports"];

/// The longest purpose said, in characters: a sentence, not a paragraph.
const LONGEST_PURPOSE: usize = 280;

/// How long after a change is heard the quest is read, so a document
/// written in several steps is read once, whole.
const SETTLED_AFTER: Duration = Duration::from_millis(250);

/// The longest waiting file read, in bytes: a quest's few waits, never a
/// file grown or written wrong.
const LONGEST_WAITING: u64 = 256 * 1024;

/// The longest file of what's become of the waits read, in bytes. Its
/// person's word on every wait is kept for good, some 2 KB a day, so it
/// holds years of them, though never a file written wrong.
const LONGEST_SETTLED: u64 = 8 * 1024 * 1024;

/// How often every quest held is looked at again, for a change a watch
/// missed, and the quests no cell works on any more let go.
const ROUND: Duration = Duration::from_secs(300);
/// How often the waiting folder is looked at, as a watch can miss it made
/// or removed, and one no watch hears read again, so a wait reaches its
/// person within moments even on a machine past its watches.
const HARK: Duration = Duration::from_secs(5);

/// One document of a quest, as its front matter and its checkboxes say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Document {
    /// Its id, such as `PLAN-20261116-KESTRL`.
    pub id: String,
    /// Its kind: plan, research, brief or report.
    pub kind: String,
    /// Its title, its first heading.
    pub title: String,
    /// Its stage, such as think, draft, build or concluded.
    pub stage: String,
    /// How many of its steps are ticked.
    pub ticked: u32,
    /// How many of its steps are still open.
    pub open: u32,
}

/// A quest's record, as a forage reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Quest {
    /// The first sentence of its Summary.
    pub purpose: Option<String>,
    /// Its status, such as active or concluded.
    pub status: Option<String>,
    /// The quest it sits under, by id.
    pub parent: Option<String>,
    /// Its documents, by id.
    pub documents: Vec<Document>,
    /// What it waits on its person for, as its waiting file says.
    pub waits: Vec<Wait>,
    /// What its README says decides which outside changes feed it, which
    /// no forage serves.
    #[serde(skip)]
    pub claims: Claims,
}

/// What a quest's README says of the outside changes that are its own:
/// the changes and calendar events it claims, the sessions that are its,
/// and its realm. Only the node's feeding reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Claims {
    /// The changes its `github-pr` aliases name, as `owner/name#N`.
    pub changes: Vec<String>,
    /// The calendar events its `calendar-event` aliases name, by id.
    pub events: Vec<String>,
    /// The sessions it lists as active, by id.
    pub sessions: Vec<String>,
    /// Its realm.
    pub realm: Option<String>,
}

/// The most entries of a README's list read, a guard against a file
/// written wrong.
const MOST_LISTED: usize = 500;

/// The entries of the list under the key `key` in front matter `lines`,
/// each as its own keys and values, and a value unquoted as front
/// matter's are. The list's first `- ` sets where its entries start: an
/// entry is a `- ` there, its keys are the lines two further in, as YAML
/// lays a block list's entry, and anything deeper, a session's process
/// or a list of its own, belongs to none of them.
fn listed(lines: &[String], key: &str) -> Vec<HashMap<String, String>> {
    let mut entries: Vec<HashMap<String, String>> = Vec::new();
    let mut within = false;
    let mut dash: Option<usize> = None;
    for line in lines {
        if !line.starts_with([' ', '\t', '-']) {
            within = line
                .split_once(':')
                .is_some_and(|(said, _)| said.trim() == key);
            dash = None;
            continue;
        }
        if !within {
            continue;
        }
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        let field = if let Some(field) = trimmed.strip_prefix("- ") {
            if indent != *dash.get_or_insert(indent) {
                continue;
            }
            if entries.len() == MOST_LISTED {
                break;
            }
            entries.push(HashMap::new());
            field
        } else if dash.is_some_and(|dash| indent == dash + 2) {
            trimmed
        } else {
            continue;
        };
        let (Some(entry), Some((name, value))) = (entries.last_mut(), field.split_once(':')) else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '\'']);
        if !value.is_empty() {
            entry.insert(name.trim().to_owned(), value.to_owned());
        }
    }
    entries
}

/// What the README at `path` says decides which outside changes feed
/// its quest.
fn read_claims(path: &Path) -> Claims {
    let Some(mut lines) = lines_of(path) else {
        return Claims::default();
    };
    if lines.next().as_deref().map(str::trim_end) != Some("---") {
        return Claims::default();
    }
    let front: Vec<String> = lines.take_while(|line| line.trim_end() != "---").collect();
    let aliases = listed(&front, "aliases");
    let of = |kind: &str| -> Vec<String> {
        aliases
            .iter()
            .filter(|alias| alias.get("type").map(String::as_str) == Some(kind))
            .filter_map(|alias| alias.get("value").cloned())
            .collect()
    };
    let changes = of("github-pr");
    let events = of("calendar-event");
    let sessions = listed(&front, "sessions")
        .into_iter()
        .filter(|session| session.get("status").map(String::as_str) == Some("active"))
        .filter_map(|mut session| session.remove("id"))
        .collect();
    let realm = front_matter(
        &mut std::iter::once("---".to_owned())
            .chain(front.iter().cloned())
            .chain(std::iter::once("---".to_owned())),
    )
    .remove("realm");
    Claims {
        changes,
        events,
        sessions,
        realm,
    }
}

/// One thing a quest waits on its person for, as the launcher keeps it in
/// the quest's waiting file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Wait {
    /// Its id, which the launcher clears it by, or its title when it has
    /// none, as the launcher then knows it.
    pub id: String,
    /// What the person is asked to do, in a line.
    pub title: String,
    /// Why it waits on them.
    pub why: String,
    /// About how many whole minutes of theirs it takes.
    pub minutes: Option<u32>,
    /// When it is for, in words, such as `today`.
    pub when: Option<String>,
    /// Whether it can be done now, nothing else waited on first.
    pub ready: bool,
    /// When it was raised.
    pub raised: Option<String>,
}

impl Wait {
    /// The wait `item` says, or none without a title. Any other field
    /// missing, null or written wrong says nothing, so the wait still
    /// shows, known by its title when it has no id.
    fn of(item: &Value) -> Option<Self> {
        let said = |field: &str| {
            item[field]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let title = said("title")?;
        Some(Self {
            id: said("id").unwrap_or_else(|| title.clone()),
            title,
            why: said("why").unwrap_or_default(),
            minutes: item["minutes"]
                .as_u64()
                .and_then(|minutes| u32::try_from(minutes).ok()),
            when: said("when"),
            ready: item["ready"].as_bool().unwrap_or(false),
            raised: said("raisedAt"),
        })
    }
}

/// A waiting file as the launcher writes it: the quest it is for, and
/// each wait, read one by one so one written wrong loses only itself.
#[derive(Deserialize)]
struct Waiting {
    quest: Option<String>,
    #[serde(default)]
    items: Vec<Value>,
}

/// What the launcher keeps beside the waiting folder of what's become of
/// each wait, by its identity, `<quest>:<id>`: what its person said is
/// done (`settled.json`) and what the to do daemon found done or ready
/// (`verdicts.json`). Each is read again only once its file changed.
#[derive(Debug, Clone, Default)]
struct Settled {
    /// `settled.json` and `verdicts.json`, if it reads them.
    files: Option<[PathBuf; 2]>,
    /// How each file stood when last read.
    seen: [Option<Seen>; 2],
    /// The waits `settled.json` last said whole are done, kept while it
    /// is written halfway.
    said_done: BTreeSet<String>,
    /// The waits `verdicts.json` last said whole were found done, kept
    /// while it is written halfway.
    found_done: BTreeSet<String>,
    /// The waits done, by either.
    done: BTreeSet<String>,
    /// The waits a verdict found ready, kept as `found_done` is.
    ready: BTreeSet<String>,
    /// The quests judged since the files were last read, by id.
    judged: BTreeSet<String>,
}

/// How a file stands: its length, when it last changed and which it is,
/// so one written anew in its place is read anew.
type Seen = (u64, Option<SystemTime>, u64);

impl Settled {
    /// What's kept beside the waiting folder `waiting`; nothing for a
    /// folder named with no parent, rather than the working folder.
    fn beside(waiting: &Path) -> Self {
        Self {
            files: waiting
                .parent()
                .filter(|mission| !mission.as_os_str().is_empty())
                .map(|mission| [mission.join("settled.json"), mission.join("verdicts.json")]),
            ..Self::default()
        }
    }

    /// How its files stand now.
    fn now(&self) -> [Option<Seen>; 2] {
        self.files.as_ref().map_or([None, None], |files| {
            files.clone().map(|file| {
                fs::symlink_metadata(file)
                    .ok()
                    .map(|meta| (meta.len(), meta.modified().ok(), meta.ino()))
            })
        })
    }

    /// Whether a file changed since it was last read.
    fn moved(&self) -> bool {
        self.files.is_some() && self.now() != self.seen
    }

    /// Reads the files again if either changed, so every quest is to be
    /// judged again. One gone, a link, longer than 8 MB, one that won't
    /// open, or saying what it says in any other shape settles nothing;
    /// one read whole but not JSON, as one caught being written is, keeps
    /// what it last said, which on the first look is nothing.
    fn refresh(&mut self) {
        let now = self.now();
        let Some([settled, verdicts]) = &self.files else {
            return;
        };
        if now == self.seen {
            return;
        }
        // `None` for a file read whole but not JSON, written halfway.
        let reading = |file: &Path| match read_at_most(file, LONGEST_SETTLED) {
            Some(bytes) => serde_json::from_slice::<Value>(&bytes).ok(),
            None => Some(Value::Null),
        };
        let named = |records: &Value, state: Option<&str>| {
            records
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(_, record)| state.is_none_or(|state| record["state"] == state))
                .map(|(identity, _)| identity.clone())
                .collect::<BTreeSet<String>>()
        };
        if let Some(settled) = reading(settled) {
            self.said_done = named(&settled["done"], None);
        }
        if let Some(verdicts) = reading(verdicts) {
            self.found_done = named(&verdicts["items"], Some("done"));
            self.ready = named(&verdicts["items"], Some("ready"));
        }
        self.done = self.said_done.union(&self.found_done).cloned().collect();
        self.seen = now;
        self.judged.clear();
    }

    /// The waits of quest `id` but those done, each ready that a verdict
    /// found so, as the launcher shows them.
    fn judge(&mut self, id: &str, waits: Vec<Wait>) -> Vec<Wait> {
        self.judged.insert(id.to_owned());
        waits
            .into_iter()
            .filter_map(|mut wait| {
                let identity = format!("{id}:{}", wait.id);
                if self.done.contains(&identity) {
                    return None;
                }
                wait.ready = wait.ready || self.ready.contains(&identity);
                Some(wait)
            })
            .collect()
    }
}

/// A cell of this machine and the quest it works on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// Its address.
    pub address: String,
    /// Its session's id.
    pub session: String,
    /// The quest it works on, by id.
    pub quest: String,
    /// The cell it answers to, which started it, by address.
    pub started_by: Option<String>,
    /// The cell it split from, by address, as its presence says.
    pub split_from: Option<String>,
}

/// The most characters a cell's address holds.
const ADDRESS_MOST: usize = 32;

/// Whether `said` could be a cell's address: letters and digits alone,
/// at most [`ADDRESS_MOST`] of them, so a presence can put nothing else
/// on a forage record as one.
#[must_use]
pub fn is_address(said: &str) -> bool {
    (1..=ADDRESS_MOST).contains(&said.len())
        && said.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// A file as last read: its length and when it last changed.
type Stamp = (u64, Option<SystemTime>);

/// What has been read of the quest store.
#[derive(Debug, Default)]
pub struct Shelf {
    /// The folder of the quests' waiting files, if it reads them.
    waiting: Option<PathBuf>,
    /// What's become of the waits, kept beside their folder.
    settled: Settled,
    /// Each quest a cell works on, as last read.
    quests: BTreeMap<String, Quest>,
    /// Each document file as last read, so an unchanged one isn't read
    /// again.
    files: HashMap<PathBuf, (Stamp, Option<Document>)>,
    /// The quests asked for since the last round.
    asked: BTreeSet<String>,
}

impl Shelf {
    /// A shelf that reads each quest's waits from its file in `waiting`,
    /// but those the launcher keeps beside it as done.
    #[must_use]
    pub fn with_waits(waiting: PathBuf) -> Self {
        Self {
            settled: Settled::beside(&waiting),
            waiting: Some(waiting),
            ..Self::default()
        }
    }

    /// The quest `id` as last read, marking it asked for.
    pub fn quest(&mut self, id: &str) -> Option<&Quest> {
        self.asked.insert(id.to_owned());
        self.quests.get(id)
    }

    /// Reads the quest `id` from `store` again, reading only the
    /// documents that changed. Whether what it holds changed.
    pub fn read(&mut self, store: &Path, id: &str) -> bool {
        let mut quest = read_quest(store, id, &mut self.files);
        if let Some(waiting) = &self.waiting {
            self.settled.refresh();
            quest.waits = self.settled.judge(id, read_waits(waiting, id));
        }
        let same = self.quests.get(id) == Some(&quest);
        self.quests.insert(id.to_owned(), quest);
        !same
    }

    /// Lets go of the quests no cell asked for since the last round, and
    /// their documents, but those a cell in `living` works on, so the
    /// first forage asked for after a quiet spell isn't empty; the ones
    /// it keeps.
    pub fn round(&mut self, store: &Path, living: &BTreeSet<String>) -> BTreeSet<String> {
        let mut asked = std::mem::take(&mut self.asked);
        asked.extend(living.iter().cloned());
        self.quests.retain(|id, _| asked.contains(id));
        let kept: Vec<PathBuf> = self.quests.keys().map(|id| store.join(id)).collect();
        self.files
            .retain(|path, _| kept.iter().any(|folder| path.starts_with(folder)));
        self.quests.keys().cloned().collect()
    }

    /// What it holds of the quests `due` in `store` but the files in
    /// `stale`, as a shelf of its own, so they're read from the disk
    /// without holding this one and every forage made meanwhile.
    #[must_use]
    pub fn lend(&self, store: &Path, due: &BTreeSet<String>, stale: &[PathBuf]) -> Self {
        let folders: Vec<PathBuf> = due.iter().map(|id| store.join(id)).collect();
        Self {
            waiting: self.waiting.clone(),
            settled: self.settled.clone(),
            quests: due
                .iter()
                .filter_map(|id| Some((id.clone(), self.quests.get(id)?.clone())))
                .collect(),
            files: self
                .files
                .iter()
                .filter(|(path, _)| {
                    folders.iter().any(|folder| path.starts_with(folder)) && !stale.contains(path)
                })
                .map(|(path, read)| (path.clone(), read.clone()))
                .collect(),
            asked: BTreeSet::new(),
        }
    }

    /// Takes back what `read`, a shelf it lent, read of its quests in
    /// `store`, in place of what it held of them.
    pub fn take_back(&mut self, store: &Path, read: Self) {
        let folders: Vec<PathBuf> = read.quests.keys().map(|id| store.join(id)).collect();
        self.files
            .retain(|path, _| !folders.iter().any(|folder| path.starts_with(folder)));
        self.files.extend(read.files);
        self.quests.extend(read.quests);
        self.settled = read.settled;
    }

    /// The quests held whose waits weren't judged by what's become of
    /// them as it stands: every one once it changed since it was last
    /// read, else those read before it was, as reading one quest reads it
    /// for all.
    fn settled_moved(&self) -> Vec<String> {
        if self.settled.files.is_none() {
            return Vec::new();
        }
        let moved = self.settled.moved();
        self.quests
            .keys()
            .filter(|id| moved || !self.settled.judged.contains(*id))
            .cloned()
            .collect()
    }

    /// Whether `id` has been read.
    #[must_use]
    pub fn holds(&self, id: &str) -> bool {
        self.quests.contains_key(id)
    }

    /// The quests held whose waiting file is the one named `file`.
    fn waiting_on(&self, file: &Path) -> Vec<String> {
        let Some(name) = file.file_name().and_then(|name| name.to_str()) else {
            return Vec::new();
        };
        self.quests
            .keys()
            .filter(|id| waiting_file(id) == name)
            .cloned()
            .collect()
    }
}

/// What the node holds of its cells' forages.
pub type Held = Arc<Mutex<Shelf>>;

/// Whether `id` can name a quest's folder: one plain name, no path.
fn is_quest_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|each| each.is_ascii_alphanumeric() || each == '-' || each == '_')
}

/// The front matter of `lines`, as key and value, up to its closing
/// `---`; none when it opens without one. A key whose value is YAML's
/// null, a bare `null`, `Null`, `NULL` or `~`, says nothing, as one left empty does; a
/// quoted `"null"` is the word. A ` #` after an unquoted value starts a
/// comment, which is no part of it.
fn front_matter(lines: &mut impl Iterator<Item = String>) -> HashMap<String, String> {
    let mut said = HashMap::new();
    if lines.next().as_deref().map(str::trim_end) != Some("---") {
        return said;
    }
    for line in lines.by_ref() {
        if line.trim_end() == "---" {
            break;
        }
        // A nested line, such as a list's, belongs to the key above it.
        if line.starts_with([' ', '\t', '-']) {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let raw = value.trim();
            // A quoted value is what its quotes hold, a comment after them
            // or not.
            let value = if let Some(mark @ ('"' | '\'')) = raw.chars().next() {
                raw[1..]
                    .split_once(mark)
                    .map_or_else(|| raw.trim_matches(['"', '\'']), |(inner, _)| inner)
            } else {
                let said = raw
                    .char_indices()
                    .find(|&(at, each)| {
                        each == '#' && (at == 0 || raw[..at].ends_with([' ', '\t']))
                    })
                    .map_or(raw, |(at, _)| &raw[..at])
                    .trim_end();
                if matches!(said, "null" | "Null" | "NULL" | "~") {
                    continue;
                }
                said
            };
            if !value.is_empty() {
                said.insert(key.trim().to_owned(), value.to_owned());
            }
        }
    }
    said
}

/// The lines of the file at `path`, never through a link, or none.
fn lines_of(path: &Path) -> Option<impl Iterator<Item = String>> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    Some(BufReader::new(file).lines().map_while(io::Result::ok))
}

/// The sentences of `paragraph`, each ending at a `.`, `!` or `?` before
/// a space or the end. One inside a quote doesn't end its sentence, which
/// ends at the quote's close instead, as `start building." This` does,
/// where anything but a lower-case letter follows; when the quotes don't
/// pair, they're read as any other mark.
fn sentences(paragraph: &str) -> Vec<&str> {
    let straight = paragraph.matches('"').count();
    let paired = straight.is_multiple_of(2)
        && paragraph.matches('\u{201c}').count() == paragraph.matches('\u{201d}').count();
    let mut sentences = Vec::new();
    let (mut from, mut quoted, mut stopped) = (0, false, false);
    for (at, each) in paragraph.char_indices() {
        let after = at + each.len_utf8();
        let closes = match each {
            '"' if paired => {
                quoted = !quoted;
                !quoted
            }
            '\u{201c}' if paired => {
                quoted = true;
                false
            }
            '\u{201d}' if paired => {
                quoted = false;
                true
            }
            _ => false,
        };
        let stops = matches!(each, '.' | '!' | '?');
        let at_end = paragraph[after..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace);
        // Asked only at a quote closing on a stop, so a long run of spaces
        // is walked once.
        let anew = || {
            paragraph[after..]
                .trim_start()
                .chars()
                .next()
                .is_none_or(|next| !next.is_lowercase())
        };
        if at_end && ((stops && !quoted) || (closes && stopped && anew())) {
            sentences.push(paragraph[from..after].trim());
            from = after;
        }
        stopped = stops;
    }
    let rest = paragraph[from..].trim();
    if !rest.is_empty() {
        sentences.push(rest);
    }
    sentences
}

/// The first sentence of `paragraph`, a quote in it kept whole, at most
/// [`LONGEST_PURPOSE`] characters. A quest often opens by quoting its
/// person, and their words are usually the clearest purpose it has.
fn first_sentence(paragraph: &str) -> String {
    let sentence = sentences(paragraph).first().copied().unwrap_or_default();
    if sentence.chars().count() <= LONGEST_PURPOSE {
        return sentence.to_owned();
    }
    // Three plain dots rather than an ellipsis, as every word the node
    // says is plain ASCII punctuation.
    let cut: String = sentence.chars().take(LONGEST_PURPOSE - 3).collect();
    format!("{}...", cut.trim_end())
}

/// What a quest's README says: its parent, its status and its purpose.
fn read_readme(path: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let Some(mut lines) = lines_of(path) else {
        return (None, None, None);
    };
    let mut said = front_matter(&mut lines);
    let mut in_summary = false;
    let mut paragraph: Vec<String> = Vec::new();
    for line in lines {
        let line = line.trim();
        let heading = line.strip_prefix("## ");
        if (line.is_empty() || heading.is_some()) && !paragraph.is_empty() {
            // The template's guidance, left above the quest's own words,
            // isn't them, so the paragraph after it is.
            if !is_guidance(&paragraph.join(" ")) {
                break;
            }
            paragraph.clear();
        }
        if let Some(heading) = heading {
            in_summary = heading.contains("Summary");
            continue;
        }
        // Guidance on a line of its own, right above the quest's words,
        // isn't them either.
        if !in_summary || line.is_empty() || (paragraph.is_empty() && is_guidance(line)) {
            continue;
        }
        // A quote of the person's words leads with who said it; the purpose
        // is the quest's own sentence, so the first plain line starts it.
        paragraph.push(line.to_owned());
    }
    let summary = paragraph.join(" ");
    let purpose = (!summary.is_empty() && !is_guidance(&summary)).then(|| first_sentence(&summary));
    (said.remove("parent"), said.remove("status"), purpose)
}

/// Whether `paragraph` is one span in italics, as the quest template's
/// guidance is, left for the quest's own words to replace. A paragraph
/// that only opens and closes on an italic word, or is in bold, is the
/// quest's own.
fn is_guidance(paragraph: &str) -> bool {
    ['_', '*'].into_iter().any(|mark| {
        paragraph
            .strip_prefix(mark)
            .and_then(|rest| rest.strip_suffix(mark))
            .is_some_and(|inner| !inner.is_empty() && !inner.contains(mark))
    })
}

/// The name of the waiting file of quest `id`: the last part of its id,
/// as the launcher names it, such as `KESTRL.json`.
fn waiting_file(id: &str) -> String {
    let short = id.rsplit_once('-').map_or(id, |(_, short)| short);
    format!("{short}.json")
}

/// What quest `id` waits on its person for, as its file in `waiting`
/// says: none when there's no file, it's a link, it's too long to be one,
/// it doesn't parse or it doesn't name this quest as its own. A wait written wrong is left
/// out, keeping the rest.
fn read_waits(waiting: &Path, id: &str) -> Vec<Wait> {
    if !is_quest_id(id) {
        return Vec::new();
    }
    let Some(file) = read_at_most(&waiting.join(waiting_file(id)), LONGEST_WAITING)
        .and_then(|bytes| serde_json::from_slice::<Waiting>(&bytes).ok())
    else {
        return Vec::new();
    };
    // Quests whose ids end alike share a file name, so only the quest the
    // file names is believed.
    if file.quest.as_deref() != Some(id) {
        return Vec::new();
    }
    file.items.iter().filter_map(Wait::of).collect()
}

/// What the regular file at `path` holds, or none when it holds more than
/// `most` bytes. It is opened never through a link and without waiting
/// on a writer, and what was opened is what is judged, so a file swapped
/// in meanwhile can neither hold nor flood its reader.
pub(crate) fn read_at_most(path: &Path, most: u64) -> Option<Vec<u8>> {
    let opened = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .ok()?;
    let file = fs::File::from(opened);
    if !file.metadata().ok()?.file_type().is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(most + 1).read_to_end(&mut bytes).ok()?;
    (u64::try_from(bytes.len()).ok()? <= most).then_some(bytes)
}

/// What a document says of itself, or none when it isn't one.
fn read_document(path: &Path) -> Option<Document> {
    let mut lines = lines_of(path)?;
    let mut said = front_matter(&mut lines);
    let id = said.remove("id")?;
    let mut title = None;
    let (mut ticked, mut open) = (0_u32, 0_u32);
    for line in lines {
        let line = line.trim_start();
        if title.is_none()
            && let Some(heading) = line.strip_prefix("# ")
        {
            title = Some(heading.trim().to_owned());
            continue;
        }
        if line.starts_with("- [x]") || line.starts_with("- [X]") {
            ticked = ticked.saturating_add(1);
        } else if line.starts_with("- [ ]") {
            open = open.saturating_add(1);
        }
    }
    Some(Document {
        title: title.unwrap_or_else(|| id.clone()),
        id,
        kind: said.remove("kind").unwrap_or_default(),
        stage: said.remove("stage").unwrap_or_default(),
        ticked,
        open,
    })
}

/// When the file at `path` last changed and how long it is.
fn stamp(path: &Path) -> Option<Stamp> {
    let meta = fs::symlink_metadata(path).ok()?;
    meta.file_type()
        .is_file()
        .then(|| (meta.len(), meta.modified().ok()))
}

/// The quest `id` as `store` holds it, reading a document only when it
/// changed since `files` last read it.
fn read_quest(
    store: &Path,
    id: &str,
    files: &mut HashMap<PathBuf, (Stamp, Option<Document>)>,
) -> Quest {
    if !is_quest_id(id) {
        return Quest::default();
    }
    let folder = store.join(id);
    let (parent, status, purpose) = read_readme(&folder.join("README.md"));
    let claims = read_claims(&folder.join("README.md"));
    let mut documents = Vec::new();
    for kind in DOCUMENTS {
        let Ok(entries) = fs::read_dir(folder.join(kind)) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
            .collect();
        paths.sort();
        for path in paths {
            let Some(now) = stamp(&path) else {
                continue;
            };
            let document = match files.get(&path) {
                Some((was, document)) if *was == now => document.clone(),
                _ => {
                    let document = read_document(&path);
                    files.insert(path, (now, document.clone()));
                    document
                }
            };
            documents.extend(document);
        }
    }
    Quest {
        purpose,
        status,
        parent,
        documents,
        waits: Vec::new(),
        claims,
    }
}

/// The quest a change at `path` in `store` is to, if any.
fn quest_of(store: &Path, path: &Path) -> Option<String> {
    match path.strip_prefix(store).ok()?.components().next()? {
        Component::Normal(name) => name.to_str().map(str::to_owned),
        _ => None,
    }
}

/// The forage of each of `cells`, keyed as the daemon interface keys
/// them, from what `shelf` holds; and the quests it hasn't read yet.
pub fn records(
    shelf: &mut Shelf,
    cells: &[Cell],
    machine: &str,
) -> (BTreeMap<String, Value>, Vec<String>) {
    let mut unread = Vec::new();
    let records = cells
        .iter()
        .filter_map(|cell| {
            let Some(quest) = shelf.quest(&cell.quest) else {
                unread.push(cell.quest.clone());
                return None;
            };
            // A quest the store has no record of gives the cell no forage,
            // rather than an empty one.
            if *quest == Quest::default() {
                return None;
            }
            let mut fields = json!({
                "cell": cell.address,
                "session": cell.session,
                "machine": machine,
                "quest": cell.quest,
                "purpose": quest.purpose,
                "status": quest.status,
                "documents": quest.documents,
                "lineage": {
                    "started_by": cell.started_by,
                    "split_from": cell.split_from,
                    "parent": quest.parent,
                },
            });
            // Most quests wait on nobody, and ten thousand empty lists
            // would weigh on the node for nothing.
            if !quest.waits.is_empty() {
                fields["waits"] = json!(quest.waits);
            }
            let key = crate::daemon::key(KIND, &cell.address);
            Some((key, crate::daemon::record(KIND, &cell.address, 0, fields)))
        })
        .collect();
    unread.sort();
    unread.dedup();
    (records, unread)
}

/// The folder of the quests' waiting files, heard apart from the quests'
/// own folders and for as long as the node runs, since its files come and
/// go as waits do.
struct WaitingFolder {
    /// The folder as the node names it; none when it reads no waits.
    folder: Option<PathBuf>,
    /// Once the folder is read, its real path, which a watch may say a
    /// change at instead, and which folder it is, by device and inode, so
    /// one removed and made anew is heard anew.
    heard: Option<(PathBuf, (u64, u64))>,
    /// The files the folder held when last read, said again once they're
    /// gone, so a quest whose file went reads its waits again however it
    /// went: removed alone, with the folder, or with no watch hearing.
    files: BTreeSet<PathBuf>,
    /// Whether the folder was read with no watch hearing it, so is read
    /// again each hark rather than each turn, which would never wait.
    deaf: bool,
    /// Whether the next turn reads the folder again and listens to it
    /// afresh, though it seems the same folder.
    again: bool,
    /// Whether a watch hears the folder's parent, as it does while the
    /// folder isn't there, to hear it made.
    awaited: bool,
}

impl WaitingFolder {
    /// The waiting folder `held` reads its waits from.
    fn of(held: &Held) -> Self {
        let folder = held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .waiting
            .clone();
        Self {
            folder,
            heard: None,
            files: BTreeSet::new(),
            deaf: false,
            again: false,
            awaited: false,
        }
    }

    /// Reads the folder again and listens to it afresh on the next turn,
    /// as a watch may have died with no word, or one remade on the same
    /// inode never been heard; and hears a missing folder's parent anew.
    fn round(&mut self) {
        self.again = self.heard.is_some();
        if self.heard.is_none() {
            self.awaited = false;
        }
    }

    /// Reads a folder no watch hears again on the next turn. The turn
    /// also looks whether the folder came or went, as a watch can miss
    /// either, as a Mac's was seen to; a watch on a missing folder's
    /// parent is left as it is. Harks come only while there is a
    /// watcher; with none at all, only the five-minute round reads the
    /// quests' waits again, a folder made late among them.
    fn hark(&mut self) {
        self.again = self.again || self.deaf;
    }

    /// The quests `held` holds whose waiting file is among `touched`.
    fn changed(&self, held: &Held, touched: &[PathBuf]) -> Vec<String> {
        let Some(folder) = &self.folder else {
            return Vec::new();
        };
        let resolved = self.heard.as_ref().map(|(resolved, _)| resolved);
        let holding = held.lock().unwrap_or_else(PoisonError::into_inner);
        touched
            .iter()
            .filter(|path| {
                path.starts_with(folder) || resolved.is_some_and(|real| path.starts_with(real))
            })
            .flat_map(|path| holding.waiting_on(path))
            .collect()
    }

    /// Reads the folder again and listens to it afresh when `touched`
    /// holds the folder itself, as when it is removed or made: a watch
    /// dies with its folder, one made anew may take the same inode, and a
    /// Mac may say the folder for a change to its files.
    fn heard_of(&mut self, touched: &[PathBuf]) {
        let Some(folder) = &self.folder else {
            return;
        };
        let real = self.heard.as_ref().map(|(real, _)| real);
        if touched
            .iter()
            .any(|path| path == folder || real.is_some_and(|real| path == real))
        {
            self.again = true;
        }
    }

    /// Has `watcher` hear the folder as it stands: the folder while it's
    /// there, its parent while it isn't, and a folder made anew afresh.
    /// Says the files in a folder read anew, which may have been written
    /// before any watch heard them, and those it held when last read and
    /// holds no longer.
    fn listen(&mut self, watcher: &mut impl notify::Watcher) -> Vec<PathBuf> {
        let Some(folder) = &self.folder else {
            return Vec::new();
        };
        let now = fs::metadata(folder)
            .ok()
            .filter(fs::Metadata::is_dir)
            .map(|meta| (meta.dev(), meta.ino()));
        let parent = folder.parent().filter(|parent| parent.is_dir());
        match (&self.heard, now) {
            (Some((_, was)), Some(now)) if *was == now && !self.again => Vec::new(),
            (_, Some(now)) => {
                self.again = false;
                // A watch on a folder since removed is gone or stale.
                let _ = watcher.unwatch(folder);
                self.deaf = watcher
                    .watch(folder, notify::RecursiveMode::NonRecursive)
                    .is_err();
                let real = fs::canonicalize(folder).unwrap_or_else(|_| folder.clone());
                self.heard = Some((real, now));
                if self.awaited
                    && let Some(parent) = parent
                {
                    let _ = watcher.unwatch(parent);
                    self.awaited = false;
                }
                let files: BTreeSet<PathBuf> = fs::read_dir(folder)
                    .map(|entries| {
                        entries
                            .filter_map(Result::ok)
                            .map(|entry| folder.join(entry.file_name()))
                            .collect()
                    })
                    .unwrap_or_default();
                let said = files.union(&self.files).cloned().collect();
                self.files = files;
                said
            }
            (_, None) => {
                // The files of a folder now gone are said, so their quests
                // read their waits again, whether or not a watch heard.
                self.heard = None;
                self.deaf = false;
                self.again = false;
                if !self.awaited
                    && let Some(parent) = parent
                {
                    self.awaited = watcher
                        .watch(parent, notify::RecursiveMode::NonRecursive)
                        .is_ok();
                }
                std::mem::take(&mut self.files).into_iter().collect()
            }
        }
    }
}

/// Keeps `held` true to `store` for as long as the node runs: reads each
/// quest `wanted` names, and each one held again when a watch on its
/// folders hears a change, telling `tell` whenever what it holds changed.
/// `sessions` says when the node's cells may have changed, which changes
/// whose forages there are, so it tells `tell` then too, once `cells`,
/// this machine's cells as they stand, differ from those it last told of.
/// Each quest is read on a shelf `held` lends, so a round over thousands
/// of quests never keeps a forage waiting on the disk.
pub async fn keep(
    store: PathBuf,
    held: Held,
    mut wanted: mpsc::UnboundedReceiver<String>,
    mut sessions: watch::Receiver<u64>,
    cells: impl Fn() -> Vec<Cell> + Send + 'static,
    tell: watch::Sender<u64>,
) {
    let mut told_of: Vec<Cell> = Vec::new();
    let (heard, mut changed) = mpsc::unbounded_channel::<PathBuf>();
    let mut watcher = crate::watching::watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            for path in event.paths {
                // Nothing hears this once the node has stopped.
                let _ = heard.send(path);
            }
        }
    })
    .ok();
    // A watch may say a change at the store's real path, through any link
    // on the way to it, as macOS's does for a folder under /var.
    // Each is said again under the store as the node names it.
    let resolved = fs::canonicalize(&store).unwrap_or_else(|_| store.clone());
    let in_store = |path: PathBuf| match path.strip_prefix(&resolved) {
        Ok(within) if !path.starts_with(&store) => store.join(within),
        _ => path,
    };
    let mut listened_to: BTreeSet<PathBuf> = BTreeSet::new();
    let mut waiting = WaitingFolder::of(&held);
    let mut rounds = tokio::time::interval(ROUND);
    rounds.tick().await;
    let mut harks = tokio::time::interval(HARK);
    harks.tick().await;
    loop {
        // Waiting files a watch may not have heard written are read at
        // once, as no change may come to wake the loop for them.
        let mut touched = watcher
            .as_mut()
            .map(|watcher| waiting.listen(watcher))
            .unwrap_or_default();
        let mut due: BTreeSet<String> = waiting.changed(&held, &touched).into_iter().collect();
        if due.is_empty() {
            tokio::select! {
            Some(id) = wanted.recv() => {
                due.insert(id);
                while let Ok(id) = wanted.try_recv() {
                    due.insert(id);
                }
            }
            Some(path) = changed.recv() => {
                tokio::time::sleep(SETTLED_AFTER).await;
                touched.push(in_store(path));
                while let Ok(path) = changed.try_recv() {
                    touched.push(in_store(path));
                }
                due.extend(touched.iter().filter_map(|path| quest_of(&store, path)));
                due.extend(waiting.changed(&held, &touched));
                waiting.heard_of(&touched);
            }
            Ok(()) = sessions.changed() => {
                // A presence beats every few seconds while its cells seldom
                // move: the beats within the next moment are heard as one,
                // and told only when a cell came, went or moved quest.
                tokio::time::sleep(SETTLED_AFTER).await;
                sessions.borrow_and_update();
                let now = cells();
                if now != told_of {
                    told_of = now;
                    tell.send_modify(|changes| *changes += 1);
                }
                continue;
            }
            _ = harks.tick(), if waiting.folder.is_some() && watcher.is_some() => {
                // The next turn looks whether the folder came or went, and
                // reads one no watch hears again, as on a machine past its
                // watches; and every quest's waits are read again once
                // what's become of them, kept beside the folder, changed.
                waiting.hark();
                due.extend(held.lock().unwrap_or_else(PoisonError::into_inner).settled_moved());
            }
            _ = rounds.tick() => {
                waiting.round();
                let living: BTreeSet<String> = cells().into_iter().map(|cell| cell.quest).collect();
                due.extend(round(&held, &store, &living, watcher.as_mut(), &mut listened_to));
            }
            }
        }
        due.retain(|id| is_quest_id(id));
        if due.is_empty() {
            continue;
        }
        let (reading, of, shelf) = (store.clone(), due.clone(), Arc::clone(&held));
        let read = tokio::task::spawn_blocking(move || {
            // A file a watch heard written is read again even when its
            // length and time look the same, as a step ticked within the
            // clock's last tick does.
            let mut lent = shelf
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .lend(&reading, &of, &touched);
            let moved = of
                .iter()
                .fold(false, |moved, id| lent.read(&reading, id) | moved);
            shelf
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take_back(&reading, lent);
            moved
        });
        // A reading that panicked leaves the forages as last read.
        if read.await.unwrap_or(false) {
            tell.send_modify(|changes| *changes += 1);
        }
        if let Some(watcher) = watcher.as_mut() {
            listen_to(watcher, &store, &due, &mut listened_to);
        }
    }
}

/// Has `held` let go of the quests in `store` no cell asked for since
/// the last round and none `living` works on, and `watcher` stop hearing
/// their folders among `listened_to`; the quests it keeps.
fn round(
    held: &Held,
    store: &Path,
    living: &BTreeSet<String>,
    watcher: Option<&mut impl notify::Watcher>,
    listened_to: &mut BTreeSet<PathBuf>,
) -> BTreeSet<String> {
    let kept = held
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .round(store, living);
    let keeping: BTreeSet<PathBuf> = kept.iter().flat_map(|id| folders(store, id)).collect();
    if let Some(watcher) = watcher {
        for gone in listened_to.difference(&keeping) {
            // A folder already gone is no longer watched.
            let _ = watcher.unwatch(gone);
        }
    }
    listened_to.retain(|folder| keeping.contains(folder));
    kept
}

/// Has `watcher` hear the folders of each quest `due` in `store` that
/// are there and not yet among `listened_to`.
fn listen_to(
    watcher: &mut impl notify::Watcher,
    store: &Path,
    due: &BTreeSet<String>,
    listened_to: &mut BTreeSet<PathBuf>,
) {
    for folder in due.iter().flat_map(|id| folders(store, id)) {
        if !listened_to.contains(&folder)
            && folder.is_dir()
            && watcher
                .watch(&folder, notify::RecursiveMode::NonRecursive)
                .is_ok()
        {
            listened_to.insert(folder);
        }
    }
}

/// The forages of `records` that `named` names, by its cell's address or
/// its quest's id, ignoring case; all of them when it names none. Sorted
/// by machine, then address.
#[must_use]
pub fn chosen(records: Vec<Value>, named: Option<&str>) -> Vec<Value> {
    let said = |record: &Value, field: &str| record[field].as_str().unwrap_or_default().to_owned();
    let mut chosen: Vec<Value> = records
        .into_iter()
        .filter(|record| {
            named.is_none_or(|name| {
                ["cell", "quest"]
                    .iter()
                    .any(|field| said(record, field).eq_ignore_ascii_case(name))
            })
        })
        .collect();
    chosen.sort_by_key(|record| (said(record, "machine"), said(record, "cell")));
    chosen
}

/// `text` as one line for a terminal: each control character, a newline
/// or an escape among them, as a space, and each mark that turns text's
/// direction left out, so words read from a file can't draw lines of
/// their own, steer the terminal, or read backwards past their own end.
fn one_line(text: &str) -> String {
    text.chars()
        .filter(|each| !turns_direction(*each))
        .map(|each| if each.is_control() { ' ' } else { each })
        .collect()
}

/// Whether `each` is one of Unicode's marks that turn the direction text
/// is drawn in, which a terminal may honour past the words they came in.
pub(crate) const fn turns_direction(each: char) -> bool {
    matches!(each, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// The most lines a cell's purpose takes when drawn, so each block leads
/// with what the cell is for and what it waits on, not a quote's whole
/// paragraph.
const PURPOSE_LINES: usize = 2;

/// How many spaces [`first_sentence`] may trim from the end of what it
/// keeps of a sentence it cuts, so a purpose that short of its length,
/// before its three dots, was still cut there.
const CUT_TRIMS: usize = 8;

/// What [`first_sentence`] kept of a purpose it cut at its length, before
/// the three dots it ends one with; a purpose shorter, its dots its
/// author's, has none.
fn read_cut(purpose: &str) -> Option<&str> {
    let kept = purpose.strip_suffix("...")?.trim_end();
    (kept.chars().count() + CUT_TRIMS >= LONGEST_PURPOSE - 3).then_some(kept)
}

/// A purpose, `said` as its node wrote it, cleaned and wrapped under its
/// indent in `width` columns, in at most [`PURPOSE_LINES`] unless it is
/// drawn `whole`; a purpose cut there, or read already cut at its length,
/// ends in one ellipsis after a whole word.
fn purpose_drawn(said: &str, width: usize, whole: bool) -> String {
    use crate::swarm::wrapped;
    // Judged as its node counted it, before the marks one_line drops.
    let length = read_cut(said).map(|kept| kept.chars().count());
    let cleaned = one_line(said);
    let (purpose, read_cut) = match (length, cleaned.strip_suffix("...")) {
        // Cut where it reached its length, often within a word, so that
        // word goes too and the mark comes after a whole one; a single
        // word is kept, to be cut within as one too long is.
        (Some(length), Some(kept)) if length >= LONGEST_PURPOSE - 3 => {
            let kept = kept.trim_end();
            (
                kept.rsplit_once(' ')
                    .map_or(kept, |(before, _)| before.trim_end()),
                true,
            )
        }
        // A cut that fell on a space, its trim leaving it short, left
        // the word before it whole.
        (Some(_), Some(kept)) => (kept.trim_end(), true),
        _ => (cleaned.as_str(), false),
    };
    let most = if whole { usize::MAX } else { PURPOSE_LINES };
    let drawn = wrapped(purpose, "  ", "  ", width);
    if !read_cut && drawn.lines().count() <= most {
        return drawn;
    }
    let mut kept: Vec<String> = drawn.lines().take(most).map(str::to_owned).collect();
    // Drawn whole, every word read stays, so a mark with no room left on
    // the last line takes one of its own.
    let full = kept
        .last()
        .is_some_and(|last| last.chars().count() + 2 > width);
    if whole && full {
        kept.push("  \u{2026}".to_owned());
    } else if let Some(last) = kept.last_mut() {
        while last.chars().count() + 2 > width {
            if let Some(at) = last.trim_end().rfind(' ').filter(|at| *at > 2) {
                last.truncate(at);
            } else {
                *last = last.chars().take(width.saturating_sub(2)).collect();
                break;
            }
        }
        last.push_str(" …");
    }
    kept.iter().fold(String::new(), |mut out, line| {
        out.push_str(line);
        out.push('\n');
        out
    })
}

/// Whether `document` is closed, concluded or retired, so a list of every
/// cell counts it rather than giving it a row.
fn closed(document: &Value) -> bool {
    matches!(document["stage"].as_str(), Some("concluded" | "retired"))
}

/// A forage's `documents` as rows in `width` columns, each title cut where
/// its row would pass `width`. Unless a cell is `named`, the closed ones
/// are counted in a line of their own, saying how `cell`'s are listed.
fn documents_drawn(documents: &[Value], cell: &str, named: Option<&str>, width: usize) -> String {
    use crate::swarm::{fit, wrapped};
    if documents.is_empty() {
        return wrapped("No research, plans or reports yet.", "  ", "  ", width);
    }
    let text = |value: &Value| one_line(value.as_str().unwrap_or_default());
    let shown: Vec<&Value> = documents
        .iter()
        .filter(|each| named.is_some() || !closed(each))
        .collect();
    let kinds = shown
        .iter()
        .map(|each| text(&each["kind"]).len())
        .max()
        .unwrap_or(0);
    let stages = shown
        .iter()
        .map(|each| text(&each["stage"]).len())
        .max()
        .unwrap_or(0);
    let mut drawn = String::new();
    for document in &shown {
        let (ticked, open) = (
            document["ticked"].as_u64().unwrap_or(0),
            document["open"].as_u64().unwrap_or(0),
        );
        let steps = if ticked + open == 0 {
            "no steps".to_owned()
        } else {
            format!("{ticked} of {} ticked", ticked + open)
        };
        let row = format!(
            "  {:kinds$}  {:stages$}  {steps:<16}  {}",
            text(&document["kind"]),
            text(&document["stage"]),
            text(&document["title"]),
        );
        // Writing to a String never fails.
        let _ = writeln!(drawn, "{}", fit(&row, width));
    }
    let counted = documents.len() - shown.len();
    if counted > 0 {
        // A cell's address comes as its node wrote it, unchecked, so only
        // one that is an address as written is offered as a command.
        let lists = if is_address(cell) {
            format!("mycelium forage {cell}")
        } else {
            "its cell's forage".to_owned()
        };
        let said = if shown.is_empty() {
            format!("{counted} closed, none open, which {lists} lists")
        } else {
            format!("and {counted} closed, which {lists} lists")
        };
        drawn.push_str(&wrapped(&said, "  ", "  ", width));
    }
    drawn
}

/// `forages` drawn for a person in `width` columns: a block each, those
/// that wait on them first, its cell, machine and quest, its purpose in
/// two lines at most, or whole for a cell `named`, a line for each thing
/// it waits on them for, then its documents. Says so in words when there are none, or none `named` names.
#[must_use]
pub fn draw(forages: &[Value], named: Option<&str>, width: usize) -> String {
    use crate::swarm::{fit, wrapped};
    if forages.is_empty() {
        let said = match named {
            Some(name) => format!("No cell named {name} has a forage."),
            None => "No cell works on a quest, so none has a forage.".to_owned(),
        };
        return wrapped(&said, "", "", width);
    }
    let text = |value: &Value| one_line(value.as_str().unwrap_or_default());
    let mut ordered: Vec<&Value> = forages.iter().collect();
    // A stable sort, so those that wait keep the order they came in.
    ordered.sort_by_key(|forage| forage["waits"].as_array().is_none_or(Vec::is_empty));
    let blocks: Vec<String> = ordered
        .into_iter()
        .map(|forage| {
            let heading = format!(
                "{}  on {}  {}",
                text(&forage["cell"]),
                text(&forage["machine"]),
                text(&forage["quest"])
            );
            let mut block = fit(&heading, width);
            block.push('\n');
            let purpose = forage["purpose"]
                .as_str()
                .unwrap_or("Its quest says no purpose yet.");
            block.push_str(&purpose_drawn(purpose, width, named.is_some()));
            for wait in forage["waits"].as_array().map_or(&[][..], Vec::as_slice) {
                let when = wait["when"]
                    .as_str()
                    .filter(|when| !when.is_empty())
                    .map(one_line);
                let stands = match (wait["ready"].as_bool().unwrap_or(false), when) {
                    (true, Some(when)) => format!(" (ready, {when})"),
                    (true, None) => " (ready)".to_owned(),
                    (false, Some(when)) => format!(" ({when})"),
                    (false, None) => String::new(),
                };
                let said = format!("Waits on you: {}{stands}", text(&wait["title"]));
                block.push_str(&wrapped(&said, "  ", "    ", width));
            }
            let documents = forage["documents"]
                .as_array()
                .map_or(&[][..], Vec::as_slice);
            block.push_str(&documents_drawn(
                documents,
                forage["cell"].as_str().unwrap_or_default(),
                named,
                width,
            ));
            block
        })
        .collect();
    blocks.join("\n")
}

/// The folders of quest `id` a watch hears its changes in.
fn folders(store: &Path, id: &str) -> impl Iterator<Item = PathBuf> {
    let folder = store.join(id);
    std::iter::once(folder.clone()).chain(DOCUMENTS.map(|kind| folder.join(kind)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A quest store in a temporary folder, removed when dropped.
    struct Store(PathBuf);

    impl Store {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("mycelium-forage-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("a store");
            Self(path)
        }

        fn write(&self, relative: &str, text: &str) {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().expect("a folder")).expect("its folder");
            fs::write(path, text).expect("written");
        }
    }

    impl Drop for Store {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const README: &str = "---\nid: QEST-20261116-KESTRL\nkind: subquest\nparent: QEST-20261101-OSPREY\nstatus: active\naliases:\n  - type: url\n    value: https://example.org\n---\n# Teach Kestrel to Hover\n\n## 📜 Summary\n\nKestrel learns to hover over one field. It then\nlearns a second.\n\n## 🧭 Purpose\n\nNot this.\n";

    const PLAN: &str = "---\nid: PLAN-20261116-HOVERS\nkind: plan\nquest: QEST-20261116-KESTRL\nstage: build\n---\n# Hover Over the Field\n\n## Steps\n\n- [x] lift\n- [x] hold\n  - [ ] a nested step\n- [ ] turn\n";

    fn cell(quest: &str) -> Cell {
        Cell {
            address: "KESTRL".to_owned(),
            session: "0001".to_owned(),
            quest: quest.to_owned(),
            started_by: Some("OSPREY".to_owned()),
            split_from: Some("TERNSS".to_owned()),
        }
    }

    #[test]
    fn an_address_is_letters_and_digits_alone_and_not_too_many() {
        for address in ["KP0L6A", "C554836B", &"A".repeat(ADDRESS_MOST)] {
            assert!(is_address(address), "{address}");
        }
        for not in [
            "",
            "peer.kp0l6a",
            "KP0L6A ",
            "K\u{202e}P",
            &"A".repeat(ADDRESS_MOST + 1),
        ] {
            assert!(!is_address(not), "{not:?}");
        }
    }

    #[test]
    fn a_quest_whose_parent_is_null_has_none() {
        let store = Store::new("null-parent");
        let unquoted = README.replace("parent: QEST-20261101-OSPREY", "parent: null");
        store.write("QEST-20261116-KESTRL/README.md", &unquoted);
        store.write(
            "QEST-20261116-SKIFFS/README.md",
            &README.replace("parent: QEST-20261101-OSPREY", "parent: ~"),
        );
        store.write(
            "QEST-20261116-OSPREY/README.md",
            &README.replace("parent: QEST-20261101-OSPREY", "parent: \"null\""),
        );
        let mut shelf = Shelf::default();
        for quest in [
            "QEST-20261116-KESTRL",
            "QEST-20261116-SKIFFS",
            "QEST-20261116-OSPREY",
        ] {
            assert!(shelf.read(&store.0, quest), "{quest} is read");
        }
        let mut parent = |quest: &str| shelf.quest(quest).expect("read").parent.clone();
        assert_eq!(
            parent("QEST-20261116-KESTRL"),
            None,
            "YAML's null is no parent"
        );
        assert_eq!(
            parent("QEST-20261116-SKIFFS"),
            None,
            "YAML's ~ is no parent"
        );
        assert_eq!(
            parent("QEST-20261116-OSPREY").as_deref(),
            Some("null"),
            "a quoted null is the word"
        );
    }

    #[test]
    fn a_quest_claims_its_github_aliases_for_its_active_sessions_alone() {
        let store = Store::new("claims");
        let front = "---\nid: QEST-20261116-KESTRL\nrealm: work\naliases:\n  - type: \
            github-pr\n    value: jsama/tern#12\n  - type: slack-message\n    value: \
            C0/1\n  - type: github-pr\n    value: \"jsama/tern#13\"\n  - type: \
            calendar-event\n    value: abcde_20261118T150000Z\nsessions:\n  - id: \
            s-live\n    status: active\n    process:\n      status: ended\n  - id: \
            s-gone\n    status: ended\nparent: QEST-20261101-OSPREY\n";
        let readme = README.replacen("---\n", front, 1);
        store.write("QEST-20261116-KESTRL/README.md", &readme);
        let mut shelf = Shelf::default();
        assert!(shelf.read(&store.0, "QEST-20261116-KESTRL"), "read");
        let quest = shelf.quest("QEST-20261116-KESTRL").expect("read");
        assert_eq!(
            quest.claims,
            Claims {
                changes: vec!["jsama/tern#12".to_owned(), "jsama/tern#13".to_owned()],
                events: vec!["abcde_20261118T150000Z".to_owned()],
                sessions: vec!["s-live".to_owned()],
                realm: Some("work".to_owned()),
            }
        );
        let served = serde_json::to_string(quest).expect("json");
        assert!(
            !served.contains("tern")
                && !served.contains("s-live")
                && !served.contains("abcde_2026"),
            "a forage served what feeds its quest: {served}"
        );
    }

    #[test]
    fn claims_are_read_at_any_list_indent_and_a_nested_list_claims_nothing() {
        let store = Store::new("claims-laid-out");
        // Lists flush with their key, as YAML allows, and a session holding
        // a list of its own whose entries look like sessions.
        let front = "---\nid: QEST-20261116-KESTRL\nrealm: work\naliases:\n- type: \
            github-pr\n  value: jsama/tern#12\nsessions:\n- id: s-live\n  status: active\n- \
            id: s-dormant\n  status: ended\n  children:\n  - id: s-nested\n    status: \
            active\n- id: s-gone\n  status: ended\nparent: QEST-20261101-OSPREY\n";
        let readme = README.replacen("---\n", front, 1);
        store.write("QEST-20261116-KESTRL/README.md", &readme);
        let mut shelf = Shelf::default();
        assert!(shelf.read(&store.0, "QEST-20261116-KESTRL"), "read");
        let quest = shelf.quest("QEST-20261116-KESTRL").expect("read");
        assert_eq!(
            (&quest.claims.changes, &quest.claims.sessions),
            (
                &vec!["jsama/tern#12".to_owned()],
                &vec!["s-live".to_owned()]
            ),
            "a list laid out flush was misread, or a nested entry was taken as a session"
        );
    }

    #[test]
    fn every_key_left_as_yamls_null_says_nothing() {
        let store = Store::new("null-status");
        for (quest, null) in [
            ("QEST-20261116-KESTRL", "null"),
            ("QEST-20261116-SKIFFS", "Null"),
            ("QEST-20261116-OSPREY", "NULL"),
            ("QEST-20261117-TERNSS", "null # top level"),
            ("QEST-20261117-SKIFFS", "# none"),
        ] {
            let readme = README
                .replace("status: active", &format!("status: {null}"))
                .replace("parent: QEST-20261101-OSPREY", &format!("parent: {null}"));
            store.write(&format!("{quest}/README.md"), &readme);
        }
        let mut shelf = Shelf::default();
        for quest in [
            "QEST-20261116-KESTRL",
            "QEST-20261116-SKIFFS",
            "QEST-20261116-OSPREY",
            "QEST-20261117-TERNSS",
            "QEST-20261117-SKIFFS",
        ] {
            shelf.read(&store.0, quest);
            let read = shelf.quest(quest).expect("read");
            assert_eq!((&read.status, &read.parent), (&None, &None), "{quest}");
        }
        for (quest, said) in [
            ("QEST-20261117-KESTRL", "QEST-20261101-OSPREY # split off"),
            (
                "QEST-20261117-OSPREY",
                "\"QEST-20261101-OSPREY\" # split off",
            ),
            ("QEST-20261118-KESTRL", "QEST-20261101-OSPREY\t# split off"),
            ("QEST-20261118-OSPREY", "'QEST-20261101-OSPREY'"),
        ] {
            let commented =
                README.replace("parent: QEST-20261101-OSPREY", &format!("parent: {said}"));
            store.write(&format!("{quest}/README.md"), &commented);
            shelf.read(&store.0, quest);
            assert_eq!(
                shelf.quest(quest).expect("read").parent.as_deref(),
                Some("QEST-20261101-OSPREY"),
                "a comment is no part of {said}"
            );
        }
    }

    /// A waiting file for quest `id`, its waits as `items`.
    fn waiting(id: &str, items: &Value) -> String {
        json!({
            "v": 2,
            "quest": id,
            "items": items,
            "cleared": [{"id": "gone", "title": "Said already"}],
        })
        .to_string()
    }

    #[test]
    fn a_quest_says_what_it_waits_on_its_person_for() {
        let store = Store::new("waits");
        store.write("QEST-20261116-KESTRL/README.md", README);
        let items = json!([
            {"id": "fly", "title": "Say whether it hovers", "why": "It flies.",
             "minutes": 5, "when": "today", "window": "Kestrel", "ready": true,
             "raisedAt": "2026-11-16T18:00:00Z", "steps": ["look"]},
            {"id": "no-title"},
            {"id": "land", "title": "Pick a field"},
            {"title": "Pick a hedge", "why": "It perches."},
            {"id": "", "title": "Pick a post"},
            {"id": "odd", "title": "Odd fields", "why": null, "ready": null,
             "minutes": 2.5, "when": "", "raisedAt": 7},
        ]);
        store.write(
            "waiting/KESTRL.json",
            &waiting("QEST-20261116-KESTRL", &items),
        );
        let mut shelf = Shelf::with_waits(store.0.join("waiting"));
        assert!(shelf.read(&store.0, "QEST-20261116-KESTRL"));
        let waits = &shelf.quest("QEST-20261116-KESTRL").expect("read").waits;
        assert_eq!(
            waits,
            &vec![
                Wait {
                    id: "fly".to_owned(),
                    title: "Say whether it hovers".to_owned(),
                    why: "It flies.".to_owned(),
                    minutes: Some(5),
                    when: Some("today".to_owned()),
                    ready: true,
                    raised: Some("2026-11-16T18:00:00Z".to_owned()),
                },
                Wait {
                    id: "land".to_owned(),
                    title: "Pick a field".to_owned(),
                    why: String::new(),
                    minutes: None,
                    when: None,
                    ready: false,
                    raised: None,
                },
                Wait {
                    id: "Pick a hedge".to_owned(),
                    title: "Pick a hedge".to_owned(),
                    why: "It perches.".to_owned(),
                    minutes: None,
                    when: None,
                    ready: false,
                    raised: None,
                },
                Wait {
                    id: "Pick a post".to_owned(),
                    title: "Pick a post".to_owned(),
                    why: String::new(),
                    minutes: None,
                    when: None,
                    ready: false,
                    raised: None,
                },
                Wait {
                    id: "odd".to_owned(),
                    title: "Odd fields".to_owned(),
                    why: String::new(),
                    minutes: None,
                    when: None,
                    ready: false,
                    raised: None,
                },
            ],
            "a wait with no title is left out, one with no id or an empty one known by its title, \
             one with odd fields kept, and the cleared ones aren't read"
        );
        let (records, _) = records(&mut shelf, &[cell("QEST-20261116-KESTRL")], "kestrel");
        let forage = &records["forage:KESTRL"];
        assert_eq!(forage["waits"][0]["title"], "Say whether it hovers");
        assert_eq!(forage["waits"][0]["raised"], "2026-11-16T18:00:00Z");
        assert_eq!(forage["waits"][1]["ready"], false);
        let mut waitless = Shelf::with_waits(store.0.join("none-here"));
        assert!(waitless.read(&store.0, "QEST-20261116-KESTRL"));
        let (bare, _) = super::records(&mut waitless, &[cell("QEST-20261116-KESTRL")], "kestrel");
        assert!(
            bare["forage:KESTRL"].get("waits").is_none(),
            "a forage waiting on nothing leaves `waits` out"
        );
    }

    #[test]
    fn a_wait_its_person_or_a_verdict_said_is_done_is_gone() {
        let store = Store::new("settled");
        store.write("QEST-20261116-KESTRL/README.md", README);
        let items = json!([
            {"id": "fly", "title": "Say whether it hovers"},
            {"id": "land", "title": "Pick a field"},
            {"title": "Pick a hedge"},
            {"id": "post", "title": "Pick a post"},
            {"id": "nest", "title": "Choose a ledge"},
        ]);
        store.write(
            "waiting/KESTRL.json",
            &waiting("QEST-20261116-KESTRL", &items),
        );
        store.write(
            "settled.json",
            &json!({
                "done": {"QEST-20261116-KESTRL:fly": 1, "QEST-20261116-OSPREY:land": 2},
                "notNow": {"QEST-20261116-KESTRL:land": {"said": "x", "when": 3}},
            })
            .to_string(),
        );
        store.write(
            "verdicts.json",
            &json!({"v": 1, "items": {
                "QEST-20261116-KESTRL:Pick a hedge": {"state": "done", "at": 4},
                "QEST-20261116-KESTRL:post": {"state": "ready", "at": 5},
                "QEST-20261116-KESTRL:nest": {"state": "waiting", "at": 6},
            }})
            .to_string(),
        );
        let mut shelf = Shelf::with_waits(store.0.join("waiting"));
        shelf.read(&store.0, "QEST-20261116-KESTRL");
        let said = |shelf: &mut Shelf| -> Vec<(String, bool)> {
            shelf
                .quest("QEST-20261116-KESTRL")
                .expect("read")
                .waits
                .iter()
                .map(|wait| (wait.id.clone(), wait.ready))
                .collect()
        };
        assert_eq!(
            said(&mut shelf),
            [("land", false), ("post", true), ("nest", false)]
                .map(|(id, ready)| (id.to_owned(), ready)),
            "one its person said is done, or a verdict found done, is gone, by id or by title; \
             one put off, or another quest's done, stays; one a verdict found ready is ready"
        );
        store.write("QEST-20261116-OSPREY/README.md", README);
        shelf.read(&store.0, "QEST-20261116-OSPREY");
        assert!(shelf.settled_moved().is_empty(), "both read as it stands");
        store.write("settled.json", &json!({"done": {}}).to_string());
        assert_eq!(
            shelf.settled_moved(),
            ["QEST-20261116-KESTRL", "QEST-20261116-OSPREY"].map(str::to_owned),
            "a change to what was settled is to every quest held"
        );
        shelf.read(&store.0, "QEST-20261116-OSPREY");
        assert_eq!(
            shelf.settled_moved(),
            ["QEST-20261116-KESTRL".to_owned()],
            "reading one quest reads it for all, so the others are still due"
        );
        shelf.read(&store.0, "QEST-20261116-KESTRL");
        assert!(
            said(&mut shelf).contains(&("fly".to_owned(), false)),
            "one no longer settled is back"
        );
        assert!(shelf.settled_moved().is_empty(), "once read, nothing moved");
        store.write("verdicts.json", &json!({"items": {}}).to_string());
        assert_eq!(
            shelf.settled_moved().len(),
            2,
            "a change to the verdicts alone is heard"
        );
        shelf.read(&store.0, "QEST-20261116-KESTRL");
        assert!(
            said(&mut shelf).contains(&("Pick a hedge".to_owned(), false)),
            "one no longer found done is back"
        );
        store.write(
            "settled.json",
            &json!({"done": {"QEST-20261116-KESTRL:fly": 1}}).to_string(),
        );
        shelf.read(&store.0, "QEST-20261116-KESTRL");
        assert_eq!(said(&mut shelf).len(), 4, "settled again");
        for unread in ["[1, 2]", r#"{"done": ["QEST-20261116-KESTRL:land"]}"#] {
            store.write("settled.json", unread);
            store.write(
                "verdicts.json",
                r#"{"items": {"QEST-20261116-KESTRL:land": "done"}}"#,
            );
            shelf.read(&store.0, "QEST-20261116-KESTRL");
            assert_eq!(said(&mut shelf).len(), 5, "{unread} settles nothing");
        }
    }

    #[test]
    fn a_settled_file_written_halfway_keeps_what_was_last_read() {
        let store = Store::new("settled-halfway");
        store.write("QEST-20261116-KESTRL/README.md", README);
        let items = json!([
            {"id": "fly", "title": "Say whether it hovers"},
            {"id": "land", "title": "Pick a field"},
            {"id": "post", "title": "Pick a post"},
        ]);
        store.write(
            "waiting/KESTRL.json",
            &waiting("QEST-20261116-KESTRL", &items),
        );
        store.write(
            "settled.json",
            &json!({"done": {"QEST-20261116-KESTRL:fly": 1}}).to_string(),
        );
        store.write(
            "verdicts.json",
            &json!({"items": {"QEST-20261116-KESTRL:land": {"state": "done"}}}).to_string(),
        );
        let mut shelf = Shelf::with_waits(store.0.join("waiting"));
        let said = |shelf: &mut Shelf| -> Vec<String> {
            shelf.read(&store.0, "QEST-20261116-KESTRL");
            shelf
                .quest("QEST-20261116-KESTRL")
                .expect("read")
                .waits
                .iter()
                .map(|wait| wait.id.clone())
                .collect()
        };
        assert_eq!(said(&mut shelf), ["post"], "both read");
        store.write("settled.json", r#"{"done": {"QEST-20261116-KESTRL:fl"#);
        assert_eq!(
            said(&mut shelf),
            ["post"],
            "a settled.json written halfway keeps what was last read"
        );
        store.write("verdicts.json", r#"{"items": {"#);
        assert_eq!(
            said(&mut shelf),
            ["post"],
            "and so does a verdicts.json written halfway"
        );
        store.write("settled.json", &json!({"done": {}}).to_string());
        assert_eq!(
            said(&mut shelf),
            ["fly", "post"],
            "the next whole one is read, the other's last read still kept"
        );
        fs::remove_file(store.0.join("verdicts.json")).expect("removed");
        assert_eq!(
            said(&mut shelf),
            ["fly", "land", "post"],
            "one gone settles nothing"
        );
        let fly = json!({"done": {"QEST-20261116-KESTRL:fly": 1}}).to_string();
        store.write("settled.json", &fly);
        assert_eq!(said(&mut shelf), ["land", "post"], "whole again");
        store.write("whole.json", &fly);
        fs::remove_file(store.0.join("settled.json")).expect("removed");
        std::os::unix::fs::symlink(store.0.join("whole.json"), store.0.join("settled.json"))
            .expect("linked");
        assert_eq!(
            said(&mut shelf),
            ["fly", "land", "post"],
            "a link lasts, so it settles nothing rather than keeping what was last read"
        );
        fs::remove_file(store.0.join("settled.json")).expect("removed");
        store.write("settled.json", r#"{"done": {"QEST-20261116-KESTRL:fl"#);
        let mut fresh = Shelf::with_waits(store.0.join("waiting"));
        assert_eq!(
            said(&mut fresh),
            ["fly", "land", "post"],
            "one written halfway on the first look has nothing to keep"
        );
    }

    #[test]
    fn no_waits_are_read_from_another_quests_file_a_link_or_none() {
        let store = Store::new("no-waits");
        store.write("QEST-20261116-KESTRL/README.md", README);
        store.write("QEST-20261116-SKIFFS/README.md", README);
        store.write("QEST-20261116-OSPREY/README.md", README);
        let one = json!([{"id": "fly", "title": "Say whether it hovers"}]);
        store.write(
            "waiting/KESTRL.json",
            &waiting("QEST-20261101-KESTRL", &one),
        );
        store.write("elsewhere.json", &waiting("QEST-20261116-SKIFFS", &one));
        std::os::unix::fs::symlink(
            store.0.join("elsewhere.json"),
            store.0.join("waiting/SKIFFS.json"),
        )
        .expect("a link");
        store.write("waiting/OSPREY.json", "not json");
        store.write("waiting/TERNSS.json", &json!({"items": one}).to_string());
        let long = json!({
            "quest": "QEST-20261117-KESTRL",
            "items": one,
            "pad": "k".repeat(256 * 1024),
        });
        store.write("waiting/KESTRL.json.long", &long.to_string());
        let mut shelf = Shelf::with_waits(store.0.join("waiting"));
        for quest in [
            "QEST-20261116-KESTRL",
            "QEST-20261116-SKIFFS",
            "QEST-20261116-OSPREY",
            "QEST-20261116-TERNSS",
        ] {
            shelf.read(&store.0, quest);
            assert_eq!(
                shelf.quest(quest).expect("read").waits,
                Vec::<Wait>::new(),
                "{quest}"
            );
        }
        let mut plain = Shelf::default();
        store.write(
            "waiting/KESTRL.json",
            &waiting("QEST-20261116-KESTRL", &one),
        );
        plain.read(&store.0, "QEST-20261116-KESTRL");
        assert_eq!(
            plain.quest("QEST-20261116-KESTRL").expect("read").waits,
            Vec::<Wait>::new(),
            "a shelf told no waiting folder reads none"
        );
        fs::rename(
            store.0.join("waiting/KESTRL.json.long"),
            store.0.join("waiting/KESTRL.json"),
        )
        .expect("renamed");
        shelf.read(&store.0, "QEST-20261117-KESTRL");
        assert_eq!(
            shelf.quest("QEST-20261117-KESTRL").expect("read").waits,
            Vec::<Wait>::new(),
            "a file past 256 KB gives none"
        );
    }

    #[test]
    fn a_change_to_a_waiting_file_is_to_the_quests_held_it_names() {
        let store = Store::new("waiting-on");
        store.write("QEST-20261116-KESTRL/README.md", README);
        store.write("QEST-20261116-SKIFFS/README.md", README);
        let mut shelf = Shelf::with_waits(store.0.join("waiting"));
        shelf.read(&store.0, "QEST-20261116-KESTRL");
        shelf.read(&store.0, "QEST-20261116-SKIFFS");
        assert_eq!(
            shelf.waiting_on(&store.0.join("waiting/KESTRL.json")),
            ["QEST-20261116-KESTRL"]
        );
        let none: Vec<String> = Vec::new();
        assert_eq!(shelf.waiting_on(&store.0.join("waiting/OSPREY.json")), none);
        assert_eq!(
            shelf.waiting_on(&store.0.join("waiting/.KESTRL.json.tmp")),
            none
        );
    }

    #[test]
    fn a_quest_says_its_purpose_parent_status_and_documents() {
        let store = Store::new("reads");
        store.write("QEST-20261116-KESTRL/README.md", README);
        store.write("QEST-20261116-KESTRL/plans/PLAN-20261116-HOVERS.md", PLAN);
        store.write("QEST-20261116-KESTRL/plans/notes.txt", "not a document");
        let mut shelf = Shelf::default();
        assert!(shelf.read(&store.0, "QEST-20261116-KESTRL"));
        let quest = shelf.quest("QEST-20261116-KESTRL").expect("read");
        assert_eq!(
            quest.purpose.as_deref(),
            Some("Kestrel learns to hover over one field.")
        );
        assert_eq!(quest.parent.as_deref(), Some("QEST-20261101-OSPREY"));
        assert_eq!(quest.status.as_deref(), Some("active"));
        assert_eq!(
            quest.documents,
            vec![Document {
                id: "PLAN-20261116-HOVERS".to_owned(),
                kind: "plan".to_owned(),
                title: "Hover Over the Field".to_owned(),
                stage: "build".to_owned(),
                ticked: 2,
                open: 2,
            }]
        );
    }

    #[test]
    fn a_cell_has_a_forage_keyed_by_its_address_once_its_quest_is_read() {
        let store = Store::new("records");
        store.write("QEST-20261116-KESTRL/README.md", README);
        let mut shelf = Shelf::default();
        let cells = [cell("QEST-20261116-KESTRL")];
        let (records, unread) = records(&mut shelf, &cells, "kestrel");
        assert_eq!(records, BTreeMap::new());
        assert_eq!(unread, vec!["QEST-20261116-KESTRL".to_owned()]);
        shelf.read(&store.0, "QEST-20261116-KESTRL");
        let (records, unread) = super::records(&mut shelf, &cells, "kestrel");
        assert_eq!(unread, Vec::<String>::new());
        let forage = &records["forage:KESTRL"];
        assert_eq!(forage["quest"], "QEST-20261116-KESTRL");
        assert_eq!(forage["machine"], "kestrel");
        assert_eq!(forage["lineage"]["started_by"], "OSPREY");
        assert_eq!(forage["lineage"]["split_from"], "TERNSS");
        assert_eq!(forage["lineage"]["parent"], "QEST-20261101-OSPREY");
    }

    #[test]
    fn an_unchanged_document_is_not_read_again() {
        let store = Store::new("unchanged");
        store.write("QEST-20261116-KESTRL/README.md", README);
        let plan = "QEST-20261116-KESTRL/plans/PLAN-20261116-HOVERS.md";
        store.write(plan, PLAN);
        let mut shelf = Shelf::default();
        shelf.read(&store.0, "QEST-20261116-KESTRL");
        // What is kept for a file stands for it while its stamp holds,
        // so a kept reading changed by hand is what a second read gives.
        let path = store.0.join(plan);
        if let Some((_, Some(document))) = shelf.files.get_mut(&path) {
            document.title = "as kept".to_owned();
        }
        assert!(shelf.read(&store.0, "QEST-20261116-KESTRL"));
        assert_eq!(
            shelf.quest("QEST-20261116-KESTRL").expect("read").documents[0].title,
            "as kept"
        );
        store.write(plan, &PLAN.replace("- [ ] turn", "- [x] turn\n- [ ] land"));
        shelf.read(&store.0, "QEST-20261116-KESTRL");
        let document = &shelf.quest("QEST-20261116-KESTRL").expect("read").documents[0];
        assert_eq!(
            (document.title.as_str(), document.ticked, document.open),
            ("Hover Over the Field", 3, 2)
        );
    }

    #[test]
    fn a_quest_no_cell_asks_for_is_let_go_at_the_round() {
        let store = Store::new("round");
        store.write("QEST-20261116-KESTRL/README.md", README);
        store.write("QEST-20261116-SKIFFS/README.md", README);
        let mut shelf = Shelf::default();
        shelf.read(&store.0, "QEST-20261116-KESTRL");
        shelf.read(&store.0, "QEST-20261116-SKIFFS");
        shelf.quest("QEST-20261116-KESTRL");
        let kept = shelf.round(&store.0, &BTreeSet::new());
        assert_eq!(kept, BTreeSet::from(["QEST-20261116-KESTRL".to_owned()]));
        assert!(!shelf.holds("QEST-20261116-SKIFFS"));
    }

    #[test]
    fn a_quest_a_living_cell_works_on_is_kept_unasked() {
        let store = Store::new("round-living");
        store.write("QEST-20261116-KESTRL/README.md", README);
        store.write("QEST-20261116-SKIFFS/README.md", README);
        let mut shelf = Shelf::default();
        shelf.read(&store.0, "QEST-20261116-KESTRL");
        shelf.read(&store.0, "QEST-20261116-SKIFFS");
        let living = BTreeSet::from([
            "QEST-20261116-SKIFFS".to_owned(),
            "QEST-20261116-OSPREY".to_owned(),
        ]);
        let kept = shelf.round(&store.0, &living);
        assert_eq!(
            kept,
            BTreeSet::from(["QEST-20261116-SKIFFS".to_owned()]),
            "a living cell's quest is kept, and one never read isn't made up"
        );
        assert!(!shelf.holds("QEST-20261116-KESTRL"));
        shelf.round(&store.0, &living);
        assert!(
            shelf.holds("QEST-20261116-SKIFFS"),
            "and kept round after round while its cell lives"
        );
    }

    #[test]
    fn a_quest_id_that_is_a_path_reads_as_nothing() {
        let store = Store::new("paths");
        store.write("README.md", README);
        let mut shelf = Shelf::default();
        shelf.read(&store.0, "../mycelium-forage-paths");
        assert_eq!(
            shelf.quest("../mycelium-forage-paths"),
            Some(&Quest::default())
        );
        assert!(!is_quest_id("a/b"));
        assert!(!is_quest_id(""));
        assert!(is_quest_id("QEST-20261116-KESTRL"));
    }

    #[test]
    fn a_document_through_a_link_is_not_read() {
        let store = Store::new("links");
        store.write("QEST-20261116-KESTRL/README.md", README);
        store.write("elsewhere.md", PLAN);
        fs::create_dir_all(store.0.join("QEST-20261116-KESTRL/plans")).expect("plans");
        std::os::unix::fs::symlink(
            store.0.join("elsewhere.md"),
            store
                .0
                .join("QEST-20261116-KESTRL/plans/PLAN-20261116-HOVERS.md"),
        )
        .expect("a link");
        let mut shelf = Shelf::default();
        shelf.read(&store.0, "QEST-20261116-KESTRL");
        assert_eq!(
            shelf.quest("QEST-20261116-KESTRL").expect("read").documents,
            Vec::<Document>::new()
        );
    }

    /// The purpose read from Kestrel's README with `instead` written
    /// where it says "Kestrel learns", in a store named `name`.
    fn purpose_with(name: &str, instead: &str) -> Option<String> {
        let store = Store::new(name);
        let readme = README.replace("Kestrel learns", instead);
        store.write("QEST-20261116-KESTRL/README.md", &readme);
        let mut shelf = Shelf::default();
        assert!(shelf.read(&store.0, "QEST-20261116-KESTRL"));
        shelf
            .quest("QEST-20261116-KESTRL")
            .expect("read")
            .purpose
            .clone()
    }

    #[test]
    fn a_summary_left_as_the_templates_guidance_is_no_purpose() {
        let store = Store::new("guidance");
        let guidance = README.replace(
            "Kestrel learns to hover over one field. It then\nlearns a second.",
            "_One paragraph: what this quest is and why it exists._",
        );
        store.write("QEST-20261116-KESTRL/README.md", &guidance);
        let mut shelf = Shelf::default();
        assert!(shelf.read(&store.0, "QEST-20261116-KESTRL"));
        let quest = shelf.quest("QEST-20261116-KESTRL").expect("read");
        assert_eq!(quest.purpose, None);
    }

    #[test]
    fn the_quests_own_words_after_the_templates_guidance_are_its_purpose() {
        assert_eq!(
            purpose_with(
                "after-guidance",
                "*One paragraph: what this quest is.*\n\nKestrel learns"
            )
            .as_deref(),
            Some("Kestrel learns to hover over one field.")
        );
    }

    #[test]
    fn only_one_span_in_italics_is_guidance() {
        assert!(is_guidance("_One paragraph: what this quest is._"));
        assert!(is_guidance("*One paragraph.*"));
        assert!(!is_guidance("_Kestrel_ learns to hover over _one field_"));
        assert!(!is_guidance("__Kestrel learns to hover.__"));
        assert!(!is_guidance("**Kestrel learns to hover.**"));
        assert!(!is_guidance("_"));
        assert!(!is_guidance("Kestrel learns to hover."));
    }

    #[test]
    fn a_purpose_is_its_first_sentence_cut_short() {
        assert_eq!(first_sentence("One thing. Then another."), "One thing.");
        assert_eq!(first_sentence("Version 1.5 ships"), "Version 1.5 ships");
        let long = "word ".repeat(100);
        let purpose = first_sentence(&long);
        assert_eq!(purpose.chars().count(), LONGEST_PURPOSE);
        assert!(purpose.ends_with("..."));
    }

    #[test]
    fn a_quote_in_a_purpose_is_kept_whole() {
        assert_eq!(
            first_sentence(
                "Kestrel's person said at 09:15: \"i love this ... it hovers. start.\" \
                 This quest teaches it to hover. Then to land."
            ),
            "Kestrel's person said at 09:15: \"i love this ... it hovers. start.\""
        );
        assert_eq!(
            first_sentence("They said \u{201c}go. now.\u{201d} Kestrel goes."),
            "They said \u{201c}go. now.\u{201d}"
        );
        assert_eq!(
            first_sentence("Only \"quoted. words.\" here"),
            "Only \"quoted. words.\" here",
            "a quote that ends no sentence leaves it running on"
        );
        assert_eq!(
            first_sentence("An unpaired \" mark. Kestrel hovers."),
            "An unpaired \" mark.",
            "a mark that pairs with none is no quote, so its sentence ends where it would"
        );
        assert_eq!(
            sentences("It said \"go.\" Then it went."),
            ["It said \"go.\"", "Then it went."]
        );
    }

    #[test]
    fn guidance_right_above_the_quests_words_is_not_them() {
        assert_eq!(
            purpose_with(
                "guidance-above",
                "_One paragraph: what this quest is._\nKestrel learns"
            )
            .as_deref(),
            Some("Kestrel learns to hover over one field.")
        );
    }

    #[test]
    fn guidance_over_lines_ending_the_readme_is_no_purpose() {
        let store = Store::new("guidance-last");
        let last = "---\nid: QEST-20261116-KESTRL\n---\n# Kestrel\n\n## \u{1f4dc} Summary\n\n_One paragraph: what\nthis quest is._\n";
        store.write("QEST-20261116-KESTRL/README.md", last);
        let mut shelf = Shelf::default();
        shelf.read(&store.0, "QEST-20261116-KESTRL");
        assert_eq!(
            shelf.quest("QEST-20261116-KESTRL").expect("read").purpose,
            None
        );
    }

    fn forage(cell: &str, machine: &str, quest: &str) -> Value {
        json!({
            "cell": cell,
            "machine": machine,
            "quest": quest,
            "purpose": "Kestrel learns to hover over one field.",
            "documents": [{
                "id": "PLAN-20261116-HOVERS",
                "kind": "plan",
                "title": "Hover Over the Field",
                "stage": "build",
                "ticked": 2,
                "open": 2,
            }],
        })
    }

    #[test]
    fn a_forage_is_chosen_by_its_cell_or_its_quest() {
        let records = vec![
            forage("SKIFFS", "skiff", "QEST-2"),
            forage("KESTRL", "kestrel", "QEST-1"),
            forage("OSPREY", "kestrel", "QEST-3"),
        ];
        let cells = |chosen: Vec<Value>| -> Vec<String> {
            chosen
                .iter()
                .map(|each| each["cell"].as_str().unwrap_or_default().to_owned())
                .collect()
        };
        assert_eq!(
            cells(chosen(records.clone(), None)),
            ["KESTRL", "OSPREY", "SKIFFS"]
        );
        assert_eq!(cells(chosen(records.clone(), Some("kestrl"))), ["KESTRL"]);
        assert_eq!(cells(chosen(records, Some("QEST-2"))), ["SKIFFS"]);
    }

    #[test]
    fn a_forage_is_drawn_as_its_purpose_and_documents() {
        let drawn = draw(&[forage("KESTRL", "kestrel", "QEST-1")], None, 80);
        assert_eq!(
            drawn,
            "KESTRL  on kestrel  QEST-1\n  Kestrel learns to hover over one field.\n  plan  build  2 of 4 ticked     Hover Over the Field\n"
        );
    }

    #[test]
    fn a_forage_fits_a_narrow_terminal_its_purpose_wrapped_and_titles_cut() {
        let drawn = draw(&[forage("KESTRL", "kestrel", "QEST-1")], None, 40);
        assert_eq!(
            drawn,
            "KESTRL  on kestrel  QEST-1\n  Kestrel learns to hover over one\n  field.\n  plan  build  2 of 4 ticked     Hover…\n"
        );
        assert!(
            drawn.lines().all(|line| line.chars().count() <= 40),
            "no line passes the terminal's width"
        );
    }

    #[test]
    fn a_forage_is_drawn_with_what_it_waits_on_its_person_for() {
        let mut waiting = forage("KESTRL", "kestrel", "QEST-1");
        waiting["documents"] = json!([]);
        waiting["waits"] = json!([
            {"title": "Say whether it hovers", "ready": true, "when": "today"},
            {"title": "Pick a field", "ready": true},
            {"title": "Land it", "ready": false, "when": "after it hovers"},
            {"title": "Name it", "ready": false},
            {"title": "Fly\nnow\u{1b}[2J", "when": "to\rday"},
        ]);
        assert_eq!(
            draw(&[waiting.clone()], None, 80),
            "KESTRL  on kestrel  QEST-1\n  Kestrel learns to hover over one field.\n  \
             Waits on you: Say whether it hovers (ready, today)\n  \
             Waits on you: Pick a field (ready)\n  \
             Waits on you: Land it (after it hovers)\n  \
             Waits on you: Name it\n  \
             Waits on you: Fly now [2J (to day)\n  \
             No research, plans or reports yet.\n",
            "a control character read from a file is drawn as a space"
        );
        assert_eq!(
            draw(&[waiting], None, 40),
            "KESTRL  on kestrel  QEST-1\n  Kestrel learns to hover over one\n  field.\n  \
             Waits on you: Say whether it hovers\n    (ready, today)\n  \
             Waits on you: Pick a field (ready)\n  \
             Waits on you: Land it (after it\n    hovers)\n  \
             Waits on you: Name it\n  \
             Waits on you: Fly now [2J (to day)\n  \
             No research, plans or reports yet.\n",
            "a wait too long for the terminal wraps under its indent"
        );
    }

    #[test]
    fn no_forage_is_said_in_words() {
        assert_eq!(
            draw(&[], None, 80),
            "No cell works on a quest, so none has a forage.\n"
        );
        assert_eq!(
            draw(&[], Some("KESTRL"), 80),
            "No cell named KESTRL has a forage.\n"
        );
        assert_eq!(
            draw(&[], None, 30),
            "No cell works on a quest, so\nnone has a forage.\n"
        );
    }

    /// The headings of what `drawn` holds, a cell's each.
    fn headings(drawn: &str) -> Vec<&str> {
        drawn
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with(' '))
            .collect()
    }

    #[test]
    fn the_cells_that_wait_on_their_person_are_drawn_first() {
        // More cells than the few an unstable sort leaves in their order,
        // as one did when tried here, so only a stable sort keeps each
        // half as it came.
        let quest = |at: usize| format!("QEST-{at}");
        let forages: Vec<Value> = (0..30)
            .map(|at| {
                let mut each = forage(&format!("CELL{at:02}"), "kestrel", &quest(at));
                if at % 2 == 0 {
                    each["waits"] = json!([{"title": "Pick a field", "ready": at % 4 == 0}]);
                }
                each
            })
            .collect();
        let expected: Vec<String> = (0..30)
            .filter(|at| at % 2 == 0)
            .chain((0..30).filter(|at| at % 2 == 1))
            .map(|at| format!("CELL{at:02}  on kestrel  {}", quest(at)))
            .collect();
        assert_eq!(headings(&draw(&forages, None, 80)), expected);
    }

    #[test]
    fn a_purpose_takes_two_lines_at_most_and_says_once_that_it_was_cut() {
        let mut long = forage("KESTRL", "kestrel", "QEST-1");
        long["purpose"] = json!(
            "Kestrel learns to hover over one field, then over the next, and the one \
             after that, until every field in the valley is known."
        );
        assert_eq!(
            draw(&[long], None, 40),
            "KESTRL  on kestrel  QEST-1\n  Kestrel learns to hover over one\n  \
             field, then over the next, and the …\n  \
             plan  build  2 of 4 ticked     Hover…\n"
        );
        // Dots its author wrote are theirs, not a cut's.
        for written in ["Kestrel waits for the wind...", "..."] {
            let mut dots = forage("KESTRL", "kestrel", "QEST-1");
            dots["purpose"] = json!(written);
            assert_eq!(
                draw(&[dots], None, 80),
                format!(
                    "KESTRL  on kestrel  QEST-1\n  {written}\n  \
                     plan  build  2 of 4 ticked     Hover Over the Field\n"
                ),
                "a purpose its author ended in three dots was read as cut"
            );
        }
    }

    #[test]
    fn a_purpose_read_cut_at_its_length_ends_after_a_whole_word() {
        // Cut within "over", as a quest's first sentence is cut where it
        // reaches its length.
        let mut cut = forage("KESTRL", "kestrel", "QEST-1");
        cut["purpose"] = json!(first_sentence(&"Kestrel hovers over field ".repeat(12)));
        for (named, words) in [(None, 23), (Some("KESTRL"), 42)] {
            let drawn = draw(&[cut.clone()], named, 80);
            let purpose: Vec<&str> = drawn
                .lines()
                .skip(1)
                .take_while(|line| !line.starts_with("  plan"))
                .flat_map(str::split_whitespace)
                .collect();
            assert_eq!(
                purpose.last(),
                Some(&"\u{2026}"),
                "no mark ends it:\n{drawn}"
            );
            assert!(
                purpose[..purpose.len() - 1]
                    .iter()
                    .all(|word| ["Kestrel", "hovers", "over", "field"].contains(word)),
                "a word was drawn part of itself:\n{drawn}"
            );
            assert_eq!(
                purpose.len() - 1,
                words,
                "{named:?} drew too few words:\n{drawn}"
            );
        }
    }

    /// The words of `cell`'s purpose drawn with it `named` in `width`
    /// columns, the mark among them.
    fn purpose_words(cell: &Value, named: Option<&str>, width: usize) -> Vec<String> {
        draw(std::slice::from_ref(cell), named, width)
            .lines()
            .skip(1)
            .take_while(|line| !line.starts_with("  plan"))
            .flat_map(str::split_whitespace)
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_purpose_is_read_as_cut_only_near_its_length() {
        // As short as a cut one can be once its spaces are trimmed, and
        // one shorter, whose dots are its author's.
        for (kept, cut) in [
            (LONGEST_PURPOSE - 3 - CUT_TRIMS, true),
            (LONGEST_PURPOSE - 4 - CUT_TRIMS, false),
        ] {
            let mut cell = forage("KESTRL", "kestrel", "QEST-1");
            cell["purpose"] = json!(format!("{} hovers...", "K".repeat(kept - 7)));
            let words = purpose_words(&cell, Some("KESTRL"), 400);
            assert_eq!(
                words.last().map(String::as_str),
                Some(if cut { "\u{2026}" } else { "hovers..." }),
                "{kept} characters before the dots read cut: {cut}, not as {words:?}"
            );
        }
    }

    #[test]
    fn a_purpose_is_read_cut_at_the_length_its_node_counted() {
        // The node counted the marks one_line drops, so a purpose they
        // bring to its length was cut there.
        let mut cell = forage("KESTRL", "kestrel", "QEST-1");
        cell["purpose"] = json!(format!(
            "\u{200f}\u{200f}\u{200f}{} hovers...",
            "K".repeat(LONGEST_PURPOSE - 3 - CUT_TRIMS - 10)
        ));
        let words = purpose_words(&cell, Some("KESTRL"), 400);
        assert_eq!(
            words.last().map(String::as_str),
            Some("\u{2026}"),
            "{words:?}"
        );
    }

    #[test]
    fn a_purpose_cut_on_a_space_keeps_its_last_word() {
        // Its 277th character is a space, so the word before it is whole.
        let mut cell = forage("KESTRL", "kestrel", "QEST-1");
        cell["purpose"] = json!(first_sentence(&format!("abc {}", "abcdef ".repeat(45))));
        let words = purpose_words(&cell, Some("KESTRL"), 80);
        assert_eq!(words.len(), 41, "a whole word was lost: {words:?}");
        assert_eq!(words[39], "abcdef");
    }

    #[test]
    fn a_named_cell_keeps_every_word_read_when_its_last_line_is_full() {
        // At 12 columns each line after the first holds two words and no
        // room for the mark, so the mark takes a line of its own.
        let mut cell = forage("KESTRL", "kestrel", "QEST-1");
        cell["purpose"] = json!(first_sentence(&format!("a {}", "abcd ".repeat(60))));
        let words = purpose_words(&cell, Some("KESTRL"), 12);
        assert_eq!(words.len(), 57, "a word was lost for the mark: {words:?}");
        assert_eq!(words.last().map(String::as_str), Some("\u{2026}"));
    }

    #[test]
    fn a_purpose_of_one_long_word_is_cut_within_it() {
        // Whether it was read whole or already cut within the word.
        for word in ["K".repeat(100), first_sentence(&"K".repeat(300))] {
            let mut long = forage("KESTRL", "kestrel", "QEST-1");
            long["purpose"] = json!(word);
            let drawn = draw(&[long], None, 40);
            let purpose: Vec<&str> = drawn.lines().skip(1).take(2).collect();
            assert!(
                purpose[0] == format!("  {}", "K".repeat(38))
                    && purpose[1] == format!("  {} …", "K".repeat(36)),
                "the word wasn't cut to make room for its mark:\n{drawn}"
            );
        }
    }

    #[test]
    fn a_cell_named_says_its_whole_purpose() {
        let mut long = forage("KESTRL", "kestrel", "QEST-1");
        long["purpose"] = json!(
            "Kestrel learns to hover over one field, then over the next, and the one \
             after that, until every field in the valley is known."
        );
        assert_eq!(
            draw(&[long], Some("KESTRL"), 40),
            "KESTRL  on kestrel  QEST-1\n  Kestrel learns to hover over one\n  \
             field, then over the next, and the one\n  \
             after that, until every field in the\n  valley is known.\n  \
             plan  build  2 of 4 ticked     Hover…\n"
        );
    }

    #[test]
    fn closed_documents_are_counted_unless_a_cell_is_named() {
        let mut kept = forage("KESTRL", "kestrel", "QEST-1");
        kept["documents"] = json!([
            {"kind": "plan", "title": "Hover Over the Field", "stage": "build",
             "ticked": 2, "open": 2},
            {"kind": "plan", "title": "Learn the Wind", "stage": "concluded",
             "ticked": 3, "open": 0},
            {"kind": "research", "title": "Which Field", "stage": "retired",
             "ticked": 0, "open": 0},
        ]);
        assert_eq!(
            draw(&[kept.clone()], None, 80),
            "KESTRL  on kestrel  QEST-1\n  Kestrel learns to hover over one field.\n  \
             plan  build  2 of 4 ticked     Hover Over the Field\n  \
             and 2 closed, which mycelium forage KESTRL lists\n"
        );
        assert_eq!(
            draw(&[kept.clone()], Some("KESTRL"), 80),
            "KESTRL  on kestrel  QEST-1\n  Kestrel learns to hover over one field.\n  \
             plan      build      2 of 4 ticked     Hover Over the Field\n  \
             plan      concluded  3 of 3 ticked     Learn the Wind\n  \
             research  retired    no steps          Which Field\n",
            "a cell named alone shows every document"
        );
        kept["documents"].as_array_mut().unwrap().remove(0);
        assert_eq!(
            draw(&[kept.clone()], None, 80),
            "KESTRL  on kestrel  QEST-1\n  Kestrel learns to hover over one field.\n  \
             2 closed, none open, which mycelium forage KESTRL lists\n"
        );
        // A cell's address isn't checked, so one that isn't an address
        // is never offered as a command to run.
        // Nor is one past an address's length, one with no name, or one
        // whose marks would be dropped from the command offered, which
        // then names another cell.
        let long = "K".repeat(ADDRESS_MOST + 1);
        for cell in ["K; rm -rf ~", &long, "", "KES\u{200f}TRL"] {
            let mut odd = forage(cell, "kestrel", "QEST-1");
            odd["documents"] = kept["documents"].clone();
            assert!(
                draw(&[odd], None, 200)
                    .ends_with("  2 closed, none open, which its cell's forage lists\n"),
                "{cell:?} isn't an address but was offered as a command"
            );
        }
    }

    #[test]
    fn a_change_is_to_the_quest_whose_folder_holds_it() {
        let store = Path::new("/quests");
        assert_eq!(
            quest_of(store, Path::new("/quests/QEST-1/plans/PLAN-1.md")).as_deref(),
            Some("QEST-1")
        );
        assert_eq!(quest_of(store, Path::new("/elsewhere/QEST-1")), None);
    }

    #[test]
    fn a_quest_is_read_on_a_shelf_of_its_own_then_taken_back() {
        let store = Store::new("lent");
        let plan = "QEST-20261116-KESTRL/plans/PLAN-20261116-HOVERS.md";
        store.write("QEST-20261116-KESTRL/README.md", README);
        store.write(plan, PLAN);
        let mut shelf = Shelf::default();
        shelf.read(&store.0, "QEST-20261116-KESTRL");
        let ticked = |shelf: &mut Shelf| {
            shelf
                .quest("QEST-20261116-KESTRL")
                .map(|quest| quest.documents[0].ticked)
        };
        store.write(plan, &PLAN.replace("- [ ] turn", "- [x] turn"));
        let due = BTreeSet::from(["QEST-20261116-KESTRL".to_owned()]);
        let mut lent = shelf.lend(&store.0, &due, &[store.0.join(plan)]);
        assert!(lent.read(&store.0, "QEST-20261116-KESTRL"));
        assert_eq!(ticked(&mut shelf), Some(2), "the shelf lent is unchanged");
        shelf.take_back(&store.0, lent);
        assert_eq!(ticked(&mut shelf), Some(3));
        let mut again = shelf.lend(&store.0, &due, &[]);
        assert!(
            !again.read(&store.0, "QEST-20261116-KESTRL"),
            "what was read is lent again, so nothing reads as changed"
        );
    }

    #[tokio::test]
    async fn only_cells_that_moved_are_told() {
        let store = Store::new("beats");
        let held = Held::default();
        let (_want, wanted) = mpsc::unbounded_channel();
        let (beat, sessions) = watch::channel(0);
        let tell = watch::Sender::new(0);
        let mut told = tell.subscribe();
        let kestrel = Cell {
            address: "KESTRL".to_owned(),
            session: "0000-kestrel".to_owned(),
            quest: "QEST-20261116-KESTRL".to_owned(),
            started_by: None,
            split_from: None,
        };
        let cells = Arc::new(Mutex::new(vec![kestrel.clone()]));
        let now = Arc::clone(&cells);
        tokio::spawn(keep(
            store.0.clone(),
            held,
            wanted,
            sessions,
            move || now.lock().expect("cells").clone(),
            tell,
        ));
        let beats = |count: usize| {
            for _ in 0..count {
                beat.send_modify(|beats| *beats += 1);
            }
        };
        beats(20);
        let within = Duration::from_secs(30);
        tokio::time::timeout(within, told.wait_for(|told| *told >= 1))
            .await
            .expect("told in time")
            .expect("told");
        for _ in 0..3 {
            beats(1);
            // A beat that moved no cell tells nothing, so this one waits
            // out its bound.
            let _ = tokio::time::timeout(SETTLED_AFTER * 2, told.changed()).await;
        }
        cells.lock().expect("cells").push(Cell {
            address: "OSPREY".to_owned(),
            ..kestrel
        });
        beats(1);
        tokio::time::timeout(within, told.wait_for(|told| *told >= 2))
            .await
            .expect("told in time")
            .expect("told");
        assert_eq!(
            *told.borrow(),
            2,
            "told once for the first cells, once as one moved"
        );
    }

    /// Keeps `store`'s quests in `held` as the node does, with no cells,
    /// and asks for Kestrel's. Answers what to hold for as long as the
    /// keeping runs, and the watch told of each change.
    fn keeping_kestrel(
        store: &Store,
        held: &Held,
    ) -> (
        mpsc::UnboundedSender<String>,
        watch::Sender<u64>,
        watch::Receiver<u64>,
    ) {
        let (want, wanted) = mpsc::unbounded_channel();
        let (sessions_sent, sessions) = watch::channel(0);
        let tell = watch::Sender::new(0);
        let told = tell.subscribe();
        tokio::spawn(keep(
            store.0.clone(),
            Arc::clone(held),
            wanted,
            sessions,
            Vec::new,
            tell,
        ));
        want.send("QEST-20261116-KESTRL".to_owned()).expect("sent");
        (want, sessions_sent, told)
    }

    #[tokio::test]
    async fn a_waiting_folder_made_late_or_made_anew_is_heard() {
        let store = Store::new("waits-kept");
        store.write("QEST-20261116-KESTRL/README.md", README);
        // Apart from the quests, as the node's own is, so only the
        // folder's own watch can hear it.
        let mission = Store::new("waits-kept-mission");
        let folder = mission.0.join("waiting");
        let held: Held = Arc::new(Mutex::new(Shelf::with_waits(folder.clone())));
        let (_want, _sessions, mut told) = keeping_kestrel(&store, &held);
        let titles = |held: &Held| -> Vec<String> {
            held.lock()
                .expect("held")
                .quest("QEST-20261116-KESTRL")
                .map(|quest| quest.waits.iter().map(|wait| wait.title.clone()).collect())
                .unwrap_or_default()
        };
        let until = async |told: &mut watch::Receiver<u64>, want: &[&str]| {
            let by = tokio::time::Instant::now() + Duration::from_secs(30);
            while titles(&held) != want {
                tokio::time::timeout_at(by, told.changed())
                    .await
                    .unwrap_or_else(|_| panic!("heard {want:?} in time"))
                    .expect("told");
            }
        };
        // The quest is read, with no folder yet, before the folder is made.
        tokio::time::timeout(Duration::from_secs(30), told.changed())
            .await
            .expect("told in time")
            .expect("told");
        assert!(!folder.exists(), "the folder is made after keep reads");
        let one = json!([{"id": "fly", "title": "Say whether it hovers"}]);
        mission.write(
            "waiting/KESTRL.json",
            &waiting("QEST-20261116-KESTRL", &one),
        );
        until(&mut told, &["Say whether it hovers"]).await;
        fs::remove_dir_all(&folder).expect("removed");
        until(&mut told, &[]).await;
        let two = json!([{"id": "land", "title": "Pick a field"}]);
        mission.write(
            "waiting/KESTRL.json",
            &waiting("QEST-20261116-KESTRL", &two),
        );
        until(&mut told, &["Pick a field"]).await;
        // Removed and made anew within one moment, perhaps on the same
        // inode, so only the folder's own change says to listen afresh.
        fs::remove_dir_all(&folder).expect("removed");
        let three = json!([{"id": "nest", "title": "Choose a ledge"}]);
        mission.write(
            "waiting/KESTRL.json",
            &waiting("QEST-20261116-KESTRL", &three),
        );
        until(&mut told, &["Choose a ledge"]).await;
        let four = json!([{"id": "perch", "title": "Watch the river"}]);
        mission.write(
            "waiting/KESTRL.json",
            &waiting("QEST-20261116-KESTRL", &four),
        );
        until(&mut told, &["Watch the river"]).await;
    }

    #[tokio::test]
    async fn a_wait_said_done_is_gone_within_moments() {
        let store = Store::new("settled-kept");
        store.write("QEST-20261116-KESTRL/README.md", README);
        let mission = Store::new("settled-kept-mission");
        let two = json!([
            {"id": "fly", "title": "Say whether it hovers"},
            {"id": "land", "title": "Pick a field"},
        ]);
        mission.write(
            "waiting/KESTRL.json",
            &waiting("QEST-20261116-KESTRL", &two),
        );
        let held: Held = Arc::new(Mutex::new(Shelf::with_waits(mission.0.join("waiting"))));
        let (_want, _sessions, mut told) = keeping_kestrel(&store, &held);
        let count = |held: &Held| {
            held.lock()
                .expect("held")
                .quest("QEST-20261116-KESTRL")
                .map_or(0, |quest| quest.waits.len())
        };
        let by = tokio::time::Instant::now() + Duration::from_secs(30);
        while count(&held) != 2 {
            tokio::time::timeout_at(by, told.changed())
                .await
                .expect("read in time")
                .expect("told");
        }
        mission.write(
            "settled.json",
            &json!({"done": {"QEST-20261116-KESTRL:fly": 1}}).to_string(),
        );
        // Within two harks, though no watch hears the file.
        let by = tokio::time::Instant::now() + HARK * 2 + Duration::from_secs(5);
        while count(&held) != 1 {
            tokio::time::timeout_at(by, told.changed())
                .await
                .expect("settled in time")
                .expect("told");
        }
    }

    /// A watcher that hears nothing, as one past the system's limit.
    struct Deaf;

    impl notify::Watcher for Deaf {
        fn new<F: notify::EventHandler>(_: F, _: notify::Config) -> notify::Result<Self> {
            Ok(Self)
        }

        fn watch(&mut self, _: &Path, _: notify::RecursiveMode) -> notify::Result<()> {
            Err(notify::Error::generic("past the limit"))
        }

        fn unwatch(&mut self, _: &Path) -> notify::Result<()> {
            Ok(())
        }

        fn kind() -> notify::WatcherKind {
            notify::WatcherKind::NullWatcher
        }
    }

    #[test]
    fn a_waiting_folder_no_watch_hears_is_read_once_a_round() {
        let mission = Store::new("waits-deaf");
        mission.write("waiting/KESTRL.json", "{}");
        let folder = mission.0.join("waiting");
        let held: Held = Arc::new(Mutex::new(Shelf::with_waits(folder.clone())));
        let mut waiting = WaitingFolder::of(&held);
        let file = vec![folder.join("KESTRL.json")];
        assert_eq!(waiting.listen(&mut Deaf), file, "read when first found");
        assert_eq!(
            waiting.listen(&mut Deaf),
            Vec::<PathBuf>::new(),
            "not read again each turn, which would never wait"
        );
        waiting.round();
        assert_eq!(waiting.listen(&mut Deaf), file, "read again each round");
        waiting.hark();
        assert_eq!(waiting.listen(&mut Deaf), file, "read again each hark");
        fs::remove_file(&file[0]).expect("removed");
        waiting.hark();
        assert_eq!(
            waiting.listen(&mut Deaf),
            file,
            "a file gone from a folder no watch hears is said"
        );
        waiting.hark();
        assert_eq!(waiting.listen(&mut Deaf), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_waiting_folder_read_unheard_and_removed_is_said_gone() {
        let mission = Store::new("waits-deaf-gone");
        mission.write("waiting/KESTRL.json", "{}");
        let folder = mission.0.join("waiting");
        let held: Held = Arc::new(Mutex::new(Shelf::with_waits(folder.clone())));
        let mut waiting = WaitingFolder::of(&held);
        let file = vec![folder.join("KESTRL.json")];
        assert_eq!(waiting.listen(&mut Deaf), file);
        waiting.hark();
        fs::remove_dir_all(&folder).expect("removed");
        assert_eq!(waiting.listen(&mut Deaf), file, "its files said gone");
    }

    /// A watcher that takes every watch and tells nothing, keeping what it
    /// was asked, as one that misses a change, as a Mac's was seen to.
    #[derive(Default)]
    struct Asked(Vec<String>);

    impl notify::Watcher for Asked {
        fn new<F: notify::EventHandler>(_: F, _: notify::Config) -> notify::Result<Self> {
            Ok(Self::default())
        }

        fn watch(&mut self, path: &Path, _: notify::RecursiveMode) -> notify::Result<()> {
            self.0.push(format!("watch {}", path.display()));
            Ok(())
        }

        fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
            self.0.push(format!("unwatch {}", path.display()));
            Ok(())
        }

        fn kind() -> notify::WatcherKind {
            notify::WatcherKind::NullWatcher
        }
    }

    #[test]
    fn a_hark_leaves_a_missing_folders_parent_heard() {
        let mission = Store::new("waits-parent");
        let folder = mission.0.join("waiting");
        let held: Held = Arc::new(Mutex::new(Shelf::with_waits(folder.clone())));
        let mut waiting = WaitingFolder::of(&held);
        let mut asked = Asked::default();
        waiting.listen(&mut asked);
        assert_eq!(asked.0, vec![format!("watch {}", mission.0.display())]);
        waiting.hark();
        waiting.listen(&mut asked);
        assert_eq!(asked.0.len(), 1, "the parent's watch kept: {:?}", asked.0);
    }

    #[test]
    fn a_waiting_folder_replaced_unheard_says_the_files_it_lost() {
        let mission = Store::new("waits-replaced");
        mission.write("waiting/KESTRL.json", "{}");
        let folder = mission.0.join("waiting");
        let held: Held = Arc::new(Mutex::new(Shelf::with_waits(folder.clone())));
        let mut waiting = WaitingFolder::of(&held);
        assert_eq!(waiting.listen(&mut Mute), vec![folder.join("KESTRL.json")]);
        // Held open, the old folder's inode can't be the new one's.
        let old = mission.0.join("old");
        fs::rename(&folder, &old).expect("moved");
        mission.write("waiting/OSPREY.json", "{}");
        assert_eq!(
            waiting.listen(&mut Mute),
            vec![folder.join("KESTRL.json"), folder.join("OSPREY.json")],
            "the new file and the one gone"
        );
    }

    #[test]
    fn an_event_at_the_waiting_folder_reads_it_again() {
        let mission = Store::new("waits-said");
        mission.write("waiting/KESTRL.json", "{}");
        let folder = mission.0.join("waiting");
        let held: Held = Arc::new(Mutex::new(Shelf::with_waits(folder.clone())));
        let mut waiting = WaitingFolder::of(&held);
        let file = vec![folder.join("KESTRL.json")];
        assert_eq!(waiting.listen(&mut Mute), file);
        assert_eq!(waiting.listen(&mut Mute), Vec::<PathBuf>::new());
        waiting.heard_of(std::slice::from_ref(&folder));
        assert_eq!(
            waiting.listen(&mut Mute),
            file,
            "read again, not every quest"
        );
    }

    /// A watcher that takes every watch and tells nothing, as one that
    /// misses a change, as a Mac's was seen to miss a folder made late.
    struct Mute;

    impl notify::Watcher for Mute {
        fn new<F: notify::EventHandler>(_: F, _: notify::Config) -> notify::Result<Self> {
            Ok(Self)
        }

        fn watch(&mut self, _: &Path, _: notify::RecursiveMode) -> notify::Result<()> {
            Ok(())
        }

        fn unwatch(&mut self, _: &Path) -> notify::Result<()> {
            Ok(())
        }

        fn kind() -> notify::WatcherKind {
            notify::WatcherKind::NullWatcher
        }
    }

    #[test]
    fn a_missing_waiting_folder_is_looked_for_though_its_parent_is_heard() {
        let mission = Store::new("waits-missed");
        let folder = mission.0.join("waiting");
        let held: Held = Arc::new(Mutex::new(Shelf::with_waits(folder.clone())));
        let mut waiting = WaitingFolder::of(&held);
        assert_eq!(waiting.listen(&mut Mute), Vec::<PathBuf>::new());
        mission.write("waiting/KESTRL.json", "{}");
        assert_eq!(
            waiting.listen(&mut Mute),
            vec![folder.join("KESTRL.json")],
            "read once it's found"
        );
    }

    #[test]
    fn a_waiting_folder_removed_unheard_has_its_quests_read_again() {
        let store = Store::new("waits-gone-quests");
        store.write("QEST-20261116-KESTRL/README.md", README);
        let mission = Store::new("waits-gone");
        let folder = mission.0.join("waiting");
        let perch = json!([{"id": "perch", "title": "Watch the river"}]);
        mission.write(
            "waiting/KESTRL.json",
            &waiting("QEST-20261116-KESTRL", &perch),
        );
        let held: Held = Arc::new(Mutex::new(Shelf::with_waits(folder.clone())));
        held.lock()
            .expect("held")
            .read(&store.0, "QEST-20261116-KESTRL");
        assert_eq!(
            held.lock()
                .expect("held")
                .quest("QEST-20261116-KESTRL")
                .map(|quest| quest.waits.len()),
            Some(1),
            "its wait read"
        );
        let mut waiting = WaitingFolder::of(&held);
        assert_eq!(waiting.listen(&mut Mute), vec![folder.join("KESTRL.json")]);
        fs::remove_dir_all(&folder).expect("removed");
        let touched = waiting.listen(&mut Mute);
        assert_eq!(
            touched,
            vec![folder.join("KESTRL.json")],
            "its file said gone"
        );
        let due = waiting.changed(&held, &touched);
        assert_eq!(due, vec!["QEST-20261116-KESTRL".to_owned()]);
        let mut holding = held.lock().expect("held");
        holding.read(&store.0, "QEST-20261116-KESTRL");
        assert_eq!(
            holding
                .quest("QEST-20261116-KESTRL")
                .map(|quest| quest.waits.len()),
            Some(0),
            "its waits are gone with their file"
        );
    }

    #[test]
    fn a_file_is_read_up_to_its_bound_and_a_pipe_never_waited_on() {
        let mission = Store::new("bounded");
        mission.write("eight", "12345678");
        let eight = mission.0.join("eight");
        assert_eq!(read_at_most(&eight, 8).as_deref(), Some(&b"12345678"[..]));
        assert_eq!(read_at_most(&eight, 7), None, "one byte past the bound");
        let pipe = mission.0.join("pipe");
        let made = std::process::Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "a pipe");
        // With no writer, opening a pipe that waits would never return.
        assert_eq!(read_at_most(&pipe, 8), None);
    }

    #[test]
    fn a_mark_turning_text_backwards_is_left_out() {
        assert_eq!(one_line("fly\u{202E}nwod\u{2069} now"), "flynwod now");
    }

    #[tokio::test]
    async fn a_document_written_is_told_and_read() {
        let store = Store::new("kept");
        store.write("QEST-20261116-KESTRL/README.md", README);
        store.write("QEST-20261116-KESTRL/plans/PLAN-20261116-HOVERS.md", PLAN);
        let held = Held::default();
        let (_want, _sessions, mut told) = keeping_kestrel(&store, &held);
        tokio::time::timeout(Duration::from_secs(30), told.changed())
            .await
            .expect("told in time")
            .expect("told");
        let ticked = |held: &Held| {
            held.lock()
                .expect("held")
                .quest("QEST-20261116-KESTRL")
                .map(|quest| quest.documents[0].ticked)
        };
        assert_eq!(ticked(&held), Some(2));
        store.write(
            "QEST-20261116-KESTRL/plans/PLAN-20261116-HOVERS.md",
            &PLAN.replace("- [ ] turn", "- [x] turn"),
        );
        tokio::time::timeout(Duration::from_secs(30), told.changed())
            .await
            .expect("told in time")
            .expect("told");
        assert_eq!(ticked(&held), Some(3));
    }
}
