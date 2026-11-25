//! What a model's lend may spend: the machine's cap, held by what each
//! request reserves before it is sent, settled to what its answer says it
//! used, and spend-watch's word on the budget it spends.

use super::*;

/// What `ASKED` reserves of `a-model`, in millionths of a dollar: its 49
/// bytes each a token at the dearest a token asked costs, a cache write's
/// $3.75 a million, and its 64 tokens answered at $15 a million, rounded
/// up.
const RESERVED: u64 = 1144;

/// What an answer saying it used 10 tokens asked and 20 answered costs of
/// `a-model`: $3 a million times 10 and $15 a million times 20.
const USED: u64 = 330;

/// A lend of `agents-` machines through `models`, each capped at
/// `usd_a_day` and allowed 100 requests a day.
fn capped(usd_a_day: f64) -> Lending {
    Lending::with(&json!({
        "vms": [{"prefix": "agents-", "budget": "models", "requests_a_day": 100,
                 "usd_a_day": usd_a_day}],
        "budgets": {"models": {
            "upstream": "relay/agents.sock",
            "host": "models.example",
            "paths": ["/v1/messages"],
            "auth_header": "authorization",
        }},
    }))
}

/// A model call asking `body`, as pi's shim hands it on.
fn asking(body: &serde_json::Value) -> Lend {
    Lend {
        body: STANDARD.encode(body.to_string()),
        ..lend("/v1/messages")
    }
}

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

/// One chunk of a chunked answer, holding `data`.
fn chunk(data: &str) -> Vec<u8> {
    format!("{:x}\r\n{data}\r\n", data.len()).into_bytes()
}

/// The head of a streamed answer of events.
const STREAMED: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                          transfer-encoding: chunked\r\n\r\n";

#[tokio::test]
async fn a_request_past_what_is_left_of_its_machines_day_is_refused_unasked() {
    // $0.001 is less than one request's most.
    let mut lending = capped(0.001);
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    assert_eq!(refusal(&mut answers).await, 429);
    assert!(
        !lending.relay.was_asked(),
        "a request past the cap isn't carried"
    );
    assert_eq!(lending.spent("agents-1"), 0, "and spends nothing");
}

#[tokio::test]
async fn requests_asked_at_once_never_pass_the_cap_together() {
    // $0.005 holds four requests' most, 4576 millionths, and not a fifth.
    let mut lending = capped(0.005);
    let mut all: Vec<_> = (0..8)
        .map(|_| answer(&lending.lender, "agents-1", lend("/v1/messages")))
        .collect();
    for _ in 0..4 {
        let asked = lending.relay.asked().await;
        asked.answer.send(used_whole()).expect("the relay answers");
    }
    let mut refused = 0;
    for answers in &mut all {
        match next(answers).await {
            Some(Lent::Refused { status: 429, .. }) => refused += 1,
            Some(Lent::Head { status: 200, .. }) => {
                let (lent, _) = whole(answers).await;
                assert_eq!(lent.last(), Some(&Lent::Done), "{lent:?}");
            }
            other => panic!("each is carried or refused 429, not {other:?}"),
        }
    }
    assert_eq!(refused, 4, "only four fit the cap's reservations");
    assert!(!lending.relay.was_asked(), "and no fifth was carried");
    assert_eq!(
        lending.spent("agents-1"),
        4 * USED,
        "each settled to what it used"
    );
}

#[tokio::test]
async fn spend_watch_silent_or_stale_or_past_an_allowance_lends_no_model() {
    let lend_file = json!({
        "vms": [{"prefix": "agents-", "budget": "models", "requests_a_day": 100,
                 "usd_a_day": 10.0}],
        "budgets": {"models": {
            "upstream": "relay/agents.sock",
            "host": "models.example",
            "paths": ["/v1/messages"],
            "auth_header": "authorization",
            "prices": {"a-model": a_model()},
            "spend_watch": "proxy",
        }},
    });
    let mut lending = Lending::bare(&lend_file);
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    assert_eq!(refusal(&mut answers).await, 503, "no word at all");
    let proxy = |spent: f64| {
        json!([{"name": "proxy", "allowances": [
            {"label": "Week", "unit": "dollars", "allowance": 100.0, "spent": spent}]}])
    };
    let stale = jiff::Timestamp::now() - jiff::SignedDuration::from_mins(20);
    lending.spend_watch_said(stale, &proxy(1.0));
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    assert_eq!(refusal(&mut answers).await, 503, "a word 20 minutes old");
    lending.spend_watch_says(&json!([{"name": "elsewhere", "allowances": []}]));
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    assert_eq!(refusal(&mut answers).await, 503, "silent of its budget");
    lending.spend_watch_says(&json!([{"name": "proxy", "undeclared": true}]));
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    assert_eq!(
        refusal(&mut answers).await,
        503,
        "a budget it has only seen spent"
    );
    lending.spend_watch_says(&proxy(100.0));
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    assert_eq!(refusal(&mut answers).await, 429, "a budget spent");
    assert!(!lending.relay.was_asked(), "none was carried");
    assert_eq!(lending.spent("agents-1"), 0, "and none spent any");
    lending.spend_watch_says(&proxy(1.0));
    let _answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    lending.relay.asked().await;
}

