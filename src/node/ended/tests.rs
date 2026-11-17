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

/// How the sessions stand, for the tests, as the node's standing says:
/// running where not ended, unreachable ones too, else `parked` where
/// named and as nothing otherwise.
fn standing_of<'a>(parked: &'a [&'a str]) -> impl Fn(&Session) -> Option<&'static str> + 'a {
    move |session: &Session| match session.state {
        State::Ended => parked.contains(&session.id.as_str()).then_some("parked"),
        _ => Some("running"),
    }
}

/// When `at` was written, as the node reads a presence's time.
fn written(at: &str) -> Option<i64> {
    at.parse::<jiff::Timestamp>()
        .ok()
        .map(jiff::Timestamp::as_millisecond)
}

/// The ids of what `told_at` makes.
fn ids_of(told: Vec<(String, i64, Value)>) -> Vec<String> {
    told.into_iter().map(|(id, _, _)| id).collect()
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
        settled_after: SETTLED_AFTER,
        written: &written,
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
    let mut gone = ended_here("gone01");
    gone.at = said_at(NOW + HOUR);
    let sessions = [gone];
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
    let mut attached = ended_here("atta01");
    attached.orchestrated_by = Some("TERN".to_owned());
    let told = told(&[claims, attached], &quests(&["atta01"]), (&[], &[]));
    let said: Vec<(&str, &Value)> = told
        .iter()
        .map(|(id, _, fields)| (id.as_str(), &fields["orchestrator"]))
        .collect();
    assert_eq!(
        said,
        [("atta01", &json!("OARS"))],
        "a presence naming a quest or an orchestrator doesn't choose who is told"
    );
}

#[test]
fn an_end_is_told_only_while_nobody_works_its_quest_and_settles_anew_after() {
    let quests = quests(&["gone01", "next01"]);
    let mut seen = Seen::default();
    let mut ids = |sessions: &[Session], now: i64| {
        ids_of(told_at(now, &mut seen, sessions, &quests, (&[], &[])))
    };
    let gone = ended_here("gone01");
    let worked = [gone.clone(), running("next01")];
    let stopped = [
        gone.clone(),
        session("next01", "kestrel", State::Ended, json!({})),
    ];
    ids(std::slice::from_ref(&gone), NOW);
    assert_eq!(
        ids(std::slice::from_ref(&gone), NOW + SETTLED_AFTER),
        ["gone01"]
    );
    assert!(
        ids(&worked, NOW + SETTLED_AFTER + 1).is_empty(),
        "a session running on the quest carries the work on"
    );
    assert!(
        ids(&stopped, NOW + 2 * SETTLED_AFTER).is_empty(),
        "work stopping again settles before it's told"
    );
    assert_eq!(
        ids(&stopped, NOW + 3 * SETTLED_AFTER - 1),
        Vec::<String>::new()
    );
    assert_eq!(
        ids(&stopped, NOW + 3 * SETTLED_AFTER),
        ["gone01"],
        "the end here is told again once the work has stopped, the mesh side telling it once"
    );
}

#[test]
fn work_while_an_end_settles_restarts_its_settling() {
    let quests = quests(&["gone01", "next01"]);
    let mut seen = Seen::default();
    let mut ids = |sessions: &[Session], now: i64| {
        ids_of(told_at(now, &mut seen, sessions, &quests, (&[], &[])))
    };
    let gone = ended_here("gone01");
    ids(std::slice::from_ref(&gone), NOW);
    ids(&[gone.clone(), running("next01")], NOW + SETTLED_AFTER / 2);
    assert_eq!(
        ids(std::slice::from_ref(&gone), NOW + SETTLED_AFTER),
        Vec::<String>::new(),
        "the work stopped only now, so the end settles from now"
    );
    assert_eq!(
        ids(std::slice::from_ref(&gone), NOW + 2 * SETTLED_AFTER),
        ["gone01"]
    );
}

#[test]
fn a_session_running_on_two_quests_works_each() {
    let mut quests = quests(&["gone01", "both01"]);
    let mut other = quests[0].clone();
    other["id"] = json!("QEST-20261116-SKERRY");
    other["sessions"] = json!(["both01"]);
    quests.push(other);
    assert_eq!(
        told(
            &[ended_here("gone01"), running("both01")],
            &quests,
            (&[], &[])
        ),
        [],
        "whichever of its quests it works, HARBOR's work goes on"
    );
}

