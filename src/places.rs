//! Where work can begin on this machine, each by the name a person says
//! it by: every repository checked out under `~/src`, the tree of each
//! bare repository just below home that has its trunk checked out, and
//! every zone a checkout's manifest names, checked out or not. The
//! launcher's folder list and a sprout's `--in` read the same places, so
//! a name found in one is begun in by the other.
//!
//! On 24 November 2026 the person asked to start work in a monorepo's
//! zone by its name. The list offered only folders, none of them the
//! zone, and a sprout from home with no folder named was refused with
//! nothing begun.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sprout::Disk;

/// Where a checkout names its zones: one JSON object whose keys are each
/// zone's path from the checkout's top, as `//<path>`.
pub const MANIFEST: &str = ".meta/manifest.json";

/// The most of a manifest read, in bytes; one past it names no zones.
pub const MANIFEST_MOST: u64 = 8 << 20;

/// The trunks a bare repository's tree may have checked out, in the order
/// they are looked for.
const TRUNKS: [&str; 2] = ["refs/heads/main", "refs/heads/master"];

/// What a place is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A repository's checkout.
    Checkout,
    /// A zone a checkout's manifest names.
    Zone,
}

/// A place work can begin in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    /// Its folder, whole.
    pub folder: PathBuf,
    /// Its name as a person says it: `~/src/<repo>` for a checkout in
    /// home, `//<path>` for a zone.
    pub name: String,
    /// What it is.
    pub kind: Kind,
    /// Whether it isn't checked out, as a zone of a partial checkout may
    /// not be.
    pub out: bool,
    /// The command that checks it out, where it isn't.
    #[serde(default, rename = "checkOut", skip_serializing_if = "Option::is_none")]
    pub check_out: Option<String>,
}

/// Every place on `disk` under `home`: each checkout, then the zones its
/// manifest names, sorted by name within each checkout.
#[must_use]
pub fn on(home: &Path, disk: &impl Disk) -> Vec<Place> {
    let mut places = Vec::new();
    for checkout in checkouts(home, disk) {
        places.push(Place {
            name: named(&checkout, home),
            folder: checkout.clone(),
            kind: Kind::Checkout,
            out: false,
            check_out: None,
        });
        places.extend(zones(&checkout, disk));
    }
    places
}

/// Each checkout work can begin in: those under `~/src`, as
/// `<repo>` or `<host>/<owner>/<repo>`, and the trunk's tree of each bare
/// repository at `~/<name>` or `~/<name>/<name>`, hidden folders passed
/// over, each once.
fn checkouts(home: &Path, disk: &impl Disk) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for top in disk.folders(&home.join("src")) {
        if disk.is_checkout(&top) {
            found.push(top);
            continue;
        }
        // Not a checkout, so a host, holding its owners' repositories.
        for owner in disk.folders(&top) {
            found.extend(
                disk.folders(&owner)
                    .into_iter()
                    .filter(|repository| disk.is_checkout(repository)),
            );
        }
    }
    for top in disk.folders(home).into_iter().filter(|top| !hidden(top)) {
        let below = disk.folders(&top);
        let bare = std::iter::once(top)
            .chain(below.into_iter().filter(|one| !hidden(one)))
            .filter(|one| disk.is_bare(one));
        found.extend(bare.filter_map(|repository| disk.trunk(&repository)));
    }
    found.sort();
    found.dedup();
    found
}

fn hidden(folder: &Path) -> bool {
    folder
        .file_name()
        .and_then(|name| name.to_str())
        .is_none_or(|name| name.starts_with('.'))
}

/// `folder` as a person names it: from `~` where it is in `home`.
fn named(folder: &Path, home: &Path) -> String {
    match folder.strip_prefix(home) {
        Ok(within) => format!("~/{}", within.display()),
        Err(_) => folder.display().to_string(),
    }
}

/// The zones `checkout`'s manifest names, sorted by name; none where it
/// has none, or one that can't be read as an object.
fn zones(checkout: &Path, disk: &impl Disk) -> Vec<Place> {
    let Some(said) = disk.read(&checkout.join(MANIFEST), MANIFEST_MOST) else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Object(manifest)) = serde_json::from_str(&said) else {
        return Vec::new();
    };
    let mut zones: Vec<Place> = manifest
        .keys()
        .filter_map(|name| {
            let path = zone_path(name)?;
            let folder = checkout.join(path);
            let out = !disk.is_folder(&folder);
            Some(Place {
                check_out: out
                    .then(|| format!("git -C {} sparse-checkout add {path}", checkout.display())),
                folder,
                name: name.clone(),
                kind: Kind::Zone,
                out,
            })
        })
        .collect();
    zones.sort_by(|one, other| one.name.cmp(&other.name));
    zones
}

/// The path from a checkout's top that the zone `name` (`//<path>`) is
/// at, where each step is a plain name, so none walks out of it.
fn zone_path(name: &str) -> Option<&str> {
    let path = name.strip_prefix("//")?;
    (!path.is_empty() && path.split('/').all(is_plain)).then_some(path)
}

