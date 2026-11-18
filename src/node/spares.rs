//! Which work tree an agent's run asked from another machine may stand
//! in.
//!
//! A fence lets a run into a work tree only where its session holds it,
//! and a run asked from another machine is told no session, so it stands
//! in none. A quest shared between two machines is the one exception: a
//! session there may run in this machine's trees of the quest it shares.
//! The node tells the fence so (`--shared-tree`) only for a folder it has
//! opened itself and found in a tree whose holding session works, as this
//! machine's own record of that session says, a quest whose ledger names
//! the machine that asks, shared in its own right rather than only as a
//! quest above another. The run then enters the folder it opened, not
//! its path again, so a link swapped meanwhile moves nothing. What the
//! ask says never spares it, nor anything a share carries in.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::share::exchange::Places;

/// Where a far agent's run stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ground {
    /// In no work tree: it runs in `folder`, spared nothing.
    Plain(PathBuf),
    /// In a work tree held only for quests shared with the machine that
    /// asks: it runs in `folder`, and the fence is told to spare it.
    Shared {
        /// The folder it runs in, as the opened folder's own path says.
        folder: PathBuf,
        /// The quests whose tree it is, named in order.
        quest: String,
    },
}

/// Where a far agent's run stands, and the folder opened to judge it,
/// which the run enters.
#[derive(Debug)]
pub struct Footing {
    /// Where it stands.
    pub ground: Ground,
    /// The folder, opened.
    pub opened: OwnedFd,
}

impl Footing {
    /// Where the run starts: the very folder judged, not its path again.
    #[must_use]
    pub fn entered(self) -> (crate::run::Folder, Ground) {
        (crate::run::Folder::Opened(self.opened), self.ground)
    }
}

/// Where the work broker keeps its trees and its record of each, and
/// where pi keeps its record of each session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trees {
    /// The folder every tree is cut in, one folder a tree.
    pub root: PathBuf,
    /// The folder of the records, a JSON file a tree, naming its path and
    /// the sessions holding it.
    pub records: PathBuf,
    /// The folder of pi's records of its sessions, a JSON file a session
    /// named by its id, saying the quest it works. No share carries them.
    pub sessions: PathBuf,
}

/// Where a run asked by `asker`, another machine, in `folder` stands: in
/// no work tree of `trees`, or in one of a quest `shares` holds with
/// `asker`.
///
/// # Errors
///
/// When it stands in a work tree held for any quest not shared with
/// `asker`, or its folder can't be opened and named, so it can't be
/// judged, which the run is refused, so nothing runs there.
pub fn ground(
    folder: &Path,
    asker: &str,
    trees: &Trees,
    shares: Option<&Places>,
) -> Result<Footing, String> {
    let (opened, real) = opened(folder).map_err(|error| {
        format!(
            "{asker} asked to run in {}, which can't be opened to be judged: {error}",
            folder.display()
        )
    })?;
    Ok(Footing {
        ground: judged(&real, asker, trees, shares)?,
        opened,
    })
}

/// Where a run asked by `asker` in `real`, the path of a folder opened,
/// stands.
fn judged(
    real: &Path,
    asker: &str,
    trees: &Trees,
    shares: Option<&Places>,
) -> Result<Ground, String> {
    let Some(tree) = tree_under(real, &trees.root) else {
        return Ok(Ground::Plain(real.to_owned()));
    };
    let refused = format!("{asker} asked to run in {}, a work tree", tree.display());
    let holders = holders(&tree, &trees.records);
    if holders.is_empty() {
        return Err(format!("{refused} no session here is recorded holding"));
    }
    let quests: BTreeSet<String> = holders
        .iter()
        .filter_map(|session| quest_of(session, &trees.sessions))
        .collect();
    if quests.is_empty() {
        return Err(format!(
            "{refused} held by {}, which work no quest here",
            holders.join(", ")
        ));
    }
    let named = quests.iter().cloned().collect::<Vec<_>>().join(", ");
    let Some(places) = shares else {
        return Err(format!(
            "{refused} of {named}, and this machine shares no quest"
        ));
    };
    // Every quest the tree is held for must be shared, since a session
    // of any other works in it too.
    let ledgers = places.ledgers();
    let (mut unshared, mut above) = (Vec::new(), Vec::new());
    for quest in &quests {
        match ledgers.read(quest) {
            Ok(ledger) if ledger.theirs.contains_key(asker) => {
                // A quest shared only as one above another crosses as its
                // README alone, so none of its trees is the far machine's.
                if ledger.above {
                    above.push(quest.as_str());
                }
            }
            _ => unshared.push(quest.as_str()),
        }
    }
    if unshared.len() == quests.len() {
        Err(format!(
            "{refused} of {named}, none of which is shared with {asker}"
        ))
    } else if !unshared.is_empty() {
        Err(format!(
            "{refused} of {named}, of which {} isn't shared with {asker}",
            unshared.join(", ")
        ))
    } else if !above.is_empty() {
        Err(format!(
            "{refused} of {named}, shared with {asker} only as above another: {}",
            above.join(", ")
        ))
    } else {
        Ok(Ground::Shared {
            folder: real.to_owned(),
            quest: named,
        })
    }
}

