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
//! A program that exits with [`holder::PARKED`] is parked, not ended: it
//! has let its memory go while it waits. Its holder keeps its terminal
//! and every window joined to it, and starts it again on the same
//! terminal when a key is typed in one of them or when its wake file,
//! `<id>.wake`, appears. The program is told that file in
//! `MYCELIUM_WAKE`, so whatever wakes it needs to know nothing of this
//! folder. One that says as it parks where its steers land, in
//! `<id>.steers` (told in `MYCELIUM_STEERS`), wakes too as one lands
//! there that would give it a turn ([`steering`]). One that exits with
//! [`holder::GONE`] having written `<id>.gone` (told in `MYCELIUM_GONE`),
//! its pi host away, is kept the same way, and also starts again by
//! itself after a pause, as its host may be back; any other exit ends it.
//! Whether its host was ever here since the holder started is kept in
//! `<id>.reached` (told in `MYCELIUM_REACHED`), which the program writes
//! once it finds its host, so the window can say which.
//!
//! - [`holder`]: the holder, keeping the program's terminal
//! - [`attach`]: a terminal joining a holder
//! - [`end`]: ending a held session for good, as its orchestrator or the
//!   person asks
//! - [`start`]: starting a holder in a process of its own
//! - [`modes`]: what the program asked of its terminal, for one joining
//! - [`title`]: the window title the program set last, for one joining
//! - [`environment`]: the person's environment as it is now, which each
//!   start of a held program is given
//! - [`journal`]: where a holder says how each run of its program ended
//! - [`steering`]: the steers a parked program wakes for
//! - [`turn`]: a resuming program's wait for its node to give it a turn
//!
//! What a joined terminal sends its holder is framed, as [`Said`]; what
//! the holder sends back is the modes the program asked for so far and
//! the title it set last, then the program's output as it came.

pub mod attach;
pub mod end;
pub mod environment;
pub mod holder;
pub mod journal;
pub mod modes;
pub mod start;
pub mod steering;
pub mod title;
pub mod turn;

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
    /// The program's process, or 0 while it is parked.
    pub pid: u32,
    /// Whether the program is parked, waiting to be started again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub parked: bool,
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

/// The file whose appearing wakes the session held as `id` in `dir`
/// while it is parked.
#[must_use]
pub fn wake(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.wake"))
}

/// The file a program held as `id` in `dir` may write, as it parks,
/// saying where its steers land and which wake it ([`steering::Steers`]).
#[must_use]
pub fn steers(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.steers"))
}

/// The file a program held as `id` in `dir` writes as it exits
/// [`holder::GONE`], saying its pi host is away, as attach does: a
/// program exiting so for a reason of its own, as one whose tools are
/// unavailable may, writes none and ends as any other. It holds the
/// writer's pid and how its host is gone ([`Gone::said`]), so the holder
/// heeds only the program it started, never a process under it.
#[must_use]
pub fn gone(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.gone"))
}

/// How a held program's pi host is gone, as it says in [`gone`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gone {
    /// The host was there since the holder started, and has gone, as it
    /// does as it stops or restarts.
    Away,
    /// No host by its name has been here since the holder started, as for
    /// a mistyped realm or a host taken away, or one not yet started.
    Unseen,
}

impl Gone {
    /// What [`say_gone`] writes for the process `pid`.
    #[must_use]
    pub fn said(self, pid: u32) -> String {
        let how = match self {
            Self::Away => "away",
            Self::Unseen => "unseen",
        };
        format!("{pid} {how}\n")
    }

    /// How the gone file `said` says the host is gone, if the process
    /// `pid` wrote it: one another process wrote says nothing. One
    /// written as nothing is an older attach's, which named no writer,
    /// as one still on the path after a rollback writes it, and says the
    /// host is away, as that attach meant.
    #[must_use]
    pub fn heard(said: &str, pid: u32) -> Option<Self> {
        let mut words = said.split_whitespace();
        let Some(writer) = words.next() else {
            return Some(Self::Away);
        };
        if writer.parse::<u32>().ok()? != pid {
            return None;
        }
        Some(match words.next() {
            Some("unseen") => Self::Unseen,
            _ => Self::Away,
        })
    }
}

/// Says to the holder of the program running this, if any, that its pi
/// host is gone, and how, as it is about to exit [`holder::GONE`].
pub fn say_gone(how: Gone) {
    if let Some(gone) = std::env::var_os(GONE_SAID) {
        // A holder not told ends the window, as it did before it kept one
        // for a pi host away: nothing worse than that follows.
        let _ = fs::write(gone, how.said(std::process::id()));
    }
}

/// What a held program is told the path of [`gone`] in.
pub const GONE_SAID: &str = "MYCELIUM_GONE";

/// The file a program held as `id` in `dir` writes just before it
/// quits to park, so its holder keeps it parked however that quit ends:
/// pi's own end may hang, as one did on a name lookup on 2026-11-21,
/// until a signal ends it, which says no [`holder::PARKED`].
#[must_use]
pub fn parked(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.parked"))
}

/// What a held program is told the path of [`parked`] in.
pub const PARKED_SAID: &str = "MYCELIUM_PARKED";

/// The file a program held as `id` in `dir` writes, naming its pi host,
/// once that host answers it ([`say_reached`]), kept until the holder
/// ends, so a later run that finds none knows its host was here and is
/// away, rather than never here at all.
#[must_use]
pub fn reached(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.reached"))
}

/// Says to the holder of the program running this, if any, that its pi
/// host `host` answered it.
pub fn say_reached(host: &str) {
    if let Some(reached) = std::env::var_os(REACHED_SAID) {
        // Unsaid, a later run finding no socket says its host was never
        // here, and the window still waits for it.
        let _ = fs::write(reached, host);
    }
}

