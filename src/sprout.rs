//! Sprouting a cell: one act that takes a piece of work, a link or a few
//! words, and starts a pi session of its own on it, in the repository it
//! belongs to, told to make it a quest at the top of the tree and to own
//! all of it.
//!
//! Starting new work took a person four to eight steps, each read from a
//! help page, and the session it made could land in a home folder with
//! none of its repository's skills, filed under whichever quest happened
//! to be loaded. A sprout says where the work goes before anything starts,
//! refusing a home folder, and leaves nothing to the session it starts but
//! the work itself.

use std::path::{Path, PathBuf};

/// What a sprout is given: a link, which the session seeds its quest
/// from, or words, which name the work themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seed {
    /// A link to the work, as a pull request, an issue or a thread.
    Link(String),
    /// The work in a person's own words.
    Words(String),
}

impl Seed {
    /// The seed `words` make: a link when they are one alone, else the
    /// words as said, joined by spaces.
    #[must_use]
    pub fn of(words: &[String]) -> Self {
        match words {
            [one] if is_link(one) => Self::Link(one.clone()),
            _ => Self::Words(words.join(" ").trim().to_owned()),
        }
    }

    /// Whether it says nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Link(said) | Self::Words(said) => said.is_empty(),
        }
    }
}

fn is_link(word: &str) -> bool {
    word.starts_with("https://") || word.starts_with("http://")
}

/// Where a sprout is asked to begin, and what it can see of this machine.
#[derive(Debug, Clone, Copy)]
pub struct Asked<'a> {
    /// The folder named for it, if one was.
    pub folder: Option<&'a Path>,
    /// The folder it was asked from.
    pub cwd: &'a Path,
    /// This machine's home.
    pub home: &'a Path,
    /// Whether it starts on this machine, whose folders can be looked at.
    pub here: bool,
}

/// What this machine's disk says of a folder, so the choice is tested
/// without one.
pub trait Disk {
    /// Whether `folder` is a folder here.
    fn is_folder(&self, folder: &Path) -> bool;
    /// Whether `folder` is within a repository's checkout here.
    fn in_repository(&self, folder: &Path) -> bool;
}

/// This machine's disk.
pub struct ThisDisk;

impl Disk for ThisDisk {
    fn is_folder(&self, folder: &Path) -> bool {
        folder.is_dir()
    }

    fn in_repository(&self, folder: &Path) -> bool {
        folder.ancestors().any(|above| above.join(".git").exists())
    }
}

/// The folder a sprout begins in: the one named for it; else the
/// checkout here of the repository its link is on, under `~/src` as
/// `<repo>` or `<host>/<owner>/<repo>`; else the folder it was asked from,
/// where that is in a repository. A home folder is never one, since a
/// session started there has none of its repository's skills, and a
/// folder named for this machine that isn't one here is refused rather
/// than begun in home.
///
/// # Errors
///
/// When none of those gives a folder, or the one named is home or isn't
/// here, saying what to name instead.
pub fn folder(asked: Asked<'_>, seed: &Seed, disk: &impl Disk) -> Result<PathBuf, String> {
    if let Some(named) = asked.folder {
        let named = if named.is_absolute() {
            named.to_owned()
        } else {
            asked.cwd.join(named)
        };
        if named == asked.home {
            return Err(home_refused());
        }
        if asked.here && !disk.is_folder(&named) {
            return Err(format!("{} isn't a folder here", named.display()));
        }
        return Ok(named);
    }
    if let Seed::Link(link) = seed {
        let checkout = checkouts(link, asked.home)
            .into_iter()
            .find(|checkout| disk.is_folder(checkout));
        if let Some(checkout) = checkout {
            return Ok(checkout);
        }
    }
    if asked.cwd != asked.home && disk.in_repository(asked.cwd) {
        return Ok(asked.cwd.to_owned());
    }
    Err(format!(
        "it needs a repository to work in, and {} isn't one: say which, as `mycelium sprout --in <folder> ...`",
        asked.cwd.display()
    ))
}

fn home_refused() -> String {
    "a session begun in a home folder has none of its repository's skills: name the repository's folder".to_owned()
}

