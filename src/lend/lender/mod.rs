//! The lend's end on the machine lending: each model call a machine it
//! made asks of it, carried to this machine's model relay with the key
//! added here, and its answer streamed back as it comes.
//!
//! What may be lent is a sealed file (`--lend`), read again at every
//! request: each machine lent to, by the start of its name, with the
//! budget its requests go through and how many it may make a day, and
//! each budget's one socket, host, paths and key. A machine the file
//! names none for, or two, a path or a budget it doesn't list, and a
//! machine cut off are refused 403; one past its day's count 429. Only
//! the headers a model's answer turns on are carried on, so whatever
//! credentials the borrower sent stay with it, and the key, which the
//! borrower never sees, is added here. Each use is written as one line,
//! with no prompt, answer, header or key in it. The node's stop waits
//! for the lends in flight, as [`drain`] says.

mod drain;
mod health;
mod plain_file;
mod spend;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::lend::forge::BORROWER;
use crate::lend::serve::{HANDED_ON, MOST_BODY, MOST_HEAD};
use crate::lend::{BACKLOG, Lend, Lent};

use drain::{CUT, CUT_WHY, InFlight, Lends, STOPPING};
use spend::{Meter, dollars, millionths, spend_watch_allows, usd};

pub use drain::LENDS_DRAIN_WITHIN;

/// The most of an answer's body one [`Lent::Body`] carries.
const PIECE: usize = 16 * 1024;

/// What a relay answers when the one connection it was asked on was
/// misdirected: pi's own relay says so to send pi straight to its proxy,
/// which a borrower can't reach, so a lend asks it once more instead.
const MISDIRECTED: u16 = 421;

/// How often a lend streaming looks again at whether its machine was cut
/// off, so a cut ends it within this.
const CUTS_LOOKED_AT: Duration = Duration::from_secs(1);

/// How long a budget's key command has to say the key.
const KEY_WITHIN: Duration = Duration::from_secs(30);

/// How long the model has to begin its answer once it was asked: as
/// long as a whole answer not streamed may take, so a relay that takes
/// a lend and never answers ends the turn rather than hanging it.
const HEAD_WITHIN: Duration = Duration::from_secs(10 * 60);

/// How long the relay has to take a request whole, so one wedged with
/// its socket still bound, whose backlog lets a lend connect and whose
/// full buffer then stops the write, ends the turn rather than hanging
/// it.
const SENT_WITHIN: Duration = Duration::from_secs(2 * 60);

/// How long an answer begun may go without a byte, far past the pings a
/// model's stream sends a few seconds apart, so one that goes quiet
/// partway ends the turn rather than hanging it.
const QUIET_WITHIN: Duration = Duration::from_secs(5 * 60);

/// How long a lend waits for another node's hold on the day's counts,
/// a moment at a time, before refusing, so a node stopped holding them
/// never stops this one.
const COUNTS_WITHIN: Duration = Duration::from_millis(200);

/// The methods a lend may ask with.
const METHODS: [&str; 2] = ["POST", "GET"];

/// How the cuts name every machine at once.
const EVERY: &str = "*";

/// What may be lent, as its file says it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Written {
    /// The machines lent to.
    vms: Vec<Vm>,
    /// The budgets they may be lent through, by name.
    budgets: BTreeMap<String, Budget>,
}

/// The machines one lend is for.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Vm {
    /// The start of their names.
    prefix: String,
    /// The budget their requests go through where they name none.
    budget: String,
    /// The other budgets they may name, as a forge beside their models.
    #[serde(default)]
    also: Vec<String>,
    /// How many requests each may make a day, through all its budgets.
    requests_a_day: u64,
    /// How many dollars of model calls each may spend a day, by UTC,
    /// past which each is refused; none where it may make none.
    #[serde(default)]
    usd_a_day: Option<f64>,
}

/// What a budget's upstream is.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    /// A model relay, told nothing of who asked.
    #[default]
    Models,
    /// A forge, told which machine asked in [`BORROWER`], since it signs
    /// and pushes for each by name.
    Forge,
}

/// Where a budget's requests go, and with what key.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Budget {
    /// The socket each goes on to, whole or within the runtime folder.
    upstream: PathBuf,
    /// The host they are for.
    host: String,
    /// The paths they may ask for, each with all below it.
    paths: Vec<String>,
    /// The header the key goes in.
    auth_header: String,
    /// The command, by its program's whole path, printing the header's
    /// value; none where the upstream adds its own.
    #[serde(default)]
    key: Option<Vec<String>>,
    /// What answers at its upstream.
    #[serde(default)]
    kind: Kind,
    /// What each model it may be asked costs, by name; a model not
    /// named is refused.
    #[serde(default)]
    prices: BTreeMap<String, Price>,
    /// The budget in spend-watch whose money a model's requests spend,
    /// refused while spend-watch is silent, says it never declared it,
    /// or says it is past an allowance.
    #[serde(default)]
    spend_watch: Option<String>,
}

/// What a model's tokens cost, in dollars a million, and the most it
/// answers with.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct Price {
    /// A million tokens asked.
    input: f64,
    /// A million tokens answered.
    output: f64,
    /// A million tokens written to its cache, else a quarter more than
    /// `input`.
    #[serde(default)]
    cache_write: Option<f64>,
    /// A million tokens read from its cache, else `input`.
    #[serde(default)]
    cache_read: Option<f64>,
    /// The most tokens it answers with, past which a request is refused.
    most_output: u64,
}

/// The machines a lend is cut off from.
pub enum Cut {
    /// One, by its name.
    Machine(String),
    /// Every one.
    All,
}

/// A refusal, as it is said to the borrower.
#[derive(Debug)]
struct Refusal {
    status: u16,
    why: String,
}

fn refused(status: u16, why: impl Into<String>) -> Refusal {
    Refusal {
        status,
        why: why.into(),
    }
}

/// How one lend ended.
enum Ended {
    /// Its answer was sent whole.
    Whole,
    /// It was refused, before its answer or partway through it.
    Refused(Refusal),
    /// Its borrower stopped reading.
    Gone,
    /// The node's stop cut it, still going at the drain's bound.
    Cut,
}

