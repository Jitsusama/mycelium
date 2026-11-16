//! A watch a client asked for: the records whole, then only what changed,
//! no faster than a reader can take them.

use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use super::records::{Shared, shared};
use super::{BETWEEN_SENDS, Holds, changes};

/// A watch a client asked for, running, with what it watches, so asking
/// again under its id for the same records changes only `when` it sends.
pub(super) struct Watching {
    /// The kind it watches.
    pub(super) kind: String,
    /// The keys it watches, or all of the kind.
    pub(super) keys: Option<Vec<String>>,
    /// The fields whose changes say only what moved inside them.
    pub(super) deep: Vec<String>,
    /// The fields whose moving it sends for, told to it as they change.
    pub(super) when: watch::Sender<Option<Vec<String>>>,
    /// What sends it.
    pub(super) task: tokio::task::JoinHandle<()>,
}

/// Sends `kind`'s records, or those of `keys`, whole, then what changes
/// each time `told` says they may have, no sooner than [`BETWEEN_SENDS`]
/// after the last, and nothing while nothing changes. While `when` names
/// fields, a record's change moving none of them is held back, and sent
/// with that record's next that does, or at once when `when` is changed
/// to let it through. A field `deep` names says only what moved inside it
/// since it was last sent, held back or not.
pub(super) async fn keep_watching<H: Holds>(
    holds: Arc<H>,
    id: Value,
    (kind, keys, deep): (String, Option<Vec<String>>, Vec<String>),
    mut told: watch::Receiver<u64>,
    mut when: watch::Receiver<Option<Vec<String>>>,
    answering: mpsc::Sender<String>,
) {
    let held = |holds: &H, told: u64| {
        let records = match holds.built() {
            Some(built) => built.records(holds, &kind, told),
            None => holds
                .records(&kind)
                .map(|fresh| Arc::new(shared(None, fresh))),
        }
        .unwrap_or_default();
        match &keys {
            Some(keys) => Arc::new(
                records
                    .iter()
                    .filter(|(key, _)| keys.contains(*key))
                    .map(|(key, record)| (key.clone(), Arc::clone(record)))
                    .collect(),
            ),
            None => records,
        }
    };
    let mut sent = Shared::clone(&held(&holds, *told.borrow_and_update()));
    let in_order = super::in_order(holds.as_ref(), &kind, sent.values().map(AsRef::as_ref));
    let Some(first) = whole(&id, in_order) else {
        return;
    };
    if answering.send(first).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            heard = told.changed() => {
                if heard.is_err() {
                    return;
                }
                tokio::time::sleep(BETWEEN_SENDS).await;
            }
            asked = when.changed() => {
                if asked.is_err() {
                    return;
                }
            }
        }
        let now = held(&holds, *told.borrow_and_update());
        let Some((changed, gone)) = answer(when.borrow().as_deref(), &deep, &mut sent, &now) else {
            continue;
        };
        if answering
            .send(json!({"id": id, "changed": changed, "gone": gone}).to_string())
            .await
            .is_err()
        {
            return;
        }
    }
}

/// A watch's first answer, `records` whole, its fields in the order a
/// `Value` gives its keys.
#[derive(Serialize)]
struct Whole<'a> {
    id: &'a Value,
    records: Vec<&'a Value>,
}

/// The first answer to watch `id`, serialized from the very `records`, in
/// order, that it shares with every other watch, copying none of them.
/// Records always serialize, being values already; were one ever not to,
/// the watch ends rather than send a broken line.
fn whole<'a>(id: &'a Value, records: Vec<&'a Value>) -> Option<String> {
    serde_json::to_string(&Whole { id, records }).ok()
}

/// What a watch that has sent `sent` answers now that the records stand
/// at `now`: the records changed it sends and the keys gone, with `sent`
/// moved to match. Nothing, when there is nothing to send.
fn answer(
    when: Option<&[String]>,
    deep: &[String],
    sent: &mut Shared,
    now: &Shared,
) -> Option<(Vec<Value>, Vec<String>)> {
    let (changed, gone) = changes(sent, now, deep);
    let changed = sends(when, sent, changed);
    if changed.is_empty() && gone.is_empty() {
        return None;
    }
    for record in &changed {
        if let Some((key, record)) = record
            .get("key")
            .and_then(Value::as_str)
            .and_then(|key| now.get_key_value(key))
        {
            sent.insert(key.clone(), Arc::clone(record));
        }
    }
    for key in &gone {
        sent.remove(key);
    }
    Some((changed, gone))
}

