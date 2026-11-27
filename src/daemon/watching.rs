//! A watch a client asked for: the records whole, then only what changed,
//! no faster than a reader can take them.

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tokio::sync::{mpsc, watch};

use super::records::{Build, Change, Record, Shared, shared};
use super::{BETWEEN_SENDS, Holds};

/// A watch a client asked for, running, with what it watches, so asking
/// again under its id for the same records changes only `when` it sends.
pub(super) struct Watching {
    /// The kind it watches.
    pub(super) kind: String,
    /// The keys it watches, or all of the kind.
    pub(super) keys: Option<Vec<String>>,
    /// The fields whose changes say only what moved inside them.
    pub(super) deep: Vec<String>,
    /// At most how many of those heard last it watches, or all.
    pub(super) latest: Option<usize>,
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
/// since it was last sent, held back or not. Asked for the `latest`, it
/// watches only that many of those heard last, as [`answer_now`] says.
pub(super) async fn keep_watching<H: Holds>(
    holds: Arc<H>,
    id: Value,
    (kind, keys, deep, latest): (String, Option<Vec<String>>, Vec<String>, Option<usize>),
    mut told: watch::Receiver<u64>,
    mut when: watch::Receiver<Option<Vec<String>>>,
    answering: mpsc::Sender<String>,
) {
    let held = |holds: &H, told: u64| {
        match holds.built() {
            Some(built) => built.records(holds, &kind, told),
            None => holds
                .records(&kind)
                .map(|fresh| Arc::new(Build::after(shared(None, fresh), None))),
        }
        .unwrap_or_default()
    };
    let first = held(&holds, *told.borrow_and_update());
    let window = latest.map(|n| heard_last(first.records(), keys.as_deref(), n));
    let watched = window.as_deref().or(keys.as_deref());
    let mut sent: Shared = first
        .records()
        .iter()
        .filter(|(key, _)| watched.is_none_or(|keys| keys.contains(*key)))
        .map(|(key, record)| (key.clone(), Arc::clone(record)))
        .collect();
    drop(first);
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
        let Some((changed, gone)) = answer_now(
            (when.borrow().as_deref(), keys.as_deref(), latest),
            &deep,
            &mut sent,
            &now,
        ) else {
            continue;
        };
        let Ok(said) = serde_json::to_string(&Since {
            id: &id,
            changed: &changed,
            gone: &gone,
        }) else {
            return;
        };
        if answering.send(said).await.is_err() {
            return;
        }
    }
}

/// A watch's first answer, `records` whole, its fields in the order a
/// `Value` gives its keys.
#[derive(Serialize)]
struct Whole<'a> {
    id: &'a Value,
    records: Vec<&'a Record>,
}

/// A watch's answer after its first: each record changed, as the very
/// bytes every watch sent the same change sends, and the keys gone. Its
/// fields are in the order a `Value` gives its keys.
#[derive(Serialize)]
struct Since<'a> {
    changed: &'a [Arc<Change>],
    gone: &'a [String],
    id: &'a Value,
}

/// The first answer to watch `id`, written from the very bytes of the
/// `records`, in order, that it shares with every other watch, copying
/// none of them. Records always serialize, being serialized already; were
/// one ever not to, the watch ends rather than send a broken line.
fn whole<'a>(id: &'a Value, records: Vec<&'a Record>) -> Option<String> {
    serde_json::to_string(&Whole { id, records }).ok()
}

/// What a watch that has sent `sent` answers now that the records stand
/// as `now` built them, as [`answer`] says, asked `when` of those `keys`
/// names or all. Asked for the `latest`, it answers only of that many of
/// them heard last, and says each it sent that has left them gone, as it
/// says one gone from the kind. A page showing the latest 20 messages
/// was first sent 12,179 of them, 1796 KB, on 26 November.
fn answer_now(
    (when, keys, latest): (Option<&[String]>, Option<&[String]>, Option<usize>),
    deep: &[String],
    sent: &mut Shared,
    now: &Build,
) -> Option<(Vec<Arc<Change>>, Vec<String>)> {
    let Some(latest) = latest else {
        return answer((when, keys), deep, sent, now);
    };
    let window = heard_last(now.records(), keys, latest);
    let left = leave(sent, now.records(), &window);
    match answer((when, Some(window.as_slice())), deep, sent, now) {
        Some((changed, mut gone)) => {
            gone.extend(left);
            Some((changed, gone))
        }
        None if left.is_empty() => None,
        None => Some((Vec::new(), left)),
    }
}

