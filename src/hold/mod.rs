//! Held sessions: a program run on a terminal that a holder of its own
//! keeps, so the window it started in can close and any terminal on this
//! machine can take it up again, as dtach does.
//!
//! Each holder is a process of its own, outside the node, since a service
//! manager restarting the node stops everything the node started. It
//! serves one socket, `<id>.sock`, in the realm's folder of held sessions,
//! beside a record, `<id>.json`, saying what it holds; both go when the
//! program ends.
//!
//! - [`holder`]: the holder, keeping the program's terminal
//! - [`attach`]: a terminal joining a holder
//!
//! What a joined terminal sends its holder is framed, as [`Said`]; what
//! the holder sends back is the program's output as it came.

pub mod attach;
pub mod holder;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};

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
}

const TYPED: u8 = 0;
const SIZE: u8 = 1;

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

fn runs(pid: u32) -> bool {
    i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let typed: Vec<u8> = heard[2..]
            .iter()
            .flat_map(|one| match one {
                Said::Typed(bytes) => bytes.clone(),
                Said::Size(_) => Vec::new(),
            })
            .collect();
        assert_eq!(
            typed, long,
            "bytes past one frame's length cross in several"
        );
    }
}
