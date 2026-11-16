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
//! its parent and status and whose Summary's first sentence is its purpose,
//! and its documents under `plans/`, `research/`, `briefs/` and
//! `reports/`, each with front matter saying its id, kind and stage. A
//! plan can grow past 400 KB, so a document is read only when its length
//! or time changed since it was last read, and a quest only when a watch
//! on its folders hears a change, or on the slow round that catches what
//! a watch missed. What a reader asks is answered from what was read.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::io::{self, BufRead as _, BufReader};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use notify::Watcher as _;
use serde::Serialize;
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

/// How often every quest held is looked at again, for a change a watch
/// missed, and the quests no cell works on any more let go.
const ROUND: Duration = Duration::from_secs(300);

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
    /// Each quest a cell works on, as last read.
    quests: BTreeMap<String, Quest>,
    /// Each document file as last read, so an unchanged one isn't read
    /// again.
    files: HashMap<PathBuf, (Stamp, Option<Document>)>,
    /// The quests asked for since the last round.
    asked: BTreeSet<String>,
}

impl Shelf {
    /// The quest `id` as last read, marking it asked for.
    pub fn quest(&mut self, id: &str) -> Option<&Quest> {
        self.asked.insert(id.to_owned());
        self.quests.get(id)
    }

    /// Reads the quest `id` from `store` again, reading only the
    /// documents that changed. Whether what it holds changed.
    pub fn read(&mut self, store: &Path, id: &str) -> bool {
        let quest = read_quest(store, id, &mut self.files);
        let same = self.quests.get(id) == Some(&quest);
        self.quests.insert(id.to_owned(), quest);
        !same
    }

    /// Lets go of the quests no cell asked for since the last round, and
    /// their documents; the ones it keeps.
    pub fn round(&mut self, store: &Path) -> BTreeSet<String> {
        let asked = std::mem::take(&mut self.asked);
        self.quests.retain(|id, _| asked.contains(id));
        let kept: Vec<PathBuf> = asked.iter().map(|id| store.join(id)).collect();
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
/// null, a bare `null` or `~`, says nothing, as one left empty does; a
/// quoted `"null"` is the word.
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
            if raw == "null" || raw == "~" {
                continue;
            }
            let value = raw.trim_matches(['"', '\'']);
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

/// The first sentence of `paragraph`, at most [`LONGEST_PURPOSE`] characters.
fn first_sentence(paragraph: &str) -> String {
    let ends = paragraph
        .char_indices()
        .find(|&(at, each)| {
            matches!(each, '.' | '!' | '?')
                && paragraph[at + each.len_utf8()..]
                    .chars()
                    .next()
                    .is_none_or(char::is_whitespace)
        })
        .map_or(paragraph.len(), |(at, each)| at + each.len_utf8());
    let sentence = paragraph[..ends].trim();
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
        if !in_summary || line.is_empty() {
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
            let fields = json!({
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
            let key = crate::daemon::key(KIND, &cell.address);
            Some((key, crate::daemon::record(KIND, &cell.address, 0, fields)))
        })
        .collect();
    unread.sort();
    unread.dedup();
    (records, unread)
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
    let mut rounds = tokio::time::interval(ROUND);
    rounds.tick().await;
    loop {
        let mut due: BTreeSet<String> = BTreeSet::new();
        let mut touched: Vec<PathBuf> = Vec::new();
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
            _ = rounds.tick() => {
                let kept = held.lock().unwrap_or_else(PoisonError::into_inner).round(&store);
                let keeping: BTreeSet<PathBuf> = kept.iter().flat_map(|id| folders(&store, id)).collect();
                if let Some(watcher) = watcher.as_mut() {
                    for gone in listened_to.difference(&keeping) {
                        // A folder already gone is no longer watched.
                        let _ = watcher.unwatch(gone);
                    }
                }
                listened_to.retain(|folder| keeping.contains(folder));
                due.extend(kept);
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
            for folder in due.iter().flat_map(|id| folders(&store, id)) {
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

/// `forages` drawn for a person: a block each, its cell, machine and
/// quest, its purpose, then a line for each document. Says so in words
/// when there are none, or none `named` names.
#[must_use]
pub fn draw(forages: &[Value], named: Option<&str>) -> String {
    if forages.is_empty() {
        return match named {
            Some(name) => format!("No cell named {name} has a forage.\n"),
            None => "No cell works on a quest, so none has a forage.\n".to_owned(),
        };
    }
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    let blocks: Vec<String> = forages
        .iter()
        .map(|forage| {
            let mut block = format!(
                "{}  on {}  {}\n",
                text(&forage["cell"]),
                text(&forage["machine"]),
                text(&forage["quest"])
            );
            let purpose = forage["purpose"]
                .as_str()
                .unwrap_or("Its quest says no purpose yet.");
            // Writing to a String never fails.
            let _ = writeln!(block, "  {purpose}");
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
                let _ = writeln!(
                    block,
                    "  {:kinds$}  {:stages$}  {steps:<16}  {}",
                    text(&document["kind"]),
                    text(&document["stage"]),
                    text(&document["title"]),
                );
            }
            if documents.is_empty() {
                block.push_str("  No research, plans or reports yet.\n");
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
        let kept = shelf.round(&store.0);
        assert_eq!(kept, BTreeSet::from(["QEST-20261116-KESTRL".to_owned()]));
        assert!(!shelf.holds("QEST-20261116-SKIFFS"));
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
        let drawn = draw(&[forage("KESTRL", "kestrel", "QEST-1")], None);
        assert_eq!(
            drawn,
            "KESTRL  on kestrel  QEST-1\n  Kestrel learns to hover over one field.\n  plan  build  2 of 4 ticked     Hover Over the Field\n"
        );
    }

    #[test]
    fn no_forage_is_said_in_words() {
        assert_eq!(
            draw(&[], None),
            "No cell works on a quest, so none has a forage.\n"
        );
        assert_eq!(
            draw(&[], Some("KESTRL")),
            "No cell named KESTRL has a forage.\n"
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
