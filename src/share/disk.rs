//! A shared quest's files on this machine's disk, read as they cross and
//! written as this machine reads them.
//!
//! The record names places on the machine that wrote it: a quest's working
//! folder, its trees, its scratch. Those sit under the account's home
//! folder, which differs between machines (`/home/jitsusama` on one,
//! `/Users/joel.gerber` on another), so the record's text crosses with its
//! home folder as a mark that each machine writes back as its own. A file's
//! hash is taken of what crosses, so both ends name the same content alike.
//!
//! The harness changes a record's file only under the quest's lock, by
//! writing a hidden file and renaming it over the old, so a record's file
//! is written here the same way, under the same lock: a session's change
//! and this machine's are never interleaved, and a reader never sees half
//! of either.

use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use super::tree::{Content, Place, Quest, Shared, content};

/// What stands for the account's home folder in a record's text as it
/// crosses: a mark no person writes.
const HOME: &[u8] = b"\x01home\x01";

/// The harness's lock on a quest, in the quest's folder.
const LOCK: &str = ".quest.lock";

/// How long to wait for a session holding a quest's lock.
const LOCK_WITHIN: Duration = Duration::from_secs(5);

/// How long between tries for a held lock.
const LOCK_RETRY: Duration = Duration::from_millis(25);

/// How old a lock is before it is taken as left by a holder that died,
/// as the harness takes it.
const LOCK_STALE: Duration = Duration::from_secs(30);

impl Quest {
    /// What a shared file holds as it crosses, or `None` if it isn't here.
    pub fn read(&self, shared: &Shared) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.path_of(shared)?) {
            Ok(bytes) if translates(shared) => Ok(Some(portable(&bytes, &self.home))),
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// What a shared file holds, named as it crosses.
    pub fn content_of(&self, shared: &Shared) -> io::Result<Content> {
        let bytes = self.read(shared)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("{} is gone", shared.path))
        })?;
        Ok(content(&bytes))
    }

    /// Writes what crossed into a shared file, or deletes it for `None`.
    pub fn write(&self, shared: &Shared, body: Option<&[u8]>) -> io::Result<()> {
        let path = self.path_of(shared)?;
        let _lock = match shared.place {
            Place::Record => {
                fs::create_dir_all(&self.record)?;
                Some(QuestLock::take(&self.record)?)
            }
            Place::Scratch => None,
        };
        match body {
            Some(body) if translates(shared) => staged_write(&path, &local(body, &self.home)),
            Some(body) => staged_write(&path, body),
            None => match fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            },
        }
    }
}

/// Whether a file's text names places, and so crosses with its home
/// folder marked: the record's own text, not what it attaches.
fn translates(shared: &Shared) -> bool {
    shared.place == Place::Record
        && !shared.path.starts_with("attachments/")
        && matches!(
            Path::new(&shared.path)
                .extension()
                .and_then(|kind| kind.to_str()),
            Some("md" | "log")
        )
}

