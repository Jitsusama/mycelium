//! What a carried session's cell brings with it: its name, the ties
//! this node holds of it, and when each network its trees and shared
//! folders make formed here. The ties were made from messages this node
//! held, so they go beside the session's done file for the node it
//! arrives on to keep, as names and counts only, never what was said. A
//! formed network goes so the node it arrives on keeps the time it
//! first formed, rather than forming it afresh.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What the file beside a carried session's done file ends with, after
/// its session's id.
pub const FILE: &str = ".cell.json";

/// The most formed networks a cell brings, those that formed earliest,
/// so its file stays small however many trees its session holds: one
/// left out forms afresh where the session arrives.
pub const FORMED_MOST: usize = 64;

/// The most ties a cell brings, its strongest, as the node it leaves
/// writes them and the node it arrives on reads them: so a file its
/// session wrote can't fill the node it arrives on with ties, and a tie
/// left out forms afresh as the cell talks there.
pub const TIES_MOST: usize = 128;

/// The most a cell's file read where its session arrives may hold, in
/// bytes: far past what a cell brings, so only a file grown by
/// something else is passed over.
pub const FILE_MOST: u64 = 1 << 20;

/// The longest name a cell may be read with where its session arrives,
/// in bytes, past every address the mesh gives.
const NAME_MOST: usize = 32;

/// A carried session's cell, as the file beside its done file says it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Brought {
    /// The session's id.
    pub session: String,
    /// The cell's name, which keys it `cell:<cell>`: its session's
    /// address, uppercase by ASCII's rules, as the tie kind names it.
    pub cell: String,
    /// Every tie with the cell at an end, whole as the tie kind sends it,
    /// its strength as heard rather than faded to now.
    #[serde(default)]
    pub ties: Vec<Value>,
    /// The cell's own record of its talk, `tie:<cell>`, or none where it
    /// has none.
    #[serde(default)]
    pub talk: Option<Value>,
    /// The `formed:<network>` records this node holds of the networks
    /// the session's trees and shared folders make, whole as the formed
    /// kind sends them: at most [`FORMED_MOST`], earliest formed first.
    #[serde(default)]
    pub formed: Vec<Value>,
}

/// The cell the file at `path` says the session `session` brought, as
/// the node it arrived on reads it, or none when it says another
/// session's, names no cell a tie could, or isn't a cell's file. The
/// handover is its sessions' to write, so the file is read never
/// through a link and at most [`FILE_MOST`] bytes of it, and only what
/// a cell could bring of it is kept: each tie with the cell at an end,
/// its ends in the order the tie kind keys them, the cell's own talk,
/// each with only the fields the tie kind says, and records of formed
/// networks, at most [`FORMED_MOST`].
#[must_use]
pub fn arrived(path: &Path, session: &str) -> Option<Brought> {
    let bytes = crate::forage::read_at_most(path, FILE_MOST)?;
    let said: Brought = serde_json::from_slice(&bytes).ok()?;
    if said.session != session || !named(&said.cell) {
        return None;
    }
    let cell = said.cell.as_str();
    let ties = said
        .ties
        .iter()
        .filter(|tie| {
            tie_keyed(tie).is_some_and(|key| {
                key.split_once('~').is_some_and(|(one, other)| {
                    (one == cell || other == cell) && one < other && named(one) && named(other)
                })
            })
        })
        .map(|tie| tie_fields(Shape::Pair, tie))
        .collect();
    let ties = strongest(ties);
    let talk = said
        .talk
        .as_ref()
        .filter(|talk| tie_keyed(talk).is_some_and(|key| key == cell))
        .map(|talk| tie_fields(Shape::Talk, talk));
    let mut formed: Vec<Value> = said
        .formed
        .into_iter()
        .filter(|one| keyed(one, "formed:").is_some_and(|name| !name.is_empty()))
        .collect();
    formed.truncate(FORMED_MOST);
    Some(Brought {
        session: said.session,
        cell: said.cell,
        ties,
        talk,
        formed,
    })
}

