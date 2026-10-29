//! One end of a link's talk about shared quests: which it shares, what it
//! holds of each, and the files the other end lacks.
//!
//! Each end says which quests it shares, and the other starts sharing any
//! it doesn't: its own copy of that quest, if it has one, is moved aside
//! first, so the quest it then takes is the sharing machine's and nothing
//! it held is lost. Each end then says what it holds of each file, by
//! content and version, again whenever a scan finds a change, and asks for
//! each file the other holds newer. A file written on both ends apart is
//! settled by one end alone, the machine whose name sorts first, so the
//! two never both merge and chase each other's merges; the other takes
//! what it settled, which is newer than either.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use super::clones::{Clone, Cloning};
use super::ledger::{Entry, Ledger, Ledgers, is_quest, shared_of};
use super::pieces::{PIECE, Pieces, Staged};
use super::settle::{Merge, Settled, State, beside, settle};
use super::tree::{Place, Quest, Shared, content, may_name};
use super::watch::Watch;
use super::{journey, log};
use crate::group::Group;
use crate::record::Intent;
use crate::wire::Draft;

/// Where this machine keeps quests, and mycelium's own folder.
#[derive(Debug, Clone)]
pub struct Places {
    /// The quest store: a folder a quest.
    pub quests: PathBuf,
    /// The quests' scratch folders: one a quest, by its id.
    pub scratch: PathBuf,
    /// The account's home folder.
    pub home: PathBuf,
    /// Mycelium's own folder, where the ledgers are kept and a quest's
    /// copy is moved aside to.
    pub state: PathBuf,
}

impl Places {
    /// Where `id`'s record is kept on this machine.
    #[must_use]
    pub fn record(&self, id: &str) -> PathBuf {
        self.quests.join(id)
    }

    /// Where `id` is kept on this machine: its record, and the folders of
    /// its scratch its ledger names, never the whole, since a scratch can
    /// hold gigabytes of clones and data no other machine needs.
    #[must_use]
    pub fn quest(&self, id: &str, ledger: &Ledger) -> Quest {
        let quest = Quest::new(self.record(id), self.home.clone());
        // An ended quest's record still crosses, so the other end learns it
        // ended, but its scratch is let be: concluding takes it away here.
        if self.ended(id) {
            return quest;
        }
        quest.sharing(self.scratch.join(id), ledger.scratch.clone())
    }

    /// Whether quest `id` has ended on this machine: its README says it
    /// concluded or was retired.
    #[must_use]
    pub fn ended(&self, id: &str) -> bool {
        fs::read_to_string(self.record(id).join("README.md")).is_ok_and(|readme| has_ended(&readme))
    }

    /// Names `folder` of shared quest `id`'s scratch for sharing, refusing
    /// one that may not be named, isn't there, is a git clone, or is too
    /// heavy to cross, saying which.
    pub fn name_scratch(&self, id: &str, folder: &str) -> io::Result<()> {
        let refused = |why: String| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{folder} of {id}'s scratch can't be shared: {why}"),
            )
        };
        if !may_name(folder) {
            return Err(refused(
                "only a folder right in the scratch may be, and never tmp/, build output \
                 or a hidden one"
                    .to_owned(),
            ));
        }
        let at = self.scratch.join(id).join(folder);
        if !at.is_dir() {
            return Err(refused(format!("{} is no folder", at.display())));
        }
        if at.join(".git").exists() {
            // A clone crosses as its remote, so how much it holds is no
            // matter, but one that can't be made again from it is refused.
            if !Clone::read(&at).is_some_and(|clone| clone.may_clone()) {
                return Err(refused(
                    "it is a git clone with no origin another machine can clone from".to_owned(),
                ));
            }
            let mut held = self.ledgers().hold(id)?;
            held.ledger.scratch.insert(folder.to_owned());
            return held.keep();
        }
        let only = Quest::new(self.record(id), self.home.clone())
            .sharing(self.scratch.join(id), BTreeSet::from([folder.to_owned()]));
        let (mut files, mut bytes) = (0_u64, 0_u64);
        only.each_file(&mut |shared| {
            if shared.place != Place::Scratch {
                return Ok(());
            }
            let len = fs::metadata(only.path_of(&shared)?)?.len();
            if len > LARGEST_FILE {
                return Err(refused(format!(
                    "{} holds {len} bytes, more than the {LARGEST_FILE} a file may, since \
                     each is held whole once it lands",
                    shared.path
                )));
            }
            files += 1;
            bytes += len;
            if files > MOST_FILES || bytes > MOST_BYTES {
                return Err(refused(format!(
                    "it holds more than {MOST_FILES} files or {MOST_BYTES} bytes"
                )));
            }
            Ok(())
        })?;
        let mut held = self.ledgers().hold(id)?;
        held.ledger.scratch.insert(folder.to_owned());
        held.keep()
    }

    /// This machine's ledgers.
    #[must_use]
    pub fn ledgers(&self) -> Ledgers {
        Ledgers::in_state(&self.state)
    }

    /// The group the sessions on quest `id` hear, named by the short ids of
    /// the quests above it, outermost first, as the mesh names it: a group
    /// is heard by its own and its parents' names, never a part of one.
    #[must_use]
    pub fn group(&self, id: &str) -> Option<Group> {
        let mut shorts = Vec::new();
        let mut quest = Some(id.to_owned());
        while let Some(at) = quest {
            // A parent named twice is a loop no quest store should hold.
            if shorts.len() > MOST_NESTED {
                return None;
            }
            shorts.push(at.rsplit('-').next()?.to_lowercase());
            let readme = fs::read_to_string(self.quests.join(&at).join("README.md")).ok()?;
            quest = parent(&readme);
        }
        shorts.reverse();
        Group::new(format!("quest.{}", shorts.join("."))).ok()
    }
}

/// The most files a scratch folder named for sharing may hold, so a
/// folder of bulk data isn't named by mistake.
const MOST_FILES: u64 = 10_000;
/// The most bytes a scratch folder named for sharing may hold.
const MOST_BYTES: u64 = 1 << 30;
/// The largest file a scratch folder named for sharing may hold: a file
/// crosses in pieces, but is held whole once it lands, to be settled.
const LARGEST_FILE: u64 = 64 << 20;

/// How many quests deep a quest may sit under others.
const MOST_NESTED: usize = 16;

/// The quest a README's front matter names as its parent, if any.
fn parent(readme: &str) -> Option<String> {
    let front = readme.strip_prefix("---\n")?.split("\n---").next()?;
    let parent = front
        .lines()
        .find_map(|line| line.strip_prefix("parent:"))?
        .trim()
        .trim_matches(['"', '\'']);
    (!parent.is_empty() && parent != "null" && parent != "~").then(|| parent.to_owned())
}

