//! The holder: one process keeping one program's terminal, and serving
//! any terminal that joins it. Every joined terminal is given the modes
//! the program asked for and the title it set last, then sees the
//! program's output and types into it, and the program's size is the one
//! the latest to join or resize gave. Leaving is closing the connection;
//! the program runs on.

use std::fs;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pty_process::OwnedWritePty;
use rustix::process::{Pid, kill_process};
use tokio::io::AsyncWriteExt;
use tokio::io::unix::AsyncFd;
use tokio::net::UnixStream;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinSet;

use super::environment::{Environment, keeping_path};
use super::journal::{Ended, How, Journal};
use super::modes::Modes;
use super::steering::{Heard, Steers, Watching};
use super::title::Title;
use super::{Held, Said, Size, again, next, socket, steers, turn, wake};
use crate::node;
use crate::record::Window;

/// How many pieces of output a joined terminal may fall behind by before
/// it misses some; one that does is drawn whole again (`serve`).
const OUTPUT_BACKLOG: usize = 1024;

/// How long the program's last output may take to reach whoever is joined
/// once it has ended: a child it left running can keep its terminal open.
const LAST_WORDS: Duration = Duration::from_millis(500);

/// The size a program starts at, before any terminal has joined.
const FIRST_SIZE: Size = Size { rows: 24, cols: 80 };

/// The exit code a held program parks with, rather than ending: sysexits'
/// `EX_TEMPFAIL`, as it stops only until it is woken. An `i32`, as the
/// holder reads it from a child's `ExitStatus::code`.
pub const PARKED: i32 = 75;

/// The exit code `mycelium attach --host` ends with when its host let the
/// conversation go back to plain pi, as a release does, rather than 0 for
/// any other leaving. Whatever runs in the window next starts plain pi on
/// the conversation's log on this code alone, so the window it was viewed
/// in carries on as that pi. A `u8`, as attach ends with it through
/// `ExitCode::from`, which takes no other.
pub const RELEASED: u8 = 3;

/// The exit code `mycelium attach --host` ends with when its host stayed
/// gone past its patience for a restart: sysexits' `EX_UNAVAILABLE`, so
/// whatever runs in the window next waits for the host to come back,
/// rather than closing the window as it does for a person leaving.
pub const GONE: u8 = 69;

/// How often a parked holder looks for its wake file. A stat a second
/// costs nothing, and a message waking a session waits a second at most.
const WAKE_LOOK: Duration = Duration::from_secs(1);

/// What every joined window shows while its program is parked.
const PARKED_LINE: &[u8] = b"\r\nParked, its memory given back. A key or a message wakes it.\r\n";

/// Asks a window to say when it gains and loses focus, as xterm's mode
/// 1004 does, so a parked program wakes as its window comes in front of
/// the person, before they type (Joel, 2026-11-11, switching between
/// windows: "i'm not able to tell if any of them were not drawing ...
/// except for parked ones which stay parked").
const FOCUS_ON: &[u8] = b"\x1b[?1004h";

/// Stops a window saying when it gains and loses focus, as the program
/// starts again: one that wants it asks for it itself.
const FOCUS_OFF: &[u8] = b"\x1b[?1004l";

/// What a window says as it gains focus, and as it loses it.
const FOCUS_IN: &[u8] = b"\x1b[I";
const FOCUS_OUT: &[u8] = b"\x1b[O";

/// What a window typed, its focus reports taken out: whether it said it
/// gained focus, and the rest.
fn focus_of(typed: &[u8]) -> (bool, Vec<u8>) {
    let mut gained = false;
    let mut rest = Vec::with_capacity(typed.len());
    let mut at = 0;
    while at < typed.len() {
        let here = &typed[at..];
        if here.starts_with(FOCUS_IN) {
            gained = true;
            at += FOCUS_IN.len();
        } else if here.starts_with(FOCUS_OUT) {
            at += FOCUS_OUT.len();
        } else {
            rest.push(typed[at]);
            at += 1;
        }
    }
    (gained, rest)
}

/// What wakes a parked program, said again after a start that failed.
const PARKED_WAKES: &str = "A key or a message tries again.";

/// Variables a program's first terminal set that would name the wrong
/// window once it is held, since it outlives that window.
const WINDOW_VARIABLES: [&str; 5] = [
    "KITTY_WINDOW_ID",
    "KITTY_LISTEN_ON",
    "KITTY_PID",
    "WINDOWID",
    "MYCELIUM_APP_ID",
];

/// What to hold.
pub struct Holding<'a> {
    /// The realm's folder of held sessions.
    pub dir: &'a Path,
    /// The realm it is held in, which the program is in too.
    pub realm: &'a str,
    /// Its id.
    pub id: &'a str,
    /// The program and its arguments.
    pub command: &'a [String],
    /// The words each start of the program runs inside, the first and
    /// every one after a park: the login shell a node's start needs
    /// (`start::login_shell`), or none for a hold from a terminal, which
    /// has the person's environment already.
    pub through: &'a [String],
    /// The folder the program starts in.
    pub folder: &'a Path,
    /// The person's environment as it is now, which each start of the
    /// program is given over the holder's own.
    pub environment: &'a dyn Environment,
    /// Where each run's end is said.
    pub journal: &'a dyn Journal,
    /// What tells the holder to end its program, and so itself.
    pub ending: Ending,
    /// The node's socket, asked for a turn before a program that resumes
    /// a session first starts ([`turn`]); none starts it at once.
    pub node: Option<&'a Path>,
}

/// What tells a holder to end its program, and so itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// A terminate signal or a hang-up, as its service manager sends
    /// stopping its scope as the person logs out.
    Signalled,
    /// Nothing: it ends only as its program does, as a test's holder,
    /// sharing its process with others, must.
    Unsignalled,
}

/// The longest a program told to end, as its holder was, has to save what
/// it holds and go before it's killed: well inside the five seconds a
/// service manager gives a scope stopping as the person logs out.
const ENDING_GRACE: Duration = Duration::from_secs(3);

/// Hears what tells a holder to end.
struct Told {
    signals: Option<(Signal, Signal)>,
}

impl Told {
    fn listening(ending: Ending) -> io::Result<Self> {
        let signals = match ending {
            Ending::Signalled => Some((
                signal(SignalKind::terminate())?,
                signal(SignalKind::hangup())?,
            )),
            Ending::Unsignalled => None,
        };
        Ok(Self { signals })
    }

    /// What `until` ends with, or none when the holder is told to end
    /// first.
    async fn before<T>(&mut self, until: impl Future<Output = T>) -> Option<T> {
        tokio::select! {
            ended = until => Some(ended),
            () = self.told() => None,
        }
    }

    /// Resolves once the holder is told to end.
    async fn told(&mut self) {
        match &mut self.signals {
            Some((terminate, hangup)) => tokio::select! {
                _ = terminate.recv() => {}
                _ = hangup.recv() => {}
            },
            None => std::future::pending().await,
        }
    }
}

/// Ends the program as `pid`, its holder told to end: tells it to end too
/// and waits while it saves what it holds, its terminal still there to
/// write to, killing it only past [`ENDING_GRACE`]. A holder that went
/// first would take the terminal with it, and a program whose terminal
/// has gone, as pi's has, stops at once and saves nothing more.
async fn ends(child: &mut tokio::process::Child, pid: u32) -> io::Result<ExitStatus> {
    // A service manager tells the program itself as well, and one told
    // twice ends once.
    if let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) {
        let _ = kill_process(pid, rustix::process::Signal::TERM);
    }
    if let Ok(status) = tokio::time::timeout(ENDING_GRACE, child.wait()).await {
        return status;
    }
    child.start_kill()?;
    child.wait().await
}

/// The files a held program and whatever wakes it speak through.
struct Files {
    /// Wakes it once parked.
    waking: PathBuf,
    /// Where it may name its next command as it parks.
    naming: PathBuf,
    /// Where its next command is named for it from outside.
    chosen: PathBuf,
    /// Where it may say, as it parks, which steers wake it.
    steering: PathBuf,
}

impl Files {
    /// The files of the program held as `id` in `dir`, any left from before
    /// it ran taken away, as they say nothing to it.
    fn fresh(dir: &Path, id: &str) -> Self {
        let files = Self {
            waking: wake(dir, id),
            naming: again(dir, id),
            chosen: next(dir, id),
            steering: steers(dir, id),
        };
        for left in [&files.waking, &files.naming, &files.chosen, &files.steering] {
            let _ = fs::remove_file(left);
        }
        files
    }
}

/// How `command` starts, as `holding` says, told its wake, next-command
/// and steers files.
fn starting(
    holding: &Holding<'_>,
    command: &[String],
    files: &Files,
) -> io::Result<pty_process::Command> {
    let words: Vec<&String> = holding.through.iter().chain(command).collect();
    let Some((program, arguments)) = words.split_first() else {
        return Err(io::Error::other("there is nothing to start"));
    };
    // The person's environment goes first, so what the holder says and
    // takes away below is its own whatever that environment holds.
    let mut command = pty_process::Command::new(program)
        .args(arguments)
        .current_dir(holding.folder)
        .envs(keeping_path(
            holding.environment.now(),
            std::env::var("PATH").ok().as_deref(),
        ))
        .env("MYCELIUM_REALM", holding.realm)
        .env("MYCELIUM_HELD", holding.id)
        .env("MYCELIUM_WAKE", &files.waking)
        .env("MYCELIUM_AGAIN", &files.naming)
        .env("MYCELIUM_STEERS", &files.steering);
    for variable in WINDOW_VARIABLES {
        command = command.env_remove(variable);
    }
    Ok(command)
}