fn is_plain(step: &str) -> bool {
    !step.is_empty()
        && !step.chars().all(|c| c == '.')
        && step
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Whether `said` names a place rather than a folder: a zone's path,
/// `//<path>`, or one plain name, such as `reef`.
#[must_use]
pub fn is_name(said: &str) -> bool {
    zone_path(said).is_some() || is_plain(said)
}

/// Why `name` finds no one place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unfound {
    /// It names none.
    None,
    /// It names several, each by its whole name, the first few.
    Several(Vec<String>),
}

/// The most places an unfound name lists.
const LISTED_MOST: usize = 8;

/// The one place in `places` that `name` names: a zone by its whole path,
/// `//<path>`, or a zone or checkout by its last step, as `reef` names
/// `//system/reef`. Where the same name is a zone of several checkouts,
/// the first checkout's is meant, as they are sorted.
///
/// # Errors
///
/// Where it names none, or several, saying which.
pub fn found<'a>(places: &'a [Place], name: &str) -> Result<&'a Place, Unfound> {
    let whole = zone_path(name).is_some();
    let mut named = places.iter().filter(|place| {
        if whole {
            place.name == name
        } else {
            place.folder.file_name().and_then(|last| last.to_str()) == Some(name)
        }
    });
    let Some(first) = named.next() else {
        return Err(Unfound::None);
    };
    let others: Vec<&Place> = named.filter(|other| other.name != first.name).collect();
    if others.is_empty() {
        return Ok(first);
    }
    let mut several: Vec<String> = std::iter::once(first)
        .chain(others)
        .map(|place| place.name.clone())
        .collect();
    several.sort_unstable();
    several.dedup();
    several.truncate(LISTED_MOST);
    Err(Unfound::Several(several))
}

/// The folder a link into `checkout` begins in: the deepest zone the
/// path after its `tree/<ref>/` or `blob/<ref>/` lies in, where that zone
/// is checked out; the checkout itself otherwise.
#[must_use]
pub fn zone_of_link(places: &[Place], checkout: &Path, after_repository: &[&str]) -> PathBuf {
    let path = match after_repository {
        ["tree" | "blob", _, rest @ ..] if !rest.is_empty() => rest,
        _ => return checkout.to_owned(),
    };
    let wanted = checkout.join(path.join("/"));
    places
        .iter()
        .filter(|place| place.kind == Kind::Zone && !place.out)
        .filter(|place| place.folder.starts_with(checkout) && wanted.starts_with(&place.folder))
        .max_by_key(|place| place.folder.components().count())
        .map_or_else(|| checkout.to_owned(), |zone| zone.folder.clone())
}

