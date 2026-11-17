//! The node's answers read for only what the networks make of them.
//!
//! A session record is about 4 KB, almost all of it what the networks
//! never read: its landings twice over, its owner, its tokens by kind. At
//! ten thousand cells the node's answer is about 40 MB, and parsed whole
//! it held over 500 MB before a line was drawn. So each answer is read as
//! it streams in, every field the model doesn't read skipped unparsed,
//! and only what it does read is ever held.

use std::collections::BTreeMap;
use std::fmt;
use std::io;

use serde::Deserialize as _;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

/// What of a JSON value is kept: all of it, or only the fields named,
/// each kept as it says. An array keeps each of its values so.
pub(super) enum Kept {
    /// All of it.
    Whole,
    /// Only these fields, each kept as it says.
    Only(&'static [(&'static str, Kept)]),
}

use Kept::{Only, Whole};

/// What the networks read of a record: a team's, a session's, a
/// machine's or a receptor's fields, as `Mycelium::of`,
/// `Mycelium::sensing` and the swarm's names read them.
pub(super) const RECORD: Kept = Only(&[
    ("address", Whole),
    ("counts", Whole),
    ("doing", Whole),
    ("events", Whole),
    ("glyph", Whole),
    ("groups", Whole),
    ("here", Whole),
    ("id", Whole),
    ("jobs", Whole),
    ("key", Whole),
    ("lead", Whole),
    ("lent", Whole),
    ("lines", Whole),
    ("machine", Whole),
    ("model_name", Whole),
    ("name", Whole),
    ("presence", PRESENCE),
    ("quest", Whole),
    ("room", Only(&[("pressure", Only(&[("cpu", Whole)]))])),
    ("said", Whole),
    ("since", Whole),
    ("source", Whole),
    ("standing", Whole),
    ("state", Whole),
    ("step", Whole),
    ("swarm", Whole),
    ("systems", Whole),
    ("title", Whole),
    ("wants", Whole),
    ("within", Whole),
    ("working_on", Whole),
]);

/// What the networks read of a session's presence.
pub(super) const PRESENCE: Kept = Only(&[
    ("addresses", Whole),
    ("cwd", Whole),
    ("paused", Whole),
    ("questInfo", Only(&[("short", Whole), ("title", Whole)])),
    ("repo", Whole),
    ("state", Whole),
    ("trees", Whole),
    (
        "usage",
        Only(&[
            ("context", Whole),
            ("pi.cost.usd", Only(&[("today", Whole)])),
            (
                "turns",
                Only(&[("last", Only(&[("at", Whole)])), ("today", Whole)]),
            ),
        ]),
    ),
]);

/// The paths of every field `kept` keeps whole, its keys joined by dots,
/// as a list asks a node for only those fields.
pub(super) fn paths(kept: &Kept) -> Vec<String> {
    match kept {
        Whole => Vec::new(),
        Only(fields) => fields
            .iter()
            .flat_map(|(key, under)| match under {
                Whole => vec![(*key).to_owned()],
                Only(_) => paths(under)
                    .into_iter()
                    .map(|path| format!("{key}.{path}"))
                    .collect(),
            })
            .collect(),
    }
}

/// The answers to the asks numbered from one to `many`, read from
/// `reading` as the node sends them: which ask each answers and why it was
/// refused, with each record it lists handed to `each` with its ask's
/// number as it is read, kept as [`RECORD`] says. So an answer's records
/// are never all held unless `each` holds them, and a reader making one
/// small thing of each record holds only those. Any other line, such as
/// the hello's, is read and let go.
///
/// # Errors
///
/// When the node hangs up first, or sends what isn't JSON, or a read
/// takes longer than the socket allows.
pub(super) fn answered(
    mut reading: impl io::BufRead,
    many: u64,
    mut each: impl FnMut(u64, Value),
) -> io::Result<BTreeMap<u64, Value>> {
    let mut answers = BTreeMap::new();
    // A node answers a line each, and a line read whole parses several
    // times faster than serde_json reading the socket a byte at a time.
    let mut line = Vec::new();
    while (1..=many).any(|id| !answers.contains_key(&id)) {
        line.clear();
        if reading.read_until(b'\n', &mut line)? == 0 {
            return Err(io::Error::other("the node hung up before it answered"));
        }
        if line.trim_ascii().is_empty() {
            continue;
        }
        let answering = Answer {
            many,
            each: &mut each,
        };
        let mut json = serde_json::Deserializer::from_slice(&line);
        let answer = answering.deserialize(&mut json)?;
        json.end()?;
        if let Some(id) = answer["id"].as_u64().filter(|id| (1..=many).contains(id)) {
            answers.insert(id, answer);
        }
    }
    Ok(answers)
}

