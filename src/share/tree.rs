//! What of a shared quest crosses between machines, and what each file
//! holds.
//!
//! A quest's record is what the harness and quest-backup call one: its
//! README, its journey, its documents in their kind folders, its
//! attachments and its harness log, and nothing else in its folder, since
//! the rest is each machine's own bookkeeping (the quest's lock, its stale
//! copies, files being written). Its scratch is its working material,
//! without `tmp/`, which is a session's `$TMPDIR`, without any git clone,
//! whose trees name it by absolute path, and without links, such as a
//! build's out-link into the Nix store. Anything hidden stays home in
//! either: a file being written, or a machine's own note.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The folders a quest's documents sit in, one per kind.
const DOCUMENT_FOLDERS: [&str; 4] = ["plans", "research", "briefs", "reports"];
/// The folder of what a quest's documents cite.
const ATTACHMENTS: &str = "attachments";
/// The record's files that sit beside its README.
const BESIDE: [&str; 3] = ["README.md", "JOURNEY.md", "harness.log"];

/// Which of a quest's two folders a file is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Place {
    /// The quest's record, in the quest store.
    Record,
    /// The quest's scratch folder.
    Scratch,
}

/// A shared file, by its folder and its path within it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Shared {
    /// The folder it is in.
    pub place: Place,
    /// Its path within that folder, with `/` between names.
    pub path: String,
}

/// What a file holds, named by its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Content {
    /// Its content's hash, as hex.
    pub hash: String,
    /// Its length in bytes.
    pub len: u64,
}

/// Where one machine keeps a quest.
#[derive(Debug, Clone)]
pub struct Quest {
    /// Its record's folder.
    pub record: PathBuf,
    /// Its scratch folder, where this machine carries it.
    pub scratch: Option<PathBuf>,
    /// The account's home folder on this machine, which paths in the
    /// record are written against ([`super::disk`]).
    pub home: PathBuf,
}

impl Quest {
    /// A quest kept in `record`, and `scratch` if that is carried too, by
    /// an account at `home`.
    #[must_use]
    pub fn new(record: PathBuf, scratch: Option<PathBuf>, home: PathBuf) -> Self {
        Self {
            record,
            scratch,
            home,
        }
    }

    /// Whether this machine carries a shared file of this quest: one that
    /// crosses, in a folder it carries.
    #[must_use]
    pub fn carries(&self, shared: &Shared) -> bool {
        crosses(shared) && (shared.place == Place::Record || self.scratch.is_some())
    }

    /// Where a shared file lives on this machine.
    pub fn path_of(&self, shared: &Shared) -> io::Result<PathBuf> {
        let root = match shared.place {
            Place::Record => &self.record,
            Place::Scratch => self.scratch.as_ref().ok_or_else(|| {
                io::Error::other(format!("this machine doesn't carry the scratch, so not {}", shared.path))
            })?,
        };
        Ok(root.join(&shared.path))
    }