/// Holds the program until it ends, answering its exit code, if it gave
/// one. `ready` is told what is held once a terminal can join it. Each
/// start of the program has the person's environment as it is then, over
/// the one the holder was started with. The program is told its realm in `MYCELIUM_REALM`, whatever realm the
/// environment it was held from said, its id in `MYCELIUM_HELD`, the file that wakes it once
/// parked in `MYCELIUM_WAKE`, the file it may name its next command in
/// as it parks in `MYCELIUM_AGAIN`, and the file it may say where its
/// steers land in as it parks in `MYCELIUM_STEERS`, which wakes it too as
/// one lands that would give it a turn. A program exiting with [`PARKED`] is
/// started again on the same terminal when woken, with the command named
/// for it from outside if any was (see [`super::name_next`]), else with
/// the one it named itself; one that wrote its wake file before it went is restarting,
/// and starts again at once, never shown as parked. How each run ended,
/// parked or not, is said to the holding's journal.
#[expect(
    clippy::too_many_lines,
    reason = "a holder's one life, its first start to its end, reads best whole"
)]
pub async fn hold(
    holding: &Holding<'_>,
    ready: impl FnOnce(&Held) -> io::Result<()>,
) -> io::Result<Option<i32>> {
    if holding.command.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "there is nothing to hold: name a program",
        ));
    }
    // Heard from the first, so being told to end while the program starts
    // ends it as well, rather than the holder alone.
    let mut told = Told::listening(holding.ending)?;
    let socket = node::bind(&socket(holding.dir, holding.id))?;
    let files = Files::fresh(holding.dir, holding.id);
    let (pty, end, starts) = ChildEnd::open(FIRST_SIZE)?;
    // The holder keeps the terminal's other end, so a parked program's
    // terminal outlives it and the program starts again on the same one.
    let start =
        |command: &[String], size: Size| end.start(starting(holding, command, &files)?, size);
    let mut command = holding.command.to_vec();
    let waits = holding
        .node
        .and_then(|node| Some((node, turn::resumed(&command)?)));
    let first = match waits {
        Some(_) => None,
        None => Some(start(&command, FIRST_SIZE)?),
    };
    let held = Held {
        id: holding.id.to_owned(),
        command: holding.command.to_vec(),
        folder: holding.folder.to_owned(),
        holder: std::process::id(),
        pid: first
            .as_ref()
            .and_then(tokio::process::Child::id)
            .unwrap_or_default(),
        // One waiting for its turn runs no process yet, and a key wakes
        // it, as a parked one.
        parked: first.is_none(),
        since: jiff::Timestamp::now().to_string(),
        windows: Vec::new(),
    };
    let record = Arc::new(Record::write(holding.dir, &held)?);
    ready(&held)?;

    let reading = Reading::of(&pty)?;
    let (_, writing) = pty.into_split();
    let (output, _) = broadcast::channel(OUTPUT_BACKLOG);
    let terminal = Arc::new(Terminal {
        writing: tokio::sync::Mutex::new(writing),
        size: Mutex::new(FIRST_SIZE),
        drawn: output.clone(),
        parked: watch::Sender::new(false),
    });
    let shown = Arc::new(Shown {
        modes: Mutex::new(Modes::default()),
        title: Mutex::new(Title::default()),
        output,
        over: watch::Sender::new(false),
        waiting: watch::Sender::new(false),
    });
    let mut reader = tokio::spawn(read_output(reading, shown.clone(), starts));
    let mut joined = JoinSet::new();
    // A terminal may join whether the program runs or is parked.
    let serving = |stream| serve(stream, terminal.clone(), shown.clone(), record.clone());
    let first = match (first, waits) {
        (Some(child), _) => Some(child),
        (None, Some((node, session))) => {
            let waited = told.before(its_turn(node, &session, &terminal, &shown, &files));
            match joining_until(waited, &socket, &mut joined, &serving).await {
                Some(()) => {
                    let child = start(&command, terminal.size())?;
                    record.runs(child.id().unwrap_or_default(), &command);
                    Some(child)
                }
                None => None,
            }
        }
        (None, None) => None,
    };
    // Told to end as it waited, it ends having started nothing.
    let code = if let Some(mut child) = first {
        let status = 'held: loop {
            let pid = child.id().unwrap_or_default();
            let run = told.before(child.wait());
            let (status, telling) = match joining_until(run, &socket, &mut joined, &serving).await {
                Some(status) => (status?, false),
                None => (ends(&mut child, pid).await?, true),
            };
            ended(holding, pid, &command, status);
            // Told to end, a program that parked as it saved stays down.
            if telling || status.code() != Some(PARKED) {
                break status;
            }
            let mut restarting = restarting(&files.waking, &record, &shown);
            // One that can't start again stays parked, saying why, rather than
            // ending: its session, its window and its next wake are all still
            // here, and a command named again may start.
            child = loop {
                if !std::mem::take(&mut restarting) {
                    let woke = told.before(woken(&terminal, &files.waking, &files.steering));
                    // A parked program has saved all it holds, so being told
                    // to end then ends only the holder.
                    if joining_until(woke, &socket, &mut joined, &serving)
                        .await
                        .is_none()
                    {
                        break 'held status;
                    }
                    shown.show(FOCUS_OFF);
                }
                // Where its steers land is said again at each park, so what
                // this one said is spent whether a wake or a restart ends it.
                let _ = fs::remove_file(&files.steering);
                // Both are taken, so the program's own never waits for a
                // later start; the one named for it wins.
                let its_own = named(&files.naming);
                command = named(&files.chosen).or(its_own).unwrap_or(command);
                match start(&command, terminal.size()) {
                    Ok(child) => break child,
                    Err(error) => parks(&record, &shown, &unstarted(&error)),
                }
            };
            record.runs(child.id().unwrap_or_default(), &command);
        };
        status.code()
    } else {
        None
    };
    // The terminal's other end goes with the program, so its last output
    // ends as it did, then whoever is joined is let go.
    drop(end);
    drop(socket);
    record.end();
    if tokio::time::timeout(LAST_WORDS, &mut reader).await.is_err() {
        reader.abort();
    }
    // Each window holds what is shown as long as it's joined, so it is
    // told the program is over rather than left to find out, which it
    // never would: a holder told to end would wait out its windows'
    // patience below, half a second of a shutdown's five, for nothing.
    shown.ends();
    drop(shown);
    let _ = tokio::time::timeout(LAST_WORDS, async {
        while joined.join_next().await.is_some() {}
    })
    .await;
    Ok(code)
}

/// Waits until the node at `node` gives `session` its turn, or a key typed
/// in a joined window starts it now, each window joining meanwhile told
/// that it waits.
async fn its_turn(node: &Path, session: &str, terminal: &Terminal, shown: &Shown, files: &Files) {
    shown.waiting.send_replace(true);
    shown.show(WAITING_LINE);
    tokio::select! {
        () = turn::wait(node, session) => {}
        () = woken(terminal, &files.waking, &files.steering) => {}
    }
    // Its turn may have come while it watched for a key.
    terminal.parked.send_replace(false);
    shown.waiting.send_replace(false);
}

/// Says to the holding's journal that the run of `command` as `pid` ended
/// as `status`.
fn ended(holding: &Holding<'_>, pid: u32, command: &[String], status: ExitStatus) {
    holding.journal.ended(&Ended {
        id: holding.id.to_owned(),
        realm: holding.realm.to_owned(),
        pid,
        program: command.join(" "),
        how: How::of(status),
    });
}

/// Whether a program that exited as parked is restarting: one that wrote
/// its own wake before it went, as a pi handing its window to a pi host
/// does. It starts again at once, with no parked line drawn over its last
/// frame, and a key typed meanwhile reaches what starts rather than being
/// spent as a wake. Any other is parked, and its windows say so.
fn restarting(waking: &Path, record: &Record, shown: &Shown) -> bool {
    let restarting = fs::remove_file(waking).is_ok();
    if !restarting {
        parks(record, shown, PARKED_LINE);
    }
    restarting
}

/// What every joined window shows while its program waits for its turn
/// to resume, a key starting it at once. Focus wakes nothing here, as it
/// does a parked program: a restart opens every window, and each would
/// take its turn as it came in front.
const WAITING_LINE: &[u8] =
    b"\r\nWaiting its turn: sessions come back a few at a time. A key starts it now.\r\n";

/// What a parked program's windows say when it couldn't start again.
fn unstarted(error: &io::Error) -> Vec<u8> {
    format!("\r\nIt couldn't start again: {error}. {PARKED_WAKES}\r\n").into_bytes()
}

/// Says the program is parked, in its record and with `line` in every
/// joined window, each asked to say when it comes in front, which wakes
/// the program as a key does. A window joining while it's parked is
/// asked as it's given the modes.
fn parks(record: &Record, shown: &Shown, line: &[u8]) {
    record.parks();
    shown.show(&[FOCUS_ON, line].concat());
}

/// The command a parking program named in `naming`, a JSON list of its
/// program and arguments, taken away once read. One that names nothing a
/// program could be started as is no command, and the last one is kept.
fn named(naming: &Path) -> Option<Vec<String>> {
    let body = fs::read(naming).ok()?;
    let _ = fs::remove_file(naming);
    let command: Vec<String> = serde_json::from_slice(&body).ok()?;
    command
        .first()
        .is_some_and(|program| !program.is_empty())
        .then_some(command)
}

/// Serves each terminal that joins on `socket` until `until` ends,
/// answering what it ended with.
async fn joining_until<T, Serving: Future<Output = ()> + Send + 'static>(
    until: impl Future<Output = T>,
    socket: &node::Socket,
    joined: &mut JoinSet<()>,
    serving: &impl Fn(UnixStream) -> Serving,
) -> T {
    tokio::pin!(until);
    loop {
        tokio::select! {
            ended = &mut until => return ended,
            accepted = socket.accept() => {
                if let Ok(stream) = accepted {
                    joined.spawn(serving(stream));
                }
            }
            Some(_) = joined.join_next(), if !joined.is_empty() => {}
        }
    }
}

/// Waits while the program is parked, until a key is typed in a joined
/// terminal, the file `waking` appears, or a steer that wakes it lands in
/// an inbox it named in `steering`, then spends the wake.
async fn woken(terminal: &Terminal, waking: &Path, steering: &Path) {
    let mut parked = terminal.parked.subscribe();
    terminal.parked.send_replace(true);
    let steers = Steers::read(steering);
    let (mut watching, mut heard) = steers
        .as_ref()
        .and_then(Watching::of)
        .map_or((None, None), |(watching, heard)| {
            (Some(watching), Some(heard))
        });
    // Looked at once the watch is set, so a steer that landed as the
    // program parked, which no watch heard, wakes it too.
    let mut steered = watching.as_mut().is_some_and(Watching::arm);
    let mut look = tokio::time::interval(WAKE_LOOK);
    while !steered {
        tokio::select! {
            _ = parked.wait_for(|parked| !*parked) => break,
            _ = look.tick() => {
                if waking.exists() {
                    break;
                }
                // An inbox made since, or made again, is watched from now.
                steered = watching.as_mut().is_some_and(Watching::arm);
            }
            event = hearing(&mut heard) => {
                steered = watching
                    .as_mut()
                    .is_some_and(|watching| watching.heard(&event));
            }
        }
    }
    terminal.parked.send_replace(false);
    // The wake is spent once it has woken the program. One that can't be
    // taken away would only wake it again at its next park.
    let _ = fs::remove_file(waking);
}

