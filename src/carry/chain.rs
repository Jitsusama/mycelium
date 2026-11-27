//! Who orchestrates which quests, as this node recorded it when each
//! orchestration was made, never as a session says it later.
//!
//! An orchestrator's session tells its node, over its own connection,
//! which quest it heads and which quests it orchestrates, once it loads
//! the quest and again whenever those change. The node proves which
//! session asks, from the kernel and pi's own records, and keeps each
//! claim in a file of its own. A quest another session already holds by
//! that record is refused: only its session hands it on, by releasing it
//! or by ending once continued, as a session rolled over into a new log
//! that names it as its `parentSession`; a claim whose session no longer
//! stands passes to the one that continues it, whether or not that one
//! tells the node again, and a fork of a session still running takes
//! nothing of its. An ordered carry's chain of
//! orchestrators is walked through this record, so neither a rewritten
//! quests' index nor an edited quest README moves it.
//!
//! Every file here is one any process of the person's account can write,
//! so the record holds against a fenced session only where the fence
//! keeps it read-only. Who asks is proved as far as the kernel and pi's
//! records go: a session's log held open by its process says "this
//! process says so", which stops a confused helper, not a hostile pi.
//!
//! Long because its tests, three fifths of it, walk each way a claim
//! is made, refused, handed on, continued and forked against the one
//! record they prove, and read best beside it.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::ordered::{Known, Link, Order};
use super::shown;

/// Says where an ordered carry's chain came from, for its record.
pub const SOURCE: &str = "this node's record of orchestrations, checked against the quests' \
                          record and the mesh's orchestration groups";

/// Every orchestration this node recorded, by the orchestrating session's
/// id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Orchestrations {
    /// Each orchestrating session's claim, by its id.
    pub sessions: BTreeMap<String, Claim>,
}

/// What one orchestrating session claimed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    /// The address it goes by, as the mesh names it.
    pub address: String,
    /// The quest it heads, the orchestrating one it has loaded.
    pub heads: String,
    /// The quests it orchestrates, as it last said them.
    pub quests: BTreeSet<String>,
    /// When it last said them.
    pub at: jiff::Timestamp,
}

impl Orchestrations {
    /// The record kept at `file`, or none yet where nothing is kept.
    ///
    /// # Errors
    ///
    /// When the file can't be read, or isn't in this shape: a record
    /// that can't be read proves no chain.
    pub fn read(file: &Path) -> io::Result<Self> {
        match fs::read(file) {
            Ok(text) => serde_json::from_slice(&text).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error),
        }
    }

    /// Keeps the record at `file`, whole, so no reader sees half of it.
    ///
    /// # Errors
    ///
    /// When the file can't be written.
    pub fn keep(&self, file: &Path) -> io::Result<()> {
        if let Some(folder) = file.parent() {
            fs::create_dir_all(folder)?;
        }
        let fresh = file.with_extension("json.new");
        fs::write(
            &fresh,
            serde_json::to_vec_pretty(self).map_err(io::Error::other)?,
        )?;
        fs::rename(fresh, file)
    }

    /// Records that `asker` heads `heads` and orchestrates `quests`, as of
    /// `at`, in place of what it claimed before. Refused, changing
    /// nothing, where another session holds any of them.
    ///
    /// # Errors
    ///
    /// When another session holds one of the quests.
    pub fn claim(
        &mut self,
        asker: Known,
        heads: &str,
        quests: &[String],
        at: jiff::Timestamp,
    ) -> Result<(), String> {
        // What the asker's log continues is never its own here: a claim
        // passes to its continuer only once its session no longer stands
        // (`lapse`), so a fork of a session still running takes nothing
        // of its.
        let own = |session: &str| session == asker.session;
        let held = |quest: &str, heading: bool| {
            self.sessions.iter().find(|(session, claim)| {
                !own(session)
                    && if heading {
                        claim.heads == quest
                    } else {
                        claim.quests.contains(quest)
                    }
            })
        };
        let claimed = std::iter::once((heads, true))
            .chain(quests.iter().map(|quest| (quest.as_str(), false)));
        for (quest, heading) in claimed {
            if let Some((_, holder)) = held(quest, heading) {
                let what = if heading { "heads" } else { "orchestrates" };
                return Err(format!(
                    "{}'s session already {what} {quest} by this node's record, so {} can't: \
                     only that session hands it on, by letting go or by ending once a session \
                     continues its log, or the person",
                    holder.address, asker.address
                ));
            }
        }
        self.sessions.insert(
            asker.session.to_owned(),
            Claim {
                address: asker.address.to_owned(),
                heads: heads.to_owned(),
                quests: quests.iter().cloned().collect(),
                at,
            },
        );
        Ok(())
    }

    /// Forgets what `session` claimed, as it unloads or concludes its
    /// quest; it claimed nothing more.
    pub fn release(&mut self, session: &str) {
        self.sessions.remove(session);
    }

    /// Frees `quest` from whichever session holds it, as the person asks,
    /// answering that session's address: one that heads it lets go of
    /// everything it claimed, since what it orchestrates hangs from it,
    /// and one that orchestrates it lets go of that quest alone.
    ///
    /// # Errors
    ///
    /// When no session holds `quest`, so nothing is freed.
    pub fn free(&mut self, quest: &str) -> Result<String, String> {
        if let Some(session) = self
            .sessions
            .iter()
            .find(|(_, claim)| claim.heads == quest)
            .map(|(session, _)| session.clone())
        {
            return Ok(self
                .sessions
                .remove(&session)
                .map(|claim| claim.address)
                .unwrap_or_default());
        }
        self.sessions
            .values_mut()
            .find_map(|claim| claim.quests.remove(quest).then(|| claim.address.clone()))
            .ok_or_else(|| {
                format!("no session holds {quest} by this node's record, so nothing is freed")
            })
    }

    /// Hands on or forgets what each session claimed that no longer runs
    /// or waits parked, as `standing` names those that do, each with the
    /// address the node knows it by, where it knows one.
    ///
    /// A lapsing claim passes, keeping what it heads and its quests, to
    /// the one standing session whose log continues the claimer's, as
    /// `continues` tells what each session's log continues: directly, or
    /// through logs that claimed nothing and no longer stand, up to
    /// [`CONTINUED_MOST`] logs back. It takes that session's address
    /// where the node knows it, else keeps its own. A session that has
    /// claimed for itself takes nothing, as its own word stands, and a
    /// claim two standing sessions continue passes to neither, since
    /// which goes on can't be told. Any other lapses, so an orchestrator
    /// started afresh, which continues no session, may claim its quests
    /// again. `continues` is asked only where some claim lapses, so the
    /// logs are read only then, and once. Where it gives nothing, as the
    /// logs folder can't be read, each lapsing claim is kept as it is
    /// until a later lapse can tell where it goes, rather than lost or
    /// handed to a planted log's session. Where it can't tell what a log
    /// followed back continues, as two logs named for one session name
    /// different sessions, each is followed, and only a lapsing claim
    /// one of them may lead to is kept so; the rest pass or lapse as
    /// they would. A log that can't be opened or read as a header still
    /// reads as continuing none, so a copy that can't be read agrees
    /// with one naming none and disagrees with one naming a session.
    ///
    /// Answers what became of each claim that lapsed, so a record that
    /// keeps the change can say so: [`Lapsed::Handed`] to the session
    /// that took it, [`Lapsed::Held`] where a log that can't be told
    /// may continue it, or [`Lapsed::Forgotten`]. A claim kept as the
    /// logs folder can't be read is in none of them.
    pub fn lapse<Continues: Fn(&str) -> Continued>(
        &mut self,
        standing: &BTreeMap<String, Option<String>>,
        continues: impl FnOnce() -> Option<Continues>,
    ) -> Vec<Lapsed> {
        let (kept, lapsing): (BTreeMap<_, _>, BTreeMap<_, _>) = std::mem::take(&mut self.sessions)
            .into_iter()
            .partition(|(session, _)| standing.contains_key(session));
        self.sessions = kept;
        if lapsing.is_empty() {
            return Vec::new();
        }
        let Some(continues) = continues() else {
            self.sessions.extend(lapsing);
            return Vec::new();
        };
        let (heirs, doubted) = heirs(&lapsing, standing, &continues);
        let mut lapsed = Vec::new();
        for (claimer, claim) in &lapsing {
            // Kept by the claimer it was, which no longer stands, so no
            // standing session's own claim is overwritten.
            if let Some(doubting) = doubted.get(claimer.as_str()) {
                self.sessions.insert(claimer.clone(), claim.clone());
                lapsed.push(Lapsed::Held {
                    claimer: claimer.clone(),
                    heads: claim.heads.clone(),
                    doubting: doubting.clone(),
                });
                continue;
            }
            let forgotten = |why: String| Lapsed::Forgotten {
                claimer: claimer.clone(),
                heads: claim.heads.clone(),
                why,
            };
            let heir = match heirs.get(claimer.as_str()).map(Vec::as_slice) {
                None | Some([]) => {
                    lapsed.push(forgotten("no standing session continues it".to_owned()));
                    continue;
                }
                Some([heir]) => *heir,
                Some(several) => {
                    lapsed.push(forgotten(format!(
                        "{} standing sessions continue it, so which goes on can't be told",
                        several.len()
                    )));
                    continue;
                }
            };
            // Its own claim stands, and isn't overwritten by the one it
            // continues.
            if self.sessions.contains_key(heir) {
                lapsed.push(forgotten(format!(
                    "{heir}, which continues it, holds a claim of its own"
                )));
                continue;
            }
            let address = standing
                .get(heir)
                .cloned()
                .flatten()
                .unwrap_or_else(|| claim.address.clone());
            self.sessions.insert(
                heir.to_owned(),
                Claim {
                    address,
                    ..claim.clone()
                },
            );
            lapsed.push(Lapsed::Handed {
                claimer: claimer.clone(),
                heir: heir.to_owned(),
                heads: claim.heads.clone(),
            });
        }
        lapsed
    }

    /// The quest the session `session` heads, by this record.
    #[must_use]
    pub fn heads(&self, session: &str) -> Option<&str> {
        self.sessions.get(session).map(|claim| claim.heads.as_str())
    }

    /// Who orchestrates `quest` up to `asker`, by this record: the
    /// session that orchestrates it, then the one orchestrating the quest
    /// that session heads, and so on, until one is `asker`. Refused,
    /// saying where the record stops, when none is.
    ///
    /// # Errors
    ///
    /// When the record doesn't lead from `quest` to `asker`.
    pub fn chain(&self, quest: &str, asker: Known) -> Result<Order, String> {
        let mut links = Vec::new();
        let mut passed = BTreeSet::new();
        let mut at = quest;
        while passed.insert(at) {
            let Some((session, claim)) = self
                .sessions
                .iter()
                .find(|(_, claim)| claim.quests.contains(at))
            else {
                break;
            };
            links.push(Link {
                quest: at.to_owned(),
                orchestrator: claim.address.clone(),
            });
            if session == asker.session {
                return Ok(Order {
                    by: claim.address.clone(),
                    quest: quest.to_owned(),
                    chain: links,
                });
            }
            at = &claim.heads;
        }
        if links.is_empty() {
            return Err(format!(
                "no orchestrator has told this node it orchestrates {quest}, so only the \
                 person carries it"
            ));
        }
        let could: Vec<String> = links
            .iter()
            .map(|link| format!("{}'s session", link.orchestrator))
            .collect();
        Err(format!(
            "{} doesn't orchestrate {quest} by this node's record, so it can't order its \
             carry: only {} could",
            asker.address,
            could.join(" or ")
        ))
    }
}

