//! The terminal a viewer joins from: the keys typed in it, read on a
//! thread of their own that stands aside when asked, and its editor, which
//! has the terminal to itself while it runs, as pi's own editor has pi's.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use rustix::process::{Signal, getpid, kill_process};
use rustix::termios::{self, OptionalActions, SpecialCodeIndex, Termios};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

/// What a viewer's client can do for the host, as it says at attach.
pub const FEATURES: [&str; 2] = ["edit", "suspend"];

/// The terminal a viewer runs in, as joining a conversation uses it.
pub trait Here {
    /// What this terminal can do for the host: edit, suspend, or neither.
    fn features(&self) -> &'static [&'static str];

    /// The next keys typed here, or none once nothing more can be.
    fn keys(&mut self) -> impl Future<Output = Option<Vec<u8>>>;

    /// Edits `text` in this terminal's editor, which has the terminal to
    /// itself until it ends: what the file holds then, or none when the
    /// editor failed.
    fn edit(&mut self, text: &str) -> impl Future<Output = Option<String>>;
}

/// This process's terminal, raw while joined, its keys read on a thread of
/// their own. Input that isn't a terminal is read as it is, and has no
/// editor to give nor a shell to suspend to.
pub struct Tty {
    keys: mpsc::Receiver<Vec<u8>>,
    gate: Arc<Gate>,
    cooked: Option<Termios>,
    edits: u32,
}

impl Tty {
    /// Takes this process's terminal raw and starts reading its keys.
    ///
    /// # Errors
    ///
    /// When the terminal can't be set raw, or no thread can read it.
    pub fn enter() -> io::Result<Self> {
        let input = io::stdin();
        let cooked = if termios::isatty(&input) {
            let was = termios::tcgetattr(&input)?;
            termios::tcsetattr(&input, OptionalActions::Now, &reading(&was))?;
            Some(was)
        } else {
            None
        };
        let (tell, keys) = mpsc::channel(64);
        let gate = Arc::new(Gate::default());
        let reader = Arc::clone(&gate);
        let tty = cooked.is_some();
        std::thread::Builder::new()
            .name("keys".to_owned())
            .spawn(move || read_keys(&reader, &tell, tty))?;
        Ok(Self {
            keys,
            gate,
            cooked,
            edits: 0,
        })
    }

    /// Gives the terminal back as it was and stops this process, as Ctrl+Z
    /// stops a program; once continued, takes it raw again.
    ///
    /// # Errors
    ///
    /// When the process can't stop itself.
    pub async fn suspend(&mut self) -> io::Result<()> {
        let Some(cooked) = self.cooked.clone() else {
            return Ok(());
        };
        self.stand_aside().await;
        set(&cooked);
        let stopped = kill_process(getpid(), Signal::TSTP);
        // Here once the shell continues this process.
        set(&reading(&cooked));
        self.gate.resume();
        stopped.map_err(io::Error::from)
    }

    async fn stand_aside(&self) {
        let gate = Arc::clone(&self.gate);
        // The reader stands aside between reads, each a tenth of a second at
        // most, so this waits off the runtime's own thread.
        let _ = tokio::task::spawn_blocking(move || gate.pause()).await;
    }
}

impl Here for Tty {
    fn features(&self) -> &'static [&'static str] {
        if self.cooked.is_some() {
            &FEATURES
        } else {
            &[]
        }
    }

    async fn keys(&mut self) -> Option<Vec<u8>> {
        self.keys.recv().await
    }

    async fn edit(&mut self, text: &str) -> Option<String> {
        let cooked = self.cooked.clone()?;
        self.stand_aside().await;
        set(&cooked);
        // Ctrl+C in an editor that leaves the terminal cooked reaches this
        // process too, which would leave the conversation for it.
        let interrupts = signal(SignalKind::interrupt()).ok();
        self.edits += 1;
        let editor = editor_named(std::env::var("VISUAL").ok(), std::env::var("EDITOR").ok());
        let edited = edit_in_editor(text, self.edits, &editor).await;
        drop(interrupts);
        set(&reading(&cooked));
        self.gate.resume();
        edited
    }
}

impl Drop for Tty {
    fn drop(&mut self) {
        if let Some(cooked) = &self.cooked {
            set(cooked);
        }
    }
}

/// `cooked` made raw, each read giving up after a tenth of a second with no
/// key, so the reader can stand aside for an editor between reads.
fn reading(cooked: &Termios) -> Termios {
    let mut raw = cooked.clone();
    raw.make_raw();
    raw.special_codes[SpecialCodeIndex::VMIN] = 0;
    raw.special_codes[SpecialCodeIndex::VTIME] = 1;
    raw
}

