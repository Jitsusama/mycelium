//! The node's stop and the lends in flight: none is taken once it has
//! begun, each in flight runs to its end, and what is left at the bound,
//! or at a second stop, is cut, saying so. The clock is paused, so the
//! bound passes as soon as nothing else can happen.

use std::future::pending;

use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::spending::{RESERVED, USED, used_whole};
use super::*;

/// A lender of models to `agents-` machines, with no key to have, so
/// nothing it does waits on a program of its own.
fn keyless() -> Lending {
    Lending::with(&json!({
        "vms": [{"prefix": "agents-", "budget": "models", "requests_a_day": 100}],
        "budgets": {"models": {
            "upstream": "relay/agents.sock",
            "host": "models.example",
            "paths": ["/v1/messages"],
            "auth_header": "authorization",
        }},
    }))
}

/// A model call of `machine`'s carried as [`answer`] carries it: what its
/// line says of it once it ends, and each piece of its answer.
fn carried_for(
    lender: &Arc<Lender>,
    machine: &str,
) -> (JoinHandle<(String, Used)>, mpsc::Receiver<Lent>) {
    let (sending, answers) = mpsc::channel(BACKLOG);
    let in_flight = lender.lends.begun();
    let (lender, machine) = (lender.clone(), machine.to_owned());
    let carrying = tokio::spawn(async move {
        carry(&lender, &machine, lend("/v1/messages"), &sending, in_flight).await
    });
    (carrying, answers)
}

/// The node's stop draining `lender`'s lends, `again` its second stop:
/// how long the drain took.
fn draining(
    lender: &Arc<Lender>,
    again: impl Future<Output = ()> + Send + 'static,
) -> JoinHandle<Duration> {
    let lender = lender.clone();
    tokio::spawn(async move {
        let began = Instant::now();
        lender.drain(LENDS_DRAIN_WITHIN, again).await;
        began.elapsed()
    })
}

/// Once `lender` takes no more lends, its node's stop having begun.
async fn stopping(lender: &Lender) {
    let mut standing = lender.lends.0.subscribe();
    standing
        .wait_for(|standing| standing.stopping)
        .await
        .expect("the lender keeps its lends");
}

#[tokio::test]
async fn a_lend_in_flight_at_the_stop_runs_to_its_end_and_is_charged_whole() {
    let mut lending = keyless();
    let mut answers = answer(&lending.lender, "agents-wren", lend("/v1/messages"));
    let asked = lending.relay.asked().await;
    tokio::time::pause();
    let drained = draining(&lending.lender, pending());
    stopping(&lending.lender).await;
    // The answer comes a second into the stop, so a drain that waits
    // for it takes that second.
    let answered_after = Duration::from_secs(1);
    tokio::time::advance(answered_after).await;
    asked.answer.send(used_whole()).expect("the relay answers");
    let (lent, _) = whole(&mut answers).await;
    assert!(
        matches!(lent.first(), Some(Lent::Head { status: 200, .. })),
        "{lent:?}"
    );
    assert_eq!(lent.last(), Some(&Lent::Done), "{lent:?}");
    // As the link lets an answer go once it has taken its last piece.
    drop(answers);
    let took = drained.await.expect("the drain ends");
    assert!(
        (answered_after..LENDS_DRAIN_WITHIN).contains(&took),
        "the stop ends with its last lend, not before nor at the bound: {took:?}"
    );
    assert_eq!(
        lending.spent("agents-wren"),
        USED,
        "it is charged what its answer says it used"
    );
}

#[tokio::test]
async fn the_stop_waits_for_a_whole_answer_to_leave_not_only_to_be_queued() {
    let mut lending = keyless();
    let mut answers = answer(&lending.lender, "agents-harbour", lend("/v1/messages"));
    let asked = lending.relay.asked().await;
    asked.answer.send(used_whole()).expect("the relay answers");
    let (lent, _) = whole(&mut answers).await;
    assert_eq!(lent.last(), Some(&Lent::Done), "{lent:?}");
    tokio::time::pause();
    // Its last piece is queued, but the link hasn't let the answer go.
    let drained = draining(&lending.lender, pending());
    stopping(&lending.lender).await;
    let taken_after = Duration::from_secs(2);
    tokio::time::advance(taken_after).await;
    assert!(!drained.is_finished(), "the stop still counts it in flight");
    drop(answers);
    let took = drained.await.expect("the drain ends");
    assert!(
        (taken_after..LENDS_DRAIN_WITHIN).contains(&took),
        "the stop ends once the link took it, before the bound: {took:?}"
    );
}