/// Whether `cell` is a name a tie's key could hold at an end: as the
/// mesh's addresses are, uppercase, and never the `~` that joins a
/// pair.
fn named(cell: &str) -> bool {
    !cell.is_empty()
        && cell.len() <= NAME_MOST
        && cell
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || b"-_".contains(&byte))
}

/// The most groups a brought tie may say its messages went to, far past
/// what a day's talk between two cells reaches, so no file fills a tie
/// with names.
const GROUPS_MOST: usize = 64;

/// The most intents a brought tie may say, one for each the mesh has.
const INTENTS_MOST: usize = 5;

/// The strength `tie` says it was heard at, where it says one a tie
/// could have: a number, never below none.
#[must_use]
pub fn strength(tie: &Value) -> Option<f64> {
    a_strength(tie.get("strength")?)
}

/// `value` as a strength a tie could have: a number, never below none.
fn a_strength(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|strength| strength.is_finite() && *strength >= 0.0)
}

/// Which of the tie kind's two records a brought one is: a tie between
/// two cells, or a cell's own talk.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// A tie between two cells: each end's counts in a pair, its intents,
    /// its groups and its strength.
    Pair,
    /// A cell's own talk: its counts, and how many ties it has.
    Talk,
}

/// `value` as the tie kind writes its field `field` in a record of
/// `shape`, or none where it reads as nothing the tie kind writes
/// there: so a cell's file brings no other field under the tie kind's
/// name, nor a field of the other record's, nor one holding what no tie
/// could, and a list keeps only the entries that read, each once, at
/// most as many as a tie could say.
fn typed(shape: Shape, field: &str, value: &Value) -> Option<Value> {
    let count = |value: &Value| {
        value
            .as_u64()
            .filter(|count| u32::try_from(*count).is_ok())
            .map(Value::from)
    };
    match (shape, field) {
        (_, "key" | "kind") => value.is_string().then(|| value.clone()),
        (_, "heard") => value.as_i64().map(Value::from),
        (Shape::Pair, "strength") => a_strength(value).map(Value::from),
        (Shape::Pair, "hour" | "today") => match value.as_array()?.as_slice() {
            [one, other] => Some(Value::from(vec![count(one)?, count(other)?])),
            _ => None,
        },
        (Shape::Pair, "intents") => Some(listed(value, INTENTS_MOST, |one| {
            serde_json::from_value::<crate::record::Intent>(one.clone()).is_ok()
        })),
        (Shape::Pair, "groups") => Some(listed(value, GROUPS_MOST, |one| {
            one.as_str()
                .is_some_and(|name| crate::group::Group::new(name).is_ok())
        })),
        (Shape::Talk, "hour" | "today" | "ties") => count(value),
        _ => None,
    }
}

/// The entries of `value`, a list, that `reads` takes, each once, at
/// most `most`: none where it is no list.
fn listed(value: &Value, most: usize, reads: impl Fn(&Value) -> bool) -> Value {
    let mut kept: Vec<Value> = Vec::new();
    for one in value.as_array().into_iter().flatten() {
        if kept.len() < most && reads(one) && !kept.contains(one) {
            kept.push(one.clone());
        }
    }
    Value::from(kept)
}

/// `ties`, strongest first, those as strong by key, one saying no
/// strength a tie could have as none, and at most [`TIES_MOST`].
fn strongest(mut ties: Vec<Value>) -> Vec<Value> {
    let order = |tie: &Value| {
        (
            strength(tie).unwrap_or_default(),
            tie["key"].as_str().map(str::to_owned),
        )
    };
    ties.sort_by(|one, other| {
        let ((one_strength, one_key), (other_strength, other_key)) = (order(one), order(other));
        other_strength
            .total_cmp(&one_strength)
            .then_with(|| one_key.cmp(&other_key))
    });
    ties.truncate(TIES_MOST);
    ties
}

/// What follows `tie:` in the key of `record`, where it is of the tie
/// kind, or none.
fn tie_keyed(record: &Value) -> Option<String> {
    (record.get("kind")? == "tie").then_some(())?;
    keyed(record, "tie:")
}