/// Each standing session continuing a lapsing claim, by the claimer, as
/// [`Orchestrations::lapse`] follows each back through `continues`, and
/// each claimer a log that can't be told may lead to, with the session
/// whose logs those are, the first a walk met. Every standing
/// session that continues a claim counts, one that claimed for itself
/// too: that it continues the claimer still makes which goes on unclear
/// to tell.
fn heirs<'a>(
    lapsing: &'a BTreeMap<String, Claim>,
    standing: &'a BTreeMap<String, Option<String>>,
    continues: &impl Fn(&str) -> Continued,
) -> (BTreeMap<&'a str, Vec<&'a str>>, BTreeMap<&'a str, String>) {
    let mut heirs: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut doubted = BTreeMap::new();
    for session in standing.keys() {
        // Each log reached, and the first session whose logs can't be
        // told that led there: one path, until a log's continuation
        // can't be told.
        let mut reached: BTreeSet<(String, Option<String>)> =
            BTreeSet::from([(session.clone(), None)]);
        for _ in 0..CONTINUED_MOST {
            let mut next = BTreeSet::new();
            for (at, doubting) in reached {
                let (before, doubting): (Vec<String>, Option<String>) = match continues(&at) {
                    Ok(before) => (before.into_iter().collect(), doubting),
                    Err(may) => (may, Some(doubting.unwrap_or_else(|| at.clone()))),
                };
                for before in before {
                    if let Some((claimer, _)) = lapsing.get_key_value(&before) {
                        if let Some(doubting) = &doubting {
                            doubted
                                .entry(claimer.as_str())
                                .or_insert_with(|| doubting.clone());
                        } else {
                            heirs.entry(claimer).or_default().push(session);
                        }
                    } else if !standing.contains_key(&before) {
                        // A standing session's claim, or its having none,
                        // is its own: what it continues isn't handed past
                        // it, so the walk goes on only past one that
                        // doesn't stand.
                        next.insert((before, doubting.clone()));
                    }
                }
            }
            reached = next;
        }
    }
    (heirs, doubted)
}

/// How many logs back a lapsing claim is followed to the session that
/// goes on: a session rolls over into a new log only now and then, so a
/// chain longer than this is taken as no continuation.
pub const CONTINUED_MOST: usize = 8;

/// The sessions' logs under a logs folder, each in a folder of its working
/// folder's, `<logs>/<folder>/<time>_<session>.jsonl`, found by the
/// session each keeps in one scan of the folders, opening none: a log is
/// opened only as what it continues is asked, only its first line is
/// read, and each session's logs are read once a scan.
#[derive(Debug, Default)]
pub struct Logs {
    /// Each log found, by the session its name says it keeps: all of
    /// them, where several are named for one session, as a log planted
    /// under another folder can't be told from its own. Each session's
    /// are in their paths' order, not the order the folders read, so
    /// what [`Self::disagreeing`] says of them is the same every scan.
    found: BTreeMap<String, Vec<PathBuf>>,
    /// Each folder in it that couldn't be read, and why, passed over.
    unread: Vec<String>,
    /// What each session asked of [`Self::follows`] continues, as its
    /// logs said the first time, so a lapse walking past it again reads
    /// none of them twice.
    told: RefCell<BTreeMap<String, Continued>>,
}

