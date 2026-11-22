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
//! with no prompt, answer, header or key in it.

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

use crate::lend::serve::{HANDED_ON, MOST_BODY, MOST_HEAD};
use crate::lend::{BACKLOG, Lend, Lent};

/// The most of an answer's body one [`Lent::Body`] carries.
const PIECE: usize = 16 * 1024;

/// How often a lend streaming looks again at whether its machine was cut
/// off, so a cut ends it within this.
const CUTS_LOOKED_AT: Duration = Duration::from_secs(1);

/// How long a budget's key command has to say the key.
const KEY_WITHIN: Duration = Duration::from_secs(30);

/// How long the model has to begin its answer once it was asked: as
/// long as a whole answer not streamed may take, so a relay that takes
/// a lend and never answers ends the turn rather than hanging it.
const HEAD_WITHIN: Duration = Duration::from_secs(10 * 60);

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
    /// The budget their requests go through.
    budget: String,
    /// How many requests each may make a day.
    requests_a_day: u64,
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
}

/// What one lend's line says of it, as it goes.
#[derive(Default)]
struct Used {
    budget: Option<String>,
    model: Option<String>,
    upstream: Option<String>,
    request_bytes: usize,
    response_bytes: usize,
    status: Option<u16>,
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
        }
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
        Self::new(file, declarers, folder, crate::place::login_runtime())
    }

    /// What may be lent, as the file says now. Why it isn't taken goes to
    /// this machine's log alone: a parse error quotes what it couldn't
    /// read, which may be a key pasted in the wrong place, and the asker
    /// is another machine's agent.
    fn written(&self) -> Result<Written, Refusal> {
        crate::declared::sealed(&self.file, &self.declarers)
            .and_then(|()| fs::read(&self.file).map_err(|error| error.to_string()))
            .and_then(|text| serde_json::from_slice(&text).map_err(|error| error.to_string()))
            .map_err(|why| {
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

    /// Counts a request of `machine`'s against its `most` a day, refusing
    /// it once the day's are spent, and answering the day it counted in.
    fn count(&self, machine: &str, most: u64) -> Result<String, Refusal> {
        let day = jiff::Timestamp::now().strftime("%Y-%m-%d").to_string();
        self.counted(machine, &day, |made| {
            if *made >= most {
                return Err(refused(
                    429,
                    format!("{machine} has made its {most} requests today"),
                ));
            }
            *made += 1;
            Ok(())
        })?;
        Ok(day)
    }

    /// Gives back a request of `machine`'s counted on `day`, which the
    /// lender couldn't carry for a fault of its own; to that day, though
    /// another has begun since.
    fn uncount(&self, machine: &str, day: &str) {
        if let Err(refusal) = self.counted(machine, day, |made| {
            *made = made.saturating_sub(1);
            Ok(())
        }) {
            eprintln!(
                "mycelium: a request of {machine}'s can't be given back: {}",
                refusal.why
            );
        }
    }

    /// Changes `machine`'s count of `day`'s requests as `changing` says,
    /// keeping it whole: within this node by its lock, and across every
    /// node on the machine by a lock on the folder's `counts.lock`, so
    /// two lending nodes, which only one should be, lose no count.
    fn counted(
        &self,
        machine: &str,
        day: &str,
        changing: impl FnOnce(&mut u64) -> Result<(), Refusal>,
    ) -> Result<(), Refusal> {
        let _held = self
            .counting
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _locked = self
            .locked()
            .map_err(|error| refused(503, format!("the day's counts can't be locked: {error}")))?;
        let file = self.folder.join(format!("counts-{day}.json"));
        let mut counts: BTreeMap<String, u64> = match fs::read(&file) {
            Ok(text) => serde_json::from_slice(&text).map_err(|error| {
                refused(503, format!("the day's counts can't be read: {error}"))
            })?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => {
                return Err(refused(
                    503,
                    format!("the day's counts can't be read: {error}"),
                ));
            }
        };
        changing(counts.entry(machine.to_owned()).or_default())?;
        write_whole(&self.folder, &file, &counts)
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
/// ever cut.
fn cuts(folder: &Path) -> io::Result<Vec<String>> {
    match fs::read(folder.join("cuts.json")) {
        Ok(text) => serde_json::from_slice(&text).map_err(io::Error::other),
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
    let lender = lender.clone();
    let machine = machine.to_owned();
    tokio::spawn(async move {
        let began = Instant::now();
        let mut used = Used::default();
        let ended = lent(&lender, &machine, lend, &sending, &mut used).await;
        let outcome = match &ended {
            Ended::Whole => "sent".to_owned(),
            Ended::Refused(refusal) => format!("refused: {}", refusal.why),
            Ended::Gone => "let go: its borrower stopped reading".to_owned(),
        };
        if let Ended::Refused(Refusal { status, why }) = ended {
            used.status.get_or_insert(status);
            // A borrower gone meanwhile has nothing to be told.
            let _ = sending.send(Lent::Refused { status, why }).await;
        }
        say(&machine, &used, began.elapsed(), &outcome);
    });
    answers
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
        "service": "models",
        "budget": used.budget,
        "model": used.model,
        "upstream": used.upstream,
        "request_bytes": used.request_bytes,
        "response_bytes": used.response_bytes,
        "status": used.status,
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
    let (asking, budget, most) = match allowed(lender, machine, &lend, used) {
        Ok(allowed) => allowed,
        Err(refusal) => return Ended::Refused(refusal),
    };
    let day = match lender.count(machine, most) {
        Ok(day) => day,
        Err(refusal) => return Ended::Refused(refusal),
    };
    // Given back where its key can't be had or its relay can't be
    // reached or asked, so a lender's own outage spends nobody's day,
    // and where it was cut off meanwhile: each before the whole ask went
    // upstream. Never after, since an answer that fails may still have
    // been made.
    let (stream, key) = match reached(lender, &budget).await {
        Ok(reached) => reached,
        Err(refusal) => {
            lender.uncount(machine, &day);
            return Ended::Refused(refusal);
        }
    };
    if let Some(why) = lender.cut_off_why(machine) {
        lender.uncount(machine, &day);
        return Ended::Refused(refused(403, why));
    }
    let reading = match asked(stream, key, &asking, &budget).await {
        Ok(reading) => reading,
        Err(refusal) => {
            lender.uncount(machine, &day);
            return Ended::Refused(refusal);
        }
    };
    tokio::select! {
        ended = answered(reading, sending, used) => ended,
        ended = watched(lender, machine, sending) => ended,
    }
}

/// How a lend being answered ends without its answer: its borrower
/// stopped reading, or `machine` was cut off meanwhile, which a lend
/// finds within [`CUTS_LOOKED_AT`]. Never, while neither is so.
async fn watched(lender: &Lender, machine: &str, sending: &mpsc::Sender<Lent>) -> Ended {
    loop {
        tokio::select! {
            () = sending.closed() => return Ended::Gone,
            () = tokio::time::sleep(CUTS_LOOKED_AT) => {
                if let Some(why) = lender.cut_off_why(machine) {
                    return Ended::Refused(refused(403, why));
                }
            }
        }
    }
}

/// Streams the relay's answer to `sending` as it comes: its head, its
/// body's pieces, then that it is done.
async fn answered(
    mut reading: BufReader<UnixStream>,
    sending: &mpsc::Sender<Lent>,
    used: &mut Used,
) -> Ended {
    let (status, headers, framing) = match tokio::time::timeout(HEAD_WITHIN, head(&mut reading))
        .await
    {
        Ok(Ok(head)) => head,
        Ok(Err(why)) => return Ended::Refused(refused(502, format!("the model's answer {why}"))),
        Err(_) => {
            return Ended::Refused(refused(
                504,
                format!(
                    "the model's answer didn't begin within {} minutes",
                    HEAD_WITHIN.as_secs() / 60
                ),
            ));
        }
    };
    used.status = Some(status);
    if sending.send(Lent::Head { status, headers }).await.is_err() {
        return Ended::Gone;
    }
    streamed(&mut reading, framing, sending, used).await
}

/// A lend as it is carried on: its method, path, headers and body.
struct Asking {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Whether `machine` may ask `lend`: what is carried on, through which
/// budget, and how many requests a day it may make.
fn allowed(
    lender: &Lender,
    machine: &str,
    lend: &Lend,
    used: &mut Used,
) -> Result<(Asking, Budget, u64), Refusal> {
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
    used.budget = Some(vm.budget.clone());
    if let Some(asked) = lend.budget.as_deref()
        && asked != vm.budget
    {
        return Err(refused(
            403,
            format!("{machine} may not borrow through the budget {asked}"),
        ));
    }
    let most = vm.requests_a_day;
    let named = vm.budget.clone();
    let budget = written.budgets.remove(&named).ok_or_else(|| {
        refused(
            403,
            format!("the budget {named} isn't in this machine's lend"),
        )
    })?;
    used.upstream = Some(budget.host.clone());
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
    used.model = serde_json::from_slice::<Modelled>(&body)
        .ok()
        .and_then(|modelled| modelled.model);
    Ok((
        Asking {
            method: lend.method.clone(),
            path: lend.path.clone(),
            headers,
            body,
        },
        budget,
        most,
    ))
}

/// The model a request's body asks for, where it names one.
#[derive(Deserialize)]
struct Modelled {
    model: Option<String>,
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
    let stream = UnixStream::connect(&upstream).await.map_err(|error| {
        refused(
            503,
            format!("this machine's model relay isn't answering: {error}"),
        )
    })?;
    Ok((stream, key))
}

/// Asks `asking` of `budget`'s upstream over `stream`, with its `key`
/// added: the upstream's answer, to be read.
async fn asked(
    mut stream: UnixStream,
    key: Option<String>,
    asking: &Asking,
    budget: &Budget,
) -> Result<BufReader<UnixStream>, Refusal> {
    let mut said = format!(
        "{} {} HTTP/1.1\r\nhost: {}\r\n",
        asking.method, asking.path, budget.host
    );
    if let Some(key) = key {
        let _ = write!(said, "{}: {key}\r\n", budget.auth_header);
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
                if !matches!(line(reading, &mut 0).await, Ok(end) if end.is_empty()) {
                    return short();
                }
            }
            let Ok(size) = line(reading, &mut 0).await else {
                return short();
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
        let read = match reading.read(&mut piece[..room]).await {
            Ok(0) if left.is_none() => break,
            Ok(0) | Err(_) => return short(),
            Ok(read) => read,
        };
        left = left.map(|left| left - read);
        used.response_bytes += read;
        let body = Lent::Body(STANDARD.encode(&piece[..read]));
        if sending.send(body).await.is_err() {
            return Ended::Gone;
        }
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