/// What one lend's line says of it, as it goes.
#[derive(Default)]
struct Used {
    service: Kind,
    budget: Option<String>,
    model: Option<String>,
    upstream: Option<String>,
    request_bytes: usize,
    response_bytes: usize,
    status: Option<u16>,
    /// How many times it was asked whole of its relay: twice where the
    /// first answered 421.
    asked: u8,
    /// What its answer says it used, read as it streams, for a model's.
    meter: Option<Meter>,
    /// What it was charged, in millionths of a dollar, once settled.
    charged: Option<u64>,
    /// What its machine has spent today, this lend's charge in it.
    spent_today: Option<u64>,
}

/// What one model request may spend: its machine's most a day, and
/// what it reserves of that before it is sent, in millionths of a
/// dollar.
#[derive(Debug, Clone, Copy)]
struct Spending {
    most: u64,
    reserved: u64,
    price: Price,
    /// Its tokens asked at most, as its body's bytes.
    asked: u64,
    /// Its tokens answered at most: its `max_tokens`.
    answered: u64,
}

/// How many requests a lend's machine may make a day, and what a model's
/// may spend.
#[derive(Debug, Clone, Copy)]
struct Limits {
    requests: u64,
    spending: Option<Spending>,
}

/// One machine's day, as the lender keeps it.
#[derive(Debug, Default)]
struct Tally {
    /// The requests it made.
    requests: u64,
    /// What it spent and holds reserved, in millionths of a dollar.
    spent: u64,
}

/// This machine's lend: its file, and where it keeps its counts and cuts.
pub struct Lender {
    /// The sealed file saying what may be lent.
    file: PathBuf,
    /// The accounts that may seal it.
    declarers: Vec<u32>,
    /// Where the day's counts and the cuts are kept.
    folder: PathBuf,
    /// The folder an upstream named within it is found in, if there is
    /// one.
    runtime: Option<PathBuf>,
    /// Held while a count is read and written, so two never count as one.
    counting: Mutex<()>,
    /// The lends being answered, which the node's stop waits on.
    lends: Lends,
}

impl Lender {
    /// A lender reading `file`, sealed by `declarers`, keeping its counts
    /// and cuts in `folder`, and finding an upstream not named whole
    /// within `runtime`. Nothing is read until a lend is asked.
    #[must_use]
    pub fn new(
        file: PathBuf,
        declarers: Vec<u32>,
        folder: PathBuf,
        runtime: Option<PathBuf>,
    ) -> Self {
        Self {
            file,
            declarers,
            folder,
            runtime,
            counting: Mutex::new(()),
            lends: Lends::default(),
        }
    }

    /// Takes no lend from now, each asked refused 503, and waits for
    /// those in flight to end: at most `within`, past which those left
    /// are cut, or until `again`, a second stop, which cuts them and ends
    /// the wait at once. With none in flight it ends at once.
    pub async fn drain(&self, within: Duration, again: impl Future<Output = ()>) {
        self.lends.drain(within, again).await;
    }

    /// A lender reading `file`, sealed by `declarers`, keeping its counts
    /// and cuts in this machine's [`folder`], whatever the node's `--state`
    /// says, since `mycelium lend cut` asks no node and reads that folder
    /// too, which it says in the node's log, beside what `lend cut`
    /// says it wrote; and finding an upstream within the login's runtime
    /// folder.
    #[must_use]
    pub fn from_file(file: PathBuf, declarers: Vec<u32>) -> Self {
        let folder = folder();
        eprintln!(
            "mycelium: this machine lends as {} says, its cuts in {}",
            file.display(),
            folder.join("cuts.json").display()
        );
        let lender = Self::new(file, declarers, folder, crate::place::login_runtime());
        // Read once at the start, so a lend that isn't taken says so in
        // the log before any machine asks of it; health reads it afresh.
        let _ = lender.written();
        lender
    }

    /// The lender `file` says, if one is named, as [`Lender::from_file`]
    /// reads it: one, shared by the node's links and its stop.
    #[must_use]
    pub fn named(file: Option<PathBuf>, declarers: &[u32]) -> Option<Arc<Self>> {
        file.map(|file| Arc::new(Self::from_file(file, declarers.to_vec())))
    }

    /// What may be lent, as the file says now. Why it isn't taken goes to
    /// this machine's log alone, since the asker is another machine's
    /// agent, and says where it doesn't parse but never what it found.
    fn written(&self) -> Result<Written, Refusal> {
        self.read().map_err(|why| {
            eprintln!(
                "mycelium: this machine's lend, {}, isn't taken: {why}",
                self.file.display()
            );
            refused(
                503,
                "this machine's lend isn't taken; its node's log says why".to_owned(),
            )
        })
    }

    /// What may be lent, as the file says now, or why it isn't taken: where
    /// it doesn't parse, only what kind of fault and where, never what it
    /// found, so it may be said beyond the log. Only a plain file is
    /// read, so a pipe put in its place can't hold the node's health.
    fn read(&self) -> Result<Written, String> {
        crate::declared::sealed(&self.file, &self.declarers)
            .and_then(|()| plain_file::read(&self.file).map_err(|error| error.to_string()))
            .and_then(|text| serde_json::from_slice(&text).map_err(|error| unparsed(&error)))
    }

    /// Why `machine` is cut off, as the cuts say now, where it is: cuts
    /// that can't be read say so, and cut every machine off, since a
    /// lend can't be told apart from one meant to have ended; why goes
    /// to this machine's log.
    fn cut_off_why(&self, machine: &str) -> Option<String> {
        match cuts(&self.folder) {
            Ok(cuts) => cuts
                .iter()
                .any(|cut| cut == machine || cut == EVERY)
                .then(|| format!("{machine} is cut off from this machine's lend")),
            Err(error) => {
                eprintln!(
                    "mycelium: this machine's lend cuts, in {}, can't be read, so it lends \
                     nothing: {error}",
                    self.folder.display()
                );
                Some("this machine's lend cuts can't be read, so it lends nothing".to_owned())
            }
        }
    }