/// The tree of the bare repository `porcelain` lists, as `git worktree
/// list --porcelain` says them, that has a trunk checked out, `main`
/// before `master`.
#[must_use]
pub fn trunk_of(porcelain: &str) -> Option<PathBuf> {
    let trees: Vec<(&str, Option<&str>)> = porcelain
        .split("\n\n")
        .filter_map(|tree| {
            let mut lines = tree.lines();
            let folder = lines.next()?.strip_prefix("worktree ")?;
            let branch = lines.find_map(|line| line.strip_prefix("branch "));
            Some((folder, branch))
        })
        .collect();
    TRUNKS.iter().find_map(|trunk| {
        trees
            .iter()
            .find(|(_, branch)| *branch == Some(*trunk))
            .map(|(folder, _)| PathBuf::from(folder))
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::sprout::tests::Held;

    const HOME: &str = "/home/kestrel";
    const BASIN: &str = "/home/kestrel/basin/trees/root/src";
    const CHECKS_OUT_WREN: &str =
        "git -C /home/kestrel/basin/trees/root/src sparse-checkout add areas/tools/wren";

    /// A home holding `~/src/skiff`, `~/src/example.org/owner/tern` and
    /// a bare repository at `~/basin/git` whose trunk's tree is `BASIN`,
    /// naming the zones `//system/reef` (checked out), `//areas/reef`
    /// and `//areas/tools/wren` (not).
    pub(crate) fn home() -> Held {
        Held::of(
            &[
                "/home/kestrel/src",
                "/home/kestrel/src/skiff",
                "/home/kestrel/src/example.org",
                "/home/kestrel/src/example.org/owner",
                "/home/kestrel/src/example.org/owner/tern",
                "/home/kestrel/basin",
                "/home/kestrel/basin/git",
                "/home/kestrel/.cache",
                "/home/kestrel/.cache/hidden",
                BASIN,
                "/home/kestrel/basin/trees/root/src/system",
                "/home/kestrel/basin/trees/root/src/system/reef",
            ],
            &[
                "/home/kestrel/src/skiff",
                "/home/kestrel/src/example.org/owner/tern",
                BASIN,
            ],
        )
        .bare("/home/kestrel/basin/git", BASIN)
        .bare("/home/kestrel/.cache/hidden", "/home/kestrel/elsewhere")
        .holding(
            "/home/kestrel/basin/trees/root/src/.meta/manifest.json",
            r#"{"//system/reef": {"id": "W-1"}, "//areas/reef": {"id": "W-2"},
                "//areas/tools/wren": {}, "//../etc": {}, "not a zone": {}}"#,
        )
    }

    fn names(places: &[Place]) -> Vec<&str> {
        places.iter().map(|place| place.name.as_str()).collect()
    }

    #[test]
    fn every_checkout_and_every_zone_its_manifest_names_is_a_place() {
        let places = on(Path::new(HOME), &home());
        assert_eq!(
            names(&places),
            [
                "~/basin/trees/root/src",
                "//areas/reef",
                "//areas/tools/wren",
                "//system/reef",
                "~/src/example.org/owner/tern",
                "~/src/skiff",
            ],
            "a hidden folder's bare repository and a path walking out aren't"
        );
        let reef = &places[3];
        assert_eq!(reef.folder, Path::new(BASIN).join("system/reef"));
        assert_eq!(
            (reef.kind, reef.out, &reef.check_out),
            (Kind::Zone, false, &None)
        );
        let wren = &places[2];
        assert!(wren.out, "a zone not checked out says so");
        assert_eq!(
            wren.check_out.as_deref(),
            Some(CHECKS_OUT_WREN),
            "with the command that checks it out"
        );
    }

    #[test]
    fn a_place_is_written_as_the_launcher_reads_it() {
        let places = on(Path::new(HOME), &home());
        assert_eq!(
            serde_json::to_value(&places[2]).expect("it writes"),
            serde_json::json!({
                "folder": "/home/kestrel/basin/trees/root/src/areas/tools/wren",
                "name": "//areas/tools/wren",
                "kind": "zone",
                "out": true,
                "checkOut": CHECKS_OUT_WREN,
            })
        );
        assert_eq!(
            serde_json::to_value(&places[5]).expect("it writes"),
            serde_json::json!({
                "folder": "/home/kestrel/src/skiff",
                "name": "~/src/skiff",
                "kind": "checkout",
                "out": false,
            })
        );
    }

    #[test]
    fn a_name_finds_a_zone_by_its_path_or_its_last_step() {
        let places = on(Path::new(HOME), &home());
        let folder = |name| found(&places, name).map(|place| place.folder.clone());
        assert_eq!(
            folder("//system/reef"),
            Ok(Path::new(BASIN).join("system/reef"))
        );
        assert_eq!(
            folder("wren"),
            Ok(Path::new(BASIN).join("areas/tools/wren"))
        );
        assert_eq!(
            folder("skiff"),
            Ok(PathBuf::from("/home/kestrel/src/skiff"))
        );
        assert_eq!(
            folder("reef"),
            Err(Unfound::Several(vec![
                "//areas/reef".to_owned(),
                "//system/reef".to_owned()
            ])),
            "a last step two zones share names neither"
        );
        assert_eq!(folder("//system/gone"), Err(Unfound::None));
        assert_eq!(folder("gone"), Err(Unfound::None));
    }

    #[test]
    fn a_name_is_a_zones_path_or_one_plain_step() {
        for name in ["//system/reef", "reef", "aquifer-pi", "v2.1"] {
            assert!(is_name(name), "{name}");
        }
        for not in [
            "src/skiff",
            "/home/kestrel",
            "//",
            "//../etc",
            "..",
            ".",
            "~/src",
            "",
        ] {
            assert!(!is_name(not), "{not}");
        }
    }

    #[test]
    fn a_link_into_a_checkout_begins_in_the_deepest_zone_it_lies_in() {
        let places = on(Path::new(HOME), &home());
        let basin = Path::new(BASIN);
        assert_eq!(
            zone_of_link(&places, basin, &["tree", "main", "system", "reef", "src"]),
            basin.join("system/reef")
        );
        assert_eq!(
            zone_of_link(
                &places,
                basin,
                &["blob", "main", "areas", "tools", "wren", "x.rs"]
            ),
            basin.to_owned(),
            "one in a zone not checked out begins at the top"
        );
        assert_eq!(
            zone_of_link(&places, basin, &["pull", "7"]),
            basin.to_owned(),
            "as does one to no path"
        );
    }

    #[test]
    fn a_bare_repositorys_tree_is_the_one_on_its_trunk() {
        let porcelain = "worktree /home/kestrel/basin/git\nbare\n\n\
                         worktree /home/kestrel/basin/trees/fix/src\nHEAD 1\ndetached\n\n\
                         worktree /home/kestrel/basin/trees/old/src\nHEAD 2\n\
                         branch refs/heads/master\n\n\
                         worktree /home/kestrel/basin/trees/root/src\nHEAD 3\n\
                         branch refs/heads/main\n";
        assert_eq!(
            trunk_of(porcelain),
            Some(PathBuf::from("/home/kestrel/basin/trees/root/src"))
        );
        assert_eq!(
            trunk_of("worktree /a\nbare\n\nworktree /b\nHEAD 1\nbranch refs/heads/fix\n"),
            None
        );
    }
}
