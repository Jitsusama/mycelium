//! What of a shared quest crosses between machines, and what each file
//! holds.
//!
//! A quest's record is what the harness and quest-backup call one: its
//! README, its journey, its documents in their kind folders, its
//! attachments and its harness log, and nothing else in its folder, since
//! the rest is each machine's own bookkeeping (the quest's lock, its stale
//! copies, files being written). Of its scratch, only the folders named
//! for sharing cross, since a scratch can hold gigabytes of clones and
//! data, and never `tmp/`, which is a session's `$TMPDIR`. Within a named
//! folder, no git clone crosses, since its trees name it by absolute path
//! and two machines writing one break it, no build output, and no links,
//! such as a build's out-link into the Nix store. Anything hidden stays
//! home in either: a file being written, or a machine's own note.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The folders a quest's documents sit in, one per kind.
const DOCUMENT_FOLDERS: [&str; 4] = ["plans", "research", "briefs", "reports"];
/// The folder of what a quest's documents cite.
const ATTACHMENTS: &str = "attachments";
/// The record's files that sit beside its README.
const BESIDE: [&str; 3] = ["README.md", "JOURNEY.md", "harness.log"];
/// The scratch's folder that is a session's `$TMPDIR`.
const TMP: &str = "tmp";
/// Folders a build fills, which each machine builds for itself.
const BUILT: [&str; 2] = ["target", "node_modules"];

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
    /// The folders in its scratch that are shared, by name.
    pub folders: BTreeSet<String>,
    /// The account's home folder on this machine, which paths in the
    /// record are written against ([`super::disk`]).
    pub home: PathBuf,
}

impl Quest {
    /// A quest kept in `record` by an account at `home`, sharing nothing
    /// of its scratch.
    #[must_use]
    pub fn new(record: PathBuf, home: PathBuf) -> Self {
        Self {
            record,
            scratch: None,
            folders: BTreeSet::new(),
            home,
        }
    }

    /// The same quest, with its scratch in `scratch`, sharing `folders`
    /// of it.
    #[must_use]
    pub fn sharing(self, scratch: PathBuf, folders: BTreeSet<String>) -> Self {
        Self {
            scratch: Some(scratch),
            folders,
            ..self
        }
    }

    /// The same quest, walking only `folders` of the scratch folders it
    /// shares, as a look does for those a watch saw change.
    #[must_use]
    pub fn walking(self, folders: &BTreeSet<String>) -> Self {
        Self {
            folders: self.folders.intersection(folders).cloned().collect(),
            ..self
        }
    }

    /// Whether this machine carries a shared file of this quest: one that
    /// crosses, in its record or in a scratch folder it shares.
    #[must_use]
    pub fn carries(&self, shared: &Shared) -> bool {
        crosses(shared)
            && match shared.place {
                Place::Record => true,
                Place::Scratch => {
                    self.scratch.is_some()
                        && shared
                            .path
                            .split('/')
                            .next()
                            .is_some_and(|folder| self.folders.contains(folder))
                }
            }
    }

    /// Where a shared file lives on this machine.
    pub fn path_of(&self, shared: &Shared) -> io::Result<PathBuf> {
        let root = match shared.place {
            Place::Record => &self.record,
            Place::Scratch => self.scratch.as_ref().ok_or_else(|| {
                io::Error::other(format!(
                    "this machine doesn't carry the scratch, so not {}",
                    shared.path
                ))
            })?,
        };
        Ok(root.join(&shared.path))
    }

    /// Calls `each` with every file of this quest this machine carries; a
    /// folder that isn't here yet holds none.
    pub fn each_file(&self, each: &mut impl FnMut(Shared) -> io::Result<()>) -> io::Result<()> {
        let folders = self.scratch.iter().flat_map(|root| {
            self.folders
                .iter()
                .map(move |folder| (Place::Scratch, root, root.join(folder)))
        });
        let record = (Place::Record, &self.record, self.record.clone());
        for (place, root, folder) in std::iter::once(record).chain(folders) {
            walk(root, &folder, &mut |path| {
                let shared = Shared { place, path };
                if self.carries(&shared) {
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
        Place::Scratch => {
            names.len() > 1
                && names.first().is_some_and(|folder| may_name(folder))
                && !names.iter().any(|name| BUILT.contains(name))
        }
    }
}

/// Whether `folder` may be named for sharing in a quest's scratch: one
/// folder, neither hidden, nor `tmp/`, nor build output, nor another
/// machine's copy.
#[must_use]
pub fn may_name(folder: &str) -> bool {
    !folder.is_empty()
        && !folder.contains('/')
        && !folder.starts_with('.')
        && !folder.contains(".from-")
        && folder != TMP
        && !BUILT.contains(&folder)
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
/// links, hidden folders, build output, and any folder holding a git
/// clone, so none of those is ever looked through.
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
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if kind.is_dir() && !name.starts_with('.') && !BUILT.contains(&name.as_ref()) {
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
    fn only_the_record_and_the_scratch_folders_named_cross() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let scratch = dir.path().join("scratch");
        let quest = Quest::new(dir.path().join("record"), dir.path().join("home")).sharing(
            scratch.clone(),
            BTreeSet::from(["notes".to_owned(), "clone".to_owned()]),
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
            "notes/plan.md",
            "notes/runs/one.json",
            "notes/runs/target/debug/big",
            "notes/web/node_modules/left-pad/index.js",
            "notes/.direnv/env",
            "notes/ttc/.git/HEAD",
            "notes/ttc/src/lib.rs",
            "notes/.plan.md.42.mycelium.tmp",
            "notes/plan.md.from-equilibrius",
            "clone/.git/HEAD",
            "clone/src/lib.rs",
            "hub-home/big.bin",
            "tmp/big.bin",
            "loose.md",
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
            ["notes/plan.md", "notes/runs/one.json"],
            "only the named folders cross, without clones, build output, hidden files or \
             other machines' copies"
        );
    }

    #[test]
    fn only_a_folder_of_the_scratch_may_be_named() {
        for folder in ["notes", "hub-home", "bench"] {
            assert!(may_name(folder), "{folder} may be named");
        }
        for folder in [
            "",
            "tmp",
            "target",
            "node_modules",
            ".direnv",
            "notes/runs",
            "notes.from-equilibrius",
        ] {
            assert!(!may_name(folder), "{folder:?} may not be named");
        }
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
        let quest = Quest::new(dir.path().join("none"), dir.path().join("home")).sharing(
            dir.path().join("neither"),
            BTreeSet::from(["notes".to_owned()]),
        );
        assert!(
            quest.files().expect("nothing to read").is_empty(),
            "no folder, no files"
        );
    }
}