    /// Counts a request of `machine`'s against its `limits`, refusing it
    /// once the day's requests are made or where what it may spend would
    /// pass what is left of the day's, and answering the day it counted
    /// in. A model's reserves the most it may spend, so requests asked at
    /// once can't pass the cap together.
    fn count(&self, machine: &str, limits: Limits) -> Result<String, Refusal> {
        let day = jiff::Timestamp::now().strftime("%Y-%m-%d").to_string();
        self.counted(machine, &day, |tally| {
            if tally.requests >= limits.requests {
                return Err(refused(
                    429,
                    format!("{machine} has made its {} requests today", limits.requests),
                ));
            }
            if let Some(spending) = limits.spending {
                if tally.spent.saturating_add(spending.reserved) > spending.most {
                    return Err(refused(
                        429,
                        format!(
                            "{machine} has spent ${} of its ${} today, too little left for \
                             this request's most, ${}",
                            dollars(tally.spent),
                            dollars(spending.most),
                            dollars(spending.reserved)
                        ),
                    ));
                }
                tally.spent += spending.reserved;
            }
            tally.requests += 1;
            Ok(())
        })?;
        Ok(day)
    }

    /// Settles a model request of `machine`'s counted on `day` to what it
    /// was `charged` in place of what it `reserved`, answering what the
    /// machine has spent that day.
    fn settle(&self, machine: &str, day: &str, reserved: u64, charged: u64) -> Option<u64> {
        let mut spent = None;
        match self.counted(machine, day, |tally| {
            tally.spent = tally.spent.saturating_sub(reserved).saturating_add(charged);
            spent = Some(tally.spent);
            Ok(())
        }) {
            Ok(()) => spent,
            Err(refusal) => {
                eprintln!(
                    "mycelium: a request of {machine}'s can't be settled, so it stays charged \
                     its most: {}",
                    refusal.why
                );
                None
            }
        }
    }

    /// Gives back a request of `machine`'s counted on `day`, and what it
    /// `reserved`, which the lender couldn't carry for a fault of its
    /// own; to that day, though another has begun since.
    fn uncount(&self, machine: &str, day: &str, reserved: u64) {
        if let Err(refusal) = self.counted(machine, day, |tally| {
            tally.requests = tally.requests.saturating_sub(1);
            tally.spent = tally.spent.saturating_sub(reserved);
            Ok(())
        }) {
            eprintln!(
                "mycelium: a request of {machine}'s can't be given back: {}",
                refusal.why
            );
        }
    }

    /// Changes `machine`'s tally of `day` as `changing` says, keeping it
    /// whole: within this node by its lock, and across every node on the
    /// machine by a lock on the folder's `counts.lock`, so two lending
    /// nodes, which only one should be, lose no count. Its requests are
    /// kept in `counts-DAY.json` and what it spent in `spent-DAY.json`.
    fn counted(
        &self,
        machine: &str,
        day: &str,
        changing: impl FnOnce(&mut Tally) -> Result<(), Refusal>,
    ) -> Result<(), Refusal> {
        let _held = self
            .counting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _locked = self
            .locked()
            .map_err(|error| refused(503, format!("the day's counts can't be locked: {error}")))?;
        let counts_file = self.folder.join(format!("counts-{day}.json"));
        let spent_file = self.folder.join(format!("spent-{day}.json"));
        let mut counts = kept(&counts_file)?;
        let mut spent = kept(&spent_file)?;
        let mut tally = Tally {
            requests: counts.get(machine).copied().unwrap_or_default(),
            spent: spent.get(machine).copied().unwrap_or_default(),
        };
        changing(&mut tally)?;
        let was_spent = spent.get(machine).copied().unwrap_or_default();
        counts.insert(machine.to_owned(), tally.requests);
        spent.insert(machine.to_owned(), tally.spent);
        // What it spent first, so a count kept with no spend beside it
        // never lets a request through uncharged.
        if tally.spent != was_spent {
            write_whole(&self.folder, &spent_file, &spent)
                .map_err(|error| refused(503, format!("the day's spend can't be kept: {error}")))?;
        }
        write_whole(&self.folder, &counts_file, &counts)
            .map_err(|error| refused(503, format!("the day's count can't be kept: {error}")))
    }

