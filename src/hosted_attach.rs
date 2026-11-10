//! A terminal joining a conversation a pi host holds among many: what it
//! types and each size it takes go to the host's socket as JSON lines, and
//! the screen's output comes back the same way. The conversation's screen
//! lives in the host, so leaving only detaches this terminal; the host
//! lets it go once the conversation closes.
//!
//! The host's socket is `pi-host/<key>.sock` in the login's runtime
//! folder, its key `<realm>-attended` or `<realm>-background`, and its
//! frames are the harness's host socket's, version 1.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use crate::hold::Size;
use crate::hold::attach::{Raw, SIZE_UNSAID, size_here};

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
/// until the host lets it go or goes.
///
/// # Errors
///
/// When the host can't be reached, refuses the session, or breaks its
/// frames.
pub async fn join_here(socket: &Path, session: &str) -> io::Result<Left> {
    let raw = Raw::enter()?;
    let (tell, sizes) = mpsc::unbounded_channel();
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
    let size = size_here().unwrap_or(SIZE_UNSAID);
    let mut output = tokio::io::stdout();
    let left = tokio::select! {
        left = join(socket, session, size, tokio::io::stdin(), &mut output, sizes) => left,
        () = watching => Ok(Left::Gone),
    };
    output.write_all(LEFT.as_bytes()).await?;
    output.flush().await?;
    drop(raw);
    left
}

/// Joins `session` through the host serving `socket` at `size`: what
/// `input` gives is typed into its screen, each size from `sizes` is the
/// terminal's new one, and the screen's output goes to `output`. It
/// answers once the host lets it go or its connection ends. Once `input`
/// ends, nothing more is typed, but the screen's output still comes.
///
/// # Errors
///
/// When the host can't be reached, refuses the session, or breaks its
/// frames.
pub async fn join(
    socket: &Path,
    session: &str,
    size: Size,
    input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
    sizes: mpsc::UnboundedReceiver<Size>,
) -> io::Result<Left> {
    let stream = UnixStream::connect(socket).await?;
    let (from, mut to) = stream.into_split();
    let mut from = BufReader::new(from);
    let (rows, columns) = sides(size);
    let asked = json!({
        "v": 1, "id": "attach", "op": "attach",
        "sessionId": session, "rows": rows, "columns": columns,
    });
    send(&mut to, &asked).await?;
    let Some(answer) = frame(&mut from).await? else {
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
    tokio::select! {
        shown = show(&mut from, &mut output) => shown,
        typed = type_into(&mut to, input, sizes) => typed.map(|()| Left::Gone),
    }
}

/// Writes each output frame the host sends to `output`, until it lets
/// this terminal go.
async fn show(
    from: &mut (impl tokio::io::AsyncBufRead + Unpin),
    output: &mut (impl AsyncWrite + Unpin),
) -> io::Result<Left> {
    while let Some(said) = frame(from).await? {
        match said["type"].as_str() {
            Some("output") => {
                let data = said["data"].as_str().unwrap_or_default();
                output.write_all(data.as_bytes()).await?;
                output.flush().await?;
            }
            Some("detached") => {
                let why = said["reason"].as_str().unwrap_or("detached");
                return Ok(Left::Detached(why.to_owned()));
            }
            Some("error") => {
                let why = said["message"].as_str().unwrap_or("the host failed");
                return Err(io::Error::other(why.to_owned()));
            }
            _ => return Err(broken("a frame of no kind a viewer is sent")),
        }
    }
    Ok(Left::Gone)
}

/// Says what `input` gives and each size from `sizes` to the host, for as
/// long as the connection lasts.
async fn type_into(
    to: &mut (impl AsyncWrite + Unpin),
    mut input: impl AsyncRead + Unpin,
    mut sizes: mpsc::UnboundedReceiver<Size>,
) -> io::Result<()> {
    let mut buffer = vec![0u8; 4096];
    let mut typing = Typing::default();
    let mut typed = true;
    let mut resized = true;
    loop {
        tokio::select! {
            read = input.read(&mut buffer), if typed => match read {
                Ok(0) | Err(_) => typed = false,
                Ok(read) => {
                    let text = typing.text(&buffer[..read]);
                    if !text.is_empty() {
                        send(to, &json!({"type": "input", "data": text})).await?;
                    }
                }
            },
            size = sizes.recv(), if resized => match size {
                Some(size) => {
                    let (rows, columns) = sides(size);
                    send(to, &json!({"type": "resize", "rows": rows, "columns": columns})).await?;
                }
                None => resized = false,
            },
            // Nothing more will be said, and the host takes a closed
            // connection as leaving, so the connection stays.
            else => std::future::pending::<()>().await,
        }
    }
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

/// The next frame `from` sends, or none once it ends.
async fn frame(from: &mut (impl tokio::io::AsyncBufRead + Unpin)) -> io::Result<Option<Value>> {
    let mut line = Vec::new();
    let limit = u64::try_from(LONGEST_FRAME + 1).unwrap_or(u64::MAX);
    let read = (&mut *from)
        .take(limit)
        .read_until(b'\n', &mut line)
        .await?;
    if read == 0 {
        return Ok(None);
    }
    if line.pop() != Some(b'\n') {
        return Err(broken("a frame longer than 1 MiB, or cut short"));
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|_| broken("a frame that isn't JSON"))
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

    /// A host that answers one attach as `answer`, then sends `frames`,
    /// keeping what the viewer said. It serves on the test's runtime.
    fn host(
        answer: Value,
        frames: Vec<Value>,
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
            let mut from = BufReader::new(from);
            let mut heard = vec![frame(&mut from).await.expect("a frame").expect("an attach")];
            send(&mut to, &answer).await.expect("answered");
            if answer["ok"] != Value::Bool(true) {
                return heard;
            }
            // What the viewer types comes before anything is sent back.
            heard.push(frame(&mut from).await.expect("a frame").expect("typing"));
            for said in &frames {
                send(&mut to, said).await.expect("sent");
            }
            heard
        });
        (dir, path, serving)
    }

    #[tokio::test]
    async fn a_viewer_types_and_shows_until_the_host_lets_it_go() {
        let (_dir, path, serving) = host(
            json!({"v": 1, "id": "attach", "ok": true, "result": {"sessionId": "s-1"}}),
            vec![
                json!({"type": "output", "data": "hello ❄"}),
                json!({"type": "detached", "reason": "Conversation closed"}),
            ],
        );
        let (_tell, sizes) = mpsc::unbounded_channel();
        let mut shown = Vec::new();
        let left = join(&path, "s-1", SIZE, &b"\x1b[13u"[..], &mut shown, sizes)
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
                json!({"v": 1, "id": "attach", "op": "attach", "sessionId": "s-1", "rows": 40, "columns": 120}),
                json!({"type": "input", "data": "\x1b[13u"}),
            ],
            "the attach at its size, then the keys as typed"
        );
    }

    #[tokio::test]
    async fn a_session_the_host_refuses_says_why() {
        let (_dir, path, _serving) = host(
            json!({"v": 1, "id": "attach", "ok": false,
                   "error": {"code": "not_found", "message": "Conversation is not hosted: s-9"}}),
            Vec::new(),
        );
        let (_tell, sizes) = mpsc::unbounded_channel();
        let refused = join(&path, "s-9", SIZE, &b""[..], Vec::new(), sizes)
            .await
            .expect_err("refused");
        assert_eq!(refused.to_string(), "Conversation is not hosted: s-9");
    }
}