/// Of a change's records, those a watch sends: every one while `when`
/// names nothing; else each record come, and each moving a field `when`
/// names, such as a session's `told`, which moves only with what a shut
/// launcher reads. The rest are held back, so one session's `told`
/// moving sends that session alone, not every other's step beside it.
fn sends(when: Option<&[String]>, sent: &Shared, changed: Vec<Value>) -> Vec<Value> {
    let Some(when) = when else {
        return changed;
    };
    changed
        .into_iter()
        .filter(|record| {
            record
                .get("key")
                .and_then(Value::as_str)
                .is_none_or(|key| !sent.contains_key(key))
                || when.iter().any(|field| record.get(field).is_some())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// A run of builds of a kind's records: steps moving, a digest moving,
    /// one going, one coming, a field dropped, and one moving back to what
    /// it was two builds before.
    fn run() -> Vec<BTreeMap<String, Value>> {
        let build = |all: &[Value]| -> BTreeMap<String, Value> {
            all.iter()
                .map(|one| (one["key"].as_str().expect("a key").to_owned(), one.clone()))
                .collect()
        };
        vec![
            build(&[
                json!({"key": "session:a", "step": "x", "told": "1"}),
                json!({"key": "session:b", "step": "x", "told": "1"}),
            ]),
            build(&[
                json!({"key": "session:a", "step": "y", "told": "1"}),
                json!({"key": "session:b", "step": "x", "told": "1"}),
            ]),
            build(&[
                json!({"key": "session:a", "step": "y", "told": "2"}),
                json!({"key": "session:b", "step": "x", "told": "1"}),
                json!({"key": "session:c", "step": "x"}),
            ]),
            build(&[
                json!({"key": "session:a", "step": "x", "told": "2"}),
                json!({"key": "session:c", "step": "x"}),
            ]),
            build(&[
                json!({"key": "session:a", "step": "x"}),
                json!({"key": "session:c", "step": "x"}),
            ]),
            build(&[
                json!({"key": "session:a", "step": "x"}),
                json!({"key": "session:c", "step": "x"}),
            ]),
        ]
    }

    /// What a watch asked `when` sends over `builds`, as the bytes a
    /// client reads, its first answer whole and then each change.
    fn heard(when: Option<&[String]>, builds: &[Shared]) -> Vec<String> {
        let (first, rest) = builds.split_first().expect("a first build");
        let mut sent = first.clone();
        let records = sent.values().map(AsRef::as_ref).collect();
        let mut answers = vec![whole(&json!(1), records).expect("records serialize")];
        for now in rest {
            if let Some((changed, gone)) = answer(when, &[], &mut sent, now) {
                answers.push(json!({"id": 1, "changed": changed, "gone": gone}).to_string());
            }
        }
        answers
    }

    #[test]
    fn a_watch_of_shared_records_sends_byte_for_byte_what_one_of_its_own_would() {
        let mut last: Option<Shared> = None;
        let kept: Vec<Shared> = run()
            .into_iter()
            .map(|fresh| {
                let now = shared(last.as_ref(), fresh);
                last = Some(now.clone());
                now
            })
            .collect();
        let own: Vec<Shared> = run().into_iter().map(|fresh| shared(None, fresh)).collect();
        assert!(
            Arc::ptr_eq(&kept[1]["session:b"], &kept[0]["session:b"]),
            "the run shares what didn't change"
        );
        let records: Vec<Value> = kept[0]
            .values()
            .map(|record| Value::clone(record))
            .collect();
        assert_eq!(
            whole(&json!(1), kept[0].values().map(AsRef::as_ref).collect()),
            Some(json!({"id": 1, "records": records}).to_string()),
            "the first answer reads as one built whole would"
        );
        let told = ["told".to_owned()];
        for when in [None, Some(&told[..])] {
            let answers = heard(when, &kept);
            assert_eq!(
                answers,
                heard(when, &own),
                "asked when {when:?}, sharing changes not a byte of what is sent"
            );
            assert!(
                answers.len() > 2,
                "the run sends changes, asked when {when:?}"
            );
        }
        assert_eq!(
            heard(None, &kept).len(),
            5,
            "every build that changed something sends, and the last, unchanged, doesn't"
        );
    }

    #[test]
    fn what_a_watch_held_back_comes_deep_once_let_through() {
        let at = |at: u32, step: &str| -> Shared {
            Shared::from([(
                "session:skiff".to_owned(),
                Arc::new(json!({
                    "key": "session:skiff", "told": "1", "step": step,
                    "presence": {"at": at, "step": step, "worksOn": ["kestrel"]},
                })),
            )])
        };
        let told = ["told".to_owned()];
        let deep = ["presence".to_owned()];
        let mut sent = at(1, "read");
        assert_eq!(
            answer(Some(&told), &deep, &mut sent, &at(2, "edit")),
            None,
            "shut, a tick and a step are held back"
        );
        assert_eq!(
            answer(None, &deep, &mut sent, &at(3, "edit")),
            Some((
                vec![json!({
                    "key": "session:skiff", "step": "edit",
                    "presence": {"at": 3, "step": "edit"},
                })],
                Vec::new(),
            )),
            "let through, it says all that moved since it was sent, and inside its presence \
             only that"
        );
    }

    #[test]
    fn a_watch_asked_when_a_field_moves_sends_only_the_records_moving_it() {
        let sent: Shared = Shared::from([
            (
                "session:a".to_owned(),
                Arc::new(json!({"key": "session:a"})),
            ),
            (
                "session:c".to_owned(),
                Arc::new(json!({"key": "session:c"})),
            ),
        ]);
        let told = ["told".to_owned()];
        let stepped = || vec![json!({"key": "session:a", "step": "y"})];
        assert_eq!(
            sends(None, &sent, stepped()),
            stepped(),
            "asked when nothing in particular, every change is sent"
        );
        assert_eq!(
            sends(Some(&told), &sent, stepped()),
            Vec::<Value>::new(),
            "a step alone moves nothing a shut launcher reads"
        );
        assert_eq!(
            sends(
                Some(&told),
                &sent,
                vec![
                    json!({"key": "session:a", "step": "y", "told": "2"}),
                    json!({"key": "session:b", "step": "y"}),
                    json!({"key": "session:c", "step": "z"}),
                ]
            ),
            vec![
                json!({"key": "session:a", "step": "y", "told": "2"}),
                json!({"key": "session:b", "step": "y"}),
            ],
            "one moving its digest is sent, and one come, but not another's step"
        );
    }
}
