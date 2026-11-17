use serde_json::{Value, json};

use super::*;

/// When the tests stand: 17 November 2026, 06:00 UTC.
const NOW: i64 = 1_795_413_600_000;

/// An hour, in milliseconds.
const HOUR: i64 = 60 * 60 * 1000;

/// `millis` since the epoch, in RFC 3339, as a presence says when.
fn said_at(millis: i64) -> String {
    jiff::Timestamp::from_millisecond(millis)
        .expect("the tests stand in this age")
        .to_string()
}

/// The session `id` on `machine`, standing as `state`, with `presence`,
/// last written an hour before now.
fn session(id: &str, machine: &str, state: State, presence: Value) -> Session {
    Session {
        id: id.to_owned(),
        address: id.to_ascii_uppercase(),
        machine: machine.to_owned(),
        title: None,
        working_on: None,
        groups: Vec::new(),
        state,
        step: None,
        since: None,
        orchestrated_by: None,
        quest: None,
        wants: None,
        landings: Vec::new(),
        doing: None,
        events: Vec::new(),
        window: None,
        held: None,
        attach: None,
        host: None,
        conversation: None,
        at: said_at(NOW - HOUR),
        presence: presence.into(),
    }
}

/// A session here on skiff that ended.
fn ended_here(id: &str) -> Session {
    session(id, "skiff", State::Ended, json!({"state": "ended"}))
}

/// A session running on kestrel.
fn running(id: &str) -> Session {
    session(id, "kestrel", State::Busy, json!({"state": "busy"}))
}

/// The quests' record as the node serves it: the quest `HARBOR`, active
/// and orchestrated by `oars`, attaching `sessions`.
fn quests(sessions: &[&str]) -> Vec<Value> {
    vec![json!({
        "key": "quest:QEST-20261116-HARBOR", "kind": "quest", "heard": NOW,
        "id": "QEST-20261116-HARBOR", "address": "HARBOR", "status": "active",
        "orchestrator": "oars", "sessions": sessions,
    })]
}

/// How the sessions stand, for the tests: `parked` where named, running
/// where busy or idle, else as nothing.
fn standing_of<'a>(parked: &'a [&'a str]) -> impl Fn(&Session) -> Option<&'static str> + 'a {
    move |session: &Session| match session.state {
        State::Busy | State::Idle => Some("running"),
        _ => parked.contains(&session.id.as_str()).then_some("parked"),
    }
}

/// What `ended` makes of `sessions` against `quests` at `now`, the ones
/// named in `parked` parked and in `back` coming back.
fn told_at(
    now: i64,
    seen: &mut Seen,
    sessions: &[Session],
    quests: &[Value],
    (parked, back): (&[&str], &[&str]),
) -> Vec<(String, i64, Value)> {
    let standing = standing_of(parked);
    let coming_back: HashSet<String> = back.iter().map(|id| (*id).to_owned()).collect();
    let judged = Judged {
        machine: "skiff",
        standing: &standing,
        coming_back: &coming_back,
    };
    ended(sessions, &judged, quests, now, seen)
}

/// What is told of `sessions` once they have been seen ended long enough.
fn told(
    sessions: &[Session],
    quests: &[Value],
    judged: (&[&str], &[&str]),
) -> Vec<(String, i64, Value)> {
    let mut seen = Seen::default();
    told_at(NOW, &mut seen, sessions, quests, judged);
    told_at(NOW + SETTLED_AFTER, &mut seen, sessions, quests, judged)
}

#[test]
fn a_session_here_that_ended_on_an_active_quest_is_told_to_its_orchestrator() {
    let gone = ended_here("gone01");
    let at = gone.at.clone();
    assert_eq!(
        told(&[gone], &quests(&["gone01"]), (&[], &[])),
        [(
            "gone01".to_owned(),
            NOW,
            json!({
                "session": "gone01", "address": "GONE01", "quest": "QEST-20261116-HARBOR",
                "orchestrator": "OARS", "at": at, "why": "ended",
            })
        )],
        "its record names the quest's orchestrator, heard when the node first saw it end"
    );
}

#[test]
fn an_end_is_told_only_once_the_node_has_seen_it_settle_and_for_a_day() {
    let sessions = [ended_here("gone01")];
    let quests = quests(&["gone01"]);
    let mut seen = Seen::default();
    let ids = |seen: &mut Seen, now: i64| -> Vec<String> {
        told_at(now, seen, &sessions, &quests, (&[], &[]))
            .into_iter()
            .map(|(id, _, _)| id)
            .collect()
    };
    assert!(
        ids(&mut seen, NOW).is_empty(),
        "just seen, it may yet be a park's marker late"
    );
    assert!(
        ids(&mut seen, NOW + SETTLED_AFTER - 1).is_empty(),
        "not until it has settled"
    );
    assert_eq!(
        ids(&mut seen, NOW + SETTLED_AFTER),
        ["gone01"],
        "settled, it's told"
    );
    assert_eq!(ids(&mut seen, NOW + KEPT_FOR), ["gone01"], "and for a day");
    assert!(
        ids(&mut seen, NOW + KEPT_FOR + 1).is_empty(),
        "a day from when the node saw it, not from its presence's last write"
    );
}

