//! Held sessions: a program run on a terminal that a holder of its own
//! keeps, so the window it started in can close and any terminal on this
//! machine can take it up again, as dtach does.
//!
//! Each holder is a process of its own, outside the node, since a service
//! manager restarting the node stops everything the node started; a node
//! that starts one as a systemd unit puts it in a scope of its own. It
//! serves one socket, `<id>.sock`, in the realm's folder of held sessions,
//! beside a record, `<id>.json`, saying what it holds; both go when the
//! program ends.
//!
//! - [`holder`]: the holder, keeping the program's terminal
//! - [`attach`]: a terminal joining a holder
//! - [`start`]: starting a holder in a process of its own
//!
//! What a joined terminal sends its holder is framed, as [`Said`]; what
//! the holder sends back is the program's output as it came.

pub mod attach;
pub mod holder;
pub mod start;

use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use notify::Watcher;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;

use crate::record::Window;

/// What a holder keeps beside its socket, saying what it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    /// Its id, which names its socket.
    pub id: String,
    /// The program and its arguments.
    pub command: Vec<String>,
    /// The folder the program started in.
    pub folder: PathBuf,
    /// The holder's process.
    pub holder: u32,
    /// The program's process.
    pub pid: u32,
    /// When it was started, as an RFC 3339 time.
    pub since: String,
    /// The windows joined to it that said which they are, the latest
    /// last; each goes the moment its terminal leaves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<Window>,
}

/// A terminal's size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    /// Its rows.
    pub rows: u16,
    /// Its columns.
    pub cols: u16,
}

/// What a joined terminal says to its holder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Said {
    /// Bytes typed, for the program.
    Typed(Vec<u8>),
    /// The terminal's size, first on joining and again on each change.
    Size(Size),
    /// The window the terminal is, once on joining, if it can say.
    Window(Window),
}

const TYPED: u8 = 0;
const SIZE: u8 = 1;
const WINDOW: u8 = 2;

impl Said {
    /// The frame this is sent as: a tag, a length of two bytes, and what
    /// it carries. Bytes typed past a frame's length go in several.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut frames = Vec::new();
        match self {
            Self::Typed(bytes) => {
                for piece in bytes.chunks(usize::from(u16::MAX)) {
                    frames.push(TYPED);
                    frames.extend_from_slice(&length(piece).to_be_bytes());
                    frames.extend_from_slice(piece);
                }
            }
            Self::Size(size) => {
                frames.push(SIZE);
                frames.extend_from_slice(&4u16.to_be_bytes());
                frames.extend_from_slice(&size.rows.to_be_bytes());
                frames.extend_from_slice(&size.cols.to_be_bytes());
            }
            Self::Window(window) => {
                // A window is a few short names, far under a frame's length.
                let body = serde_json::to_vec(window).unwrap_or_default();
                frames.push(WINDOW);
                frames.extend_from_slice(&length(&body).to_be_bytes());
                frames.extend_from_slice(&body);
            }
        }
        frames
    }

    /// The next frame from `reading`, or nothing once it has closed.
    pub async fn read(reading: &mut (impl AsyncRead + Unpin)) -> io::Result<Option<Self>> {
        let mut head = [0u8; 3];
        match reading.read_exact(&mut head).await {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error),
        }
        let mut body = vec![0u8; usize::from(u16::from_be_bytes([head[1], head[2]]))];
        reading.read_exact(&mut body).await?;
        match (head[0], body.as_slice()) {
            (TYPED, _) => Ok(Some(Self::Typed(body))),
            (SIZE, [r0, r1, c0, c1]) => Ok(Some(Self::Size(Size {
                rows: u16::from_be_bytes([*r0, *r1]),
                cols: u16::from_be_bytes([*c0, *c1]),
            }))),
            (WINDOW, _) => serde_json::from_slice(&body)
                .map(|window| Some(Self::Window(window)))
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            (tag, _) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("a terminal sent a frame this holder doesn't know, tagged {tag}"),
            )),
        }
    }
}

fn length(piece: &[u8]) -> u16 {
    u16::try_from(piece.len()).unwrap_or(u16::MAX)
}

/// A new holder's id: short, and unlike any other held on this machine.
#[must_use]
pub fn new_id() -> String {
    let seed = format!(
        "{}-{}",
        jiff::Timestamp::now().as_nanosecond(),
        std::process::id()
    );
    blake3::hash(seed.as_bytes()).to_hex()[..8].to_owned()
}

/// The socket of the session held as `id` in `dir`.
#[must_use]
pub fn socket(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.sock"))
}

