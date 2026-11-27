//! The store's own tests: its sweeps, marks and changes, over the fake.

use super::*;

/// Changes waiting behind the next are taken with it, each once and
/// in the order first seen, and the next waits for what comes after.
#[tokio::test]
async fn waiting_changes_are_told_once_each() {
    let (telling, told) = mpsc::unbounded_channel();
    let mut changes = Changes {
        changes: told,
        keep: Box::new(()),
    };
    for change in [
        Change::Presence,
        Change::Message("m1".to_owned()),
        Change::Presence,
        Change::Carries("wren".to_owned()),
        Change::Message("m1".to_owned()),
        Change::Presence,
    ] {
        telling.send(change).expect("the change is told");
    }
    assert_eq!(
        changes.gathered().await,
        Some(vec![
            Change::Presence,
            Change::Message("m1".to_owned()),
            Change::Carries("wren".to_owned()),
        ])
    );
    telling.send(Change::Presence).expect("the change is told");
    assert_eq!(changes.gathered().await, Some(vec![Change::Presence]));
    drop(telling);
    assert_eq!(changes.gathered().await, None);
}

/// No more than [`GATHERED_AT_MOST`] are taken at once, so the rest
/// wait for the next.
#[tokio::test]
async fn a_flood_of_changes_is_taken_in_parts() {
    let (telling, told) = mpsc::unbounded_channel();
    let mut changes = Changes {
        changes: told,
        keep: Box::new(()),
    };
    for id in 0..=GATHERED_AT_MOST {
        telling
            .send(Change::Message(id.to_string()))
            .expect("the change is told");
    }
    let first = changes.gathered().await.expect("changes are told");
    assert_eq!(first.len(), GATHERED_AT_MOST);
    assert_eq!(
        changes.gathered().await,
        Some(vec![Change::Message(GATHERED_AT_MOST.to_string())])
    );
}

/// A machine is a worker as its link last said, by its admission or a
/// put, over the fake as over folders: a link's drop and its machine's
/// leaving put it back as they read it, so it stays one through both,
/// and only a put saying it is none unmakes it.
#[test]
fn a_worker_stays_one_until_its_link_says_it_is_none() {
    let store = memory::Memory::default();
    let worker = |store: &memory::Memory| {
        store
            .machines()
            .into_iter()
            .find(|one| one.name == "tern")
            .map(|one| one.worker)
    };
    let mut tern = crate::placement::tests::machine("tern", "linux", 32, &[]);
    tern.worker = false;
    store.put_machine(&tern).expect("kept");
    assert_eq!(worker(&store), Some(false), "a put saying none makes none");
    store.mark_worker("tern", true);
    assert_eq!(
        worker(&store),
        Some(true),
        "its link's admission makes one, whatever its record said"
    );
    out_of_reach(&store, "tern", 1_793_305_804_647, None);
    left(&store, "tern", jiff::Timestamp::UNIX_EPOCH);
    assert_eq!(
        worker(&store),
        Some(true),
        "a link's drop and its leaving keep it one"
    );
    store.put_machine(&tern).expect("kept");
    assert_eq!(
        worker(&store),
        Some(false),
        "a put saying it is none unmakes one"
    );
    store.mark_worker("tern", true);
    store.mark_worker("tern", false);
    assert!(
        !store.is_worker("tern"),
        "a link let in as none unmakes an earlier link's mark"
    );
}

