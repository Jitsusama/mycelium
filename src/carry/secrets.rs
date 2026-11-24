//! What keeps a secret from riding a carry to a worker: before a quest's
//! handover is given to one, every file in it is looked at by its name,
//! and the carry is refused on the first that names a secret. A worker is
//! lent, not the person's own, so what it is given must hold no key,
//! token or login of theirs. Only names are judged: a secret inside a git
//! bundle or a session's log isn't seen here.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// How deep a handover's folders are looked into: a session's notes and
/// cells sit a few folders down, and a deeper tree is refused rather
/// than walked without end.
const DEEPEST: usize = 16;

/// The first file under `folder` whose name says it holds a secret, or
/// none; links are never followed, so nothing outside it is judged, and
/// a link is judged by its own name. A missing folder holds none.
///
/// # Errors
///
/// When a folder can't be read, or the tree is deeper than [`DEEPEST`],
/// so what it holds can't be told.
pub fn first_in(folder: &Path) -> io::Result<Option<PathBuf>> {
    match fs::symlink_metadata(folder) {
        Ok(meta) if meta.is_dir() => look(folder, 0),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The first secret's file under `folder`, `depth` folders down.
fn look(folder: &Path, depth: usize) -> io::Result<Option<PathBuf>> {
    if depth > DEEPEST {
        return Err(io::Error::other(format!(
            "{} is more than {DEEPEST} folders deep, so what it holds can't be told",
            folder.display()
        )));
    }
    let mut entries: Vec<_> = fs::read_dir(folder)?.collect::<io::Result<_>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        if names_a_secret(&name.to_string_lossy()) {
            return Ok(Some(path));
        }
        // The entry's own type, which is a link's and not its target's.
        if entry.file_type()?.is_dir()
            && let Some(found) = look(&path, depth + 1)?
        {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// What a carry itself puts in a handover, named for a branch, a tree or
/// a session: a word in such a name says what the work is about, not
/// what the file holds, so only a key's ending refuses one.
const CARRIED: [&str; 4] = [".bundle", ".patch", ".jsonl", ".done.json"];

/// Whether a file named `name` holds a secret, as its name says: a
/// harness's login, a private key or certificate's key, a TPM's key, a
/// netrc, or one with the word credentials or token in its name.
#[must_use]
pub fn names_a_secret(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let private_ssh = ["id_rsa", "id_ecdsa", "id_ed25519", "id_dsa"]
        .iter()
        .any(|key| {
            lower.starts_with(key)
                && !Path::new(&lower)
                    .extension()
                    .is_some_and(|public| public == "pub")
        });
    let ends = [".pem", ".key", ".p12", ".pfx", ".tpm"]
        .iter()
        .any(|end| lower.ends_with(end));
    let carried = CARRIED.iter().any(|end| lower.ends_with(end));
    let worded = lower
        .split(|letter: char| !letter.is_ascii_alphanumeric())
        .any(|word| matches!(word, "token" | "tokens" | "credentials" | "credential"));
    private_ssh || ends || lower == "auth.json" || lower == ".netrc" || (worded && !carried)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_holds_a_secret_is_told() {
        for name in [
            "auth.json",
            "id_ed25519",
            "id_rsa",
            "id_ecdsa-cert",
            "server.pem",
            "Host.KEY",
            "bundle.p12",
            "cert.pfx",
            "signing.tpm",
            ".netrc",
            "credentials",
            "credentials.json",
            "github-token",
            "refresh_TOKEN.txt",
        ] {
            assert!(names_a_secret(name), "{name} holds a secret");
        }
    }

    #[test]
    fn a_name_that_holds_none_is_let_be() {
        for name in [
            "id_ed25519.pub",
            "id_rsa.pub",
            "session.done.json",
            "QEST-20261107-HARBOR.bundle",
            "keys.md",
            "monkey.txt",
            "tokenizer.rs",
            "wren-refresh-token.bundle",
            "token-budget.patch",
            "notes.jsonl",
            "authors.json",
        ] {
            assert!(!names_a_secret(name), "{name} holds none");
        }
    }

    #[test]
    fn a_handover_holding_a_secret_names_it_and_one_holding_none_names_nothing() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        let handover = scratch.path().join("handover");
        let deep = handover.join("skiff").join("cells");
        fs::create_dir_all(&deep).expect("its folders");
        fs::write(deep.join("one.json"), "{}").expect("a cell");
        fs::write(handover.join("skiff").join("one.done.json"), "{}").expect("a done file");
        assert_eq!(first_in(&handover).expect("read"), None, "nothing secret");
        fs::write(deep.join("auth.json"), "{}").expect("a login");
        assert_eq!(
            first_in(&handover).expect("read"),
            Some(deep.join("auth.json")),
            "a login a folder down is found"
        );
        assert_eq!(
            first_in(&scratch.path().join("gone")).expect("read"),
            None,
            "a handover not there holds none"
        );
    }

    #[test]
    fn a_link_is_judged_by_its_name_and_never_followed() {
        let scratch = tempfile::tempdir().expect("a scratch folder");
        let outside = scratch.path().join("outside");
        fs::create_dir_all(&outside).expect("outside");
        fs::write(outside.join("auth.json"), "{}").expect("a login outside");
        let handover = scratch.path().join("handover");
        fs::create_dir_all(&handover).expect("the handover");
        std::os::unix::fs::symlink(&outside, handover.join("elsewhere")).expect("a link");
        assert_eq!(
            first_in(&handover).expect("read"),
            None,
            "what a link leads to isn't judged"
        );
        std::os::unix::fs::symlink(outside.join("auth.json"), handover.join("id_rsa"))
            .expect("a link named for a key");
        assert_eq!(
            first_in(&handover).expect("read"),
            Some(handover.join("id_rsa")),
            "a link named for a secret is"
        );
    }
}
