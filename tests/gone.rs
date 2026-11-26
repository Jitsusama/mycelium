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

use mycelium::hold::holder::GONE_AGAIN;
use mycelium::hold::{Said, socket, watch};
use mycelium::record;

/// How long anything here has to happen before the test gives up.
const PATIENCE: Duration = Duration::from_secs(20);

/// How long a key typed in the window has to start the program again.
/// It is shorter than the pause a holder waits before trying again by
/// itself, so only the key can meet it.
const KEY_PATIENCE: Duration = Duration::from_secs(10);

/// How long a window shows nothing before what it showed is taken as
/// done: far longer than a run's late words take to arrive, and far
/// shorter than its holder's next try by itself.
const STILL: Duration = Duration::from_secs(2);

// A key is told apart from the pause only while the pause is the longer.
const _: () = assert!(KEY_PATIENCE.as_secs() < GONE_AGAIN.as_secs());

/// How the pi host answers a join it takes.
const ACCEPTED: &str = r#"{"v": 1, "id": "attach", "ok": true, "result": {"sessionId": "s-1"}}"#;

/// What the pi host shows the window once it is back.
const BACK: &str = "the conversation, back again";

/// What attach says when the pi host closes its join unanswered.
const GONE_WORDS: &str = "the pi host harbour-background has gone";

/// What attach says when it finds no pi host to join.
const NO_HOST: &str = "no pi host harbour-background is running here";

/// What attach says when it finds no pi host to join, and none has been
/// there since its holder started.
const NEVER_HOST: &str = "no pi host harbour-background has run here since this window opened";

/// What the holder says while no pi host by its program's name has been
/// here since it started.
const NEVER: &str = "No pi host by that name has run here since this window opened";

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
        // Each start lays the user manager's environment over the holder's,
        // and that names the real runtime folder, where attach would find
        // no stand-in host and this test would hang on its accept.
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
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
    /// Joins the holder `window1` held in `held`, once it serves this
    /// window. A connect alone returns before then, and under load the
    /// holder could show its program's first words before the window was
    /// joined to them, leaving it only the redraw that follows. The holder
    /// lists a window that says which it is only once that window is
    /// joined to what its program shows, so this waits for the listing.
    fn joins(held: &Path) -> Self {
        let (_watching, mut changed) = watch(held).expect("the held folder is watched");
        let mut stream = UnixStream::connect(socket(held, "window1")).expect("a window joins");
        let window = record::Window {
            terminal: "kitty".into(),
            id: "1".into(),
            within: None,
            app_id: None,
        };
        stream
            .write_all(&Said::Window(window).encode())
            .expect("the window says which it is");
        let waiting = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("the window has a clock to wait by");
        while !mycelium::hold::held(held)
            .iter()
            .any(|one| !one.windows.is_empty())
        {
            let told = waiting.block_on(tokio::time::timeout(PATIENCE, changed.recv()));
            assert!(
                matches!(told, Ok(Some(()))),
                "the holder never listed the window as joined"
            );
        }
        Self {
            stream,
            unseen: String::new(),
        }
    }

    /// Reads until the window has shown `wanted`, failing once `patience`
    /// passes with nothing more shown, with what it did show. What came
    /// after it is kept for the next look.
    fn sees(&mut self, wanted: &str, patience: Duration) {
        self.sees_then(wanted, "", patience);
    }

    /// Reads until the window has shown `first`, then `then` after it,
    /// failing once `patience` passes with nothing more shown. What came
    /// after `then` is kept for the next look. Attach's last words come
    /// before it exits, and the holder draws its away line again after
    /// any of them read late, so a run's away line is looked for after
    /// that run's own words: one drawn for an earlier run is never taken
    /// for it.
    fn sees_then(&mut self, first: &str, then: &str, patience: Duration) {
        self.stream
            .set_read_timeout(Some(patience))
            .expect("the window's patience is set");
        let mut buffer = [0u8; 4096];
        loop {
            let after = self.unseen.find(first).map(|at| at + first.len());
            if let Some(end) =
                after.and_then(|after| Some(after + self.unseen[after..].find(then)? + then.len()))
            {
                self.unseen.drain(..end);
                return;
            }
            let read = self.stream.read(&mut buffer).unwrap_or(0);
            assert!(
                read > 0,
                "the window never showed {first:?} then {then:?}; it showed {:?}",
                self.unseen
            );
            self.unseen
                .push_str(&String::from_utf8_lossy(&buffer[..read]));
        }
    }

    /// Reads, and lets go of, everything the window shows until it has
    /// shown nothing for `still`, so that a later look reads only what
    /// comes after: a run's late words, and the holder's line drawn again
    /// after them, are never taken for the next run's.
    fn quiets(&mut self, still: Duration) {
        self.stream
            .set_read_timeout(Some(still))
            .expect("the window's patience is set");
        let mut buffer = [0u8; 4096];
        while self.stream.read(&mut buffer).is_ok_and(|read| read > 0) {}
        self.unseen.clear();
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
    window.sees_then(GONE_WORDS, AWAY, PATIENCE);
    going.join().expect("the host stopped");

    // With the host down and nobody taking a join at its socket, a key
    // tries again at once, can't reach it, and the window stays, told
    // again that its host is away.
    window.types_return();
    window.sees_then(UNREACHABLE, AWAY, KEY_PATIENCE);

    // With its socket gone too, the next key finds no host at all, one it
    // had since it started, so it's still told its host is away.
    fs::remove_file(&at).expect("the stopped host's socket goes");
    window.types_return();
    window.sees_then(NO_HOST, AWAY, KEY_PATIENCE);
    assert!(
        holder.0.try_wait().expect("the holder is asked").is_none(),
        "the holder keeps its window while its pi host is down"
    );

    // Back, the host answers the join the next key sends, and shows the
    // conversation.
    let serving = serves(UnixListener::bind(&at).expect("the pi host listens again"));
    window.types_return();
    window.sees(BACK, KEY_PATIENCE);
    let again = serving.join().expect("the host served the join");
    drop(again);
}

