//! The tests of machines made from demand ([`crate::node::demand`]): a
//! room kept while it is used, the budget makes are held to, starts
//! packed onto a room, and marks taken from a machine's maker.

use std::collections::BTreeSet;

use super::*;
use crate::declared::{Effect, Taint};
use crate::made::{Doubt, Marks};
use crate::node::demand::{self, Demand, EMPTY_FOR, Makes, Use, WORLD};

/// A node of the realm `work` on skiff, keeping its book in `dir`, that
/// made `machine` for the agent QK4T7Z and named it a room.
fn room_made(dir: &Path, machine: &str) -> (Shared<Memory>, Made) {
    let shared = made_for(dir, &[]);
    let mut made = made_by("/nowhere/lend", machine, a_day_out());
    made.asker = Some("QK4T7Z".to_owned());
    shared.book.made(&made).expect("the make is booked");
    fs::write(dir.join(made::AGENT_ROOMS), format!("{machine}\n")).expect("named a room");
    (shared, made)
}

/// The taint that keeps every cell not naming world off a world machine.
fn world_taint() -> Taint {
    Taint {
        key: WORLD.to_owned(),
        effect: Effect::Never,
        why: "a world machine".to_owned(),
    }
}

/// The marks of a machine its maker says reaches world and carries its
/// taint.
fn world_marks() -> Marks {
    Marks {
        reach: vec![WORLD.to_owned()],
        taints: vec![world_taint()],
    }
}

/// A machine named `name` that says it reaches `reach`.
fn claiming(name: &str, reach: &[&str]) -> facts::Machine {
    placement::tests::machine(name, "linux", 32, reach)
}

fn reaches_world(machine: &facts::Machine) -> bool {
    machine
        .declared
        .as_ref()
        .is_some_and(|declared| declared.reach.iter().any(|one| one == WORLD))
}

fn world_tainted(machine: &facts::Machine) -> bool {
    machine
        .declared
        .as_ref()
        .is_some_and(|declared| declared.taints.iter().any(|one| one.key == WORLD))
}

#[tokio::test(start_paused = true)]
async fn a_room_is_kept_past_its_handover_while_used_and_let_go_once_empty_a_while() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let (shared, made) = room_made(dir.path(), "tern");
    assert!(
        shared.kept_past_handover(&made)(),
        "a room's link keeps it as its first work hands over"
    );
    shared.lock().state.set_sessions(vec![on_quest(
        "s-tern",
        "TERNA1",
        "tern",
        "idle",
        "QEST-20261128-TERNAA",
    )]);
    let dial = shared.book_for(&made);
    let keeping = shared.keeping(&made);
    let kept = tokio::spawn(keep_made(
        dial,
        made,
        keeping,
        Arc::new(Notify::new()),
        std::future::pending(),
    ));

    tokio::time::sleep(EMPTY_FOR * 2).await;
    assert!(
        !kept.is_finished(),
        "a room another cell still runs on is kept, however long"
    );
    shared.lock().state.set_sessions(Vec::new());
    tokio::time::sleep(EMPTY_FOR - Duration::from_secs(60)).await;
    assert!(
        !kept.is_finished(),
        "a room is kept a while after its last session ends"
    );
    tokio::time::timeout(Duration::from_secs(120), kept)
        .await
        .expect("a room empty for its while is let go within a look of it")
        .expect("what keeps it ends whole");

    let book = fs::read_to_string(dir.path().join("made.jsonl")).expect("the book reads");
    assert!(
        book.contains("no session ran there for 20 minutes"),
        "the book says why it was let go: {book}"
    );
    assert!(
        shared.book.standing().expect("the book reads").made.is_empty(),
        "the book says it was let go"
    );
}

#[tokio::test(start_paused = true)]
async fn a_room_a_start_is_on_its_way_to_is_used() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let (shared, _) = room_made(dir.path(), "tern");
    assert_eq!(shared.used("tern"), Use::Empty, "nothing runs there yet");
    shared
        .lock()
        .starts
        .placed("tern", jiff::Timestamp::now());
    assert_eq!(
        shared.used("tern"),
        Use::Used,
        "a start placed there that hasn't shown in its room yet uses it"
    );
}