/// What one end says to the other about shared quests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Say {
    /// Every quest this end shares.
    Shares {
        /// Their ids.
        quests: Vec<String>,
        /// The folders of each quest's scratch that are shared, by the
        /// quest's id; an end older than this names none.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        scratch: BTreeMap<String, BTreeSet<String>>,
        /// The git clones in those folders, by each one's path in the
        /// scratch, by the quest's id; an end older than this names none.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        clones: BTreeMap<String, BTreeMap<String, Clone>>,
        /// Those of the quests that have ended at this end; an end older
        /// than this names none.
        #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
        ended: BTreeSet<String>,
    },
    /// What this end holds of some of a quest's files.
    Holds {
        /// The quest's id.
        quest: String,
        /// Each file's state, by its key.
        files: BTreeMap<String, State>,
    },
    /// Asks for a file.
    Want {
        /// The quest's id.
        quest: String,
        /// The file's key.
        file: String,
        /// Where in it to go on from, for a file crossing in pieces; an end
        /// older than pieces names none, and is sent the file whole.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<u64>,
    },
    /// A piece of a file larger than one, as it crosses ([`pieces`]).
    Piece {
        /// The quest's id.
        quest: String,
        /// The file's key.
        file: String,
        /// Its state on the end that sent it.
        state: State,
        /// Where in the file the piece begins.
        at: u64,
        /// How long the whole file is.
        len: u64,
        /// What the piece holds, in base64.
        body: String,
        /// Who may read, write and run the file.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<u32>,
    },
    /// A file, as it crosses.
    File {
        /// The quest's id.
        quest: String,
        /// The file's key.
        file: String,
        /// Its state on the end that sent it.
        state: State,
        /// What it holds, in base64, or `None` once deleted.
        body: Option<String>,
        /// Who may read, write and run it, as its mode's permission bits;
        /// an end older than this names none, and the file keeps its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<u32>,
    },
}

/// This end's part in the talk.
pub struct Exchange {
    places: Places,
    machine: String,
    other: String,
    /// The quests last said to be shared, with the folders of each one's
    /// scratch and the clones in them.
    told: BTreeMap<String, Told>,
    /// What the quests' sessions should hear, not yet said.
    findings: Vec<Draft>,
    /// The large files crossing from the other end, a piece at a time.
    pieces: Pieces,
    /// The shared scratch folders, watched so a look walks only those that
    /// changed.
    watch: Watch,
    /// The files of each quest found changed other than by a look, which
    /// the next look says.
    unsaid: BTreeMap<String, BTreeSet<String>>,
    /// The clones being made of those the other end holds.
    cloning: Cloning,
    /// The quests the other end last said it shares, and those ended there,
    /// once it has said.
    theirs: Option<Theirs>,
}

/// The quests the other end said it shares.
#[derive(Debug, Default)]
struct Theirs {
    /// Every one.
    quests: BTreeSet<String>,
    /// Those that have ended there.
    ended: BTreeSet<String>,
}

/// What was last said of a shared quest's scratch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Told {
    /// The folders named.
    folders: BTreeSet<String>,
    /// The clones in them.
    clones: BTreeMap<String, Clone>,
    /// Whether the quest has ended here.
    ended: bool,
}

impl Exchange {
    /// `machine`'s end of the talk with `other`, clearing any crossing an
    /// earlier link to `other` left unfinished, since nothing goes on with
    /// it.
    #[must_use]
    pub fn new(places: Places, machine: String, other: String) -> Self {
        let pieces = Pieces::from(&places.state, &other);
        if let Err(error) = pieces.clear() {
            eprintln!("couldn't clear what an earlier link to {other} left staged: {error}");
        }
        Self {
            watch: Watch::new(places.scratch.clone()),
            places,
            machine,
            other,
            told: BTreeMap::new(),
            findings: Vec::new(),
            pieces,
            unsaid: BTreeMap::new(),
            cloning: Cloning::default(),
            theirs: None,
        }
    }

    /// What the shared quests' sessions should hear since last asked, as
    /// findings this machine's node sends: from `mycelium@<machine>`, since
    /// it is no session, with any dash in the machine's name made `_`, as a
    /// mesh id parts its sender from the rest by dashes.
    pub fn findings(&mut self) -> Vec<Draft> {
        std::mem::take(&mut self.findings)
    }

    /// Tells the sessions on quest `id` that its file `key` was written on
    /// both machines apart, and where the other's now is.
    fn kept_beside(&mut self, id: &str, key: &str, beside: &str) {
        let text = format!(
            "{key} of {id} was written on {other} and {here} apart. {here} kept its own, \
             and {other}'s is beside it on {here} as {beside}: read both and keep what each says.",
            other = self.other,
            here = self.machine,
        );
        match self.places.group(id) {
            Some(group) => self.findings.push(Draft {
                from: format!("mycelium@{}", self.machine.replace('-', "_")),
                groups: vec![group],
                intent: Some(Intent::Finding),
                text,
                re: None,
            }),
            None => eprintln!("{text} (its sessions' group couldn't be read, so none was told)"),
        }
    }

    /// Keeps file `key` of quest `id`, found changed other than by a look,
    /// for the next look to say.
    fn unsay(&mut self, id: &str, key: &str) {
        self.unsaid
            .entry(id.to_owned())
            .or_default()
            .insert(key.to_owned());
    }

    /// Whether quest `id` has ended at the other end as far as this end
    /// knows: it said so, or it no longer names it at all.
    fn has_ended_there(&self, id: &str) -> bool {
        self.theirs
            .as_ref()
            .is_some_and(|theirs| theirs.ended.contains(id) || !theirs.quests.contains(id))
    }

    /// Whether this end settles files written apart.
    fn settles(&self) -> bool {
        self.machine < self.other
    }