/// What the watch of a parked program's inboxes hears next; never, for a
/// program with none watched.
async fn hearing(heard: &mut Option<mpsc::UnboundedReceiver<Heard>>) -> Heard {
    if let Some(heard) = heard
        && let Some(event) = heard.recv().await
    {
        return event;
    }
    std::future::pending().await
}

/// The longest a program joined at the size it has is kept one column
/// narrower, for one that draws nothing when its size changes.
const NUDGE_HELD: Duration = Duration::from_millis(500);

/// How long the narrower size stands once the program has drawn, so one
/// that was drawing for its own reasons as the nudge came still reads it.
const NUDGE_SEEN: Duration = Duration::from_millis(50);

/// The program's terminal, as every joined terminal shares it.
struct Terminal {
    writing: tokio::sync::Mutex<OwnedWritePty>,
    size: Mutex<Size>,
    /// What the program draws, to know it has drawn since a nudge.
    drawn: broadcast::Sender<Arc<[u8]>>,
    /// Whether the program is parked, so a key typed wakes it.
    parked: watch::Sender<bool>,
}

impl Terminal {
    /// Wakes a parked program, answering whether it was parked: the key
    /// that woke it is then not the program's to read.
    fn wakes(&self) -> bool {
        self.parked.send_if_modified(std::mem::take)
    }

    /// Whether the program is parked.
    fn is_parked(&self) -> bool {
        *self.parked.borrow()
    }

    /// The size the program was last given.
    fn size(&self) -> Size {
        *self.size.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Gives the program `size`. A terminal joining at the size the program
    /// already has nudges it one column narrower first, so the program is
    /// told of a change and draws itself again, as dtach's redraw does.
    /// A program reads its size once it is told it changed, and one told
    /// twice before it looks finds the size it had and draws nothing new,
    /// as pi does, so the narrower size stands until the program has drawn
    /// since.
    async fn resize(&self, size: Size, joining: bool) -> io::Result<()> {
        let writing = self.writing.lock().await;
        let was = std::mem::replace(
            &mut *self.size.lock().unwrap_or_else(PoisonError::into_inner),
            size,
        );
        if joining && was == size {
            let narrower = Size {
                cols: size.cols.saturating_sub(1).max(1),
                ..size
            };
            let mut drawn = self.drawn.subscribe();
            writing
                .resize(size_of(narrower))
                .map_err(io::Error::other)?;
            let _ = tokio::time::timeout(NUDGE_HELD, async {
                // Drawing too much to keep up with, or ended, it has drawn
                // all the same.
                let _ = drawn.recv().await;
                tokio::time::sleep(NUDGE_SEEN).await;
            })
            .await;
        }
        writing.resize(size_of(size)).map_err(io::Error::other)
    }
}

/// What the program shows, and the modes it asked for and the title it
/// set in showing it, as every joined terminal takes them.
struct Shown {
    modes: Mutex<Modes>,
    /// Only ever taken while `modes` is held, so the two are read as one.
    title: Mutex<Title>,
    output: broadcast::Sender<Arc<[u8]>>,
    /// Whether the program has shown all it will, its last output read.
    over: watch::Sender<bool>,
    /// Whether the program waits for its turn, so a window joining is
    /// told, as nothing else it is shown says so.
    waiting: watch::Sender<bool>,
}

impl Shown {
    /// Says the program has shown all it will, so each joined window is
    /// given what's left of it and let go.
    fn ends(&self) {
        self.over.send_replace(true);
    }

    /// Passes `bytes` to every joined terminal, reading them for modes
    /// and a title.
    fn show(&self, bytes: &[u8]) {
        let mut modes = self.modes.lock().unwrap_or_else(PoisonError::into_inner);
        modes.saw(bytes);
        self.title
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .saw(bytes);
        // With nobody joined there is nobody to show it to.
        let _ = self.output.send(Arc::from(bytes));
    }

    /// Whether the program asked to be told when its window gains and
    /// loses focus, or the holder did while it's parked.
    fn reports_focus(&self) -> bool {
        self.modes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_set(1004)
    }

