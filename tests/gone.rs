//! A holder whose program is a window on a pi host's conversation, joined
//! by `mycelium attach --host`, when the host stops before answering it
//! and stays down a while: the holder, the attach and the host each a
//! process or socket of their own, as a terminal's window on a hosted
//! session runs them. On 19 November 2026 a window closed this way as its
//! host restarted, since the holder ended with its program's 69; it now
//! keeps the window, and tries again while the host is still down.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use mycelium::hold::{Said, socket};

/// How long anything here has to happen before the test gives up.
const PATIENCE: Duration = Duration::from_secs(20);

/// How long a key typed in the window has to start the program again.
/// It is shorter than the pause a holder waits before trying again by
/// itself, so only the key can meet it.
const KEY_PATIENCE: Duration = Duration::from_secs(10);

/// How the pi host answers a join it takes.
const ACCEPTED: &str = r#"{"v": 1, "id": "attach", "ok": true, "result": {"sessionId": "s-1"}}"#;

/// What the pi host shows the window once it is back.
const BACK: &str = "the conversation, back again";

/// What attach says when it finds no pi host to join.
const NO_HOST: &str = "no pi host harbour-background is running here";

/// What attach says when the pi host's socket is there but nobody takes
/// the join.
const UNREACHABLE: &str = "the pi host harbour-background can't be reached";

/// What the holder says while its program's pi host is away.
const AWAY: &str = "pi host is away";

/// A process that is stopped when the test is done with it.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        // One that ended already has nothing to stop.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A holder of a window on the conversation `s1` within the pi host
/// `harbour-background`, run as a machine whose runtime folder is under
/// `home`, answering once it says it holds.
fn holder(home: &Path) -> Running {
    let attach = format!(
        "exec {} --realm harbour attach --host harbour-background --session s1",
        env!("CARGO_BIN_EXE_mycelium")
    );
    let mut holder = Command::new(env!("CARGO_BIN_EXE_mycelium"))
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", home.join("r"))
        .env("XDG_STATE_HOME", home.join("s"))
        .args([
            "--realm", "harbour", "holder", "--id", "window1", "--folder",
        ])
        .arg(home)
        .args(["--", "sh", "-c", &attach])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the holder starts");
    let mut said = String::new();
    BufReader::new(holder.stdout.take().expect("its standard output"))
        .read_line(&mut said)
        .expect("the holder says how it went");
    assert_eq!(said.trim(), "held", "the holder holds its program");
    Running(holder)
}

/// A window joined to the holder, keeping what it has been shown past
/// each thing looked for, so the next look sees it.
struct Window {
    stream: UnixStream,
    unseen: String,
}

impl Window {
    /// Joins the holder `window1` held in `held`.
    fn joins(held: &Path) -> Self {
        let stream = UnixStream::connect(socket(held, "window1")).expect("a window joins");
        Self {
            stream,
            unseen: String::new(),
        }
    }

    /// Reads until the window has shown `wanted`, failing once `patience`
    /// passes with nothing more shown, with what it did show. What came
    /// after it is kept for the next look.
    fn sees(&mut self, wanted: &str, patience: Duration) {
        self.sees_each(&[wanted], patience);
    }

    /// Reads until the window has shown each of `wanted`, in any order, as
    /// attach's last words and the holder's own may come either way round,
    /// failing once `patience` passes with nothing more shown. What came
    /// after the last of them is kept for the next look.
    fn sees_each(&mut self, wanted: &[&str], patience: Duration) {
        self.stream
            .set_read_timeout(Some(patience))
            .expect("the window's patience is set");
        let mut buffer = [0u8; 4096];
        loop {
            let ends: Option<Vec<usize>> = wanted
                .iter()
                .map(|one| self.unseen.find(one).map(|at| at + one.len()))
                .collect();
            if let Some(last) = ends.and_then(|ends| ends.into_iter().max()) {
                self.unseen.drain(..last);
                return;
            }
            let read = self.stream.read(&mut buffer).unwrap_or(0);
            assert!(
                read > 0,
                "the window never showed each of {wanted:?}; it showed {:?}",
                self.unseen
            );
            self.unseen
                .push_str(&String::from_utf8_lossy(&buffer[..read]));
        }
    }

    /// Types a Return.
    fn types_return(&mut self) {
        self.stream
            .write_all(&Said::Typed(b"\r".to_vec()).encode())
            .expect("the window types");
    }
}

/// Takes the next join at `listener` and reads the attach it opens with,
/// failing if it says nothing within the test's patience. A join that
/// never comes fails the window's wait instead.
fn joined(listener: &UnixListener) -> BufReader<UnixStream> {
    let (stream, _) = listener.accept().expect("the window's attach joins");
    stream
        .set_read_timeout(Some(PATIENCE))
        .expect("the host's patience is set");
    let mut from = BufReader::new(stream);
    let mut attach = String::new();
    from.read_line(&mut attach).expect("the attach is heard");
    assert!(
        attach.contains(r#""op":"attach""#) && attach.contains(r#""sessionId":"s1""#),
        "the window asks to join its conversation: {attach:?}"
    );
    from
}

#[test]
fn a_window_whose_pi_host_stops_stays_while_it_is_down_and_joins_it_once_back() {
    let home = tempfile::tempdir().expect("a temporary home");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home.path().join("r/pi-host"))
        .expect("its runtime folder is made");
    let at = home.path().join("r/pi-host/harbour-background.sock");
    let host = UnixListener::bind(&at).expect("the pi host listens");
    let (joining, window_joined) = mpsc::channel::<()>();
    let going = std::thread::spawn(move || {
        // The host stops, as a stopping one does, once the window is there
        // to see what follows: its first join is closed unanswered, and it
        // stops listening, its socket left behind for now.
        let first = joined(&host);
        window_joined
            .recv_timeout(PATIENCE)
            .expect("the window joins the holder");
        drop(first);
        drop(host);
    });

    let mut holder = holder(home.path());
    let mut window = Window::joins(&home.path().join("r/mycelium/harbour.held"));
    joining.send(()).expect("the host hears the window joined");
    window.sees(AWAY, PATIENCE);
    going.join().expect("the host stopped");

    // With the host down and nobody taking a join at its socket, a key
    // tries again at once, can't reach it, and the window stays, told
    // again that its host is away.
    window.types_return();
    window.sees_each(&[UNREACHABLE, AWAY], KEY_PATIENCE);

    // With its socket gone too, the next key finds no host at all.
    fs::remove_file(&at).expect("the stopped host's socket goes");
    window.types_return();
    window.sees_each(&[NO_HOST, AWAY], KEY_PATIENCE);
    assert!(
        holder.0.try_wait().expect("the holder is asked").is_none(),
        "the holder keeps its window while its pi host is down"
    );

    // Back, the host answers the join the next key sends, and shows the
    // conversation.
    let host = UnixListener::bind(&at).expect("the pi host listens again");
    let serving = std::thread::spawn(move || {
        let mut again = joined(&host).into_inner();
        again
            .write_all(format!("{ACCEPTED}\n").as_bytes())
            .expect("the host answers");
        let output = format!(r#"{{"type":"output","data":"{BACK}"}}"#);
        again
            .write_all(format!("{output}\n").as_bytes())
            .expect("the host shows the conversation");
        again
    });
    window.types_return();
    window.sees(BACK, KEY_PATIENCE);
    let again = serving.join().expect("the host served the join");
    drop(again);
}
