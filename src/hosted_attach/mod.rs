//! A terminal joining a conversation a pi host holds among many: what it
//! types and each size it takes go to the host's socket as JSON lines, and
//! the screen's output comes back the same way. The conversation's screen
//! lives in the host, so leaving only detaches this terminal; the host
//! lets it go once the conversation closes. The host may ask it to edit a
//! draft in this terminal's own editor, for pi's Ctrl+G, or to suspend,
//! for pi's Ctrl+Z, after which it joins again once continued.
//!
//! The host's socket is `pi-host/<key>.sock` in the login's runtime
//! folder, its key `<realm>-attended` or `<realm>-background`, and its
//! frames are the harness's host socket's, version 1.

mod here;

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

pub use here::{FEATURES, Here, Tty};

use crate::hold::Size;
use crate::hold::attach::{SIZE_UNSAID, size_here};

/// The longest frame either end sends, past which the connection is
/// broken rather than read.
const LONGEST_FRAME: usize = 1024 * 1024;

/// The most rows or columns the host takes.
const LARGEST_SIDE: u16 = 1000;

/// What a terminal is given back as it leaves: its keys, pastes, mouse and
/// focus reports as a fresh one has them, its cursor shown and its main
/// screen, whatever the screen it showed asked for.
const LEFT: &str = "\x1b[<u\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l\
                    \x1b[?1006l\x1b[?25h\x1b[?1049l";

/// How a joined terminal stopped showing the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Left {
    /// The host let it go, saying why: the conversation closed, or this
    /// terminal fell too far behind.
    Detached(String),
    /// The host's end of the connection closed without saying.
    Gone,
    /// The host let it go to suspend, as pi's Ctrl+Z asks, to join again
    /// once continued.
    Suspended,
}

/// The socket of the host `key` names in `realm`, in the runtime folder
/// `runtime`, refusing a key of another realm or of no kind a host is.
///
/// # Errors
///
/// When `key` isn't `<realm>-attended` or `<realm>-background`.
pub fn socket(runtime: &Path, realm: &str, key: &str) -> Result<PathBuf, String> {
    let kind = key
        .strip_prefix(realm)
        .and_then(|rest| rest.strip_prefix('-'));
    match kind {
        Some("attended" | "background") => Ok(runtime.join("pi-host").join(format!("{key}.sock"))),
        _ => Err(format!(
            "{key:?} is no host of realm {realm}, which are {realm}-attended and {realm}-background"
        )),
    }
}

