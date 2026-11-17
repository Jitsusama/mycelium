//! Which work tree an agent's run asked from another machine may stand
//! in.
//!
//! A fence lets a run into a work tree only where its session holds it,
//! and a run asked from another machine is told no session, so it stands
//! in none. A quest shared between two machines is the one exception: a
//! session there may run in this machine's trees of the quest it shares.
//! The node tells the fence so (`--shared-tree`) only once it has found
//! the folder itself, its links and `..` resolved, in a tree a session of
//! that quest holds, and the quest's ledger names the machine that asks.
//! What the ask says never spares it.

use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::share::exchange::Places;

/// Where a far agent's run stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ground {
    /// In no work tree: it runs in `folder`, spared nothing.
    Plain(PathBuf),
    /// In a work tree of `quest`, which is shared with the machine that
    /// asks: it runs in `folder`, and the fence is told to spare it.
    Shared {
        /// The folder it runs in, its links and `..` resolved.
        folder: PathBuf,
        /// The quest whose tree it is.
        quest: String,
    },
}

/// Where the work broker keeps its trees and its record of each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trees {
    /// The folder every tree is cut in, one folder a tree.
    pub root: PathBuf,
    /// The folder of the records, a JSON file a tree, naming its path and
    /// the sessions holding it.
    pub records: PathBuf,
}

/// Where a run asked by `asker`, another machine, in `folder` stands: in
/// no work tree of `trees`, or in one of a quest `shares` holds with
/// `asker`.
///
/// # Errors
///
/// When it stands in a work tree of no quest shared with `asker`, which
/// the run is refused, so nothing runs there.
pub fn ground(
    folder: &Path,
    asker: &str,
    trees: &Trees,
    shares: Option<&Places>,
) -> Result<Ground, String> {
    // A folder that isn't there is in no tree this machine can tell, and
    // runs where it was asked, spared nothing, so the fence judges it.
    let Ok(real) = fs::canonicalize(folder) else {
        return Ok(Ground::Plain(folder.to_owned()));
    };
    let Some(tree) = fs::canonicalize(&trees.root)
        .ok()
        .and_then(|root| tree_of(&real, &root))
    else {
        return Ok(Ground::Plain(real));
    };
    let holders = holders(&tree, &trees.records);
    let refused = format!(
        "{asker} asked to run in {}, a work tree of no quest this machine shares with it",
        tree.display()
    );
    let Some(places) = shares else {
        return Err(refused);
    };
    let ledgers = places.ledgers();
    for quest in ledgers.quests().unwrap_or_default() {
        let shared_with_asker = ledgers
            .read(&quest)
            .is_ok_and(|ledger| ledger.theirs.contains_key(asker));
        if !shared_with_asker {
            continue;
        }
        let Ok(readme) = fs::read_to_string(places.quests.join(&quest).join("README.md")) else {
            continue;
        };
        if sessions_of(&readme)
            .iter()
            .any(|session| holders.contains(session))
        {
            return Ok(Ground::Shared {
                folder: real,
                quest,
            });
        }
    }
    Err(refused)
}

