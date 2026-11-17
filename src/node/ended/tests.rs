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

/// Reads an RFC 3339 time as the node does.
fn millis(time: &str) -> i64 {
    time.parse::<jiff::Timestamp>()
        .expect("the tests say whole times")
        .as_millisecond()
}

/// The session `id` on `machine`, standing as `state`, last changed `ago`
/// before now, on `quest` where it says one, with `presence`.
fn session(id: &str, machine: &str, state: State, ago: i64, presence: Value) -> Session {
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
        at: said_at(NOW - ago),
        presence: presence.into(),
    }
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

/// What `ended` makes of `sessions` on skiff against `quests`, each
/// standing as `parked` says: parked where it names it, else unknown.
fn told(sessions: &[Session], quests: &[Value], parked: &[&str]) -> Vec<(String, i64, Value)> {
    let standing = |session: &Session| parked.contains(&session.id.as_str()).then_some("parked");
    ended(sessions, "skiff", &standing, quests, (NOW, &millis))
}

#[test]
fn a_session_here_that_ended_on_an_active_quest_is_told_to_its_orchestrator() {
    let gone = session(
        "gone01",
        "skiff",
        State::Ended,
        HOUR,
        json!({"state": "ended"}),
    );
    let at = gone.at.clone();
    assert_eq!(
        told(&[gone], &quests(&["gone01"]), &[]),
        [(
            "gone01".to_owned(),
            NOW - HOUR,
            json!({
                "session": "gone01", "address": "GONE01", "quest": "QEST-20261116-HARBOR",
                "orchestrator": "OARS", "at": at, "why": "ended",
            })
        )],
        "its record names the quest's orchestrator, by the quests' record alone"
    );
}

#[test]
fn a_park_nothing_can_wake_is_told_and_one_that_can_is_not() {
    let napping = json!({"state": "parked"});
    let stale = session("stale1", "skiff", State::Ended, HOUR, napping.clone());
    let asleep = session("sleep1", "skiff", State::Ended, HOUR, napping);
    let quests = quests(&["stale1", "sleep1"]);
    let told = told(&[stale, asleep], &quests, &["sleep1"]);
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
fn a_session_names_its_quest_itself_or_by_the_quest_that_attaches_it() {
    let mut named = session("name01", "skiff", State::Ended, HOUR, json!({}));
    named.quest = Some("QEST-20261116-HARBOR".to_owned());
    let attached = session("atta01", "skiff", State::Ended, HOUR, json!({}));
    let loose = session("loos01", "skiff", State::Ended, HOUR, json!({}));
    let told = told(&[named, attached, loose], &quests(&["atta01"]), &[]);
    let ids: Vec<&str> = told.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(
        ids,
        ["name01", "atta01"],
        "one on no quest has no orchestrator to tell"
    );
}

#[test]
fn only_what_ended_here_within_a_day_on_a_quest_nobody_took_up_is_told() {
    let ended_here = |id: &str, ago: i64| session(id, "skiff", State::Ended, ago, json!({}));
    let cases = [
        (
            "one that ended elsewhere is its own node's to tell",
            vec![session("away01", "kestrel", State::Ended, HOUR, json!({}))],
            quests(&["away01"]),
        ),
        (
            "one still running hasn't ended",
            vec![session("live01", "skiff", State::Idle, HOUR, json!({}))],
            quests(&["live01"]),
        ),
        (
            "one that ended over a day ago has been told",
            vec![ended_here("old001", KEPT_FOR + HOUR)],
            quests(&["old001"]),
        ),
        (
            "a session running on the quest took the work up",
            vec![
                ended_here("gone01", HOUR),
                session("next01", "kestrel", State::Busy, 0, json!({})),
            ],
            quests(&["gone01", "next01"]),
        ),
        (
            "a quest that isn't active has nothing to carry on",
            vec![ended_here("done01", HOUR)],
            {
                let mut done = quests(&["done01"]);
                done[0]["status"] = json!("concluded");
                done
            },
        ),
        (
            "a quest nobody orchestrates has nobody to tell",
            vec![ended_here("lone01", HOUR)],
            {
                let mut lone = quests(&["lone01"]);
                lone[0]["orchestrator"] = Value::Null;
                lone
            },
        ),
        (
            "no quests served, so no orchestrator can be named",
            vec![ended_here("blind1", HOUR)],
            Vec::new(),
        ),
    ];
    for (why, sessions, quests) in cases {
        assert_eq!(told(&sessions, &quests, &[]), [], "{why}");
    }
}

#[test]
fn each_record_is_keyed_by_its_session_in_the_daemon_interface() {
    let gone = session("gone01", "skiff", State::Ended, HOUR, json!({}));
    let made = records(told(&[gone], &quests(&["gone01"]), &[]));
    let record = made.get("ended:gone01").expect("keyed ended:<session>");
    assert_eq!(
        (&record["kind"], &record["heard"], &record["orchestrator"]),
        (&json!("ended"), &json!(NOW - HOUR), &json!("OARS")),
        "heard when it ended"
    );
}
