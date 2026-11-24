use serde_json::json;

use super::*;

/// When the tests stand: 23 November 2026, 12:00 UTC.
const NOW: i64 = 1_795_435_200_000;

/// A minute, in milliseconds.
const MINUTE: i64 = 60 * 1000;

/// `millis` since the epoch, in RFC 3339, as a message says when.
fn said_at(millis: i64) -> String {
    jiff::Timestamp::from_millisecond(millis)
        .expect("the tests stand in this age")
        .to_string()
}

/// A message `id` from `from`, sent `ago` before now, to `groups`, with
/// `intent`, answering `re`.
fn said(id: &str, from: &str, ago: i64, groups: &[&str], intent: Option<Intent>) -> Message {
    Message {
        id: id.to_owned(),
        at: said_at(NOW - ago),
        from: from.to_owned(),
        groups: groups
            .iter()
            .map(|group| Group::new(*group).expect("the test names a valid group"))
            .collect(),
        intent,
        text: format!("the words of {id}, which stay on the node"),
        re: None,
        supersedes: None,
        from_realm: None,
        for_realm: None,
        ends_hold: false,
        pid: None,
        hosted: false,
        invite: None,
        rest: serde_json::Map::new(),
    }
}

/// `message`, answering `re`.
fn answering(re: &str, message: Message) -> Message {
    Message {
        re: Some(re.to_owned()),
        ..message
    }
}

/// The records `messages` make as now stands, no session going by
/// another address.
fn made(messages: &[Message]) -> BTreeMap<String, Value> {
    read(talk(messages, &HashMap::new(), NOW))
}

/// The records of `talk`, each parsed to read its fields.
fn read(talk: Talk) -> BTreeMap<String, Value> {
    records(talk)
        .into_iter()
        .map(|(key, record)| (key, record.value()))
        .collect()
}

#[test]
fn a_message_of_the_day_written_with_an_offset_still_ties() {
    let mut state = crate::state::State::default();
    state.add_messages(vec![
        said("m1", "OSPREY", 25 * HOUR, &["peer.tern"], None),
        Message {
            at: "2026-11-22T10:00:00-12:00".to_owned(),
            ..said("m2", "KP0L6A", 14 * HOUR, &["peer.hnp07b"], None)
        },
        said("m3", "HNP07B", MINUTE, &["quest.x"], None),
    ]);
    let records = read(talk(state.messages(), &HashMap::new(), NOW));
    assert_eq!(
        records["tie:HNP07B~KP0L6A"]["heard"],
        json!(NOW - 14 * HOUR),
        "sent 14 hours ago, though its words sort before a day ago's: {records:?}"
    );
}

#[test]
fn a_peer_named_is_tied_to_its_sender_with_each_ends_counts() {
    let messages = [
        said(
            "m1",
            "KP0L6A",
            3 * HOUR,
            &["peer.hnp07b"],
            Some(Intent::Question),
        ),
        said(
            "m2",
            "HNP07B",
            10 * MINUTE,
            &["peer.kp0l6a"],
            Some(Intent::Fyi),
        ),
    ];
    let records = made(&messages);
    let tie = &records["tie:HNP07B~KP0L6A"];
    assert!(
        !records.contains_key("tie:KP0L6A~HNP07B"),
        "its key names its ends sorted"
    );
    assert_eq!(tie["hour"], json!([1, 0]), "each end's messages this hour");
    assert_eq!(tie["today"], json!([1, 1]), "each end's messages today");
    assert_eq!(tie["intents"], json!(["fyi", "question"]));
    assert_eq!(tie["groups"], json!(["peer.hnp07b", "peer.kp0l6a"]));
    assert_eq!(tie["heard"], json!(NOW - 10 * MINUTE), "its last message");
}

#[test]
fn an_answer_ties_its_sender_to_who_it_answers() {
    let messages = [
        said("m1", "KP0L6A", HOUR / 2, &["quest.hnp07b"], None),
        answering(
            "m1",
            said("m2", "HNP07B", MINUTE, &["quest.hnp07b"], Some(Intent::Fyi)),
        ),
    ];
    let records = made(&messages);
    let tie = &records["tie:HNP07B~KP0L6A"];
    assert_eq!(
        tie["today"],
        json!([1, 0]),
        "only the answer is on the tie; what it answers went to everyone"
    );
    assert_eq!(tie["groups"], json!(["quest.hnp07b"]));
}

#[test]
fn a_broadcast_ties_no_one_but_is_its_senders_talk() {
    let messages = [
        said("m1", "KP0L6A", 2 * HOUR, &["quest.hnp07b", "machine"], None),
        said("m2", "KP0L6A", MINUTE, &["orchestration.wcjqsg"], None),
        said("m3", "HNP07B", MINUTE, &["peer.kp0l6a"], None),
    ];
    let records = made(&messages);
    let keys: Vec<&str> = records.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["tie:HNP07B", "tie:HNP07B~KP0L6A", "tie:KP0L6A"],
        "one tie, from the one message naming a peer, and each cell's talk"
    );
    let talked = &records["tie:KP0L6A"];
    assert_eq!(talked["hour"], json!(1), "every message it sent this hour");
    assert_eq!(talked["today"], json!(2), "every message it sent today");
    assert_eq!(talked["ties"], json!(1), "the cells it is tied to");
    assert_eq!(
        talked["heard"],
        json!(NOW - MINUTE),
        "it was last talked to a minute ago"
    );
}

