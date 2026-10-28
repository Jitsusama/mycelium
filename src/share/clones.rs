//! The git clones in a shared quest's scratch, which cross as their remote
//! and branch rather than file by file.
//!
//! A clone copied file by file breaks when two machines write its `.git`
//! at once, and its worktrees name it by absolute path, so each machine
//! keeps a clone of its own, made from the same remote. One end says each
//! clone it holds; the other clones any it lacks, in a process of its own
//! the link never waits on, and never touches a folder that is already
//! there, so nothing a session did in a clone is lost.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use serde::{Deserialize, Serialize};

/// A clone, as the other end can make it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clone {
    /// Where its `origin` fetches from.
    pub remote: String,
    /// The branch it has checked out, or none where its head is detached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

impl Clone {
    /// The clone at `folder`, as its `origin` and its head say it, or
    /// nothing where it has no `origin` to clone again from, or where its
    /// `.git` is a file, as a worktree's is, naming another clone.
    #[must_use]
    pub fn read(folder: &Path) -> Option<Self> {
        let git = folder.join(".git");
        let config = fs::read_to_string(git.join("config")).ok()?;
        let remote = origin(&config)?;
        let head = fs::read_to_string(git.join("HEAD")).ok()?;
        let branch = head
            .trim()
            .strip_prefix("ref: refs/heads/")
            .map(str::to_owned);
        Some(Self { remote, branch })
    }

    /// Whether another machine may clone this: a remote reached over the
    /// network, by `https://`, `ssh://` or `user@host:path`, and a branch
    /// git can't take for an option. A local path names the other machine's
    /// disk, and a transport such as `ext::` runs a command, so neither is.
    #[must_use]
    pub fn may_clone(&self) -> bool {
        let remote = self.remote.as_str();
        let networked = remote.starts_with("https://")
            || remote.starts_with("ssh://")
            || remote.split_once(':').is_some_and(|(at, path)| {
                at.contains('@')
                    && !at.contains('/')
                    && !path.starts_with(':')
                    && !at.starts_with('-')
            });
        networked
            && !remote.chars().any(char::is_whitespace)
            && self.branch.as_deref().is_none_or(|branch| {
                !branch.is_empty()
                    && !branch.starts_with('-')
                    && !branch.chars().any(char::is_whitespace)
            })
    }
}

/// The url of the `origin` remote in a git config's text.
fn origin(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
        } else if in_origin
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "url"
        {
            return Some(value.trim().to_owned());
        }
    }
    None
}

/// The clones this end is making, for those the other end holds.
#[derive(Default)]
pub struct Cloning {
    /// Each clone being made, by the folder it goes to.
    making: Vec<(PathBuf, Child)>,
}

impl Cloning {
    /// Starts cloning `clone` into `folder`, unless the folder is there or
    /// a clone into it is being made; a clone that may not be made is
    /// passed over. Answers whether it began one.
    pub fn start(&mut self, folder: &Path, clone: &Clone) -> io::Result<bool> {
        if !clone.may_clone() {
            return Ok(false);
        }
        self.begin(folder, clone)
    }

    /// Starts cloning `clone` into `folder` as `start` does, whatever its
    /// remote. A machine with no `git` to clone with is told so, and the
    /// clone is passed over.
    fn begin(&mut self, folder: &Path, clone: &Clone) -> io::Result<bool> {
        if folder.exists() || self.making.iter().any(|(making, _)| making == folder) {
            return Ok(false);
        }
        if let Some(parent) = folder.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut git = Command::new("git");
        git.arg("clone").arg("--quiet");
        if let Some(branch) = &clone.branch {
            git.arg("--branch").arg(branch);
        }
        let child = git
            .arg("--")
            .arg(&clone.remote)
            .arg(folder)
            // A clone needing a password fails rather than waiting on one
            // nobody is there to give.
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn();
        match child {
            Ok(child) => {
                self.making.push((folder.to_owned(), child));
                Ok(true)
            }
            Err(error) => {
                eprintln!("cloning into {}: git: {error}", folder.display());
                Ok(false)
            }
        }
    }

    /// The folders being cloned into, for a test to see what was begun.
    #[cfg(test)]
    pub fn making(&self) -> Vec<PathBuf> {
        self.making
            .iter()
            .map(|(folder, _)| folder.clone())
            .collect()
    }