/// The keys of the `latest` of `records` heard last, of those `keys`
/// names or all, newest first and the same moment's by key. Only that
/// many are put in order, not every record.
fn heard_last(records: &Shared, keys: Option<&[String]>, latest: usize) -> Vec<String> {
    fn newest<'a>(&(key, heard): &(&'a String, i64)) -> (std::cmp::Reverse<i64>, &'a String) {
        (std::cmp::Reverse(heard), key)
    }
    let mut named: Vec<(&String, i64)> = records
        .iter()
        .filter(|(key, _)| keys.is_none_or(|keys| keys.contains(*key)))
        .map(|(key, record)| (key, record.heard()))
        .collect();
    if named.len() > latest {
        named.select_nth_unstable_by_key(latest, newest);
        named.truncate(latest);
    }
    named.sort_unstable_by_key(newest);
    named.into_iter().map(|(key, _)| key.clone()).collect()
}

/// Takes out of `sent` each record `now` still holds that is no longer
/// among the `window`, answering their keys, so they are said gone.
fn leave(sent: &mut Shared, now: &Shared, window: &[String]) -> Vec<String> {
    let left: Vec<String> = sent
        .keys()
        .filter(|key| now.contains_key(*key) && !window.contains(*key))
        .cloned()
        .collect();
    for key in &left {
        sent.remove(key);
    }
    left
}