#[tokio::test]
async fn a_request_whose_cost_cant_be_bounded_is_refused() {
    let mut lending = capped(10.0);
    for (body, status) in [
        (json!({"model": "a-model", "messages": []}), 400),
        (
            json!({"model": "a-model", "max_tokens": 1001, "messages": []}),
            400,
        ),
        (json!({"max_tokens": 64, "messages": []}), 400),
        (
            json!({"model": "another", "max_tokens": 64, "messages": []}),
            403,
        ),
    ] {
        let said = body.to_string();
        let mut answers = answer(&lending.lender, "agents-1", asking(&body));
        assert_eq!(refusal(&mut answers).await, status, "{said}");
    }
    assert!(!lending.relay.was_asked(), "none was carried");
}

#[tokio::test]
async fn a_machine_whose_lend_caps_nothing_is_lent_no_model() {
    let mut lending = Lending::bare(&json!({
        "vms": [{"prefix": "agents-", "budget": "models", "requests_a_day": 100}],
        "budgets": {"models": {
            "upstream": "relay/agents.sock",
            "host": "models.example",
            "paths": ["/v1/messages"],
            "auth_header": "authorization",
            "prices": {"a-model": a_model()},
            "spend_watch": "proxy",
        }},
    }));
    lending.spend_watch_says(&json!([{"name": "proxy", "allowances": []}]));
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    assert_eq!(refusal(&mut answers).await, 403);
    assert!(!lending.relay.was_asked(), "none was carried");
}

#[tokio::test]
async fn a_stream_cut_short_is_charged_its_reservation() {
    let mut lending = capped(10.0);
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    let asked = lending.relay.asked().await;
    asked.answer.send(STREAMED.to_vec()).expect("the head");
    let started = r#"data: {"type":"message_start","message":{"usage":{"input_tokens":10}}}"#;
    asked
        .answer
        .send(chunk(&format!("{started}\n\n")))
        .expect("an event");
    drop(asked.answer);
    let (lent, _) = whole(&mut answers).await;
    assert!(
        matches!(lent.last(), Some(Lent::Refused { status: 502, .. })),
        "{lent:?}"
    );
    assert_eq!(
        lending.spent("agents-1"),
        RESERVED,
        "it may have been made whole upstream"
    );
}

#[tokio::test]
async fn an_upstream_refusing_a_request_whole_charges_it_nothing() {
    let mut lending = capped(10.0);
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    let asked = lending.relay.asked().await;
    asked
        .answer
        .send(b"HTTP/1.1 429 Too Many Requests\r\ncontent-length: 0\r\n\r\n".to_vec())
        .expect("the relay refuses");
    let (lent, _) = whole(&mut answers).await;
    assert_eq!(lent.last(), Some(&Lent::Done), "{lent:?}");
    assert_eq!(lending.spent("agents-1"), 0);
}

#[tokio::test]
async fn a_whole_stream_is_settled_to_what_it_says_it_used() {
    let mut lending = capped(10.0);
    let (sending, mut answers) = mpsc::channel(BACKLOG);
    let carrying = {
        let lender = lending.lender.clone();
        tokio::spawn(async move {
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
        })
    };
    let asked = lending.relay.asked().await;
    asked.answer.send(STREAMED.to_vec()).expect("the head");
    for event in [
        r#"{"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":1}}}"#,
        // A model's text saying usage is never read for it.
        r#"{"type":"content_block_delta","delta":{"text":"\"usage\":{\"output_tokens\":9999}"}}"#,
        r#"{"type":"message_delta","usage":{"output_tokens":20}}"#,
        r#"{"type":"message_stop"}"#,
    ] {
        asked
            .answer
            .send(chunk(&format!("data: {event}\n\n")))
            .expect("an event");
    }
    asked.answer.send(b"0\r\n\r\n".to_vec()).expect("the end");
    let (lent, _) = whole(&mut answers).await;
    assert_eq!(lent.last(), Some(&Lent::Done), "{lent:?}");
    let used = carrying.await.expect("it was carried");
    assert_eq!(used.charged, Some(USED), "it is charged what it used");
    assert_eq!(
        used.spent_today,
        Some(USED),
        "its line says what its machine has spent today"
    );
    assert_eq!(lending.spent("agents-1"), USED);
}
