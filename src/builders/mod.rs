//! The builders this machine sends Nix builds to, beside the realm's
//! machines: those its machines files name, read as Nix reads them
//! (`line`), each file named by what it is, such as `standing` or `lent`.
//! A builder is no machine of the realm: no node of its own says what it
//! is, nothing is placed on it and nothing offers it, so it is said apart,
//! as what its files say and how it stands.
//!
//! How it stands is found without asking it for work. A builder of a file
//! that is dialled is reached as SSH would reach it, for its greeting
//! (`reach`), once when first named and then at most every five minutes.
//! A builder any lent file names is never dialled, since a connection may
//! keep a builder made for a while from ending itself once idle, costing
//! its lender; its lender's word says whether it runs (`lent`).
//!
//! Each file is read whole whenever its folder says it changed, as its
//! program writes it whole, and again each minute, so a file in a folder
//! that can't be watched is still heard.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

mod lent;
mod line;
pub mod reach;

pub use lent::Lent;
pub use line::{Line, lines};
pub use reach::Answered;

/// How often the files are read again and the lent builders' lenders
/// heard: never faster, so no builder is asked after more often.
pub const HEARD_EVERY: Duration = Duration::from_secs(60);

/// How long a dialled builder is left before it is dialled again.
pub const DIALLED_EVERY: Duration = Duration::from_secs(5 * 60);

/// How long after a change the files are left before they are read, so a
/// burst of writes is read once.
const SETTLED_AFTER: Duration = Duration::from_millis(20);

/// One machines file, named by what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// What it is, such as `standing`.
    pub name: String,
    /// Where it is, in full.
    pub path: PathBuf,
    /// Whether its builders are lent, and so never dialled.
    pub lent: bool,
}

impl File {
    /// A file as `NAME=PATH` says it, lent or not.
    ///
    /// # Errors
    ///
    /// When it isn't a name, `=` and a full path.
    pub fn parse(said: &str, lent: bool) -> Result<Self, String> {
        let (name, path) = said.split_once('=').ok_or_else(|| {
            format!("{said:?} isn't NAME=PATH, such as standing=/etc/nix/machines")
        })?;
        let path = PathBuf::from(path);
        if name.is_empty() || !path.is_absolute() {
            return Err(format!("{said:?} isn't a name and a full path"));
        }
        Ok(Self {
            name: name.to_owned(),
            path,
            lent,
        })
    }
}

/// What the builders are read from.
#[derive(Debug, Clone, Default)]
pub struct Given {
    /// The machines files, in the order given; a builder several name takes
    /// what the first of those says of it, the dialled before the lent.
    pub files: Vec<File>,
    /// Where each lent builder's lender says how it stands, its host as
    /// `{host}` (`lent`).
    pub lent_health: Option<String>,
    /// SSH's program, asked where each dialled builder is.
    pub ssh: String,
}

/// One builder, as its files say it and as it stands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Builder {
    /// Its name: its host as its files name it, in lower case, its last
    /// dot and `.local` left off, or `address-` and a number for a host
    /// named by its address, which isn't said.
    pub name: String,
    /// The systems it builds for, such as `x86_64-linux`.
    pub systems: Vec<String>,
    /// How many builds it takes at once.
    pub jobs: u32,
    /// How much faster than one it builds, as Nix ranks builders.
    pub speed: f64,
    /// The features it supports.
    pub supported: Vec<String>,
    /// The features a build must ask for to be sent to it.
    pub mandatory: Vec<String>,
    /// The files naming it, by their names, in the order given.
    pub lines: Vec<String>,
    /// Whether a lent file names it, so it is never dialled.
    pub lent: bool,
    /// How it stands: `answering` or `not answering` when dialled,
    /// `running` or `not running` as its lender says, `unknown` where
    /// nothing can say, or `unheard` before it is first heard.
    pub state: String,
    /// What more it says of how it stands, such as a lender's word or why
    /// it is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub said: Option<String>,
    /// When it was last heard, dialled or its lender read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heard_at: Option<jiff::Timestamp>,
    /// The host and port its files give, kept here only: SSH's, for a
    /// builder dialled, and for a lent one its lent line's, which names
    /// its lender's health.
    #[serde(skip)]
    reached: Option<(String, Option<u16>)>,
}

