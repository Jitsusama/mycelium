//! What a carried session's cell brings with it: its name, the ties
//! this node holds of it, and when each tree network its trees make
//! formed here. The ties were made from messages this node held, so
//! they go beside the session's done file for the node it arrives on to
//! keep, as names and counts only, never what was said. A tree network
//! goes so the node it arrives on keeps the time it first formed, rather
//! than forming it afresh.

use serde::Serialize;
use serde_json::Value;

/// What the file beside a carried session's done file ends with, after
/// its session's id.
pub const FILE: &str = ".cell.json";

/// A carried session's cell, as the file beside its done file says it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Brought {
    /// The session's id.
    pub session: String,
    /// The cell's name, which keys it `cell:<cell>`: its session's
    /// address, uppercase by ASCII's rules, as the tie kind names it.
    pub cell: String,
    /// Every tie with the cell at an end, whole as the tie kind sends it,
    /// its strength as heard rather than faded to now.
    pub ties: Vec<Value>,
    /// The cell's own record of its talk, `tie:<cell>`, or none where it
    /// has none.
    pub talk: Option<Value>,
    /// Each `formed:<network>` record this node holds of a tree network
    /// the session's trees make, whole as the formed kind sends it.
    pub formed: Vec<Value>,
}

/// Each of `formed`, every formed network the node holds, that is a
/// tree network the trees `presence` holds make.
#[must_use]
pub fn formed_of(presence: &Value, formed: &[Value]) -> Vec<Value> {
    let networks = crate::networks::tree_networks(presence);
    formed
        .iter()
        .filter(|one| {
            one["key"]
                .as_str()
                .and_then(|key| key.strip_prefix("formed:"))
                .is_some_and(|name| networks.contains(name))
        })
        .cloned()
        .collect()
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
        ties: ties.iter().filter(at_an_end).cloned().collect(),
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

    use super::{brought, formed_of};

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
            [formed[0].clone(), formed[1].clone()],
            "a worktree's and a snapshot's networks, whole; none of another tree, nor one \
             whose name only begins alike, nor of a tree saying neither branch nor commit"
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
