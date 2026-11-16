//! An object's fields found in the bytes it is written as, and an object
//! written again from pieces of such bytes, neither parsed.
//!
//! A record is held as the bytes `serde_json` wrote it as, so its fields
//! can be told apart without being read: a field's name runs between two
//! quotes, and its value to the comma or brace closing it at its own
//! depth, strings skipped whole. Splitting the ten thousand sessions of a
//! deep watch this way, and the presence inside each, took 52 ms where
//! parsing them took 124 to 130 (17 November), since a parse checks every
//! byte it reads where a split only looks for quotes, eight bytes at a
//! time.
//!
//! Only JSON as `serde_json` writes a value is split: compact, its
//! object's keys sorted and each once. Anything else isn't, and is read
//! as no object at all, so whoever splits falls back to the record whole
//! rather than reading it wrong.

use std::borrow::Cow;

/// One field of an object, as the object's bytes hold it.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Field<'a> {
    /// Its name, escapes read.
    pub name: Cow<'a, str>,
    /// Its name as written between its quotes, escapes and all.
    pub written: &'a str,
    /// Its value as written.
    pub value: &'a str,
}

/// A field of an object to be written: its name, the name as written
/// between its quotes, and its value as written.
#[derive(Debug)]
pub(super) struct Piece<'f, 'v> {
    /// Its name, escapes read.
    pub name: &'f str,
    /// Its name as written between its quotes.
    pub written: &'f str,
    /// Its value as written.
    pub value: Cow<'v, str>,
}

impl<'f, 'v> Piece<'f, 'v> {
    /// `field` as it was, with its value `value`.
    pub fn of(field: &'f Field<'v>, value: Cow<'v, str>) -> Self {
        Self {
            name: &field.name,
            written: field.written,
            value,
        }
    }
}

/// The fields of the object `json` is, in the order it holds them, which
/// is the order of their names; none where it is no object as
/// `serde_json` writes one.
pub(super) fn fields(json: &str) -> Option<Vec<Field<'_>>> {
    let bytes = json.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut fields: Vec<Field<'_>> = Vec::new();
    if json == "{}" {
        return Some(fields);
    }
    let mut at = 1;
    loop {
        if bytes.get(at) != Some(&b'"') {
            return None;
        }
        let name_end = string_end(bytes, at)?;
        let written = json.get(at + 1..name_end - 1)?;
        let name = if written.contains('\\') {
            Cow::Owned(serde_json::from_str(json.get(at..name_end)?).ok()?)
        } else {
            Cow::Borrowed(written)
        };
        if fields.last().is_some_and(|last| last.name >= name) {
            return None;
        }
        if bytes.get(name_end) != Some(&b':') {
            return None;
        }
        let value_end = value_end(bytes, name_end + 1)?;
        fields.push(Field {
            name,
            written,
            value: json.get(name_end + 1..value_end)?,
        });
        match bytes.get(value_end) {
            Some(b',') => at = value_end + 1,
            Some(b'}') if value_end + 1 == bytes.len() => return Some(fields),
            _ => return None,
        }
    }
}

/// `pieces` as one object's bytes, in their order.
pub(super) fn object(pieces: &[Piece<'_, '_>]) -> String {
    let size: usize = pieces
        .iter()
        .map(|piece| piece.written.len() + piece.value.len() + 4)
        .sum();
    let mut written = String::with_capacity(size + 2);
    written.push('{');
    for (at, piece) in pieces.iter().enumerate() {
        if at > 0 {
            written.push(',');
        }
        written.push('"');
        written.push_str(piece.written);
        written.push_str("\":");
        written.push_str(&piece.value);
    }
    written.push('}');
    written
}

/// Where the string opening at `at` in `bytes` ends, just past its
/// closing quote.
fn string_end(bytes: &[u8], at: usize) -> Option<usize> {
    let mut at = at + 1;
    loop {
        at = quote_or_escape(bytes, at)?;
        if bytes[at] == b'"' {
            return Some(at + 1);
        }
        at += 2;
    }
}

/// Where the value beginning at `at` in `bytes` ends, just past it.
fn value_end(bytes: &[u8], at: usize) -> Option<usize> {
    match *bytes.get(at)? {
        b'"' => string_end(bytes, at),
        b'{' | b'[' => {
            let mut depth = 0_usize;
            let mut next = at;
            loop {
                match *bytes.get(next)? {
                    b'"' => {
                        next = string_end(bytes, next)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(next + 1);
                        }
                    }
                    _ => {}
                }
                next += 1;
            }
        }
        _ => {
            let ends = |byte: &u8| matches!(byte, b',' | b'}' | b']');
            let rest = bytes.get(at..)?;
            Some(at + rest.iter().position(ends).unwrap_or(rest.len()))
        }
    }
}

