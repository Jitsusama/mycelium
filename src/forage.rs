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
//! its own in the waiting folder, named by the last part of its id. A
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

/// How often every quest held is looked at again, for a change a watch
/// missed, and the quests no cell works on any more let go.
const ROUND: Duration = Duration::from_secs(300);
/// How often a waiting folder no watch hears is read again, so a wait
/// reaches its person within moments even on a machine past its watches.
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
}

/// One thing a quest waits on its person for, as the launcher keeps it in
/// the quest's waiting file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Wait {
    /// Its id, which the launcher clears it by.
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
    /// The wait `item` says, or none without an id and a title. Any other
    /// field missing, null or written wrong says nothing, so the wait
    /// still shows.
    fn of(item: &Value) -> Option<Self> {
        let said = |field: &str| {
            item[field]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        Some(Self {
            id: said("id")?,
            title: said("title")?,
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
}

/// A file as last read: its length and when it last changed.
type Stamp = (u64, Option<SystemTime>);

/// What has been read of the quest store.
#[derive(Debug, Default)]
pub struct Shelf {
    /// The folder of the quests' waiting files, if it reads them.
    waiting: Option<PathBuf>,
    /// Each quest a cell works on, as last read.
    quests: BTreeMap<String, Quest>,
    /// Each document file as last read, so an unchanged one isn't read
    /// again.
    files: HashMap<PathBuf, (Stamp, Option<Document>)>,
    /// The quests asked for since the last round.
    asked: BTreeSet<String>,
}

impl Shelf {
    /// A shelf that reads each quest's waits from its file in `waiting`.
    #[must_use]
    pub fn with_waits(waiting: PathBuf) -> Self {
        Self {
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
            quest.waits = read_waits(waiting, id);
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
fn read_at_most(path: &Path, most: u64) -> Option<Vec<u8>> {
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
    /// Whether the folder was read with no watch hearing it, so is read
    /// again each round rather than each turn, which would never wait.
    deaf: bool,
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
            deaf: false,
            awaited: false,
        }
    }

    /// Listens afresh on the next turn wherever no watch may hear: a
    /// folder read unheard is read again, and its parent heard again, as
    /// a watch on it may have died with no word.
    fn round(&mut self) {
        if self.deaf {
            self.heard = None;
            self.deaf = false;
        }
        if self.heard.is_none() {
            self.awaited = false;
        }
    }

    /// Whether no watch hears the folder, nor its parent while it's
    /// missing, so it is read again every hark rather than every round.
    fn unheard(&self) -> bool {
        self.folder.is_some() && (self.deaf || (self.heard.is_none() && !self.awaited))
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

    /// Forgets the watch on the folder when `touched` holds the folder
    /// itself, as when it is removed or made: a watch dies with its folder,
    /// and one made anew may take the same inode, so only a change heard
    /// says to listen afresh.
    fn heard_of(&mut self, touched: &[PathBuf]) {
        let Some(folder) = &self.folder else {
            return;
        };
        let real = self.heard.as_ref().map(|(real, _)| real);
        if touched
            .iter()
            .any(|path| path == folder || real.is_some_and(|real| path == real))
        {
            self.heard = None;
        }
    }

    /// Has `watcher` hear the folder as it stands: the folder while it's
    /// there, its parent while it isn't, and a folder made anew afresh.
    /// Says the files in a folder heard anew, which may have been written
    /// before any watch heard them.
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
            (Some((_, was)), Some(now)) if *was == now => Vec::new(),
            (_, Some(now)) => {
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
                fs::read_dir(folder)
                    .map(|entries| {
                        entries
                            .filter_map(Result::ok)
                            .map(|entry| folder.join(entry.file_name()))
                            .collect()
                    })
                    .unwrap_or_default()
            }
            (_, None) => {
                self.heard = None;
                self.deaf = false;
                if !self.awaited
                    && let Some(parent) = parent
                {
                    self.awaited = watcher
                        .watch(parent, notify::RecursiveMode::NonRecursive)
                        .is_ok();
                }
                Vec::new()
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
            _ = harks.tick(), if waiting.unheard() => {
                // A folder no watch hears, as on a machine past its
                // watches, is read again on the next turn.
                waiting.round();
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
const fn turns_direction(each: char) -> bool {
    matches!(each, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// `forages` drawn for a person in `width` columns: a block each, its
/// cell, machine and quest, its purpose wrapped under its indent, a line
/// for each thing it waits on them for, then a line for each document,
/// its title cut where the row would pass `width`. Says so in words when
/// there are none, or none `named` names.
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
    let blocks: Vec<String> = forages
        .iter()
        .map(|forage| {
            let heading = format!(
                "{}  on {}  {}",
                text(&forage["cell"]),
                text(&forage["machine"]),
                text(&forage["quest"])
            );
            let mut block = fit(&heading, width);
            block.push('\n');
            let purpose = one_line(
                forage["purpose"]
                    .as_str()
                    .unwrap_or("Its quest says no purpose yet."),
            );
            block.push_str(&wrapped(&purpose, "  ", "  ", width));
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
            let kinds = documents
                .iter()
                .map(|each| text(&each["kind"]).len())
                .max()
                .unwrap_or(0);
            let stages = documents
                .iter()
                .map(|each| text(&each["stage"]).len())
                .max()
                .unwrap_or(0);
            for document in documents {
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
                let _ = writeln!(block, "{}", fit(&row, width));
            }
            if documents.is_empty() {
                block.push_str(&wrapped(
                    "No research, plans or reports yet.",
                    "  ",
                    "  ",
                    width,
                ));
            }
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
                    id: "odd".to_owned(),
                    title: "Odd fields".to_owned(),
                    why: String::new(),
                    minutes: None,
                    when: None,
                    ready: false,
                    raised: None,
                },
            ],
            "a wait with no title is left out, one with odd fields kept, and the \
             cleared ones aren't read"
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
        let store = Store::new("after-guidance");
        let guidance = README.replace(
            "Kestrel learns",
            "*One paragraph: what this quest is.*\n\nKestrel learns",
        );
        store.write("QEST-20261116-KESTRL/README.md", &guidance);
        let mut shelf = Shelf::default();
        assert!(shelf.read(&store.0, "QEST-20261116-KESTRL"));
        assert_eq!(
            shelf
                .quest("QEST-20261116-KESTRL")
                .expect("read")
                .purpose
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
        let store = Store::new("guidance-above");
        let guidance = README.replace(
            "Kestrel learns",
            "_One paragraph: what this quest is._\nKestrel learns",
        );
        store.write("QEST-20261116-KESTRL/README.md", &guidance);
        let mut shelf = Shelf::default();
        shelf.read(&store.0, "QEST-20261116-KESTRL");
        assert_eq!(
            shelf
                .quest("QEST-20261116-KESTRL")
                .expect("read")
                .purpose
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

    #[tokio::test]
    async fn a_waiting_folder_made_late_or_made_anew_is_heard() {
        let store = Store::new("waits-kept");
        store.write("QEST-20261116-KESTRL/README.md", README);
        // Apart from the quests, as the node's own is, so only the
        // folder's own watch can hear it.
        let mission = Store::new("waits-kept-mission");
        let folder = mission.0.join("waiting");
        let held: Held = Arc::new(Mutex::new(Shelf::with_waits(folder.clone())));
        let (want, wanted) = mpsc::unbounded_channel();
        let (_sessions, sessions) = watch::channel(0);
        let tell = watch::Sender::new(0);
        let mut told = tell.subscribe();
        tokio::spawn(keep(
            store.0.clone(),
            Arc::clone(&held),
            wanted,
            sessions,
            Vec::new,
            tell,
        ));
        want.send("QEST-20261116-KESTRL".to_owned()).expect("sent");
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
        assert!(waiting.unheard(), "a folder no watch hears is harked to");
        fs::remove_dir_all(&folder).expect("removed");
        waiting.listen(&mut Deaf);
        assert!(
            waiting.unheard(),
            "a folder whose parent no watch hears too"
        );
        assert!(
            !WaitingFolder::of(&Held::default()).unheard(),
            "a node reading no waits harks to nothing"
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
        let (want, wanted) = mpsc::unbounded_channel();
        let (_sessions, sessions) = watch::channel(0);
        let tell = watch::Sender::new(0);
        let mut told = tell.subscribe();
        tokio::spawn(keep(
            store.0.clone(),
            Arc::clone(&held),
            wanted,
            sessions,
            Vec::new,
            tell,
        ));
        want.send("QEST-20261116-KESTRL".to_owned()).expect("sent");
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
