use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};

use super::*;

/// Longer than any of these waits should take, so a test fails rather
/// than hangs.
const PATIENCE: Duration = Duration::from_secs(10);

/// What the relay was asked, and how the test answers it.
struct Asked {
    /// The request's line and headers, as they came.
    head: String,
    /// Its body.
    body: Vec<u8>,
    /// Each piece the relay writes back, as the test says it; dropping
    /// it hangs the relay up.
    answer: mpsc::UnboundedSender<Vec<u8>>,
    /// Told once the lender hangs up on the relay.
    left: oneshot::Receiver<()>,
}

/// A model relay on a socket, telling its test each request it is asked.
struct Relay {
    asked: mpsc::Receiver<Asked>,
}

impl Relay {
    fn at(socket: &Path) -> Self {
        fs::create_dir_all(socket.parent().expect("a folder")).expect("the relay's folder");
        let listener = UnixListener::bind(socket).expect("the fake relay binds");
        let (tell, asked) = mpsc::channel(8);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let tell = tell.clone();
                tokio::spawn(async move {
                    let (reading, mut writing) = stream.into_split();
                    let mut reading = BufReader::new(reading);
                    let mut head = String::new();
                    loop {
                        let mut line = String::new();
                        reading.read_line(&mut line).await.expect("the lender asks");
                        head.push_str(&line);
                        if line == "\r\n" || line.is_empty() {
                            break;
                        }
                    }
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .map_or(0, |length| length.parse().expect("a length"));
                    let mut body = vec![0; length];
                    reading.read_exact(&mut body).await.expect("the body comes");
                    let (answer, mut answers) = mpsc::unbounded_channel::<Vec<u8>>();
                    let (gone, left) = oneshot::channel();
                    tell.send(Asked {
                        head,
                        body,
                        answer,
                        left,
                    })
                    .await
                    .expect("the test listens");
                    let mut leaving = Some(gone);
                    let mut rest = Vec::new();
                    loop {
                        tokio::select! {
                            piece = answers.recv() => {
                                let Some(piece) = piece else { return };
                                if writing.write_all(&piece).await.is_err() {
                                    return;
                                }
                            }
                            _ = reading.read_to_end(&mut rest), if leaving.is_some() => {
                                if let Some(gone) = leaving.take() {
                                    let _ = gone.send(());
                                }
                            }
                        }
                    }
                });
            }
        });
        Self { asked }
    }

    async fn asked(&mut self) -> Asked {
        tokio::time::timeout(PATIENCE, self.asked.recv())
            .await
            .expect("the lender asks in time")
            .expect("the fake relay runs")
    }

    /// Whether it was asked anything, once the lend it might have been
    /// carried for has answered.
    fn was_asked(&mut self) -> bool {
        self.asked.try_recv().is_ok()
    }
}

/// A lender over a lend file and relay in a folder of its own, lending
/// `agents-` machines 3 requests a day through `models`.
struct Lending {
    folder: tempfile::TempDir,
    lender: Arc<Lender>,
    relay: Relay,
}

impl Lending {
    fn new() -> Self {
        Self::with(&json!({
            "vms": [{"prefix": "agents-", "budget": "models", "requests_a_day": 3}],
            "budgets": {"models": {
                "upstream": "relay/agents.sock",
                "host": "models.example",
                "paths": ["/v1/messages"],
                "auth_header": "authorization",
                "key": ["/bin/sh", "-c", "printf 'Bearer lent-key\\n'"],
            }},
        }))
    }

    fn with(lend: &serde_json::Value) -> Self {
        let folder = tempfile::tempdir().expect("a folder");
        let file = folder.path().join("lend.json");
        fs::write(&file, lend.to_string()).expect("the lend is written");
        let relay = Relay::at(&folder.path().join("relay/agents.sock"));
        let lender = Arc::new(Lender::new(
            file,
            vec![0, 65534, rustix::process::getuid().as_raw()],
            folder.path().join("state"),
            Some(folder.path().to_owned()),
        ));
        Self {
            folder,
            lender,
            relay,
        }
    }

    fn state(&self) -> PathBuf {
        self.folder.path().join("state")
    }
}