/// The first quote or backslash in `bytes` from `at`, looked for eight
/// bytes at a time: a byte of a word is one sought where the word, with
/// that byte's value taken from each of its bytes, has a byte of nought.
fn quote_or_escape(bytes: &[u8], at: usize) -> Option<usize> {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const HIGHS: u64 = 0x8080_8080_8080_8080;
    // A quote, 0x22, and a backslash, 0x5c, in every byte.
    const QUOTES: u64 = 0x2222_2222_2222_2222;
    const ESCAPES: u64 = 0x5c5c_5c5c_5c5c_5c5c;
    // The high bit of each byte of nought, and of none before the first.
    let noughts = |word: u64| word.wrapping_sub(ONES) & !word & HIGHS;
    let rest = bytes.get(at..)?;
    let mut words = rest.chunks_exact(8);
    for (word_at, word) in words.by_ref().enumerate() {
        let word = u64::from_le_bytes(word.try_into().ok()?);
        let found = noughts(word ^ QUOTES) | noughts(word ^ ESCAPES);
        if found != 0 {
            let byte = usize::try_from(found.trailing_zeros() / 8).ok()?;
            return Some(at + word_at * 8 + byte);
        }
    }
    let tail = at + rest.len() - words.remainder().len();
    let sought = |byte: &u8| matches!(byte, b'"' | b'\\');
    Some(tail + words.remainder().iter().position(sought)?)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use serde_json::value::RawValue;

    use super::*;

    /// `json`'s fields by name and as written, as `serde_json` reads them.
    fn read(json: &str) -> Vec<(String, String)> {
        let read: BTreeMap<String, &RawValue> = serde_json::from_str(json).expect("an object");
        read.into_iter()
            .map(|(name, value)| (name, value.get().to_owned()))
            .collect()
    }

    /// `json`'s fields by name and as written, as they are split.
    fn split(json: &str) -> Vec<(String, String)> {
        fields(json)
            .expect("an object")
            .into_iter()
            .map(|field| (field.name.into_owned(), field.value.to_owned()))
            .collect()
    }

    #[test]
    fn an_object_is_split_into_the_fields_a_parse_reads() {
        let record = json!({
            "key": "session:a",
            "heard": -7,
            "cost": 1.25e-3,
            "busy": true,
            "gone": null,
            "empty": {},
            "none": [],
            "title": "the \"tide\" \\ and a back\\slash",
            "say \"hi\"": "named with quotes",
            "tab\there": "line\none",
            "crab": "\u{1F980} beside the skiff",
            "presence": {"at": "2026-11-17", "landings": [{"title": "}], {\"["}, [1, [2, {}]]]},
            "z": "last",
        });
        let json = record.to_string();
        assert_eq!(split(&json), read(&json), "{json}");
        let presence = record["presence"].to_string();
        assert_eq!(split(&presence), read(&presence));
        assert_eq!(fields("{}"), Some(Vec::new()), "an object of nothing");
    }

    #[test]
    fn a_field_named_with_escapes_keeps_its_name_as_written() {
        let json = json!({"say \"hi\"": 1, "tab\there": 2}).to_string();
        let fields = fields(&json).expect("an object");
        assert_eq!(fields[0].name, "say \"hi\"");
        assert_eq!(fields[0].written, "say \\\"hi\\\"");
        assert_eq!(fields[1].name, "tab\there");
        let pieces: Vec<Piece<'_, '_>> = fields
            .iter()
            .map(|field| Piece::of(field, Cow::Borrowed(field.value)))
            .collect();
        assert_eq!(object(&pieces), json, "written again as it was");
    }

    #[test]
    fn what_isnt_an_object_as_serde_writes_one_is_no_object() {
        for odd in [
            "",
            "[1]",
            "\"x\"",
            "{",
            "{\"a\":1",
            "{\"a\":1,}",
            "{\"a\":1}x",
            "{ \"a\":1}",
            "{\"a\" :1}",
            "{\"b\":1,\"a\":2}",
            "{\"a\":1,\"a\":2}",
            "{\"a\":\"open}",
        ] {
            assert_eq!(fields(odd), None, "{odd:?} isn't split");
        }
    }

    #[test]
    fn a_quote_or_an_escape_is_found_wherever_it_falls_in_a_word() {
        for length in 0..20 {
            for at in 0..length {
                for sought in [b'"', b'\\'] {
                    let mut bytes = vec![b'a'; length];
                    bytes[at] = sought;
                    if at + 1 < length {
                        bytes[at + 1] = b'"';
                    }
                    assert_eq!(quote_or_escape(&bytes, 0), Some(at), "{length} {at}");
                    assert_eq!(quote_or_escape(&bytes, at), Some(at));
                }
            }
            let none = vec![0xe2; length];
            assert_eq!(
                quote_or_escape(&none, 0),
                None,
                "{length} bytes, none sought"
            );
        }
    }
}