/// What a watch that has sent `sent` answers now that the records stand
/// as `now` built them, asked `when` of those `keys` names or all: the
/// records changed it sends and the keys gone, with `sent` moved to
/// match. Nothing, when there is nothing to send.
fn answer(
    (when, keys): (Option<&[String]>, Option<&[String]>),
    deep: &[String],
    sent: &mut Shared,
    now: &Build,
) -> Option<(Vec<Arc<Change>>, Vec<String>)> {
    let (changed, gone) = now.changes(sent, keys, deep);
    let changed = sends(when, sent, changed);
    let now = now.records();
    if changed.is_empty() && gone.is_empty() {
        return None;
    }
    for change in &changed {
        if let Some((key, record)) = now.get_key_value(change.key()) {
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
fn sends(when: Option<&[String]>, sent: &Shared, changed: Vec<Arc<Change>>) -> Vec<Arc<Change>> {
    let Some(when) = when else {
        return changed;
    };
    changed
        .into_iter()
        .filter(|change| {
            !sent.contains_key(change.key()) || when.iter().any(|field| change.carries(field))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;

    /// Each of `records` as the change a watch sends of it new.
    fn changes(records: Vec<Value>) -> Vec<Arc<Change>> {
        records
            .into_iter()
            .map(|record| {
                let key = record["key"].as_str().expect("a key").to_owned();
                Arc::new(Change::whole(&key, &Record::from(record)))
            })
            .collect()
    }

    /// An answer's changes read back, to compare them as values.
    fn read((changed, gone): (Vec<Arc<Change>>, Vec<String>)) -> (Vec<Value>, Vec<String>) {
        (changed.iter().map(|change| change.value()).collect(), gone)
    }

    /// A run of builds of a kind's records: steps moving, a digest moving,
    /// one going, one coming, a field dropped, and one moving back to what
    /// it was two builds before.
    fn run() -> Vec<BTreeMap<String, Record>> {
        let build = |all: &[Value]| -> BTreeMap<String, Record> {
            all.iter()
                .map(|one| {
                    let key = one["key"].as_str().expect("a key").to_owned();
                    (key, Record::from(one.clone()))
                })
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
    fn heard(when: Option<&[String]>, builds: &[Build]) -> Vec<String> {
        let (first, rest) = builds.split_first().expect("a first build");
        let mut sent = first.records().clone();
        let records = sent.values().map(AsRef::as_ref).collect();
        let mut answers = vec![whole(&json!(1), records).expect("records serialize")];
        for now in rest {
            if let Some((changed, gone)) = answer((when, None), &[], &mut sent, now) {
                let changed = Since {
                    id: &json!(1),
                    changed: &changed,
                    gone: &gone,
                };
                answers.push(serde_json::to_string(&changed).expect("changes serialize"));
            }
        }
        answers
    }

    #[test]
    fn a_watch_of_shared_records_sends_byte_for_byte_what_one_of_its_own_would() {
        let mut last: Option<Arc<Shared>> = None;
        let kept: Vec<Build> = run()
            .into_iter()
            .map(|fresh| {
                let now = shared(last.as_deref(), fresh);
                let before = last.replace(Arc::new(now.clone()));
                Build::after(now, before)
            })
            .collect();
        let own: Vec<Build> = run()
            .into_iter()
            .map(|fresh| Build::after(shared(None, fresh), None))
            .collect();
        assert!(
            Arc::ptr_eq(
                &kept[1].records()["session:b"],
                &kept[0].records()["session:b"]
            ),
            "the run shares what didn't change"
        );
        let records: Vec<Value> = kept[0]
            .records()
            .values()
            .map(|record| record.value())
            .collect();
        assert_eq!(
            whole(
                &json!(1),
                kept[0].records().values().map(AsRef::as_ref).collect()
            ),
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
    fn a_watch_of_the_latest_sends_those_heard_last_and_says_one_pushed_out_gone() {
        let heard = |all: &[(&str, i64)]| -> Shared {
            all.iter()
                .map(|&(key, heard)| {
                    let record = Record::from(json!({"key": key, "heard": heard}));
                    (key.to_owned(), Arc::new(record))
                })
                .collect()
        };
        let first = heard(&[("message:a", 1), ("message:b", 3), ("message:c", 2)]);
        assert_eq!(
            heard_last(&first, None, 2),
            vec!["message:b".to_owned(), "message:c".to_owned()],
            "the two heard last, newest first"
        );
        assert_eq!(heard_last(&first, None, 5).len(), 3, "and all, where there are fewer");
        let mut sent: Shared = first
            .iter()
            .filter(|(key, _)| key.as_str() != "message:a")
            .map(|(key, record)| (key.clone(), Arc::clone(record)))
            .collect();
        let next = heard(&[
            ("message:a", 1),
            ("message:b", 3),
            ("message:c", 2),
            ("message:d", 4),
        ]);
        assert_eq!(
            answer_now(
                (None, None, Some(2)),
                &[],
                &mut sent,
                &Build::after(next, Some(Arc::new(first)))
            )
            .map(read),
            Some((
                vec![json!({"key": "message:d", "heard": 4})],
                vec!["message:c".to_owned()],
            )),
            "one heard anew is sent, and the one it pushed out is said gone"
        );
        assert_eq!(
            sent.keys().collect::<Vec<_>>(),
            ["message:b", "message:d"],
            "so the watch holds only the latest"
        );
    }

    #[test]
    fn what_a_watch_held_back_comes_deep_once_let_through() {
        let at = |at: u32, step: &str| -> Shared {
            Shared::from([(
                "session:skiff".to_owned(),
                Arc::new(Record::from(json!({
                    "key": "session:skiff", "told": "1", "step": step,
                    "presence": {"at": at, "step": step, "worksOn": ["kestrel"]},
                }))),
            )])
        };
        let told = ["told".to_owned()];
        let deep = ["presence".to_owned()];
        let first = Arc::new(at(1, "read"));
        let second = Arc::new(at(2, "edit"));
        let mut sent = Shared::clone(&first);
        assert_eq!(
            answer(
                (Some(&told), None),
                &deep,
                &mut sent,
                &Build::after(Shared::clone(&second), Some(first))
            )
            .map(read),
            None,
            "shut, a tick and a step are held back"
        );
        assert_eq!(
            answer(
                (None, None),
                &deep,
                &mut sent,
                &Build::after(at(3, "edit"), Some(second))
            )
            .map(read),
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
                Arc::new(Record::from(json!({"key": "session:a"}))),
            ),
            (
                "session:c".to_owned(),
                Arc::new(Record::from(json!({"key": "session:c"}))),
            ),
        ]);
        let told = ["told".to_owned()];
        let stepped = || vec![json!({"key": "session:a", "step": "y"})];
        let said = |when: Option<&[String]>, changed: Vec<Value>| {
            read((sends(when, &sent, changes(changed)), Vec::new())).0
        };
        assert_eq!(
            said(None, stepped()),
            stepped(),
            "asked when nothing in particular, every change is sent"
        );
        assert_eq!(
            said(Some(&told[..]), stepped()),
            Vec::<Value>::new(),
            "a step alone moves nothing a shut launcher reads"
        );
        assert_eq!(
            said(
                Some(&told[..]),
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