/// Whether the pi host `host` answered the program running this in an
/// earlier run since its holder started ([`say_reached`]); one another
/// host answered says no, and one no holder runs is never told, so says
/// it was.
#[must_use]
pub fn reached_before(host: &str) -> bool {
    reached_in(
        std::env::var_os(REACHED_SAID).as_deref().map(Path::new),
        host,
    )
}

/// Whether the file at `reached`, told to the program running this, says
/// `host` answered it; with none told, no holder runs it, so it was.
fn reached_in(reached: Option<&Path>, host: &str) -> bool {
    reached.is_none_or(|reached| read_small(reached).is_some_and(|said| said == host))
}

/// The few bytes a program under a holder writes to tell it something,
/// read without following a link and only from a plain file, so that
/// nothing under the program can make the holder wait on a pipe, read
/// without end, or take a terminal as its own; none when there's no such
/// file.
#[must_use]
pub fn read_small(path: &Path) -> Option<String> {
    use rustix::fs::{Mode, OFlags};
    use std::io::Read as _;
    // A pid and a word, or a host's name, with room to spare.
    const SAID_MOST: u64 = 256;
    let flags =
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
    let file = fs::File::from(rustix::fs::open(path, flags, Mode::empty()).ok()?);
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut said = String::new();
    file.take(SAID_MOST).read_to_string(&mut said).ok()?;
    Some(said)
}

/// What a held program is told the path of [`reached`] in.
pub const REACHED_SAID: &str = "MYCELIUM_REACHED";

/// The file a program held as `id` in `dir` may write, as it parks, with
/// the command that takes it up again where it left off.
#[must_use]
pub fn again(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.again"))
}

/// The file anyone but the program may write with the command the
/// session held as `id` in `dir` starts with when it next starts again.
/// It wins over the one the program names as it parks, since a program
/// names itself: one that runs on an old conversation would otherwise
/// write that conversation over a newer one named for it while it ran.
#[must_use]
pub fn next(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.next"))
}

/// Names `command` as what the session held as `id` in `dir` starts with
/// when it next starts again, and wakes it if it is parked, answering
/// whether it woke it. The file is written whole, under another name
/// first, so its holder never reads half of it.
///
/// # Errors
///
/// When nothing is held as `id`, `command` names no program, or the file
/// can't be written.
pub fn name_next(dir: &Path, id: &str, command: &[String]) -> io::Result<bool> {
    if command.first().is_none_or(String::is_empty) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a command names a program",
        ));
    }
    let Some(one) = held(dir).into_iter().find(|one| one.id == id) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("nothing is held as {id}; `mycelium holds` lists what is"),
        ));
    };
    let writing = dir.join(format!(".{id}.next"));
    fs::write(
        &writing,
        serde_json::to_vec(command).map_err(io::Error::other)?,
    )?;
    fs::rename(&writing, next(dir, id))?;
    if one.parked {
        fs::write(wake(dir, id), b"{\"named\":true}\n")?;
    }
    Ok(one.parked)
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
    let mut watcher = crate::watching::watcher(move |event: notify::Result<notify::Event>| {
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
/// answering one would have the node read the records again without end:
/// the watch asks for none (`watching`), and this keeps out any a backend
/// says anyway.
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
    fn a_gone_file_is_heard_only_from_its_writer_and_says_how() {
        assert_eq!(Gone::heard(&Gone::Away.said(7), 7), Some(Gone::Away));
        assert_eq!(
            Gone::heard(&Gone::Unseen.said(7), 7),
            Some(Gone::Unseen),
            "a host never here is told apart from one away"
        );
        assert_eq!(
            Gone::heard(&Gone::Away.said(8), 7),
            None,
            "one another process wrote says nothing"
        );
        assert_eq!(
            Gone::heard("\n", 7),
            Some(Gone::Away),
            "one written as nothing is an older attach's, saying its host is away"
        );
        assert_eq!(
            Gone::heard("7 lost\n", 7),
            Some(Gone::Away),
            "and a word it doesn't know says away, as an attach of before did"
        );
    }

    #[test]
    fn what_a_held_program_says_is_read_only_from_a_plain_file_and_only_so_far() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let plain = root.path().join("plain");
        fs::write(&plain, "4100 away\n").expect("a file written");
        assert_eq!(read_small(&plain).as_deref(), Some("4100 away\n"));
        let long = root.path().join("long");
        fs::write(&long, "7".repeat(100_000)).expect("a file written");
        assert_eq!(
            read_small(&long).map(|said| said.len()),
            Some(256),
            "one that goes on is read only so far"
        );
        let linked = root.path().join("linked");
        std::os::unix::fs::symlink(&plain, &linked).expect("a link made");
        assert_eq!(read_small(&linked), None, "a link is never followed");
        let pipe = root.path().join("pipe");
        let made = std::process::Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "a pipe made");
        assert_eq!(read_small(&pipe), None, "nor is a pipe waited on");
        assert_eq!(read_small(&root.path().join("none")), None);
    }

    #[test]
    fn a_host_was_reached_only_as_the_file_names_it() {
        let root = tempfile::tempdir().expect("a temporary folder");
        let reached = root.path().join("window1.reached");
        assert!(
            !reached_in(Some(&reached), "harbour-background"),
            "none written says no host has answered"
        );
        fs::write(&reached, "harbour-background").expect("a file written");
        assert!(
            reached_in(Some(&reached), "harbour-background"),
            "the host it names answered"
        );
        assert!(
            !reached_in(Some(&reached), "harbour-attended"),
            "another host never did"
        );
        assert!(
            reached_in(None, "harbour-background"),
            "a program no holder runs is never told, so says it was"
        );
    }

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
                app_id: Some("pi-session.4".to_owned()),
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
