//! What this machine's cells murmured, served as the node's `murmur`
//! kind: each murmur its cells said into a quest's web (`web`) in the
//! last day, newest first, with what it is tied to and how it was settled.
//!
//! Every quest's web on this machine is read through the web's own
//! reading, as `mycelium web` folds it, so the page and the node read one
//! murmur alike. Each node serves only its own machine's murmurs, the
//! lines of its own file in each web, since every other machine's node
//! serves its own; a tie or settling another machine said of one is still
//! its tie or settling. The webs are kept read: a writer's file is opened
//! only once its size or time moved, and then read on from where it
//! ended, so ten thousand cells' murmurs cost a pass only what changed.
//!
//! A cell's purpose is what its session says it works on, as the node
//! holds it when the records are made, never a quest's record; it moves
//! on the wire with the cell's next murmur, or as a murmur ages out.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use crate::daemon;
use crate::web::{self, Reading};

/// The kind of a murmur's records.
pub const KIND: &str = "murmur";

/// How long a murmur is served after it was said, in seconds: a day.
pub const KEPT_FOR: i64 = 24 * 60 * 60;

/// How often the webs are read again though nothing was heard, so a
/// murmur ages out and a change a watch missed is still read.
const AGAIN_EVERY: Duration = Duration::from_secs(60);

/// How long after a change the webs are left before they are read, so a
/// burst of writes is read once.
const SETTLED_AFTER: Duration = Duration::from_millis(20);

/// One murmur of this machine's, as its web says it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Said {
    /// The cell that said it: the session at the machine that did, or
    /// the machine.
    pub cell: String,
    /// The machine whose file holds it.
    pub machine: String,
    /// When, in whole seconds since the epoch.
    pub at: i64,
    /// Its number in its machine's file, which orders two said in one
    /// second.
    pub seq: u64,
    /// Its words.
    pub text: String,
    /// What its ties name at their other ends, each once: a repository,
    /// a tree, a change, as each was said.
    pub tied_to: Vec<String>,
    /// How it was settled, if it was.
    pub settled: Option<String>,
}

/// Every quest's web in a scratch folder, as this machine's murmurs, kept
/// read as each grows.
#[derive(Debug)]
pub struct Webs {
    scratch: PathBuf,
    machine: String,
    quests: BTreeMap<String, Quest>,
    /// The folders whose changes may change the murmurs.
    watched: BTreeSet<PathBuf>,
    /// Why the scratch folder couldn't be read, if it couldn't.
    refused: Option<String>,
}

/// One quest's web as last read.
#[derive(Debug, Default)]
struct Quest {
    reading: Reading,
    said: Arc<[Said]>,
}

/// What the webs said as last read, shared with whoever asks while they
/// are read again.
#[derive(Debug, Default)]
struct Seen {
    /// Each quest's murmurs.
    said: Vec<Arc<[Said]>>,
    /// Why the scratch folder couldn't be read, if it couldn't.
    refused: Option<String>,
}

impl Webs {
    /// The webs of the quests whose scratch folders are in `scratch`, as
    /// the machine `machine` murmured in them, none read yet.
    #[must_use]
    pub fn new(scratch: PathBuf, machine: String) -> Self {
        Self {
            scratch,
            machine,
            quests: BTreeMap::new(),
            watched: BTreeSet::new(),
            refused: None,
        }
    }

    /// Reads every quest's web as it stands now, opening only the files
    /// changed since, and answers whether its murmurs changed. A scratch
    /// folder not there holds none, which is no trouble.
    pub fn read(&mut self) -> bool {
        // A scratch folder not made yet is watched for from the nearest
        // folder above it that is there.
        let nearest = self.scratch.ancestors().find(|folder| folder.is_dir());
        let mut watched: BTreeSet<PathBuf> = nearest.map(Path::to_owned).into_iter().collect();
        let mut seen = BTreeSet::new();
        let mut changed = false;
        let listed = match fs::read_dir(&self.scratch) {
            Ok(listed) => {
                self.refused = None;
                Some(listed)
            }
            Err(error) => {
                self.refused = self
                    .scratch
                    .exists()
                    .then(|| format!("{} can't be read: {error}", self.scratch.display()));
                None
            }
        };
        for entry in listed.into_iter().flatten().flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let quest = entry.path();
            let web = quest.join(web::FOLDER);
            let events = web::events_in(&web);
            // The nearest of its web's folders there, so the first murmur
            // of a web not made yet is heard as it's made.
            let nearest = [&events, &web, &quest]
                .into_iter()
                .find(|folder| folder.is_dir());
            let Some(nearest) = nearest else {
                continue;
            };
            watched.insert(nearest.clone());
            if *nearest != events {
                continue;
            }
            let held = self.quests.entry(name.clone()).or_default();
            seen.insert(name);
            if held.reading.read(&events) {
                held.said = said_by(&held.reading.events(), &self.machine).into();
                changed = true;
            }
        }
        let before = self.quests.len();
        self.quests.retain(|name, _| seen.contains(name));
        self.watched = watched;
        changed || self.quests.len() != before
    }

    /// This machine's murmurs said since `since`, in whole seconds, in
    /// every quest's web.
    pub fn said_since(&self, since: i64) -> impl Iterator<Item = &Said> {
        self.quests
            .values()
            .flat_map(|quest| quest.said.iter())
            .filter(move |said| said.at > since)
    }

    /// What it holds as last read, sharing each quest's murmurs.
    fn seen(&self) -> Seen {
        Seen {
            said: self
                .quests
                .values()
                .map(|quest| Arc::clone(&quest.said))
                .collect(),
            refused: self.refused.clone(),
        }
    }

    /// Whether any murmur ages out between `was` and `now`, as a day
    /// passes over it.
    #[must_use]
    pub fn ages_out(&self, was: i64, now: i64) -> bool {
        self.said_since(was - KEPT_FOR)
            .any(|said| said.at <= now - KEPT_FOR)
    }

    /// The folders whose changes may change its murmurs, as it last read.
    #[must_use]
    pub fn watched(&self) -> &BTreeSet<PathBuf> {
        &self.watched
    }
}