/// `record`, of `shape`, with only the fields the tie kind writes
/// there, each as it writes it, as [`typed`] reads them.
fn tie_fields(shape: Shape, record: &Value) -> Value {
    let fields = record.as_object().into_iter().flatten();
    Value::Object(
        fields
            .filter_map(|(field, value)| Some((field.clone(), typed(shape, field, value)?)))
            .collect(),
    )
}

/// What follows `prefix` in the key of `record`, an object, or none.
fn keyed(record: &Value, prefix: &str) -> Option<String> {
    record
        .as_object()?
        .get("key")?
        .as_str()?
        .strip_prefix(prefix)
        .map(str::to_owned)
}

/// Each of `formed`, every formed network the node holds, that is a
/// network the trees or shares `presence` holds make: earliest formed first,
/// those at one time by key, one saying no time last, and at most
/// [`FORMED_MOST`].
#[must_use]
pub fn formed_of(presence: &Value, formed: &[Value]) -> Vec<Value> {
    let networks = crate::networks::formed_networks(presence);
    let mut kept: Vec<Value> = formed
        .iter()
        .filter(|one| {
            one["key"]
                .as_str()
                .and_then(|key| key.strip_prefix("formed:"))
                .is_some_and(|name| networks.contains(name))
        })
        .cloned()
        .collect();
    kept.sort_by(|one, other| {
        let order = |record: &Value| {
            (
                record["at"].as_i64().unwrap_or(i64::MAX),
                record["key"].as_str().unwrap_or_default().to_owned(),
            )
        };
        order(one).cmp(&order(other))
    });
    kept.truncate(FORMED_MOST);
    kept
}