    /// What a terminal joining now is given first, and its share of all
    /// that is shown after. Both are had under the lock `show` takes, so
    /// no mode or title is missed between them, or given twice. The title
    /// comes after the modes, as it is no mode.
    fn join(&self) -> (Vec<u8>, broadcast::Receiver<Arc<[u8]>>) {
        let modes = self.modes.lock().unwrap_or_else(PoisonError::into_inner);
        let mut first = modes.restored();
        first.extend(
            self.title
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .restored(),
        );
        if *self.waiting.borrow() {
            first.extend_from_slice(WAITING_LINE);
        }
        (first, self.output.subscribe())
    }
}

/// One joined terminal: it is given the program's modes, what it types
/// goes to the program, and the program's output comes back, until
/// either side ends. A terminal that falls so far behind that it misses
/// output is caught up as one joining is: given the modes again, and the
/// program nudged to draw itself whole, since what it missed would
/// otherwise leave a gap on its screen that nothing fills, such as a
/// typed letter the program had and the window never showed.
async fn serve(
    stream: UnixStream,
    terminal: Arc<Terminal>,
    shown: Arc<Shown>,
    record: Arc<Record>,
) {
    let (modes, mut output) = shown.join();
    let (mut reading, mut writing) = stream.into_split();
    let typing = async {
        let mut joining = true;
        // Its window leaves the record however the terminal goes.
        let mut window = Joined {
            record: &record,
            key: None,
        };
        while let Ok(Some(said)) = Said::read(&mut reading).await {
            let done = match said {
                Said::Typed(bytes) => typed(&terminal, &shown, bytes).await,
                Said::Size(size) => terminal.resize(size, std::mem::take(&mut joining)).await,
                Said::Window(said) => {
                    window.is(said);
                    Ok(())
                }
            };
            if done.is_err() {
                return;
            }
        }
    };
    let showing = async {
        if writing.write_all(&modes).await.is_err() {
            return;
        }
        let mut over = shown.over.subscribe();
        loop {
            let received = tokio::select! {
                // What was shown before the program was over goes first.
                biased;
                received = output.recv() => Some(received),
                _ = over.wait_for(|over| *over) => None,
            };
            let Some(received) = received else {
                showing_the_rest(&mut output, &mut writing).await;
                return;
            };
            match received {
                Ok(bytes) => {
                    if writing.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    // Said on the node's log, as a window that fell behind
                    // is a stall worth finding, never a silent gap.
                    eprintln!(
                        "mycelium: a window of {} fell {missed} pieces of output behind, \
                         so its program draws it whole again",
                        record.kept().held.id
                    );
                    // What is still waiting is older than the redraw to
                    // come, so it is let go with what was missed.
                    let (modes, caught) = shown.join();
                    output = caught;
                    if writing.write_all(&modes).await.is_err()
                        || terminal.resize(terminal.size(), true).await.is_err()
                    {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    };
    tokio::select! {
        () = typing => {}
        () = showing => {}
    }
}

/// Gives a window what was shown and it hasn't had yet, the program over,
/// so its last words reach it before it's let go. A window that fell
/// behind as the program ended is given what's left after what it missed.
async fn showing_the_rest(
    output: &mut broadcast::Receiver<Arc<[u8]>>,
    writing: &mut (impl AsyncWriteExt + Unpin),
) {
    loop {
        match output.try_recv() {
            Ok(bytes) => {
                if writing.write_all(&bytes).await.is_err() {
                    return;
                }
            }
            Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => return,
        }
    }
}

/// Takes what a window typed. A parked program is woken by a key or by
/// its window coming in front, and what woke it isn't the program's to
/// read; a window going behind wakes nothing. A running program is given
/// what was typed, without the focus reports it didn't ask for, which
/// it would read as keys.
async fn typed(terminal: &Terminal, shown: &Shown, bytes: Vec<u8>) -> io::Result<()> {
    let (gained, rest) = focus_of(&bytes);
    if terminal.is_parked() {
        if gained || !rest.is_empty() {
            terminal.wakes();
        }
        return Ok(());
    }
    let given = if shown.reports_focus() { bytes } else { rest };
    if given.is_empty() {
        return Ok(());
    }
    terminal.writing.lock().await.write_all(&given).await
}

/// Passes the program's output to every joined terminal until it ends.
async fn read_output(mut reading: Reading, shown: Arc<Shown>, starts: impl Again) {
    let mut buffer = vec![0u8; 8192];
    // A terminal whose program has gone reads as an error on Linux and as
    // its end elsewhere; there is no more unless a start opens it again.
    loop {
        match reading.read(&mut buffer).await {
            Ok(read) if read > 0 => shown.show(&buffer[..read]),
            _ => {
                if !starts.again().await {
                    return;
                }
                reading.started();
            }
        }
    }
}

/// The terminal's parent end, as the program's output is read from it,
/// watched afresh at each start. Once the child end closes, the parent
/// end reads as ended to a watch begun before then, for good: a start
/// that opens the child end again leaves such a watch finding it ready
/// with nothing to read, at every turn. A watch begun after the start
/// waits for what the program shows.
struct Reading {
    parent: OwnedFd,
    watched: Option<AsyncFd<OwnedFd>>,
}

impl Reading {
    fn of(pty: &pty_process::Pty) -> io::Result<Self> {
        let parent = pty.as_fd().try_clone_to_owned()?;
        let watched = Some(watching(&parent)?);
        Ok(Self { parent, watched })
    }

    /// Watches the parent end afresh, as a start has opened its child end
    /// again. One that can't be watched reads as ended until the next,
    /// said on the holder's log, as output nobody sees is a gap worth
    /// finding.
    fn started(&mut self) {
        // The old watch goes first, so the two never overlap.
        self.watched = None;
        self.watched = watching(&self.parent)
            .inspect_err(|error| {
                eprintln!(
                    "mycelium: a holder can't watch its terminal, so what this start \
                     shows goes unseen: {error}"
                );
            })
            .ok();
    }

    /// Reads what the program shows into `buffer`, nothing once the
    /// terminal reads as ended to this watch.
    async fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
        let Some(watched) = &self.watched else {
            return Err(io::ErrorKind::NotConnected.into());
        };
        loop {
            let mut ready = watched.readable().await?;
            let ended = ready.ready().is_read_closed();
            match ready.try_io(|fd| Ok(rustix::io::read(fd, &mut *buffer)?)) {
                Ok(read) => return read,
                // Ended with nothing to read, it stays ended to this watch,
                // however long the program runs on an end opened again.
                Err(_) if ended => return Ok(0),
                Err(_) => {}
            }
        }
    }
}

/// Watches a copy of the terminal's parent end, from now on.
fn watching(parent: &OwnedFd) -> io::Result<AsyncFd<OwnedFd>> {
    let copy = parent.try_clone()?;
    // SAFETY: an `OwnedFd` is open, owned and the same descriptor for as
    // long as it lives, and the watch owns this one until it is dropped,
    // as `register` asks.
    #[expect(unsafe_code, reason = "tokio watches a descriptor only by register")]
    let watched = unsafe { AsyncFd::register(copy) };
    watched.map_err(|refused| refused.into_parts().1)
}

// The terminal's child end, which each start of the program is given,
// kept as its platform keeps it: Linux's in `kept`, macOS's in
// `reopened`. Each opens the terminal with its child end, answering what
// tells the reader of the program's output of each start.
#[cfg(not(target_os = "macos"))]
use kept::ChildEnd;
#[cfg(target_os = "macos")]
use reopened::ChildEnd;

/// What the reader of the program's output is told of each start by, as
/// each platform's child end tells it.
trait Again: Send + 'static {
    /// Whether output may come once the terminal has read as ended.
    fn again(&self) -> impl Future<Output = bool> + Send;
}

/// The child end as Linux has it: a program starts on the same end again
/// and again, and the holder keeping it open is what keeps a parked
/// program's terminal.
#[cfg(not(target_os = "macos"))]
mod kept {
    use std::io;

    use super::{Again, Size, size_of};

    /// The terminal's child end, open for as long as the holder holds it.
    pub(super) struct ChildEnd {
        pts: pty_process::Pts,
    }

    impl ChildEnd {
        /// Opens a terminal at `size`, answering its parent end, its child
        /// end kept open for every start, and that output never comes once
        /// it has read as ended.
        pub(super) fn open(size: Size) -> io::Result<(pty_process::Pty, Self, Never)> {
            let (pty, pts) = pty_process::open().map_err(io::Error::other)?;
            pty.resize(size_of(size)).map_err(io::Error::other)?;
            Ok((pty, Self { pts }, Never))
        }

        /// Starts `command` on the end at `size`, which the end, never
        /// closed, keeps from the last start.
        pub(super) fn start(
            &self,
            mut command: pty_process::Command,
            _size: Size,
        ) -> io::Result<tokio::process::Child> {
            command.spawn_borrowed(&self.pts).map_err(io::Error::other)
        }
    }

    /// No start opens the terminal again, as its child end stays open
    /// until the program is done.
    pub(super) struct Never;

    impl Again for Never {
        /// Never: the terminal reads as ended only once the holder has let
        /// the child end go.
        fn again(&self) -> impl Future<Output = bool> + Send {
            std::future::ready(false)
        }
    }
}

/// The child end as macOS has it: it takes a terminal away from whatever
/// holds it once its session's leader ends, and pty-process starts on a
/// borrowed end only elsewhere, so each start opens the end again by its
/// name, as `pty_process::open` opens it.
#[cfg(target_os = "macos")]
mod reopened {
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{Again, Size, size_of};

    /// The terminal's child end by its name, opened anew at each start.
    pub(super) struct ChildEnd {
        named: std::ffi::CString,
        starts: Arc<Starts>,
    }

    impl ChildEnd {
        /// Opens a terminal at `size`, answering its parent end, its child
        /// end by name, closed until a start opens it anew, and each start
        /// as the reader of its output is told of it.
        pub(super) fn open(size: Size) -> io::Result<(pty_process::Pty, Self, Arc<Starts>)> {
            let (pty, pts) = pty_process::open().map_err(io::Error::other)?;
            // Sized while its child end is still open, as a start opens it
            // anew at its own size.
            pty.resize(size_of(size)).map_err(io::Error::other)?;
            let named = rustix::termios::ttyname(&pts, Vec::new())?;
            let starts = Arc::new(Starts::default());
            let end = Self {
                named,
                starts: starts.clone(),
            };
            Ok((pty, end, starts))
        }

        /// Starts `command` on the end opened anew at `size`, since macOS
        /// forgets a terminal's size once nothing holds its child end.
        pub(super) fn start(
            &self,
            command: pty_process::Command,
            size: Size,
        ) -> io::Result<tokio::process::Child> {
            use rustix::fs::{Mode, OFlags};
            let flags = OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC;
            let opened = rustix::fs::open(self.named.as_c_str(), flags, Mode::empty())?;
            rustix::termios::tcsetwinsize(
                &opened,
                rustix::termios::Winsize {
                    ws_row: size.rows,
                    ws_col: size.cols,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                },
            )?;
            // SAFETY: `opened` was just opened by this terminal's child end's
            // own name, so it is valid, open and the child end of a pty, as
            // `from_fd` asks.
            #[expect(
                unsafe_code,
                reason = "pty-process makes a Pts of an end opened again only by from_fd"
            )]
            let pts = unsafe { pty_process::Pts::from_fd(opened) };
            let child = command.spawn(pts).map_err(io::Error::other)?;
            self.starts.started.notify_one();
            Ok(child)
        }
    }

    /// The end goes as the holder is done with its program, so the reader
    /// of its output is told no start is coming, rather than waiting out
    /// its last words for one.
    impl Drop for ChildEnd {
        fn drop(&mut self) {
            self.starts.over.store(true, Ordering::SeqCst);
            self.starts.started.notify_one();
        }
    }

    /// Each start of the program, as the reader of its output is told of it.
    #[derive(Default)]
    pub(super) struct Starts {
        started: tokio::sync::Notify,
        /// Whether the holder is done starting the program.
        over: AtomicBool,
    }

    impl Again for Arc<Starts> {
        /// Once the program starts again, on its end opened anew, and never
        /// once the holder is done starting it.
        async fn again(&self) -> bool {
            if self.over.load(Ordering::SeqCst) {
                return false;
            }
            // Being done tells it as a start does, so it's heard here too.
            self.started.notified().await;
            !self.over.load(Ordering::SeqCst)
        }
    }
}

fn size_of(size: Size) -> pty_process::Size {
    pty_process::Size::new(size.rows, size.cols)
}

/// The record beside a holder's socket, written again as windows join and
/// leave, and taken away once the holder ends or is dropped.
struct Record {
    path: PathBuf,
    staged: PathBuf,
    kept: Mutex<Kept>,
}

/// What the record says, and each joined window by its terminal's key.
struct Kept {
    held: Held,
    windows: Vec<(u64, Window)>,
    next: u64,
    ended: bool,
}

impl Record {
    fn write(dir: &Path, held: &Held) -> io::Result<Self> {
        let record = Self {
            path: dir.join(format!("{}.json", held.id)),
            staged: dir.join(format!(".{}.json.tmp", held.id)),
            kept: Mutex::new(Kept {
                held: held.clone(),
                windows: Vec::new(),
                next: 0,
                ended: false,
            }),
        };
        record.save(&record.kept())?;
        Ok(record)
    }

    fn kept(&self) -> std::sync::MutexGuard<'_, Kept> {
        self.kept.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Writes the record whole, through a staged file renamed into place,
    /// unless the holder has ended.
    fn save(&self, kept: &Kept) -> io::Result<()> {
        if kept.ended {
            return Ok(());
        }
        let held = Held {
            windows: kept
                .windows
                .iter()
                .map(|(_, window)| window.clone())
                .collect(),
            ..kept.held.clone()
        };
        fs::write(
            &self.staged,
            serde_json::to_vec(&held).map_err(io::Error::other)?,
        )?;
        fs::rename(&self.staged, &self.path)
    }

    /// Adds a joined terminal's window, the latest, answering its key.
    fn joined(&self, window: Window) -> u64 {
        let mut kept = self.kept();
        let key = kept.next;
        kept.next += 1;
        kept.windows.push((key, window));
        // A record that can't be written keeps what it said before; the
        // next join or leave writes it whole again.
        let _ = self.save(&kept);
        key
    }

    /// Takes away the window of the terminal keyed `key`, which has left.
    fn left(&self, key: u64) {
        let mut kept = self.kept();
        kept.windows.retain(|(each, _)| *each != key);
        // As in `joined`, the next change writes it whole again.
        let _ = self.save(&kept);
    }

    /// Says the program is parked, with no process of its own.
    fn parks(&self) {
        let mut kept = self.kept();
        kept.held.pid = 0;
        kept.held.parked = true;
        // As in `joined`, the next change writes it whole again.
        let _ = self.save(&kept);
    }

    /// Says the program runs again, as `pid`, started with `command`.
    fn runs(&self, pid: u32, command: &[String]) {
        let mut kept = self.kept();
        kept.held.pid = pid;
        command.clone_into(&mut kept.held.command);
        kept.held.parked = false;
        // As in `joined`, the next change writes it whole again.
        let _ = self.save(&kept);
    }

    /// Takes the record away for good, so no terminal leaving after can
    /// write it back.
    fn end(&self) {
        self.kept().ended = true;
        // The record is this holder's alone; if it's already gone there is
        // nothing to take away.
        let _ = fs::remove_file(&self.path);
    }
}

impl Drop for Record {
    fn drop(&mut self) {
        self.end();
    }
}

/// A joined terminal's window in the record, taken away when dropped.
struct Joined<'a> {
    record: &'a Record,
    key: Option<u64>,
}

impl Joined<'_> {
    /// The terminal says it is `window`, in place of any it said before.
    fn is(&mut self, window: Window) {
        if let Some(key) = self.key.take() {
            self.record.left(key);
        }
        self.key = Some(self.record.joined(window));
    }
}