/// `machine`'s murmurs among `events`, as its web weaves them.
fn said_by(events: &[web::Event], machine: &str) -> Vec<Said> {
    web::murmurs(events)
        .murmurs
        .iter()
        .filter(|murmur| murmur.said.writer == machine)
        .map(|murmur| Said {
            cell: murmur.cell().to_owned(),
            machine: murmur.said.writer.clone(),
            at: murmur.said.at,
            seq: murmur.said.seq,
            text: murmur.words.to_owned(),
            tied_to: murmur.tied_to().into_iter().map(str::to_owned).collect(),
            settled: murmur.settled.map(|(_, how)| how.to_owned()),
        })
        .collect()
}

/// The records of `said`, each keyed `murmur:<cell>:<at>`, with `.2`,
/// `.3` and on after the time of a cell's second and later murmurs of one
/// second, in the order its machine said them; each with its cell's
/// purpose as `purposes` gives it by the cell's address, and heard when
/// it was said.
pub fn records<'a, S: std::hash::BuildHasher>(
    said: impl IntoIterator<Item = &'a Said>,
    purposes: &HashMap<String, String, S>,
) -> BTreeMap<String, Value> {
    let mut said: Vec<&Said> = said.into_iter().collect();
    said.sort_by(|one, other| {
        (&one.cell, one.at, &one.machine, one.seq).cmp(&(
            &other.cell,
            other.at,
            &other.machine,
            other.seq,
        ))
    });
    let mut records = BTreeMap::new();
    let mut last: Option<(&str, i64, usize)> = None;
    for one in said {
        let nth = match last {
            Some((cell, at, nth)) if cell == one.cell && at == one.at => nth + 1,
            _ => 1,
        };
        last = Some((one.cell.as_str(), one.at, nth));
        let id = if nth == 1 {
            format!("{}:{}", one.cell, one.at)
        } else {
            format!("{}:{}.{nth}", one.cell, one.at)
        };
        let fields = json!({
            "at": one.at,
            "writer": one.cell,
            "machine": one.machine,
            "text": one.text,
            "tied_to": one.tied_to,
            "purpose": purposes.get(&one.cell),
            "settled": one.settled,
        });
        let record = daemon::record(KIND, &id, one.at.saturating_mul(1000), fields);
        records.insert(daemon::key(KIND, &id), record);
    }
    records
}

/// This machine's murmurs, kept read as its webs change, for as long as
/// this is kept.
#[derive(Debug)]
pub struct Murmurs {
    /// The scratch folder read, if one is.
    scratch: Option<PathBuf>,
    /// What was last read, swapped whole as each read ends, so asking
    /// never waits on a read.
    held: Arc<Mutex<Arc<Seen>>>,
    told: watch::Sender<u64>,
    keeping: Option<tokio::task::JoinHandle<()>>,
}

impl Murmurs {
    /// Keeps the murmurs of the machine `machine` in the quests' scratch
    /// folders in `scratch` read, on the runtime this runs on; with no
    /// scratch folder, it holds none.
    #[must_use]
    pub fn start(scratch: Option<PathBuf>, machine: String) -> Self {
        let (told, _) = watch::channel(0);
        let held = Arc::default();
        let keeping = scratch.clone().map(|scratch| {
            let webs = Webs::new(scratch, machine);
            tokio::spawn(keep(webs, Arc::clone(&held), told.clone()))
        });
        Self {
            scratch,
            held,
            told,
            keeping,
        }
    }