#[test]
fn a_park_nothing_can_wake_is_told_and_one_that_can_is_not() {
    let napping = json!({"state": "parked"});
    let stale = session("stale1", "skiff", State::Ended, napping.clone());
    let asleep = session("sleep1", "skiff", State::Ended, napping);
    let quests = quests(&["stale1", "sleep1"]);
    let told = told(&[stale, asleep], &quests, (&["sleep1"], &[]));
    let whys: Vec<(&str, &Value)> = told
        .iter()
        .map(|(id, _, fields)| (id.as_str(), &fields["why"]))
        .collect();
    assert_eq!(
        whys,
        [("stale1", &json!("parked-unwakeable"))],
        "a park its node says can wake is asleep, not ended"
    );
}

#[test]
fn a_session_coming_back_is_not_told() {
    let sessions = [ended_here("carr01"), ended_here("turn01")];
    assert_eq!(
        told(
            &sessions,
            &quests(&["carr01", "turn01"]),
            (&[], &["carr01", "turn01"])
        ),
        [],
        "carried from here, or waiting its turn to resume, its work goes on"
    );
}

#[test]
fn a_session_is_on_the_quest_the_quests_record_attaches_it_to_alone() {
    let mut claims = ended_here("clai01");
    claims.quest = Some("QEST-20261116-HARBOR".to_owned());
    let attached = ended_here("atta01");
    let told = told(&[claims, attached], &quests(&["atta01"]), (&[], &[]));
    let ids: Vec<&str> = told.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        ["atta01"],
        "a presence naming a quest doesn't choose whose orchestrator is told"
    );
}

#[test]
fn work_taken_up_is_never_told_again_while_the_node_runs() {
    let quests = quests(&["gone01", "next01"]);
    let mut seen = Seen::default();
    let mut ids = |sessions: &[Session], now: i64| -> Vec<String> {
        told_at(now, &mut seen, sessions, &quests, (&[], &[]))
            .into_iter()
            .map(|(id, _, _)| id)
            .collect()
    };
    let gone = ended_here("gone01");
    ids(std::slice::from_ref(&gone), NOW);
    assert_eq!(
        ids(std::slice::from_ref(&gone), NOW + SETTLED_AFTER),
        ["gone01"]
    );
    assert!(
        ids(&[gone.clone(), running("next01")], NOW + SETTLED_AFTER + 1).is_empty(),
        "a session running on the quest took the work up"
    );
    let next_gone = session("next01", "kestrel", State::Ended, json!({}));
    assert!(
        ids(&[gone, next_gone], NOW + 2 * SETTLED_AFTER + 2).is_empty(),
        "the first end was answered, and the second is its own node's to tell"
    );
}

#[test]
fn only_what_ended_here_on_a_quest_nobody_runs_on_is_told() {
    let cases = [
        (
            "one that ended elsewhere is its own node's to tell",
            vec![session("away01", "kestrel", State::Ended, json!({}))],
            quests(&["away01"]),
        ),
        (
            "one still running hasn't ended",
            vec![session("live01", "skiff", State::Idle, json!({}))],
            quests(&["live01"]),
        ),
        (
            "a quest that isn't active has nothing to carry on",
            vec![ended_here("done01")],
            {
                let mut done = quests(&["done01"]);
                done[0]["status"] = json!("concluded");
                done
            },
        ),
        (
            "a quest nobody orchestrates has nobody to tell",
            vec![ended_here("lone01")],
            {
                let mut lone = quests(&["lone01"]);
                lone[0]["orchestrator"] = Value::Null;
                lone
            },
        ),
        (
            "an orchestrator that doesn't read as an address isn't told",
            vec![ended_here("odd001")],
            {
                let mut odd = quests(&["odd001"]);
                odd[0]["orchestrator"] = json!("#machine");
                odd
            },
        ),
        (
            "no quests served, so no orchestrator can be named",
            vec![ended_here("blind1")],
            Vec::new(),
        ),
    ];
    for (why, sessions, quests) in cases {
        assert_eq!(told(&sessions, &quests, (&[], &[])), [], "{why}");
    }
}

#[test]
fn an_unreachable_session_on_the_quest_hasnt_taken_it_up() {
    let away = session("away01", "kestrel", State::Unreachable, json!({}));
    let told = told(
        &[ended_here("gone01"), away],
        &quests(&["gone01", "away01"]),
        (&[], &[]),
    );
    let ids: Vec<&str> = told.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        ["gone01"],
        "whether it runs is unknown, so it took nothing up"
    );
}

#[test]
fn each_record_is_keyed_by_its_session_in_the_daemon_interface() {
    let made = records(told(
        &[ended_here("gone01")],
        &quests(&["gone01"]),
        (&[], &[]),
    ));
    let record = made.get("ended:gone01").expect("keyed ended:<session>");
    assert_eq!(
        (&record["kind"], &record["heard"], &record["orchestrator"]),
        (&json!("ended"), &json!(NOW), &json!("OARS")),
        "heard when the node first saw it end"
    );
}
