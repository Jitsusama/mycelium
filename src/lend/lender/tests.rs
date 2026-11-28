use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};

use super::spend::SPEND_WATCH;
use super::*;

mod spending;

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

    /// A lender over `lend`, each machine it names capped at $10 a day
    /// where it names no cap, each budget of models pricing `a-model` and
    /// spending spend-watch's `proxy` where it prices none, and
    /// spend-watch saying `proxy` is within its allowance.
    fn with(lend: &serde_json::Value) -> Self {
        let mut lend = lend.clone();
        for vm in lend["vms"].as_array_mut().into_iter().flatten() {
            if vm.get("usd_a_day").is_none() {
                vm["usd_a_day"] = json!(10.0);
            }
        }
        for budget in lend["budgets"]
            .as_object_mut()
            .into_iter()
            .flat_map(|all| all.values_mut())
        {
            let models = budget.get("kind").is_none_or(|kind| kind == "models");
            if models && budget.get("prices").is_none() {
                budget["prices"] = json!({"a-model": a_model()});
                budget["spend_watch"] = json!("proxy");
            }
        }
        let lending = Self::bare(&lend);
        lending.spend_watch_says(&json!([{
            "name": "proxy",
            "allowances": [{"label": "Week", "unit": "dollars", "state": "ok",
                            "allowance": 100.0, "spent": 1.0}],
        }]));
        lending
    }

    /// A lender over `lend` as it is, with no word from spend-watch.
    fn bare(lend: &serde_json::Value) -> Self {
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

    /// Has spend-watch say `budgets` stand as they do, now.
    fn spend_watch_says(&self, budgets: &serde_json::Value) {
        self.spend_watch_said(jiff::Timestamp::now(), budgets);
    }

    /// Has spend-watch have said `budgets` stand as they do, `at`.
    fn spend_watch_said(&self, at: jiff::Timestamp, budgets: &serde_json::Value) {
        let file = self.folder.path().join(SPEND_WATCH);
        fs::create_dir_all(file.parent().expect("its folder")).expect("spend-watch's folder");
        let said = json!({"v": 1, "at": at.to_string(), "budgets": budgets});
        fs::write(file, said.to_string()).expect("spend-watch's word is written");
    }

    /// What `machine` has spent today, in millionths of a dollar.
    fn spent(&self, machine: &str) -> u64 {
        let day = jiff::Timestamp::now().strftime("%Y-%m-%d").to_string();
        kept(&self.state().join(format!("spent-{day}.json")))
            .expect("the day's spend reads")
            .get(machine)
            .copied()
            .unwrap_or_default()
    }
}

/// What `a-model` costs: $3 a million tokens asked, $15 a million
/// answered, at most 1000 answered.
fn a_model() -> serde_json::Value {
    json!({"input": 3.0, "output": 15.0, "most_output": 1000})
}

/// A model call's body, as pi sends it.
const ASKED: &[u8] = br#"{"model":"a-model","max_tokens":64,"messages":[]}"#;

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
        body: STANDARD.encode(ASKED),
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
    refusal_and_why(answers).await.0
}

