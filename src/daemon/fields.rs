//! A record cut down to the fields a list asked for, so a reader of
//! thousands of records is sent what it reads rather than all of each.
//!
//! A field is named by its path of object keys joined by dots, such as
//! `presence.questInfo.title`. A key may hold dots of its own, as
//! `pi.cost.usd` does, so a path is matched against the keys a record
//! has rather than split up front: a key is kept whole where a path names
//! it, and kept in part where paths run on past it and a dot. Nothing
//! looks into an array, and a path a record lacks is left out rather than
//! sent as null. Every record keeps its `key` and `kind`, so a reader can
//! still tell records apart however few fields it asked for.
//!
//! A record is cut down from the bytes it is held as: only the objects a
//! path runs through are split into their fields, and what is kept is
//! copied as its bytes, never parsed.

use std::borrow::Cow;

use serde_json::Value;
use serde_json::value::RawValue;

use super::records::{Pieces, fields, object};

/// The most fields one list may ask for, so an ask can't make the node
/// match every key of every record against thousands of paths.
pub const MOST_FIELDS: usize = 64;

/// The fields every record keeps, whatever was asked.
const ALWAYS: [&str; 2] = ["key", "kind"];

/// The fields a list asked for, or why they can't be: each a path of
/// object keys joined by dots, at most [`MOST_FIELDS`] of them.
///
/// # Errors
///
/// When `asked` isn't a list of such paths, or names too many.
pub fn read(asked: &Value) -> Result<Vec<String>, String> {
    let paths = asked
        .as_array()
        .ok_or("`fields` is a list of paths, such as [\"presence.repo\"]")?;
    if paths.len() > MOST_FIELDS {
        return Err(format!(
            "a list may ask for at most {MOST_FIELDS} fields, not {}",
            paths.len()
        ));
    }
    paths
        .iter()
        .map(|path| {
            path.as_str()
                .filter(|path| !path.split('.').any(str::is_empty))
                .map(str::to_owned)
                .ok_or_else(|| format!("{path} isn't a path of keys joined by dots"))
        })
        .collect()
}

/// The record whose bytes are `record` with only the fields of `paths`,
/// and its `key` and `kind`, as the bytes it is sent as.
#[must_use]
pub fn projected(record: &str, paths: &[String]) -> Box<RawValue> {
    let mut asked: Vec<&str> = paths.iter().map(String::as_str).collect();
    asked.extend(ALWAYS);
    let kept = kept(record, &asked).unwrap_or_else(|| "{}".to_owned());
    // What is kept was read as JSON, so it is JSON written again.
    RawValue::from_string(kept).unwrap_or_else(|_| RawValue::NULL.to_owned())
}

/// What of the value whose bytes are `json` the `paths` under it name, as
/// its bytes, or none where they name nothing it holds, an object of
/// nothing included.
fn kept(json: &str, paths: &[&str]) -> Option<String> {
    let fields = fields(json)?;
    let kept: Pieces<'_, '_> = fields
        .iter()
        .filter_map(|(key, value)| {
            if paths.contains(&key.as_str()) {
                return Some((key.as_str(), Cow::Borrowed(value.get())));
            }
            let under: Vec<&str> = paths
                .iter()
                .filter_map(|path| path.strip_prefix(key.as_str())?.strip_prefix('.'))
                .collect();
            if under.is_empty() {
                return None;
            }
            Some((key.as_str(), Cow::Owned(kept(value.get(), &under)?)))
        })
        .collect();
    (!kept.is_empty()).then(|| object(&kept))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{MOST_FIELDS, read};

    /// `record` projected to `asked`, as the bytes a list sends, which
    /// are the bytes the value it means is written as.
    fn projected(record: &serde_json::Value, asked: &[String]) -> serde_json::Value {
        let sent = super::projected(&record.to_string(), asked);
        let value: serde_json::Value =
            serde_json::from_str(sent.get()).expect("a projected record is JSON");
        assert_eq!(
            sent.get(),
            value.to_string(),
            "sent as its value is written"
        );
        value
    }

    /// A session record, much as the node holds one.
    fn session() -> serde_json::Value {
        json!({
            "key": "session:a",
            "kind": "session",
            "address": "AB12CD",
            "landings": [{"ref": "world#1"}],
            "groups": ["repo.world"],
            "presence": {
                "questInfo": {"short": "AB12CD", "title": "A quest"},
                "owner": {"pid": 1},
                "usage": {"pi.cost.usd": {"last": 0.1, "today": 2.5}},
            },
        })
    }

    #[test]
    fn a_projected_record_holds_only_the_paths_asked_and_its_key_and_kind() {
        let asked = [
            "address",
            "groups",
            "presence.questInfo.title",
            "presence.usage.pi.cost.usd.today",
        ]
        .map(str::to_owned);
        assert_eq!(
            projected(&session(), &asked),
            json!({
                "key": "session:a",
                "kind": "session",
                "address": "AB12CD",
                "groups": ["repo.world"],
                "presence": {
                    "questInfo": {"title": "A quest"},
                    "usage": {"pi.cost.usd": {"today": 2.5}},
                },
            }),
            "a key holding dots of its own is reached through them"
        );
    }

    #[test]
    fn a_path_a_record_lacks_is_left_out_and_nothing_looks_into_an_array() {
        let asked = ["machine", "presence.repo", "groups.0", "address.x"].map(str::to_owned);
        assert_eq!(
            projected(&session(), &asked),
            json!({"key": "session:a", "kind": "session"}),
            "neither null nor an empty object stands in for what isn't there"
        );
    }

    #[test]
    fn a_key_or_field_written_with_escapes_is_kept_as_its_value_is_written() {
        let record = json!({
            "key": "session:\"a\"",
            "kind": "session",
            "say \"hi\"": {"tab\there": "line\none", "left": 1},
            "dropped": "\u{1F980}",
        });
        let asked = ["say \"hi\".tab\there".to_owned()];
        assert_eq!(
            projected(&record, &asked),
            json!({
                "key": "session:\"a\"",
                "kind": "session",
                "say \"hi\"": {"tab\there": "line\none"},
            }),
            "a key is matched as it reads, not as it is escaped"
        );
    }

    #[test]
    fn fields_are_paths_of_keys_and_too_many_are_refused() {
        assert_eq!(
            read(&json!(["address", "presence.questInfo.title"])),
            Ok(vec![
                "address".to_owned(),
                "presence.questInfo.title".to_owned()
            ])
        );
        for wrong in [
            json!("address"),
            json!([1]),
            json!([""]),
            json!(["presence..repo"]),
            json!([".address"]),
        ] {
            assert!(read(&wrong).is_err(), "{wrong} isn't a list of paths");
        }
        let many: Vec<String> = (0..=MOST_FIELDS).map(|n| format!("f{n}")).collect();
        assert_eq!(
            read(&json!(many)),
            Err(format!(
                "a list may ask for at most {MOST_FIELDS} fields, not {}",
                MOST_FIELDS + 1
            ))
        );
    }
}