/// A model call as pi's shim hands it on, with what pi sent of its own
/// credentials beside it.
fn lend(path: &str) -> Lend {
    Lend {
        budget: None,
        method: "POST".to_owned(),
        path: path.to_owned(),
        headers: vec![
            ("content-type".to_owned(), "application/json".to_owned()),
            ("authorization".to_owned(), "Bearer the-vms-own".to_owned()),
            ("x-api-key".to_owned(), "the-vms-key".to_owned()),
            ("host".to_owned(), "elsewhere.example".to_owned()),
        ],
        body: STANDARD.encode(br#"{"model":"a-model","messages":[]}"#),
    }
}

/// The next piece of a lend's answer.
async fn next(answers: &mut mpsc::Receiver<Lent>) -> Option<Lent> {
    tokio::time::timeout(PATIENCE, answers.recv())
        .await
        .expect("the lender answers in time")
}

/// The status a lend is refused with, before any head.
async fn refusal(answers: &mut mpsc::Receiver<Lent>) -> u16 {
    match next(answers).await {
        Some(Lent::Refused { status, .. }) => status,
        other => panic!("the lend is refused, not {other:?}"),
    }
}

/// Every piece of a lend's answer until it ends, its body's pieces
/// joined.
async fn whole(answers: &mut mpsc::Receiver<Lent>) -> (Vec<Lent>, Vec<u8>) {
    let mut lent = Vec::new();
    let mut body = Vec::new();
    while let Some(piece) = next(answers).await {
        if let Lent::Body(piece) = &piece {
            body.extend(STANDARD.decode(piece).expect("base64"));
        }
        lent.push(piece);
    }
    (lent, body)
}

#[tokio::test]
async fn a_lend_is_carried_with_this_machines_key_and_none_of_the_borrowers() {
    let mut lending = Lending::new();
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    let asked = lending.relay.asked().await;
    let head = asked.head.to_ascii_lowercase();
    assert!(head.starts_with("post /v1/messages http/1.1\r\n"), "{head}");
    assert!(head.contains("\r\nhost: models.example\r\n"), "{head}");
    assert!(
        head.contains("\r\nauthorization: bearer lent-key\r\n"),
        "{head}"
    );
    assert!(
        head.contains("\r\ncontent-type: application/json\r\n"),
        "{head}"
    );
    assert!(head.contains("\r\naccept-encoding: identity\r\n"), "{head}");
    for kept in [
        "the-vms-own",
        "the-vms-key",
        "elsewhere.example",
        "x-api-key",
    ] {
        assert!(
            !head.contains(kept),
            "{kept} stays with the borrower: {head}"
        );
    }
    assert_eq!(asked.body, br#"{"model":"a-model","messages":[]}"#);
    asked
        .answer
        .send(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n".to_vec())
        .expect("the relay answers");
    asked
        .answer
        .send(b"5\r\nhello\r\n".to_vec())
        .expect("a chunk");
    asked
        .answer
        .send(b"6\r\n world\r\n0\r\n\r\n".to_vec())
        .expect("the last");
    let (lent, body) = whole(&mut answers).await;
    assert!(
        matches!(&lent[0], Lent::Head { status: 200, headers }
            if headers.contains(&("content-type".to_owned(), "text/event-stream".to_owned()))),
        "{lent:?}"
    );
    assert_eq!(body, b"hello world");
    assert_eq!(lent.last(), Some(&Lent::Done), "{lent:?}");
    let said = format!("{lent:?}");
    assert!(!said.contains("lent-key"), "the key stays here: {said}");
}

#[tokio::test]
async fn an_answer_of_a_length_is_streamed_whole_as_it_comes() {
    let mut lending = Lending::new();
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages/count"));
    let asked = lending.relay.asked().await;
    asked
        .answer
        .send(b"HTTP/1.1 400 Bad Request\r\ncontent-length: 11\r\n\r\nhello".to_vec())
        .expect("the relay answers");
    assert!(matches!(
        next(&mut answers).await,
        Some(Lent::Head { status: 400, .. })
    ));
    asked.answer.send(b" world".to_vec()).expect("the rest");
    let (lent, body) = whole(&mut answers).await;
    assert_eq!(body, b"hello world");
    assert_eq!(lent.last(), Some(&Lent::Done));
}

#[tokio::test]
async fn an_interim_head_is_passed_over_and_a_304_has_no_body() {
    let mut lending = Lending::new();
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    let asked = lending.relay.asked().await;
    asked
        .answer
        .send(
            b"HTTP/1.1 103 Early Hints\r\nlink: </a>\r\n\r\n\
              HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nhi"
                .to_vec(),
        )
        .expect("the relay answers");
    let (lent, body) = whole(&mut answers).await;
    assert!(
        matches!(lent.first(), Some(Lent::Head { status: 200, .. })),
        "the final head is the answer's: {lent:?}"
    );
    assert_eq!(body, b"hi", "and the interim head is no part of its body");
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    let asked = lending.relay.asked().await;
    // Held open, so only the head can end the answer.
    asked
        .answer
        .send(b"HTTP/1.1 304 Not Modified\r\ncontent-length: 5\r\n\r\n".to_vec())
        .expect("the relay answers");
    let (lent, body) = whole(&mut answers).await;
    assert_eq!(
        (lent.last(), body.len()),
        (Some(&Lent::Done), 0),
        "a 304 ends at its head, whatever its length says: {lent:?}"
    );
}

#[tokio::test]
async fn an_answer_the_relay_ends_short_is_refused_partway() {
    let mut lending = Lending::new();
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    let asked = lending.relay.asked().await;
    asked
        .answer
        .send(b"HTTP/1.1 200 OK\r\ncontent-length: 50\r\n\r\nhello".to_vec())
        .expect("the relay answers");
    drop(asked.answer);
    let (lent, _) = whole(&mut answers).await;
    assert!(
        matches!(lent.last(), Some(Lent::Refused { status: 502, .. })),
        "{lent:?}"
    );
}

#[tokio::test]
async fn a_machine_the_lend_names_none_for_or_two_for_is_refused() {
    let mut lending = Lending::with(&json!({
        "vms": [
            {"prefix": "agents-", "budget": "models", "requests_a_day": 3},
            {"prefix": "agents-b", "budget": "models", "requests_a_day": 3},
        ],
        "budgets": {"models": {
            "upstream": "relay/agents.sock",
            "host": "models.example",
            "paths": ["/v1/messages"],
            "auth_header": "authorization",
        }},
    }));
    for machine in ["tern", "agents-b1"] {
        let mut answers = answer(&lending.lender, machine, lend("/v1/messages"));
        assert_eq!(refusal(&mut answers).await, 403, "{machine}");
    }
    assert!(
        !lending.relay.was_asked(),
        "nothing refused reaches the relay"
    );
}

#[tokio::test]
async fn a_path_or_a_budget_or_a_method_not_lent_is_refused() {
    let mut lending = Lending::new();
    let mut asks = Vec::new();
    for path in [
        "/v1/messagesx",
        "/v1/models",
        "/v1/messages/../../admin",
        "/v1/messages/%2e%2e/admin",
        "/v1/messages#/../../admin",
        "/v1/messages/..\\..\\v1/files",
        "/v1/messages?to=%2e%2e",
        "/v1/messages?to=/admin",
        "/v1/messages HTTP/1.1\r\nhost: elsewhere.example",
        "http://elsewhere.example/v1/messages",
    ] {
        asks.push(lend(path));
    }
    asks.push(Lend {
        budget: Some("another".to_owned()),
        ..lend("/v1/messages")
    });
    asks.push(Lend {
        method: "DELETE".to_owned(),
        ..lend("/v1/messages")
    });
    for asked in asks {
        let said = format!("{asked:?}");
        let mut answers = answer(&lending.lender, "agents-1", asked);
        assert_eq!(refusal(&mut answers).await, 403, "{said}");
    }
    assert!(
        !lending.relay.was_asked(),
        "nothing refused reaches the relay"
    );
}

#[tokio::test]
async fn a_machine_past_its_days_count_is_refused_until_the_next() {
    let mut lending = Lending::new();
    for _ in 0..3 {
        let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
        let asked = lending.relay.asked().await;
        asked
            .answer
            .send(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n".to_vec())
            .expect("the relay answers");
        let (lent, _) = whole(&mut answers).await;
        assert_eq!(lent.last(), Some(&Lent::Done));
    }
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    assert_eq!(refusal(&mut answers).await, 429);
    assert!(!lending.relay.was_asked(), "a fourth isn't carried");
    let _answers = answer(&lending.lender, "agents-2", lend("/v1/messages"));
    lending.relay.asked().await;
}

#[tokio::test]
async fn a_machine_cut_off_is_refused_by_name_or_with_every_other_until_let_back() {
    let mut lending = Lending::new();
    let state = lending.state();
    cut(&state, &Cut::Machine("agents-1".to_owned())).expect("cut");
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    assert_eq!(refusal(&mut answers).await, 403);
    cut(&state, &Cut::All).expect("cut every one");
    let mut answers = answer(&lending.lender, "agents-2", lend("/v1/messages"));
    assert_eq!(refusal(&mut answers).await, 403);
    let refused = uncut(&state, &Cut::Machine("agents-2".to_owned()))
        .expect_err("one machine isn't let back while every one is cut");
    assert!(
        refused.to_string().contains("let every machine back first"),
        "{refused}"
    );
    assert!(
        uncut(&state, &Cut::All)
            .expect("let every one back")
            .is_empty()
    );
    cut(&state, &Cut::Machine("agents-2".to_owned())).expect("cut");
    assert_eq!(
        uncut(&state, &Cut::Machine("agents-2".to_owned())).expect("let back"),
        Vec::<String>::new(),
        "one machine cut by name is let back by name"
    );
    assert!(!lending.relay.was_asked(), "nothing cut reaches the relay");
    let _answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    lending.relay.asked().await;
}

#[tokio::test]
async fn a_cut_partway_through_an_answer_ends_it_short() {
    let mut lending = Lending::new();
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    let asked = lending.relay.asked().await;
    asked
        .answer
        .send(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n5\r\nhello\r\n".to_vec())
        .expect("the relay answers");
    assert!(matches!(next(&mut answers).await, Some(Lent::Head { .. })));
    assert!(matches!(next(&mut answers).await, Some(Lent::Body(_))));
    cut(&lending.state(), &Cut::Machine("agents-1".to_owned())).expect("cut");
    assert!(
        matches!(
            next(&mut answers).await,
            Some(Lent::Refused { status: 403, .. })
        ),
        "the cut ends the answer"
    );
    tokio::time::timeout(PATIENCE, asked.left)
        .await
        .expect("the relay is hung up on in time")
        .expect("the fake relay tells");
}

#[tokio::test]
async fn a_borrower_that_stops_reading_lets_its_lend_go() {
    let mut lending = Lending::new();
    let answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    let asked = lending.relay.asked().await;
    asked
        .answer
        .send(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec())
        .expect("the relay answers");
    drop(answers);
    tokio::time::timeout(PATIENCE, asked.left)
        .await
        .expect("the relay is hung up on in time")
        .expect("the fake relay tells");
}

#[tokio::test]
async fn a_lend_with_no_relay_answering_is_refused_as_unavailable() {
    let lending = Lending::new();
    fs::remove_file(lending.folder.path().join("relay/agents.sock")).expect("the relay goes");
    for _ in 0..4 {
        let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
        assert_eq!(refusal(&mut answers).await, 503);
    }
    // Four refused for the relay's outage spend none of the day's three.
    let mut relay = Relay::at(&lending.folder.path().join("relay/agents.sock"));
    let _answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    relay.asked().await;
}

#[tokio::test]
async fn cuts_that_cant_be_read_cut_every_machine_until_every_one_is_let_back() {
    let mut lending = Lending::new();
    let state = lending.state();
    fs::create_dir_all(&state).expect("the state folder");
    fs::write(state.join("cuts.json"), "[\"agents-1\",").expect("broken");
    let mut answers = answer(&lending.lender, "agents-2", lend("/v1/messages"));
    match next(&mut answers).await {
        Some(Lent::Refused { status: 403, why }) => {
            assert!(why.contains("cuts can't be read"), "{why}");
        }
        other => panic!("the lend is refused, not {other:?}"),
    }
    uncut(&state, &Cut::All).expect("every one let back, over what can't be read");
    assert!(!lending.relay.was_asked(), "nothing cut reaches the relay");
    let _answers = answer(&lending.lender, "agents-2", lend("/v1/messages"));
    lending.relay.asked().await;
}

#[tokio::test]
async fn a_lend_file_that_cant_be_read_lends_nothing() {
    let lending = Lending::new();
    fs::write(lending.folder.path().join("lend.json"), "{\"vms\": []").expect("broken");
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    assert_eq!(refusal(&mut answers).await, 503);
    // A key pasted where a list belongs is quoted by the parser, which
    // the borrower never hears.
    fs::write(
        lending.folder.path().join("lend.json"),
        "{\"vms\": \"sk-pasted-key\", \"budgets\": {}}",
    )
    .expect("broken");
    assert!(
        serde_json::from_str::<Written>("{\"vms\": \"sk-pasted-key\", \"budgets\": {}}")
            .expect_err("not a lend")
            .to_string()
            .contains("sk-pasted"),
        "the parser quotes it, so the test can fail"
    );
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    match next(&mut answers).await {
        Some(Lent::Refused { status: 503, why }) => {
            assert!(!why.contains("sk-pasted"), "{why}");
            assert!(!why.contains("lend.json"), "nor where it is: {why}");
        }
        other => panic!("the lend is refused, not {other:?}"),
    }
}

#[tokio::test]
async fn a_key_that_cant_be_had_is_refused_and_never_said() {
    let mut lending = Lending::with(&json!({
        "vms": [{"prefix": "agents-", "budget": "models", "requests_a_day": 3}],
        "budgets": {"models": {
            "upstream": "relay/agents.sock",
            "host": "models.example",
            "paths": ["/v1/messages"],
            "auth_header": "authorization",
            "key": ["/bin/sh", "-c", "printf 'half-a-key'; exit 1"],
        }},
    }));
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    match next(&mut answers).await {
        Some(Lent::Refused { status: 502, why }) => {
            assert!(!why.contains("half-a-key"), "{why}");
        }
        other => panic!("the lend is refused, not {other:?}"),
    }
    assert!(
        !lending.relay.was_asked(),
        "nothing keyless reaches the relay"
    );
}