#[test]
fn a_session_going_by_several_addresses_is_one_cell() {
    let session: Session = serde_json::from_value(json!({
        "id": "019a0000-0000-7000-8000-00004c3144b7",
        "address": "HNP07B",
        "machine": "kestrel",
        "groups": [],
        "state": "busy",
        "at": "2026-11-17T11:59:00Z",
        "presence": {"addresses": ["HNP07B", "4c3144b7"]},
    }))
    .expect("a session");
    let messages = [
        said("m1", "4C3144B7", HOUR / 2, &["peer.kp0l6a"], None),
        said("m2", "KP0L6A", MINUTE, &["peer.4c3144b7"], None),
        said("m3", "KP0L6A", MINUTE, &["peer.hnp07b"], None),
    ];
    let records = read(talk(&messages, &aliases([&session]), NOW));
    assert_eq!(records.len(), 3, "one tie and two cells: {records:?}");
    assert_eq!(records["tie:HNP07B~KP0L6A"]["today"], json!([1, 2]));
}

#[test]
fn a_name_holding_the_key_separator_is_no_cell() {
    let messages = [
        said("m1", "KP0L6A~HNP07B", MINUTE, &["peer.tern"], None),
        answering("m1", said("m2", "OSPREY", MINUTE, &["quest.x"], None)),
    ];
    let records = made(&messages);
    let keys: Vec<&str> = records.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["tie:OSPREY"],
        "a cell named with `~` would read as a tie, so it neither talks nor is tied"
    );
    assert_eq!(records["tie:OSPREY"]["ties"], json!(0));
}

#[test]
fn no_one_is_tied_to_themself() {
    let messages = [said("m1", "KP0L6A", MINUTE, &["peer.kp0l6a"], None)];
    let records = made(&messages);
    assert!(
        records.keys().all(|key| !key.contains('~')),
        "a note to oneself is no tie: {records:?}"
    );
}

#[test]
fn a_tie_thickens_with_each_message_and_halves_each_half_life() {
    let messages = [
        said("m1", "KP0L6A", 3 * HALF_LIFE, &["peer.hnp07b"], None),
        said("m2", "HNP07B", 2 * HALF_LIFE, &["peer.kp0l6a"], None),
        said("m3", "KP0L6A", HALF_LIFE, &["peer.hnp07b"], None),
    ];
    let tie = &made(&messages)["tie:HNP07B~KP0L6A"];
    assert_eq!(
        tie["strength"],
        json!(1.75),
        "a quarter, a half and one, as it stood at its last message"
    );
    assert_eq!(tie["heard"], json!(NOW - HALF_LIFE));
}

#[test]
fn a_tie_quiet_for_a_day_is_gone() {
    let messages = [
        said("m1", "KP0L6A", DAY, &["peer.hnp07b"], None),
        said("m2", "HNP07B", DAY + MINUTE, &["peer.kp0l6a"], None),
    ];
    assert!(made(&messages).is_empty(), "nothing said in the last day");
}

#[test]
fn an_answer_today_to_an_old_message_still_ties() {
    let messages = [
        said("m1", "KP0L6A", 3 * DAY, &["quest.hnp07b"], None),
        answering("m1", said("m2", "HNP07B", MINUTE, &["quest.hnp07b"], None)),
    ];
    let records = made(&messages);
    assert_eq!(records["tie:HNP07B~KP0L6A"]["today"], json!([1, 0]));
    assert_eq!(
        records["tie:KP0L6A"]["today"],
        json!(0),
        "it said nothing today, but was answered"
    );
}

#[test]
fn no_record_holds_a_messages_words_or_its_id() {
    let messages = [
        said(
            "m-first",
            "KP0L6A",
            HOUR,
            &["peer.hnp07b"],
            Some(Intent::Hold),
        ),
        answering(
            "m-first",
            said("m-second", "HNP07B", MINUTE, &["quest.x"], None),
        ),
    ];
    let records = made(&messages);
    let said = serde_json::to_string(&records).expect("records are JSON");
    for message in &messages {
        assert!(
            !said.contains(&message.text) && !said.contains("words"),
            "a message's words stay on the node: {said}"
        );
        assert!(
            !said.contains(&message.id),
            "a message's id stays on the node: {said}"
        );
    }
    let allowed = [
        "key", "kind", "heard", "hour", "today", "intents", "groups", "strength", "ties",
    ];
    for record in records.values() {
        let fields = record.as_object().expect("a record is an object");
        assert!(
            fields.keys().all(|field| allowed.contains(&field.as_str())),
            "a record says who, when, how and where, and nothing else: {record}"
        );
    }
}