/// A machine let go while its link holds its claim, as a link let in
/// already does, still leaves: its record is put back with its
/// `leaves_at` all the same, and it stays a worker, as its link said.
/// Said to leave again, it keeps the soonest.
#[test]
fn a_machine_left_while_its_link_holds_its_claim_still_leaves() {
    // Followup 544(4): left can't take a claim its link holds, and must say it left anyway.
    let store = memory::Memory::default();
    let said = |store: &memory::Memory| {
        store
            .machines()
            .into_iter()
            .find(|one| one.name == "tern")
            .expect("tern is among the machines")
    };
    let leaves = |store: &memory::Memory| said(store).declared.and_then(|one| one.leaves_at);
    let mut tern = crate::placement::tests::machine("tern", "linux", 32, &[]);
    tern.declared = Some(crate::declared::Declared::default());
    store.put_machine(&tern).expect("kept");
    let claim = store
        .claim("tern")
        .expect("claimed")
        .expect("no link holds it yet");
    store.mark_worker("tern", true);

    let hour = jiff::SignedDuration::from_hours(1);
    let at = jiff::Timestamp::UNIX_EPOCH + hour * 498_140;
    left(&store, "tern", at);
    assert_eq!(
        leaves(&store),
        Some(at),
        "its record is put back as leaving, though its link holds the claim"
    );
    assert!(
        store.is_worker("tern"),
        "it stays a worker, as its link said"
    );
    assert!(said(&store).worker, "and its record reads so");

    left(&store, "tern", at + hour);
    assert_eq!(
        leaves(&store),
        Some(at),
        "a later leaving keeps the sooner one"
    );
    left(&store, "tern", at - hour);
    assert_eq!(
        leaves(&store),
        Some(at - hour),
        "a sooner leaving takes its place"
    );
    drop(claim);
}

/// A machine that declared nothing has no `leaves_at` to say it left by,
/// so it stays among the machines, declaring nothing still.
#[test]
fn a_machine_that_declared_nothing_stays_when_it_leaves() {
    // Followup 544(4): left's doc says one that declared nothing stays; this holds it to that.
    let store = memory::Memory::default();
    let mut tern = crate::placement::tests::machine("tern", "linux", 32, &[]);
    tern.declared = None;
    store.put_machine(&tern).expect("kept");

    left(&store, "tern", jiff::Timestamp::UNIX_EPOCH);
    let kept = store
        .machines()
        .into_iter()
        .find(|one| one.name == "tern")
        .expect("it stays among the machines");
    assert_eq!(kept.declared, None, "and still declares nothing");
}

/// A worker's mark that moves is told as its machine's change, as a put
/// is, so a watch of the machines sees an admission that says nothing
/// more; one that moves nothing tells nothing.
#[tokio::test]
async fn a_worker_mark_that_moves_is_told() {
    let store = memory::Memory::default();
    let mut changes = store.changes().expect("the fake tells its changes");
    store.mark_worker("tern", true);
    store.mark_worker("tern", true);
    store.mark_worker("tern", false);
    store.mark_worker("tern", false);
    let tern = Change::Machine("tern".to_owned());
    assert_eq!(changes.changes.recv().await, Some(tern.clone()), "made");
    assert_eq!(changes.changes.recv().await, Some(tern), "unmade");
    assert!(
        changes.changes.try_recv().is_err(),
        "a mark that moves nothing tells nothing"
    );
}

/// A machine let go while a link still holds its claim, as one whose
/// time is up while its link runs, leaves all the same: that link's
/// claim holds no put back.
#[test]
fn a_machine_let_go_while_its_link_holds_it_leaves() {
    let store = memory::Memory::default();
    let mut tern = crate::placement::tests::machine("tern", "linux", 32, &[]);
    tern.declared = Some(crate::declared::Declared::default());
    tern.worker = true;
    store.put_machine(&tern).expect("kept");
    let held = store.claim("tern").expect("the fake claims");
    assert!(held.is_some(), "its link holds it");
    left(&store, "tern", jiff::Timestamp::UNIX_EPOCH);
    let kept = store
        .machines()
        .into_iter()
        .find(|one| one.name == "tern")
        .expect("still kept");
    assert_eq!(
        kept.declared.and_then(|declared| declared.leaves_at),
        Some(jiff::Timestamp::UNIX_EPOCH),
        "it leaves though its link holds it"
    );
    assert!(kept.worker, "and stays a worker as its link said");
    drop(held);
}