#[tokio::test(start_paused = true)]
async fn a_machine_made_for_the_person_is_let_go_as_its_work_hands_over() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let shared = made_for(dir.path(), &[]);
    let mut made = made_by("/nowhere/lend", "osprey", a_day_out());
    made.asker = Some(made::PERSON.to_owned());
    shared.book.made(&made).expect("the make is booked");
    // Another machine is a room, so the rooms file is there and read.
    fs::write(dir.path().join(made::AGENT_ROOMS), "tern\n").expect("named a room");
    let dial = shared.book_for(&made);

    assert!(
        !shared.kept_past_handover(&made)(),
        "a machine made for the person is released as its work hands over, as before"
    );
    assert_eq!(shared.used("osprey"), Use::NoRoom);
    assert!(
        tokio::time::timeout(EMPTY_FOR * 6, demand::emptied(|| shared.used("osprey")))
            .await
            .is_err(),
        "a machine that is no room is never let go for standing empty"
    );

    let keeping = shared.keeping(&made);
    tokio::time::timeout(
        PATIENCE,
        keep_made(dial, made, keeping, Arc::new(Notify::new()), async {
            Linked::HandedOver
        }),
    )
    .await
    .expect("its link let it go");
    let book = fs::read_to_string(dir.path().join("made.jsonl")).expect("the book reads");
    assert!(
        book.contains("its work handed over"),
        "the book says its work handed over, as before: {book}"
    );
}

#[test]
fn a_room_is_kept_past_its_handover_only_till_near_its_end() {
    let now = jiff::Timestamp::now();
    let until = now + jiff::SignedDuration::from_hours(2);
    assert!(demand::kept_past_handover(true, now, until));
    assert!(
        !demand::kept_past_handover(false, now, until),
        "a machine that is no room is released as its work hands over"
    );
    assert!(
        !demand::kept_past_handover(true, until - jiff::SignedDuration::from_mins(5), until),
        "a room near its end is released as its work hands over, as any machine made is"
    );
}

/// A budget of `budget` for the realm `work`, kept in `dir`, and a book
/// there standing by one make of kestrel, at 0.12 dollars an hour.
fn budgeted(dir: &Path, budget: &str) -> (Arc<Makes>, Book, Made) {
    let file = dir.join("budget.json");
    fs::write(&file, budget).expect("the budget is written");
    let book = Book::at(dir.join("made.jsonl")).of_realm("work");
    let standing = made_by("/nowhere/lend", "kestrel", a_day_out());
    book.made(&standing).expect("the make is booked");
    (Arc::new(Makes::at(Some(file), "work")), book, standing)
}

#[tokio::test(start_paused = true)]
async fn a_make_past_the_budget_waits_for_room_and_takes_it_once_a_machine_is_let_go() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let (makes, book, standing) = budgeted(dir.path(), r#"{"work": {"machines_at_most": 1}}"#);
    let costs = standing.costs.clone();

    let began = tokio::time::Instant::now();
    let refused = makes
        .room_for(&book, &costs)
        .await
        .err()
        .expect("a make past the budget is refused");
    assert!(
        refused.contains("budget of at most 1 machines made, as 1 are made and not let go")
            && refused.contains("in the 60 seconds it waited"),
        "the refusal says which limit and the counts: {refused}"
    );
    assert!(
        began.elapsed() >= Duration::from_secs(60),
        "it waited for room first"
    );

    let freeing = {
        let (book, standing) = (book.clone(), standing.clone());
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(20)).await;
            book.released(&standing, "its time was up")
                .expect("its release is kept");
        })
    };
    let began = tokio::time::Instant::now();
    let reserved = makes
        .room_for(&book, &costs)
        .await
        .expect("a make takes the room a machine let go frees");
    assert!(
        began.elapsed() >= Duration::from_secs(20),
        "it waited for the room to free"
    );
    freeing.await.expect("the machine was let go");

    let refused = makes
        .room_for(&book, &costs)
        .await
        .err()
        .expect("a make under way counts against the budget");
    assert!(
        refused.contains("as 1 are made and not let go"),
        "the make under way is counted: {refused}"
    );
    drop(reserved);
    makes
        .room_for(&book, &costs)
        .await
        .expect("a make done with frees its room");
}

#[tokio::test(start_paused = true)]
async fn a_make_past_the_dollars_an_hour_is_refused_with_what_they_cost() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let (makes, book, standing) =
        budgeted(dir.path(), r#"{"work": {"usd_an_hour_at_most": "0.20"}}"#);
    let refused = makes
        .room_for(&book, &standing.costs)
        .await
        .err()
        .expect("a make past the dollars an hour is refused");
    assert!(
        refused.contains("budget of at most 0.20 dollars an hour")
            && refused.contains("the 1 machines made and not let go cost 0.12 and it costs 0.12"),
        "the refusal says which limit and the counts: {refused}"
    );
    let free = offer::Costs {
        hourly: "0.00".to_owned(),
        currency: "USD".to_owned(),
    };
    makes
        .room_for(&book, &free)
        .await
        .expect("a machine that costs nothing fits");
}