    /// Lets go of the clones made since last asked, telling of any that
    /// failed.
    pub fn reap(&mut self) {
        self.making
            .retain_mut(|(folder, child)| match child.try_wait() {
                Ok(None) => true,
                Ok(Some(status)) => {
                    if !status.success() {
                        eprintln!("cloning into {} failed: {status}", folder.display());
                    }
                    false
                }
                Err(error) => {
                    eprintln!("cloning into {}: {error}", folder.display());
                    false
                }
            });
    }

    /// Waits for every clone being made, for a test that must see them made.
    #[cfg(test)]
    pub fn wait(&mut self) -> Vec<bool> {
        self.making
            .drain(..)
            .map(|(_, mut child)| child.wait().is_ok_and(|status| status.success()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clone(remote: &str, branch: Option<&str>) -> Clone {
        Clone {
            remote: remote.to_owned(),
            branch: branch.map(str::to_owned),
        }
    }

    #[test]
    fn a_clone_is_read_as_its_origin_and_branch() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let git = dir.path().join("ttc/.git");
        fs::create_dir_all(&git).expect("made");
        fs::write(
            git.join("config"),
            "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://example.com/up.git\n\
             [remote \"origin\"]\n\turl = git@github.com:Shopify/ttc.git\n\
             \tfetch = +refs/heads/*:refs/remotes/origin/*\n",
        )
        .expect("written");
        fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("written");
        assert_eq!(
            Clone::read(&dir.path().join("ttc")),
            Some(clone("git@github.com:Shopify/ttc.git", Some("main"))),
            "its origin, not another remote, and its branch"
        );
        fs::write(
            git.join("HEAD"),
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904\n",
        )
        .expect("written");
        assert_eq!(
            Clone::read(&dir.path().join("ttc")),
            Some(clone("git@github.com:Shopify/ttc.git", None)),
            "a detached head names no branch"
        );
        fs::write(git.join("config"), "[core]\n\tbare = false\n").expect("written");
        assert_eq!(
            Clone::read(&dir.path().join("ttc")),
            None,
            "a clone with no origin can't be made again"
        );
    }

    #[test]
    fn only_a_remote_over_the_network_may_be_cloned() {
        for remote in [
            "https://github.com/Jitsusama/mycelium.git",
            "ssh://git@github.com/Jitsusama/mycelium.git",
            "git@github.com:Jitsusama/mycelium.git",
        ] {
            assert!(clone(remote, Some("main")).may_clone(), "{remote} may");
        }
        for remote in [
            "/home/jitsusama/src/mycelium",
            "file:///home/jitsusama/src/mycelium",
            "ext::sh -c touch% /tmp/owned",
            "-uhttps://evil",
            "../mycelium",
        ] {
            assert!(!clone(remote, None).may_clone(), "{remote} may not");
        }
        assert!(
            !clone("https://github.com/a/b.git", Some("--upload-pack=evil")).may_clone(),
            "nor may a branch git would take for an option"
        );
    }

    #[test]
    fn a_folder_already_there_is_never_cloned_into() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let mut cloning = Cloning::default();
        let there = dir.path().join("ttc");
        fs::create_dir_all(&there).expect("made");
        assert!(
            !cloning
                .start(&there, &clone("https://github.com/a/b.git", None))
                .expect("asked"),
            "a folder already there is left as it is"
        );
        assert!(
            !cloning
                .start(&dir.path().join("evil"), &clone("ext::sh -c id", None))
                .expect("asked"),
            "and a clone that may not be made isn't"
        );
        assert!(cloning.wait().is_empty(), "so nothing was begun");
    }

    #[test]
    fn a_clone_is_made_from_its_remote_at_its_branch() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let remote = dir.path().join("remote");
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .arg("-C")
                .arg(&remote)
                .args(args)
                .stdout(Stdio::null())
                .status()
                .expect("git runs");
            assert!(status.success(), "git {args:?}");
        };
        fs::create_dir_all(&remote).expect("made");
        git(&["init", "--quiet", "--initial-branch", "main"]);
        git(&["commit", "--quiet", "--allow-empty", "--message", "first"]);
        git(&["branch", "work"]);
        let mut cloning = Cloning::default();
        let folder = dir.path().join("scratch/notes/ttc");
        let from = clone(remote.to_str().expect("a utf-8 path"), Some("work"));
        assert!(
            cloning.begin(&folder, &from).expect("begun"),
            "a clone is begun"
        );
        assert!(
            !cloning.begin(&folder, &from).expect("asked"),
            "but only once while it is being made"
        );
        assert_eq!(cloning.wait(), vec![true], "and it is made");
        assert_eq!(
            Clone::read(&folder),
            Some(from),
            "from its remote, at its branch"
        );
    }
}