impl Logs {
    /// The logs under `logs`, as they are now; none where it isn't
    /// there, as on a machine pi never ran on. A folder gone between
    /// the scan's steps is taken as gone, and a file beside the folders
    /// is passed over. A folder in it that can't be read is passed over
    /// too, named in [`Self::unread`], so one stray folder costs only
    /// the logs it holds, never every claim's.
    ///
    /// # Errors
    ///
    /// Where `logs` is there but can't be read, so which log continues
    /// which can't be told: read as none, a claim that should pass on
    /// would lapse instead.
    pub fn scan(logs: &Path) -> io::Result<Self> {
        let mut scanned = Self::default();
        let folders = match fs::read_dir(logs) {
            Ok(folders) => folders,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(scanned),
            Err(error) => return Err(error),
        };
        for folder in folders {
            let folder = folder?.path();
            let read = fs::read_dir(&folder).and_then(|files| {
                files
                    .map(|file| file.map(|file| (file.file_name(), file.path())))
                    .collect::<io::Result<Vec<_>>>()
            });
            let files = match read {
                Ok(files) => files,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue;
                }
                Err(error) => {
                    scanned
                        .unread
                        .push(format!("{}: {error}", folder.display()));
                    continue;
                }
            };
            for (name, path) in files {
                if let Some(session) = name.to_str().and_then(session_of_log) {
                    scanned
                        .found
                        .entry(session.to_owned())
                        .or_default()
                        .push(path);
                }
            }
        }
        for found in scanned.found.values_mut() {
            found.sort();
        }
        Ok(scanned)
    }

    /// Each folder the scan passed over as it couldn't be read, with why.
    #[must_use]
    pub fn unread(&self) -> &[String] {
        &self.unread
    }

    /// The session the log of `session` continues, as its own first line
    /// names it. None where it has no log here, or two that disagree,
    /// where that line names another session, or where it continues none.
    #[must_use]
    pub fn continues(&self, session: &str) -> Option<String> {
        self.follows(session).ok().flatten()
    }

    /// What the log of `session` continues, as [`Self::continues`] tells
    /// it. Where several logs are named for it, each is read, and what
    /// they all say is what it continues: a copy of its own log leaves
    /// nothing to tell apart.
    ///
    /// # Errors
    ///
    /// Where the logs named for `session` disagree, so which is its own,
    /// and what it continues, can't be told: each session one of them
    /// names.
    pub fn follows(&self, session: &str) -> Continued {
        if let Some(told) = self.told.borrow().get(session) {
            return told.clone();
        }
        let continued = self.found.get(session).map_or(Ok(None), |found| {
            let named: BTreeSet<Option<String>> = found
                .iter()
                .map(|log| Self::header_continues(log, session))
                .collect();
            if named.len() > 1 {
                return Err(named.into_iter().flatten().collect());
            }
            Ok(named.into_iter().next().flatten())
        });
        self.told
            .borrow_mut()
            .insert(session.to_owned(), continued.clone());
        continued
    }

    /// The sentence naming each log of `session`, by its path, where they
    /// don't all say what it continues ([`Self::follows`]), so the copy
    /// that isn't its own can be found; none where they agree.
    #[must_use]
    pub fn disagreeing(&self, session: &str) -> Option<String> {
        self.follows(session).err()?;
        let found = self.found.get(session)?;
        let paths: Vec<String> = found
            .iter()
            .map(|log| shown(&log.display().to_string()))
            .collect();
        Some(format!(
            "the {} logs named for {} don't all say what it continues, so a claim it may \
             lead to is kept until the one that isn't its own is moved: {}",
            found.len(),
            shown(session),
            paths.join(", ")
        ))
    }

    /// What the log at `log` continues, as its first line names it, where
    /// that line names `session` as its own.
    fn header_continues(log: &Path, session: &str) -> Option<String> {
        let header: serde_json::Value = serde_json::from_str(&first_line(log)?).ok()?;
        (header.get("id")?.as_str()? == session)
            .then(|| header.get("parentSession")?.as_str().map(parent_of))
            .flatten()
    }

    /// What the log of each of `sessions` continues, as
    /// [`Self::continues`] tells it, leaving out each that continues none.
    #[must_use]
    pub fn continued(&self, sessions: &[&str]) -> BTreeMap<String, String> {
        sessions
            .iter()
            .filter_map(|session| Some(((*session).to_owned(), self.continues(session)?)))
            .collect()
    }
}

/// What a session's log continues: the session or none, or, where its
/// logs disagree, each session one of them names.
pub type Continued = Result<Option<String>, Vec<String>>;

/// What became of a claim whose session no longer stands, as
/// [`Orchestrations::lapse`] tells it. Said in the node's log, so each
/// name in it is escaped: what a quest heads is the asker's word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lapsed {
    /// Passed whole to `heir`, the one standing session continuing it.
    Handed {
        /// The session that claimed it.
        claimer: String,
        /// The session that holds it now.
        heir: String,
        /// The quest it heads.
        heads: String,
    },
    /// Kept as it was, by the session that claimed it, as a log that
    /// can't be told apart from another may continue it: neither handed
    /// on nor lost until a later lapse can tell.
    Held {
        /// The session that claimed it.
        claimer: String,
        /// The quest it heads.
        heads: String,
        /// The session whose logs don't all say what it continues, the
        /// first such a walk back to the claim met, so the copies can be
        /// found ([`Logs::disagreeing`]).
        doubting: String,
    },
    /// Let go of, for the reason `why`.
    Forgotten {
        /// The session that claimed it.
        claimer: String,
        /// The quest it headed.
        heads: String,
        /// Why no session took it.
        why: String,
    },
}

impl std::fmt::Display for Lapsed {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Handed {
                claimer,
                heir,
                heads,
            } => write!(
                out,
                "the claim to head {} passed from {}, which no longer stands, to {}, whose log continues it",
                shown(heads),
                shown(claimer),
                shown(heir)
            ),
            Self::Held {
                claimer,
                heads,
                doubting,
            } => write!(
                out,
                "the claim to head {} is kept for {}, which no longer stands, as the logs named \
                 for {}, which may continue it, don't all say what it continues (one that \
                 can't be read says none)",
                shown(heads),
                shown(claimer),
                shown(doubting)
            ),
            Self::Forgotten {
                claimer,
                heads,
                why,
            } => write!(
                out,
                "the claim to head {} lapsed, as {} no longer stands and {}",
                shown(heads),
                shown(claimer),
                shown(why)
            ),
        }
    }
}

/// The most of a log's first line read: pi's header is a few hundred
/// bytes, so one longer is cut here, and isn't read on; cut, it is
/// taken as a header only where what was read is whole JSON.
const FIRST_LINE_MOST: u64 = 64 * 1024;

/// The first line of the log at `path`, up to [`FIRST_LINE_MOST`] bytes.
/// None where it isn't a plain file or can't be read: it is opened
/// without waiting, so a FIFO named as a log never holds the node for a
/// writer, and a device is never read from.
fn first_line(path: &Path) -> Option<String> {
    use io::{BufRead as _, Read as _};
    use rustix::fs::{Mode, OFlags};
    let log = fs::File::from(
        rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .ok()?,
    );
    if !log.metadata().ok()?.is_file() {
        return None;
    }
    let mut first = String::new();
    io::BufReader::new(log.take(FIRST_LINE_MOST))
        .read_line(&mut first)
        .ok()?;
    Some(first)
}

/// The session a log file named `name` holds, as pi names it:
/// `<time>_<session>.jsonl`.
fn session_of_log(name: &str) -> Option<&str> {
    let (_, session) = name.strip_suffix(".jsonl")?.split_once('_')?;
    Some(session)
}

/// The session a log's `parentSession` names. pi writes the parent's log
/// as a path, whose file name holds its session; a bare session is taken
/// as it is.
fn parent_of(named: &str) -> String {
    Path::new(named)
        .file_name()
        .and_then(|name| session_of_log(name.to_str()?))
        .unwrap_or(named)
        .to_owned()
}

