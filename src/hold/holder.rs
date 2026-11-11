//! The holder: one process keeping one program's terminal, and serving
//! any terminal that joins it. Every joined terminal is given the modes
//! the program asked for, then sees the program's output and types into
//! it, and the program's size is the one the latest
//! to join or resize gave. Leaving is closing the connection; the program
//! runs on.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pty_process::{OwnedReadPty, OwnedWritePty};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinSet;

use super::environment::{Environment, keeping_path};
use super::modes::Modes;
use super::{Held, Said, Size, again, socket, wake};
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
/// `EX_TEMPFAIL`, as it stops only until it is woken.
pub const PARKED: i32 = 75;

/// How often a parked holder looks for its wake file. A stat a second
/// costs nothing, and a message waking a session waits a second at most.
const WAKE_LOOK: Duration = Duration::from_secs(1);

/// What every joined window shows while its program is parked.
const PARKED_LINE: &[u8] = b"\r\nParked, its memory given back. A key or a message wakes it.\r\n";

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
}

/// Holds the program until it ends, answering its exit code, if it gave
/// one. `ready` is told what is held once a terminal can join it. Each
/// start of the program has the person's environment as it is then, over
/// the one the holder was started with. The program is told its realm in `MYCELIUM_REALM`, whatever realm the
/// environment it was held from said, its id in `MYCELIUM_HELD`, the file that wakes it once
/// parked in `MYCELIUM_WAKE`, and the file it may name its next command in
/// as it parks in `MYCELIUM_AGAIN`. A program exiting with [`PARKED`] is
/// started again on the same terminal when woken, with that command if it
/// named one; one that wrote its wake file before it went is restarting,
/// and starts again at once, never shown as parked.
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
    let socket = node::bind(&socket(holding.dir, holding.id))?;
    let waking = wake(holding.dir, holding.id);
    let naming = again(holding.dir, holding.id);
    // A wake or a command left from before this program ran says nothing
    // to it.
    let _ = fs::remove_file(&waking);
    let _ = fs::remove_file(&naming);
    let (pty, pts) = pty_process::open().map_err(io::Error::other)?;
    pty.resize(size_of(FIRST_SIZE)).map_err(io::Error::other)?;
    let starts = Arc::new(Starts::default());
    let end = ChildEnd::of(pts, starts.clone())?;
    // The holder keeps the terminal's other end, so a parked program's
    // terminal outlives it and the program starts again on the same one.
    let start = |command: &[String], size: Size| {
        let words: Vec<&String> = holding.through.iter().chain(command).collect();
        let Some((program, arguments)) = words.split_first() else {
            return Err(io::Error::other("there is nothing to start"));
        };
        // The person's environment goes first, so what the holder says
        // and takes away below is its own whatever that environment holds.
        let mut command = pty_process::Command::new(program)
            .args(arguments)
            .current_dir(holding.folder)
            .envs(keeping_path(
                holding.environment.now(),
                std::env::var("PATH").ok().as_deref(),
            ))
            .env("MYCELIUM_REALM", holding.realm)
            .env("MYCELIUM_HELD", holding.id)
            .env("MYCELIUM_WAKE", &waking)
            .env("MYCELIUM_AGAIN", &naming);
        for variable in WINDOW_VARIABLES {
            command = command.env_remove(variable);
        }
        end.start(command, size)
    };
    let mut command = holding.command.to_vec();
    let mut child = start(&command, FIRST_SIZE)?;
    let held = Held {
        id: holding.id.to_owned(),
        command: holding.command.to_vec(),
        folder: holding.folder.to_owned(),
        holder: std::process::id(),
        pid: child.id().unwrap_or_default(),
        parked: false,
        since: jiff::Timestamp::now().to_string(),
        windows: Vec::new(),
    };
    let record = Arc::new(Record::write(holding.dir, &held)?);
    ready(&held)?;

    let (reading, writing) = pty.into_split();
    let (output, _) = broadcast::channel(OUTPUT_BACKLOG);
    let terminal = Arc::new(Terminal {
        writing: tokio::sync::Mutex::new(writing),
        size: Mutex::new(FIRST_SIZE),
        drawn: output.clone(),
        parked: watch::Sender::new(false),
    });
    let shown = Arc::new(Shown {
        modes: Mutex::new(Modes::default()),
        output,
    });
    let mut reader = tokio::spawn(read_output(reading, shown.clone(), starts));
    let mut joined = JoinSet::new();
    // A terminal may join whether the program runs or is parked.
    let serving = |stream| serve(stream, terminal.clone(), shown.clone(), record.clone());
    let status = loop {
        let status = joining_until(child.wait(), &socket, &mut joined, &serving).await?;
        if status.code() != Some(PARKED) {
            break status;
        }
        let mut restarting = restarting(&waking, &record, &shown);
        // One that can't start again stays parked, saying why, rather than
        // ending: its session, its window and its next wake are all still
        // here, and a command named again may start.
        child = loop {
            if !std::mem::take(&mut restarting) {
                joining_until(woken(&terminal, &waking), &socket, &mut joined, &serving).await;
            }
            command = named(&naming).unwrap_or(command);
            match start(&command, terminal.size()) {
                Ok(child) => break child,
                Err(error) => parks(&record, &shown, &unstarted(&error)),
            }
        };
        record.runs(child.id().unwrap_or_default(), &command);
    };
    // The terminal's other end goes with the program, so its last output
    // ends as it did, then whoever is joined is let go.
    drop(end);
    drop(socket);
    record.end();
    if tokio::time::timeout(LAST_WORDS, &mut reader).await.is_err() {
        reader.abort();
    }
    drop(shown);
    let _ = tokio::time::timeout(LAST_WORDS, async {
        while joined.join_next().await.is_some() {}
    })
    .await;
    Ok(status.code())
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