    /// The folder's `counts.lock`, held for as long as what it answers
    /// lives, waiting at most [`COUNTS_WITHIN`] for another node holding
    /// it, since this runs on the node's only thread.
    fn locked(&self) -> io::Result<fs::File> {
        let lock = lock_file(&self.folder, "counts.lock")?;
        let began = Instant::now();
        loop {
            match rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(lock),
                Err(rustix::io::Errno::WOULDBLOCK) if began.elapsed() < COUNTS_WITHIN => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(rustix::io::Errno::WOULDBLOCK) => {
                    return Err(io::Error::other("another node holds them"));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

/// A day's tally by machine, as `file` keeps it; none where there is no
/// file yet.
fn kept(file: &Path) -> Result<BTreeMap<String, u64>, Refusal> {
    let unread = |why: String| refused(503, format!("the day's tally can't be read: {why}"));
    match plain_file::read(file) {
        Ok(text) => serde_json::from_slice(&text).map_err(|error| unread(error.to_string())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(unread(error.to_string())),
    }
}

/// `name` in `folder`, opened to be locked, the folder made for this
/// account alone.
fn lock_file(folder: &Path, name: &str) -> io::Result<fs::File> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(folder)?;
    fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(folder.join(name))
}

/// The folder's `cuts.lock`, held while the cuts are changed, so two
/// cuts made at once both hold.
fn cutting(folder: &Path) -> io::Result<fs::File> {
    let lock = lock_file(folder, "cuts.lock")?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)?;
    Ok(lock)
}

/// Where this machine keeps its lend's counts and cuts.
#[must_use]
pub fn folder() -> PathBuf {
    crate::place::state().join("lend")
}

/// The machines cut off, as `folder` keeps them; none where nothing was
/// ever cut. Where they don't parse, the error says only what kind of
/// fault and where, as [`unparsed`] does, never what was found. Only a
/// plain file is read, so a pipe put in its place can't hold a lend or
/// the node's health.
fn cuts(folder: &Path) -> io::Result<Vec<String>> {
    match plain_file::read(&folder.join("cuts.json")) {
        Ok(text) => serde_json::from_slice(&text)
            .map_err(|error| io::Error::other(unparsed_as(&error, "a list of machines"))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// Cuts `cut` off from the lend kept in `folder`, at once: each lend it
/// asks from now is refused, and each it is answered ends within a
/// second. Cutting every machine off overwrites cuts that can't be read.
/// Answers the machines cut off now.
///
/// # Errors
///
/// When the cuts can't be read, but for cutting every machine, or can't
/// be written.
pub fn cut(folder: &Path, cut: &Cut) -> io::Result<Vec<String>> {
    let _held = cutting(folder)?;
    let mut cuts = match (cuts(folder), cut) {
        (Err(_), Cut::All) => Vec::new(),
        (cuts, _) => cuts?,
    };
    let name = match cut {
        Cut::Machine(machine) => machine.as_str(),
        Cut::All => EVERY,
    };
    if !cuts.iter().any(|one| one == name) {
        cuts.push(name.to_owned());
    }
    write_whole(folder, &folder.join("cuts.json"), &cuts)?;
    Ok(cuts)
}

/// Lets `cut` borrow again from the lend kept in `folder`; [`Cut::All`]
/// lets every machine, each cut by name too, and overwrites cuts that
/// can't be read. Answers the machines still cut off.
///
/// # Errors
///
/// When the cuts can't be read, but for letting every machine back, or
/// can't be written; and when one machine is let back while every
/// machine is cut off, which would leave it cut off.
pub fn uncut(folder: &Path, cut: &Cut) -> io::Result<Vec<String>> {
    let _held = cutting(folder)?;
    let mut cuts = match (cuts(folder), cut) {
        (Err(_), Cut::All) => Vec::new(),
        (cuts, _) => cuts?,
    };
    match cut {
        Cut::Machine(machine) => {
            if cuts.iter().any(|one| one == EVERY) {
                return Err(io::Error::other(format!(
                    "every machine is cut off, so {machine} stays cut off; let every machine \
                     back first"
                )));
            }
            cuts.retain(|one| one != machine);
        }
        Cut::All => cuts.clear(),
    }
    write_whole(folder, &folder.join("cuts.json"), &cuts)?;
    Ok(cuts)
}

/// Writes `value` to `file` in `folder` whole, by a rename, so nothing
/// reads it half written; the folder is made for this account alone.
fn write_whole(folder: &Path, file: &Path, value: &impl serde::Serialize) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(folder)?;
    let writing = folder.join(format!(
        ".{}.{}",
        file.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let mut opened = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&writing)?;
    io::Write::write_all(&mut opened, &serde_json::to_vec(value)?)?;
    opened.sync_all()?;
    fs::rename(&writing, file)
}

/// Answers `lend`, asked by `machine`, in a task of its own: each piece
/// of its answer comes on the receiver as the relay sends it. Dropping
/// the receiver lets the lend go.
#[must_use]
pub fn answer(lender: &Arc<Lender>, machine: &str, lend: Lend) -> mpsc::Receiver<Lent> {
    let (sending, answers) = mpsc::channel(BACKLOG);
    // Taken as it is asked, before its task first runs, so a stop begun
    // meanwhile waits for it.
    let in_flight = lender.lends.begun();
    let lender = lender.clone();
    let machine = machine.to_owned();
    tokio::spawn(async move {
        carry(&lender, &machine, lend, &sending, in_flight).await;
    });
    answers
}

/// Carries `lend` for `machine`, `in_flight` as the node's stop counts
/// it, or refuses it 503 where none is, the stop having begun; then
/// writes its line, answering the line's outcome and what it used.
async fn carry(
    lender: &Lender,
    machine: &str,
    lend: Lend,
    sending: &mpsc::Sender<Lent>,
    in_flight: Option<InFlight>,
) -> (String, Used) {
    let began = Instant::now();
    let mut used = Used::default();
    let ended = match &in_flight {
        Some(_) => lent(lender, machine, lend, sending, &mut used).await,
        None => Ended::Refused(refused(503, STOPPING)),
    };
    let (outcome, told) = match ended {
        Ended::Whole => ("sent".to_owned(), None),
        Ended::Refused(refusal) => (format!("refused: {}", refusal.why), Some(refusal)),
        Ended::Gone => ("let go: its borrower stopped reading".to_owned(), None),
        Ended::Cut => (CUT.to_owned(), Some(refused(503, CUT_WHY))),
    };
    if let Some(Refusal { status, .. }) = &told {
        used.status.get_or_insert(*status);
    }
    // Said before the refusal is sent, which waits on a borrower that
    // may have stopped taking what it is sent, so a cut lend is in the
    // log however its borrower behaves; and the stop waits no longer for
    // it once it is.
    say(machine, &used, began.elapsed(), &outcome);
    drop(in_flight);
    if let Some(Refusal { status, why }) = told {
        // A borrower gone meanwhile has nothing to be told.
        let _ = sending.send(Lent::Refused { status, why }).await;
    }
    (outcome, used)
}

/// Writes one line of what a lend was, as the node's own output, so the
/// lending machine keeps every use: never its prompt, answer, headers or
/// key.
fn say(machine: &str, used: &Used, took: Duration, outcome: &str) {
    let lender = rustix::system::uname()
        .nodename()
        .to_string_lossy()
        .into_owned();
    let line = json!({
        "time": format!("{:.3}", jiff::Timestamp::now()),
        "lender": lender,
        "vm": machine,
        "service": match used.service {
            Kind::Models => "models",
            Kind::Forge => "forge",
        },
        "budget": used.budget,
        "model": used.model,
        "upstream": used.upstream,
        "request_bytes": used.request_bytes,
        "response_bytes": used.response_bytes,
        "status": used.status,
        "asked": used.asked,
        "charged_usd": used.charged.map(usd),
        "spent_usd_today": used.spent_today.map(usd),
        "duration_ms": u64::try_from(took.as_millis()).unwrap_or(u64::MAX),
        "outcome": outcome,
    });
    eprintln!("mycelium-lend: {line}");
}

/// Carries `lend` for `machine`, as far as it goes, sending its answer
/// on `sending`.
async fn lent(
    lender: &Lender,
    machine: &str,
    lend: Lend,
    sending: &mpsc::Sender<Lent>,
    used: &mut Used,
) -> Ended {
    let (asking, budget, limits) = match allowed(lender, machine, &lend, used) {
        Ok(allowed) => allowed,
        Err(refusal) => return Ended::Refused(refusal),
    };
    let day = match lender.count(machine, limits) {
        Ok(day) => day,
        Err(refusal) => return Ended::Refused(refusal),
    };
    let reserved = limits.spending.map_or(0, |spending| spending.reserved);
    let (ended, asked_whole) = carried(
        lender,
        machine,
        (&asking, &budget),
        sending,
        used,
        (&day, reserved),
    )
    .await;
    // One given back before it was asked whole was given back its
    // reservation too.
    if let Some(spending) = limits.spending
        && asked_whole
    {
        let charged = charged(&ended, used, spending);
        used.charged = Some(charged);
        used.spent_today = lender.settle(machine, &day, spending.reserved, charged);
    }
    ended
}

/// What a model request that `spending` limits is charged, as it `ended`:
/// what its answer said it used, once whole and done; nothing where the
/// upstream refused it whole, saying it used nothing; for an answer not
/// whole whose upstream stated its tokens asked, as a stream's
/// `message_start` does, those at their own prices and its `max_tokens`
/// answered, since it may have been made whole upstream but never runs
/// past its most; and otherwise all it reserved, as an answer cut short
/// before it stated them or whole but never saying what it used.
fn charged(ended: &Ended, used: &Used, spending: Spending) -> u64 {
    let Ended::Whole = ended else {
        return match used.meter.as_ref().and_then(Meter::asked) {
            Some(asked) => spending.price.cut_short(asked, spending.answered),
            None => spending.reserved,
        };
    };
    let refused_whole = used.status.is_some_and(|status| status >= 400);
    match used.meter.as_ref().and_then(Meter::usage) {
        Some(usage) => spending.price.charged(usage, spending.asked),
        None if refused_whole => 0,
        None => spending.reserved,
    }
}

/// Carries `asking` through `budget` for `machine`, counted on `day` with
/// what it `reserved`, as far as it goes, sending its answer on
/// `sending`: how it ended, and whether it was asked whole, never where
/// it was given back.
async fn carried(
    lender: &Lender,
    machine: &str,
    (asking, budget): (&Asking, &Budget),
    sending: &mpsc::Sender<Lent>,
    used: &mut Used,
    (day, reserved): (&str, u64),
) -> (Ended, bool) {
    let give_back = |ended: Ended| {
        lender.uncount(machine, day, reserved);
        (ended, false)
    };
    // Given back where its key can't be had or its relay can't be
    // reached or asked, so a lender's own outage spends nobody's day,
    // and where it was cut off or let go meanwhile: each before the whole
    // ask went upstream. Never after, since an answer that fails may
    // still have been made. The node's stop cuts it here too, so a key
    // slow to be had never holds the stop past its bound.
    let reaching = tokio::select! {
        reaching = reached(lender, budget) => reaching,
        () = lender.lends.cut() => return give_back(Ended::Cut),
    };
    let (stream, key) = match reaching {
        Ok(reached) => reached,
        Err(refusal) => return give_back(Ended::Refused(refusal)),
    };
    if let Some(why) = lender.cut_off_why(machine) {
        return give_back(Ended::Refused(refused(403, why)));
    }
    if sending.is_closed() {
        return give_back(Ended::Gone);
    }
    if lender.lends.is_cut() {
        return give_back(Ended::Cut);
    }
    // Named on the line only as it is sent, so a line refused before
    // then, by its count, a cut or anything else, names no upstream it
    // never asked.
    used.upstream = Some(budget.host.clone());
    // Watched while it is sent too, so a relay that stops taking it
    // can't hold the lend past a cut or its borrower letting go.
    let sent = tokio::select! {
        sent = tokio::time::timeout(
            SENT_WITHIN,
            asked(stream, key.as_deref(), asking, budget, machine),
        ) => sent,
        ended = watched(lender, machine, sending) => return give_back(ended),
    };
    let reading = match sent {
        Ok(Ok(reading)) => reading,
        Ok(Err(refusal)) => return give_back(Ended::Refused(refusal)),
        Err(_) => {
            return give_back(Ended::Refused(refused(
                504,
                format!(
                    "the model relay didn't take the request within {} minutes",
                    SENT_WITHIN.as_secs() / 60
                ),
            )));
        }
    };
    used.asked = 1;
    // Asked again with the same key on a fresh connection, and with the
    // same bound on taking the request whole. None where the relay can't
    // be reached or asked again, so the first answer is handed on.
    let again = async || {
        let stream = connected(lender, budget).await.ok()?;
        let sent = asked(stream, key.as_deref(), asking, budget, machine);
        tokio::time::timeout(SENT_WITHIN, sent).await.ok()?.ok()
    };
    let ended = tokio::select! {
        ended = answered(reading, again, sending, used) => ended,
        ended = watched(lender, machine, sending) => ended,
    };
    (ended, true)
}

/// How a lend being answered ends without its answer: its borrower
/// stopped reading, the node's stop cut it, or `machine` was cut off
/// meanwhile, which a lend finds within [`CUTS_LOOKED_AT`]. Never, while
/// none is so.
async fn watched(lender: &Lender, machine: &str, sending: &mpsc::Sender<Lent>) -> Ended {
    loop {
        tokio::select! {
            () = sending.closed() => return Ended::Gone,
            () = lender.lends.cut() => return Ended::Cut,
            () = tokio::time::sleep(CUTS_LOOKED_AT) => {
                if let Some(why) = lender.cut_off_why(machine) {
                    return Ended::Refused(refused(403, why));
                }
            }
        }
    }
}

/// Streams the relay's answer to `sending` as it comes: its head, its
/// body's pieces, then that it is done. A head of 421 is asked `again`
/// once, before anything of it is sent on, and the second answer is
/// streamed whatever it is; where it can't be asked again, the first is.
async fn answered(
    reading: BufReader<UnixStream>,
    again: impl AsyncFnOnce() -> Option<BufReader<UnixStream>>,
    sending: &mpsc::Sender<Lent>,
    used: &mut Used,
) -> Ended {
    let mut answer = match headed(reading).await {
        Ok(answer) => answer,
        Err(ended) => return ended,
    };
    // Only before the head is sent on, so the borrower never hears two
    // answers to one request.
    if answer.status == MISDIRECTED
        && let Some(reading) = again().await
    {
        used.asked += 1;
        answer = match headed(reading).await {
            Ok(answer) => answer,
            Err(ended) => return ended,
        };
    }
    let Headed {
        status,
        headers,
        framing,
        mut reading,
    } = answer;
    used.status = Some(status);
    if let Some(meter) = used.meter.as_mut() {
        meter.headed(&headers);
    }
    if sending.send(Lent::Head { status, headers }).await.is_err() {
        return Ended::Gone;
    }
    streamed(&mut reading, framing, sending, used).await
}

/// An answer whose head was read: its status, headers and framing, and
/// its body still to read.
struct Headed {
    status: u16,
    headers: Vec<(String, String)>,
    framing: Framing,
    reading: BufReader<UnixStream>,
}

/// The relay's answer on `reading`, its head read within
/// [`HEAD_WITHIN`]; else how the lend ends.
async fn headed(mut reading: BufReader<UnixStream>) -> Result<Headed, Ended> {
    match tokio::time::timeout(HEAD_WITHIN, head(&mut reading)).await {
        Ok(Ok((status, headers, framing))) => Ok(Headed {
            status,
            headers,
            framing,
            reading,
        }),
        Ok(Err(why)) => Err(Ended::Refused(refused(
            502,
            format!("the model's answer {why}"),
        ))),
        Err(_) => Err(Ended::Refused(refused(
            504,
            format!(
                "the model's answer didn't begin within {} minutes",
                HEAD_WITHIN.as_secs() / 60
            ),
        ))),
    }
}

/// A lend as it is carried on: its method, path, headers and body.
struct Asking {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Whether `machine` may ask `lend`: what is carried on, through which
/// budget, and how many requests a day it may make and what a model's
/// may spend.
fn allowed(
    lender: &Lender,
    machine: &str,
    lend: &Lend,
    used: &mut Used,
) -> Result<(Asking, Budget, Limits), Refusal> {
    let mut written = lender.written()?;
    if let Some(why) = lender.cut_off_why(machine) {
        return Err(refused(403, why));
    }
    let mut matching = written
        .vms
        .iter()
        .filter(|vm| machine.starts_with(vm.prefix.as_str()));
    let (Some(vm), None) = (matching.next(), matching.next()) else {
        let named = written
            .vms
            .iter()
            .filter(|vm| machine.starts_with(vm.prefix.as_str()))
            .count();
        return Err(refused(
            403,
            if named == 0 {
                format!("this machine lends nothing to {machine}")
            } else {
                format!("{machine} is named by {named} lends, so it is lent by none")
            },
        ));
    };
    let named = match lend.budget.as_deref() {
        None => vm.budget.clone(),
        Some(asked) if asked == vm.budget || vm.also.iter().any(|also| also == asked) => {
            asked.to_owned()
        }
        Some(asked) => {
            used.budget = Some(vm.budget.clone());
            return Err(refused(
                403,
                format!("{machine} may not borrow through the budget {asked}"),
            ));
        }
    };
    used.budget = Some(named.clone());
    let (most, usd_a_day) = (vm.requests_a_day, vm.usd_a_day);
    let budget = written.budgets.remove(&named).ok_or_else(|| {
        refused(
            403,
            format!("the budget {named} isn't in this machine's lend"),
        )
    })?;
    used.service = budget.kind;
    if !METHODS.contains(&lend.method.as_str()) {
        return Err(refused(403, format!("a lend may not ask {}", lend.method)));
    }
    if !(spoken(&lend.path) && budget.paths.iter().any(|path| within(&lend.path, path))) {
        return Err(refused(
            403,
            format!("{machine} may not ask for {}", lend.path.escape_debug()),
        ));
    }
    let mut headers = Vec::new();
    for (name, value) in &lend.headers {
        let name = name.to_ascii_lowercase();
        if !HANDED_ON.contains(&name.as_str()) {
            continue;
        }
        if !spoken(value) {
            return Err(refused(400, format!("the header {name} isn't one")));
        }
        headers.push((name, value.clone()));
    }
    let body = STANDARD
        .decode(&lend.body)
        .map_err(|_| refused(400, "a lend's body isn't base64"))?;
    if body.len() > MOST_BODY {
        return Err(refused(
            413,
            format!("a lend's body is at most {MOST_BODY} bytes"),
        ));
    }
    used.request_bytes = body.len();
    let spending = match budget.kind {
        Kind::Models => Some(spending(
            lender,
            machine,
            (&named, &budget),
            usd_a_day,
            &body,
            used,
        )?),
        Kind::Forge => None,
    };
    Ok((
        Asking {
            method: lend.method.clone(),
            path: lend.path.clone(),
            headers,
            body,
        },
        budget,
        Limits {
            requests: most,
            spending,
        },
    ))
}

/// What a model's request `body` through `budget`, `named`, may spend of
/// `machine`'s `usd_a_day`; refused where its machine has no cap, its
/// budget no spend-watch budget or spend-watch says no, or where it names
/// no model priced or no `max_tokens` within the model's most, since what
/// it may cost can't then be bounded.
fn spending(
    lender: &Lender,
    machine: &str,
    (named, budget): (&str, &Budget),
    usd_a_day: Option<f64>,
    body: &[u8],
    used: &mut Used,
) -> Result<Spending, Refusal> {
    let modelled = serde_json::from_slice::<Modelled>(body).ok();
    used.model = modelled
        .as_ref()
        .and_then(|modelled| modelled.model.clone());
    let most_a_day = usd_a_day.and_then(millionths).ok_or_else(|| {
        refused(
            403,
            format!("this machine's lend caps no spend of {machine}'s, so it lends it no model"),
        )
    })?;
    let watched = budget.spend_watch.as_deref().ok_or_else(|| {
        refused(
            403,
            format!("the budget {named} names none in spend-watch, so it lends no model"),
        )
    })?;
    let model = used
        .model
        .as_deref()
        .ok_or_else(|| refused(400, "a model's request names no model"))?;
    let price = budget
        .prices
        .get(model)
        .copied()
        .filter(|price| price.is_one())
        .ok_or_else(|| {
            refused(
                403,
                format!(
                    "the model {} has no price in this machine's lend",
                    model.escape_debug()
                ),
            )
        })?;
    let most = modelled
        .and_then(|modelled| modelled.max_tokens)
        .ok_or_else(|| refused(400, "a model's request names no max_tokens"))?;
    if most > price.most_output() {
        return Err(refused(
            400,
            format!(
                "a request of {model} may ask at most {} tokens, not {most}",
                price.most_output()
            ),
        ));
    }
    spend_watch_allows(lender.runtime.as_deref(), watched)?;
    let asked = u64::try_from(body.len()).unwrap_or(u64::MAX);
    used.meter = Some(Meter::default());
    Ok(Spending {
        most: most_a_day,
        reserved: price.reserved(asked, most),
        price,
        asked,
        answered: most,
    })
}

/// The model a request's body asks for, and the most tokens it may
/// answer with, where it names them.
#[derive(Deserialize)]
struct Modelled {
    model: Option<String>,
    max_tokens: Option<u64>,
}

/// Whether `said` can stand in a request's head as it is: printable,
/// with no space or line end in it but a header's inner spaces.
fn spoken(said: &str) -> bool {
    said.bytes()
        .all(|byte| byte == b' ' || byte.is_ascii_graphic())
        && !said.starts_with(' ')
        && !said.ends_with(' ')
}

/// Whether `path` is `allowed`, or below it where `allowed` ends `/*`,
/// and climbs out of nowhere: its path only letters, digits, `/`, `_`,
/// `-` and `.`, its query only those but `/` and with `=` and `&`, and
/// nothing else, so no relay can read it as climbing by a separator or
/// an escape of its own. Only exactly, unless asked, since below a
/// path a model serves can lie another that spends far more, as a batch
/// of thousands does below `/v1/messages`.
fn within(path: &str, allowed: &str) -> bool {
    let (bare, query) = path.split_once('?').unwrap_or((path, ""));
    let plain = |byte: u8| byte.is_ascii_alphanumeric() || b"_-.".contains(&byte);
    let lent = match allowed.strip_suffix("/*") {
        Some(above) => bare
            .strip_prefix(above)
            .is_some_and(|rest| rest.len() > 1 && rest.starts_with('/')),
        None => bare == allowed,
    };
    lent && bare.starts_with('/')
        && bare.bytes().all(|byte| plain(byte) || byte == b'/')
        && query
            .bytes()
            .all(|byte| plain(byte) || byte == b'=' || byte == b'&')
        && !bare.split('/').any(|part| part == ".." || part == ".")
}

/// `budget`'s key, had, and its upstream, reached; else why not.
async fn reached(
    lender: &Lender,
    budget: &Budget,
) -> Result<(UnixStream, Option<String>), Refusal> {
    let key = match &budget.key {
        Some(command) => Some(key(command).await?),
        None => None,
    };
    Ok((connected(lender, budget).await?, key))
}

/// `budget`'s upstream, reached on a connection of its own; else why not.
async fn connected(lender: &Lender, budget: &Budget) -> Result<UnixStream, Refusal> {
    let upstream = if budget.upstream.is_absolute() {
        budget.upstream.clone()
    } else {
        lender
            .runtime
            .as_ref()
            .ok_or_else(|| {
                refused(
                    503,
                    "this machine has no runtime folder to find its relay in",
                )
            })?
            .join(&budget.upstream)
    };
    UnixStream::connect(&upstream).await.map_err(|error| {
        refused(
            503,
            format!("this machine's model relay isn't answering: {error}"),
        )
    })
}

/// Asks `asking` of `budget`'s upstream over `stream`, with its `key`
/// added, and for a forge the `machine` that asked: the upstream's
/// answer, to be read.
async fn asked(
    mut stream: UnixStream,
    key: Option<&str>,
    asking: &Asking,
    budget: &Budget,
    machine: &str,
) -> Result<BufReader<UnixStream>, Refusal> {
    let mut said = format!(
        "{} {} HTTP/1.1\r\nhost: {}\r\n",
        asking.method, asking.path, budget.host
    );
    if let Some(key) = key {
        let _ = write!(said, "{}: {key}\r\n", budget.auth_header);
    }
    // Named by the link, never by what the borrower said, which hands on
    // no header of this name.
    if budget.kind == Kind::Forge && spoken(machine) {
        let _ = write!(said, "{BORROWER}: {machine}\r\n");
    }
    for (name, value) in &asking.headers {
        let _ = write!(said, "{name}: {value}\r\n");
    }
    let _ = write!(
        said,
        "content-length: {}\r\naccept-encoding: identity\r\nconnection: close\r\n\r\n",
        asking.body.len()
    );
    let sent = async {
        stream.write_all(said.as_bytes()).await?;
        stream.write_all(&asking.body).await?;
        stream.flush().await
    };
    sent.await.map_err(|error| {
        refused(
            502,
            format!("the request couldn't be sent to the model relay: {error}"),
        )
    })?;
    Ok(BufReader::new(stream))
}

/// The header's value a budget's key command prints, which is never
/// said, logged or handed to the borrower.
async fn key(command: &[String]) -> Result<String, Refusal> {
    let unhad = || refused(502, "this machine's key for the budget couldn't be had");
    let (program, words) = command.split_first().ok_or_else(unhad)?;
    if !Path::new(program).is_absolute() {
        return Err(unhad());
    }
    let run = tokio::process::Command::new(program)
        .args(words)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(KEY_WITHIN, run)
        .await
        .map_err(|_| unhad())?
        .map_err(|_| unhad())?;
    let printed = String::from_utf8(output.stdout).map_err(|_| unhad())?;
    let key = printed.trim_end_matches(['\r', '\n']);
    if !output.status.success() || key.is_empty() || !spoken(key) {
        return Err(unhad());
    }
    Ok(key.to_owned())
}

/// How an answer's body is told apart from what follows it.
enum Framing {
    /// In chunks, each saying its size.
    Chunked,
    /// Of a length.
    Length(usize),
    /// Until the relay hangs up.
    Closed,
}

/// An answer's status and headers, read whole, and how its body is
/// framed; else why it isn't one. An interim head, 1xx but 101, is
/// passed over for the one that follows it, and a 204 or 304 has no
/// body whatever its headers say.
async fn head<R: AsyncBufRead + Unpin>(
    reading: &mut R,
) -> Result<(u16, Vec<(String, String)>, Framing), &'static str> {
    let mut spent = 0;
    loop {
        let (status, headers, framing) = one_head(reading, &mut spent).await?;
        match status {
            100 | 102..=199 => {}
            204 | 304 => return Ok((status, headers, Framing::Length(0))),
            _ => return Ok((status, headers, framing)),
        }
    }
}

/// One head of an answer, counting what it took in `spent`.
async fn one_head<R: AsyncBufRead + Unpin>(
    reading: &mut R,
    spent: &mut usize,
) -> Result<(u16, Vec<(String, String)>, Framing), &'static str> {
    let status_line = line(reading, spent).await?;
    let mut words = status_line.splitn(3, ' ');
    let status = match (words.next(), words.next()) {
        (Some(version), Some(status)) if version.starts_with("HTTP/1.") => status
            .parse::<u16>()
            .ok()
            .filter(|status| (100..=599).contains(status)),
        _ => None,
    }
    .ok_or("isn't HTTP")?;
    let mut headers = Vec::new();
    let mut framing = Framing::Closed;
    loop {
        let said = line(reading, spent).await?;
        if said.is_empty() {
            return Ok((status, headers, framing));
        }
        let (name, value) = said.split_once(':').ok_or("has a header that isn't one")?;
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim().to_owned());
        match name.as_str() {
            "transfer-encoding" if value.to_ascii_lowercase().contains("chunked") => {
                framing = Framing::Chunked;
            }
            "content-length" if !matches!(framing, Framing::Chunked) => {
                framing =
                    Framing::Length(value.parse().map_err(|_| "has a length that isn't one")?);
            }
            _ => {}
        }
        headers.push((name, value));
    }
}

/// The next line of an answer's head, without its line end, counting
/// what it took against [`MOST_HEAD`].
async fn line<R: AsyncBufRead + Unpin>(
    reading: &mut R,
    spent: &mut usize,
) -> Result<String, &'static str> {
    let mut said = Vec::new();
    let room = u64::try_from(MOST_HEAD.saturating_sub(*spent)).unwrap_or(u64::MAX);
    let read = (&mut *reading)
        .take(room)
        .read_until(b'\n', &mut said)
        .await
        .map_err(|_| "couldn't be read")?;
    *spent += read;
    let said = said.strip_suffix(b"\n").ok_or(if *spent >= MOST_HEAD {
        "has a head too long"
    } else {
        "ended within its head"
    })?;
    let said = said.strip_suffix(b"\r").unwrap_or(said);
    String::from_utf8(said.to_vec()).map_err(|_| "has a head that isn't text")
}

/// Streams an answer's body to `sending` as it comes, then that it is
/// done.
async fn streamed(
    reading: &mut BufReader<UnixStream>,
    framing: Framing,
    sending: &mpsc::Sender<Lent>,
    used: &mut Used,
) -> Ended {
    let mut left = match framing {
        Framing::Length(length) => Some(length),
        Framing::Chunked | Framing::Closed => None,
    };
    let chunked = matches!(framing, Framing::Chunked);
    let mut piece = vec![0; PIECE];
    loop {
        if chunked && left.is_none_or(|left| left == 0) {
            if left == Some(0) {
                // The line end after the chunk just read.
                match unquiet(line(reading, &mut 0)).await {
                    Ok(Ok(end)) if end.is_empty() => {}
                    Ok(_) => return short(),
                    Err(ended) => return ended,
                }
            }
            let size = match unquiet(line(reading, &mut 0)).await {
                Ok(Ok(size)) => size,
                Ok(Err(_)) => return short(),
                Err(ended) => return ended,
            };
            let size = size.split(';').next().unwrap_or_default().trim();
            let Ok(size) = usize::from_str_radix(size, 16) else {
                return short();
            };
            if size == 0 {
                break;
            }
            left = Some(size);
        }
        let room = left.map_or(PIECE, |left| left.min(PIECE));
        if room == 0 {
            break;
        }
        let read = match unquiet(reading.read(&mut piece[..room])).await {
            Err(ended) => return ended,
            Ok(Ok(0)) if left.is_none() => break,
            Ok(Ok(0) | Err(_)) => return short(),
            Ok(Ok(read)) => read,
        };
        left = left.map(|left| left - read);
        used.response_bytes += read;
        if let Some(meter) = used.meter.as_mut() {
            meter.read(&piece[..read]);
        }
        let body = Lent::Body(STANDARD.encode(&piece[..read]));
        if sending.send(body).await.is_err() {
            return Ended::Gone;
        }
    }
    if let Some(meter) = used.meter.as_mut() {
        meter.ended();
    }
    if sending.send(Lent::Done).await.is_err() {
        return Ended::Gone;
    }
    Ended::Whole
}

/// An answer the relay ended before its last byte.
fn short() -> Ended {
    Ended::Refused(refused(502, "the model's answer ended short"))
}

/// What `reading` gives within [`QUIET_WITHIN`]; else the lend ends, as
/// an answer gone quiet.
async fn unquiet<T>(reading: impl Future<Output = T>) -> Result<T, Ended> {
    tokio::time::timeout(QUIET_WITHIN, reading)
        .await
        .map_err(|_| {
            Ended::Refused(refused(
                504,
                format!(
                    "the model's answer went quiet for {} minutes",
                    QUIET_WITHIN.as_secs() / 60
                ),
            ))
        })
}

/// Why a lend file doesn't parse, as the log may keep it: what kind of
/// fault and where, never the words found there, which may be a key
/// pasted in the wrong place.
fn unparsed(error: &serde_json::Error) -> String {
    unparsed_as(error, "a lend")
}

/// Why a file meant to hold `what` doesn't parse, as [`unparsed`] says it.
fn unparsed_as(error: &serde_json::Error, what: &str) -> String {
    let kind = match error.classify() {
        serde_json::error::Category::Io => "couldn't be read".to_owned(),
        serde_json::error::Category::Syntax => "isn't JSON".to_owned(),
        serde_json::error::Category::Data => format!("isn't {what}"),
        serde_json::error::Category::Eof => "ends early".to_owned(),
    };
    format!(
        "it {kind} at line {}, column {}",
        error.line(),
        error.column()
    )
}