/// Whether the chain the quests' record gives agrees with the node's.
///
/// # Errors
///
/// When they name different orchestrators.
pub fn agrees(node: &Order, record: &Order) -> Result<(), String> {
    let named = |order: &Order| -> Vec<String> {
        order
            .chain
            .iter()
            .map(|link| link.orchestrator.clone())
            .collect()
    };
    if node.chain == record.chain && node.by == record.by {
        return Ok(());
    }
    Err(format!(
        "the quests' record gives {}'s chain as {}, but this node's record gives it as {}, so \
         the chain isn't believed",
        node.quest,
        named(record).join(" then "),
        named(node).join(" then ")
    ))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;

    use super::{CONTINUED_MOST, Continued, Lapsed, Logs, Orchestrations, agrees};
    use crate::carry::ordered::{Known, Link, Order};

    const STEWARD: Known = Known {
        session: "s-steward",
        address: "WCJQSG",
    };
    const WEIGH: Known = Known {
        session: "s-weigh",
        address: "OSPHT4",
    };

    fn at() -> jiff::Timestamp {
        "2026-11-15T17:00:00Z".parse().expect("a time")
    }

    fn quests(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    /// The steward heads WCJQSG's quest and orchestrates OSPHT4's; OSPHT4
    /// heads its own and orchestrates HARBOR's.
    fn nested() -> Orchestrations {
        let mut record = Orchestrations::default();
        record
            .claim(
                STEWARD,
                "QEST-20261007-WCJQSG",
                &quests(&["QEST-20261111-OSPHT4"]),
                at(),
            )
            .expect("the steward claims");
        record
            .claim(
                WEIGH,
                "QEST-20261111-OSPHT4",
                &quests(&["QEST-20261115-HARBOR"]),
                at(),
            )
            .expect("OSPHT4 claims");
        record
    }

    fn link(quest: &str, orchestrator: &str) -> Link {
        Link {
            quest: quest.to_owned(),
            orchestrator: orchestrator.to_owned(),
        }
    }

    #[test]
    fn the_chain_runs_up_what_each_orchestrator_claimed() {
        let record = nested();
        assert_eq!(
            record.chain("QEST-20261115-HARBOR", WEIGH),
            Ok(Order {
                by: "OSPHT4".to_owned(),
                quest: "QEST-20261115-HARBOR".to_owned(),
                chain: vec![link("QEST-20261115-HARBOR", "OSPHT4")],
            }),
            "the nearest orchestrator orders"
        );
        assert_eq!(
            record.chain("QEST-20261115-HARBOR", STEWARD),
            Ok(Order {
                by: "WCJQSG".to_owned(),
                quest: "QEST-20261115-HARBOR".to_owned(),
                chain: vec![
                    link("QEST-20261115-HARBOR", "OSPHT4"),
                    link("QEST-20261111-OSPHT4", "WCJQSG"),
                ],
            }),
            "and so does the one above it"
        );
    }

    #[test]
    fn a_session_no_orchestrator_claimed_above_it_orders_nothing() {
        let record = nested();
        assert_eq!(
            record.chain("QEST-20261111-OSPHT4", WEIGH),
            Err(
                "OSPHT4 doesn't orchestrate QEST-20261111-OSPHT4 by this node's record, so it \
                 can't order its carry: only WCJQSG's session could"
                    .to_owned()
            ),
            "a quest's own session doesn't order its carry"
        );
        assert_eq!(
            record.chain("QEST-20261115-ELSEWH", STEWARD),
            Err(
                "no orchestrator has told this node it orchestrates QEST-20261115-ELSEWH, so \
                 only the person carries it"
                    .to_owned()
            ),
        );
    }

    #[test]
    fn a_quest_another_session_holds_isnt_taken() {
        let mut record = nested();
        let before = record.clone();
        let helper = Known {
            session: "s-helper",
            address: "HARBOR",
        };
        assert_eq!(
            record.claim(
                helper,
                "QEST-20261115-HARBOR",
                &quests(&["QEST-20261111-OSPHT4"]),
                at(),
            ),
            Err(
                "WCJQSG's session already orchestrates QEST-20261111-OSPHT4 by this node's \
                 record, so HARBOR can't: only that session hands it on, by letting go or by \
                 ending once a session continues its log, or the person"
                    .to_owned()
            ),
            "a session claiming its own orchestrator's quest is refused"
        );
        assert_eq!(
            record.claim(helper, "QEST-20261007-WCJQSG", &[], at()),
            Err(
                "WCJQSG's session already heads QEST-20261007-WCJQSG by this node's record, so \
                 HARBOR can't: only that session hands it on, by letting go or by ending once a \
                 session continues its log, or the person"
                    .to_owned()
            ),
            "and so is one claiming to head another's quest"
        );
        assert_eq!(record, before, "a refusal changes nothing");
    }

    #[test]
    fn an_orchestrator_says_its_quests_again_and_releases_them() {
        let mut record = nested();
        record
            .claim(
                STEWARD,
                "QEST-20261007-WCJQSG",
                &quests(&["QEST-20261111-OSPHT4", "QEST-20261115-5HJZ8Z"]),
                at(),
            )
            .expect("the steward says its quests again");
        assert_eq!(
            record.sessions.get("s-steward").map(|claim| &claim.quests),
            Some(&BTreeSet::from([
                "QEST-20261111-OSPHT4".to_owned(),
                "QEST-20261115-5HJZ8Z".to_owned()
            ])),
            "in place of what it said before"
        );
        record.release("s-weigh");
        assert!(
            record.chain("QEST-20261115-HARBOR", STEWARD).is_err(),
            "a released orchestration leads nowhere"
        );
        let helper = Known {
            session: "s-helper",
            address: "OSPHT4",
        };
        record
            .claim(
                helper,
                "QEST-20261111-OSPHT4",
                &quests(&["QEST-20261115-HARBOR"]),
                at(),
            )
            .expect("and another session may take it up");
    }

    /// The sessions that stand, each with the address the node knows it
    /// by, where it knows one.
    fn standing(sessions: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        sessions
            .iter()
            .map(|(session, address)| ((*session).to_owned(), address.map(str::to_owned)))
            .collect()
    }

    /// What each log continues, as `(session, continued)` pairs say.
    fn logs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(session, continued)| ((*session).to_owned(), (*continued).to_owned()))
            .collect()
    }

    /// Lapses `record` as `standing` and the logs' `continued` say,
    /// answering whether any log was read.
    fn lapsed(
        record: &mut Orchestrations,
        standing: &BTreeMap<String, Option<String>>,
        continued: &BTreeMap<String, String>,
    ) -> bool {
        let read = Cell::new(false);
        record.lapse(standing, || {
            read.set(true);
            Some(reading(continued))
        });
        read.get()
    }

    /// What each log continues, as `continued` says, every log told.
    fn reading(continued: &BTreeMap<String, String>) -> impl Fn(&str) -> Continued + '_ {
        |session: &str| Ok(continued.get(session).cloned())
    }

    #[test]
    fn a_claim_lapses_once_its_session_has_ended() {
        let mut record = nested();
        lapsed(
            &mut record,
            &standing(&[("s-weigh", Some("OSPHT4"))]),
            &BTreeMap::new(),
        );
        assert_eq!(
            record.sessions.keys().collect::<Vec<_>>(),
            ["s-weigh"],
            "only what a standing session claimed is kept"
        );
    }

    #[test]
    fn no_log_is_read_where_no_claim_lapses() {
        let mut record = nested();
        let before = record.clone();
        assert!(
            !lapsed(
                &mut record,
                &standing(&[("s-steward", Some("WCJQSG")), ("s-weigh", Some("OSPHT4"))]),
                &logs(&[("s-weigh", "s-steward")]),
            ),
            "every claim's session stands, so no log is read"
        );
        assert_eq!(record, before, "and nothing changes");
    }

    #[test]
    fn a_lapsing_claim_passes_to_the_session_whose_log_continues_it() {
        let mut record = nested();
        let was = record.sessions.get("s-steward").cloned().expect("claimed");
        assert!(lapsed(
            &mut record,
            &standing(&[("s-steward-2", Some("WCJQSG")), ("s-weigh", Some("OSPHT4"))]),
            &logs(&[("s-steward-2", "s-steward")]),
        ));
        assert!(
            !record.sessions.contains_key("s-steward"),
            "the ended session holds nothing"
        );
        assert_eq!(
            record.sessions.get("s-steward-2"),
            Some(&was),
            "its continuation holds its quests, what it heads and its address"
        );
        let rolled = Known {
            session: "s-steward-2",
            address: "WCJQSG",
        };
        assert_eq!(
            record
                .chain("QEST-20261115-HARBOR", rolled)
                .map(|order| order.by),
            Ok("WCJQSG".to_owned()),
            "so the chain still leads to it"
        );
    }

    #[test]
    fn what_became_of_each_lapsing_claim_is_answered() {
        let mut record = nested();
        let continued = logs(&[("s-steward-2", "s-steward")]);
        let said = record.lapse(&standing(&[("s-steward-2", Some("WCJQSG"))]), || {
            Some(reading(&continued))
        });
        assert_eq!(
            said,
            [
                Lapsed::Handed {
                    claimer: "s-steward".to_owned(),
                    heir: "s-steward-2".to_owned(),
                    heads: "QEST-20261007-WCJQSG".to_owned(),
                },
                Lapsed::Forgotten {
                    claimer: "s-weigh".to_owned(),
                    heads: "QEST-20261111-OSPHT4".to_owned(),
                    why: "no standing session continues it".to_owned(),
                },
            ]
        );
        assert_eq!(
            said[0].to_string(),
            "the claim to head QEST-20261007-WCJQSG passed from s-steward, which no longer \
             stands, to s-steward-2, whose log continues it"
        );

        let mut twice = nested();
        let continued = logs(&[("s-a", "s-weigh"), ("s-b", "s-weigh")]);
        let said = twice.lapse(
            &standing(&[("s-steward", Some("WCJQSG")), ("s-a", None), ("s-b", None)]),
            || Some(reading(&continued)),
        );
        assert_eq!(
            said.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "the claim to head QEST-20261111-OSPHT4 lapsed, as s-weigh no longer stands and \
              2 standing sessions continue it, so which goes on can't be told"
            ]
        );

        let mut unread = nested();
        assert_eq!(
            unread.lapse(&BTreeMap::new(), || None::<fn(&str) -> Continued>),
            [],
            "a claim kept as the logs can't be read is in neither"
        );
    }

    #[test]
    fn a_lapsing_claim_passes_through_a_log_that_never_claimed() {
        let mut record = nested();
        let was = record.sessions.get("s-steward").cloned().expect("claimed");
        lapsed(
            &mut record,
            &standing(&[("s-steward-3", None), ("s-weigh", Some("OSPHT4"))]),
            &logs(&[("s-steward-3", "s-steward-2"), ("s-steward-2", "s-steward")]),
        );
        assert_eq!(
            record.sessions.get("s-steward-3"),
            Some(&was),
            "the gap, which neither claimed nor stands, passes it on, and where the node \
             knows no address the claim keeps its own"
        );
        assert_eq!(record.sessions.len(), 2, "and none is held twice");
    }

    #[test]
    fn a_lapsing_claim_takes_the_address_its_continuation_goes_by() {
        let mut record = nested();
        lapsed(
            &mut record,
            &standing(&[("s-steward-2", Some("KESTRL")), ("s-weigh", None)]),
            &logs(&[("s-steward-2", "s-steward")]),
        );
        assert_eq!(
            record
                .sessions
                .get("s-steward-2")
                .map(|claim| claim.address.as_str()),
            Some("KESTRL")
        );
    }

    #[test]
    fn a_continuation_too_long_takes_nothing() {
        let mut record = nested();
        let mut pairs: Vec<(String, String)> = (0..CONTINUED_MOST)
            .map(|at| (format!("s-roll-{}", at + 1), format!("s-roll-{at}")))
            .collect();
        pairs.push(("s-roll-0".to_owned(), "s-steward".to_owned()));
        let continued: BTreeMap<String, String> = pairs.into_iter().collect();
        let last = format!("s-roll-{CONTINUED_MOST}");
        lapsed(
            &mut record,
            &standing(&[(&last, None), ("s-weigh", None)]),
            &continued,
        );
        assert_eq!(
            record.sessions.keys().collect::<Vec<_>>(),
            ["s-weigh"],
            "a walk deeper than {CONTINUED_MOST} logs is given up"
        );
        let mut record = nested();
        lapsed(
            &mut record,
            &standing(&[("s-roll-1", None), ("s-weigh", None)]),
            &continued,
        );
        assert!(
            record.sessions.contains_key("s-roll-1"),
            "though a shorter one is walked"
        );
    }

    #[test]
    fn a_lapsing_claim_passes_to_no_unrelated_standing_session() {
        let mut record = nested();
        lapsed(
            &mut record,
            &standing(&[
                ("s-helper", Some("HARBOR")),
                ("s-fork", Some("WCJQSG")),
                ("s-weigh", Some("OSPHT4")),
            ]),
            &logs(&[("s-helper", "s-elsewhere"), ("s-fork", "s-weigh")]),
        );
        assert_eq!(
            record.sessions.keys().collect::<Vec<_>>(),
            ["s-weigh"],
            "neither a session continuing another, nor one forked from a standing one, takes it"
        );
    }

    #[test]
    fn a_lapsing_claim_passes_to_the_nearest_standing_continuation() {
        let mut record = nested();
        lapsed(
            &mut record,
            &standing(&[
                ("s-steward-2", None),
                ("s-steward-2-fork", None),
                ("s-weigh", None),
            ]),
            &logs(&[
                ("s-steward-2-fork", "s-steward-2"),
                ("s-steward-2", "s-steward"),
            ]),
        );
        assert_eq!(
            record.sessions.keys().collect::<Vec<_>>(),
            ["s-steward-2", "s-weigh"],
            "a fork of the standing continuation doesn't vie with it"
        );
    }

    #[test]
    fn a_lapsing_claim_two_sessions_continue_passes_to_neither() {
        let mut record = nested();
        lapsed(
            &mut record,
            &standing(&[
                ("s-steward-2", None),
                ("s-steward-fork", None),
                ("s-weigh", None),
            ]),
            &logs(&[
                ("s-steward-2", "s-steward"),
                ("s-steward-fork", "s-steward"),
            ]),
        );
        assert_eq!(
            record.sessions.keys().collect::<Vec<_>>(),
            ["s-weigh"],
            "which of the two goes on can't be told, so it lapses"
        );
    }

    #[test]
    fn a_lapsing_claim_passes_over_a_continuation_that_claimed_its_own() {
        let mut record = nested();
        record
            .claim(
                Known {
                    session: "s-steward-2",
                    address: "WCJQSG",
                },
                "QEST-20261115-TERNXX",
                &[],
                at(),
            )
            .expect("it claims");
        let own = record.sessions.get("s-steward-2").cloned();
        let continued = logs(&[("s-steward-2", "s-steward")]);
        let said = record.lapse(
            &standing(&[("s-steward-2", None), ("s-weigh", None)]),
            || Some(reading(&continued)),
        );
        assert_eq!(
            record.sessions.get("s-steward-2").cloned(),
            own,
            "what a session said itself stands over what it continues"
        );
        assert!(!record.sessions.contains_key("s-steward"));
        assert_eq!(
            said.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "the claim to head QEST-20261007-WCJQSG lapsed, as s-steward no longer stands \
                 and s-steward-2, which continues it, holds a claim of its own"
            ],
            "and the claim it passed over is said to have lapsed, saying why"
        );
    }

    #[test]
    fn a_lapse_says_no_control_character_its_asker_wrote() {
        let forged = Lapsed::Forgotten {
            claimer: "s-weigh".to_owned(),
            heads: "QEST-1\nthe claim to head QEST-2 passed".to_owned(),
            why: "no standing session continues it".to_owned(),
        };
        assert_eq!(
            forged.to_string(),
            "the claim to head QEST-1\\nthe claim to head QEST-2 passed lapsed, as s-weigh no \
             longer stands and no standing session continues it",
            "a line break it named is shown, never written, so no line is forged"
        );
    }

    #[test]
    fn every_lapsing_claim_is_kept_where_a_log_followed_back_may_continue_it() {
        let mut record = nested();
        let before = record.clone();
        let standing = standing(&[("s-steward-2", Some("WCJQSG")), ("s-other", None)]);
        let said = record.lapse(&standing, || {
            Some(|session: &str| match session {
                "s-steward-2" => Ok(Some("s-steward".to_owned())),
                "s-other" => Err(quests(&["s-steward", "s-weigh"])),
                _ => Ok(None),
            })
        });
        assert_eq!(
            record, before,
            "one log that can't be told may continue either, so neither is handed on or lost"
        );
        assert_eq!(
            said,
            [
                Lapsed::Held {
                    claimer: "s-steward".to_owned(),
                    heads: "QEST-20261007-WCJQSG".to_owned(),
                    doubting: "s-other".to_owned(),
                },
                Lapsed::Held {
                    claimer: "s-weigh".to_owned(),
                    heads: "QEST-20261111-OSPHT4".to_owned(),
                    doubting: "s-other".to_owned(),
                },
            ],
            "and each is said to be held, naming whose logs can't be told, so a claim kept \
             with no end names its cause"
        );
        assert_eq!(
            said[0].to_string(),
            "the claim to head QEST-20261007-WCJQSG is kept for s-steward, which no longer \
             stands, as the logs named for s-other, which may continue it, don't all say what \
             it continues (one that can't be read says none)"
        );
        let planted = Lapsed::Held {
            claimer: "s-steward".to_owned(),
            heads: "QEST-20261007-WCJQSG\nforged".to_owned(),
            doubting: "s-other\nforged".to_owned(),
        }
        .to_string();
        assert!(
            !planted.contains('\n') && planted.matches("\\nforged").count() == 2,
            "each name the asker or a log chose is escaped, so the line stays one: {planted}"
        );
    }

    #[test]
    fn a_held_claim_passes_once_a_later_lapse_can_tell() {
        let mut record = nested();
        let was = record.sessions.get("s-steward").cloned().expect("claimed");
        let standing = standing(&[("s-steward-2", Some("WCJQSG")), ("s-other", None)]);
        record.lapse(&standing, || {
            Some(|session: &str| match session {
                "s-steward-2" => Ok(Some("s-steward".to_owned())),
                "s-other" => Err(quests(&["s-steward", "s-elder"])),
                _ => Ok(None),
            })
        });
        assert_eq!(
            record.sessions.keys().collect::<Vec<_>>(),
            ["s-steward"],
            "held at first, as the claim no doubt reaches lapses"
        );
        // The stray copy is gone, so its logs agree again.
        let said = record.lapse(&standing, || {
            Some(|session: &str| match session {
                "s-steward-2" => Ok(Some("s-steward".to_owned())),
                "s-other" => Ok(Some("s-elder".to_owned())),
                _ => Ok(None),
            })
        });
        assert_eq!(
            said,
            [Lapsed::Handed {
                claimer: "s-steward".to_owned(),
                heir: "s-steward-2".to_owned(),
                heads: "QEST-20261007-WCJQSG".to_owned(),
            }],
            "once the logs agree, the held claim passes on"
        );
        assert_eq!(record.sessions.get("s-steward-2"), Some(&was));
    }

    #[test]
    fn a_doubtful_walk_ends_at_a_cycle_and_past_its_bound() {
        let mut record = nested();
        let standing = standing(&[("s-steward-2", Some("WCJQSG")), ("s-other", None)]);
        let said = record.lapse(&standing, || {
            Some(|session: &str| match session {
                "s-steward-2" => Ok(Some("s-steward".to_owned())),
                // Two doubtful logs that lead to each other, and a line
                // that reaches the claimer one log past the bound.
                "s-other" => Err(quests(&["s-loop-a", "s-far-1"])),
                "s-loop-a" => Err(quests(&["s-loop-b", "s-elder"])),
                "s-loop-b" => Err(quests(&["s-loop-a", "s-older"])),
                far => match far
                    .strip_prefix("s-far-")
                    .and_then(|at| at.parse::<usize>().ok())
                {
                    Some(at) if at < CONTINUED_MOST => Ok(Some(format!("s-far-{}", at + 1))),
                    Some(_) => Ok(Some("s-weigh".to_owned())),
                    None => Ok(None),
                },
            })
        });
        assert_eq!(
            said,
            [
                Lapsed::Handed {
                    claimer: "s-steward".to_owned(),
                    heir: "s-steward-2".to_owned(),
                    heads: "QEST-20261007-WCJQSG".to_owned(),
                },
                Lapsed::Forgotten {
                    claimer: "s-weigh".to_owned(),
                    heads: "QEST-20261111-OSPHT4".to_owned(),
                    why: "no standing session continues it".to_owned(),
                },
            ],
            "a cycle of doubtful logs holds nothing they don't reach, and a claim past the \
             bound is no doubt's to hold"
        );
    }

    #[test]
    fn a_held_claim_names_the_first_doubtful_session_its_walk_met() {
        let mut record = nested();
        let standing = standing(&[("s-steward-2", Some("WCJQSG")), ("s-other", None)]);
        let said = record.lapse(&standing, || {
            Some(|session: &str| match session {
                "s-steward-2" => Ok(Some("s-steward".to_owned())),
                // Through a log that claimed nothing and no longer
                // stands, to two doubtful logs in a row, the second the
                // nearer the claimer.
                "s-other" => Ok(Some("s-gone".to_owned())),
                "s-gone" => Err(quests(&["s-nearer", "s-elder"])),
                "s-nearer" => Err(quests(&["s-weigh", "s-older"])),
                _ => Ok(None),
            })
        });
        assert_eq!(
            said,
            [
                Lapsed::Handed {
                    claimer: "s-steward".to_owned(),
                    heir: "s-steward-2".to_owned(),
                    heads: "QEST-20261007-WCJQSG".to_owned(),
                },
                Lapsed::Held {
                    claimer: "s-weigh".to_owned(),
                    heads: "QEST-20261111-OSPHT4".to_owned(),
                    doubting: "s-gone".to_owned(),
                },
            ],
            "held, naming the first session met whose logs can't be told, where the walk's \
             doubt began, not the nearer ones it went on through"
        );
    }

    #[test]
    fn a_log_that_cant_be_told_holds_back_only_the_claims_it_may_continue() {
        let mut record = nested();
        let weigh = record.sessions.get("s-weigh").cloned();
        let standing = standing(&[
            ("s-steward-2", Some("WCJQSG")),
            ("s-other", None),
            ("s-stray", None),
        ]);
        let said = record.lapse(&standing, || {
            Some(|session: &str| match session {
                "s-steward-2" => Ok(Some("s-steward".to_owned())),
                // Its logs disagree, one through a log that claimed
                // nothing and no longer stands.
                "s-other" => Err(quests(&["s-elder", "s-gone"])),
                "s-gone" => Ok(Some("s-weigh".to_owned())),
                "s-stray" => Err(quests(&["s-elsewhere", "s-older"])),
                _ => Ok(None),
            })
        });
        assert_eq!(
            said,
            [
                Lapsed::Handed {
                    claimer: "s-steward".to_owned(),
                    heir: "s-steward-2".to_owned(),
                    heads: "QEST-20261007-WCJQSG".to_owned(),
                },
                Lapsed::Held {
                    claimer: "s-weigh".to_owned(),
                    heads: "QEST-20261111-OSPHT4".to_owned(),
                    doubting: "s-other".to_owned(),
                },
            ],
            "a claim no doubtful log may lead to passes on, though others can't be told, and \
             one held names the logs that lead to it, not another's that can't be told"
        );
        assert_eq!(
            record.sessions.keys().collect::<Vec<_>>(),
            ["s-steward-2", "s-weigh"],
        );
        assert_eq!(
            record.sessions.get("s-weigh").cloned(),
            weigh,
            "and one a doubtful log may lead to is kept as it was, neither handed nor lost"
        );
    }

    #[test]
    fn a_lapsing_claim_passes_to_neither_where_one_of_two_continuations_claimed_its_own() {
        let mut record = nested();
        record
            .claim(
                Known {
                    session: "s-steward-2",
                    address: "WCJQSG",
                },
                "QEST-20261115-TERNXX",
                &[],
                at(),
            )
            .expect("it claims");
        let own = record.sessions.get("s-steward-2").cloned();
        lapsed(
            &mut record,
            &standing(&[
                ("s-steward-2", None),
                ("s-steward-fork", None),
                ("s-weigh", None),
            ]),
            &logs(&[
                ("s-steward-2", "s-steward"),
                ("s-steward-fork", "s-steward"),
            ]),
        );
        assert_eq!(
            record.sessions.keys().collect::<Vec<_>>(),
            ["s-steward-2", "s-weigh"],
            "the other continuation doesn't take it alone, since which goes on still \
             can't be told"
        );
        assert_eq!(
            record.sessions.get("s-steward-2").cloned(),
            own,
            "and the one that claimed keeps its own"
        );
    }

    #[test]
    fn the_person_frees_a_quest_whoever_holds_it() {
        let mut record = nested();
        assert_eq!(
            record.free("QEST-20261115-HARBOR"),
            Ok("OSPHT4".to_owned()),
            "a quest orchestrated is freed from its orchestrator"
        );
        assert_eq!(
            record
                .sessions
                .get("s-weigh")
                .map(|claim| claim.quests.len()),
            Some(0),
            "which keeps heading its own"
        );
        assert_eq!(
            record.free("QEST-20261007-WCJQSG"),
            Ok("WCJQSG".to_owned()),
            "and a quest headed frees all its session held"
        );
        assert!(
            !record.sessions.contains_key("s-steward"),
            "so its quests hang from nothing"
        );
        assert_eq!(
            record.free("QEST-20261115-HARBOR"),
            Err(
                "no session holds QEST-20261115-HARBOR by this node's record, so nothing is \
                 freed"
                    .to_owned()
            ),
        );
    }

    #[test]
    fn the_record_is_kept_whole_and_read_back() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let file = folder.path().join("mycelium").join("orchestrations.json");
        assert_eq!(
            Orchestrations::read(&file).ok(),
            Some(Orchestrations::default()),
            "none kept yet reads as none"
        );
        let record = nested();
        record.keep(&file).expect("it is kept");
        assert_eq!(Orchestrations::read(&file).ok(), Some(record));
        fs::write(&file, "{\"sessions\": 3}").expect("written");
        assert!(
            Orchestrations::read(&file).is_err(),
            "a record that can't be read proves nothing"
        );
    }

    #[test]
    fn a_log_names_the_session_it_continues() {
        let logs = tempfile::tempdir().expect("a temporary folder");
        let folder = logs.path().join("--home-joel-src-mycelium--");
        fs::create_dir_all(&folder).expect("made");
        fs::write(
            folder.join("2026-11-15T15-23-41-665Z_s-weigh-2.jsonl"),
            "{\"type\":\"session\",\"version\":3,\"id\":\"s-weigh-2\",\
             \"parentSession\":\"s-weigh\"}\n{\"type\":\"message\"}\n",
        )
        .expect("written");
        fs::write(
            folder.join("2026-11-15T14-00-00-000Z_s-steward.jsonl"),
            "{\"type\":\"session\",\"version\":3,\"id\":\"s-steward\"}\n",
        )
        .expect("written");
        let found = Logs::scan(logs.path()).expect("scanned");
        assert_eq!(found.continues("s-weigh-2"), Some("s-weigh".to_owned()));
        assert_eq!(
            found.continues("s-steward"),
            None,
            "one that continues none"
        );
        assert_eq!(found.continues("s-nobody"), None, "nor one with no log");
    }

    #[test]
    fn a_log_names_its_parent_by_the_path_pi_writes() {
        let logs = tempfile::tempdir().expect("a temporary folder");
        let folder = logs.path().join("--home-joel-src-mycelium--");
        fs::create_dir_all(&folder).expect("made");
        let parent =
            folder.join("2026-10-11T22-41-54-195Z_01a12d21-3813-76ee-b875-6702b23f353a.jsonl");
        fs::write(
            folder.join("2026-10-23T02-03-44-444Z_01a1667f-f5bc-7ba6-9290-033e899fff34.jsonl"),
            format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"01a1667f-f5bc-7ba6-9290-033e899fff34\",\
                 \"timestamp\":\"2026-10-23T02:03:44.444Z\",\"cwd\":\"/home/joel/src/mycelium\",\
                 \"parentSession\":{}}}\n",
                serde_json::Value::from(parent.to_str().expect("a path in words"))
            ),
        )
        .expect("written");
        assert_eq!(
            Logs::scan(logs.path())
                .expect("scanned")
                .continues("01a1667f-f5bc-7ba6-9290-033e899fff34"),
            Some("01a12d21-3813-76ee-b875-6702b23f353a".to_owned()),
            "the session the parent's log file is named for"
        );
    }

    #[test]
    fn a_parent_path_not_named_as_pi_names_a_log_is_taken_whole() {
        let logs = tempfile::tempdir().expect("a temporary folder");
        let folder = logs.path().join("--home-joel-src-mycelium--");
        fs::create_dir_all(&folder).expect("made");
        fs::write(
            folder.join("2026-11-15T15-23-41-665Z_s-weigh-2.jsonl"),
            "{\"id\":\"s-weigh-2\",\"parentSession\":\"/home/joel/notes/s-weigh.json\"}\n",
        )
        .expect("written");
        assert_eq!(
            Logs::scan(logs.path())
                .expect("scanned")
                .continues("s-weigh-2"),
            Some("/home/joel/notes/s-weigh.json".to_owned()),
            "a file name that isn't <time>_<id>.jsonl names no session, so the whole is kept"
        );
    }

    #[test]
    fn a_log_that_is_no_plain_file_or_no_header_continues_none() {
        let logs = tempfile::tempdir().expect("a temporary folder");
        let folder = logs.path().join("--home-joel-src-mycelium--");
        fs::create_dir_all(&folder).expect("made");
        let made = std::process::Command::new("mkfifo")
            .arg(folder.join("2026-11-15T15-23-41-665Z_s-fifo.jsonl"))
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "the FIFO is made");
        let long = format!(
            "{{\"id\":\"s-long\",\"parentSession\":\"s-weigh\",\"pad\":\"{}\"}}\n",
            "x".repeat(usize::try_from(super::FIRST_LINE_MOST).expect("a size"))
        );
        fs::write(folder.join("2026-11-15T15-23-41-665Z_s-long.jsonl"), long).expect("written");
        let found = Logs::scan(logs.path()).expect("scanned");
        // Read off the test's thread, so a read that waits for a writer
        // fails the test in time rather than hanging it.
        let (told, heard) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            told.send((found.continues("s-fifo"), found.continues("s-long")))
                .expect("the test waits");
        });
        assert_eq!(
            heard
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("a FIFO among the logs never holds the read"),
            (None, None),
            "a FIFO is never read, and a first line past the most is no header"
        );
    }

    #[test]
    fn a_session_whose_logs_disagree_continues_none() {
        let logs = tempfile::tempdir().expect("a temporary folder");
        let (own, planted) = (
            logs.path().join("--home-joel-src-mycelium--"),
            logs.path().join("--home-joel--"),
        );
        for folder in [&own, &planted] {
            fs::create_dir_all(folder).expect("made");
        }
        // Each names a session, so a scan that kept either, in whatever
        // order the folders are read, would name one.
        fs::write(
            own.join("2026-11-23T11-31-00-000Z_s-weigh.jsonl"),
            "{\"id\":\"s-weigh\",\"parentSession\":\"s-elder\"}\n",
        )
        .expect("written");
        fs::write(
            planted.join("2026-11-23T12-00-00-000Z_s-weigh.jsonl"),
            "{\"id\":\"s-weigh\",\"parentSession\":\"s-steward\"}\n",
        )
        .expect("written");
        let found = Logs::scan(logs.path()).expect("scanned");
        assert_eq!(
            found.continues("s-weigh"),
            None,
            "which log is its own can't be told, so neither names what it continues"
        );
        assert_eq!(
            found.follows("s-weigh"),
            Err(vec!["s-elder".to_owned(), "s-steward".to_owned()]),
            "and it says what each may continue, for a lapse to keep those claims"
        );
        assert_eq!(
            found.follows("s-nobody"),
            Ok(None),
            "one with no log continues none"
        );
    }

    #[test]
    fn a_session_whose_logs_agree_continues_what_they_name() {
        let logs = tempfile::tempdir().expect("a temporary folder");
        let (own, copied) = (
            logs.path().join("--home-joel-src-mycelium--"),
            logs.path().join("--home-joel--"),
        );
        for folder in [&own, &copied] {
            fs::create_dir_all(folder).expect("made");
        }
        for folder in [&own, &copied] {
            fs::write(
                folder.join("2026-11-23T11-31-00-000Z_s-weigh.jsonl"),
                "{\"id\":\"s-weigh\",\"parentSession\":\"s-elder\"}\n",
            )
            .expect("written");
            fs::write(
                folder.join("2026-11-23T11-00-00-000Z_s-steward.jsonl"),
                "{\"id\":\"s-steward\"}\n",
            )
            .expect("written");
        }
        let found = Logs::scan(logs.path()).expect("scanned");
        assert_eq!(
            (found.follows("s-weigh"), found.follows("s-steward")),
            (Ok(Some("s-elder".to_owned())), Ok(None)),
            "a copy that says what the log says leaves nothing to tell apart"
        );
    }

    #[test]
    fn a_copy_naming_none_disagrees_with_one_naming_a_session() {
        let logs = tempfile::tempdir().expect("a temporary folder");
        let (own, planted) = (
            logs.path().join("--home-joel-src-mycelium--"),
            logs.path().join("--home-joel--"),
        );
        for folder in [&own, &planted] {
            fs::create_dir_all(folder).expect("made");
        }
        fs::write(
            own.join("2026-11-23T11-31-00-000Z_s-weigh.jsonl"),
            "{\"id\":\"s-weigh\",\"parentSession\":\"s-elder\"}\n",
        )
        .expect("written");
        // Unreadable as a header, so it reads as continuing none.
        fs::write(
            planted.join("2026-11-23T12-00-00-000Z_s-weigh.jsonl"),
            "not a header\n",
        )
        .expect("written");
        let found = Logs::scan(logs.path()).expect("scanned");
        assert_eq!(
            found.follows("s-weigh"),
            Err(vec!["s-elder".to_owned()]),
            "one naming none and one naming a session can't be told apart, so what the one names may be continued"
        );
        let said = found.disagreeing("s-weigh").unwrap_or_default();
        for log in [
            own.join("2026-11-23T11-31-00-000Z_s-weigh.jsonl"),
            planted.join("2026-11-23T12-00-00-000Z_s-weigh.jsonl"),
        ] {
            assert!(
                said.contains(&log.display().to_string()),
                "it names each copy, so the stray can be found: {said:?}"
            );
        }
        assert!(
            said.contains("s-weigh"),
            "and whose logs they are: {said:?}"
        );
        assert_eq!(
            found.disagreeing("s-steward"),
            None,
            "a session with no logs here has none that disagree"
        );
    }

    #[test]
    fn logs_that_disagree_are_named_in_one_order_however_the_folders_read() {
        let logs = tempfile::tempdir().expect("a temporary folder");
        let log = "2026-11-23T11-31-00-000Z_s-weigh.jsonl";
        // Eight copies, made out of their order, so a folder read in the
        // order its entries were made, or the reverse, lists them out of
        // theirs, as one read in a hashed order does all but one time in
        // 40,320.
        for at in [5, 2, 7, 0, 3, 6, 1, 4] {
            let folder = logs.path().join(format!("--home-kestrel-{at}--"));
            fs::create_dir_all(&folder).expect("made");
            fs::write(
                folder.join(log),
                format!("{{\"id\":\"s-weigh\",\"parentSession\":\"s-elder-{at}\"}}\n"),
            )
            .expect("written");
        }
        let paths: Vec<String> = (0..8)
            .map(|at| {
                let folder = logs.path().join(format!("--home-kestrel-{at}--"));
                folder.join(log).display().to_string()
            })
            .collect();
        assert_eq!(
            Logs::scan(logs.path())
                .expect("scanned")
                .disagreeing("s-weigh"),
            Some(format!(
                "the 8 logs named for s-weigh don't all say what it continues, so a claim it \
                 may lead to is kept until the one that isn't its own is moved: {}",
                paths.join(", ")
            )),
            "in their paths' order, so the sentence a node says once is the same at every \
             lapse's scan, never said again as a folder reads in another order"
        );
    }

    #[test]
    fn each_log_is_read_once_a_scan() {
        let logs = tempfile::tempdir().expect("a temporary folder");
        let own = logs.path().join("--home-joel-src-mycelium--");
        fs::create_dir_all(&own).expect("made");
        let log = own.join("2026-11-23T11-31-00-000Z_s-weigh.jsonl");
        fs::write(&log, "{\"id\":\"s-weigh\",\"parentSession\":\"s-elder\"}\n").expect("written");
        let found = Logs::scan(logs.path()).expect("scanned");
        assert_eq!(found.follows("s-weigh"), Ok(Some("s-elder".to_owned())));
        fs::remove_file(&log).expect("removed");
        assert_eq!(
            found.follows("s-weigh"),
            Ok(Some("s-elder".to_owned())),
            "a lapse walking past it again reads what it read, not the disk again"
        );
    }

    #[test]
    fn the_logs_are_scanned_once_and_read_only_where_asked() {
        let logs = tempfile::tempdir().expect("a temporary folder");
        let (one, other) = (
            logs.path().join("--home-joel-src-mycelium--"),
            logs.path().join("--home-joel--"),
        );
        for folder in [&one, &other] {
            fs::create_dir_all(folder).expect("made");
        }
        fs::write(
            one.join("2026-11-23T11-31-00-000Z_s-steward-2.jsonl"),
            "{\"type\":\"session\",\"id\":\"s-steward-2\",\"parentSession\":\"s-steward\"}\n",
        )
        .expect("written");
        fs::write(
            other.join("2026-11-23T12-00-00-000Z_s-steward-3.jsonl"),
            "{\"type\":\"session\",\"id\":\"s-steward-3\",\"parentSession\":\"s-steward-2\"}\n",
        )
        .expect("written");
        fs::write(
            one.join("2026-11-23T12-00-00-000Z_s-liar.jsonl"),
            "{\"type\":\"session\",\"id\":\"s-other\",\"parentSession\":\"s-steward\"}\n",
        )
        .expect("written");
        fs::write(one.join("notes.txt"), "not a log\n").expect("written");
        fs::write(logs.path().join("README"), "beside the folders\n").expect("written");
        let found = Logs::scan(logs.path()).expect("a file beside the folders is passed over");
        // Whatever lands after the scan isn't seen, so the folder was read
        // once, as the scan was made.
        fs::write(
            one.join("2026-11-23T13-00-00-000Z_s-later.jsonl"),
            "{\"type\":\"session\",\"id\":\"s-later\",\"parentSession\":\"s-steward\"}\n",
        )
        .expect("written");
        assert_eq!(
            found.continues("s-steward-3"),
            Some("s-steward-2".to_owned())
        );
        assert_eq!(found.continues("s-steward-2"), Some("s-steward".to_owned()));
        assert_eq!(
            found.continues("s-liar"),
            None,
            "a log whose first line names another session continues none"
        );
        assert_eq!(found.continues("s-later"), None, "nor one not yet there");
        assert_eq!(
            found.continued(&["s-steward-3", "s-steward-2", "s-nobody"]),
            BTreeMap::from([
                ("s-steward-2".to_owned(), "s-steward".to_owned()),
                ("s-steward-3".to_owned(), "s-steward-2".to_owned()),
            ]),
            "and a set of sessions reads as one"
        );
    }

    #[test]
    fn no_logs_folder_reads_as_no_logs() {
        let logs = tempfile::tempdir().expect("a temporary folder");
        let found = Logs::scan(&logs.path().join("never-made")).expect("scanned");
        assert_eq!(found.continues("s-steward-2"), None);
    }

    #[test]
    fn a_logs_folder_that_cant_be_read_is_refused_not_read_as_empty() {
        let logs = tempfile::tempdir().expect("a temporary folder");
        let file = logs.path().join("sessions");
        fs::write(&file, "not a folder\n").expect("written");
        assert_eq!(
            Logs::scan(&file).map(|_| ()).map_err(|error| error.kind()),
            Err(std::io::ErrorKind::NotADirectory),
            "logs that can't be listed can't say what continues what"
        );
    }

    #[test]
    fn a_folder_that_cant_be_read_is_passed_over_and_named() {
        use std::os::unix::fs::PermissionsExt as _;
        let logs = tempfile::tempdir().expect("a temporary folder");
        let session = "01a1667f-f5bc-7ba6-9290-033e899fff34";
        let readable = logs.path().join("--home-joel-src--");
        fs::create_dir_all(&readable).expect("made");
        fs::write(
            readable.join(format!("2026-11-24T10-00-00-000Z_{session}.jsonl")),
            format!("{{\"id\":\"{session}\",\"parentSession\":\"s-steward\"}}\n"),
        )
        .expect("written");
        let shut = logs.path().join("--root--");
        fs::create_dir_all(&shut).expect("made");
        fs::set_permissions(&shut, fs::Permissions::from_mode(0o000)).expect("unreadable");
        // An account that reads past modes reads it, so names none, and
        // the refusal goes untested there: said, so the skip is seen.
        let unreadable = fs::read_dir(&shut).is_err();
        if !unreadable {
            eprintln!(
                "this account reads a folder of mode 0o000, so a folder that can't be read goes untested here"
            );
        }
        let scanned = Logs::scan(logs.path());
        fs::set_permissions(&shut, fs::Permissions::from_mode(0o700)).expect("readable");
        let scanned = scanned.expect("one folder unread costs only its own logs");
        assert_eq!(scanned.continues(session).as_deref(), Some("s-steward"));
        assert_eq!(
            scanned.unread().iter().any(|why| why.contains("--root--")),
            unreadable,
            "{:?}",
            scanned.unread()
        );
    }

    #[test]
    fn a_lapsing_claim_is_kept_where_the_logs_cant_be_read() {
        let mut record = nested();
        let before = record.clone();
        record.lapse(
            &standing(&[("s-steward-2", Some("WCJQSG")), ("s-weigh", Some("OSPHT4"))]),
            || None::<fn(&str) -> Continued>,
        );
        assert_eq!(
            record, before,
            "where which session goes on can't be told, nothing is lost or handed on"
        );
        lapsed(
            &mut record,
            &standing(&[("s-steward-2", Some("WCJQSG")), ("s-weigh", Some("OSPHT4"))]),
            &logs(&[("s-steward-2", "s-steward")]),
        );
        assert_eq!(
            record
                .sessions
                .get("s-steward-2")
                .map(|claim| claim.heads.as_str()),
            before
                .sessions
                .get("s-steward")
                .map(|claim| claim.heads.as_str()),
            "a kept claim passes on once the logs can be read"
        );
    }

    #[test]
    fn the_quests_record_must_name_the_same_orchestrators() {
        let node = Order {
            by: "WCJQSG".to_owned(),
            quest: "QEST-20261115-HARBOR".to_owned(),
            chain: vec![
                link("QEST-20261115-HARBOR", "OSPHT4"),
                link("QEST-20261111-OSPHT4", "WCJQSG"),
            ],
        };
        assert_eq!(agrees(&node, &node.clone()), Ok(()));
        let rewritten = Order {
            by: "WCJQSG".to_owned(),
            quest: "QEST-20261115-HARBOR".to_owned(),
            chain: vec![link("QEST-20261115-HARBOR", "WCJQSG")],
        };
        assert_eq!(
            agrees(&node, &rewritten),
            Err(
                "the quests' record gives QEST-20261115-HARBOR's chain as WCJQSG, but this \
                 node's record gives it as OSPHT4 then WCJQSG, so the chain isn't believed"
                    .to_owned()
            ),
        );
    }
}