/// What a parked program's windows say when it couldn't start again.
fn unstarted(error: &io::Error) -> Vec<u8> {
    format!("\r\nIt couldn't start again: {error}. {PARKED_WAKES}\r\n").into_bytes()
}

/// Says the program is parked, in its record and with `line` in every
/// joined window.
fn parks(record: &Record, shown: &Shown, line: &[u8]) {
    record.parks();
    shown.show(line);
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
/// terminal or the file `waking` appears, then spends the wake.
async fn woken(terminal: &Terminal, waking: &Path) {
    let mut parked = terminal.parked.subscribe();
    terminal.parked.send_replace(true);
    let mut look = tokio::time::interval(WAKE_LOOK);
    loop {
        tokio::select! {
            _ = parked.wait_for(|parked| !*parked) => break,
            _ = look.tick() => if waking.exists() { break },
        }
    }
    terminal.parked.send_replace(false);
    // The wake is spent once it has woken the program. One that can't be
    // taken away would only wake it again at its next park.
    let _ = fs::remove_file(waking);
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

/// What the program shows, and the modes it asked for in showing it, as
/// every joined terminal takes them.
struct Shown {
    modes: Mutex<Modes>,
    output: broadcast::Sender<Arc<[u8]>>,
}

impl Shown {
    /// Passes `bytes` to every joined terminal, reading them for modes.
    fn show(&self, bytes: &[u8]) {
        let mut modes = self.modes.lock().unwrap_or_else(PoisonError::into_inner);
        modes.saw(bytes);
        // With nobody joined there is nobody to show it to.
        let _ = self.output.send(Arc::from(bytes));
    }

    /// What a terminal joining now is given first, and its share of all
    /// that is shown after. Both are had under the lock `show` takes, so
    /// no mode is missed between them, or given twice.
    fn join(&self) -> (Vec<u8>, broadcast::Receiver<Arc<[u8]>>) {
        let modes = self.modes.lock().unwrap_or_else(PoisonError::into_inner);
        (modes.restored(), self.output.subscribe())
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
                Said::Typed(_) if terminal.wakes() => Ok(()),
                Said::Typed(bytes) => terminal.writing.lock().await.write_all(&bytes).await,
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
        loop {
            match output.recv().await {
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

/// Passes the program's output to every joined terminal until it ends.
async fn read_output(mut reading: OwnedReadPty, shown: Arc<Shown>, starts: Arc<Starts>) {
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
            }
        }
    }
}

/// The terminal's child end, which each start of the program is given.
/// Linux starts a program on the same end again and again, and the holder
/// keeping it open is what keeps a parked program's terminal. macOS takes
/// a terminal away from whatever holds it once its session's leader ends,
/// and pty-process starts on a borrowed end only elsewhere, so there each
/// start opens the end again by its name, as `pty_process::open` opens it.
struct ChildEnd {
    #[cfg(not(target_os = "macos"))]
    pts: pty_process::Pts,
    #[cfg(target_os = "macos")]
    named: std::ffi::CString,
    #[cfg(target_os = "macos")]
    starts: Arc<Starts>,
}

impl ChildEnd {
    #[cfg(not(target_os = "macos"))]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "macOS's names the end, which can fail"
    )]
    fn of(pts: pty_process::Pts, _starts: Arc<Starts>) -> io::Result<Self> {
        Ok(Self { pts })
    }

    #[cfg(target_os = "macos")]
    fn of(pts: pty_process::Pts, starts: Arc<Starts>) -> io::Result<Self> {
        let named = rustix::termios::ttyname(&pts, Vec::new())?;
        Ok(Self { named, starts })
    }

    /// Starts `command` on the end at `size`, which Linux's end, never
    /// closed, keeps from the last start.
    #[cfg(not(target_os = "macos"))]
    fn start(
        &self,
        mut command: pty_process::Command,
        _size: Size,
    ) -> io::Result<tokio::process::Child> {
        command.spawn_borrowed(&self.pts).map_err(io::Error::other)
    }

    /// Starts `command` on the end opened anew at `size`, since macOS
    /// forgets a terminal's size once nothing holds its child end.
    #[cfg(target_os = "macos")]
    fn start(
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
        // SAFETY: `opened` was just opened by this terminal's child end's own
        // name, so it is valid, open and the child end of a pty, as
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

/// Each start of the program, as the reader of its output is told of it.
#[derive(Default)]
struct Starts {
    #[cfg(target_os = "macos")]
    started: tokio::sync::Notify,
}

impl Starts {
    /// Whether output may come once the terminal has read as ended: never
    /// on Linux, where the holder keeps the child end open until the
    /// program is done.
    #[cfg(not(target_os = "macos"))]
    #[expect(clippy::unused_self, reason = "macOS's waits on its own starts")]
    fn again(&self) -> impl Future<Output = bool> {
        std::future::ready(false)
    }

    /// Whether output may come once the terminal has read as ended: on
    /// macOS once the program starts again, on its end opened anew.
    #[cfg(target_os = "macos")]
    async fn again(&self) -> bool {
        self.started.notified().await;
        true
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
    use super::super::{attach, held as listed};
    use super::*;

    /// How long a test waits for what it expects to show before failing.
    const PATIENCE: Duration = Duration::from_secs(20);

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
        let dir = root.join("held");
        let folder = root.to_owned();
        let (told, ready) = oneshot::channel();
        let holder = {
            let dir = dir.clone();
            tokio::spawn(async move {
                let command = vec!["sh".to_owned()];
                let holding = Holding {
                    dir: &dir,
                    realm: "harbour",
                    id: "trial1",
                    command: &command,
                    through: &through,
                    folder: &folder,
                    environment: &environment,
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
        };
        let refused = hold(&holding, |_| Ok(())).await;
        assert!(refused.is_err(), "holding nothing is refused");
    }
}
