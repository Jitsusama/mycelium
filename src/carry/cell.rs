//! What a carried session's cell brings with it: its name and the ties
//! this node holds of it. The ties were made from messages this node
//! held, so they go beside the session's done file for the node it
//! arrives on to keep, as names and counts only, never what was said.

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
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::brought;

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
    fn a_cells_file_says_only_its_four_fields() {
        let cell = brought(
            "s-busy01",
            "BUSY01",
            &[json!({"key": "tie:BUSY01", "kind": "tie", "ties": 0})],
        );
        assert_eq!(
            serde_json::to_value(&cell).expect("said"),
            json!({
                "session": "s-busy01",
                "cell": "BUSY01",
                "ties": [],
                "talk": {"key": "tie:BUSY01", "kind": "tie", "ties": 0},
            }),
            "the fields the node it arrives on reads"
        );
    }
}
