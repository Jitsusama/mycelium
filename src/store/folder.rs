//! A folder of the mesh's, and the files in it, reached without following
//! a link: where the mesh is shared with another account, as a worker
//! machine's is, that account can put a link anywhere inside it, and a
//! node that followed one would read or write a file of its own account's
//! it was pointed at. So only the mesh's own root is reached by its path,
//! links and all, as whoever set the machine up made it; every folder and
//! file inside is opened from the folder holding it, never through a link,
//! and only a plain file is read or written. A file is written only by
//! staging it under a name of its own and renaming it into place, which
//! replaces a link of the same name rather than writing through it.

use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use rustix::fs::{AtFlags, FileType, Mode, OFlags};

/// The most a file of the mesh's is read to: a message or a presence is a
/// few kilobytes, so one past this was put there to be read, not written by
/// the mesh.
const LARGEST_FILE: u64 = 8 << 20;

/// Numbers each staged write of this process, so no two share a file.
static STAGED: AtomicU64 = AtomicU64::new(0);

/// What a name in a folder is, as the folder says without following it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A plain file.
    File,
    /// A folder.
    Folder,
    /// Anything else: a link, a pipe, a socket or a device.
    Other,
}

/// A folder of the mesh's, opened.
#[derive(Debug)]
pub struct Folder(OwnedFd);

impl Folder {
    /// The folder `inside` names under `root`: `root` reached by its path,
    /// as its own machine set it, and each folder below it opened from the
    /// one above, never through a link.
    pub fn open(root: &Path, inside: &[&str]) -> io::Result<Self> {
        let mut folder = Self(rustix::fs::open(
            root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        for name in inside {
            folder = folder.folder(name)?;
        }
        Ok(folder)
    }

    /// The folder `name` in this one, opened never through a link.
    pub fn folder(&self, name: &str) -> io::Result<Self> {
        Ok(Self(rustix::fs::openat(
            &self.0,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?))
    }

    /// Each name in this folder and what it is, without following any.
    /// A name that isn't text is left out, since the mesh names none so.
    pub fn names(&self) -> io::Result<Vec<(String, Kind)>> {
        let mut names = Vec::new();
        for entry in rustix::fs::Dir::read_from(&self.0)? {
            let entry = entry?;
            let Ok(name) = entry.file_name().to_str() else {
                continue;
            };
            if name == "." || name == ".." {
                continue;
            }
            let kind = match entry.file_type() {
                FileType::RegularFile => Kind::File,
                FileType::Directory => Kind::Folder,
                // Some file systems say nothing in the listing, so the
                // name itself is looked at, never followed.
                FileType::Unknown => self.kind(name),
                _ => Kind::Other,
            };
            names.push((name.to_owned(), kind));
        }
        Ok(names)
    }

    /// What `name` in this folder is, looked at without following it; a
    /// name that isn't there is [`Kind::Other`].
    pub fn kind(&self, name: &str) -> Kind {
        match rustix::fs::statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => match FileType::from_raw_mode(stat.st_mode) {
                FileType::RegularFile => Kind::File,
                FileType::Directory => Kind::Folder,
                _ => Kind::Other,
            },
            Err(_) => Kind::Other,
        }
    }

    /// Whether `name` is a plain file in this folder.
    pub fn holds(&self, name: &str) -> bool {
        self.kind(name) == Kind::File
    }

    /// The plain file `name` in this folder, opened to read, never through
    /// a link nor waiting on a pipe's writer; none where it isn't there.
    /// Anything else of that name is refused.
    pub fn file(&self, name: &str) -> io::Result<Option<fs::File>> {
        let file = match rustix::fs::openat(
            &self.0,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(file) => fs::File::from(file),
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        plain(&file, name)?;
        Ok(Some(file))
    }

    /// What the plain file `name` in this folder holds; none where it
    /// isn't there.
    pub fn read(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        self.file(name)?.map(|file| whole(file, name)).transpose()
    }

    /// Writes `bytes` as `name` in this folder, whole: staged under a name
    /// no other write shares, made new so nothing there is written
    /// through, then renamed into place over whatever had the name.
    pub fn write_whole(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let staged = format!(
            ".{name}.{}.{}.mycelium.tmp",
            std::process::id(),
            STAGED.fetch_add(1, Ordering::Relaxed)
        );
        let written = rustix::fs::openat(
            &self.0,
            staged.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o666),
        )
        .map_err(io::Error::from)
        .and_then(|file| fs::File::from(file).write_all(bytes))
        .and_then(|()| {
            rustix::fs::renameat(&self.0, staged.as_str(), &self.0, name).map_err(io::Error::from)
        });
        if written.is_err() {
            // A staged file left behind is only clutter, which the mesh's
            // own sweep clears; the write's own error is the one worth
            // telling.
            let _ = rustix::fs::unlinkat(&self.0, staged.as_str(), AtFlags::empty());
        }
        written
    }

    /// Sets when the plain file `name` in this folder was last written to
    /// `at`, as the mesh reads when a session was last vouched for.
    pub fn touch(&self, name: &str, at: SystemTime) -> io::Result<()> {
        let file = fs::File::from(rustix::fs::openat(
            &self.0,
            name,
            OFlags::WRONLY | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        plain(&file, name)?;
        file.set_modified(at)
    }

    /// Takes `name` out of this folder, whatever it is but a folder; one
    /// already gone is no error.
    pub fn remove(&self, name: &str) -> io::Result<()> {
        match rustix::fs::unlinkat(&self.0, name, AtFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

/// `file`, opened as `name`, if it is a plain file of that one name: a
/// second name is another account's way to a file of this one's, as a
/// link is, and the mesh never makes one.
fn plain(file: &fs::File, name: &str) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let found = file.metadata()?;
    if !found.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{name:?} isn't a plain file"),
        ));
    }
    if found.nlink() > 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{name:?} is a file of more than one name"),
        ));
    }
    Ok(())
}