impl Builder {
    /// Whether it says the same as `other` but for when it was heard.
    fn alike(&self, other: &Self) -> bool {
        Self {
            heard_at: None,
            ..self.clone()
        } == Self {
            heard_at: None,
            ..other.clone()
        }
    }
}

/// The name a builder of `host` goes by, the same however its files write
/// it, since SSH reaches the same builder by each: in lower case, and its
/// last dot and `.local`, the name of the network's own lookups, left off.
fn name_of(host: &str) -> String {
    let host = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    match host.strip_suffix(".local") {
        Some(name) => name.to_owned(),
        None => host,
    }
}

/// Whether `host` is an address rather than a name, which is never said,
/// as a network's addresses say where it is.
fn is_address(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok()
}

/// The builders that `read`, each file and the lines in it, name, in the
/// order first named: one for each name, saying what the first file
/// naming it says, with every file that names it.
#[must_use]
pub fn merged(read: &[(&File, Vec<Line>)]) -> Vec<Builder> {
    let mut builders: Vec<Builder> = Vec::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    let mut addressed = 0;
    for (file, lines) in read {
        for line in lines {
            let named = name_of(&line.host);
            if let Some(&at) = by_name.get(&named) {
                let builder = &mut builders[at];
                if !builder.lines.contains(&file.name) {
                    builder.lines.push(file.name.clone());
                }
                if file.lent && !builder.lent {
                    // Never dialled now, it is reached only for its
                    // lender's health, which its lent line's host names.
                    builder.reached = Some((line.host.clone(), line.port));
                }
                builder.lent |= file.lent;
                continue;
            }
            let name = if is_address(&line.host) {
                addressed += 1;
                format!("address-{addressed}")
            } else {
                named.clone()
            };
            by_name.insert(named, builders.len());
            builders.push(Builder {
                name,
                systems: line.systems.clone(),
                jobs: line.jobs,
                speed: line.speed,
                supported: line.supported.clone(),
                mandatory: line.mandatory.clone(),
                lines: vec![file.name.clone()],
                lent: file.lent,
                state: "unheard".to_owned(),
                said: None,
                heard_at: None,
                reached: Some((line.host.clone(), line.port)),
            });
        }
    }
    builders
}

/// What is held of the builders now.
#[derive(Debug, Default)]
struct Held {
    builders: Vec<Builder>,
    /// Why each file that couldn't be read wasn't, by its name.
    refused: BTreeMap<String, String>,
    /// Each builder dialled within [`DIALLED_EVERY`], by its name, kept
    /// that long whatever a read says, so a read made while a file was
    /// being written can't have it dialled again early.
    dialled: HashMap<String, Dialled>,
    /// The host of each builder ever named lent, by its name: it is heard
    /// as lent for as long as this runs, so a lent file that can't be read
    /// for a while never lets another file's line dial it.
    lent: HashMap<String, String>,
}

/// A builder dialled lately: when, and how it stood once heard.
#[derive(Debug)]
struct Dialled {
    at: Instant,
    state: String,
    said: Option<String>,
    heard_at: Option<jiff::Timestamp>,
}

/// The builders, kept read as their files change and heard as they stand,
/// for as long as this is kept.
#[derive(Debug)]
pub struct Builders {
    files: Vec<File>,
    held: Arc<Mutex<Held>>,
    told: watch::Sender<u64>,
    keeping: Option<tokio::task::JoinHandle<()>>,
}

impl Builders {
    /// Keeps the builders `given` names, on the runtime this runs on; none
    /// where it names no file.
    #[must_use]
    pub fn start(given: Given) -> Self {
        let held = Arc::new(Mutex::new(Held::default()));
        let (told, _) = watch::channel(0);
        let keeping = (!given.files.is_empty())
            .then(|| tokio::spawn(keep(given.clone(), Arc::clone(&held), told.clone())));
        Self {
            files: given.files,
            held,
            told,
            keeping,
        }
    }

    /// Every builder as it stands now, in the order first named.
    #[must_use]
    pub fn builders(&self) -> Vec<Builder> {
        self.lock().builders.clone()
    }

    /// What changes each time the builders may have.
    #[must_use]
    pub fn told(&self) -> watch::Receiver<u64> {
        self.told.subscribe()
    }