/// A machine whose last link was let in after its mark, as a new one
/// holding the mark back until its catch-up stands is, and that no
/// link carries now, as after a node killed in that link's first
/// minute, is out of reach since that link was let in, not since the
/// days-old mark.
#[test]
fn a_machine_no_link_carries_is_out_of_reach_since_it_was_last_let_in() {
    let store = memory::Memory::default();
    let (days_ago, let_in) = (1_793_046_604_647, 1_793_305_804_647);
    store
        .put_presence("s1", "kestrel", &serde_json::json!({"session": "s1"}))
        .expect("carried");
    store.mark_linked("kestrel", days_ago).expect("marked");
    store.mark_admitted("kestrel", let_in).expect("let in");

    out_of_reach_of_unclaimed(&store, let_in).expect("swept");
    let since = store
        .presences()
        .into_iter()
        .find(|one| one.session == "s1")
        .and_then(|one| one.unreachable_since);
    assert_eq!(since, Some(let_in), "out of reach since it was let in");
}

/// Easing times a lane out of reach by the `unreachableSince` and
/// `unreachableSeen` in its presence, so the store in memory marks them
/// there, as the folders do, once and no later, and a link carrying the
/// session again clears them, even where the body it carries names
/// times or an owner of its own.
#[test]
fn a_session_out_of_reach_says_since_when_in_its_presence() {
    let store = memory::Memory::default();
    let said = serde_json::json!({"session": "s1"});
    store.put_presence("s1", "kestrel", &said).expect("carried");
    let body = |store: &memory::Memory, field: &str| {
        store
            .presences()
            .into_iter()
            .find(|one| one.session == "s1")
            .map(|one| one.body[field].clone())
    };
    out_of_reach(&store, "kestrel", 7, Some(8));
    out_of_reach(&store, "kestrel", 9, Some(10));
    assert_eq!(
        (
            body(&store, "unreachableSince"),
            body(&store, "unreachableSeen")
        ),
        (Some(serde_json::json!(7)), Some(serde_json::json!(8))),
        "since the first drop, as first seen"
    );
    let carried = serde_json::json!({
        "session": "s1",
        "unreachableSince": 3,
        "unreachableSeen": 4,
        "unreachableSeenFor": 5,
        "owner": {"pid": 1},
    });
    store
        .put_presence("s1", "kestrel", &carried)
        .expect("carried again");
    assert_eq!(
        [
            "unreachableSince",
            "unreachableSeen",
            "unreachableSeenFor",
            "owner"
        ]
        .map(|field| body(&store, field)),
        [const { Some(Value::Null) }; 4],
        "a link carrying it again says it is in reach, and only its own machine \
         judges its process"
    );
}

/// A node killed and started again finds the marks its last run left,
/// which say when that run first saw each machine gone, and keeps them;
/// a session it marks at this start is seen gone from this start, though
/// out of reach since its machine was last linked.
#[test]
fn a_restart_keeps_when_a_machine_was_first_seen_gone() {
    let store = memory::Memory::default();
    for session in ["s1", "s2"] {
        store
            .put_presence(session, "kestrel", &serde_json::json!({"session": session}))
            .expect("carried");
    }
    store
        .mark_unreachable("s1", "kestrel", 7, Some(8))
        .expect("marked");
    store
        .mark_linked("kestrel", 7)
        .expect("linked until it dropped");

    let before = jiff::Timestamp::now().as_millisecond();
    out_of_reach_of_unclaimed(&store, before).expect("swept");
    let after = jiff::Timestamp::now().as_millisecond();
    let seen = |session: &str| {
        store
            .presences()
            .into_iter()
            .find(|one| one.session == session)
            .map(|one| one.body["unreachableSeen"].clone())
    };
    assert_eq!(
        seen("s1"),
        Some(serde_json::json!(8)),
        "a mark this node's last run saw keeps when it saw it"
    );
    let started = seen("s2")
        .as_ref()
        .and_then(Value::as_i64)
        .expect("a session marked at this start says when it was seen");
    assert!(
        (before..=after).contains(&started),
        "seen gone from this start, not before it"
    );

    store
        .mark_unreachable("s1", "kestrel", 3, Some(4))
        .expect("marked");
    assert_eq!(
        seen("s1"),
        Some(serde_json::json!(8)),
        "and never seen again"
    );
}