/// Every session held in `dir` whose holder still runs, oldest first.
#[must_use]
pub fn held(dir: &Path) -> Vec<Held> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut all: Vec<Held> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|entry| serde_json::from_slice::<Held>(&fs::read(entry.path()).ok()?).ok())
        .filter(|one| socket(dir, &one.id).exists() && runs(one.holder))
        .collect();
    all.sort_by(|a, b| a.since.cmp(&b.since));
    all
}

/// Watches `dir`, made if it isn't there, telling `changed` each time what
/// is held there changes: a session held or let go, or a window joined or
/// left. The watch lasts as long as what this answers is kept.
pub fn watch(dir: &Path) -> io::Result<(notify::RecommendedWatcher, mpsc::UnboundedReceiver<()>)> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let (tell, told) = mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if changes_a_record(&event) {
            // The node has stopped hearing once nothing receives this.
            let _ = tell.send(());
        }
    })
    .map_err(io::Error::other)?;
    watcher
        .watch(dir, notify::RecursiveMode::NonRecursive)
        .map_err(io::Error::other)?;
    Ok((watcher, told))
}

/// Whether `event` may change what is held: a record written, renamed or
/// removed, or events missed. Opening or reading one changes nothing, and
/// a Linux watch hears those too, the node's own reads of the records
/// among them, so answering them would have the node read the records
/// again without end.
fn changes_a_record(event: &notify::Result<notify::Event>) -> bool {
    let Ok(event) = event else {
        return true;
    };
    if event.need_rescan() {
        return true;
    }
    let only_looked = matches!(
        event.kind,
        notify::EventKind::Access(_)
            | notify::EventKind::Modify(notify::event::ModifyKind::Metadata(_))
    );
    !only_looked
        && event.paths.iter().any(|path| {
            path.extension().is_some_and(|ext| ext == "json")
                && !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with('.'))
        })
}

fn runs(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{AccessKind, AccessMode, CreateKind, EventKind, ModifyKind};

    #[test]
    fn only_a_records_change_is_told_never_its_reading() {
        let at = |kind: EventKind, name: &str| {
            Ok(notify::Event::new(kind).add_path(PathBuf::from("/run/work.held").join(name)))
        };
        let cases = [
            (EventKind::Create(CreateKind::File), "ab12.json", true),
            (EventKind::Modify(ModifyKind::Any), "ab12.json", true),
            (
                EventKind::Access(AccessKind::Open(AccessMode::Read)),
                "ab12.json",
                false,
            ),
            (
                EventKind::Access(AccessKind::Close(AccessMode::Read)),
                "ab12.json",
                false,
            ),
            (EventKind::Access(AccessKind::Read), "ab12.json", false),
            (EventKind::Create(CreateKind::File), ".ab12.json", false),
            (EventKind::Create(CreateKind::File), "ab12.sock", false),
        ];
        for (kind, name, told) in cases {
            assert_eq!(
                changes_a_record(&at(kind, name)),
                told,
                "{kind:?} of {name}"
            );
        }
        assert!(
            changes_a_record(&Err(notify::Error::generic("the queue overflowed"))),
            "a watch that failed may have missed a change"
        );
    }

    #[tokio::test]
    async fn what_a_terminal_says_crosses_whole() {
        let long = vec![b'x'; usize::from(u16::MAX) + 10];
        let said = [
            Said::Size(Size {
                rows: 30,
                cols: 100,
            }),
            Said::Typed(b"echo hi\r".to_vec()),
            Said::Typed(long.clone()),
            Said::Window(Window {
                terminal: "kitty".to_owned(),
                id: "13".to_owned(),
                within: Some("unix:@kitty-6753".to_owned()),
            }),
        ];
        let sent: Vec<u8> = said.iter().flat_map(Said::encode).collect();
        let mut reading = sent.as_slice();
        let mut heard = Vec::new();
        while let Some(one) = Said::read(&mut reading).await.expect("frames read") {
            heard.push(one);
        }
        assert_eq!(
            heard[..2],
            said[..2],
            "a size and a few bytes cross as sent"
        );
        assert_eq!(heard.last(), said.last(), "a window crosses as it was said");
        let typed: Vec<u8> = heard[2..heard.len() - 1]
            .iter()
            .flat_map(|one| match one {
                Said::Typed(bytes) => bytes.clone(),
                Said::Size(_) | Said::Window(_) => Vec::new(),
            })
            .collect();
        assert_eq!(
            typed, long,
            "bytes past one frame's length cross in several"
        );
    }
}