    /// How each file stands, as the daemon interface's `health` names a
    /// source: answering, or refused with why.
    #[must_use]
    pub fn health(&self) -> Vec<Value> {
        let held = self.lock();
        self.files
            .iter()
            .map(|file| {
                let name = format!("builders {}", file.name);
                let path = file.path.display().to_string();
                match held.refused.get(&file.name) {
                    Some(why) => {
                        json!({"name": name, "file": path, "state": "refused", "why": why})
                    }
                    None => json!({"name": name, "file": path, "state": "answering"}),
                }
            })
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, Held> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for Builders {
    fn drop(&mut self) {
        if let Some(keeping) = &self.keeping {
            keeping.abort();
        }
    }
}

/// Reads each of `files` now: its builders, or why it couldn't be read.
/// A file not there names none, which is no trouble.
fn read(files: &[File]) -> (Vec<Builder>, BTreeMap<String, String>) {
    let mut refused = BTreeMap::new();
    let mut read = Vec::new();
    // The dialled first, so a builder named by both says what they say.
    let ordered = files
        .iter()
        .filter(|file| !file.lent)
        .chain(files.iter().filter(|file| file.lent));
    for file in ordered {
        match std::fs::read_to_string(&file.path) {
            Ok(text) => read.push((file, lines(&text))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                refused.insert(
                    file.name.clone(),
                    format!("{}: {error}", file.path.display()),
                );
            }
        }
    }
    (merged(&read), refused)
}

/// Keeps the builders `given` names in `held`, telling `told` each time
/// they change: read whenever their folders change and each minute, and
/// heard as they stand once first named and each minute after.
async fn keep(given: Given, held: Arc<Mutex<Held>>, told: watch::Sender<u64>) {
    let (_watcher, mut changed) = watching(&given.files);
    let mut every = tokio::time::interval(HEARD_EVERY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = every.tick() => {}
            Some(()) = changed.recv() => {
                tokio::time::sleep(SETTLED_AFTER).await;
                while changed.try_recv().is_ok() {}
            }
        }
        let (mut builders, refused) = read(&given.files);
        kept_as_heard(
            &mut held.lock().unwrap_or_else(PoisonError::into_inner),
            &mut builders,
        );
        hear(&given, &held, &mut builders).await;
        let mut held = held.lock().unwrap_or_else(PoisonError::into_inner);
        for (file, why) in &refused {
            if held.refused.get(file) != Some(why) {
                eprintln!("builders {file}: {why}");
            }
        }
        let same = held.refused == refused
            && held.builders.len() == builders.len()
            && held
                .builders
                .iter()
                .zip(&builders)
                .all(|(one, other)| one.alike(other));
        held.refused = refused;
        held.builders = builders;
        if !same {
            told.send_modify(|changes| *changes += 1);
        }
    }
}

/// Gives each of `builders`, just read, what `held` knows of it: how it
/// stood when last heard, and whether it was ever lent.
fn kept_as_heard(held: &mut Held, builders: &mut [Builder]) {
    for builder in builders {
        if builder.lent {
            if let Some((host, _)) = &builder.reached {
                held.lent.insert(builder.name.clone(), host.clone());
            }
        } else if let Some(host) = held.lent.get(&builder.name) {
            builder.lent = true;
            builder.reached = Some((host.clone(), None));
        }
        if let Some(was) = held.builders.iter().find(|was| was.name == builder.name) {
            builder.state.clone_from(&was.state);
            builder.said.clone_from(&was.said);
            builder.heard_at = was.heard_at;
        } else if let Some(was) = held.dialled.get(&builder.name) {
            builder.state.clone_from(&was.state);
            builder.said.clone_from(&was.said);
            builder.heard_at = was.heard_at;
        }
    }
}

/// Hears how each of `builders` stands: each lent one as its lender says,
/// and each other one dialled, all at once, if it has never been or not
/// for [`DIALLED_EVERY`].
async fn hear(given: &Given, held: &Mutex<Held>, builders: &mut [Builder]) {
    let now = jiff::Timestamp::now();
    let due: Vec<usize> = {
        let mut held = held.lock().unwrap_or_else(PoisonError::into_inner);
        held.dialled
            .retain(|_, dialled| dialled.at.elapsed() < DIALLED_EVERY);
        let due: Vec<usize> = builders
            .iter()
            .enumerate()
            .filter(|(_, builder)| !builder.lent && !held.dialled.contains_key(&builder.name))
            .map(|(at, _)| at)
            .collect();
        for &at in &due {
            let builder = &builders[at];
            held.dialled.insert(
                builder.name.clone(),
                Dialled {
                    at: Instant::now(),
                    state: builder.state.clone(),
                    said: builder.said.clone(),
                    heard_at: builder.heard_at,
                },
            );
        }
        due
    };
    for builder in builders.iter_mut().filter(|builder| builder.lent) {
        let host = builder
            .reached
            .as_ref()
            .map_or("", |(host, _)| host.as_str());
        let (state, said) = match lent::lent(given.lent_health.as_deref(), host, now.as_second()) {
            Lent::Running(word) => ("running", word),
            Lent::NotRunning(why) => ("not running", why),
            Lent::Unknown(why) => ("unknown", why),
        };
        stands(builder, state, said, now);
    }
    let mut asking = JoinSet::new();
    for at in due {
        let Some((host, port)) = builders[at].reached.clone() else {
            continue;
        };
        let ssh = given.ssh.clone();
        asking.spawn(async move { (at, reach::answered(&ssh, &host, port).await) });
    }
    let mut heard = Vec::new();
    while let Some(joined) = asking.join_next().await {
        // One whose asking failed stands as it did, and the rest are heard.
        let Ok((at, answered)) = joined else {
            continue;
        };
        let (state, said) = match answered {
            Answered::Yes => ("answering", None),
            Answered::No(why) => ("not answering", Some(why)),
            Answered::Unknown(why) => ("unknown", Some(why)),
        };
        let builder = &mut builders[at];
        state.clone_into(&mut builder.state);
        builder.said = said;
        builder.heard_at = Some(jiff::Timestamp::now());
        heard.push(at);
    }
    let mut held = held.lock().unwrap_or_else(PoisonError::into_inner);
    for at in heard {
        let builder = &builders[at];
        if let Some(dialled) = held.dialled.get_mut(&builder.name) {
            dialled.state.clone_from(&builder.state);
            dialled.said.clone_from(&builder.said);
            dialled.heard_at = builder.heard_at;
        }
    }
}

/// Says `builder` stands as `state`, saying `said`, heard `now`.
fn stands(builder: &mut Builder, state: &str, said: String, now: jiff::Timestamp) {
    state.clone_into(&mut builder.state);
    builder.said = Some(said);
    builder.heard_at = Some(now);
}

/// Watches the folders of `files`, telling what this answers each time
/// one of them may have changed. A folder that can't be watched is read
/// only each minute.
fn watching(
    files: &[File],
) -> (
    Option<notify::RecommendedWatcher>,
    mpsc::UnboundedReceiver<()>,
) {
    use notify::Watcher;
    let (tell, told) = mpsc::unbounded_channel();
    let names: Vec<_> = files
        .iter()
        .filter_map(|file| file.path.file_name().map(ToOwned::to_owned))
        .collect();
    let watcher = crate::watching::watcher(move |event: notify::Result<notify::Event>| {
        let touched = event.as_ref().map_or(true, |event| {
            event.need_rescan()
                || event.paths.iter().any(|path| {
                    path.file_name()
                        .is_some_and(|name| names.iter().any(|one| one == name))
                })
        });
        if touched {
            // Nothing hears this once the builders are no longer kept.
            let _ = tell.send(());
        }
    });
    let mut watcher = match watcher {
        Ok(watcher) => watcher,
        Err(error) => {
            eprintln!("the builders' files aren't watched, so are read each minute: {error}");
            return (None, told);
        }
    };
    let mut folders: Vec<&Path> = files.iter().filter_map(|file| file.path.parent()).collect();
    folders.sort();
    folders.dedup();
    for folder in folders {
        if let Err(error) = watcher.watch(folder, notify::RecursiveMode::NonRecursive) {
            eprintln!(
                "{} isn't watched, so its builders are read each minute: {error}",
                folder.display()
            );
        }
    }
    (Some(watcher), told)
}

#[cfg(test)]
mod tests;