#[tokio::test(start_paused = true)]
async fn makes_are_unbounded_without_a_budget_and_refused_by_one_that_doesnt_read() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let (unnamed_realm, book, standing) = budgeted(dir.path(), "{}");
    let written = |name: &str, budget: &str| {
        let file = dir.path().join(name);
        fs::write(&file, budget).expect("the budget is written");
        Arc::new(Makes::at(Some(file), "work"))
    };
    let other_realm = written("home.json", r#"{"home": {"machines_at_most": 0}}"#);
    let unnamed = Arc::new(Makes::at(None, "work"));
    let missing = Arc::new(Makes::at(Some(dir.path().join("none.json")), "work"));
    for makes in [unnamed_realm, other_realm, unnamed, missing] {
        makes
            .room_for(&book, &standing.costs)
            .await
            .expect("a make no budget names is unbounded");
    }
    let broken = written("broken.json", r#"{"work": {"machines": 4}}"#);
    let began = tokio::time::Instant::now();
    let refused = broken
        .room_for(&book, &standing.costs)
        .await
        .err()
        .expect("a budget that doesn't read refuses every make");
    assert!(
        refused.contains("doesn't read, so nothing is made"),
        "{refused}"
    );
    assert_eq!(
        began.elapsed(),
        Duration::ZERO,
        "at once, as no room will come"
    );
}

#[tokio::test]
async fn starts_pack_onto_a_room_with_room_before_any_make() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let shared = made_for(dir.path(), &[("tern", "P9R1KI")]);
    fs::write(dir.path().join(made::AGENT_ROOMS), "tern\n").expect("named a room");
    shared.lock().promised.insert("tern".to_owned(), 3);
    let agent = Admits::Asker("QK4T7Z".to_owned());
    let placed = || match shared.place(
        &work(vec![Need::Reach("basin".to_owned())]),
        Counts::Always,
        None,
        Screen::Ground,
        &agent,
    ) {
        Placement::Placed { machine, .. } => Some(machine),
        _ => None,
    };
    let tern = || Some("tern".to_owned());
    assert_eq!(
        [placed(), placed(), placed()],
        [tern(), tern(), tern()],
        "a second and a third start land on the room made, so none is made for them"
    );
    assert_eq!(
        placed(),
        None,
        "past the sessions it holds, a start goes to no room, and only then is one made"
    );
}

#[test]
fn a_made_machine_reaches_world_and_carries_its_taint_only_as_its_make_says() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let shared = made_for(dir.path(), &[]);
    for name in ["tern", "wren", "kestrel"] {
        shared
            .store
            .put_machine(&claiming(name, &["basin", WORLD]))
            .expect("what it says is kept");
    }
    let mut marked = made_by("/usr/bin/lend", "tern", a_day_out());
    marked.marks = Some(world_marks());
    shared.book_for(&marked);
    // Kept before makes said their marks.
    shared.book_for(&made_by("/usr/bin/lend", "kestrel", a_day_out()));

    let others: BTreeMap<String, facts::Machine> = shared
        .others()
        .into_iter()
        .map(|one| (one.name.clone(), one))
        .collect();
    let tern = &others["tern"];
    assert!(
        reaches_world(tern) && world_tainted(tern),
        "the machine its maker marked reaches world and carries its taint"
    );
    assert!(
        tern.declared
            .as_ref()
            .is_some_and(|declared| declared.reach.iter().any(|one| one == "basin")),
        "and still reaches what it says it does"
    );
    for name in ["wren", "kestrel"] {
        assert!(
            !reaches_world(&others[name]) && !world_tainted(&others[name]),
            "{name}, unmarked, doesn't reach world for saying so"
        );
    }

    let mut world_work = work(vec![Need::Reach(WORLD.to_owned())]);
    world_work.tolerates = vec![WORLD.to_owned()];
    let person = Admits::Asker(made::PERSON.to_owned());
    let placed = |work: &Work| match shared.place(work, Counts::Never, None, Screen::Ground, &person)
    {
        Placement::Placed { machine, .. } => Some(machine),
        _ => None,
    };
    assert_eq!(
        placed(&world_work).as_deref(),
        Some("tern"),
        "world work lands only on the machine its maker marked"
    );
    let mut on_tern = work(vec![Need::Machine("tern".to_owned())]);
    assert_eq!(
        placed(&on_tern),
        None,
        "work not naming world never lands on a world machine"
    );
    on_tern.tolerates = vec![WORLD.to_owned()];
    assert_eq!(placed(&on_tern).as_deref(), Some("tern"));
}