/// `bytes` with each place under `home` written against the mark instead.
#[must_use]
pub fn portable(bytes: &[u8], home: &Path) -> Vec<u8> {
    let home = home.as_os_str().as_bytes();
    if home.is_empty() {
        return bytes.to_vec();
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some(at) = find(rest, home) {
        let after = rest.get(at + home.len()).copied();
        out.extend_from_slice(&rest[..at]);
        // Only the whole folder: `/home/jit` is no place under `/home/j`.
        if after.is_none_or(|byte| !is_name_byte(byte)) {
            out.extend_from_slice(HOME);
        } else {
            out.extend_from_slice(home);
        }
        rest = &rest[at + home.len()..];
    }
    out.extend_from_slice(rest);
    out
}

/// `bytes` as they crossed, with the mark written back as `home`.
#[must_use]
pub fn local(bytes: &[u8], home: &Path) -> Vec<u8> {
    let home = home.as_os_str().as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some(at) = find(rest, HOME) {
        out.extend_from_slice(&rest[..at]);
        out.extend_from_slice(home);
        rest = &rest[at + HOME.len()..];
    }
    out.extend_from_slice(rest);
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
}

/// Writes `bytes` to `path` by a hidden file renamed over it, so nobody
/// reads half of it.
fn staged_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let folder = path
        .parent()
        .ok_or_else(|| io::Error::other(format!("{} is in no folder", path.display())))?;
    fs::create_dir_all(folder)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other(format!("{} names no file", path.display())))?;
    let staged = folder.join(format!(
        ".{}.{}.mycelium.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    fs::write(&staged, bytes)?;
    fs::rename(&staged, path).inspect_err(|_| {
        // Nothing reads a staged file, so one left behind is only clutter.
        let _ = fs::remove_file(&staged);
    })
}

/// The harness's lock on a quest, held until dropped.
struct QuestLock(PathBuf);

impl QuestLock {
    /// Takes the lock in `folder` as the harness takes it, waiting for a
    /// session that holds it and taking one left by a holder that died.
    fn take(folder: &Path) -> io::Result<Self> {
        let path = folder.join(LOCK);
        let until = Instant::now() + LOCK_WITHIN;
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    let holder = serde_json::json!({
                        "pid": std::process::id(),
                        "startedAt": jiff::Timestamp::now().as_millisecond(),
                    });
                    serde_json::to_writer(file, &holder).map_err(io::Error::other)?;
                    return Ok(Self(path));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            if stale(&path) {
                // Another may take it first, which the next try finds.
                let _ = fs::remove_file(&path);
                continue;
            }
            if Instant::now() >= until {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{} was held past {LOCK_WITHIN:?}", path.display()),
                ));
            }
            std::thread::sleep(LOCK_RETRY);
        }
    }
}

impl Drop for QuestLock {
    fn drop(&mut self) {
        // A lock already gone was taken as stale; nothing is left to free.
        let _ = fs::remove_file(&self.0);
    }
}

fn stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > LOCK_STALE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(path: &str) -> Shared {
        Shared {
            place: Place::Record,
            path: path.to_owned(),
        }
    }

    #[test]
    fn places_cross_as_the_reader_names_them() {
        let optimus = Path::new("/home/jitsusama");
        let equilibrius = Path::new("/Users/joel.gerber");
        let text =
            b"cwd: /home/jitsusama/src/walgit-rs\nnot /home/jitsusamaX, but /home/jitsusama\n";
        let crossed = portable(text, optimus);
        assert_eq!(
            local(&crossed, equilibrius),
            b"cwd: /Users/joel.gerber/src/walgit-rs\nnot /home/jitsusamaX, but /Users/joel.gerber\n",
            "each place under the home folder is written for the reader's"
        );
        assert_eq!(local(&crossed, optimus), text, "and back again unchanged");
    }

    #[test]
    fn a_record_is_written_under_the_quests_lock() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let quest = Quest::new(
            dir.path().join("record"),
            None,
            PathBuf::from("/home/jitsusama"),
        );
        let readme = record("README.md");
        fs::create_dir_all(&quest.record).expect("the record made");
        fs::write(quest.record.join(LOCK), "{}").expect("a session holds the lock");
        let held = std::thread::spawn({
            let lock = quest.record.join(LOCK);
            move || {
                std::thread::sleep(LOCK_RETRY * 4);
                fs::remove_file(lock).expect("the session lets go");
            }
        });
        quest
            .write(&readme, Some(b"cwd: \x01home\x01/src\n"))
            .expect("written once the session lets go");
        held.join().expect("the session ended");
        assert_eq!(
            fs::read_to_string(quest.record.join("README.md")).expect("read"),
            "cwd: /home/jitsusama/src\n",
            "the record is written as this machine names its places"
        );
        assert!(!quest.record.join(LOCK).exists(), "the lock is let go");
        assert_eq!(
            quest.read(&readme).expect("read").as_deref(),
            Some(&b"cwd: \x01home\x01/src\n"[..]),
            "and read as it crosses"
        );
        quest.write(&readme, None).expect("deleted");
        assert_eq!(
            quest.read(&readme).expect("read"),
            None,
            "a deletion removes it"
        );
    }
}