/// The tree `real`, a resolved folder, stands in under `root`, the
/// resolved folder trees are cut in: the first folder below it, or none
/// where `real` is outside it or is `root` itself.
fn tree_of(real: &Path, root: &Path) -> Option<PathBuf> {
    match real.strip_prefix(root).ok()?.components().next()? {
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

/// The sessions a quest's README lists as working it: the ids of the
/// items of its frontmatter's `sessions`, and nothing nested in one.
fn sessions_of(readme: &str) -> Vec<String> {
    let Some(front) = readme
        .strip_prefix("---\n")
        .and_then(|rest| rest.split("\n---").next())
    else {
        return Vec::new();
    };
    let mut lines = front
        .lines()
        .skip_while(|line| line.trim_end() != "sessions:");
    lines.next();
    let mut items = None;
    let mut sessions = Vec::new();
    for line in lines.take_while(|line| line.starts_with([' ', '-']) || line.is_empty()) {
        let rest = line.trim_start();
        let indent = line.len() - rest.len();
        if !rest.starts_with("- ") || *items.get_or_insert(indent) != indent {
            continue;
        }
        if let Some(id) = rest.strip_prefix("- id:") {
            sessions.push(id.trim().trim_matches(['"', '\'']).to_owned());
        }
    }
    sessions
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use serde_json::json;

    use super::*;

    const SHARED: &str = "QEST-20261117-HARBOR";
    const OTHER: &str = "QEST-20261117-OTHERS";

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
            machine.quest(SHARED, &["s-near"], &["tern"]);
            machine.quest(OTHER, &["s-else"], &[]);
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

        /// Keeps `quest`, which `sessions` work and `machines` share.
        fn quest(&self, quest: &str, sessions: &[&str], machines: &[&str]) {
            let folder = self.places.quests.join(quest);
            fs::create_dir_all(&folder).expect("made");
            let listed = sessions
                .iter()
                .map(|session| format!("  - id: {session}\n    status: active\n"))
                .collect::<Vec<_>>()
                .concat();
            fs::write(
                folder.join("README.md"),
                format!("---\nid: {quest}\nsessions:\n{listed}---\n# A Quest\n"),
            )
            .expect("written");
            let theirs: serde_json::Map<_, _> = machines
                .iter()
                .map(|machine| ((*machine).to_owned(), json!({})))
                .collect();
            let ledgers = self.places.state.join("shared");
            fs::create_dir_all(&ledgers).expect("made");
            fs::write(
                ledgers.join(format!("{quest}.json")),
                json!({"files": {}, "theirs": theirs}).to_string(),
            )
            .expect("written");
        }

        fn tree(&self, name: &str) -> PathBuf {
            self.trees.root.join(name)
        }

        fn ground(&self, folder: &Path, asker: &str) -> Result<Ground, String> {
            ground(folder, asker, &self.trees, Some(&self.places))
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
        let refused = machine
            .ground(&machine.tree("harbor"), "osprey")
            .expect_err("osprey shares no quest here");
        assert!(refused.contains("osprey"), "{refused}");
    }

    #[test]
    fn a_run_in_another_quests_tree_is_refused_whoever_shares_one() {
        let machine = Machine::new();
        machine
            .ground(&machine.tree("others"), "tern")
            .expect_err("tern shares only the harbor quest");
    }

    #[test]
    fn a_link_or_a_climb_out_of_a_shared_tree_is_judged_where_it_lands() {
        let machine = Machine::new();
        let link = machine.tree("harbor").join("elsewhere");
        symlink(machine.tree("others"), &link).expect("linked");
        machine
            .ground(&link, "tern")
            .expect_err("a link in the shared tree lands in another");
        let climb = machine.tree("harbor").join("..").join("others");
        machine
            .ground(&climb, "tern")
            .expect_err("a climb out of the shared tree lands in another");
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

    #[test]
    fn a_tree_no_record_names_or_a_machine_sharing_nothing_is_refused() {
        let machine = Machine::new();
        let unrecorded = machine.tree("unrecorded");
        fs::create_dir_all(&unrecorded).expect("made");
        machine
            .ground(&unrecorded, "tern")
            .expect_err("no session is known to hold it");
        ground(&machine.tree("harbor"), "tern", &machine.trees, None)
            .expect_err("a machine sharing nothing spares no tree");
    }

    #[test]
    fn a_quests_sessions_are_those_its_readme_lists() {
        let readme = "---\nid: QEST-20261117-HARBOR\nsessions:\n  - id: s-one\n    \
                      process:\n      hosted:\n        session: s-hosted\n  - id: \"s-two\"\n\
                      trees:\n  - id: t-one\n---\n# A Quest\n  - id: s-body\n";
        assert_eq!(sessions_of(readme), ["s-one", "s-two"]);
    }
}