/// All `file`, opened as `name`, holds, so long as that is no more than
/// [`LARGEST_FILE`].
fn whole(file: fs::File, name: &str) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(LARGEST_FILE + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > LARGEST_FILE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{name:?} holds more than any file of the mesh's"),
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn a_link_inside_is_never_followed_to_read_or_write_what_it_names() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let root = dir.path().join("mesh");
        fs::create_dir_all(root.join("messages")).expect("the mesh's folder");
        let secret = dir.path().join("secret.json");
        fs::write(&secret, b"{\"key\": \"ours\"}").expect("a file of the account's own");
        symlink(&secret, root.join("messages/planted.json")).expect("a link planted");
        let messages = Folder::open(&root, &["messages"]).expect("the folder opens");

        assert_eq!(
            messages.names().expect("the folder lists"),
            vec![("planted.json".to_owned(), Kind::Other)],
            "a link is listed as what it is, not what it names"
        );
        assert!(
            messages.read("planted.json").is_err(),
            "nor is it read through"
        );
        assert!(
            messages
                .touch("planted.json", SystemTime::UNIX_EPOCH)
                .is_err(),
            "nor is what it names touched"
        );
        messages
            .write_whole("planted.json", b"{}")
            .expect("a write takes the name's place");
        assert_eq!(
            fs::read(&secret).expect("the account's file stands"),
            b"{\"key\": \"ours\"}",
            "and writes nothing through it"
        );
        assert_eq!(
            messages.read("planted.json").expect("now a plain file"),
            Some(b"{}".to_vec())
        );
    }

    #[test]
    fn a_second_name_for_a_file_is_neither_read_nor_touched() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let root = dir.path().join("mesh");
        fs::create_dir_all(&root).expect("the mesh's folder");
        let secret = dir.path().join("secret.json");
        fs::write(&secret, b"{}").expect("a file of the account's own");
        let touched = fs::metadata(&secret).and_then(|meta| meta.modified()).ok();
        fs::hard_link(&secret, root.join("planted.json")).expect("a second name planted");
        let folder = Folder::open(&root, &[]).expect("the folder opens");

        assert!(folder.read("planted.json").is_err(), "it is never read");
        assert!(
            folder
                .touch("planted.json", SystemTime::UNIX_EPOCH)
                .is_err()
        );
        assert_eq!(
            fs::metadata(&secret).and_then(|meta| meta.modified()).ok(),
            touched,
            "nor is the file's time moved"
        );
        folder
            .write_whole("planted.json", b"[]")
            .expect("a write takes the name's place");
        assert_eq!(fs::read(&secret).expect("the file stands"), b"{}");
    }

    #[test]
    fn a_link_in_place_of_a_folder_inside_is_never_opened() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let root = dir.path().join("mesh");
        fs::create_dir_all(&root).expect("the mesh's folder");
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).expect("a folder of the account's own");
        symlink(&elsewhere, root.join("presence")).expect("a link planted");

        assert!(
            Folder::open(&root, &["presence"]).is_err(),
            "a folder reached through a link inside is refused"
        );
    }

    #[test]
    fn the_root_itself_is_reached_through_the_link_its_machine_made() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let shared = dir.path().join("shared");
        fs::create_dir_all(shared.join("presence")).expect("the shared folder");
        let root = dir.path().join("mesh-workflow");
        symlink(&shared, &root).expect("the root's link");

        assert!(
            Folder::open(&root, &["presence"]).is_ok(),
            "as a worker machine's mesh is reached"
        );
    }

    // Only Linux's rustix makes a pipe at a path; the refusal is the same.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_pipe_named_like_a_file_is_refused_rather_than_waited_on() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &dir.path().join("planted.json"),
            FileType::Fifo,
            Mode::from_raw_mode(0o600),
            0,
        )
        .expect("a pipe planted");
        let folder = Folder::open(dir.path(), &[]).expect("the folder opens");

        assert!(folder.read("planted.json").is_err());
        assert_eq!(folder.read("absent.json").expect("nothing there"), None);
    }
}