#[test]
fn a_session_seen_again_ends_afresh() {
    let quests = quests(&["back01"]);
    let mut seen = Seen::default();
    let mut ids = |session: &Session, now: i64| {
        ids_of(told_at(
            now,
            &mut seen,
            std::slice::from_ref(session),
            &quests,
            (&[], &[]),
        ))
    };
    let gone = ended_here("back01");
    let mut resumed = gone.clone();
    resumed.state = State::Idle;
    ids(&gone, NOW);
    ids(&resumed, NOW + SETTLED_AFTER / 2);
    assert!(
        ids(&gone, NOW + SETTLED_AFTER).is_empty(),
        "ended again, it settles from when it was seen ending this time"
    );
    assert_eq!(ids(&gone, NOW + 2 * SETTLED_AFTER), ["back01"]);
}

#[test]
fn a_session_two_quests_attach_is_nobodys_to_tell() {
    let mut quests = quests(&["both01"]);
    let mut other = quests[0].clone();
    other["id"] = json!("QEST-20261116-SKERRY");
    other["orchestrator"] = json!("tern");
    quests.push(other);
    assert_eq!(
        told(&[ended_here("both01")], &quests, (&[], &[])),
        [],
        "whose orchestrator it is can't be told"
    );
}

#[test]
fn an_end_its_presence_wrote_over_a_day_ago_or_ahead_or_at_no_time_is_not_told() {
    let mut old = ended_here("old001");
    old.at = said_at(NOW - KEPT_FOR - 1);
    let mut ahead = ended_here("next02");
    ahead.at = said_at(NOW + KEPT_FOR + SETTLED_AFTER + 1);
    let mut timeless = ended_here("when01");
    timeless.at = "yesterday".to_owned();
    assert_eq!(
        told(
            &[old, ahead, timeless],
            &quests(&["old001", "next02", "when01"]),
            (&[], &[])
        ),
        [],
        "a node started again doesn't tell what ended long before"
    );
}

#[test]
fn a_session_elsewhere_its_node_says_works_is_working() {
    let quests = quests(&["gone01", "away01"]);
    let sessions = [
        ended_here("gone01"),
        session("away01", "kestrel", State::Idle, json!({})),
    ];
    let coming_back = HashSet::new();
    let judged = Judged {
        machine: "skiff",
        standing: &|_| None,
        coming_back: &coming_back,
        settled_after: SETTLED_AFTER,
        written: &written,
    };
    let mut seen = Seen::default();
    ended(&sessions, &judged, &quests, NOW, &mut seen);
    assert_eq!(
        ended(&sessions, &judged, &quests, NOW + SETTLED_AFTER, &mut seen),
        [],
        "though this boot hasn't heard it, kestrel's node says it's idle"
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
            "a session whose address doesn't read as one isn't named",
            vec![{
                let mut odd = ended_here("odd002");
                odd.address = "#machine".to_owned();
                odd
            }],
            quests(&["odd002"]),
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

/// A carry of `session` ordered `ago` milliseconds before now.
fn carried(session: &str, ago: i64) -> Kept {
    Kept {
        id: format!("{session}-carry"),
        at: jiff::Timestamp::from_millisecond(NOW - ago).expect("the tests stand in this age"),
        line: json!({"session": session}),
    }
}

#[test]
fn a_carry_under_way_or_a_turn_waited_is_coming_back_and_nothing_else() {
    let carries = [
        carried("going1", 0),
        carried("under1", CARRIED_WITHIN),
        carried("slow01", 45 * 60 * 1000),
        carried("stale1", CARRIED_WITHIN + 1),
        carried("refus1", 0),
        carried("back01", 0),
    ];
    let judge = |carry: &Kept| match carry.said("session") {
        "going1" => CarryState::Ordered,
        "refus1" => CarryState::Refused("it couldn't".to_owned()),
        "back01" => CarryState::Back,
        _ => CarryState::Carrying,
    };
    let mut back: Vec<String> = coming_back(&carries, judge, ["turn01"], NOW)
        .into_iter()
        .collect();
    back.sort();
    assert_eq!(
        back,
        ["going1", "slow01", "turn01", "under1"],
        "ordered or under way within the hour, a slow one too, or waiting a turn; never refused, back, \
         or carried over an hour ago and running nowhere"
    );
}

#[test]
fn only_a_carry_within_the_hour_is_judged() {
    let carries = [carried("stale1", CARRIED_WITHIN + 1)];
    let judged = std::cell::Cell::new(0);
    let judge = |_: &Kept| {
        judged.set(judged.get() + 1);
        CarryState::Carrying
    };
    coming_back(&carries, judge, [], NOW);
    assert_eq!(
        judged.get(),
        0,
        "judging reads its handover, so an old one isn't"
    );
}