/// Where a link's repository may be checked out under `home`: for
/// `https://<host>/<owner>/<repo>/...`, `~/src/<repo>` and then
/// `~/src/<host>/<owner>/<repo>`. None for a link not shaped so.
#[must_use]
pub fn checkouts(link: &str, home: &Path) -> Vec<PathBuf> {
    let Some(rest) = link
        .strip_prefix("https://")
        .or_else(|| link.strip_prefix("http://"))
    else {
        return Vec::new();
    };
    let mut parts = rest.split('/').filter(|part| !part.is_empty());
    let (Some(host), Some(owner), Some(repo)) = (parts.next(), parts.next(), parts.next()) else {
        return Vec::new();
    };
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    let src = home.join("src");
    vec![src.join(repo), src.join(host).join(owner).join(repo)]
}

/// The words a sprouted session is told first: to make the work its own
/// quest at the top of the tree, seeded from the link or named by the
/// words, and to own all of it; and, where it came from a cell, to tie
/// its quest to that cell on the map.
#[must_use]
pub fn first_words(seed: &Seed, from: Option<&str>) -> String {
    let make = match seed {
        Seed::Link(link) => format!(
            "Joel sprouted this session from {link}. Make it a quest of its own now: call quest create with kind quest, priority driving and url {link}. Then read the link and fill the quest's Summary and Purpose from it."
        ),
        Seed::Words(words) => format!(
            "Joel sprouted this session from his words: \"{words}\". Make them a quest of its own now: call quest create with kind quest, priority driving, a title of your own drawn from them, and note set to his words as he said them."
        ),
    };
    let tie = from.map_or_else(String::new, |from| {
        format!(
            " Once it exists, tie it on the map to the cell it came from: mycelium web <its id> tie <its id> {from} --how sprouted-from."
        )
    });
    format!(
        "{make} No quest is loaded here, so it is made at the top of the tree and loaded.{tie} The whole quest is yours: plan it, build it, merge what passes its checks, and tell Joel what it needs from him as it comes, each line saying the work, why it exists, where it stands and what you ask. Nobody else loads it."
    )
}

/// What a guard says, and ends with, where a session started elsewhere
/// would begin in that machine's home, as a start does when the folder it
/// asked for isn't there.
const IN_HOME: &str = "mycelium sprout: that machine has no such folder, so this session would begin in its home; nothing was started";