    /// Looks at every shared quest for changes, saying which quests are
    /// shared if that changed, everything held of a quest newly shared,
    /// and what changed of the rest.
    pub fn look(&mut self) -> io::Result<Vec<Say>> {
        self.cloning.reap();
        let ledgers = self.places.ledgers();
        let mut shares = BTreeMap::new();
        let mut holds = Vec::new();
        for id in ledgers.quests()? {
            let ended = self.places.ended(&id);
            if ended && self.has_ended_there(&id) {
                ledgers.stop(&id)?;
                self.unsaid.remove(&id);
                self.watch.forget(&id);
                eprintln!(
                    "{id} has ended here and on {}, so it is shared no longer",
                    self.other
                );
                continue;
            }
            let mut held = ledgers.hold(&id)?;
            let walk = self.watch.to_walk(&id, &held.ledger.scratch);
            let quest = self.places.quest(&id, &held.ledger).walking(&walk);
            let mut changed: BTreeSet<String> = held
                .ledger
                .scan(&quest, &self.machine)?
                .into_iter()
                .collect();
            let clones_changed = found_clones(&mut held.ledger, &quest, &walk)?;
            if !changed.is_empty() || clones_changed {
                held.keep()?;
            }
            changed.extend(self.unsaid.remove(&id).unwrap_or_default());
            let states = held.ledger.states();
            let files: BTreeMap<String, State> = if self.told.contains_key(&id) {
                states
                    .into_iter()
                    .filter(|(key, _)| changed.contains(key))
                    .collect()
            } else {
                states
            };
            if !files.is_empty() || !self.told.contains_key(&id) {
                holds.push(Say::Holds {
                    quest: id.clone(),
                    files,
                });
            }
            shares.insert(
                id,
                Told {
                    folders: held.ledger.scratch.clone(),
                    clones: held.ledger.clones.clone(),
                    ended,
                },
            );
        }
        // Which folders are shared is said first, so the other end carries
        // a newly named folder's files by the time it hears of them.
        let mut says = Vec::new();
        if shares != self.told {
            says.push(Say::Shares {
                quests: shares.keys().cloned().collect(),
                scratch: shares
                    .iter()
                    .filter(|(_, told)| !told.folders.is_empty())
                    .map(|(id, told)| (id.clone(), told.folders.clone()))
                    .collect(),
                clones: shares
                    .iter()
                    .filter(|(_, told)| !told.clones.is_empty())
                    .map(|(id, told)| (id.clone(), told.clones.clone()))
                    .collect(),
                ended: shares
                    .iter()
                    .filter(|(_, told)| told.ended)
                    .map(|(id, _)| id.clone())
                    .collect(),
            });
        }
        says.extend(holds);
        self.told = shares;
        Ok(says)
    }

    /// Takes what the other end said, returning what this end says back.
    pub fn hear(&mut self, say: Say) -> io::Result<Vec<Say>> {
        match say {
            Say::Shares {
                quests,
                scratch,
                clones,
                ended,
            } => {
                self.theirs = Some(Theirs {
                    quests: quests.iter().cloned().collect(),
                    ended,
                });
                for id in quests {
                    // A quest ended at both ends, which this end has
                    // stopped sharing, isn't taken up again; one reopened
                    // there is.
                    if !(self.places.ended(&id) && self.has_ended_there(&id)) {
                        self.share(&id)?;
                    }
                }
                for (id, folders) in scratch {
                    if !self.places.ended(&id) {
                        self.share_scratch(&id, &folders)?;
                    }
                }
                for (id, clones) in clones {
                    self.clone_what(&id, &clones)?;
                }
                Ok(Vec::new())
            }
            Say::Holds { quest, files } => {
                self.share(&quest)?;
                self.held(&quest, files)
            }
            Say::Want { quest, file, at } => self.want(&quest, &file, at),
            Say::Piece {
                quest,
                file,
                state,
                at,
                len,
                body,
                mode,
            } => self.piece(&quest, &file, &state, (at, len), &body, mode),
            Say::File {
                quest,
                file,
                state,
                body,
                mode,
            } => self.file(&quest, &file, &state, body.as_deref(), mode),
        }
    }

    /// Starts sharing `id` if this end doesn't yet, moving its own record
    /// aside first. None of its scratch is shared until a folder of it is
    /// named.
    fn share(&self, id: &str) -> io::Result<()> {
        let ledgers = self.places.ledgers();
        if !is_quest(id) || ledgers.shares(id) {
            return Ok(());
        }
        self.move_aside(&self.places.record(id), id, "record")?;
        ledgers.hold(id)?.keep()
    }

    /// Starts sharing each of `folders` of `id`'s scratch this end doesn't
    /// yet, moving its own copy of each aside first; a name that may not be
    /// shared, or a quest this end doesn't share, is passed over.
    fn share_scratch(&self, id: &str, folders: &BTreeSet<String>) -> io::Result<()> {
        let ledgers = self.places.ledgers();
        if !is_quest(id) || !ledgers.shares(id) {
            return Ok(());
        }
        let mut held = ledgers.hold(id)?;
        let new: Vec<&String> = folders
            .iter()
            .filter(|folder| may_name(folder) && !held.ledger.scratch.contains(*folder))
            .collect();
        if new.is_empty() {
            return Ok(());
        }
        for folder in new {
            let at = self.places.scratch.join(id).join(folder);
            self.move_aside(&at, id, &format!("scratch/{folder}"))?;
            held.ledger.scratch.insert(folder.clone());
        }
        held.keep()
    }

    /// Starts making each of `clones` of quest `id` this end lacks, in a
    /// scratch folder it shares, leaving any folder already there as it is.
    fn clone_what(&mut self, id: &str, clones: &BTreeMap<String, Clone>) -> io::Result<()> {
        if !is_quest(id) || !self.places.ledgers().shares(id) {
            return Ok(());
        }
        let held = self.places.ledgers().hold(id)?;
        let quest = self.places.quest(id, &held.ledger);
        drop(held);
        for (path, clone) in clones {
            let shared = Shared {
                place: Place::Scratch,
                path: path.clone(),
            };
            if !quest.holds_clone(path) {
                continue;
            }
            let Ok(folder) = quest.path_of(&shared) else {
                continue;
            };
            if self.cloning.start(&folder, clone)? {
                eprintln!(
                    "cloning {} into {} for {id}, as {} holds it",
                    clone.remote,
                    folder.display(),
                    self.other
                );
            }
        }
        Ok(())
    }

    /// Moves this machine's copy of `id`'s `name`, at `folder`, aside into
    /// mycelium's own folder, if it has one, so nothing it held is lost.
    fn move_aside(&self, folder: &std::path::Path, id: &str, name: &str) -> io::Result<()> {
        if !folder.exists() {
            return Ok(());
        }
        let aside = self
            .places
            .state
            .join("before-share")
            .join(
                jiff::Timestamp::now()
                    .strftime("%Y%m%dT%H%M%SZ")
                    .to_string(),
            )
            .join(id)
            .join(name);
        fs::create_dir_all(aside.parent().unwrap_or(&aside))?;
        fs::rename(folder, &aside)?;
        eprintln!(
            "moved this machine's copy of {id}'s {name} aside to {}",
            aside.display()
        );
        Ok(())
    }

    /// Asks for each file the other end holds that this end should take.
    fn held(&mut self, id: &str, files: BTreeMap<String, State>) -> io::Result<Vec<Say>> {
        let mut held = self.places.ledgers().hold(id)?;
        let quest = self.places.quest(id, &held.ledger);
        let mut wants = Vec::new();
        for (key, theirs) in files {
            let Some(shared) = shared_of(&key).filter(|shared| quest.carries(shared)) else {
                continue;
            };
            if held.ledger.scan_one(&quest, &shared, &self.machine)? {
                held.keep()?;
                self.unsay(id, &key);
            }
            let ours = held
                .ledger
                .files
                .get(&key)
                .map(Entry::state)
                .unwrap_or_default();
            match settle(&shared, &ours, &theirs) {
                Settled::Keep => {}
                Settled::Merge(_) | Settled::Beside if !self.settles() => {}
                _ if theirs.hash.is_none() => {
                    // A deletion needs nothing sent: it is taken here.
                    quest.write(&shared, None, None)?;
                    held.ledger.files.insert(
                        key,
                        Entry {
                            hash: None,
                            version: ours.version.joined(&theirs.version),
                            seen: None,
                        },
                    );
                    held.keep()?;
                }
                _ => wants.push(Say::Want {
                    quest: id.to_owned(),
                    file: key,
                    at: Some(0),
                }),
            }
        }
        Ok(wants)
    }

