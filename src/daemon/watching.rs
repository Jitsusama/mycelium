//! A watch a client asked for: the records whole, then only what changed,
//! no faster than a reader can take them.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};

use super::{BETWEEN_SENDS, Holds, changes};

/// A watch a client asked for, running, with what it watches, so asking
/// again under its id for the same records changes only `when` it sends.
pub(super) struct Watching {
    /// The kind it watches.
    pub(super) kind: String,
    /// The keys it watches, or all of the kind.
    pub(super) keys: Option<Vec<String>>,
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
/// to let it through.
pub(super) async fn keep_watching<H: Holds>(
    holds: Arc<H>,
    id: Value,
    (kind, keys): (String, Option<Vec<String>>),
    mut told: watch::Receiver<u64>,
    mut when: watch::Receiver<Option<Vec<String>>>,
    answering: mpsc::Sender<Value>,
) {
    let held = |holds: &H, told: u64| {
        let records = match holds.built() {
            Some(built) => built.records(holds, &kind, told),
            None => holds.records(&kind).map(Arc::new),
        }
        .unwrap_or_default();
        match &keys {
            Some(keys) => Arc::new(
                records
                    .iter()
                    .filter(|(key, _)| keys.contains(*key))
                    .map(|(key, record)| (key.clone(), record.clone()))
                    .collect(),
            ),
            None => records,
        }
    };
    let mut sent = BTreeMap::clone(&held(&holds, *told.borrow_and_update()));
    let whole: Vec<Value> = sent.values().cloned().collect();
    if answering
        .send(json!({"id": id, "records": whole}))
        .await
        .is_err()
    {
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
        let (changed, gone) = changes(&sent, &now);
        let changed = sends(when.borrow().as_deref(), &sent, changed);
        if changed.is_empty() && gone.is_empty() {
            continue;
        }
        for record in &changed {
            if let Some((key, record)) = record
                .get("key")
                .and_then(Value::as_str)
                .and_then(|key| now.get_key_value(key))
            {
                sent.insert(key.clone(), record.clone());
            }
        }
        for key in &gone {
            sent.remove(key);
        }
        if answering
            .send(json!({"id": id, "changed": changed, "gone": gone}))
            .await
            .is_err()
        {
            return;
        }
    }
}

/// Of a change's records, those a watch sends: every one while `when`
/// names nothing; else each record come, and each moving a field `when`
/// names, such as a session's `told`, which moves only with what a shut
/// launcher reads. The rest are held back, so one session's `told`
/// moving sends that session alone, not every other's step beside it.
fn sends(
    when: Option<&[String]>,
    sent: &BTreeMap<String, Value>,
    changed: Vec<Value>,
) -> Vec<Value> {
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
    use super::*;

    #[test]
    fn a_watch_asked_when_a_field_moves_sends_only_the_records_moving_it() {
        let sent: BTreeMap<String, Value> = BTreeMap::from([
            ("session:a".to_owned(), json!({"key": "session:a"})),
            ("session:c".to_owned(), json!({"key": "session:c"})),
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