/// The command a sprout starts: pi, told its first words, behind a guard
/// that ends it before it starts where it would begin in home.
#[must_use]
pub fn command(first_words: &str) -> Vec<String> {
    let guard = format!(
        r#"if [ "$PWD" = "$HOME" ]; then echo "{IN_HOME}" >&2; sleep 10; exit 1; fi; exec "$@""#
    );
    vec![
        "sh".to_owned(),
        "-c".to_owned(),
        guard,
        "sprout".to_owned(),
        "env".to_owned(),
        format!("QUEST_WORKFLOW_FIRST_TEXT={first_words}"),
        "pi".to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// A disk holding the folders and checkouts named.
    struct Held {
        folders: BTreeSet<PathBuf>,
        repositories: BTreeSet<PathBuf>,
    }

    impl Held {
        fn of(folders: &[&str], repositories: &[&str]) -> Self {
            Self {
                folders: folders.iter().map(PathBuf::from).collect(),
                repositories: repositories.iter().map(PathBuf::from).collect(),
            }
        }
    }

    impl Disk for Held {
        fn is_folder(&self, folder: &Path) -> bool {
            self.folders.contains(folder)
        }
        fn in_repository(&self, folder: &Path) -> bool {
            self.repositories
                .iter()
                .any(|repository| folder.starts_with(repository))
        }
    }

    fn asked<'a>(folder: Option<&'a Path>, cwd: &'a Path) -> Asked<'a> {
        Asked {
            folder,
            cwd,
            home: Path::new("/home/kestrel"),
            here: true,
        }
    }

    fn words(said: &str) -> Seed {
        Seed::Words(said.to_owned())
    }

    #[test]
    fn a_link_alone_is_a_link_and_anything_else_is_words() {
        let link = "https://example.org/owner/repo/pull/7".to_owned();
        assert_eq!(
            Seed::of(std::slice::from_ref(&link)),
            Seed::Link(link.clone())
        );
        assert_eq!(
            Seed::of(&["fix".to_owned(), link.clone()]),
            Seed::Words(format!("fix {link}"))
        );
        assert_eq!(
            Seed::of(&["make".to_owned(), "tea".to_owned()]),
            words("make tea")
        );
        assert!(Seed::of(&[]).is_empty());
    }

    #[test]
    fn the_folder_named_is_where_it_begins() {
        let disk = Held::of(&["/home/kestrel/src/skiff"], &[]);
        let begins = folder(
            asked(Some(Path::new("src/skiff")), Path::new("/home/kestrel")),
            &words("x"),
            &disk,
        );
        assert_eq!(begins, Ok(PathBuf::from("/home/kestrel/src/skiff")));
    }

    #[test]
    fn home_is_refused_even_when_named() {
        let disk = Held::of(&["/home/kestrel"], &["/home/kestrel"]);
        let begins = folder(
            asked(Some(Path::new("/home/kestrel")), Path::new("/tmp")),
            &words("x"),
            &disk,
        );
        assert!(begins.unwrap_err().contains("home folder"));
        let from_home = folder(asked(None, Path::new("/home/kestrel")), &words("x"), &disk);
        assert!(from_home.unwrap_err().contains("--in"));
    }

    #[test]
    fn a_folder_named_for_here_that_isnt_one_is_refused() {
        let disk = Held::of(&[], &[]);
        let begins = folder(
            asked(Some(Path::new("/home/kestrel/src/gone")), Path::new("/tmp")),
            &words("x"),
            &disk,
        );
        assert!(begins.unwrap_err().contains("isn't a folder here"));
    }

    #[test]
    fn one_named_for_another_machine_is_left_to_it() {
        let disk = Held::of(&[], &[]);
        let there = Asked {
            here: false,
            ..asked(
                Some(Path::new("/home/kestrel/src/osprey")),
                Path::new("/tmp"),
            )
        };
        assert_eq!(
            folder(there, &words("x"), &disk),
            Ok(PathBuf::from("/home/kestrel/src/osprey"))
        );
    }

    #[test]
    fn a_link_begins_in_its_repository_checked_out_here() {
        let link = Seed::Link("https://example.org/owner/skiff/pull/7".to_owned());
        let short = Held::of(&["/home/kestrel/src/skiff"], &["/home/kestrel/src/tern"]);
        assert_eq!(
            folder(
                asked(None, Path::new("/home/kestrel/src/tern")),
                &link,
                &short
            ),
            Ok(PathBuf::from("/home/kestrel/src/skiff"))
        );
        let long = Held::of(&["/home/kestrel/src/example.org/owner/skiff"], &[]);
        assert_eq!(
            folder(asked(None, Path::new("/home/kestrel")), &link, &long),
            Ok(PathBuf::from("/home/kestrel/src/example.org/owner/skiff"))
        );
    }

    #[test]
    fn otherwise_the_repository_it_was_asked_from() {
        let link = Seed::Link("https://example.org/owner/skiff/pull/7".to_owned());
        let disk = Held::of(&[], &["/home/kestrel/src/tern"]);
        assert_eq!(
            folder(
                asked(None, Path::new("/home/kestrel/src/tern/src")),
                &link,
                &disk
            ),
            Ok(PathBuf::from("/home/kestrel/src/tern/src"))
        );
        assert!(
            folder(asked(None, Path::new("/tmp")), &link, &disk)
                .unwrap_err()
                .contains("--in")
        );
    }

    #[test]
    fn a_link_not_shaped_as_a_repositorys_has_no_checkouts() {
        let none: [PathBuf; 0] = [];
        assert_eq!(
            checkouts("https://example.org/owner", Path::new("/h")),
            none
        );
        assert_eq!(checkouts("skiff", Path::new("/h")), none);
        assert_eq!(
            checkouts("https://example.org/owner/skiff.git", Path::new("/h"))[0],
            PathBuf::from("/h/src/skiff")
        );
    }

    #[test]
    fn the_first_words_make_a_top_level_quest_it_owns() {
        let link = "https://example.org/owner/skiff/issues/3";
        let said = first_words(&Seed::Link(link.to_owned()), None);
        assert!(said.contains(&format!("url {link}")));
        assert!(said.contains("kind quest"));
        assert!(said.contains("The whole quest is yours"));
        assert!(!said.contains("tie it"));
        let from = first_words(&words("make tea"), Some("HARBOR"));
        assert!(from.contains("\"make tea\""));
        assert!(from.contains("tie <its id> HARBOR --how sprouted-from"));
    }

    #[test]
    fn the_command_guards_against_home_then_runs_pi_told_its_words() {
        let said = command("hello");
        assert_eq!(&said[..2], ["sh", "-c"]);
        assert!(said[2].contains(r#""$PWD" = "$HOME""#));
        assert!(said[2].ends_with(r#"exec "$@""#));
        assert_eq!(
            &said[3..],
            ["sprout", "env", "QUEST_WORKFLOW_FIRST_TEXT=hello", "pi"]
        );
    }
}