    /// Calls `each` with every file of this quest this machine carries; a
    /// folder that isn't here yet holds none.
    pub fn each_file(&self, each: &mut impl FnMut(Shared) -> io::Result<()>) -> io::Result<()> {
        let scratch = self.scratch.iter().map(|root| (Place::Scratch, root));
        for (place, root) in std::iter::once((Place::Record, &self.record)).chain(scratch) {
            walk(root, root, &mut |path| {
                let shared = Shared { place, path };
                if crosses(&shared) {
                    each(shared)?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Every file of this quest that crosses, with what each holds.
    pub fn files(&self) -> io::Result<BTreeMap<Shared, Content>> {
        let mut files = BTreeMap::new();
        self.each_file(&mut |shared| {
            let content = self.content_of(&shared)?;
            files.insert(shared, content);
            Ok(())
        })?;
        Ok(files)
    }
}

/// Whether a file at this place crosses between machines.
#[must_use]
pub fn crosses(shared: &Shared) -> bool {
    let names: Vec<&str> = shared.path.split('/').collect();
    if names
        .iter()
        .any(|name| name.starts_with('.') || name.is_empty())
    {
        return false;
    }
    // Another machine's copy, kept beside this one's for a person to
    // settle, is this machine's alone; crossing back, it would be kept
    // beside itself again.
    if names.last().is_some_and(|name| name.contains(".from-")) {
        return false;
    }
    match shared.place {
        Place::Record => match names.as_slice() {
            [name] => BESIDE.contains(name),
            [folder, name] if DOCUMENT_FOLDERS.contains(folder) => is_document(name),
            [ATTACHMENTS, ..] => true,
            _ => false,
        },
        Place::Scratch => names.first() != Some(&"tmp"),
    }
}

/// Whether a name is a quest document's: a kind, a date and an id.
fn is_document(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    let parts: Vec<&str> = stem.split('-').collect();
    matches!(
        parts.as_slice(),
        [kind, date, id]
            if ["PLAN", "RSCH", "BRIF", "RPRT"].contains(kind)
                && date.len() == 8
                && date.bytes().all(|byte| byte.is_ascii_digit())
                && id.len() == 6
                && id.bytes().all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
    )
}

/// What `bytes` hold, named by their hash.
#[must_use]
pub fn content(bytes: &[u8]) -> Content {
    Content {
        hash: blake3::hash(bytes).to_hex().to_string(),
        len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    }
}

/// Each plain file under `folder`, by its path from `root`, leaving out
/// links, and any folder holding a git clone.
fn walk(
    root: &Path,
    folder: &Path,
    each: &mut impl FnMut(String) -> io::Result<()>,
) -> io::Result<()> {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if folder != root && folder.join(".git").exists() {
        return Ok(());
    }
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_dir() {
            walk(root, &path, each)?;
        } else if kind.is_file()
            && let Some(within) = relative(root, &path)
        {
            each(within)?;
        }
    }
    Ok(())
}

fn relative(root: &Path, path: &Path) -> Option<String> {
    let within = path.strip_prefix(root).ok()?;
    let names: Option<Vec<&str>> = within.iter().map(|name| name.to_str()).collect();
    Some(names?.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().expect("a file has a folder")).expect("its folder made");
        fs::write(path, text).expect("the file written");
    }

    fn paths(files: &BTreeMap<Shared, Content>, place: Place) -> Vec<&str> {
        files
            .keys()
            .filter(|shared| shared.place == place)
            .map(|shared| shared.path.as_str())
            .collect()
    }

    #[test]
    fn only_the_record_and_the_working_material_cross() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let scratch = dir.path().join("scratch");
        let quest = Quest::new(
            dir.path().join("record"),
            Some(scratch.clone()),
            dir.path().join("home"),
        );
        for path in [
            "README.md",
            "JOURNEY.md",
            "harness.log",
            "plans/PLAN-20261026-IZH11J.md",
            "attachments/map.md",
            "plans/notes.md",
            ".quest.lock",
            ".README.md.tmp-42-1",
            "stray.txt",
            "attachments/map.md.from-equilibrius",
        ] {
            write(&quest.record, path, path);
        }
        for path in [
            "notes.md",
            "runs/one.json",
            "tmp/big.bin",
            "clone/.git/HEAD",
            "clone/src/lib.rs",
            ".node.mycelium.tmp",
            "notes.md.from-equilibrius",
        ] {
            write(&scratch, path, path);
        }
        let files = quest.files().expect("the quest read");
        assert_eq!(
            paths(&files, Place::Record),
            [
                "JOURNEY.md",
                "README.md",
                "attachments/map.md",
                "harness.log",
                "plans/PLAN-20261026-IZH11J.md"
            ],
            "the record is its README, journey, log, documents and attachments"
        );
        assert_eq!(
            paths(&files, Place::Scratch),
            ["notes.md", "runs/one.json"],
            "the scratch crosses without tmp/, clones, hidden files or other machines' copies"
        );
    }

    #[test]
    fn a_file_is_named_by_what_it_holds() {
        let content = |text: &str| content(text.as_bytes());
        assert_eq!(
            content("same"),
            content("same"),
            "the same bytes, the same content"
        );
        assert_ne!(
            content("same").hash,
            content("other").hash,
            "other bytes, another hash"
        );
        assert_eq!(content("other").len, 5, "its length is its bytes'");
    }

    #[test]
    fn a_quest_not_here_yet_holds_nothing() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let quest = Quest::new(
            dir.path().join("none"),
            Some(dir.path().join("neither")),
            dir.path().join("home"),
        );
        assert!(
            quest.files().expect("nothing to read").is_empty(),
            "no folder, no files"
        );
    }
}