#[tokio::test]
async fn a_lend_asked_while_another_drains_is_refused_as_one_to_ask_again() {
    let mut lending = keyless();
    let (carrying, _held) = carried_for(&lending.lender, "agents-wren");
    // Held, so the relay answers nothing and the first lend stays in flight.
    let _asked = lending.relay.asked().await;
    tokio::time::pause();
    let drained = draining(&lending.lender, pending());
    stopping(&lending.lender).await;
    let mut answers = answer(&lending.lender, "agents-tern", lend("/v1/messages"));
    let (status, why) = refusal_and_why(&mut answers).await;
    assert_eq!(
        (status, why.as_str()),
        (503, STOPPING),
        "refused as stopping while the stop still waits on another"
    );
    assert!(!drained.is_finished(), "and the drain still waits");
    assert_eq!(lending.spent("agents-tern"), 0, "it spends nothing");
    drained.await.expect("the drain ends at its bound");
    let (outcome, _) = carrying.await.expect("it was carried");
    assert_eq!(outcome, CUT);
}

#[tokio::test(start_paused = true)]
async fn a_lend_asked_once_the_stop_began_is_refused_as_one_to_ask_again() {
    let mut lending = keyless();
    draining(&lending.lender, pending())
        .await
        .expect("with none in flight the drain ends");
    let mut answers = answer(&lending.lender, "agents-tern", lend("/v1/messages"));
    let (status, why) = refusal_and_why(&mut answers).await;
    assert_eq!(
        (status, why.as_str()),
        (503, STOPPING),
        "unavailable, which pi asks again on"
    );
    assert!(!lending.relay.was_asked(), "it isn't carried");
    assert_eq!(lending.spent("agents-tern"), 0, "and spends nothing");
}

#[tokio::test]
async fn a_lend_still_going_at_the_bound_is_cut_by_the_stop_and_says_so() {
    let mut lending = keyless();
    let (carrying, mut answers) = carried_for(&lending.lender, "agents-kestrel");
    let asked = lending.relay.asked().await;
    asked
        .answer
        .send(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n5\r\nhello\r\n".to_vec())
        .expect("the relay answers");
    assert!(matches!(next(&mut answers).await, Some(Lent::Head { .. })));
    assert!(matches!(next(&mut answers).await, Some(Lent::Body(_))));
    tokio::time::pause();
    let took = draining(&lending.lender, pending())
        .await
        .expect("the drain ends");
    assert!(
        took >= LENDS_DRAIN_WITHIN,
        "it waits out the bound: {took:?}"
    );
    let (outcome, used) = carrying.await.expect("it was carried");
    assert_eq!(
        outcome, "cut by the node's stop",
        "not as a borrower that stopped reading"
    );
    assert_eq!(used.response_bytes, 5, "its line says what it had sent");
    assert!(
        matches!(
            next(&mut answers).await,
            Some(Lent::Refused { status: 503, .. })
        ),
        "its borrower is told it ended short"
    );
    assert_eq!(
        lending.spent("agents-kestrel"),
        RESERVED,
        "charged as any answer cut short is"
    );
    tokio::time::timeout(PATIENCE, asked.left)
        .await
        .expect("the relay is hung up on in time")
        .expect("the fake relay tells");
}

#[tokio::test(start_paused = true)]
async fn a_node_with_no_lends_in_flight_stops_at_once() {
    let lending = keyless();
    let mut answers = answer(&lending.lender, "agents-osprey", lend("/v2/elsewhere"));
    assert_eq!(
        refusal(&mut answers).await,
        403,
        "a lend ended before the stop"
    );
    let began = Instant::now();
    lending.lender.drain(LENDS_DRAIN_WITHIN, pending()).await;
    assert_eq!(began.elapsed(), Duration::ZERO, "it doesn't wait at all");
}

#[tokio::test]
async fn a_second_stop_ends_the_drain_at_once_and_cuts_what_is_left() {
    let mut lending = keyless();
    let (carrying, _answers) = carried_for(&lending.lender, "agents-skiff");
    // Held, so the relay answers nothing and the lend waits on its head.
    let _asked = lending.relay.asked().await;
    tokio::time::pause();
    let (again, stopped_again) = oneshot::channel::<()>();
    let drained = draining(&lending.lender, async {
        let _ = stopped_again.await;
    });
    stopping(&lending.lender).await;
    again.send(()).expect("the drain hears a second stop");
    let took = drained.await.expect("the drain ends");
    assert!(
        took < LENDS_DRAIN_WITHIN,
        "it ends before the bound: {took:?}"
    );
    let (outcome, _) = carrying.await.expect("it was carried");
    assert_eq!(outcome, "cut by the node's stop");
}