/// The words that join the conversation `session` within the pi host
/// `host` of `realm` on this machine, from a terminal here.
#[must_use]
pub fn attach_here(realm: &str, host: &str, session: &str) -> Vec<String> {
    [
        "mycelium",
        "--realm",
        realm,
        "attach",
        "--host",
        host,
        "--session",
        session,
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Joins `session` through the host serving `socket` from this process's
/// own terminal, raw while joined, telling the host each size it takes,
/// until the host lets it go or goes. Suspended, it stops this process,
/// giving the terminal back, and joins again once continued, so it never
/// answers [`Left::Suspended`].
///
/// # Errors
///
/// When the host can't be reached, refuses the session, or breaks its
/// frames.
pub async fn join_here(socket: &Path, session: &str) -> io::Result<Left> {
    let mut here = Tty::enter()?;
    let (tell, mut sizes) = mpsc::unbounded_channel();
    let mut changes = signal(SignalKind::window_change())?;
    let watching = async move {
        while changes.recv().await.is_some() {
            if let Some(size) = size_here()
                && tell.send(size).is_err()
            {
                return;
            }
        }
    };
    tokio::pin!(watching);
    let mut output = tokio::io::stdout();
    loop {
        // A size from before a suspend is older than the one joined at.
        while sizes.try_recv().is_ok() {}
        let size = size_here().unwrap_or(SIZE_UNSAID);
        let left = tokio::select! {
            left = join(socket, session, size, &mut here, &mut output, &mut sizes) => left,
            () = &mut watching => Ok(Left::Gone),
        };
        output.write_all(LEFT.as_bytes()).await?;
        output.flush().await?;
        match left {
            Ok(Left::Suspended) => here.suspend().await?,
            left => return left,
        }
    }
}

/// Joins `session` through the host serving `socket` at `size`: what is
/// typed `here` goes to its screen, each size from `sizes` is the
/// terminal's new one, and the screen's output goes to `output`. An edit
/// the host asks for is made in `here`'s editor, and its answer sent back.
/// It answers once the host lets it go, suspends it, or its connection
/// ends. Once `here` has no more keys, nothing more is typed, but the
/// screen's output still comes.
///
/// # Errors
///
/// When the host can't be reached, refuses the session, or breaks its
/// frames.
pub async fn join(
    socket: &Path,
    session: &str,
    size: Size,
    here: &mut impl Here,
    mut output: impl AsyncWrite + Unpin,
    sizes: &mut mpsc::UnboundedReceiver<Size>,
) -> io::Result<Left> {
    let stream = UnixStream::connect(socket).await?;
    let (from, mut to) = stream.into_split();
    let mut from = Frames::new(BufReader::new(from));
    let (rows, columns) = sides(size);
    let asked = json!({
        "v": 1, "id": "attach", "op": "attach",
        "sessionId": session, "rows": rows, "columns": columns,
        "features": here.features(),
    });
    send(&mut to, &asked).await?;
    let Some(answer) = from.next().await? else {
        return Ok(Left::Gone);
    };
    if answer["ok"] != Value::Bool(true) {
        let why = answer["error"]["message"]
            .as_str()
            .unwrap_or("the host refused");
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            why.to_owned(),
        ));
    }
    let mut typing = Typing::default();
    let mut typed = true;
    let mut resized = true;
    loop {
        tokio::select! {
            said = from.next() => {
                let Some(said) = said? else {
                    return Ok(Left::Gone);
                };
                if let Some(left) = heed(&said, here, &mut output, &mut to).await? {
                    return Ok(left);
                }
            },
            keys = here.keys(), if typed => match keys {
                None => typed = false,
                Some(keys) => {
                    let text = typing.text(&keys);
                    if !text.is_empty() {
                        send(&mut to, &json!({"type": "input", "data": text})).await?;
                    }
                }
            },
            size = sizes.recv(), if resized => match size {
                Some(size) => {
                    let (rows, columns) = sides(size);
                    let resize = json!({"type": "resize", "rows": rows, "columns": columns});
                    send(&mut to, &resize).await?;
                }
                None => resized = false,
            },
        }
    }
}

/// Does what one frame from the host says, answering how it let this
/// terminal go if it did. A frame of a kind it doesn't know is passed
/// over, so a later host can say more without leaving behind the viewers
/// it has.
async fn heed(
    said: &Value,
    here: &mut impl Here,
    output: &mut (impl AsyncWrite + Unpin),
    to: &mut (impl AsyncWrite + Unpin),
) -> io::Result<Option<Left>> {
    match said["type"].as_str() {
        Some("output") => {
            let data = said["data"].as_str().unwrap_or_default();
            output.write_all(data.as_bytes()).await?;
            output.flush().await?;
        }
        // The host sends nothing else to this terminal until it answers,
        // and a draft it never gets back is kept.
        Some("edit") => {
            let draft = said["text"].as_str().unwrap_or_default();
            let mut edited = json!({"type": "edited", "id": said["id"]});
            if let Some(text) = here.edit(draft).await {
                edited["text"] = Value::String(text);
            }
            send(to, &edited).await?;
        }
        Some("suspend") => return Ok(Some(Left::Suspended)),
        Some("detached") => {
            let why = said["reason"].as_str().unwrap_or("detached");
            return Ok(Some(Left::Detached(why.to_owned())));
        }
        Some("error") => {
            let why = said["message"].as_str().unwrap_or("the host failed");
            return Err(io::Error::other(why.to_owned()));
        }
        _ => {}
    }
    Ok(None)
}