/// What the cell of the session `session`, going by `address`, brings
/// of `ties`, every tie record the node holds. An address holding the
/// `~` a pair's key joins its two cells with can't be told from a pair,
/// so its cell brings none.
#[must_use]
pub fn brought(session: &str, address: &str, ties: &[Value]) -> Brought {
    let cell = address.to_ascii_uppercase();
    let ties = if cell.contains('~') { &[] } else { ties };
    let key = |tie: &Value| {
        tie.get("key")
            .and_then(Value::as_str)
            .and_then(|key| key.strip_prefix("tie:"))
            .map(str::to_owned)
    };
    let at_an_end = |tie: &&Value| {
        key(tie).is_some_and(|key| {
            key.split_once('~')
                .is_some_and(|(one, other)| one == cell || other == cell)
        })
    };
    Brought {
        session: session.to_owned(),
        ties: strongest(ties.iter().filter(at_an_end).cloned().collect()),
        talk: ties
            .iter()
            .find(|tie| key(tie).is_some_and(|key| key == cell))
            .cloned(),
        cell,
        formed: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use std::fs;
    use std::path::Path;

    use super::{FILE_MOST, FORMED_MOST, TIES_MOST, arrived, brought, formed_of};

    /// `said`, written whole at `name` in `folder`.
    fn file(folder: &Path, name: &str, said: &serde_json::Value) -> std::path::PathBuf {
        let path = folder.join(name);
        fs::write(&path, serde_json::to_vec(said).expect("said")).expect("written");
        path
    }

    #[test]
    fn a_cell_arrives_with_what_it_could_bring_and_nothing_else() {
        let folder = tempfile::tempdir().expect("a handover");
        let path = file(
            folder.path(),
            "s-harbour.cell.json",
            &json!({
                "session": "s-harbour",
                "cell": "HARBOR",
                "ties": [
                    {"key": "tie:HARBOR~KELP01", "kind": "tie", "strength": 2.0,
                     "said": "what was said"},
                    {"key": "tie:AAAAAA~HARBOR", "kind": "tie"},
                    {"key": "tie:KELP01~REEF01", "kind": "tie"},
                    {"key": "tie:KELP01~HARBOR", "kind": "tie"},
                    {"key": "tie:HARBOR~REEF01", "kind": "cell"},
                    {"key": "tie:HARBOR~REEF02"},
                    {"key": "tie:HARBOR~kelp01", "kind": "tie"},
                    {"key": "tie:HARBOR~A~B", "kind": "tie"},
                    {"key": "cell:HARBOR~KELP01", "kind": "tie"},
                    "tie:HARBOR~KELP01",
                ],
                "talk": {"key": "tie:HARBOR", "kind": "tie", "ties": 2},
                "formed": [
                    {"key": TIDES, "kind": "formed", "at": 1},
                    {"key": "formed:", "kind": "formed"},
                    {"key": "tie:HARBOR~KELP01", "kind": "tie"},
                ],
                "else": "passed over",
            }),
        );
        let cell = arrived(&path, "s-harbour").expect("its cell");
        assert_eq!(cell.cell, "HARBOR");
        assert_eq!(
            cell.ties,
            [
                json!({"key": "tie:HARBOR~KELP01", "kind": "tie", "strength": 2.0}),
                json!({"key": "tie:AAAAAA~HARBOR", "kind": "tie"}),
            ],
            "only ties with it at an end between two cells' names, in order, and only \
             the tie kind's fields; never another pair's, one out of order, another \
             kind's, one saying no kind, one in lowercase, one of three, one keyed as \
             another kind's or one that isn't a record"
        );
        assert_eq!(
            cell.talk,
            Some(json!({"key": "tie:HARBOR", "kind": "tie", "ties": 2}))
        );
        assert_eq!(
            cell.formed,
            [json!({"key": TIDES, "kind": "formed", "at": 1})],
            "only records of a formed network that names one"
        );
        assert_eq!(
            arrived(&path, "s-kelp01"),
            None,
            "a file saying another session's cell is passed over"
        );
        let quiet = file(
            folder.path(),
            "s-quiet.cell.json",
            &json!({"session": "s-quiet", "cell": "QUIET9",
                    "talk": {"key": "tie:KELP01", "kind": "tie"}}),
        );
        let cell = arrived(&quiet, "s-quiet").expect("a cell bringing nothing");
        assert_eq!(
            (cell.ties.len(), cell.talk, cell.formed.len()),
            (0, None, 0),
            "no ties or formed networks said is none; another's talk isn't its own"
        );
    }

    #[test]
    fn a_cell_brings_each_field_only_as_the_tie_kind_writes_it() {
        let folder = tempfile::tempdir().expect("a handover");
        let groups: Vec<String> = (0..70).map(|one| format!("crew.{one}")).collect();
        let path = file(
            folder.path(),
            "s-harbour.cell.json",
            &json!({
                "session": "s-harbour",
                "cell": "HARBOR",
                "ties": [
                    {"key": "tie:HARBOR~KELP01", "kind": "tie", "heard": 5,
                     "hour": [1, 2], "today": [3, 4], "strength": 1.5,
                     "intents": ["fyi", "question", "fyi", "shout", 7],
                     "groups": ["crew", "Crew", "a..b", "crew"]},
                    {"key": "tie:HARBOR~REEF01", "kind": "tie", "heard": "soon",
                     "hour": [1, 2, 3], "today": [-1, 4], "strength": -2.0,
                     "intents": "fyi", "groups": groups},
                    {"key": "tie:HARBOR~TIDE01", "kind": "tie", "heard": 5,
                     "hour": 2, "today": [3], "ties": 3, "strength": 0.0},
                ],
                "talk": {"key": "tie:HARBOR", "kind": "tie", "hour": 2,
                         "today": 4_294_967_296_u64, "ties": [2], "strength": 9.0,
                         "intents": ["fyi"], "groups": ["crew"]},
            }),
        );
        let cell = arrived(&path, "s-harbour").expect("its cell");
        assert_eq!(
            cell.ties[0],
            json!({"key": "tie:HARBOR~KELP01", "kind": "tie", "heard": 5,
                   "hour": [1, 2], "today": [3, 4], "strength": 1.5,
                   "intents": ["fyi", "question"], "groups": ["crew"]}),
            "each intent and group a tie could say, once"
        );
        assert_eq!(
            cell.ties[1]["groups"].as_array().map(Vec::len),
            Some(super::GROUPS_MOST),
            "at most as many groups as a tie could say"
        );
        assert_eq!(
            (
                cell.ties[1].get("heard"),
                cell.ties[1].get("hour"),
                cell.ties[1].get("today"),
                cell.ties[1].get("strength"),
                &cell.ties[1]["intents"],
            ),
            (None, None, None, None, &json!([])),
            "a field holding what no tie could is passed over alone"
        );
        assert_eq!(
            cell.ties[2],
            json!({"key": "tie:HARBOR~TIDE01", "kind": "tie", "heard": 5,
                   "strength": 0.0}),
            "a tie brings no field only a cell's talk has, nor one count for a pair"
        );
        assert_eq!(
            cell.talk,
            Some(json!({"key": "tie:HARBOR", "kind": "tie", "hour": 2})),
            "a talk's counts only as counts a cell could have, and none of a tie's fields"
        );
    }

    #[test]
    fn a_cell_naming_what_no_tie_could_is_passed_over() {
        let folder = tempfile::tempdir().expect("a handover");
        for cell in [
            "",
            "harbor",
            "HARBOR~KELP01",
            "HAR BOR",
            &"A".repeat(33),
            "HÄRBOR",
        ] {
            let path = file(
                folder.path(),
                "s.cell.json",
                &json!({"session": "s", "cell": cell}),
            );
            assert_eq!(arrived(&path, "s"), None, "{cell:?} names no cell");
        }
        let path = file(
            folder.path(),
            "s.cell.json",
            &json!({"session": "s", "cell": "A".repeat(32)}),
        );
        assert!(arrived(&path, "s").is_some(), "a name at the bound is kept");
    }

    #[test]
    fn a_cells_file_is_read_only_as_a_small_file_of_its_own() {
        let folder = tempfile::tempdir().expect("a handover");
        let said = json!({"session": "s", "cell": "HARBOR"});
        let real = file(folder.path(), "real.json", &said);
        let link = folder.path().join("s.cell.json");
        std::os::unix::fs::symlink(&real, &link).expect("linked");
        assert!(arrived(&real, "s").is_some(), "the file itself is read");
        assert_eq!(arrived(&link, "s"), None, "never through a link");
        let big = folder.path().join("big.cell.json");
        let mut bytes = serde_json::to_vec(&said).expect("said");
        bytes.resize(usize::try_from(FILE_MOST).expect("fits") + 1, b' ');
        fs::write(&big, bytes).expect("written");
        assert_eq!(arrived(&big, "s"), None, "nor one past its bound");
        let torn = folder.path().join("torn.cell.json");
        fs::write(&torn, b"{\"session\": \"s\", \"cell").expect("written");
        assert_eq!(arrived(&torn, "s"), None, "nor one that isn't a cell's");
        assert_eq!(arrived(&folder.path().join("gone"), "s"), None);
    }

    const TIDES: &str = "formed:tree.github:harbour/oars:jsama/tides";

    #[test]
    fn a_cell_brings_the_formed_networks_its_sessions_trees_make() {
        let presence = json!({"trees": [
            {"key": "oars-tides", "repo": "github:harbour/oars", "branch": "jsama/tides"},
            {"key": "oars-pinned", "repo": "github:harbour/oars", "commit": "6b88106c"},
            {"key": "oars-nowhere", "repo": "github:harbour/oars"},
        ]});
        // As the formed kind sends them: keyed by name, with when it
        // formed and when it was left.
        let record = |key: &str, at: i64| {
            let name = key.strip_prefix("formed:").unwrap_or(key);
            json!({"key": key, "kind": "formed", "name": name, "at": at, "left": null})
        };
        let formed = [
            record(TIDES, 1_763_300_000_000),
            json!({"key": "formed:tree.github:harbour/oars:6b88106c", "kind": "formed",
                   "name": "tree.github:harbour/oars:6b88106c", "at": 2, "left": 9}),
            record("formed:tree.github:harbour/sails:jsama/tides", 3),
            record("formed:tree.github:harbour/oars:jsama/tidesX", 4),
            record("formed:tree.github:harbour/oars", 5),
            json!({"kind": "formed", "name": "tree.github:harbour/oars:jsama/tides", "at": 6}),
        ];
        assert_eq!(
            formed_of(&presence, &formed),
            [formed[1].clone(), formed[0].clone()],
            "a worktree's and a snapshot's networks, whole and earliest first; none of \
             another tree, nor one whose name only begins alike, nor of a tree saying \
             neither branch nor commit"
        );
        assert!(
            formed_of(&json!({}), &formed).is_empty(),
            "a session holding no tree brings none"
        );
        assert!(
            formed_of(&json!({"trees": "oars"}), &formed).is_empty(),
            "nor one whose trees aren't a list"
        );
    }

    #[test]
    fn a_cell_brings_the_formed_networks_of_the_folders_it_shares_too() {
        let presence = json!({
            "trees": [{"key": "oars-tides", "repo": "github:harbour/oars",
                        "branch": "jsama/tides"}],
            "shares": [
                {"key": "quest:QEST-20261112-HARBOR", "for": "Harbour", "folder": "/h"},
                {"key": "quest:a.b", "for": "Dotted", "folder": "/d"},
            ],
        });
        let record = |key: &str, at: i64| json!({"key": key, "kind": "formed", "at": at});
        let formed = [
            record("formed:files.quest:qest-20261112-harbor", 2),
            record(TIDES, 1),
            record("formed:files.quest:a.b", 3),
            record("formed:files.quest:qest-20261112-kelped", 4),
        ];
        assert_eq!(
            formed_of(&presence, &formed),
            [formed[1].clone(), formed[0].clone()],
            "its trees' and its shared folders' networks, earliest first; none of a share \
             past its bounds, nor of a folder it doesn't share"
        );
    }

    #[test]
    fn a_cell_brings_at_most_its_earliest_formed_networks() {
        let branches: Vec<String> = (0..FORMED_MOST + 6).map(|n| format!("b{n}")).collect();
        let trees: Vec<_> = branches
            .iter()
            .map(|branch| json!({"key": branch, "repo": "github:harbour/oars", "branch": branch}))
            .collect();
        let presence = json!({"trees": trees});
        // The first formed latest, so order kept as given would keep
        // the wrong ones.
        let formed: Vec<_> = branches
            .iter()
            .zip((0_i64..).map(|n| 1_000 - n))
            .map(|(branch, at)| {
                let name = format!("tree.github:harbour/oars:{branch}");
                json!({"key": format!("formed:{name}"), "kind": "formed",
                       "name": name, "at": at, "left": null})
            })
            .collect();
        let kept = formed_of(&presence, &formed);
        assert_eq!(kept.len(), FORMED_MOST, "at most {FORMED_MOST}");
        assert!(
            formed[..6].iter().all(|latest| !kept.contains(latest)),
            "the six that formed latest are left out"
        );
        let mut ats: Vec<i64> = kept.iter().filter_map(|one| one["at"].as_i64()).collect();
        let unsorted = ats.clone();
        ats.sort_unstable();
        assert_eq!(unsorted, ats, "earliest first");
    }

    #[test]
    fn a_cell_brings_and_arrives_with_at_most_its_strongest_ties() {
        let ties: Vec<serde_json::Value> = (0..500)
            .map(|n| {
                // The strongest past the first 128 written, so cutting
                // before choosing loses it.
                let strength = if n == 407 { 2e9 } else { 1e9 };
                json!({"key": format!("tie:C{n:05}~HARBOR"), "kind": "tie",
                       "strength": strength})
            })
            .collect();
        let leaving = brought("s-harbour", "harbor", &ties);
        assert_eq!(
            leaving.ties.len(),
            TIES_MOST,
            "it brings at most {TIES_MOST}"
        );
        assert_eq!(
            leaving.ties[0]["key"], "tie:C00407~HARBOR",
            "strongest first"
        );

        let folder = tempfile::tempdir().expect("a handover");
        let path = file(
            folder.path(),
            "s-harbour.cell.json",
            &json!({"session": "s-harbour", "cell": "HARBOR", "ties": ties}),
        );
        let arriving = arrived(&path, "s-harbour").expect("its cell reads");
        assert_eq!(
            arriving.ties.len(),
            TIES_MOST,
            "and a file saying more arrives with at most {TIES_MOST}"
        );
        assert_eq!(
            arriving.ties[0]["key"], "tie:C00407~HARBOR",
            "its strongest"
        );
    }

    #[test]
    fn formed_networks_at_one_time_go_by_name_and_one_saying_no_time_goes_last() {
        let trees: Vec<_> = ["a", "b", "c", "d"]
            .iter()
            .map(|branch| json!({"key": branch, "repo": "github:harbour/oars", "branch": branch}))
            .collect();
        let presence = json!({"trees": trees});
        let key = |branch: &str| format!("formed:tree.github:harbour/oars:{branch}");
        let formed = [
            json!({"key": key("d"), "kind": "formed", "at": "soon"}),
            json!({"key": key("c"), "kind": "formed", "at": 5}),
            json!({"key": key("a"), "kind": "formed", "at": 5}),
            json!({"key": key("b"), "kind": "formed", "at": 9}),
        ];
        let kept: Vec<_> = formed_of(&presence, &formed)
            .iter()
            .filter_map(|one| one["key"].as_str().map(str::to_owned))
            .collect();
        assert_eq!(
            kept,
            [key("a"), key("c"), key("b"), key("d")],
            "two at one time by name, whatever order the node gave them; one whose time \
             isn't a whole number last"
        );
    }

    #[test]
    fn a_cell_brings_each_tie_at_its_ends_and_its_own_talk() {
        let ties = [
            json!({"key": "tie:BUSY01~KESTRL", "kind": "tie", "strength": 2.5}),
            json!({"key": "tie:AAAAAA~BUSY01", "kind": "tie", "strength": 1.0}),
            json!({"key": "tie:BUSY01X~OTHER4", "kind": "tie"}),
            json!({"key": "tie:OTHER4~ZBUSY01", "kind": "tie"}),
            json!({"key": "tie:BUSY01", "kind": "tie", "ties": 2}),
            json!({"key": "tie:OTHER4", "kind": "tie", "ties": 1}),
        ];
        let cell = brought("s-busy01", "busy01", &ties);
        assert_eq!(cell.session, "s-busy01", "its session");
        assert_eq!(
            cell.cell, "BUSY01",
            "named uppercase, as the tie kind names it"
        );
        assert_eq!(
            cell.ties,
            [ties[0].clone(), ties[1].clone()],
            "each tie with it at an end, whole; none whose end only looks like it"
        );
        assert_eq!(cell.talk, Some(ties[4].clone()), "its own talk");
        assert_eq!(
            brought("s-quiet9", "QUIET9", &ties).talk,
            None,
            "a cell that never talked has no talk to bring"
        );
        let pair = brought("s-pair01", "busy01~kestrl", &ties);
        assert_eq!(
            (pair.ties.len(), pair.talk),
            (0, None),
            "an address holding a pair's `~` brings no pair's tie as its own"
        );
    }

    #[test]
    fn a_cells_file_says_only_its_five_fields() {
        let mut cell = brought(
            "s-busy01",
            "BUSY01",
            &[json!({"key": "tie:BUSY01", "kind": "tie", "ties": 0})],
        );
        cell.formed = vec![json!({"key": TIDES, "kind": "formed", "at": 1})];
        assert_eq!(
            serde_json::to_value(&cell).expect("said"),
            json!({
                "session": "s-busy01",
                "cell": "BUSY01",
                "ties": [],
                "talk": {"key": "tie:BUSY01", "kind": "tie", "ties": 0},
                "formed": [{"key": TIDES, "kind": "formed", "at": 1}],
            }),
            "the fields the node it arrives on reads"
        );
    }
}