#[test]
fn a_machine_whose_make_cant_be_read_marks_nothing_and_takes_nothing_unmarked() {
    let demand = Demand::of(demand::Given::default(), "work");
    let mut kestrel = claiming("kestrel", &["basin", WORLD]);
    let doubt = Doubt::Of {
        machines: BTreeMap::from([("kestrel".to_owned(), BTreeSet::from([None]))]),
        why: "line 3 was cut short".to_owned(),
    };
    demand.mark(&mut kestrel, &doubt);
    assert!(
        !reaches_world(&kestrel),
        "a machine whose make may be lost doesn't reach world"
    );
    assert!(
        world_tainted(&kestrel),
        "and carries the world taint, so nothing unmarked lands on it"
    );

    let mut wren = claiming("wren", &["basin"]);
    demand.mark(&mut wren, &Doubt::Any("it can't be read".to_owned()));
    assert!(
        !world_tainted(&wren),
        "a doubt naming no machine taints none this node didn't make"
    );
}

#[test]
fn a_machine_that_carried_the_world_taint_keeps_it_till_let_go() {
    let demand = Demand::of(demand::Given::default(), "work");
    let mut made = made_by("/usr/bin/lend", "tern", a_day_out());
    made.marks = Some(world_marks());
    demand.booked(&made);
    let mut tern = claiming("tern", &[]);
    demand.mark(&mut tern, &Doubt::None);
    assert!(world_tainted(&tern));

    // A make of its name since, marking nothing, as one lost and made
    // again might.
    let mut again = made.clone();
    again.marks = None;
    "tern-again".clone_into(&mut again.place);
    demand.booked(&again);
    let mut tern = claiming("tern", &[]);
    demand.mark(&mut tern, &Doubt::None);
    assert!(
        world_tainted(&tern),
        "a machine that carried the world taint keeps it, whatever is said since"
    );

    demand.unbooked(&again);
    let mut tern = claiming("tern", &[]);
    demand.mark(&mut tern, &Doubt::None);
    assert!(!world_tainted(&tern), "once let go, it carries it no longer");
}

#[test]
fn a_via_machine_is_marked_only_by_its_via_marks() {
    let harbour = Program::parse("/usr/bin/lend:harbour-1").expect("a --via");
    let skiff = Program::parse("/usr/bin/lend:skiff-1").expect("a --via");
    let given = demand::via_marks(
        &[harbour.clone(), skiff.clone()],
        vec![Marks::of_via(r#"harbour-1={"reach":["world"],"taints":[]}"#).expect("marks")],
    )
    .expect("each marks a --via");
    harbour.named.set("harbour".to_owned()).expect("named once");
    skiff.named.set("osprey".to_owned()).expect("named once");
    let demand = Demand::of(
        demand::Given {
            via_marks: given,
            makes_within: None,
        },
        "work",
    );
    let mut marked = claiming("harbour", &["basin"]);
    demand.mark(&mut marked, &Doubt::None);
    assert!(
        reaches_world(&marked),
        "a --via machine the person's configuration marks reaches world"
    );
    let mut unmarked = claiming("osprey", &[WORLD]);
    unmarked
        .declared
        .as_mut()
        .expect("declared")
        .taints
        .push(world_taint());
    demand.mark(&mut unmarked, &Doubt::None);
    assert!(
        !reaches_world(&unmarked) && world_tainted(&unmarked),
        "one with none is read as unmade: no world, its own taints kept"
    );

    assert!(
        demand::via_marks(&[skiff], vec![("harbour-1".to_owned(), Marks::default())])
            .err()
            .is_some_and(|why| why.contains("no --via dials it")),
        "marks for a place no --via dials are refused"
    );
}

#[test]
fn via_marks_read_as_a_place_and_its_marks() {
    let (place, marks) =
        Marks::of_via(r#"wren-1={"reach":["world"],"taints":[{"key":"world","effect":"never","why":"a world machine"}]}"#)
            .expect("they read");
    assert_eq!((place.as_str(), marks), ("wren-1", world_marks()));
    for wrong in ["wren-1", r#"={"reach":[]}"#, "wren-1={", r#"wren-1={"reaches":[]}"#] {
        assert!(Marks::of_via(wrong).is_err(), "{wrong} is refused");
    }
}