/// `folder`, opened, and the path the opened folder has: what the run
/// enters and what it is judged by, one and the same.
fn opened(folder: &Path) -> io::Result<(OwnedFd, PathBuf)> {
    use rustix::fs::{Mode, OFlags};
    let opened = rustix::fs::open(
        folder,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let real = path_of(&opened)?;
    Ok((opened, real))
}

/// The path the kernel has for `opened`, an open folder.
#[cfg(target_os = "macos")]
fn path_of(opened: &OwnedFd) -> io::Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let path = rustix::fs::getpath(opened)?;
    Ok(PathBuf::from(std::ffi::OsString::from_vec(
        path.into_bytes(),
    )))
}

/// The path the kernel has for `opened`, an open folder.
#[cfg(not(target_os = "macos"))]
fn path_of(opened: &OwnedFd) -> io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    fs::read_link(format!("/proc/self/fd/{}", opened.as_raw_fd()))
}

/// The tree `real`, a resolved folder, stands in under `root`, where
/// trees are cut: the first folder below it, or none where `real` is
/// outside it or is `root` itself.
fn tree_under(real: &Path, root: &Path) -> Option<PathBuf> {
    let root = fs::canonicalize(root).ok()?;
    match real.strip_prefix(&root).ok()?.components().next()? {
        Component::Normal(name) => Some(root.join(name)),
        _ => None,
    }
}

/// What the broker records of a tree.
#[derive(Deserialize)]
struct Record {
    path: PathBuf,
    #[serde(default)]
    sessions: Vec<String>,
}

/// The sessions the broker's `records` say hold `tree`, a resolved
/// folder: those of each record whose path resolves to it. A record that
/// can't be read names no one.
fn holders(tree: &Path, records: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(records) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension() == Some(OsStr::new("json")))
        .filter_map(|path| fs::read(path).ok())
        .filter_map(|text| serde_json::from_slice::<Record>(&text).ok())
        .filter(|record| fs::canonicalize(&record.path).is_ok_and(|path| path == tree))
        .flat_map(|record| record.sessions)
        .collect()
}

/// What pi records of a session.
#[derive(Deserialize)]
struct Session {
    quest: Option<String>,
}

