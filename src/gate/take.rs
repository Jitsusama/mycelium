//! Writing a gate's file a worker sent into its quest's scratch: only as
//! a new file, and only through folders that are what they seem.
//!
//! Every folder on the way below the quests' scratch is opened without
//! following a link, and made where it is missing, so nothing planted
//! there turns the write elsewhere. The file is written whole under a
//! hidden name first, which never crosses to another machine, then put
//! in place by a link that fails where any file already stands, and the
//! hidden name taken away, so nobody reads a file half written, and a
//! refusal leaves nothing behind.

use std::fs;
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::io::Errno;

use super::{Refused, sent};
use crate::share::ledger::is_quest;

/// The folders between a quest's scratch and its gates' folders.
const ON_THE_WAY: [&str; 2] = ["handover", "gates"];

/// Writes `body`, the gate's file `key` of quest `id`, into its folder in
/// `scratch`, the quests' scratch folders, answering where it now is.
///
/// # Errors
///
/// When `key` is no file a worker may send, `body` is larger than it may
/// be, a link or a file stands where a folder goes, or a file stands
/// where it goes, or it can't be written. The quests' scratch is this
/// node's own, so it is made and followed where it is missing or a link;
/// only the folders below it are held to what they seem.
pub fn take(scratch: &Path, id: &str, key: &str, body: &[u8]) -> Result<PathBuf, Refused> {
    if !is_quest(id) {
        return Err(Refused::NotGiven);
    }
    let file = sent(key)?;
    if u64::try_from(body.len()).map_or(true, |len| len > file.largest) {
        return Err(Refused::Large);
    }
    fs::create_dir_all(scratch).map_err(|_| Refused::Write)?;
    let mut folder = rustix::fs::open(
        scratch,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| Refused::Write)?;
    // A quest given to a worker may have had no scratch here yet.
    for name in [id]
        .into_iter()
        .chain(ON_THE_WAY)
        .chain([file.commit.as_str()])
    {
        folder = into(&folder, name)?;
    }
    let writing = format!(".{}.taking-{}", file.name, std::process::id());
    // One an earlier node of the same number left is this node's own.
    match rustix::fs::unlinkat(&folder, writing.as_str(), AtFlags::empty()) {
        Ok(()) | Err(Errno::NOENT) => {}
        Err(_) => return Err(Refused::Write),
    }
    let made = rustix::fs::openat(
        &folder,
        writing.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|_| Refused::Write)?;
    let mut made = fs::File::from(made);
    let placed = made
        .write_all(body)
        .and_then(|()| made.sync_all())
        .map_err(|_| Refused::Write)
        .and_then(|()| {
            rustix::fs::linkat(
                &folder,
                writing.as_str(),
                &folder,
                file.name,
                AtFlags::empty(),
            )
            .map_err(|error| {
                if error == Errno::EXIST {
                    Refused::Exists
                } else {
                    Refused::Write
                }
            })
        });
    // Gone whether it was placed or not: placed, the link holds it.
    let _ = rustix::fs::unlinkat(&folder, writing.as_str(), AtFlags::empty());
    placed?;
    Ok(scratch.join(id).join(file.within()))
}

/// The folder `name` in `folder`, made where it's missing, opened without
/// following a link.
fn into(folder: &impl AsFd, name: &str) -> Result<OwnedFd, Refused> {
    match rustix::fs::mkdirat(folder, name, Mode::from_raw_mode(0o700)) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(_) => return Err(Refused::Write),
    }
    rustix::fs::openat(folder, name, folder_flags(), Mode::empty()).map_err(|_| Refused::Link)
}

/// How a folder on the way is opened: as a folder, never through a link.
fn folder_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