/// A node stopped puts away what it saw of each machine out of reach,
/// after sweeping the machines no link carries, so the hours it is down
/// aren't seen: its next start sees the machine gone afresh from then,
/// with what it saw before stopping kept. Swept after the put-away, a
/// mark would be seen from the stop and keep that at the next start.
#[test]
fn a_stopped_node_sees_none_of_its_time_down() {
    let store = memory::Memory::default();
    for session in ["s1", "s2"] {
        store
            .put_presence(session, "kestrel", &serde_json::json!({"session": session}))
            .expect("carried");
    }
    let field = |session: &str, name: &str| {
        store
            .presences()
            .into_iter()
            .find(|one| one.session == session)
            .map(|one| one.body[name].clone())
    };
    let before = jiff::Timestamp::now().as_millisecond();
    // s1 dropped a minute ago as this node ran; s2 no link left marked.
    store
        .mark_unreachable("s1", "kestrel", 7, Some(before - 60_000))
        .expect("marked");

    // The stop is said a minute on, so what the sweep saw is a minute.
    stop_seeing_out_of_reach(&store, before, before + 60_000).expect("stopped");
    let after = jiff::Timestamp::now().as_millisecond();
    assert_eq!(
        (
            field("s1", "unreachableSeen"),
            field("s1", "unreachableSeenFor")
        ),
        (Some(Value::Null), Some(serde_json::json!(120_000))),
        "seen from its drop to the stop, and no longer seen as the node is down"
    );
    let swept = field("s2", "unreachableSeenFor")
        .as_ref()
        .and_then(Value::as_i64)
        .expect("the stop's sweep marks it, and the put-away puts that away");
    assert!(
        (60_000 - (after - before)..=60_000).contains(&swept),
        "seen from the stop's sweep to the stop"
    );
    assert_eq!(
        field("s2", "unreachableSeen"),
        Some(Value::Null),
        "and no longer seen"
    );
    store
        .put_away_unreachable("s1", "kestrel", before + 600_000)
        .expect("put away again");
    assert_eq!(
        field("s1", "unreachableSeenFor"),
        Some(serde_json::json!(120_000)),
        "a mark with nothing seen this run has nothing more to put away"
    );

    let before = jiff::Timestamp::now().as_millisecond();
    out_of_reach_of_unclaimed(&store, before).expect("swept at the next start");
    let after = jiff::Timestamp::now().as_millisecond();
    let seen = field("s1", "unreachableSeen")
        .as_ref()
        .and_then(Value::as_i64)
        .expect("seen again from the start");
    assert!(
        (before..=after).contains(&seen),
        "seen gone from this start, not from the stop"
    );
    assert_eq!(
        (
            field("s1", "unreachableSince"),
            field("s1", "unreachableSeenFor")
        ),
        (Some(serde_json::json!(7)), Some(serde_json::json!(120_000))),
        "still out of reach since 7, with what was seen before the stop"
    );
}