/// The quest `session` works, as pi's record of it in `sessions` says:
/// none for an id that isn't a plain name, or a record that can't be
/// read or names none.
fn quest_of(session: &str, sessions: &Path) -> Option<String> {
    let plain = !session.is_empty()
        && session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !plain {
        return None;
    }
    let text = fs::read(sessions.join(format!("{session}.json"))).ok()?;
    serde_json::from_slice::<Session>(&text).ok()?.quest
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use serde_json::json;

    use super::*;

    const SHARED: &str = "QEST-20261117-HARBOR";
    const OTHER: &str = "QEST-20261117-OTHERS";
    const ABOVE: &str = "QEST-20261117-ABOVES";

    /// A machine's trees and shares, in a temporary folder: `harbor`, a
    /// tree s-near holds for the quest shared with tern, and `others`, a
    /// tree s-else holds for a quest shared with no one.
    struct Machine {
        _dir: tempfile::TempDir,
        trees: Trees,
        places: Places,
        outside: PathBuf,
    }

    impl Machine {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("a temporary folder");
            let at = dir.path().canonicalize().expect("it resolves");
            let trees = Trees {
                root: at.join("work").join("trees"),
                records: at.join("work").join("cut"),
                sessions: at.join("pi").join("sessions"),
            };
            let places = Places {
                quests: at.join("quests"),
                scratch: at.join("scratch"),
                home: at.join("home"),
                state: at.join("state"),
            };
            let machine = Self {
                _dir: dir,
                outside: at.join("home"),
                trees,
                places,
            };
            fs::create_dir_all(&machine.outside).expect("made");
            machine.cut("harbor", &["s-near"]);
            machine.cut("others", &["s-else"]);
            machine.works("s-near", SHARED);
            machine.works("s-else", OTHER);
            machine.shares(SHARED, &["tern"], false);
            machine.shares(OTHER, &[], false);
            machine
        }

        /// Cuts tree `name`, held by `sessions`, as the broker records it.
        fn cut(&self, name: &str, sessions: &[&str]) {
            let tree = self.tree(name);
            fs::create_dir_all(tree.join("src")).expect("made");
            fs::create_dir_all(&self.trees.records).expect("made");
            fs::write(
                self.trees.records.join(format!("{name}.json")),
                json!({"identity": {"key": name}, "path": tree,
                       "owners": [], "sessions": sessions})
                .to_string(),
            )
            .expect("written");
        }

        /// Records that `session` works `quest`, as pi does.
        fn works(&self, session: &str, quest: &str) {
            fs::create_dir_all(&self.trees.sessions).expect("made");
            fs::write(
                self.trees.sessions.join(format!("{session}.json")),
                json!({"sessionId": session, "cwd": "/home/kestrel", "quest": quest}).to_string(),
            )
            .expect("written");
        }

        /// Keeps `quest`'s ledger, shared with `machines`, and only as a
        /// quest above another where `above`.
        fn shares(&self, quest: &str, machines: &[&str], above: bool) {
            let theirs: serde_json::Map<_, _> = machines
                .iter()
                .map(|machine| ((*machine).to_owned(), json!({})))
                .collect();
            let ledgers = self.places.state.join("shared");
            fs::create_dir_all(&ledgers).expect("made");
            fs::write(
                ledgers.join(format!("{quest}.json")),
                json!({"files": {}, "theirs": theirs, "above": above}).to_string(),
            )
            .expect("written");
        }

        fn tree(&self, name: &str) -> PathBuf {
            self.trees.root.join(name)
        }

        fn ground(&self, folder: &Path, asker: &str) -> Result<Ground, String> {
            ground(folder, asker, &self.trees, Some(&self.places)).map(|footing| footing.ground)
        }
    }

    #[test]
    fn a_run_in_no_work_tree_is_spared_nothing() {
        let machine = Machine::new();
        assert_eq!(
            machine.ground(&machine.outside, "tern"),
            Ok(Ground::Plain(machine.outside.clone()))
        );
    }

    #[test]
    fn a_folder_that_cant_be_opened_is_refused_wherever_it_is() {
        let machine = Machine::new();
        let missing = machine.outside.join("missing");
        let file = machine.outside.join("notes");
        fs::write(&file, "").expect("written");
        let in_tree = machine.tree("harbor").join("missing");
        for folder in [&missing, &file, &in_tree] {
            assert!(
                machine
                    .ground(folder, "tern")
                    .is_err_and(|why| why.starts_with(&format!(
                        "tern asked to run in {}, which can't be opened to be judged: ",
                        folder.display()
                    ))),
                "what isn't opened isn't judged, so nothing runs there: {}",
                folder.display()
            );
        }
    }

    #[test]
    fn a_tree_held_for_two_quests_is_spared_only_where_both_are_shared() {
        let machine = Machine::new();
        machine.cut("both", &["s-near", "s-else"]);
        assert_eq!(
            machine.ground(&machine.tree("both"), "tern"),
            Err(format!(
                "tern asked to run in {}, a work tree of {SHARED}, {OTHER}, of which \
                 {OTHER} isn't shared with tern",
                machine.tree("both").display()
            )),
            "s-else's work in it isn't tern's to reach"
        );
        machine.shares(OTHER, &["tern"], false);
        assert_eq!(
            machine.ground(&machine.tree("both"), "tern"),
            Ok(Ground::Shared {
                folder: machine.tree("both"),
                quest: format!("{SHARED}, {OTHER}")
            })
        );
    }

    #[test]
    fn a_run_in_a_tree_of_a_quest_shared_with_its_asker_is_spared() {
        let machine = Machine::new();
        let folder = machine.tree("harbor").join("src");
        assert_eq!(
            machine.ground(&folder, "tern"),
            Ok(Ground::Shared {
                folder,
                quest: SHARED.to_owned()
            })
        );
    }

    #[test]
    fn a_run_in_a_shared_quests_tree_from_a_machine_it_isnt_shared_with_is_refused() {
        let machine = Machine::new();
        assert_eq!(
            machine.ground(&machine.tree("harbor"), "osprey"),
            Err(format!(
                "osprey asked to run in {}, a work tree of {SHARED}, none of which is shared \
                 with osprey",
                machine.tree("harbor").display()
            ))
        );
    }

    #[test]
    fn a_run_in_another_quests_tree_is_refused_whoever_shares_one() {
        let machine = Machine::new();
        assert_eq!(
            machine.ground(&machine.tree("others"), "tern"),
            Err(format!(
                "tern asked to run in {}, a work tree of {OTHER}, none of which is shared \
                 with tern",
                machine.tree("others").display()
            ))
        );
    }

    #[test]
    fn a_readme_the_asker_writes_never_binds_a_tree_to_its_quest() {
        let machine = Machine::new();
        // tern writes the shared quest's README through the share, so it
        // may list any session of this machine as working it.
        let record = machine.places.quests.join(SHARED);
        fs::create_dir_all(&record).expect("made");
        fs::write(
            record.join("README.md"),
            format!(
                "---\nid: {SHARED}\nsessions:\n  - id: s-else\ntrees:\n  - path: {}\n---\n",
                machine.tree("others").display()
            ),
        )
        .expect("written");
        assert!(
            machine
                .ground(&machine.tree("others"), "tern")
                .is_err_and(|why| why.contains("none of which is shared with tern")),
            "s-else's own record says it works {OTHER}"
        );
    }

    #[test]
    fn a_quest_shared_only_as_above_another_spares_none_of_its_trees() {
        let machine = Machine::new();
        machine.cut("parent", &["s-above"]);
        machine.works("s-above", ABOVE);
        machine.shares(ABOVE, &["tern"], true);
        assert_eq!(
            machine.ground(&machine.tree("parent"), "tern"),
            Err(format!(
                "tern asked to run in {}, a work tree of {ABOVE}, shared with tern only as \
                 above another: {ABOVE}",
                machine.tree("parent").display()
            ))
        );
    }

    #[test]
    fn a_link_or_a_climb_out_of_a_shared_tree_is_judged_where_it_lands() {
        let machine = Machine::new();
        let link = machine.tree("harbor").join("elsewhere");
        symlink(machine.tree("others"), &link).expect("linked");
        let others = Err(format!(
            "tern asked to run in {}, a work tree of {OTHER}, none of which is shared with tern",
            machine.tree("others").display()
        ));
        assert_eq!(
            machine.ground(&link, "tern"),
            others,
            "a link in the shared tree lands in another"
        );
        let climb = machine.tree("harbor").join("..").join("others");
        assert_eq!(
            machine.ground(&climb, "tern"),
            others,
            "a climb out of the shared tree lands in another"
        );
        let into = machine.outside.join("harbor");
        symlink(machine.tree("harbor"), &into).expect("linked");
        assert_eq!(
            machine.ground(&into, "tern"),
            Ok(Ground::Shared {
                folder: machine.tree("harbor"),
                quest: SHARED.to_owned()
            }),
            "a link from outside into the shared tree runs where it lands"
        );
    }

    #[tokio::test]
    async fn the_folder_judged_is_the_one_the_run_enters() {
        let machine = Machine::new();
        let link = machine.outside.join("swapped");
        symlink(machine.tree("harbor"), &link).expect("linked");
        let footing = ground(&link, "tern", &machine.trees, Some(&machine.places))
            .expect("the shared tree is spared");
        // The link now points at another quest's tree, as an asker
        // swapping it between the check and the start would leave it.
        fs::remove_file(&link).expect("unlinked");
        symlink(machine.tree("others"), &link).expect("linked again");
        let (folder, _) = footing.entered();
        let (ran, mut said) = tokio::sync::mpsc::channel(crate::run::BACKLOG);
        tokio::spawn(crate::run::here(
            vec!["sh".to_owned(), "-c".to_owned(), "pwd -P".to_owned()],
            folder,
            None,
            ran,
        ));
        let mut out = Vec::new();
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(10), said.recv())
                .await
                .expect("the run says more")
                .expect("the run ends with its end")
            {
                crate::wire::Ran::Out(piece) => {
                    out.extend(crate::run::bytes(&piece).expect("base64"));
                }
                crate::wire::Ran::Exited(0) => break,
                other => panic!("it ran amiss: {other:?}"),
            }
        }
        assert_eq!(
            String::from_utf8_lossy(&out),
            format!("{}\n", machine.tree("harbor").display()),
            "what the run enters is still the shared tree"
        );
    }

    #[test]
    fn a_tree_no_record_names_or_a_machine_sharing_nothing_is_refused() {
        let machine = Machine::new();
        let unrecorded = machine.tree("unrecorded");
        fs::create_dir_all(&unrecorded).expect("made");
        assert_eq!(
            machine.ground(&unrecorded, "tern"),
            Err(format!(
                "tern asked to run in {}, a work tree no session here is recorded holding",
                unrecorded.display()
            ))
        );
        machine.cut("unworked", &["s-gone", "../s-near"]);
        assert_eq!(
            machine.ground(&machine.tree("unworked"), "tern"),
            Err(format!(
                "tern asked to run in {}, a work tree held by s-gone, ../s-near, which work \
                 no quest here",
                machine.tree("unworked").display()
            )),
            "a session pi keeps no record of, or an id that is a path, works nothing"
        );
        assert_eq!(
            ground(&machine.tree("harbor"), "tern", &machine.trees, None)
                .map(|footing| footing.ground),
            Err(format!(
                "tern asked to run in {}, a work tree of {SHARED}, and this machine shares \
                 no quest",
                machine.tree("harbor").display()
            ))
        );
    }
}