    /// Sends a file the other end asked for, as this end holds it now.
    fn want(&mut self, id: &str, key: &str, at: Option<u64>) -> io::Result<Vec<Say>> {
        if !self.places.ledgers().shares(id) {
            return Ok(Vec::new());
        }
        let mut held = self.places.ledgers().hold(id)?;
        let quest = self.places.quest(id, &held.ledger);
        let Some(shared) = shared_of(key).filter(|shared| quest.carries(shared)) else {
            return Ok(Vec::new());
        };
        if held.ledger.scan_one(&quest, &shared, &self.machine)? {
            held.keep()?;
            self.unsay(id, key);
        }
        let Some(entry) = held.ledger.files.get(key) else {
            return Ok(Vec::new());
        };
        let mode = quest.mode_of(&shared)?;
        let body = match at {
            None => quest.read(&shared)?,
            Some(at) => match quest.read_piece(&shared, at)? {
                Some((piece, len)) if at > 0 || len > PIECE => {
                    return Ok(vec![Say::Piece {
                        quest: id.to_owned(),
                        file: key.to_owned(),
                        state: entry.state(),
                        at,
                        len,
                        body: STANDARD.encode(piece),
                        mode,
                    }]);
                }
                // A file of one piece crosses whole.
                whole => whole.map(|(piece, _)| piece),
            },
        };
        Ok(vec![Say::File {
            quest: id.to_owned(),
            file: key.to_owned(),
            state: entry.state(),
            body: body.map(|bytes| STANDARD.encode(bytes)),
            mode,
        }])
    }

    /// Stages a piece of a file the other end sent, asking for the next,
    /// and takes the file as a whole one once it is whole.
    fn piece(
        &mut self,
        id: &str,
        key: &str,
        theirs: &State,
        (at, len): (u64, u64),
        body: &str,
        mode: Option<u32>,
    ) -> io::Result<Vec<Say>> {
        let Some(hash) = theirs.hash.as_deref() else {
            return Ok(Vec::new());
        };
        if !self.places.ledgers().shares(id) {
            return Ok(Vec::new());
        }
        let body = STANDARD
            .decode(body)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        match self.pieces.stage(id, key, hash, at, len, &body)? {
            Staged::More(next) => Ok(vec![Say::Want {
                quest: id.to_owned(),
                file: key.to_owned(),
                at: Some(next),
            }]),
            Staged::Whole(whole) => self.take_file(id, key, theirs, Some(&whole), mode),
            // The crossing begun again over it carries on.
            Staged::Astray => Ok(Vec::new()),
        }
    }

    /// Takes a file the other end sent, by how it settles against this
    /// end's as it stands now, saying what this end then holds if that is
    /// something new.
    fn file(
        &mut self,
        id: &str,
        key: &str,
        theirs: &State,
        body: Option<&str>,
        mode: Option<u32>,
    ) -> io::Result<Vec<Say>> {
        if !self.places.ledgers().shares(id) {
            return Ok(Vec::new());
        }
        let body = body
            .map(|body| STANDARD.decode(body))
            .transpose()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        self.take_file(id, key, theirs, body.as_deref(), mode)
    }

    /// Takes a whole file the other end sent, or its deletion for `None`,
    /// as [`Self::file`] does.
    fn take_file(
        &mut self,
        id: &str,
        key: &str,
        theirs: &State,
        body: Option<&[u8]>,
        mode: Option<u32>,
    ) -> io::Result<Vec<Say>> {
        if body.map(|body| content(body).hash) != theirs.hash {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{key} of {id} doesn't hold what its hash says"),
            ));
        }
        let mut held = self.places.ledgers().hold(id)?;
        let quest = self.places.quest(id, &held.ledger);
        let Some(shared) = shared_of(key).filter(|shared| quest.carries(shared)) else {
            return Ok(Vec::new());
        };
        let scanned = held.ledger.scan_one(&quest, &shared, &self.machine)?;
        if scanned {
            self.unsay(id, key);
        }
        let ours = held
            .ledger
            .files
            .get(key)
            .map(Entry::state)
            .unwrap_or_default();
        let joined = ours.version.joined(&theirs.version);
        let mut says = Vec::new();
        let entry = match settle(&shared, &ours, theirs) {
            Settled::Keep => None,
            Settled::Merge(_) | Settled::Beside if !self.settles() => None,
            Settled::Take => {
                let written = quest.write(&shared, body, mode)?;
                if is_journey(&shared) {
                    held.ledger.journey =
                        body.map(|body| String::from_utf8_lossy(body).into_owned());
                }
                Some(Entry {
                    hash: theirs.hash.clone(),
                    version: joined,
                    seen: written,
                })
            }
            Settled::Merge(merge) => {
                let ours_text = quest.read(&shared)?.unwrap_or_default();
                let ours_text = String::from_utf8_lossy(&ours_text);
                let theirs_text = String::from_utf8_lossy(body.unwrap_or_default());
                let merged = match merge {
                    Merge::Journey => {
                        journey::merged(held.ledger.journey.as_deref(), &ours_text, &theirs_text)
                    }
                    Merge::Log => log::merged(&ours_text, &theirs_text),
                };
                // Both ends' entries are kept, and so is this end's mode.
                let written = quest.write(&shared, Some(merged.as_bytes()), None)?;
                if merge == Merge::Journey {
                    held.ledger.journey = Some(merged.clone());
                }
                Some(Entry {
                    hash: Some(content(merged.as_bytes()).hash),
                    version: joined.changed_on(&self.machine),
                    seen: written,
                })
            }
            Settled::Beside => {
                let theirs_at = Shared {
                    place: shared.place,
                    path: beside(&shared.path, &self.other),
                };
                quest.write(&theirs_at, body, mode)?;
                self.kept_beside(id, key, &theirs_at.path);
                let ours_entry = held.ledger.files.get(key).cloned().unwrap_or_default();
                Some(Entry {
                    version: joined.changed_on(&self.machine),
                    ..ours_entry
                })
            }
        };
        if let Some(entry) = entry {
            let state = entry.state();
            let changed = state != *theirs;
            held.ledger.files.insert(key.to_owned(), entry);
            held.keep()?;
            if changed {
                says.push(Say::Holds {
                    quest: id.to_owned(),
                    files: BTreeMap::from([(key.to_owned(), state)]),
                });
            }
        } else if scanned {
            held.keep()?;
        }
        Ok(says)
    }
}

