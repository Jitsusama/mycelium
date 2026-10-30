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
use tokio::sync::broadcast;
use tokio::task::JoinSet;

use super::modes::Modes;
use super::{Held, Said, Size, socket};
use crate::node;
use crate::record::Window;

/// How many pieces of output a joined terminal may fall behind by before
/// it misses some; it is caught up by the program's next redraw.
const OUTPUT_BACKLOG: usize = 1024;

/// How long the program's last output may take to reach whoever is joined
/// once it has ended: a child it left running can keep its terminal open.
const LAST_WORDS: Duration = Duration::from_millis(500);

/// The size a program starts at, before any terminal has joined.
const FIRST_SIZE: Size = Size { rows: 24, cols: 80 };

/// Variables a program's first terminal set that would name the wrong
/// window once it is held, since it outlives that window.
const WINDOW_VARIABLES: [&str; 4] = [
    "KITTY_WINDOW_ID",
    "KITTY_LISTEN_ON",
    "KITTY_PID",
    "WINDOWID",
];

/// What to hold.
pub struct Holding<'a> {
    /// The realm's folder of held sessions.
    pub dir: &'a Path,
    /// Its id.
    pub id: &'a str,
    /// The program and its arguments.
    pub command: &'a [String],
    /// The folder the program starts in.
    pub folder: &'a Path,
}

/// Holds the program until it ends, answering its exit code, if it gave
/// one. `ready` is told what is held once a terminal can join it. The
/// program is told its id in `MYCELIUM_HELD`.
pub async fn hold(
    holding: &Holding<'_>,
    ready: impl FnOnce(&Held) -> io::Result<()>,
) -> io::Result<Option<i32>> {
    let Some((program, arguments)) = holding.command.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "there is nothing to hold: name a program",
        ));
    };
    let socket = node::bind(&socket(holding.dir, holding.id))?;
    let (pty, pts) = pty_process::open().map_err(io::Error::other)?;
    pty.resize(size_of(FIRST_SIZE)).map_err(io::Error::other)?;
    let mut command = pty_process::Command::new(program)
        .args(arguments)
        .current_dir(holding.folder)
        .env("MYCELIUM_HELD", holding.id);
    for variable in WINDOW_VARIABLES {
        command = command.env_remove(variable);
    }
    let mut child = command.spawn(pts).map_err(io::Error::other)?;
    let held = Held {
        id: holding.id.to_owned(),
        command: holding.command.to_vec(),
        folder: holding.folder.to_owned(),
        holder: std::process::id(),
        pid: child.id().unwrap_or_default(),
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
    });
    let shown = Arc::new(Shown {
        modes: Mutex::new(Modes::default()),
        output,
    });
    let mut reader = tokio::spawn(read_output(reading, shown.clone()));
    let mut joined = JoinSet::new();
    let status = loop {
        tokio::select! {
            status = child.wait() => break status?,
            accepted = socket.accept() => {
                if let Ok(stream) = accepted {
                    joined.spawn(serve(
                        stream,
                        terminal.clone(),
                        shown.join(),
                        record.clone(),
                    ));
                }
            }
            Some(_) = joined.join_next(), if !joined.is_empty() => {}
        }
    };
    // The program's last output still reaches whoever is joined, and then
    // each is let go as the output ends.
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
}

impl Terminal {
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
/// either side ends.
async fn serve(
    stream: UnixStream,
    terminal: Arc<Terminal>,
    (modes, mut output): (Vec<u8>, broadcast::Receiver<Arc<[u8]>>),
    record: Arc<Record>,
) {
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
                Err(broadcast::error::RecvError::Lagged(_)) => {}
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
async fn read_output(mut reading: OwnedReadPty, shown: Arc<Shown>) {
    let mut buffer = vec![0u8; 8192];
    // A terminal whose program has gone reads as an error on Linux and as
    // its end elsewhere; either way there is no more.
    while let Ok(read) = reading.read(&mut buffer).await {
        if read == 0 {
            return;
        }
        shown.show(&buffer[..read]);
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
            assert!(
                matches!(found, Ok(true)),
                "the terminal never showed {wanted:?}; it showed {:?}",
                self.seen
            );
        }
    }

    /// Holds `sh` in a new folder of held sessions, answering the folder,
    /// the record once it can be joined, and the holder's end.
    async fn holding_sh(root: &Path) -> (PathBuf, Held, JoinHandle<io::Result<Option<i32>>>) {
        let dir = root.join("held");
        let folder = root.to_owned();
        let (told, ready) = oneshot::channel();
        let holder = {
            let dir = dir.clone();
            tokio::spawn(async move {
                let command = vec!["sh".to_owned()];
                let holding = Holding {
                    dir: &dir,
                    id: "trial1",
                    command: &command,
                    folder: &folder,
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
        first.types("echo \"[$MYCELIUM_HELD] $((40 + 2))\"\r").await;
        first.sees("[trial1] 42").await;
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

    #[tokio::test]
    async fn nothing_is_held_without_a_program() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let holding = Holding {
            dir: &root.path().join("held"),
            id: "none",
            command: &[],
            folder: root.path(),
        };
        let refused = hold(&holding, |_| Ok(())).await;
        assert!(refused.is_err(), "holding nothing is refused");
    }
}
