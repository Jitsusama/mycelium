//! The one program a worker may have this machine run, where its node
//! names the worker: nix-offload's gate, `nix-offload answer QUEST
//! COMMIT`, which a machine with no way to the builders asks of one with
//! a way. A node refuses a worker every start, since a worker runs on a
//! machine others may administer; this is the single opening, and it is
//! kept narrow:
//!
//! - Only the workers named, one a line, in the file the node's
//!   `--answers-for` gives, read again at every ask, so a worker taken
//!   off it, or the file emptied, is refused from the next ask on with no
//!   restart. A worker is named by the name its link is pinned to, never
//!   by anything it says.
//! - Only that program, by the store path `--answers-with` gives, run
//!   with its act and two arguments and through no shell, so the worker
//!   never chooses the binary, a folder, an environment or another
//!   argument ([`asked`]).
//! - One answer a worker at a time, and [`AT_ONCE`] on this machine,
//!   refused past that rather than queued.
//! - Of what the worker writes of a quest it was given, only a gate's
//!   request and bundle are taken, each a new file ([`take`]), and of
//!   what the gate writes, nothing is served back to it but the result:
//!   its log stays here ([`withheld`]).
//!
//! Every answer, take and refusal is said in the node's journal, with a
//! word for why ([`Refused::word`]).

pub mod take;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};

use crate::share::ledger::is_quest;
use crate::wire::Start;

pub use take::take;

/// How many answers run on this machine at once.
pub const AT_ONCE: usize = 2;

/// The program a worker's start names, as it names it.
const PROGRAM: &str = "nix-offload";
/// The program's act that answers a gate.
const ANSWER: &str = "answer";

/// Where in a quest's scratch the gates are, as nix-offload keeps them.
const GATES: &str = "scratch/handover/gates/";
/// A gate's request, in its folder.
pub const REQUEST: &str = "request.json";
/// The commits a gate is asked for, in its folder.
pub const BUNDLE: &str = "gate.bundle";
/// What the gate found, in its folder: the one file of it a worker is
/// served.
pub const RESULT: &str = "result.json";
/// The most a request may hold, as nix-offload reads one.
pub const LARGEST_REQUEST: u64 = 16 << 10;
/// The most a bundle may hold, as nix-offload reads one.
pub const LARGEST_BUNDLE: u64 = 64 << 20;

/// Why a worker's ask, or a file it sent, isn't taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// The worker isn't named, or no file names any: answering is off
    /// for it.
    Unlisted,
    /// Another program than the gate's, or the gate's asked another act.
    Program,
    /// Other arguments than a quest and a commit.
    Arguments,
    /// A quest that isn't a quest's id.
    Quest,
    /// A commit that isn't forty lowercase hex digits.
    Commit,
    /// A folder named to run in.
    Folder,
    /// A start marked as a carry.
    Carry,
    /// This worker's answer still runs.
    Busy,
    /// [`AT_ONCE`] answers run here.
    Full,
    /// A file that is neither a gate's request nor its bundle, in a
    /// gate's folder named for its commit.
    Name,
    /// A quest this machine didn't give the worker, or no quest's id.
    NotGiven,
    /// A file this machine never asked the worker for.
    Unasked,
    /// A file already there, which nothing a worker sends writes over.
    Exists,
    /// More than the most such a file may hold.
    Large,
    /// A link, or what isn't a folder, on the way to the gate's folder.
    Link,
    /// What came doesn't hold what its hash says.
    Hash,
    /// The file couldn't be written, and is gone again.
    Write,
}

impl Refused {
    /// The one word the journal says it with.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Unlisted => "unlisted",
            Self::Program => "program",
            Self::Arguments => "arguments",
            Self::Quest => "quest",
            Self::Commit => "commit",
            Self::Folder => "folder",
            Self::Carry => "carry",
            Self::Busy => "busy",
            Self::Full => "full",
            Self::Name => "name",
            Self::NotGiven => "not-given",
            Self::Unasked => "unasked",
            Self::Exists => "exists",
            Self::Large => "large",
            Self::Link => "link",
            Self::Hash => "hash",
            Self::Write => "write",
        }
    }
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let why = match self {
            Self::Unlisted => "this machine answers no gate for it",
            Self::Program => "it may ask only `nix-offload answer`",
            Self::Arguments => "a gate takes a quest and a commit, nothing more",
            Self::Quest => "that names no quest",
            Self::Commit => "a commit is forty lowercase hex digits",
            Self::Folder => "a gate runs in no folder of the asker's",
            Self::Carry => "a gate carries nothing",
            Self::Busy => "its last gate still runs",
            Self::Full => "this machine runs as many gates as it takes",
            Self::Name => "only a gate's request and bundle are taken",
            Self::NotGiven => "that quest wasn't given to it here",
            Self::Unasked => "the file wasn't asked for",
            Self::Exists => "the file is here already",
            Self::Large => "the file is larger than a gate takes",
            Self::Link => "a link or a file stands where a gate's folder goes",
            Self::Hash => "the file doesn't hold what its hash says",
            Self::Write => "the file couldn't be written",
        };
        write!(f, "{why} ({})", self.word())
    }
}

/// What this machine answers its workers' gates with, and who it
/// answers.
#[derive(Debug)]
pub struct Gates {
    /// The gate's program, by its store path.
    program: PathBuf,
    /// The file naming the workers it answers, one a line.
    listed: PathBuf,
    /// The workers whose answer runs now.
    running: Mutex<HashSet<String>>,
}