fn set(modes: &Termios) {
    // A terminal gone by now has nothing to set.
    let _ = termios::tcsetattr(io::stdin(), OptionalActions::Now, modes);
}

/// Reads keys until nobody takes them or input ends, standing aside while
/// asked. A terminal's read that comes back empty only had no key in time.
fn read_keys(gate: &Gate, tell: &mpsc::Sender<Vec<u8>>, tty: bool) {
    let mut buffer = [0u8; 4096];
    loop {
        gate.stand_aside_while_paused();
        match rustix::io::read(io::stdin(), &mut buffer) {
            Ok(0) if tty => {}
            Ok(0) => break,
            Ok(read) => {
                if tell.blocking_send(buffer[..read].to_vec()).is_err() {
                    break;
                }
            }
            Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => {}
            Err(_) => break,
        }
    }
    gate.finish();
}

/// Where the keys reader and whoever wants the terminal meet: one asks it
/// to stand aside and waits until it has, the reader waits until let back.
#[derive(Default)]
struct Gate {
    state: Mutex<Reading>,
    changed: Condvar,
}

#[derive(Default)]
struct Reading {
    paused: bool,
    aside: bool,
    done: bool,
}

impl Gate {
    fn lock(&self) -> MutexGuard<'_, Reading> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'a>(&self, state: MutexGuard<'a, Reading>) -> MutexGuard<'a, Reading> {
        self.changed
            .wait(state)
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Asks the reader to stand aside, back once it has or has stopped.
    fn pause(&self) {
        let mut state = self.lock();
        state.paused = true;
        self.changed.notify_all();
        while !state.aside && !state.done {
            state = self.wait(state);
        }
    }

    fn resume(&self) {
        self.lock().paused = false;
        self.changed.notify_all();
    }

    fn stand_aside_while_paused(&self) {
        let mut state = self.lock();
        if !state.paused {
            return;
        }
        state.aside = true;
        self.changed.notify_all();
        while state.paused {
            state = self.wait(state);
        }
        state.aside = false;
    }

    fn finish(&self) {
        self.lock().done = true;
        self.changed.notify_all();
    }
}

/// Edits `text` with the shell words `editor` in a file of this edit's
/// own, gone once read back: what the editor left in it, or none when it
/// failed.
async fn edit_in_editor(text: &str, edit: u32, editor: &str) -> Option<String> {
    let file = std::env::temp_dir().join(format!("mycelium-edit-{}-{edit}.md", std::process::id()));
    // A file already there is someone else's, and left alone.
    let mut opened = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&file)
        .ok()?;
    let written = opened.write_all(text.as_bytes());
    drop(opened);
    let edited = if written.is_ok() && editor_ran(editor, &file).await {
        fs::read_to_string(&file).ok()
    } else {
        None
    };
    // This edit's own file; one that has already gone needs nothing.
    let _ = fs::remove_file(&file);
    edited
}

async fn editor_ran(editor: &str, file: &Path) -> bool {
    tokio::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(file)
        .status()
        .await
        .is_ok_and(|status| status.success())
}

/// The editor a terminal's user named, `$VISUAL` before `$EDITOR`, as
/// shell words, or vi when they named none.
fn editor_named(visual: Option<String>, editor: Option<String>) -> String {
    [visual, editor]
        .into_iter()
        .flatten()
        .find(|named| !named.trim().is_empty())
        .unwrap_or_else(|| "vi".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_editor_is_the_one_its_user_named() {
        let named = |visual: Option<&str>, editor: Option<&str>| {
            editor_named(visual.map(str::to_owned), editor.map(str::to_owned))
        };
        assert_eq!(named(Some("nvim"), Some("nano")), "nvim", "VISUAL first");
        assert_eq!(
            named(Some(" "), Some("code -w")),
            "code -w",
            "a blank VISUAL is none"
        );
        assert_eq!(named(None, None), "vi", "vi when none is named");
    }

    fn edit_file(edit: u32) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("mycelium-edit-{}-{edit}.md", std::process::id()))
    }

    #[tokio::test]
    async fn an_edit_comes_back_as_the_editor_left_it() {
        // Shell words, as $VISUAL may be, that add to the file they're given.
        let edited = edit_in_editor("a draft", 9001, "printf ' and more' >>").await;
        assert_eq!(edited.as_deref(), Some("a draft and more"));
        assert!(!edit_file(9001).exists(), "its file is gone once read back");
    }

    #[tokio::test]
    async fn an_editor_that_fails_gives_nothing_back() {
        let edited = edit_in_editor("a draft", 9002, "false").await;
        assert_eq!(edited, None, "the draft is kept");
        assert!(!edit_file(9002).exists(), "its file is gone all the same");
    }
}