/// Typed bytes as the text the host takes: a character split between two
/// reads waits for its end, and a byte no character starts with is said
/// as the replacement character.
#[derive(Default)]
struct Typing {
    partial: Vec<u8>,
}

impl Typing {
    fn text(&mut self, read: &[u8]) -> String {
        let mut bytes = std::mem::take(&mut self.partial);
        bytes.extend_from_slice(read);
        let mut text = String::new();
        let mut rest = bytes.as_slice();
        loop {
            match std::str::from_utf8(rest) {
                Ok(whole) => {
                    text.push_str(whole);
                    return text;
                }
                Err(error) => {
                    let (good, after) = rest.split_at(error.valid_up_to());
                    // Checked as valid just now.
                    text.push_str(std::str::from_utf8(good).unwrap_or_default());
                    if let Some(bad) = error.error_len() {
                        text.push(char::REPLACEMENT_CHARACTER);
                        rest = &after[bad..];
                    } else {
                        self.partial = after.to_vec();
                        return text;
                    }
                }
            }
        }
    }
}

/// A size as the host takes it, each side at most the largest it takes.
fn sides(size: Size) -> (u16, u16) {
    (
        size.rows.clamp(1, LARGEST_SIDE),
        size.cols.clamp(1, LARGEST_SIDE),
    )
}

async fn send(to: &mut (impl AsyncWrite + Unpin), said: &Value) -> io::Result<()> {
    let mut line = serde_json::to_vec(said).map_err(io::Error::other)?;
    line.push(b'\n');
    to.write_all(&line).await
}

/// The frames a connection sends, read so that a read another branch of a
/// `select!` outran goes on where it stopped rather than losing its start.
struct Frames<R> {
    from: R,
    line: Vec<u8>,
}

impl<R: AsyncBufRead + Unpin> Frames<R> {
    fn new(from: R) -> Self {
        Self {
            from,
            line: Vec::new(),
        }
    }

    /// The next frame, or none once the connection ends between frames.
    async fn next(&mut self) -> io::Result<Option<Value>> {
        let room = (LONGEST_FRAME + 1).saturating_sub(self.line.len());
        let limit = u64::try_from(room).unwrap_or(u64::MAX);
        (&mut self.from)
            .take(limit)
            .read_until(b'\n', &mut self.line)
            .await?;
        let mut line = std::mem::take(&mut self.line);
        if line.is_empty() {
            return Ok(None);
        }
        if line.pop() != Some(b'\n') {
            return Err(broken("a frame longer than 1 MiB, or cut short"));
        }
        serde_json::from_slice(&line)
            .map(Some)
            .map_err(|_| broken("a frame that isn't JSON"))
    }
}