/// The status a lend is refused with, before any head, and why, so a
/// test can tell refusals of one status apart.
async fn refusal_and_why(answers: &mut mpsc::Receiver<Lent>) -> (u16, String) {
    match next(answers).await {
        Some(Lent::Refused { status, why }) => (status, why),
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
    assert_eq!(asked.body, ASKED);
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
async fn a_forge_beside_its_models_is_told_which_machine_asked_by_its_link() {
    let mut lending = Lending::with(&json!({
        "vms": [{
            "prefix": "agents-", "budget": "models", "also": ["forge"],
            "requests_a_day": 3,
        }],
        "budgets": {
            "models": {
                "upstream": "relay/agents.sock", "host": "models.example",
                "paths": ["/v1/messages"], "auth_header": "authorization",
            },
            "forge": {
                "upstream": "relay/agents.sock", "host": "forge.example",
                "paths": ["/push"], "auth_header": "authorization", "kind": "forge",
            },
            "another": {
                "upstream": "relay/agents.sock", "host": "another.example",
                "paths": ["/push"], "auth_header": "authorization", "kind": "forge",
            },
        },
    }));
    let asking = |budget: Option<&str>, path: &str| {
        let mut asking = lend(path);
        asking.budget = budget.map(str::to_owned);
        // What the borrower says of itself is never handed on.
        asking
            .headers
            .push((BORROWER.to_owned(), "agents-someone-else".to_owned()));
        asking
    };
    let _forge = answer(&lending.lender, "agents-1", asking(Some("forge"), "/push"));
    let head = lending.relay.asked().await.head.to_ascii_lowercase();
    assert!(head.starts_with("post /push http/1.1\r\n"), "{head}");
    assert!(head.contains("\r\nhost: forge.example\r\n"), "{head}");
    assert!(
        head.contains(&format!("\r\n{BORROWER}: agents-1\r\n")),
        "the forge hears the machine its link names: {head}"
    );
    assert!(!head.contains("agents-someone-else"), "{head}");
    let _models = answer(&lending.lender, "agents-1", asking(None, "/v1/messages"));
    let head = lending.relay.asked().await.head.to_ascii_lowercase();
    assert!(head.contains("\r\nhost: models.example\r\n"), "{head}");
    assert!(
        !head.contains(BORROWER),
        "a model relay is told nothing of who asked: {head}"
    );
    let mut another = answer(
        &lending.lender,
        "agents-1",
        asking(Some("another"), "/push"),
    );
    assert_eq!(
        refusal(&mut another).await,
        403,
        "a budget neither its own nor beside it"
    );
    assert!(!lending.relay.was_asked());
}

#[tokio::test]
async fn an_answer_of_a_length_is_streamed_whole_as_it_comes() {
    let mut lending = Lending::new();
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
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
        // Below a path lent, a batch spends thousands of requests as one.
        "/v1/messages/batches",
        "/v1/messages/count",
        "/v1/models",
        "/v1/messages/../../admin",
        "/v1/messages/%2e%2e/admin",
        "/v1/messages#/../../admin",
        "/v1/messages/x#y",
        "/v1/messages?a=1#/../../admin",
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
async fn a_path_with_a_query_is_lent_as_it_was_asked() {
    let mut lending = Lending::new();
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages?beta=true"));
    let asked = lending.relay.asked().await;
    let head = asked.head.to_ascii_lowercase();
    assert!(
        head.starts_with("post /v1/messages?beta=true http/1.1\r\n"),
        "{head}"
    );
    asked
        .answer
        .send(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n".to_vec())
        .expect("the relay answers");
    let (lent, _) = whole(&mut answers).await;
    assert_eq!(lent.last(), Some(&Lent::Done), "{lent:?}");
}

#[tokio::test]
async fn a_header_that_would_say_more_than_itself_is_refused() {
    let mut lending = Lending::new();
    for value in [
        "application/json\r\ncontent-length: 0\r\n\r\nPOST /v1/anything HTTP/1.1",
        "a\r\nx: y",
        " application/json",
    ] {
        let mut asked = lend("/v1/messages");
        asked.headers[0].1 = value.to_owned();
        let mut answers = answer(&lending.lender, "agents-1", asked);
        assert_eq!(refusal(&mut answers).await, 400, "{value:?}");
    }
    assert!(
        !lending.relay.was_asked(),
        "no header smuggles a request to the relay"
    );
}

#[tokio::test]
async fn a_machine_past_its_days_count_is_refused_while_another_still_borrows() {
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
async fn a_lends_line_names_its_upstream_only_once_it_was_asked() {
    let mut lending = Lending::new();
    let (sending, _answers) = mpsc::channel(BACKLOG);
    let carrying = {
        let lender = lending.lender.clone();
        let sending = sending.clone();
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
            used.upstream
        })
    };
    lending
        .relay
        .asked()
        .await
        .answer
        .send(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n".to_vec())
        .expect("the relay answers");
    assert_eq!(
        carrying.await.expect("it was carried").as_deref(),
        Some("models.example"),
        "one carried names the host it was asked of"
    );

    fs::write(
        lending.state().join(format!(
            "counts-{}.json",
            jiff::Timestamp::now().strftime("%Y-%m-%d")
        )),
        json!({"agents-1": 3}).to_string(),
    )
    .expect("the day is spent");
    let mut used = Used::default();
    let ended = lent(
        &lending.lender,
        "agents-1",
        lend("/v1/messages"),
        &sending,
        &mut used,
    )
    .await;
    assert!(matches!(ended, Ended::Refused(Refusal { status: 429, .. })));
    assert_eq!(
        used.upstream, None,
        "one refused by its count asked no upstream"
    );
    assert!(!lending.relay.was_asked(), "and none was asked");
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
    assert_eq!(
        uncut(&state, &Cut::All).expect("let every one back"),
        Vec::<String>::new()
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
async fn a_relay_that_says_no_head_once_asked_still_spends_the_day() {
    let mut lending = Lending::new();
    for _ in 0..3 {
        let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
        let asked = lending.relay.asked().await;
        asked
            .answer
            .send(b"not a head at all\r\n\r\n".to_vec())
            .expect("the relay says nonsense");
        drop(asked);
        assert_eq!(refusal(&mut answers).await, 502);
    }
    // Each was asked whole, so the model may have made its answer.
    let mut answers = answer(&lending.lender, "agents-1", lend("/v1/messages"));
    assert_eq!(refusal(&mut answers).await, 429);
    assert!(!lending.relay.was_asked(), "a fourth isn't carried");
}

#[tokio::test]
async fn a_path_ending_in_a_star_lends_only_what_is_below_it() {
    let mut lending = Lending::with(&json!({
        "vms": [{"prefix": "agents-", "budget": "models", "requests_a_day": 3}],
        "budgets": {"models": {
            "upstream": "relay/agents.sock",
            "host": "models.example",
            "paths": ["/v1/*"],
            "auth_header": "authorization",
            "key": ["/bin/sh", "-c", "printf 'Bearer lent-key\\n'"],
        }},
    }));
    for path in [
        "/v1",
        "/v1/",
        "/v1x/messages",
        "/v2/messages",
        // Below /v1 by their start, so only the segments and the
        // characters refuse these.
        "/v1/../admin",
        "/v1/./messages",
        "/v1/messages/..",
        "/v1/%2e%2e/admin",
        "/v1/..\\admin",
        "/v1/x#/../y",
        "/v1/x?to=/admin",
    ] {
        let mut answers = answer(&lending.lender, "agents-1", lend(path));
        assert_eq!(refusal(&mut answers).await, 403, "{path}");
    }
    assert!(
        !lending.relay.was_asked(),
        "none of those is below /v1 as it reads"
    );
    let _answers = answer(&lending.lender, "agents-1", lend("/v1/messages/batches"));
    let asked = lending.relay.asked().await;
    let head = asked.head.to_ascii_lowercase();
    assert!(
        head.starts_with("post /v1/messages/batches http/1.1\r\n"),
        "{head}"
    );
}

#[test]
fn cuts_made_at_once_each_hold() {
    let folder = tempfile::tempdir().expect("a folder");
    let state = folder.path().join("state");
    let cutting: Vec<_> = (0..16)
        .map(|n| {
            let state = state.clone();
            std::thread::spawn(move || cut(&state, &Cut::Machine(format!("agents-{n}"))))
        })
        .collect();
    for one in cutting {
        one.join().expect("the cut runs").expect("the cut is kept");
    }
    let mut kept = uncut(&state, &Cut::Machine("agents-0".to_owned())).expect("read");
    kept.sort();
    let mut wanted: Vec<_> = (1..16).map(|n| format!("agents-{n}")).collect();
    wanted.sort();
    assert_eq!(kept, wanted, "no cut made beside another is lost");
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
    let logged = unparsed(
        &serde_json::from_str::<Written>("{\"vms\": \"sk-pasted-key\", \"budgets\": {}}")
            .expect_err("not a lend"),
    );
    assert!(
        logged.starts_with("it isn't a lend at line 1") && !logged.contains("sk-pasted"),
        "the lender's own log says where, never what: {logged}"
    );
}

#[tokio::test]
async fn a_relay_that_stops_taking_a_request_still_lets_a_cut_end_its_lend() {
    // Its most, four megabytes asked, is past $10.
    let lending = Lending::with(&json!({
        "vms": [{"prefix": "agents-", "budget": "models", "requests_a_day": 3,
                 "usd_a_day": 100.0}],
        "budgets": {"models": {
            "upstream": "relay/wedged.sock",
            "host": "models.example",
            "paths": ["/v1/messages"],
            "auth_header": "authorization",
            "key": ["/bin/sh", "-c", "printf 'Bearer lent-key\\n'"],
        }},
    }));
    let wedged = UnixListener::bind(lending.folder.path().join("relay/wedged.sock"))
        .expect("the wedged relay binds");
    let request = 4 << 20;
    let mut answers = answer(
        &lending.lender,
        "agents-1",
        Lend {
            body: STANDARD.encode(format!(
                r#"{{"model":"a-model","max_tokens":64,"messages":"{}"}}"#,
                "x".repeat(request)
            )),
            ..lend("/v1/messages")
        },
    );
    let (stream, _) = tokio::time::timeout(PATIENCE, wedged.accept())
        .await
        .expect("the lender connects in time")
        .expect("the lender connects");
    let mut stream = BufReader::new(stream);
    let mut first = String::new();
    stream
        .read_line(&mut first)
        .await
        .expect("the lender begins asking");
    assert!(first.starts_with("POST /v1/messages "), "{first}");
    // Read no further, the relay leaves the lender's write stopped on a
    // full socket with most of the request unsent.
    cut(&lending.state(), &Cut::Machine("agents-1".to_owned())).expect("cut");
    assert_eq!(
        refusal(&mut answers).await,
        403,
        "the cut ends a lend stopped sending"
    );
    let mut rest = Vec::new();
    tokio::time::timeout(PATIENCE, stream.read_to_end(&mut rest))
        .await
        .expect("the relay is hung up on in time")
        .expect("what was sent is read");
    assert!(rest.len() < request, "the request never went up whole");
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

/// A lend handed to `skiff-` machines through the budget `room`, whose
/// upstream is `relay`, with `key` as its key.
fn hands(relay: &Path, key: &str) -> Hands {
    Hands {
        lend: json!({
            "vms": [{"prefix": "skiff-", "budget": "room", "requests_a_day": 3,
                     "usd_a_day": 10.0}],
            "budgets": {"room": {
                "upstream": relay,
                "host": "models.example",
                "paths": ["/v1/messages"],
                "auth_header": "authorization",
                "prices": {"a-model": a_model()},
                "spend_watch": "proxy",
            }},
        }),
        key: Key(key.to_owned()),
    }
}

#[tokio::test]
async fn a_lender_handed_its_lend_lends_nothing_until_handed_and_then_with_the_key_handed() {
    // Its folder holds the relay and spend-watch's word, which a model's
    // lend needs as much here as on the machine that handed it.
    let mut lending = Lending::new();
    let handed = Arc::new(Lender::handed_in(
        lending.folder.path().join("handed"),
        Some(lending.folder.path().to_owned()),
    ));
    assert!(!handed.lends(), "nothing was handed yet");
    let mut answers = answer(&handed, "skiff-1", lend("/v1/messages"));
    assert_eq!(refusal(&mut answers).await, 503);
    assert!(!lending.relay.was_asked(), "nothing reaches the relay");

    let relay = lending.folder.path().join("relay/agents.sock");
    handed
        .hand(hands(&relay, "Bearer handed-key"))
        .expect("the lend is taken");
    assert!(handed.lends());
    let mut answers = answer(&handed, "skiff-1", lend("/v1/messages"));
    let asked = lending.relay.asked().await;
    let head = asked.head.to_ascii_lowercase();
    assert!(
        head.contains("\r\nauthorization: bearer handed-key\r\n"),
        "{head}"
    );
    assert!(
        !head.contains("lent-key") && !head.contains("the-vms-own"),
        "neither the file's key nor the borrower's goes up: {head}"
    );
    asked
        .answer
        .send(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok".to_vec())
        .expect("the relay answers");
    let (lent, body) = whole(&mut answers).await;
    assert_eq!(body, b"ok");
    assert_eq!(lent.last(), Some(&Lent::Done), "{lent:?}");
    assert!(
        !format!("{lent:?}").contains("handed-key"),
        "the key stays here"
    );
}

#[tokio::test]
async fn a_hand_running_a_key_command_or_with_a_key_of_two_lines_or_to_a_file_is_refused() {
    let folder = tempfile::tempdir().expect("a folder");
    let relay = folder.path().join("relay.sock");
    let handed = Lender::handed_in(folder.path().join("handed"), None);
    let mut commanded = hands(&relay, "Bearer handed-key");
    commanded.lend["budgets"]["room"]["key"] = json!(["/bin/sh", "-c", "printf key"]);
    let why = handed
        .hand(commanded)
        .expect_err("a budget handed runs no command of its own");
    assert!(why.contains("key command"), "{why}");
    let why = handed
        .hand(hands(&relay, "Bearer kittiwake\nplover"))
        .expect_err("a key of two lines can't stand in a request");
    assert!(
        !why.contains("kittiwake") && !why.contains("plover"),
        "the key isn't said: {why}"
    );
    assert!(!handed.lends(), "nothing refused is taken");

    let lending = Lending::new();
    let why = lending
        .lender
        .hand(hands(&relay, "Bearer handed-key"))
        .expect_err("a lender reading its file is handed nothing");
    assert!(!why.contains("handed-key"), "{why}");
}

#[tokio::test]
async fn a_machine_its_vm_hands_a_budget_is_handed_it_alone_with_its_key() {
    let lending = Lending::bare(&json!({"vms": [], "budgets": {}}));
    let script = lending.folder.path().join("key.sh");
    fs::write(&script, "printf 'Bearer room-key\\n'\n").expect("the key's script");
    let shell = std::env::var("BASH").unwrap_or_else(|_| "/bin/sh".to_owned());
    let key = json!([shell, script]);
    let room = json!({
        "upstream": "relay/room.sock",
        "host": "models.example",
        "paths": ["/v1/messages"],
        "auth_header": "authorization",
        "prices": {"a-model": a_model()},
        "spend_watch": "proxy",
    });
    let mut keyed = room.clone();
    keyed["key"] = key.clone();
    let lend = json!({
        "vms": [
            {"prefix": "skiff-", "budget": "models", "hands": "room",
             "requests_a_day": 40, "usd_a_day": 12.5},
            {"prefix": "tern-", "budget": "models", "requests_a_day": 3},
            {"prefix": "wren-", "budget": "models", "hands": "room",
             "requests_a_day": 5},
        ],
        "budgets": {
            "models": {
                "upstream": "relay/agents.sock",
                "host": "models.example",
                "paths": ["/v1/messages"],
                "auth_header": "authorization",
                "key": key,
            },
            "room": keyed,
        },
    });
    fs::write(lending.folder.path().join("lend.json"), lend.to_string()).expect("the lend");

    let handing = lending
        .lender
        .handing("skiff-1")
        .await
        .expect("the lend reads")
        .expect("skiff-1 is handed its budget");
    assert_eq!(handing.key, Key("Bearer room-key".to_owned()));
    assert_eq!(
        handing.lend,
        json!({
            "vms": [{"prefix": "skiff-1", "budget": "room", "requests_a_day": 40,
                     "usd_a_day": 12.5}],
            "budgets": {"room": room},
        }),
        "the lend names skiff-1 alone, and its budget no key command"
    );
    let taking = Lender::handed_in(lending.folder.path().join("handed"), None);
    taking.hand(handing).expect("what is handed is taken");
    let uncapped = lending
        .lender
        .handing("wren-1")
        .await
        .expect("the lend reads")
        .expect("wren-1 is handed its budget");
    assert_eq!(
        uncapped.lend["vms"][0]["usd_a_day"],
        serde_json::Value::Null,
        "a vm capping no spend hands none"
    );
    taking
        .hand(uncapped)
        .expect("and that is taken too, lending no model");
    assert_eq!(
        lending.lender.handing("tern-1").await,
        Ok(None),
        "a vm handed no budget is handed nothing"
    );
}