impl Drop for Joined<'_> {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.record.left(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
    use tokio::sync::{mpsc, oneshot};
    use tokio::task::JoinHandle;

    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::super::environment::Given;
    use super::super::journal::SystemJournal;
    use super::super::{attach, held as listed, name_next, next, steers};
    use super::*;

    /// How long a test waits for what it expects to show before failing.
    /// Each wait ends as soon as it is met, so only a failure waits this
    /// long; a loaded Mac ran these tests nearly seven times slower than
    /// Linux and drew the flood test's window past 20 s three times
    /// (2026-11-16).
    const PATIENCE: Duration = Duration::from_secs(60);

    /// A terminal joined to a holder: what it types, and what it shows.
    struct Joined {
        typing: DuplexStream,
        shown: DuplexStream,
        sizes: mpsc::UnboundedSender<Size>,
        seen: String,
        task: JoinHandle<io::Result<()>>,
    }

    impl Joined {
        fn join(socket: PathBuf, size: Size, window: Option<Window>) -> Self {
            let (typing, input) = tokio::io::duplex(4096);
            let (output, shown) = tokio::io::duplex(1 << 16);
            let (sizes, told) = mpsc::unbounded_channel();
            let task = tokio::spawn(async move {
                attach::join(&socket, size, window, input, output, told).await
            });
            Self {
                typing,
                shown,
                sizes,
                seen: String::new(),
                task,
            }
        }

        async fn types(&mut self, text: &str) {
            self.typing
                .write_all(text.as_bytes())
                .await
                .expect("the terminal types");
        }

        /// Waits until the terminal has shown `wanted`, failing past the
        /// test's patience with what it did show.
        async fn sees(&mut self, wanted: &str) {
            let mut buffer = [0u8; 4096];
            let found = tokio::time::timeout(PATIENCE, async {
                while !self.seen.contains(wanted) {
                    let read = self.shown.read(&mut buffer).await.expect("output read");
                    if read == 0 {
                        return false;
                    }
                    self.seen
                        .push_str(&String::from_utf8_lossy(&buffer[..read]));
                }
                true
            })
            .await;
            // Only the end of what it showed, which a flood makes long.
            let shown = self.seen.chars().rev().take(2000).collect::<String>();
            assert!(
                matches!(found, Ok(true)),
                "the terminal never showed {wanted:?}; it showed, ending, {:?}",
                shown.chars().rev().collect::<String>()
            );
        }
    }

    /// Waits until `path` exists, failing past the test's patience.
    async fn appears(path: &Path) {
        let mut look = tokio::time::interval(Duration::from_millis(20));
        let found = tokio::time::timeout(PATIENCE, async {
            while !path.exists() {
                look.tick().await;
            }
        })
        .await;
        assert!(found.is_ok(), "{} never appeared", path.display());
    }

    /// Holds `sh` in a new folder of held sessions, answering the folder,
    /// the record once it can be joined, and the holder's end.
    async fn holding_sh(root: &Path) -> (PathBuf, Held, JoinHandle<io::Result<Option<i32>>>) {
        holding_sh_through(root, Vec::new()).await
    }

    /// Holds `sh` as [`holding_sh`] does, each start run inside `through`.
    async fn holding_sh_through(
        root: &Path,
        through: Vec<String>,
    ) -> (PathBuf, Held, JoinHandle<io::Result<Option<i32>>>) {
        holding_sh_in(root, through, Given(Vec::new())).await
    }

    /// An environment that has changed at every start, telling which
    /// start it is in `TRIAL_START`.
    #[derive(Default)]
    struct Changing(AtomicUsize);

    impl Environment for Changing {
        fn now(&self) -> Vec<(String, String)> {
            let start = self.0.fetch_add(1, Ordering::SeqCst) + 1;
            vec![("TRIAL_START".to_owned(), start.to_string())]
        }
    }

    /// Holds `sh` as [`holding_sh_through`] does, each start given
    /// `environment` as the person's.
    async fn holding_sh_in(
        root: &Path,
        through: Vec<String>,
        environment: impl Environment + 'static,
    ) -> (PathBuf, Held, JoinHandle<io::Result<Option<i32>>>) {
        holding_in(root, through, environment, vec!["sh".to_owned()], None).await
    }

    /// Holds `command` as [`holding_sh_in`] does `sh`, asking the node at
    /// `node`, if any, for a turn before a command resuming a session
    /// starts.
    async fn holding_in(
        root: &Path,
        through: Vec<String>,
        environment: impl Environment + 'static,
        command: Vec<String>,
        node: Option<PathBuf>,
    ) -> (PathBuf, Held, JoinHandle<io::Result<Option<i32>>>) {
        let dir = root.join("held");
        let folder = root.to_owned();
        // Nothing listens here, so each end is said on standard error.
        let root_journal = root.join("journal");
        let (told, ready) = oneshot::channel();
        let holder = {
            let dir = dir.clone();
            tokio::spawn(async move {
                let holding = Holding {
                    dir: &dir,
                    realm: "harbour",
                    id: "trial1",
                    command: &command,
                    through: &through,
                    folder: &folder,
                    environment: &environment,
                    journal: &SystemJournal::at(&root_journal),
                    ending: Ending::Unsignalled,
                    node: node.as_deref(),
                };
                hold(&holding, |held| {
                    let _ = told.send(held.clone());
                    Ok(())
                })
                .await
            })
        };
        let held = ready.await.expect("the holder says it holds");
        (dir, held, holder)
    }

    /// A terminal whose child end has closed, read as ended, and opened
    /// again by its name, as a Mac's holder does at each start, read
    /// through a watch begun before the close, with what the opened end
    /// writes to.
    async fn reopened_terminal() -> (Reading, OwnedFd) {
        use rustix::fs::{Mode, OFlags};
        let (pty, pts) = pty_process::open().expect("a terminal");
        let named = rustix::termios::ttyname(&pts, Vec::new()).expect("its child end's name");
        let reading = Reading::of(&pty).expect("its parent end, watched");
        drop(pts);
        // Linux reads the end as an error and macOS as nothing.
        let mut buffer = [0u8; 64];
        let ended = tokio::time::timeout(PATIENCE, reading.read(&mut buffer))
            .await
            .expect("the closed end reads as ended");
        assert!(
            ended.as_ref().map_or(true, |read| *read == 0),
            "nothing is read: {ended:?}"
        );
        let flags = OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC;
        let opened = rustix::fs::open(named.as_c_str(), flags, Mode::empty())
            .expect("the child end, opened again");
        // The parent end goes on in the watch's own copy.
        drop(pty);
        (reading, opened)
    }

    /// Counts how often a read asks to be polled again.
    struct Wakes(AtomicUsize);

    impl std::task::Wake for Wakes {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Polls `reading` once for what it reads, and counts the wakes it asks
    /// for: a read with nothing to read waits for the terminal to wake it.
    fn read_once(reading: &Reading) -> (std::task::Poll<io::Result<usize>>, usize) {
        let counted = Arc::new(Wakes(AtomicUsize::new(0)));
        let waker = std::task::Waker::from(counted.clone());
        let mut buffer = [0u8; 64];
        let mut read = std::pin::pin!(reading.read(&mut buffer));
        let polled = read
            .as_mut()
            .poll(&mut std::task::Context::from_waker(&waker));
        (polled, counted.0.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn a_watch_from_before_a_start_reads_the_terminal_ended() {
        let (reading, _opened) = reopened_terminal().await;
        let mut buffer = [0u8; 64];
        let read = tokio::time::timeout(PATIENCE, reading.read(&mut buffer))
            .await
            .expect("the old watch reads it ended, rather than ready with nothing");
        assert_eq!(read.expect("the read succeeds"), 0, "nothing is read");
    }

    #[tokio::test]
    async fn a_terminal_opened_again_waits_quietly_for_output() {
        let (mut reading, opened) = reopened_terminal().await;
        reading.started();
        // Reading what it shows first lets the runtime hear all the
        // terminal has said, its child end's close included.
        rustix::io::write(&opened, b"shown").expect("the program shows something");
        let mut buffer = [0u8; 64];
        let read = tokio::time::timeout(PATIENCE, reading.read(&mut buffer))
            .await
            .expect("what it shows is read")
            .expect("the read succeeds");
        assert_eq!(&buffer[..read], b"shown");

        let (polled, wakes) = read_once(&reading);
        assert!(polled.is_pending(), "nothing more is shown: {polled:?}");
        assert_eq!(wakes, 0, "an idle terminal never wakes its reader");
    }

    #[tokio::test]
    async fn a_held_program_outlives_the_terminal_that_left_it() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let (dir, held, holder) = holding_sh(root.path()).await;
        assert_eq!(listed(&dir), vec![held.clone()], "what is held is listed");

        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut first = Joined::join(socket(&dir, &held.id), size, None);
        first
            .types("echo \"[$MYCELIUM_HELD in $MYCELIUM_REALM] $((40 + 2))\"\r")
            .await;
        first.sees("[trial1 in harbour] 42").await;
        first.task.abort();
        let _ = first.task.await;

        let mut again = Joined::join(socket(&dir, &held.id), size, None);
        again.types("stty size; echo still $((6 * 7))\r").await;
        again.sees("30 100").await;
        again.sees("still 42").await;
        again
            .sizes
            .send(Size {
                rows: 40,
                cols: 120,
            })
            .expect("a new size is told");
        again.types("stty size\r").await;
        again.sees("40 120").await;

        again.types("exit 3\r").await;
        let ended = tokio::time::timeout(PATIENCE, holder)
            .await
            .expect("the holder ends with its program")
            .expect("the holder's task ran");
        assert_eq!(
            ended.expect("it held"),
            Some(3),
            "it answers the program's exit code"
        );
        let left = tokio::time::timeout(PATIENCE, again.task)
            .await
            .expect("the terminal is let go");
        assert!(
            matches!(left, Ok(Ok(()))),
            "a terminal joined to an ended program is let go: {left:?}"
        );
        assert!(listed(&dir).is_empty(), "nothing is listed once it ended");
        assert!(
            !socket(&dir, "trial1").exists() && !dir.join("trial1.json").exists(),
            "its socket and record are gone"
        );
    }

    #[tokio::test]
    async fn a_program_joined_at_the_size_it_has_is_told_its_size_changed() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let (dir, held, _holder) = holding_sh(root.path()).await;
        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut first = Joined::join(socket(&dir, &held.id), size, None);
        // It reads its size only when told, after the sleep it is in, as a
        // program busy drawing reads it after its frame. A shell of its
        // own keeps no job control, so its sleeps share its group and the
        // terminal's word of a new size reaches it every time.
        first
            .types("sh -c 'trap \"stty size\" WINCH; echo $((6 * 7)) set; while :; do sleep 0.05; done'\r")
            .await;
        first.sees("42 set").await;
        first.task.abort();
        let _ = first.task.await;

        // Told of the nudge and the size back, the program may read only
        // the last, as both can come within one of its sleeps: that it
        // reads its size at all is the word reaching it.
        let mut again = Joined::join(socket(&dir, &held.id), size, None);
        again.sees("30 100").await;
    }

    #[tokio::test]
    async fn a_window_that_falls_behind_a_flood_is_drawn_whole_again() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let (dir, held, _holder) = holding_sh(root.path()).await;
        let size = Size {
            rows: 30,
            cols: 100,
        };
        // It draws itself whole whenever it's told its size changed, as pi
        // does, and floods far past what a window may fall behind by, as a
        // build's output can while the window can't keep up.
        let script = root.path().join("floods");
        fs::write(
            &script,
            "trap 'printf \"drawn %s\\n\" whole' WINCH\n\
             echo \"$((6 * 7)) set\"\n\
             head -c 16000000 /dev/zero | tr '\\0' x\n\
             : > flooded\n\
             while :; do sleep 0.05; done\n",
        )
        .expect("the script is written");
        let mut window = Joined::join(socket(&dir, &held.id), size, None);
        window.types("sh floods\r").await;
        window.sees("42 set").await;
        // The window reads nothing while the program floods, as one whose
        // terminal has stalled, so it falls behind and misses output.
        appears(&root.path().join("flooded")).await;
        window.sees("drawn whole").await;
    }

    #[tokio::test]
    async fn a_terminal_joining_later_is_given_the_modes_the_program_asked_for() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let (dir, held, _holder) = holding_sh(root.path()).await;
        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut first = Joined::join(socket(&dir, &held.id), size, None);
        // What is typed comes back as typed, its escapes spelt out, so only
        // the program's own output carries the modes themselves.
        first
            .types("printf '\\033[?1049h\\033[?1006h%s\\n' \"$((6 * 7)) set\"\r")
            .await;
        first.sees("42 set").await;
        first.task.abort();
        let _ = first.task.await;

        let mut again = Joined::join(socket(&dir, &held.id), size, None);
        again.sees("\u{1b}[?1006h\u{1b}[?1049h").await;
    }

    #[tokio::test]
    async fn a_terminal_joining_later_is_given_the_title_the_program_set() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let (dir, held, _holder) = holding_sh(root.path()).await;
        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut first = Joined::join(socket(&dir, &held.id), size, None);
        // Spelt out as typed, so only the program's own output sets it.
        first
            .types("printf '\\033]2;pi --session %s\\007%s\\n' far \"$((6 * 7)) set\"\r")
            .await;
        first.sees("42 set").await;
        first.task.abort();
        let _ = first.task.await;

        let mut again = Joined::join(socket(&dir, &held.id), size, None);
        again.sees("\u{1b}]2;pi --session far\u{7}").await;
    }

    #[tokio::test]
    async fn a_window_is_listed_only_while_it_is_joined() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let (dir, held, holder) = holding_sh(root.path()).await;
        let (_watching, mut changes) = super::super::watch(&dir).expect("the folder is watched");
        let windows = async |changes: &mut mpsc::UnboundedReceiver<()>, wanted: &[&str]| {
            let found = tokio::time::timeout(PATIENCE, async {
                loop {
                    let listed: Vec<String> = listed(&dir)
                        .first()
                        .map(|one| one.windows.iter().map(|window| window.id.clone()).collect())
                        .unwrap_or_default();
                    if listed == wanted {
                        return;
                    }
                    changes.recv().await;
                }
            })
            .await;
            assert!(found.is_ok(), "the windows listed never became {wanted:?}");
        };
        let kitty = |id: &str| {
            Some(Window {
                terminal: "kitty".to_owned(),
                id: id.to_owned(),
                within: Some("unix:@kitty-6753".to_owned()),
                app_id: None,
            })
        };
        let size = Size {
            rows: 30,
            cols: 100,
        };

        let mut first = Joined::join(socket(&dir, &held.id), size, kitty("13"));
        first.types("echo one-$((0 + 1))\r").await;
        first.sees("one-1").await;
        let mut second = Joined::join(socket(&dir, &held.id), size, kitty("14"));
        second.types("echo two-$((1 + 1))\r").await;
        second.sees("two-2").await;
        windows(&mut changes, &["13", "14"]).await;

        first.task.abort();
        let _ = first.task.await;
        windows(&mut changes, &["14"]).await;
        second.task.abort();
        let _ = second.task.await;
        windows(&mut changes, &[]).await;

        let mut last = Joined::join(socket(&dir, &held.id), size, None);
        last.types("exit\r").await;
        let _ = tokio::time::timeout(PATIENCE, holder)
            .await
            .expect("the holder ends with its program");
        assert!(
            listed(&dir).is_empty(),
            "a window leaving after can't bring it back"
        );
    }

    /// Waits until what is listed in `dir` says it is `parked` or not.
    async fn listed_parked(
        dir: &Path,
        changes: &mut mpsc::UnboundedReceiver<()>,
        parked: bool,
    ) -> Held {
        let found = tokio::time::timeout(PATIENCE, async {
            loop {
                if let Some(one) = listed(dir).into_iter().find(|one| one.parked == parked) {
                    return one;
                }
                changes.recv().await;
            }
        })
        .await;
        found.unwrap_or_else(|_| panic!("what is held never listed as parked: {parked}"))
    }

    #[tokio::test]
    async fn a_parked_program_starts_again_on_its_terminal_when_a_key_is_typed() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let (dir, held, _holder) = holding_sh(root.path()).await;
        let (_watching, mut changes) = super::super::watch(&dir).expect("the folder is watched");
        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut window = Joined::join(socket(&dir, &held.id), size, None);
        window.types("echo first $((40 + 2)); exit 75\r").await;
        window.sees("first 42").await;
        window.sees("Parked").await;
        let parked = listed_parked(&dir, &mut changes, true).await;
        assert_eq!(parked.pid, 0, "a parked program has no process");

        // The key that wakes it is not the program's: only what is typed
        // after reaches the shell, whose line would fail with an x before it.
        window.types("x").await;
        let again = listed_parked(&dir, &mut changes, false).await;
        assert!(
            again.pid != 0 && again.pid != held.pid,
            "it runs as a new process"
        );
        window.types("stty size; echo again $((6 * 7))\r").await;
        window.sees("30 100").await;
        window.sees("again 42").await;
    }

    /// A command resuming the session `01a1-ab`, saying so as it starts.
    fn resuming() -> Vec<String> {
        [
            "sh",
            "-c",
            "echo resumed-$1; exec sh",
            "--session",
            "01a1-ab",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    /// A stand-in node at `socket`, answering the first ask it is sent
    /// once `go` is told, and saying what it was asked.
    fn stand_in_node(socket: &Path) -> (oneshot::Sender<()>, JoinHandle<String>) {
        let listener = tokio::net::UnixListener::bind(socket).expect("the node's socket binds");
        let (go, told) = oneshot::channel::<()>();
        let node = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("a holder asks");
            let (reading, mut writing) = stream.into_split();
            let mut asked = String::new();
            tokio::io::AsyncBufReadExt::read_line(
                &mut tokio::io::BufReader::new(reading),
                &mut asked,
            )
            .await
            .expect("its ask is read");
            if told.await.is_ok() {
                let _ = writing.write_all(b"{\"Ok\":\"Turn\"}\n").await;
            }
            asked
        });
        (go, node)
    }

    #[tokio::test]
    async fn a_resuming_program_waits_for_its_turn_from_the_node() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let node = root.path().join("work.sock");
        let (go, asked) = stand_in_node(&node);
        let (dir, held, _holder) = holding_in(
            root.path(),
            Vec::new(),
            Given(Vec::new()),
            resuming(),
            Some(node),
        )
        .await;
        assert!(
            held.parked && held.pid == 0,
            "it runs nothing while it waits: {held:?}"
        );
        let mut window = Joined::join(socket(&dir, &held.id), FIRST_SIZE, None);
        window.sees("Waiting its turn").await;
        assert!(
            !window.seen.contains("resumed-"),
            "nothing starts before its turn: {:?}",
            window.seen
        );
        let _ = go.send(());
        window.sees("resumed-01a1-ab").await;
        let asked = asked.await.expect("the node was asked");
        assert_eq!(
            serde_json::from_str::<crate::wire::Request>(&asked).ok(),
            Some(crate::wire::Request::Turn {
                session: "01a1-ab".to_owned()
            }),
            "for the session it resumes"
        );
    }

    #[tokio::test]
    async fn a_key_typed_starts_a_waiting_program_at_once() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let node = root.path().join("work.sock");
        // The node never answers, as one with every place taken.
        let (_never, _node) = stand_in_node(&node);
        let (dir, held, _holder) = holding_in(
            root.path(),
            Vec::new(),
            Given(Vec::new()),
            resuming(),
            Some(node),
        )
        .await;
        let mut window = Joined::join(socket(&dir, &held.id), FIRST_SIZE, None);
        window.sees("Waiting its turn").await;
        window.types("x").await;
        window.sees("resumed-01a1-ab").await;
        // The key that started it is spent, as a parked program's wake is.
        window.types("echo typed $((6 * 7))\r").await;
        window.sees("typed 42").await;
    }

    #[test]
    fn a_windows_focus_reports_are_told_apart_from_what_it_typed() {
        assert_eq!(focus_of(b"\x1b[I"), (true, Vec::new()), "it came in front");
        assert_eq!(
            focus_of(b"\x1b[O"),
            (false, Vec::new()),
            "it went behind, which says nothing typed"
        );
        assert_eq!(
            focus_of(b"\x1b[Ox\x1b[Iy"),
            (true, b"xy".to_vec()),
            "keys typed between them are kept"
        );
        assert_eq!(
            focus_of(b"\x1b[A"),
            (false, b"\x1b[A".to_vec()),
            "an arrow is a key"
        );
    }

    #[tokio::test]
    async fn a_parked_program_starts_again_as_its_window_comes_in_front() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let (dir, held, _holder) = holding_sh(root.path()).await;
        let (_watching, mut changes) = super::super::watch(&dir).expect("the folder is watched");
        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut window = Joined::join(socket(&dir, &held.id), size, None);
        window.types("echo first $((40 + 2)); exit 75\r").await;
        window.sees("first 42").await;
        window.sees("\u{1b}[?1004h").await;
        window.sees("Parked").await;
        listed_parked(&dir, &mut changes, true).await;
        // One joining while it's parked is asked to say so too.
        let mut later = Joined::join(socket(&dir, &held.id), size, None);
        later.sees("\u{1b}[?1004h").await;

        later.types("\u{1b}[I").await;
        listed_parked(&dir, &mut changes, false).await;
        window.sees("\u{1b}[?1004l").await;
        // Running, it never asked for focus reports, so they don't reach
        // it as keys, whose line would fail with them before it.
        window
            .types("\u{1b}[O\u{1b}[Iecho again $((6 * 7))\r")
            .await;
        window.sees("again 42").await;
    }

    #[tokio::test]
    async fn a_parked_program_starts_again_when_its_wake_file_appears() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let (dir, held, holder) = holding_sh(root.path()).await;
        let (_watching, mut changes) = super::super::watch(&dir).expect("the folder is watched");
        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut window = Joined::join(socket(&dir, &held.id), size, None);
        window.types("echo \"[$MYCELIUM_WAKE]\"; exit 75\r").await;
        let waking = wake(&dir, &held.id);
        window.sees(&format!("[{}]", waking.display())).await;
        listed_parked(&dir, &mut changes, true).await;

        fs::write(&waking, b"{}").expect("the wake is written");
        listed_parked(&dir, &mut changes, false).await;
        assert!(
            !waking.exists(),
            "the wake is spent once it woke the program"
        );
        window.types("echo woken $((6 * 7)); exit 4\r").await;
        window.sees("woken 42").await;
        let ended = tokio::time::timeout(PATIENCE, holder)
            .await
            .expect("the holder ends with its program")
            .expect("the holder's task ran");
        assert_eq!(ended.expect("it held"), Some(4), "any other exit ends it");
    }

    /// Lands `body` in `inbox` as the mesh writes a steer: under another
    /// name first, then renamed whole.
    fn steer_lands(inbox: &Path, name: &str, body: &str) {
        let writing = inbox.join(format!(".{name}"));
        fs::write(&writing, body).expect("the steer is written");
        fs::rename(&writing, inbox.join(name)).expect("the steer lands");
    }

    // 14 November: a talk to a parked pi waited up to a minute for a
    // running one's look to write its wake file.
    #[tokio::test]
    async fn a_parked_program_starts_again_as_a_steer_that_wakes_it_lands_in_its_inbox() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let inbox = root.path().join("steer").join("ZJ62K2");
        fs::create_dir_all(&inbox).expect("the inbox is made");
        let (dir, held, _holder) = holding_sh(root.path()).await;
        let (_watching, mut changes) = super::super::watch(&dir).expect("the folder is watched");
        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut window = Joined::join(socket(&dir, &held.id), size, None);
        let said = format!(
            r#"{{"inboxes":["{}"],"wakes":{{"acts":["close"],"talk":true,"resumeWithWords":true}}}}"#,
            inbox.display()
        );
        window
            .types(&format!(
                "printf '%s' '{said}' > \"$MYCELIUM_STEERS\"; exit 75\r"
            ))
            .await;
        listed_parked(&dir, &mut changes, true).await;
        let steering = steers(&dir, &held.id);
        assert!(steering.exists(), "the program said where its steers land");

        // Nothing says a holder looked and let it be, so a pause is given
        // two of the holder's looks to wake it, and mustn't.
        steer_lands(&inbox, "1-a.json", r#"{"act":"pause"}"#);
        let paused =
            tokio::time::timeout(WAKE_LOOK * 2, listed_parked(&dir, &mut changes, false)).await;
        assert!(paused.is_err(), "a pause leaves it parked");

        let landed = std::time::Instant::now();
        steer_lands(&inbox, "2-b.json", r#"{"text":"are you there?"}"#);
        listed_parked(&dir, &mut changes, false).await;
        let took = landed.elapsed();
        eprintln!("a talk woke a parked program in {took:?}");
        // The holder's own look would wake it within a second too, so the
        // time can't tell the watch from the look; it proves the talk no
        // longer waits on a running pi's minute. Half a look failed 2 of 3
        // runs on a loaded machine (16 Nov), where a wake took longer.
        assert!(
            took < WAKE_LOOK * 20,
            "a talk wakes it as it lands, never after a running pi's look: {took:?}"
        );
        assert!(!steering.exists(), "what it said is spent once it woke");
        assert!(
            inbox.join("2-b.json").exists(),
            "the talk is left for the program to take"
        );
        window.types("echo woken $((6 * 7))\r").await;
        window.sees("woken 42").await;
    }

    #[tokio::test]
    async fn a_program_that_wakes_itself_as_it_parks_restarts_at_once_with_no_parked_line() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let (dir, held, holder) = holding_sh(root.path()).await;
        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut window = Joined::join(socket(&dir, &held.id), size, None);
        window
            .types(concat!(
                r#"printf '%s' '["sh","-c","echo restarted $((6 * 7)); read -r line; exit 3"]'"#,
                " > \"$MYCELIUM_AGAIN\"; printf '{}' > \"$MYCELIUM_WAKE\"; exit 75\r",
            ))
            .await;
        window.sees("restarted 42").await;
        assert!(
            !window.seen.contains("Parked"),
            "a restart draws no parked line over the program's last frame"
        );
        assert!(
            !wake(&dir, &held.id).exists(),
            "the wake it wrote is spent by its restart"
        );
        // The first key typed after it went is the program's, never a wake.
        window.types("\r").await;
        let ended = tokio::time::timeout(PATIENCE, holder)
            .await
            .expect("the holder ends with its program")
            .expect("the holder's task ran");
        assert_eq!(
            ended.expect("it held"),
            Some(3),
            "the key reached the restarted command"
        );
    }

    #[tokio::test]
    async fn a_parked_program_starts_again_with_the_command_it_named() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let (dir, held, holder) = holding_sh(root.path()).await;
        let (_watching, mut changes) = super::super::watch(&dir).expect("the folder is watched");
        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut window = Joined::join(socket(&dir, &held.id), size, None);
        window
            .types(concat!(
                r#"printf '%s' '["sh","-c","echo resumed $((6 * 7)); read -r _; exit 3"]'"#,
                " > \"$MYCELIUM_AGAIN\"; exit 75\r",
            ))
            .await;
        listed_parked(&dir, &mut changes, true).await;

        fs::write(wake(&dir, &held.id), b"{}").expect("the wake is written");
        let woken = listed_parked(&dir, &mut changes, false).await;
        window.sees("resumed 42").await;
        assert_eq!(
            woken.command[..2],
            ["sh", "-c"],
            "the record says what it runs now"
        );
        assert!(
            !again(&dir, &held.id).exists(),
            "the command named is taken once read"
        );
        // The command waits for a line, so it's still listed as running
        // when looked for: one that ended at once left its listing gone
        // before a busy builder looked (twice in three runs, 2026-11-08).
        window.types("\r").await;
        let ended = tokio::time::timeout(PATIENCE, holder)
            .await
            .expect("the holder ends with its program")
            .expect("the holder's task ran");
        assert_eq!(ended.expect("it held"), Some(3), "the named command ran");
    }

    // A pi on an old conversation, woken in its window, named itself as it
    // parked over the newer conversation named for it while it ran, and
    // each wake after started the old one again (2026-11-13).
    #[tokio::test]
    async fn a_command_named_for_a_running_program_wins_over_the_one_it_names_as_it_parks() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let (dir, held, holder) = holding_sh(root.path()).await;
        let (_watching, mut changes) = super::super::watch(&dir).expect("the folder is watched");
        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut window = Joined::join(socket(&dir, &held.id), size, None);
        let chosen = ["sh", "-c", "echo chosen $((6 * 7)); read -r _; exit 3"].map(str::to_owned);
        assert!(
            !name_next(&dir, &held.id, &chosen).expect("a command is named"),
            "a running program isn't woken"
        );
        window
            .types(concat!(
                r#"printf '%s' '["sh","-c","echo its own $((6 * 7)); read -r _; exit 4"]'"#,
                " > \"$MYCELIUM_AGAIN\"; exit 75\r",
            ))
            .await;
        listed_parked(&dir, &mut changes, true).await;

        fs::write(wake(&dir, &held.id), b"{}").expect("the wake is written");
        let woken = listed_parked(&dir, &mut changes, false).await;
        window.sees("chosen 42").await;
        assert_eq!(
            woken.command, chosen,
            "the record says the command named for it"
        );
        assert!(
            !next(&dir, &held.id).exists() && !again(&dir, &held.id).exists(),
            "both commands are taken once read"
        );
        window.types("\r").await;
        let ended = tokio::time::timeout(PATIENCE, holder)
            .await
            .expect("the holder ends with its program")
            .expect("the holder's task ran");
        assert_eq!(
            ended.expect("it held"),
            Some(3),
            "the command named for it ran"
        );
    }

    #[tokio::test]
    async fn naming_a_parked_programs_next_command_wakes_it_with_that_command() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let (dir, held, holder) = holding_sh(root.path()).await;
        let (_watching, mut changes) = super::super::watch(&dir).expect("the folder is watched");
        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut window = Joined::join(socket(&dir, &held.id), size, None);
        let named = ["sh", "-c", "echo named $((6 * 7)); read -r _; exit 5"].map(str::to_owned);
        assert_eq!(
            name_next(&dir, "nothing1", &named).map_err(|error| error.kind()),
            Err(io::ErrorKind::NotFound),
            "nothing is named for a session not held"
        );
        assert_eq!(
            name_next(&dir, &held.id, &[]).map_err(|error| error.kind()),
            Err(io::ErrorKind::InvalidInput),
            "a command names a program"
        );
        window.types("exit 75\r").await;
        listed_parked(&dir, &mut changes, true).await;

        assert!(
            name_next(&dir, &held.id, &named).expect("a command is named"),
            "a parked program is woken"
        );
        listed_parked(&dir, &mut changes, false).await;
        window.sees("named 42").await;
        window.types("\r").await;
        let ended = tokio::time::timeout(PATIENCE, holder)
            .await
            .expect("the holder ends with its program")
            .expect("the holder's task ran");
        assert_eq!(
            ended.expect("it held"),
            Some(5),
            "the command named for it ran"
        );
    }

    #[tokio::test]
    async fn every_start_of_a_parked_program_runs_inside_the_words_it_was_held_through() {
        let root = tempfile::tempdir().expect("a temporary folder");
        // Words a login shell would stand for: they say so, then run what
        // follows them, as `exec "$@"` does. They wait for a line first, so
        // what they say comes once a window has joined to see it: a window
        // sees only what is shown after it joins.
        let through = [
            "sh",
            "-c",
            r#"read -r _; echo through $((6 * 7)); exec "$@""#,
            "held",
        ]
        .map(str::to_owned)
        .to_vec();
        let (dir, held, holder) = holding_sh_through(root.path(), through).await;
        let (_watching, mut changes) = super::super::watch(&dir).expect("the folder is watched");
        assert_eq!(
            held.command,
            ["sh"],
            "the record says the program, not its shell"
        );
        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut window = Joined::join(socket(&dir, &held.id), size, None);
        window.types("\r").await;
        window.sees("through 42").await;
        window
            .types(concat!(
                r#"printf '%s' '["sh","-c","echo resumed $((6 * 7)); read -r _; exit 3"]'"#,
                " > \"$MYCELIUM_AGAIN\"; exit 75\r",
            ))
            .await;
        listed_parked(&dir, &mut changes, true).await;
        window.seen.clear();

        fs::write(wake(&dir, &held.id), b"{}").expect("the wake is written");
        listed_parked(&dir, &mut changes, false).await;
        window.types("\r").await;
        window.sees("through 42").await;
        window.sees("resumed 42").await;
        window.types("\r").await;
        let ended = tokio::time::timeout(PATIENCE, holder)
            .await
            .expect("the holder ends with its program")
            .expect("the holder's task ran");
        assert_eq!(ended.expect("it held"), Some(3), "the named command ran");
    }

    #[tokio::test]
    async fn a_parked_program_that_cant_start_again_stays_parked_and_says_why() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let (dir, held, holder) = holding_sh(root.path()).await;
        let (_watching, mut changes) = super::super::watch(&dir).expect("the folder is watched");
        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut window = Joined::join(socket(&dir, &held.id), size, None);
        window
            .types(concat!(
                r#"printf '%s' '["no-such-program-anywhere"]'"#,
                " > \"$MYCELIUM_AGAIN\"; exit 75\r",
            ))
            .await;
        listed_parked(&dir, &mut changes, true).await;

        let waking = wake(&dir, &held.id);
        fs::write(&waking, b"{}").expect("the wake is written");
        window.sees("couldn't start again").await;
        assert!(!waking.exists(), "the wake is spent though nothing started");
        assert!(!holder.is_finished(), "the holder keeps the session");
        assert!(
            listed(&dir).first().is_some_and(|one| one.parked),
            "it is still listed as parked"
        );

        // A command named again starts at the next wake.
        fs::write(
            again(&dir, &held.id),
            br#"["sh","-c","echo mended $((6 * 7)); read -r _; exit 5"]"#,
        )
        .expect("a command is named again");
        fs::write(&waking, b"{}").expect("the wake is written again");
        listed_parked(&dir, &mut changes, false).await;
        window.sees("mended 42").await;
        window.types("\r").await;
        let ended = tokio::time::timeout(PATIENCE, holder)
            .await
            .expect("the holder ends with its program")
            .expect("the holder's task ran");
        assert_eq!(ended.expect("it held"), Some(5), "the mended command ran");
    }

    #[tokio::test]
    async fn a_held_program_has_the_persons_environment_under_the_holders_own_words() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let person = Given(vec![
            ("TRIAL_MODEL", "newest"),
            ("MYCELIUM_REALM", "elsewhere"),
            ("KITTY_PID", "1"),
        ]);
        let (dir, held, _holder) = holding_sh_in(root.path(), Vec::new(), person).await;
        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut window = Joined::join(socket(&dir, &held.id), size, None);
        window
            .types("echo \"[$TRIAL_MODEL in $MYCELIUM_REALM, ${KITTY_PID:-no} window]\"\r")
            .await;
        window.sees("[newest in harbour, no window]").await;
    }

    #[tokio::test]
    async fn a_woken_program_has_the_persons_environment_as_it_is_then() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let (dir, held, _holder) =
            holding_sh_in(root.path(), Vec::new(), Changing::default()).await;
        let (_watching, mut changes) = super::super::watch(&dir).expect("the folder is watched");
        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut window = Joined::join(socket(&dir, &held.id), size, None);
        window
            .types("echo \"[start $TRIAL_START]\"; exit 75\r")
            .await;
        window.sees("[start 1]").await;
        listed_parked(&dir, &mut changes, true).await;

        fs::write(wake(&dir, &held.id), b"{}").expect("the wake is written");
        listed_parked(&dir, &mut changes, false).await;
        window.types("echo \"[start $TRIAL_START]\"\r").await;
        window.sees("[start 2]").await;
    }

    #[tokio::test]
    async fn a_program_on_the_holders_path_starts_though_the_persons_lacks_it() {
        // The person's PATH names none of the folders this test's own does,
        // as a service manager's lacks those a developer's shell adds, so
        // `sh` is found only on the holder's own.
        let root = tempfile::tempdir().expect("a temporary folder");
        let (dir, held, _holder) =
            holding_sh_in(root.path(), Vec::new(), Given(vec![("PATH", "/nowhere")])).await;
        let size = Size {
            rows: 30,
            cols: 100,
        };
        let mut window = Joined::join(socket(&dir, &held.id), size, None);
        window.types("echo \"[last ${PATH##*:}]\"\r").await;
        window.sees("[last /nowhere]").await;
    }

    #[tokio::test]
    async fn a_program_told_to_end_with_its_holder_saves_on_its_terminal_first() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let dir = root.path().join("held");
        let folder = root.path().to_owned();
        let journal = root.path().join("journal");
        // It saves only once it has said so on its terminal, as pi puts its
        // terminal back before it exits, and a while after it's told. It
        // says it's ready once a key is typed, since what it said before a
        // terminal joined isn't shown there.
        let saving = "trap 'sleep 0.3; echo saved || exit 9; exit 0' TERM; \
                      read typed; echo ready; while :; do sleep 0.05; done";
        let (told, ready) = oneshot::channel();
        let holder = {
            let dir = dir.clone();
            tokio::spawn(async move {
                let command = vec!["sh".to_owned(), "-c".to_owned(), saving.to_owned()];
                let holding = Holding {
                    dir: &dir,
                    realm: "harbour",
                    id: "ending1",
                    command: &command,
                    through: &[],
                    folder: &folder,
                    environment: &Given(Vec::new()),
                    journal: &SystemJournal::at(&journal),
                    ending: Ending::Signalled,
                    node: None,
                };
                hold(&holding, |held| {
                    let _ = told.send(held.clone());
                    Ok(())
                })
                .await
            })
        };
        let held = ready.await.expect("the holder says it holds");
        let mut joined = Joined::join(socket(&dir, &held.id), FIRST_SIZE, None);
        joined.types("go\r").await;
        joined.sees("ready").await;

        // Told as its service manager tells it, which tells only the
        // holder here, this test's own process.
        kill_process(rustix::process::getpid(), rustix::process::Signal::TERM)
            .expect("the holder is told to end");
        joined.sees("saved").await;
        let ended = tokio::time::timeout(PATIENCE, holder)
            .await
            .expect("the holder ends once its program has")
            .expect("the holder's task ran");
        assert_eq!(
            ended.expect("it held"),
            Some(0),
            "the program wrote on its terminal and ended as it chose"
        );
    }

    #[tokio::test]
    async fn nothing_is_held_without_a_program() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let holding = Holding {
            dir: &root.path().join("held"),
            realm: "harbour",
            id: "none",
            command: &[],
            through: &[],
            folder: root.path(),
            environment: &Given(Vec::new()),
            journal: &SystemJournal::at(&root.path().join("journal")),
            ending: Ending::Unsignalled,
            node: None,
        };
        let refused = hold(&holding, |_| Ok(())).await;
        assert!(refused.is_err(), "holding nothing is refused");
    }

    /// Holds `command` until it ends, with a journal listening, and
    /// answers what the holder said and the entry the journal took.
    async fn ended_by(command: &str) -> (io::Result<Option<i32>>, String) {
        let root = tempfile::tempdir().expect("a temporary folder");
        let socket = root.path().join("journal");
        let journal = std::os::unix::net::UnixDatagram::bind(&socket).expect("a journal");
        let command = vec!["sh".to_owned(), "-c".to_owned(), command.to_owned()];
        let holding = Holding {
            dir: &root.path().join("held"),
            realm: "harbour",
            id: "ends1",
            command: &command,
            through: &[],
            folder: root.path(),
            environment: &Given(Vec::new()),
            journal: &SystemJournal::at(&socket),
            ending: Ending::Unsignalled,
            node: None,
        };
        let said = tokio::time::timeout(PATIENCE, hold(&holding, |_| Ok(())))
            .await
            .expect("the program ends");
        let mut entry = vec![0; 4096];
        let read = journal.recv(&mut entry).expect("its end is said");
        (said, String::from_utf8_lossy(&entry[..read]).into_owned())
    }

    #[tokio::test]
    async fn a_held_programs_end_is_said_in_the_journal() {
        let (said, entry) = ended_by("exit 3").await;
        assert_eq!(said.ok().flatten(), Some(3), "the holder answers its code");
        assert!(
            entry.contains("MYCELIUM_HELD=ends1\n")
                && entry.contains("EXIT_CODE=exited\nEXIT_STATUS=3\n"),
            "an exit is said with its code: {entry:?}"
        );

        let (said, entry) = ended_by("kill -9 $$").await;
        assert_eq!(said.ok().flatten(), None, "a killed program gives no code");
        assert!(
            entry.contains("EXIT_CODE=killed\nEXIT_STATUS=KILL\n")
                && entry.contains("was killed by signal 9 (KILL)"),
            "a kill is said with its signal: {entry:?}"
        );
    }
}
