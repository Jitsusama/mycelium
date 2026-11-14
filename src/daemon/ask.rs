//! What a client asks on the interface, read from its line, and how a
//! refusal is answered.

use serde_json::{Value, json};

/// What a client asks, by the `id` it is answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    /// The client's own id for it, echoed in every answer to it.
    pub id: Value,
    /// What it asks.
    pub what: What,
}

/// The asks the interface knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum What {
    /// Which version and kinds this daemon speaks.
    Hello,
    /// The records of these keys, of those held.
    Get(Vec<String>),
    /// Every record of a kind, at most `limit` of them.
    List {
        /// The kind.
        kind: String,
        /// At most how many, or all.
        limit: Option<usize>,
    },
    /// The records of a kind, or of these keys of it, then each change.
    Watch {
        /// The kind.
        kind: String,
        /// Only these keys, or all of the kind.
        keys: Option<Vec<String>>,
        /// Only a change moving one of these fields, or bringing or taking
        /// a record, is sent, with all that moved since the last send; or
        /// every change.
        when: Option<Vec<String>>,
    },
    /// The end of the watch asked with this id.
    Unwatch,
    /// What the person says, put to the session going by `address`.
    Steer {
        /// The session's address.
        address: String,
        /// What is said, in the harness's own shape.
        said: Value,
    },
    /// How the daemon and each of its sources stand.
    Health,
    /// An update of `machine` to its configuration's main.
    Update {
        /// The machine's name.
        machine: String,
    },
}

/// Why an ask can't be answered, by the interface's codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// An ask this daemon doesn't know.
    UnknownAsk(String),
    /// A kind this daemon doesn't hold.
    UnknownKind(String),
    /// A line that isn't an ask at all.
    NotAnAsk(String),
    /// An ask understood, which the daemon couldn't do, and why.
    Refused(String),
}

impl Refusal {
    /// The interface's code for it.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownAsk(_) => "unknown-ask",
            Self::UnknownKind(_) => "unknown-kind",
            Self::NotAnAsk(_) => "not-an-ask",
            Self::Refused(_) => "refused",
        }
    }

    /// What it says, for a person.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::UnknownAsk(ask) => format!("this node doesn't answer {ask:?}"),
            Self::UnknownKind(kind) => format!("this node holds no {kind:?}"),
            Self::NotAnAsk(why) => format!("not an ask: {why}"),
            Self::Refused(why) => why.clone(),
        }
    }
}

/// The ask a line holds, or why it isn't one, with the `id` it gave if
/// it gave one, so even a refusal is answered to it.
///
/// # Errors
///
/// A line that isn't a JSON object with an `ask`, or asks what this
/// node doesn't know.
pub fn read(line: &str) -> Result<Ask, (Value, Refusal)> {
    let value: Value = serde_json::from_str(line)
        .map_err(|error| (Value::Null, Refusal::NotAnAsk(error.to_string())))?;
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let refused = |refusal| (id.clone(), refusal);
    let Some(ask) = value.get("ask").and_then(Value::as_str) else {
        return Err(refused(Refusal::NotAnAsk("it names no `ask`".to_owned())));
    };
    let kind = || {
        value
            .get("kind")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| refused(Refusal::NotAnAsk(format!("{ask:?} names no `kind`"))))
    };
    let keys = |name| {
        value.get(name).and_then(Value::as_array).map(|keys| {
            keys.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
    };
    let what = match ask {
        "hello" => What::Hello,
        "get" => What::Get(keys("keys").unwrap_or_default()),
        "list" => What::List {
            kind: kind()?,
            limit: value
                .get("limit")
                .and_then(Value::as_u64)
                .and_then(|limit| usize::try_from(limit).ok()),
        },
        "watch" => What::Watch {
            kind: kind()?,
            keys: keys("keys"),
            when: keys("when"),
        },
        "unwatch" => What::Unwatch,
        "health" => What::Health,
        "steer" => What::Steer {
            address: value
                .get("address")
                .and_then(Value::as_str)
                .filter(|address| !address.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    refused(Refusal::NotAnAsk("a steer names no `address`".to_owned()))
                })?,
            said: value
                .get("steer")
                .filter(|said| said.is_object())
                .cloned()
                .ok_or_else(|| refused(Refusal::NotAnAsk("a steer says nothing".to_owned())))?,
        },
        "update" => What::Update {
            machine: value
                .get("machine")
                .and_then(Value::as_str)
                .filter(|machine| !machine.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    refused(Refusal::NotAnAsk("an update names no `machine`".to_owned()))
                })?,
        },
        other => return Err(refused(Refusal::UnknownAsk(other.to_owned()))),
    };
    Ok(Ask { id, what })
}

/// The answer to `id` that it was refused, and why.
#[must_use]
pub fn refused(id: &Value, refusal: &Refusal) -> Value {
    json!({"id": id, "error": {"code": refusal.code(), "message": refusal.message()}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ask_is_read_with_its_id_and_a_refusal_is_answered_to_it() {
        assert_eq!(
            read(r#"{"id":4,"ask":"watch","kind":"session","keys":["session:a"]}"#),
            Ok(Ask {
                id: json!(4),
                what: What::Watch {
                    kind: "session".to_owned(),
                    keys: Some(vec!["session:a".to_owned()]),
                    when: None,
                },
            }),
            "a watch names its kind and keys"
        );
        let Err((id, refusal)) = read(r#"{"id":"x","ask":"juggle"}"#) else {
            panic!("an unknown ask is refused");
        };
        assert_eq!(
            refused(&id, &refusal),
            json!({"id": "x", "error": {
                "code": "unknown-ask",
                "message": "this node doesn't answer \"juggle\""
            }}),
            "with the interface's code, to the id it gave"
        );
        assert_eq!(
            read(r#"{"id":5,"ask":"list"}"#).map_err(|(_, refusal)| refusal.code()),
            Err("not-an-ask"),
            "a list names its kind"
        );
    }

    #[test]
    fn a_steer_is_read_with_whom_it_is_for_and_what_it_says() {
        assert_eq!(
            read(r#"{"id":6,"ask":"steer","address":"AB12CD","steer":{"act":"pause"}}"#),
            Ok(Ask {
                id: json!(6),
                what: What::Steer {
                    address: "AB12CD".to_owned(),
                    said: json!({"act": "pause"}),
                },
            }),
            "a steer names its session's address and what is said"
        );
        for nothing in [
            r#"{"id":7,"ask":"steer","steer":{"act":"pause"}}"#,
            r#"{"id":7,"ask":"steer","address":"","steer":{"act":"pause"}}"#,
            r#"{"id":7,"ask":"steer","address":"AB12CD"}"#,
            r#"{"id":7,"ask":"steer","address":"AB12CD","steer":"pause"}"#,
        ] {
            assert_eq!(
                read(nothing).map_err(|(id, refusal)| (id, refusal.code())),
                Err((json!(7), "not-an-ask")),
                "a steer for nobody, or saying nothing, isn't one: {nothing}"
            );
        }
    }
}