/// `value` kept as `kept` says, as if it had been read so: each field
/// kept looked up, rather than every field walked, since a presence is
/// mostly what isn't.
pub(super) fn pruned(value: &Value, kept: &Kept) -> Value {
    match (kept, value) {
        (Only(fields), Value::Object(object)) => Value::Object(
            fields
                .iter()
                .filter_map(|(name, inner)| {
                    let each = object.get(*name)?;
                    Some(((*name).to_owned(), pruned(each, inner)))
                })
                .collect(),
        ),
        (Only(_), Value::Array(each)) => {
            Value::Array(each.iter().map(|one| pruned(one, kept)).collect())
        }
        (Whole | Only(_), _) => value.clone(),
    }
}

/// An answer read for which ask it answers and why it was refused, each
/// record it lists handed to `each` as it is read.
struct Answer<'a, F> {
    many: u64,
    each: &'a mut F,
}

impl<'de, F: FnMut(u64, Value)> DeserializeSeed<'de> for Answer<'_, F> {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de, F: FnMut(u64, Value)> Visitor<'de> for Answer<'_, F> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("an answer")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut answer = Map::new();
        // A node sends an answer's `id` before its records, but records
        // come before it are held until it does, rather than lost.
        let mut early = Vec::new();
        let many = self.many;
        let asked = |answer: &Map<String, Value>| {
            answer
                .get("id")
                .and_then(Value::as_u64)
                .filter(|id| (1..=many).contains(id))
        };
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "id" | "error" => {
                    let value = map.next_value::<Value>()?;
                    answer.insert(key, value);
                }
                "records" => map.next_value_seed(Records {
                    id: asked(&answer),
                    each: &mut *self.each,
                    early: &mut early,
                })?,
                _ => map.next_value::<IgnoredAny>().map(|_| ())?,
            }
        }
        if let Some(id) = asked(&answer) {
            for record in early {
                (self.each)(id, record);
            }
        }
        Ok(Value::Object(answer))
    }
}

/// An answer's records, each handed to `each` as answering ask `id`, or
/// held in `early` while which ask it answers isn't yet known.
struct Records<'a, F> {
    id: Option<u64>,
    each: &'a mut F,
    early: &'a mut Vec<Value>,
}

impl<'de, F: FnMut(u64, Value)> DeserializeSeed<'de> for Records<'_, F> {
    type Value = ();

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de, F: FnMut(u64, Value)> Visitor<'de> for Records<'_, F> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a list of records")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(record) = seq.next_element_seed(Pruned(&RECORD))? {
            match self.id {
                Some(id) => (self.each)(id, record),
                None => self.early.push(record),
            }
        }
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }
}

/// A JSON value read as a [`Kept`] says.
struct Pruned(&'static Kept);

impl<'de> DeserializeSeed<'de> for Pruned {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        match self.0 {
            Whole => Value::deserialize(deserializer),
            Only(fields) => Fields(fields).deserialize(deserializer),
        }
    }
}

/// A JSON value read for only these fields of each object in it.
#[derive(Clone, Copy)]
struct Fields(&'static [(&'static str, Kept)]);

impl<'de> DeserializeSeed<'de> for Fields {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Fields {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut kept = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            match self.0.iter().find(|(name, _)| *name == key) {
                Some((_, inner)) => {
                    let value = map.next_value_seed(Pruned(inner))?;
                    kept.insert(key, value);
                }
                None => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(Value::Object(kept))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut each = Vec::with_capacity(seq.size_hint().unwrap_or_default());
        while let Some(value) = seq.next_element_seed(self)? {
            each.push(value);
        }
        Ok(Value::Array(each))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_string<E>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }
}