/// Enters in `ledger` the clones found in the scratch folders `walked` of
/// `quest`, forgetting those no longer there, and answers whether that
/// changed what it holds. A clone that can't be made again is left out.
fn found_clones(ledger: &mut Ledger, quest: &Quest, walked: &BTreeSet<String>) -> io::Result<bool> {
    let mut clones: BTreeMap<String, Clone> = ledger
        .clones
        .iter()
        .filter(|(path, _)| {
            path.split('/')
                .next()
                .is_some_and(|folder| !walked.contains(folder))
        })
        .map(|(path, clone)| (path.clone(), clone.clone()))
        .collect();
    for (path, clone) in quest.clones()? {
        if let Some(clone) = clone.filter(Clone::may_clone) {
            clones.insert(path, clone);
        }
    }
    let changed = clones != ledger.clones;
    ledger.clones = clones;
    Ok(changed)
}

/// Whether a quest's README says it has ended: its front matter's `status`
/// is `concluded` or `retired`.
fn has_ended(readme: &str) -> bool {
    let mut lines = readme.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return false;
    }
    lines
        .take_while(|line| line.trim_end() != "---")
        .filter_map(|line| line.strip_prefix("status:"))
        .any(|status| matches!(status.trim(), "concluded" | "retired"))
}

fn is_journey(shared: &Shared) -> bool {
    shared.place == Place::Record && shared.path == "JOURNEY.md"
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::watch::HEARD_WITHIN;

    const QUEST: &str = "QEST-20261013-4UHC5Q";

    struct Machine {
        _dir: tempfile::TempDir,
        places: Places,
        name: &'static str,
    }

    impl Machine {
        fn new(name: &'static str) -> Self {
            let dir = tempfile::tempdir().expect("a temporary folder");
            let places = Places {
                quests: dir.path().join("quests"),
                scratch: dir.path().join("scratch"),
                home: dir.path().join("home"),
                state: dir.path().join("state"),
            };
            Self {
                _dir: dir,
                places,
                name,
            }
        }

        fn write(&self, path: &str, text: &str) {
            let path = self.places.quests.join(QUEST).join(path);
            fs::create_dir_all(path.parent().expect("a file has a folder")).expect("made");
            fs::write(path, text).expect("written");
        }

        fn read(&self, path: &str) -> Option<String> {
            fs::read_to_string(self.places.quests.join(QUEST).join(path)).ok()
        }

        fn exchange(&self, other: &Machine) -> Exchange {
            Exchange::new(
                self.places.clone(),
                self.name.to_owned(),
                other.name.to_owned(),
            )
        }
    }

    /// Passes what each end says to the other until neither has more to
    /// say, after each has looked for changes once.
    fn talk(one: &mut Exchange, two: &mut Exchange) {
        let mut to_two = one.look().expect("one looks");
        let mut to_one = two.look().expect("two looks");
        for _ in 0..20 {
            if to_one.is_empty() && to_two.is_empty() {
                return;
            }
            let mut next_two = Vec::new();
            for say in to_two.drain(..) {
                next_two.extend(two.hear(say).expect("two hears"));
            }
            let mut next_one = Vec::new();
            for say in to_one.drain(..) {
                next_one.extend(one.hear(say).expect("one hears"));
            }
            to_one = next_two;
            to_two = next_one;
        }
        panic!("the two ends never stopped talking");
    }

    #[test]
    fn a_shared_quest_reaches_the_other_end_whose_own_copy_moves_aside() {
        let optimus = Machine::new("optimus");
        let equilibrius = Machine::new("equilibrius");
        optimus.write("README.md", "# Shared\n");
        optimus.write("plans/PLAN-20261026-IZH11J.md", "plan\n");
        equilibrius.write("README.md", "# Stale\n");
        optimus
            .places
            .ledgers()
            .hold(QUEST)
            .expect("held")
            .keep()
            .expect("shared");

        let (mut near, mut far) = (
            optimus.exchange(&equilibrius),
            equilibrius.exchange(&optimus),
        );
        talk(&mut near, &mut far);

        assert_eq!(
            equilibrius.read("README.md").as_deref(),
            Some("# Shared\n"),
            "the README crossed"
        );
        assert_eq!(
            equilibrius.read("plans/PLAN-20261026-IZH11J.md").as_deref(),
            Some("plan\n"),
            "the plan crossed"
        );
        let aside = fs::read_dir(equilibrius.places.state.join("before-share"))
            .expect("something moved aside")
            .next()
            .expect("one move")
            .expect("read")
            .path();
        assert_eq!(
            fs::read_to_string(aside.join(QUEST).join("record").join("README.md")).expect("kept"),
            "# Stale\n",
            "the far end's own copy was kept aside"
        );

        equilibrius.write("README.md", "# Changed there\n");
        talk(&mut near, &mut far);
        assert_eq!(
            optimus.read("README.md").as_deref(),
            Some("# Changed there\n"),
            "a change comes back"
        );
    }

    #[test]
    fn a_journey_written_on_both_ends_keeps_both_entries() {
        let optimus = Machine::new("optimus");
        let equilibrius = Machine::new("equilibrius");
        optimus.write("JOURNEY.md", "# Journey\n\n- **2026-10-26**: began\n");
        optimus
            .places
            .ledgers()
            .hold(QUEST)
            .expect("held")
            .keep()
            .expect("shared");
        let (mut near, mut far) = (
            optimus.exchange(&equilibrius),
            equilibrius.exchange(&optimus),
        );
        talk(&mut near, &mut far);

        optimus.write(
            "JOURNEY.md",
            "# Journey\n\n- **2026-10-27**: here\n- **2026-10-26**: began\n",
        );
        equilibrius.write(
            "JOURNEY.md",
            "# Journey\n\n- **2026-10-27**: there\n- **2026-10-26**: began\n",
        );
        talk(&mut near, &mut far);

        let journey = optimus.read("JOURNEY.md").expect("a journey");
        assert!(
            journey.contains("here") && journey.contains("there") && journey.contains("began"),
            "both entries and the old one are kept: {journey}"
        );
        assert_eq!(
            equilibrius.read("JOURNEY.md"),
            Some(journey),
            "both ends hold the same"
        );
    }

    #[test]
    fn a_document_written_on_both_ends_keeps_the_others_beside_and_says_so() {
        let optimus = Machine::new("optimus");
        let equilibrius = Machine::new("equilibrius");
        let plan = "plans/PLAN-20261026-IZH11J.md";
        optimus.write(plan, "one\n");
        optimus.write(
            "README.md",
            "---\nid: QEST-20261013-4UHC5Q\nparent: QEST-20261007-WCJQSG\n---\n",
        );
        for machine in [&optimus, &equilibrius] {
            let above = machine.places.quests.join("QEST-20261007-WCJQSG");
            fs::create_dir_all(&above).expect("made");
            fs::write(above.join("README.md"), "---\nparent: null\n---\n").expect("written");
        }
        optimus
            .places
            .ledgers()
            .hold(QUEST)
            .expect("held")
            .keep()
            .expect("shared");
        let (mut near, mut far) = (
            optimus.exchange(&equilibrius),
            equilibrius.exchange(&optimus),
        );
        talk(&mut near, &mut far);

        optimus.write(plan, "here\n");
        equilibrius.write(plan, "there\n");
        talk(&mut near, &mut far);

        // equilibrius sorts first, so it settles: its own stands, and
        // optimus's is beside it, on its disk alone.
        assert_eq!(
            equilibrius.read(plan).as_deref(),
            Some("there\n"),
            "the settler's stands"
        );
        assert_eq!(
            optimus.read(plan).as_deref(),
            Some("there\n"),
            "and both ends hold it"
        );
        assert_eq!(
            equilibrius.read(&beside(plan, "optimus")).as_deref(),
            Some("here\n"),
            "the other's is kept beside it"
        );
        assert_eq!(
            optimus.read(&beside(plan, "optimus")),
            None,
            "and never crosses back"
        );
        let told = far.findings();
        assert_eq!(told.len(), 1, "the settler tells of it once: {told:?}");
        assert_eq!(
            (
                told[0].from.as_str(),
                told[0].groups[0].as_str(),
                told[0].intent
            ),
            (
                "mycelium@equilibrius",
                "quest.wcjqsg.4uhc5q",
                Some(Intent::Finding)
            ),
            "as a finding to the quest's whole group"
        );
        assert!(
            told[0].text.contains(&beside(plan, "optimus")),
            "naming where the other's is: {}",
            told[0].text
        );
        assert!(near.findings().is_empty(), "the other end tells nothing");
    }

    #[test]
    fn a_deletion_crosses() {
        let optimus = Machine::new("optimus");
        let equilibrius = Machine::new("equilibrius");
        optimus.write("attachments/map.md", "map\n");
        optimus
            .places
            .ledgers()
            .hold(QUEST)
            .expect("held")
            .keep()
            .expect("shared");
        let (mut near, mut far) = (
            optimus.exchange(&equilibrius),
            equilibrius.exchange(&optimus),
        );
        talk(&mut near, &mut far);
        assert!(
            equilibrius.read("attachments/map.md").is_some(),
            "the attachment crossed"
        );

        fs::remove_file(optimus.places.quests.join(QUEST).join("attachments/map.md"))
            .expect("removed");
        talk(&mut near, &mut far);
        assert_eq!(
            equilibrius.read("attachments/map.md"),
            None,
            "and its removal did"
        );
    }

    #[test]
    fn only_the_scratch_folders_named_cross_and_the_far_ends_own_moves_aside() {
        let optimus = Machine::new("optimus");
        let equilibrius = Machine::new("equilibrius");
        let scratch = |machine: &Machine, path: &str| machine.places.scratch.join(QUEST).join(path);
        let put = |machine: &Machine, path: &str, text: &str| {
            let path = scratch(machine, path);
            fs::create_dir_all(path.parent().expect("a file has a folder")).expect("made");
            fs::write(path, text).expect("written");
        };
        optimus.write("README.md", "# Shared\n");
        put(&optimus, "notes/plan.md", "notes\n");
        put(&optimus, "notes/runs/one.json", "{}\n");
        put(&optimus, "hub-home/big.bin", "bulk\n");
        put(&optimus, "tmp/session.log", "a session's own\n");
        put(&equilibrius, "notes/old.md", "the Mac's own notes\n");
        optimus
            .places
            .ledgers()
            .hold(QUEST)
            .expect("held")
            .keep()
            .expect("shared");
        optimus
            .places
            .name_scratch(QUEST, "notes")
            .expect("notes named");

        let (mut near, mut far) = (
            optimus.exchange(&equilibrius),
            equilibrius.exchange(&optimus),
        );
        talk(&mut near, &mut far);
        let read = |machine: &Machine, path: &str| fs::read_to_string(scratch(machine, path)).ok();
        assert_eq!(
            read(&equilibrius, "notes/plan.md").as_deref(),
            Some("notes\n"),
            "the named folder crossed"
        );
        assert_eq!(
            read(&equilibrius, "notes/runs/one.json").as_deref(),
            Some("{}\n"),
            "with what is in it"
        );
        assert_eq!(
            read(&equilibrius, "notes/old.md"),
            None,
            "the far end's own folder moved aside"
        );
        let aside = fs::read_dir(equilibrius.places.state.join("before-share"))
            .expect("something moved aside")
            .map(|entry| entry.expect("an entry").path())
            .find(|at| at.join(QUEST).join("scratch/notes/old.md").exists());
        assert!(aside.is_some(), "where nothing is lost");
        for path in ["hub-home/big.bin", "tmp/session.log"] {
            assert_eq!(read(&equilibrius, path), None, "{path} stayed home");
        }

        // The Mac's look walks its notes once, and watches them from then.
        put(&equilibrius, "notes/plan.md", "notes, from the Mac\n");
        talk(&mut near, &mut far);
        assert_eq!(
            read(&optimus, "notes/plan.md").as_deref(),
            Some("notes, from the Mac\n"),
            "and a change crosses back"
        );
        put(&equilibrius, "notes/runs/two.json", "[]\n");
        assert!(
            far.watch.wait(HEARD_WITHIN),
            "the Mac's watch hears its notes change"
        );
        talk(&mut near, &mut far);
        assert_eq!(
            read(&optimus, "notes/runs/two.json").as_deref(),
            Some("[]\n"),
            "so what the watch heard crosses too"
        );

        put(&optimus, "bench/run.csv", "1,2\n");
        optimus
            .places
            .name_scratch(QUEST, "bench")
            .expect("bench named");
        talk(&mut near, &mut far);
        assert_eq!(
            read(&equilibrius, "bench/run.csv").as_deref(),
            Some("1,2\n"),
            "a folder named once the quest is shared crosses too"
        );
    }

    #[test]
    fn a_change_found_while_hearing_the_other_end_is_still_said() {
        let optimus = Machine::new("optimus");
        let equilibrius = Machine::new("equilibrius");
        optimus.write("attachments/a.md", "a\n");
        optimus.write("attachments/b.md", "b\n");
        optimus
            .places
            .ledgers()
            .hold(QUEST)
            .expect("held")
            .keep()
            .expect("shared");
        let (mut near, mut far) = (
            optimus.exchange(&equilibrius),
            equilibrius.exchange(&optimus),
        );
        talk(&mut near, &mut far);

        // Optimus writes a.md, but hears of the Mac's b.md before it looks.
        optimus.write("attachments/a.md", "a, again\n");
        equilibrius.write("attachments/b.md", "b, from the Mac\n");
        for say in far.look().expect("equilibrius looks") {
            for answer in near.hear(say).expect("optimus hears") {
                for file in far.hear(answer).expect("equilibrius hears") {
                    near.hear(file).expect("optimus hears");
                }
            }
        }
        talk(&mut near, &mut far);
        assert_eq!(
            equilibrius.read("attachments/a.md").as_deref(),
            Some("a, again\n"),
            "optimus's change, found while it heard the Mac, still crossed"
        );
        assert_eq!(
            optimus.read("attachments/b.md").as_deref(),
            Some("b, from the Mac\n"),
            "as the Mac's did"
        );
    }

    #[test]
    fn a_large_file_crosses_a_piece_to_a_frame() {
        let optimus = Machine::new("optimus");
        let equilibrius = Machine::new("equilibrius");
        let at = |machine: &Machine| machine.places.scratch.join(QUEST).join("bench/run.bin");
        let bytes: Vec<u8> = (0..=u8::MAX)
            .cycle()
            .take(usize::try_from(2 * PIECE).expect("a size") + 512 * 1024)
            .collect();
        fs::create_dir_all(at(&optimus).parent().expect("a folder")).expect("made");
        fs::write(at(&optimus), &bytes).expect("written");
        optimus
            .places
            .ledgers()
            .hold(QUEST)
            .expect("held")
            .keep()
            .expect("shared");
        optimus
            .places
            .name_scratch(QUEST, "bench")
            .expect("bench named");
        let (mut near, mut far) = (
            optimus.exchange(&equilibrius),
            equilibrius.exchange(&optimus),
        );

        // Passed by hand, to see each frame: what optimus sends for each
        // thing equilibrius asks.
        let mut to_far = near.look().expect("optimus looks");
        far.look().expect("equilibrius looks");
        let mut pieces = Vec::new();
        for _ in 0..20 {
            let mut to_near = Vec::new();
            for say in to_far.drain(..) {
                if let Say::Piece { at, body, .. } = &say {
                    pieces.push((*at, STANDARD.decode(body).expect("base64").len()));
                }
                assert!(
                    !matches!(&say, Say::File { file, .. } if file.ends_with("run.bin")),
                    "the large file never crosses in one frame"
                );
                to_near.extend(far.hear(say).expect("equilibrius hears"));
            }
            for say in to_near {
                to_far.extend(near.hear(say).expect("optimus hears"));
            }
            if to_far.is_empty() {
                break;
            }
        }
        let piece = usize::try_from(PIECE).expect("a size");
        assert_eq!(
            pieces,
            [(0, piece), (PIECE, piece), (2 * PIECE, 512 * 1024)],
            "it crossed in three pieces, each asked for once the last was staged"
        );
        assert!(
            fs::read(at(&equilibrius)).expect("it crossed") == bytes,
            "and landed whole"
        );
        assert!(
            matches!(
                near.hear(Say::Want {
                    quest: QUEST.to_owned(),
                    file: "scratch/bench/run.bin".to_owned(),
                    at: None,
                })
                .expect("optimus hears")
                .as_slice(),
                [Say::File { body: Some(_), .. }]
            ),
            "an end older than pieces, naming no offset, is sent it whole"
        );
    }

    #[test]
    fn a_clone_with_no_origin_or_a_heavy_folder_is_refused_when_named() {
        let optimus = Machine::new("optimus");
        let put = |path: &str, bytes: &[u8]| {
            let path = optimus.places.scratch.join(QUEST).join(path);
            fs::create_dir_all(path.parent().expect("a file has a folder")).expect("made");
            fs::write(path, bytes).expect("written");
        };
        put("ttc/.git/HEAD", b"ref: refs/heads/main\n");
        put("ttc/src/lib.rs", b"\n");
        for at in 0..=MOST_FILES {
            put(&format!("many/{at}"), b"");
        }
        let named = |folder: &str| optimus.places.name_scratch(QUEST, folder);
        for (folder, why) in [
            ("ttc", "git clone"),
            ("many", "more than"),
            ("tmp", "never tmp/"),
            ("gone", "no folder"),
        ] {
            let error = named(folder).expect_err("refused").to_string();
            assert!(error.contains(why), "{folder} refused as {why}: {error}");
        }
        assert!(
            optimus
                .places
                .ledgers()
                .hold(QUEST)
                .expect("held")
                .ledger
                .scratch
                .is_empty(),
            "and none of them is shared"
        );
    }

    #[test]
    fn a_quest_concluded_is_shared_until_both_ends_know_then_no_longer() {
        let optimus = Machine::new("optimus");
        let equilibrius = Machine::new("equilibrius");
        let scratch = |machine: &Machine, path: &str| machine.places.scratch.join(QUEST).join(path);
        let readme = |status: &str| format!("---\nid: {QUEST}\nstatus: {status}\n---\n# Shared\n");
        let shares = |machine: &Machine| machine.places.ledgers().shares(QUEST);
        optimus.write("README.md", &readme("active"));
        let notes = scratch(&optimus, "notes/plan.md");
        fs::create_dir_all(notes.parent().expect("a file has a folder")).expect("made");
        fs::write(&notes, "notes\n").expect("written");
        optimus
            .places
            .ledgers()
            .hold(QUEST)
            .expect("held")
            .keep()
            .expect("shared");
        optimus.places.name_scratch(QUEST, "notes").expect("named");
        let (mut near, mut far) = (
            optimus.exchange(&equilibrius),
            equilibrius.exchange(&optimus),
        );
        talk(&mut near, &mut far);
        assert!(shares(&equilibrius), "the Mac takes the quest up");

        // Concluding it writes its README and takes optimus's scratch away.
        optimus.write("README.md", &readme("concluded"));
        fs::remove_dir_all(optimus.places.scratch.join(QUEST)).expect("taken away");
        // The README crosses at one look, the Mac stops at its next, and
        // optimus at the one after it hears so.
        for _ in 0..3 {
            talk(&mut near, &mut far);
        }
        assert_eq!(
            equilibrius.read("README.md"),
            Some(readme("concluded")),
            "the Mac learns it concluded"
        );
        assert!(
            !shares(&optimus) && !shares(&equilibrius),
            "and then neither end shares it"
        );
        assert!(
            scratch(&equilibrius, "notes/plan.md").exists(),
            "while the Mac keeps its notes"
        );

        fs::write(scratch(&equilibrius, "notes/plan.md"), "later\n").expect("written");
        for _ in 0..3 {
            talk(&mut near, &mut far);
        }
        assert!(
            !optimus.places.scratch.join(QUEST).exists(),
            "a later change on the Mac never makes optimus's scratch again"
        );
        assert!(
            !shares(&optimus) && !shares(&equilibrius),
            "nor is the quest taken up again"
        );
    }

    #[test]
    fn only_a_readme_saying_so_has_ended() {
        for (readme, ended) in [
            ("---\nid: Q\nstatus: concluded\n---\n# Q\n", true),
            ("---\nstatus: retired\n---\n", true),
            ("---\nstatus: active\n---\nstatus: concluded\n", false),
            ("# Q\nstatus: concluded\n", false),
            ("---\nstatus: concludedish\n---\n", false),
        ] {
            assert_eq!(has_ended(readme), ended, "{readme:?}");
        }
    }

    #[test]
    fn a_scratch_taken_away_whole_is_no_word_on_the_other_machines() {
        let optimus = Machine::new("optimus");
        let equilibrius = Machine::new("equilibrius");
        let scratch = |machine: &Machine, path: &str| machine.places.scratch.join(QUEST).join(path);
        let put = |path: &str, text: &str| {
            let path = scratch(&optimus, path);
            fs::create_dir_all(path.parent().expect("a file has a folder")).expect("made");
            fs::write(path, text).expect("written");
        };
        optimus.write("README.md", "# Shared\n");
        put("notes/plan.md", "notes\n");
        put("notes/old.md", "old notes\n");
        optimus
            .places
            .ledgers()
            .hold(QUEST)
            .expect("held")
            .keep()
            .expect("shared");
        optimus.places.name_scratch(QUEST, "notes").expect("named");
        let (mut near, mut far) = (
            optimus.exchange(&equilibrius),
            equilibrius.exchange(&optimus),
        );
        talk(&mut near, &mut far);
        assert!(
            scratch(&equilibrius, "notes/old.md").exists(),
            "the notes cross"
        );

        fs::remove_file(scratch(&optimus, "notes/old.md")).expect("removed");
        assert!(near.watch.wait(HEARD_WITHIN), "optimus hears it");
        talk(&mut near, &mut far);
        assert!(
            !scratch(&equilibrius, "notes/old.md").exists(),
            "a file removed from a named folder is removed there too"
        );

        // Concluding a quest takes this machine's whole scratch away.
        fs::remove_dir_all(optimus.places.scratch.join(QUEST)).expect("taken away");
        // inotify hears a watched folder go with the folder holding it, so
        // on Linux the look walks it and must find nothing to remove. macOS's
        // FSEvents didn't hear it within the wait in any of 20KSI7's runs on
        // equilibrius (2026-10-29); unheard, the folder isn't walked, which
        // also crosses nothing, so the Mac's copy stays either way.
        let heard = near.watch.wait(HEARD_WITHIN);
        assert!(heard || !cfg!(target_os = "linux"), "optimus hears it");
        talk(&mut near, &mut far);
        assert_eq!(
            fs::read_to_string(scratch(&equilibrius, "notes/plan.md"))
                .ok()
                .as_deref(),
            Some("notes\n"),
            "but the Mac's copy stays when optimus's scratch goes whole"
        );
    }

    #[test]
    fn a_clone_crosses_as_its_remote_and_is_made_where_it_is_lacking() {
        let optimus = Machine::new("optimus");
        let equilibrius = Machine::new("equilibrius");
        let scratch = |machine: &Machine, path: &str| machine.places.scratch.join(QUEST).join(path);
        let put = |path: &str, text: &str| {
            let path = scratch(&optimus, path);
            fs::create_dir_all(path.parent().expect("a file has a folder")).expect("made");
            fs::write(path, text).expect("written");
        };
        let origin = |url: &str| format!("[remote \"origin\"]\n\turl = {url}\n");
        optimus.write("README.md", "# Shared\n");
        put("notes/plan.md", "notes\n");
        put(
            "notes/ttc/.git/config",
            &origin("https://nowhere.invalid/ttc.git"),
        );
        put("notes/ttc/.git/HEAD", "ref: refs/heads/main\n");
        put("notes/ttc/src/lib.rs", "// the clone's own\n");
        put(
            "notes/mine/.git/config",
            &origin("/home/jitsusama/src/mine"),
        );
        put("notes/mine/.git/HEAD", "ref: refs/heads/main\n");
        put(
            "crash/.git/config",
            &origin("git@nowhere.invalid:crash.git"),
        );
        put(
            "crash/.git/HEAD",
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904\n",
        );
        optimus
            .places
            .ledgers()
            .hold(QUEST)
            .expect("held")
            .keep()
            .expect("shared");
        for folder in ["notes", "crash"] {
            optimus.places.name_scratch(QUEST, folder).expect("named");
        }

        let (mut near, mut far) = (
            optimus.exchange(&equilibrius),
            equilibrius.exchange(&optimus),
        );
        talk(&mut near, &mut far);
        assert_eq!(
            fs::read_to_string(scratch(&equilibrius, "notes/plan.md"))
                .ok()
                .as_deref(),
            Some("notes\n"),
            "the notes cross"
        );
        assert!(
            !scratch(&equilibrius, "notes/ttc/src/lib.rs").exists(),
            "but no file of a clone does"
        );
        assert_eq!(
            far.cloning.making(),
            vec![
                scratch(&equilibrius, "crash"),
                scratch(&equilibrius, "notes/ttc")
            ],
            "each clone with a remote over the network is made from it instead, \
             and one from a path on optimus's disk is left out"
        );
        far.cloning.wait();
    }

    #[test]
    fn a_file_crosses_with_who_may_read_it() {
        use std::os::unix::fs::PermissionsExt;
        let mode_of = |machine: &Machine, path: &str| {
            fs::metadata(machine.places.quests.join(QUEST).join(path))
                .expect("there")
                .permissions()
                .mode()
                & 0o777
        };
        let optimus = Machine::new("optimus");
        let equilibrius = Machine::new("equilibrius");
        optimus.write("attachments/hub.env", "TOKEN=1\n");
        fs::set_permissions(
            optimus
                .places
                .quests
                .join(QUEST)
                .join("attachments/hub.env"),
            fs::Permissions::from_mode(0o600),
        )
        .expect("made the owner's alone");
        optimus
            .places
            .ledgers()
            .hold(QUEST)
            .expect("held")
            .keep()
            .expect("shared");
        let (mut near, mut far) = (
            optimus.exchange(&equilibrius),
            equilibrius.exchange(&optimus),
        );
        talk(&mut near, &mut far);
        assert_eq!(
            mode_of(&equilibrius, "attachments/hub.env"),
            0o600,
            "it lands as only its owner may read it"
        );

        optimus.write("attachments/hub.env", "TOKEN=2\n");
        talk(&mut near, &mut far);
        assert_eq!(
            equilibrius.read("attachments/hub.env").as_deref(),
            Some("TOKEN=2\n"),
            "a change crosses"
        );
        assert_eq!(
            mode_of(&equilibrius, "attachments/hub.env"),
            0o600,
            "still as only its owner may read it"
        );
    }
}