    /// Every murmur said in the last day before `now`, in whole seconds,
    /// as records, each cell's purpose as `purposes` gives it.
    #[must_use]
    pub fn records<S: std::hash::BuildHasher>(
        &self,
        now: i64,
        purposes: &HashMap<String, String, S>,
    ) -> BTreeMap<String, Value> {
        let seen = self.seen();
        let since = now - KEPT_FOR;
        let said = seen.said.iter().flat_map(|said| said.iter());
        records(said.filter(|said| said.at > since), purposes)
    }

    /// What changes each time the murmurs may have.
    #[must_use]
    pub fn told(&self) -> watch::Receiver<u64> {
        self.told.subscribe()
    }

    /// How the scratch folder stands, as the daemon interface's `health`
    /// names a source: answering, or refused with why; none without one.
    #[must_use]
    pub fn health(&self) -> Vec<Value> {
        let Some(scratch) = &self.scratch else {
            return Vec::new();
        };
        let file = scratch.display().to_string();
        vec![match &self.seen().refused {
            Some(why) => json!({"name": KIND, "file": file, "state": "refused", "why": why}),
            None => json!({"name": KIND, "file": file, "state": "answering"}),
        }]
    }

    fn seen(&self) -> Arc<Seen> {
        Arc::clone(&self.held.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl Drop for Murmurs {
    fn drop(&mut self) {
        if let Some(keeping) = &self.keeping {
            keeping.abort();
        }
    }
}

/// The time now, in whole seconds since the epoch.
fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// Keeps `webs` read, putting what each read saw in `held` and telling
/// `tell` each time its murmurs change: read once, then again after each
/// change in the folders it watches, and each [`AGAIN_EVERY`], when a
/// murmur aging out tells too. Each read is made off the runtime's
/// workers, and nothing waits on it but this.
async fn keep(mut webs: Webs, held: Arc<Mutex<Arc<Seen>>>, tell: watch::Sender<u64>) {
    let (heard, mut changed) = mpsc::unbounded_channel();
    let scratch = webs.scratch.clone();
    let mut watcher = crate::watching::watcher(move |event: notify::Result<notify::Event>| {
        if changes(&scratch, &event) {
            // Nothing hears this once the murmurs are no longer served.
            let _ = heard.send(());
        }
    })
    .ok();
    let mut watching = BTreeSet::new();
    let mut was = now();
    loop {
        let reading = tokio::task::spawn_blocking(move || {
            let read = webs.read();
            let at = now();
            let aged = webs.ages_out(was, at);
            (webs, read, aged, at)
        });
        // A read that panicked ends the keeping, as a panic here would.
        let Ok((read_now, read, aged, at)) = reading.await else {
            return;
        };
        webs = read_now;
        was = at;
        let folders = webs.watched().clone();
        *held.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(webs.seen());
        if read || aged {
            tell.send_modify(|changes| *changes += 1);
        }
        if let Some(watcher) = &mut watcher {
            // In one batch, so macOS's stream starts once a round, not
            // once a folder.
            let gone: Vec<PathBuf> = watching.difference(&folders).cloned().collect();
            let come: Vec<(PathBuf, notify::RecursiveMode)> = folders
                .difference(&watching)
                .map(|come| (come.clone(), notify::RecursiveMode::NonRecursive))
                .collect();
            crate::watching::watch_all(watcher, &gone, &come);
        }
        // A folder newly watched may have changed before its watch began,
        // so it is read again at once, until what is watched holds still.
        if watching != folders {
            watching = folders;
            continue;
        }
        if let Ok(Some(())) = tokio::time::timeout(AGAIN_EVERY, changed.recv()).await {
            tokio::time::sleep(SETTLED_AFTER).await;
            while changed.try_recv().is_ok() {}
        }
    }
}

/// Whether `event` may change the murmurs of the webs in `scratch`: a
/// quest's folder made or gone, a web's or its events' folder, or a
/// writer's file, or events missed. Only names are compared, as a Mac
/// names paths as the system resolves them.
fn changes(scratch: &Path, event: &notify::Result<notify::Event>) -> bool {
    let Ok(event) = event else {
        return true;
    };
    if event.need_rescan() {
        return true;
    }
    let events = web::events_in(Path::new(""));
    event.paths.iter().any(|path| {
        let name = path.file_name();
        let above = path.parent().and_then(Path::file_name);
        (above.is_some() && above == scratch.file_name())
            || name == scratch.file_name()
            || name == Some(web::FOLDER.as_ref())
            || name == events.file_name()
            || path.extension().is_some_and(|kind| kind == "jsonl")
    })
}

#[cfg(test)]
mod tests;