fn broken(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("the host sent {what}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    const SIZE: Size = Size {
        rows: 40,
        cols: 120,
    };

    #[test]
    fn a_hosts_socket_is_its_realms_and_kinds() {
        let runtime = Path::new("/run/user/1000");
        assert_eq!(
            socket(runtime, "work", "work-attended"),
            Ok(PathBuf::from("/run/user/1000/pi-host/work-attended.sock")),
            "the attended host of realm work"
        );
        assert!(
            socket(runtime, "work", "home-attended").is_err(),
            "a host of another realm is refused"
        );
        assert!(
            socket(runtime, "work", "work-../x").is_err(),
            "a key of no kind is refused, path and all"
        );
    }

    #[test]
    fn a_character_split_between_reads_is_said_once_whole() {
        let mut typing = Typing::default();
        let snow = "❄".as_bytes();
        assert_eq!(typing.text(&snow[..1]), "", "its start waits");
        assert_eq!(typing.text(&snow[1..]), "❄", "and comes whole with its end");
        assert_eq!(
            typing.text(b"a\xffb"),
            "a\u{fffd}b",
            "a byte no character starts with is the replacement character"
        );
    }

    #[test]
    fn a_size_past_what_the_host_takes_is_its_largest() {
        assert_eq!(
            sides(Size {
                rows: 0,
                cols: 4000
            }),
            (1, 1000)
        );
    }

    /// What a host in a test does, in turn, once it has answered.
    enum Step {
        /// Waits for the viewer's next frame, keeping it.
        Hear,
        /// Sends the viewer this frame.
        Say(Value),
    }

    const ACCEPTED: &str =
        r#"{"v": 1, "id": "attach", "ok": true, "result": {"sessionId": "s-1"}}"#;

    fn accepted() -> Value {
        serde_json::from_str(ACCEPTED).expect("JSON")
    }

    /// A host that answers one attach as `answer`, then takes its `steps`,
    /// keeping what the viewer said. It serves on the test's runtime.
    fn host(
        answer: Value,
        steps: Vec<Step>,
    ) -> (
        tempfile::TempDir,
        PathBuf,
        tokio::task::JoinHandle<Vec<Value>>,
    ) {
        let dir = tempfile::tempdir().expect("a folder");
        let path = dir.path().join("work-attended.sock");
        let listener = UnixListener::bind(&path).expect("a socket");
        let serving = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("a viewer");
            let (from, mut to) = stream.into_split();
            let mut from = Frames::new(BufReader::new(from));
            let mut heard = vec![from.next().await.expect("a frame").expect("an attach")];
            send(&mut to, &answer).await.expect("answered");
            if answer["ok"] != Value::Bool(true) {
                return heard;
            }
            for step in steps {
                match step {
                    Step::Hear => heard.push(from.next().await.expect("a frame").expect("one")),
                    Step::Say(said) => send(&mut to, &said).await.expect("sent"),
                }
            }
            heard
        });
        (dir, path, serving)
    }

    /// A terminal in a test: it types its keys, then no more, and its
    /// editor gives each answer in turn, keeping each draft it was given.
    struct Script {
        keys: std::collections::VecDeque<Vec<u8>>,
        answers: std::collections::VecDeque<Option<String>>,
        drafts: Vec<String>,
    }

    impl Script {
        fn typing(keys: &[u8]) -> Self {
            Self {
                keys: [keys.to_vec()].into(),
                answers: std::collections::VecDeque::new(),
                drafts: Vec::new(),
            }
        }
    }

    impl Here for Script {
        fn features(&self) -> &'static [&'static str] {
            &FEATURES
        }

        fn keys(&mut self) -> impl Future<Output = Option<Vec<u8>>> {
            std::future::ready(self.keys.pop_front())
        }

        fn edit(&mut self, text: &str) -> impl Future<Output = Option<String>> {
            self.drafts.push(text.to_owned());
            std::future::ready(self.answers.pop_front().flatten())
        }
    }

    #[tokio::test]
    async fn a_viewer_types_and_shows_until_the_host_lets_it_go() {
        let (_dir, path, serving) = host(
            accepted(),
            vec![
                // What the viewer types comes before anything is sent back.
                Step::Hear,
                Step::Say(json!({"type": "output", "data": "hello ❄"})),
                Step::Say(json!({"type": "detached", "reason": "Conversation closed"})),
            ],
        );
        let (_tell, mut sizes) = mpsc::unbounded_channel();
        let mut shown = Vec::new();
        let mut here = Script::typing(b"\x1b[13u");
        let left = join(&path, "s-1", SIZE, &mut here, &mut shown, &mut sizes)
            .await
            .expect("joined");
        assert_eq!(left, Left::Detached("Conversation closed".to_owned()));
        assert_eq!(
            String::from_utf8_lossy(&shown),
            "hello ❄",
            "the screen's output, as sent"
        );
        assert_eq!(
            serving.await.expect("the host ran"),
            vec![
                json!({"v": 1, "id": "attach", "op": "attach", "sessionId": "s-1",
                       "rows": 40, "columns": 120, "features": ["edit", "suspend"]}),
                json!({"type": "input", "data": "\x1b[13u"}),
            ],
            "the attach at its size with what it can do, then the keys as typed"
        );
    }

    #[tokio::test]
    async fn an_edit_the_host_asks_for_comes_back_as_the_editor_left_it() {
        let (_dir, path, serving) = host(
            accepted(),
            vec![
                Step::Hear,
                Step::Say(json!({"type": "edit", "id": "e1", "text": "a draft"})),
                Step::Hear,
                Step::Say(json!({"type": "edit", "id": "e2", "text": "again"})),
                Step::Hear,
                Step::Say(json!({"type": "detached", "reason": "Conversation closed"})),
            ],
        );
        let (_tell, mut sizes) = mpsc::unbounded_channel();
        let mut here = Script::typing(b"\x07");
        here.answers = [Some("a draft and more".to_owned()), None].into();
        let left = join(&path, "s-1", SIZE, &mut here, Vec::new(), &mut sizes)
            .await
            .expect("joined");
        assert_eq!(left, Left::Detached("Conversation closed".to_owned()));
        assert_eq!(
            here.drafts,
            ["a draft", "again"],
            "each draft, in its editor"
        );
        let heard = serving.await.expect("the host ran");
        assert_eq!(
            heard[2..],
            [
                json!({"type": "edited", "id": "e1", "text": "a draft and more"}),
                json!({"type": "edited", "id": "e2"}),
            ],
            "each edit's answer, with no text where the editor failed"
        );
    }

    #[tokio::test]
    async fn a_host_that_suspends_the_viewer_lets_it_go_to_come_back() {
        let (_dir, path, serving) = host(
            accepted(),
            vec![Step::Hear, Step::Say(json!({"type": "suspend"}))],
        );
        let (_tell, mut sizes) = mpsc::unbounded_channel();
        let mut here = Script::typing(b"\x1a");
        let left = join(&path, "s-1", SIZE, &mut here, Vec::new(), &mut sizes)
            .await
            .expect("joined");
        serving.await.expect("the host ran");
        assert_eq!(left, Left::Suspended);
    }

    #[tokio::test]
    async fn a_frame_that_comes_in_pieces_between_keys_is_read_whole() {
        let (mut near, far) = tokio::io::duplex(64);
        let mut frames = Frames::new(BufReader::new(far));
        near.write_all(br#"{"type": "out"#).await.expect("written");
        let outran = tokio::select! {
            biased;
            _ = frames.next() => false,
            () = std::future::ready(()) => true,
        };
        assert!(outran, "half a frame is no frame yet");
        near.write_all(b"put\"}\n").await.expect("written");
        assert_eq!(
            frames.next().await.expect("read"),
            Some(json!({"type": "output"})),
            "the start read before is kept"
        );
    }

    #[tokio::test]
    async fn a_host_that_goes_away_mid_join_lets_the_viewer_go() {
        let (_dir, path, serving) = host(
            accepted(),
            vec![
                Step::Hear,
                Step::Say(json!({"type": "title", "data": "a kind a later host might add"})),
                Step::Say(json!({"type": "output", "data": "bye"})),
            ],
        );
        // The terminal still has a size to say, so only the host ends it.
        let (_tell, mut sizes) = mpsc::unbounded_channel();
        let mut shown = Vec::new();
        let mut here = Script::typing(b"x");
        let left = join(&path, "s-1", SIZE, &mut here, &mut shown, &mut sizes)
            .await
            .expect("joined");
        serving.await.expect("the host ran");
        assert_eq!(
            left,
            Left::Gone,
            "an unknown frame is passed over, and a closed connection says the host has gone"
        );
        assert_eq!(
            String::from_utf8_lossy(&shown),
            "bye",
            "what it showed before it went"
        );
    }

    #[tokio::test]
    async fn a_session_the_host_refuses_says_why() {
        let (_dir, path, _serving) = host(
            json!({"v": 1, "id": "attach", "ok": false,
                   "error": {"code": "not_found", "message": "Conversation is not hosted: s-9"}}),
            Vec::new(),
        );
        let (_tell, mut sizes) = mpsc::unbounded_channel();
        let mut here = Script::typing(b"");
        let refused = join(&path, "s-9", SIZE, &mut here, Vec::new(), &mut sizes)
            .await
            .expect_err("refused");
        assert_eq!(refused.to_string(), "Conversation is not hosted: s-9");
    }
}