impl Gates {
    /// Answers with `program` the workers `listed` names, or none where
    /// `program` isn't a full path, since a name alone would be found on
    /// a search path.
    ///
    /// # Errors
    ///
    /// When `program` isn't a full path.
    pub fn new(program: PathBuf, listed: PathBuf) -> Result<Self, String> {
        if !program.is_absolute() {
            return Err(format!(
                "--answers-with needs the gate's full path, not {}",
                program.display()
            ));
        }
        Ok(Self {
            program,
            listed,
            running: Mutex::new(HashSet::new()),
        })
    }

    /// Whether `worker` is named now: none is where the file is gone,
    /// so taking it away stops every answer.
    #[must_use]
    pub fn lists(&self, worker: &str) -> bool {
        fs::read_to_string(&self.listed)
            .is_ok_and(|names| names.lines().any(|name| name.trim() == worker))
    }

    /// The gate `worker` asks with `start`, its place among those running
    /// taken until it is dropped, or why not.
    ///
    /// # Errors
    ///
    /// When `worker` isn't named, the start asks anything but a gate, or
    /// as many gates run as may.
    pub fn admit(self: &Arc<Self>, worker: &str, start: &Start) -> Result<Answer, Refused> {
        if !self.lists(worker) {
            return Err(Refused::Unlisted);
        }
        let (quest, commit) = asked(&start.command)?;
        if start.folder.is_some() {
            return Err(Refused::Folder);
        }
        if start.carries {
            return Err(Refused::Carry);
        }
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if running.contains(worker) {
            return Err(Refused::Busy);
        }
        if running.len() >= AT_ONCE {
            return Err(Refused::Full);
        }
        running.insert(worker.to_owned());
        Ok(Answer {
            gates: Arc::clone(self),
            worker: worker.to_owned(),
            quest: quest.to_owned(),
            commit: commit.to_owned(),
        })
    }

    /// How many answers run now.
    #[must_use]
    pub fn running(&self) -> usize {
        self.running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

/// A worker's gate, admitted, holding its place until dropped.
#[derive(Debug)]
pub struct Answer {
    gates: Arc<Gates>,
    worker: String,
    quest: String,
    commit: String,
}

impl Answer {
    /// The worker it answers.
    #[must_use]
    pub fn worker(&self) -> &str {
        &self.worker
    }

    /// The quest whose scratch holds the gate.
    #[must_use]
    pub fn quest(&self) -> &str {
        &self.quest
    }

    /// The commit gated.
    #[must_use]
    pub fn commit(&self) -> &str {
        &self.commit
    }

    /// The gate's run: its program by its store path, with its act, the
    /// quest and the commit as arguments alone, reading nothing and
    /// saying nothing back, since the result crosses as a file and its
    /// log stays here.
    #[must_use]
    pub fn command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.gates.program);
        command
            .arg(ANSWER)
            .arg(&self.quest)
            .arg(&self.commit)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }
}

impl Drop for Answer {
    fn drop(&mut self) {
        self.gates
            .running
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.worker);
    }
}

/// The quest and commit of a start that asks a gate, exactly `nix-offload
/// answer QUEST COMMIT`, or why it asks something else.
///
/// # Errors
///
/// When it names another program or act, more or fewer arguments, a
/// quest that isn't a quest's id, or a commit that isn't forty lowercase
/// hex digits.
pub fn asked(command: &[String]) -> Result<(&str, &str), Refused> {
    let [program, act, arguments @ ..] = command else {
        return Err(Refused::Program);
    };
    if program != PROGRAM || act != ANSWER {
        return Err(Refused::Program);
    }
    let [quest, commit] = arguments else {
        return Err(Refused::Arguments);
    };
    if !is_quest(quest) {
        return Err(Refused::Quest);
    }
    if !is_commit(commit) {
        return Err(Refused::Commit);
    }
    Ok((quest, commit))
}

/// Whether `commit` is a full commit's id, as git names one in sha-1.
fn is_commit(commit: &str) -> bool {
    commit.len() == 40
        && commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A gate's file a worker may send, by its key in a quest's ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateFile {
    /// The commit its gate is for, which names its folder.
    pub commit: String,
    /// Its name: the request or the bundle.
    pub name: &'static str,
    /// The most it may hold.
    pub largest: u64,
}

impl GateFile {
    /// Where it goes in its quest's scratch.
    #[must_use]
    pub fn within(&self) -> PathBuf {
        Path::new(&GATES["scratch/".len()..])
            .join(&self.commit)
            .join(self.name)
    }
}

/// The gate's file `key` names, as a quest's ledger keys it, if it is one
/// a worker may send: `scratch/handover/gates/COMMIT/` and the request
/// or the bundle.
///
/// # Errors
///
/// When `key` names any other file.
pub fn sent(key: &str) -> Result<GateFile, Refused> {
    let (commit, name) = key
        .strip_prefix(GATES)
        .and_then(|rest| rest.split_once('/'))
        .ok_or(Refused::Name)?;
    if !is_commit(commit) {
        return Err(Refused::Name);
    }
    let (name, largest) = match name {
        REQUEST => (REQUEST, LARGEST_REQUEST),
        BUNDLE => (BUNDLE, LARGEST_BUNDLE),
        _ => return Err(Refused::Name),
    };
    Ok(GateFile {
        commit: commit.to_owned(),
        name,
        largest,
    })
}

/// Whether `key` is one of the gates' files no worker is served: all of
/// them but a gate's request, its bundle and its result, so the run's
/// log, and anything else a gate leaves, stays here.
#[must_use]
pub fn withheld(key: &str) -> bool {
    let Some(rest) = key.strip_prefix(GATES) else {
        return false;
    };
    !matches!(
        rest.split_once('/'),
        Some((commit, REQUEST | BUNDLE | RESULT)) if is_commit(commit)
    )
}
