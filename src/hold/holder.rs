//! The holder: one process keeping one program's terminal, and serving
//! any terminal that joins it. Every joined terminal sees the program's
//! output and types into it, and the program's size is the one the latest
//! to join or resize gave. Leaving is closing the connection; the program
//! runs on.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use pty_process::{OwnedReadPty, OwnedWritePty};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::broadcast;
use tokio::task::JoinSet;

use super::{Held, Said, Size, socket};
use crate::node;

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
    };
    let record = Record::write(holding.dir, &held)?;
    ready(&held)?;

    let (reading, writing) = pty.into_split();
    let terminal = Arc::new(Terminal {
        writing: tokio::sync::Mutex::new(writing),
        size: Mutex::new(FIRST_SIZE),
    });
    let (output, _) = broadcast::channel(OUTPUT_BACKLOG);
    let mut reader = tokio::spawn(read_output(reading, output.clone()));
    let mut joined = JoinSet::new();
    let status = loop {
        tokio::select! {
            status = child.wait() => break status?,
            accepted = socket.accept() => {
                if let Ok(stream) = accepted {
                    joined.spawn(serve(stream, terminal.clone(), output.subscribe()));
                }
            }
            Some(_) = joined.join_next(), if !joined.is_empty() => {}
        }
    };
    // The program's last output still reaches whoever is joined, and then
    // each is let go as the output ends.
    drop(socket);
    drop(record);
    if tokio::time::timeout(LAST_WORDS, &mut reader).await.is_err() {
        reader.abort();
    }
    drop(output);
    let _ = tokio::time::timeout(LAST_WORDS, async {
        while joined.join_next().await.is_some() {}
    })
    .await;
    Ok(status.code())
}

/// The program's terminal, as every joined terminal shares it.
struct Terminal {
    writing: tokio::sync::Mutex<OwnedWritePty>,
    size: Mutex<Size>,
}

impl Terminal {
    /// Gives the program `size`. A terminal joining at the size the program
    /// already has nudges it one column narrower first, so the program is
    /// told of a change and draws itself again, as dtach's redraw does.
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
            writing
                .resize(size_of(narrower))
                .map_err(io::Error::other)?;
        }
        writing.resize(size_of(size)).map_err(io::Error::other)
    }
}

/// One joined terminal: what it types goes to the program, and the
/// program's output comes back, until either side ends.
async fn serve(
    stream: UnixStream,
    terminal: Arc<Terminal>,
    mut output: broadcast::Receiver<Arc<[u8]>>,
) {
    let (mut reading, mut writing) = stream.into_split();
    let typing = async {
        let mut joining = true;
        while let Ok(Some(said)) = Said::read(&mut reading).await {
            let done = match said {
                Said::Typed(bytes) => terminal.writing.lock().await.write_all(&bytes).await,
                Said::Size(size) => terminal.resize(size, std::mem::take(&mut joining)).await,
            };
            if done.is_err() {
                return;
            }
        }
    };
    let showing = async {
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
async fn read_output(mut reading: OwnedReadPty, output: broadcast::Sender<Arc<[u8]>>) {
    let mut buffer = vec![0u8; 8192];
    // A terminal whose program has gone reads as an error on Linux and as
    // its end elsewhere; either way there is no more.
    while let Ok(read) = reading.read(&mut buffer).await {
        if read == 0 {
            return;
        }
        // With nobody joined there is nobody to show it to.
        let _ = output.send(Arc::from(&buffer[..read]));
    }
}

fn size_of(size: Size) -> pty_process::Size {
    pty_process::Size::new(size.rows, size.cols)
}

/// The record beside a holder's socket, taken away when dropped.
struct Record(std::path::PathBuf);

impl Record {
    fn write(dir: &Path, held: &Held) -> io::Result<Self> {
        let path = dir.join(format!("{}.json", held.id));
        let staged = dir.join(format!(".{}.json.tmp", held.id));
        fs::write(&staged, serde_json::to_vec(held).map_err(io::Error::other)?)?;
        fs::rename(&staged, &path)?;
        Ok(Self(path))
    }
}

impl Drop for Record {
    fn drop(&mut self) {
        // The record is this holder's alone; if it's already gone there is
        // nothing to take away.
        let _ = fs::remove_file(&self.0);
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
        fn join(socket: PathBuf, size: Size) -> Self {
            let (typing, input) = tokio::io::duplex(4096);
            let (output, shown) = tokio::io::duplex(1 << 16);
            let (sizes, told) = mpsc::unbounded_channel();
            let task =
                tokio::spawn(async move { attach::join(&socket, size, input, output, told).await });
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
        let mut first = Joined::join(socket(&dir, &held.id), size);
        first.types("echo \"[$MYCELIUM_HELD] $((40 + 2))\"\r").await;
        first.sees("[trial1] 42").await;
        first.task.abort();
        let _ = first.task.await;

        let mut again = Joined::join(socket(&dir, &held.id), size);
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
