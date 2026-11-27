//! A lend its relay answers 421, misdirected: asked once more on a fresh
//! connection before anything of the answer reaches the borrower, and
//! counted and charged as one lend however many times it was asked.

use super::*;

/// The relay's word that the connection it was asked on was misdirected.
const MISDIRECTED: &[u8] = b"HTTP/1.1 421 Misdirected Request\r\ncontent-length: 0\r\n\r\n";

/// What an answer saying it used 10 tokens asked and 20 answered costs of
/// `a-model`: $3 a million times 10 and $15 a million times 20.
const USED: u64 = 330;

/// An answer that isn't streamed, saying it used 10 tokens asked and 20
/// answered.
fn used_whole() -> Vec<u8> {
    let body = json!({"usage": {"input_tokens": 10, "output_tokens": 20}}).to_string();
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// Carries a model call for `agents-1` in a task of its own: its answer's
/// pieces, and what its line says of it once it ends.
fn carrying(lending: &Lending) -> (mpsc::Receiver<Lent>, tokio::task::JoinHandle<Used>) {
    let (sending, answers) = mpsc::channel(BACKLOG);
    let lender = lending.lender.clone();
    let carrying = tokio::spawn(async move {
        let mut used = Used::default();
        lent(
            &lender,
            "agents-1",
            lend("/v1/messages"),
            &sending,
            &mut used,
        )
        .await;
        used
    });
    (answers, carrying)
}

/// How many requests `machine` has made today.
fn counted(lending: &Lending, machine: &str) -> u64 {
    let day = jiff::Timestamp::now().strftime("%Y-%m-%d").to_string();
    kept(&lending.state().join(format!("counts-{day}.json")))
        .expect("the day's count reads")
        .get(machine)
        .copied()
        .unwrap_or_default()
}

#[tokio::test]
async fn a_lend_answered_421_is_asked_again_and_the_borrower_hears_only_the_second() {
    let mut lending = Lending::new();
    let (mut answers, carrying) = carrying(&lending);
    let first = lending.relay.asked().await;
    first
        .answer
        .send(MISDIRECTED.to_vec())
        .expect("the relay answers");
    let second = lending.relay.asked().await;
    assert_eq!(second.head, first.head, "it is asked the same again");
    assert_eq!(second.body, ASKED);
    second
        .answer
        .send(used_whole())
        .expect("the relay answers again");
    let (lent, body) = whole(&mut answers).await;
    assert!(
        matches!(lent.first(), Some(Lent::Head { status: 200, .. })),
        "the 421 never reaches the borrower: {lent:?}"
    );
    assert!(body.starts_with(br#"{"usage""#), "the answer comes whole");
    assert_eq!(lent.last(), Some(&Lent::Done), "{lent:?}");
    let used = carrying.await.expect("it was carried");
    assert_eq!(
        (used.asked, used.status),
        (2, Some(200)),
        "its line says it was asked again"
    );
    assert!(!lending.relay.was_asked(), "the relay saw two connections");
    assert_eq!(counted(&lending, "agents-1"), 1, "it is one lend");
    assert_eq!(lending.spent("agents-1"), USED, "charged once");
}

#[tokio::test]
async fn a_lend_answered_421_twice_hands_on_the_second_421() {
    let mut lending = Lending::new();
    let (mut answers, carrying) = carrying(&lending);
    for _ in 0..2 {
        lending
            .relay
            .asked()
            .await
            .answer
            .send(MISDIRECTED.to_vec())
            .expect("the relay answers");
    }
    // The shim says it as 502, since pi would take a 421 as leave to go
    // straight to the proxy.
    let (lent, body) = whole(&mut answers).await;
    assert!(
        matches!(lent.first(), Some(Lent::Head { status: 421, .. })),
        "{lent:?}"
    );
    assert_eq!((lent.last(), body.len()), (Some(&Lent::Done), 0));
    let used = carrying.await.expect("it was carried");
    assert_eq!((used.asked, used.status), (2, Some(421)));
    assert!(
        !lending.relay.was_asked(),
        "the relay saw exactly two connections"
    );
    assert_eq!(counted(&lending, "agents-1"), 1, "it is one lend");
    assert_eq!(
        lending.spent("agents-1"),
        0,
        "refused whole, it spends nothing, as the first would have"
    );
}

#[tokio::test]
async fn a_lend_answered_421_by_a_relay_gone_meanwhile_hands_on_the_first() {
    let mut lending = Lending::new();
    let (mut answers, carrying) = carrying(&lending);
    let first = lending.relay.asked().await;
    fs::remove_file(lending.folder.path().join("relay/agents.sock")).expect("the relay goes");
    first
        .answer
        .send(MISDIRECTED.to_vec())
        .expect("the relay answers");
    let (lent, _) = whole(&mut answers).await;
    assert!(
        matches!(lent.first(), Some(Lent::Head { status: 421, .. })),
        "{lent:?}"
    );
    assert_eq!(lent.last(), Some(&Lent::Done), "{lent:?}");
    let used = carrying.await.expect("it was carried");
    assert_eq!(used.asked, 1, "it couldn't be asked again");
    assert_eq!(lending.spent("agents-1"), 0);
}