/// The pi host listening at `host` answering the next join and showing
/// the conversation, its connection kept open until the thread is joined.
fn serves(host: UnixListener) -> std::thread::JoinHandle<UnixStream> {
    std::thread::spawn(move || {
        let mut again = joined(&host).into_inner();
        again
            .write_all(format!("{ACCEPTED}\n").as_bytes())
            .expect("the host answers");
        let output = format!(r#"{{"type":"output","data":"{BACK}"}}"#);
        again
            .write_all(format!("{output}\n").as_bytes())
            .expect("the host shows the conversation");
        again
    })
}

#[test]
fn a_window_whose_pi_host_never_ran_here_says_so_and_joins_one_once_it_starts() {
    let home = tempfile::tempdir().expect("a temporary home");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home.path().join("r/pi-host"))
        .expect("its runtime folder is made");
    let at = home.path().join("r/pi-host/harbour-background.sock");

    // No host by its name has been here since the holder started, as for a
    // realm mistyped, so the window isn't told a host it never had is
    // away, and it stays.
    let mut holder = holder(home.path());
    let mut window = Window::joins(&home.path().join("r/mycelium/harbour.held"));
    // Joined after its first try may have ended, the window is told by the
    // holder itself; a key tries again with it there to see attach say so.
    window.sees(NEVER, PATIENCE);
    window.quiets(STILL);
    window.types_return();
    window.sees_then(NEVER_HOST, NEVER, KEY_PATIENCE);
    assert!(
        holder.0.try_wait().expect("the holder is asked").is_none(),
        "the holder keeps its window, as a host not yet started may come"
    );

    // A socket nobody listens on, as a host that crashed before the window
    // opened leaves, says no more that one ran here.
    drop(UnixListener::bind(&at).expect("a host listens, then crashes"));
    window.quiets(STILL);
    window.types_return();
    window.sees_then(UNREACHABLE, NEVER, KEY_PATIENCE);
    fs::remove_file(&at).expect("the crashed host's socket is cleaned up");

    // One starting, the next key joins it.
    let serving = serves(UnixListener::bind(&at).expect("the pi host listens"));
    window.types_return();
    window.sees(BACK, KEY_PATIENCE);
    let again = serving.join().expect("the host served the join");
    drop(again);
}