/// Each stop adds what its run saw to what earlier runs saw, so a node
/// restarted more often than hourly still lets a gone machine's lanes
/// go; a stop said before a sight adds nothing, a sight is never before
/// the drop, a link apart from the node marks only the drop, and a
/// machine another link claims is that link's to clear.
#[test]
fn each_stop_adds_its_run_to_what_earlier_runs_saw() {
    let store = memory::Memory::default();
    for (session, machine) in [("s1", "kestrel"), ("s2", "kestrel"), ("s3", "tern")] {
        store
            .put_presence(session, machine, &serde_json::json!({"session": session}))
            .expect("carried");
    }
    let field = |session: &str, name: &str| {
        store
            .presences()
            .into_iter()
            .find(|one| one.session == session)
            .map(|one| one.body[name].clone())
    };
    let runs = [(10, 70, 60), (100, 130, 90), (200, 150, 90)];
    for (seen, at, all) in runs {
        store
            .mark_unreachable("s1", "kestrel", 7, Some(seen))
            .expect("seen by this run");
        put_away_out_of_reach(&store, at);
        assert_eq!(
            field("s1", "unreachableSeenFor"),
            Some(serde_json::json!(all)),
            "seen from {seen} to a stop at {at}, joined to what earlier runs saw"
        );
    }

    store
        .mark_unreachable("s2", "kestrel", 50, Some(20))
        .expect("marked");
    assert_eq!(
        field("s2", "unreachableSeen"),
        Some(serde_json::json!(50)),
        "a clock behind the drop sees it no sooner than it went"
    );
    store
        .mark_unreachable("s3", "tern", 40, None)
        .expect("marked apart from the node");
    assert_eq!(
        (
            field("s3", "unreachableSince"),
            field("s3", "unreachableSeen")
        ),
        (Some(serde_json::json!(40)), Some(Value::Null)),
        "a link apart from the node marks when it went, and leaves the seeing"
    );
    store
        .mark_unreachable("s3", "tern", 45, Some(60))
        .expect("seen");
    let _claim = store
        .claim("tern")
        .expect("claimed")
        .expect("by another link");
    put_away_out_of_reach(&store, 90);
    assert_eq!(
        (
            field("s3", "unreachableSeen"),
            field("s3", "unreachableSeenFor")
        ),
        (Some(serde_json::json!(60)), Some(Value::Null)),
        "a machine another link claims is left to it"
    );
}

/// The store in memory tells nothing of a mark kept as it stands, as the
/// folders write nothing for one, and tells a sight that changes it.
#[test]
fn a_mark_kept_as_it_stands_tells_nothing() {
    let store = memory::Memory::default();
    store
        .put_presence("s1", "kestrel", &serde_json::json!({"session": "s1"}))
        .expect("carried");
    store
        .mark_unreachable("s1", "kestrel", 7, None)
        .expect("marked apart from the node");
    let mut changes = store.changes().expect("a memory store tells its changes");

    store
        .mark_unreachable("s1", "kestrel", 9, None)
        .expect("marked again");
    assert!(
        changes.changes.try_recv().is_err(),
        "the first drop kept, unsaid"
    );
    store
        .mark_unreachable("s1", "kestrel", 9, Some(10))
        .expect("seen");
    assert_eq!(
        changes.changes.try_recv().ok(),
        Some(Change::Presence),
        "a sight is said"
    );
}

/// A link running apart from the node leaves its drop unseen, so the
/// stop counts what this run saw of it from the drop, or from when the
/// run began where it went before: a node restarted more often than
/// hourly still lets such a machine's lanes go.
#[test]
fn a_stop_counts_a_drop_left_unseen_from_when_this_run_saw_it() {
    let store = memory::Memory::default();
    for (session, machine, since) in [("s1", "kestrel", 40), ("s2", "tern", 5)] {
        store
            .put_presence(session, machine, &serde_json::json!({"session": session}))
            .expect("carried");
        store
            .mark_unreachable(session, machine, since, None)
            .expect("marked apart from the node");
    }

    stop_seeing_out_of_reach(&store, 10, 100).expect("stopped");
    let seen_for = |session: &str| {
        store
            .presences()
            .into_iter()
            .find(|one| one.session == session)
            .map(|one| one.body["unreachableSeenFor"].clone())
    };
    assert_eq!(
        [seen_for("s1"), seen_for("s2")],
        [Some(serde_json::json!(60)), Some(serde_json::json!(90))],
        "from the drop at 40, and from the run's start at 10 for one gone at 5"
    );
}
