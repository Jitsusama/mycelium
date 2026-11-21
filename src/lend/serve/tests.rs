use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

use super::*;
use crate::lend::{Lend, Lent};
use crate::wire::Request;

/// Longer than any of these waits should take, so a test fails rather
/// than hangs.
const PATIENCE: Duration = Duration::from_secs(10);

/// A node that answers each lend it is asked with what its test sends,
/// telling the test each lend it took and when its asker hung up.
struct Node {
    path: PathBuf,
    /// Each lend asked, with where to send its answer's lines.
    asked: mpsc::Receiver<(Lend, mpsc::UnboundedSender<Lent>, oneshot::Receiver<()>)>,
}

impl Node {
    fn at(folder: &Path) -> Self {
        let path = folder.join("node.sock");
        let listener = UnixListener::bind(&path).expect("the fake node binds");
        let (tell, asked) = mpsc::channel(8);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let tell = tell.clone();
                tokio::spawn(async move {
                    let (reading, mut writing) = stream.into_split();
                    let mut reading = BufReader::new(reading);
                    let mut line = String::new();
                    reading.read_line(&mut line).await.expect("the shim asks");
                    let Ok(Request::Lend(lend)) = serde_json::from_str(&line) else {
                        panic!("the shim asked what isn't a lend: {line}");
                    };
                    let (answer, mut answers) = mpsc::unbounded_channel();
                    let (gone, left) = oneshot::channel();
                    tell.send((lend, answer, left))
                        .await
                        .expect("the test listens");
                    let mut rest = Vec::new();
                    let mut leaving = Some(gone);
                    loop {
                        tokio::select! {
                            said = answers.recv() => {
                                let Some(said) = said else { return };
                                let mut line = serde_json::to_string(&said).expect("a lent says itself");
                                line.push('\n');
                                if writing.write_all(line.as_bytes()).await.is_err() {
                                    return;
                                }
                            }
                            read = reading.read_to_end(&mut rest), if leaving.is_some() => {
                                let _ = read;
                                if let Some(gone) = leaving.take() {
                                    let _ = gone.send(());
                                }
                            }
                        }
                    }
                });
            }
        });
        Self { path, asked }
    }

    async fn asked(&mut self) -> (Lend, mpsc::UnboundedSender<Lent>, oneshot::Receiver<()>) {
        tokio::time::timeout(PATIENCE, self.asked.recv())
            .await
            .expect("the shim asks in time")
            .expect("the fake node runs")
    }
}

/// A shim serving at a folder of its own, lending through `node`.
fn shim(folder: &Path, node: &Node, budget: Option<&str>) -> PathBuf {
    let path = folder.join("relay").join("agents.sock");
    let socket = crate::node::bind(&path).expect("the shim binds");
    tokio::spawn(serve(socket, node.path.clone(), budget.map(str::to_owned)));
    path
}

/// What pi sends for one turn, with its credentials as pi sends them.
fn turn(body: &str) -> String {
    format!(
        "POST /apis/anthropic/v1/messages HTTP/1.1\r\nhost: proxy.example\r\n\
         authorization: Bearer the-persons-own\r\nx-api-key: sk-the-persons-own\r\n\
         content-type: application/json\r\nanthropic-version: 2023-06-01\r\n\
         anthropic-beta: tools-1\r\naccept: text/event-stream\r\n\
         content-length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// Reads from `stream` until what it said holds `wanted`.
async fn until(stream: &mut UnixStream, heard: &mut Vec<u8>, wanted: &str) {
    tokio::time::timeout(PATIENCE, async {
        while !String::from_utf8_lossy(heard).contains(wanted) {
            let mut more = [0; 4096];
            let read = stream.read(&mut more).await.expect("the shim answers");
            assert!(
                read > 0,
                "the shim hung up before saying {wanted:?}: {}",
                String::from_utf8_lossy(heard)
            );
            heard.extend_from_slice(&more[..read]);
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "the shim never said {wanted:?}: {}",
            String::from_utf8_lossy(heard)
        )
    });
}

#[tokio::test]
async fn a_lend_carries_only_the_headers_an_answer_turns_on() {
    let folder = tempfile::tempdir().expect("a folder");
    let mut node = Node::at(folder.path());
    let path = shim(folder.path(), &node, Some("agents"));
    let mut pi = UnixStream::connect(&path)
        .await
        .expect("pi reaches the shim");
    pi.write_all(turn(r#"{"model":"m"}"#).as_bytes())
        .await
        .expect("pi asks");
    let (lend, _answer, _gone) = node.asked().await;
    assert_eq!(
        lend,
        Lend {
            budget: Some("agents".to_owned()),
            method: "POST".to_owned(),
            path: "/apis/anthropic/v1/messages".to_owned(),
            headers: vec![
                ("content-type".to_owned(), "application/json".to_owned()),
                ("anthropic-version".to_owned(), "2023-06-01".to_owned()),
                ("anthropic-beta".to_owned(), "tools-1".to_owned()),
                ("accept".to_owned(), "text/event-stream".to_owned()),
            ],
            body: STANDARD.encode(br#"{"model":"m"}"#),
        }
    );
}

#[tokio::test]
async fn a_refused_lend_answers_its_status() {
    for (status, why) in [
        (429, "over today's count"),
        (403, "cut off"),
        (502, "upstream down"),
    ] {
        let folder = tempfile::tempdir().expect("a folder");
        let mut node = Node::at(folder.path());
        let path = shim(folder.path(), &node, None);
        let mut pi = UnixStream::connect(&path)
            .await
            .expect("pi reaches the shim");
        pi.write_all(turn("{}").as_bytes()).await.expect("pi asks");
        let (_, answer, _) = node.asked().await;
        answer
            .send(Lent::Refused {
                status,
                why: why.to_owned(),
            })
            .expect("the fake node answers");
        let mut heard = Vec::new();
        // Anthropic's error, its keys in the order serde_json writes them.
        let body =
            format!(r#"{{"error":{{"message":"{why}","type":"mycelium_lend"}},"type":"error"}}"#);
        until(&mut pi, &mut heard, &body).await;
        let heard = String::from_utf8_lossy(&heard);
        assert!(
            heard.starts_with(&format!("HTTP/1.1 {status} ")),
            "a refusal is said as its status: {heard}"
        );
        assert!(
            heard.contains(&format!("content-length: {}\r\n", body.len())),
            "{heard}"
        );
    }
}

#[tokio::test]
async fn a_stream_arrives_in_order_as_it_comes() {
    let folder = tempfile::tempdir().expect("a folder");
    let mut node = Node::at(folder.path());
    let path = shim(folder.path(), &node, None);
    let mut pi = UnixStream::connect(&path)
        .await
        .expect("pi reaches the shim");
    pi.write_all(turn("{}").as_bytes()).await.expect("pi asks");
    let (_, answer, _) = node.asked().await;
    let send = |lent| answer.send(lent).expect("the fake node answers");
    send(Lent::Head {
        status: 200,
        headers: vec![
            ("content-type".to_owned(), "text/event-stream".to_owned()),
            ("content-length".to_owned(), "999".to_owned()),
            ("connection".to_owned(), "close".to_owned()),
        ],
    });
    send(Lent::Body(STANDARD.encode(b"event: one\n\n")));
    let mut heard = Vec::new();
    // The first piece reaches pi before the second is even sent.
    until(&mut pi, &mut heard, "c\r\nevent: one\n\n\r\n").await;
    send(Lent::Body(STANDARD.encode(b"event: two\n\n")));
    send(Lent::Done);
    until(&mut pi, &mut heard, "0\r\n\r\n").await;
    assert_eq!(
        String::from_utf8_lossy(&heard),
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n\
         c\r\nevent: one\n\n\r\nc\r\nevent: two\n\n\r\n0\r\n\r\n",
        "the lender's framing headers give way to the shim's own"
    );
    // The connection carries pi's next turn.
    pi.write_all(turn("{}").as_bytes())
        .await
        .expect("pi asks again");
    let (_, again, _) = node.asked().await;
    again
        .send(Lent::Refused {
            status: 429,
            why: "over today's count".to_owned(),
        })
        .expect("the fake node answers");
    until(&mut pi, &mut heard, "HTTP/1.1 429 ").await;
}

#[tokio::test]
async fn a_client_that_hangs_up_hangs_up_on_the_node() {
    let folder = tempfile::tempdir().expect("a folder");
    let mut node = Node::at(folder.path());
    let path = shim(folder.path(), &node, None);
    let mut pi = UnixStream::connect(&path)
        .await
        .expect("pi reaches the shim");
    pi.write_all(turn("{}").as_bytes()).await.expect("pi asks");
    let (_, answer, gone) = node.asked().await;
    answer
        .send(Lent::Head {
            status: 200,
            headers: vec![],
        })
        .expect("the fake node answers");
    let mut heard = Vec::new();
    until(&mut pi, &mut heard, "\r\n\r\n").await;
    drop(pi);
    tokio::time::timeout(PATIENCE, gone)
        .await
        .expect("the shim hangs up on the node in time")
        .expect("the fake node saw it");
}

#[tokio::test]
async fn a_failure_partway_through_ends_the_answer_short() {
    let folder = tempfile::tempdir().expect("a folder");
    let mut node = Node::at(folder.path());
    let path = shim(folder.path(), &node, None);
    let mut pi = UnixStream::connect(&path)
        .await
        .expect("pi reaches the shim");
    pi.write_all(turn("{}").as_bytes()).await.expect("pi asks");
    let (_, answer, _) = node.asked().await;
    answer
        .send(Lent::Head {
            status: 200,
            headers: vec![],
        })
        .expect("the fake node answers");
    answer
        .send(Lent::Refused {
            status: 403,
            why: "cut off".to_owned(),
        })
        .expect("the fake node answers");
    let mut heard = Vec::new();
    tokio::time::timeout(PATIENCE, pi.read_to_end(&mut heard))
        .await
        .expect("the shim hangs up in time")
        .expect("pi reads");
    let heard = String::from_utf8_lossy(&heard);
    assert!(heard.starts_with("HTTP/1.1 200 OK\r\n"), "{heard}");
    assert!(
        !heard.contains("0\r\n\r\n"),
        "an answer cut short never says it ended: {heard}"
    );
}

#[tokio::test]
async fn a_coded_body_is_refused_here_so_pi_sends_it_plain() {
    let folder = tempfile::tempdir().expect("a folder");
    let mut node = Node::at(folder.path());
    let path = shim(folder.path(), &node, None);
    let mut pi = UnixStream::connect(&path)
        .await
        .expect("pi reaches the shim");
    let coded = turn("{}").replace("accept:", "content-encoding: zstd\r\naccept:");
    pi.write_all(coded.as_bytes()).await.expect("pi asks");
    let mut heard = Vec::new();
    until(&mut pi, &mut heard, "send it plain").await;
    assert!(String::from_utf8_lossy(&heard).starts_with("HTTP/1.1 415 "));
    // Nothing was asked of the node, and the connection carries the next.
    pi.write_all(turn("{}").as_bytes())
        .await
        .expect("pi asks plain");
    let (lend, _, _) = node.asked().await;
    assert_eq!(lend.body, STANDARD.encode(b"{}"));
}

#[tokio::test]
async fn a_node_that_does_not_answer_is_said_as_unavailable() {
    let folder = tempfile::tempdir().expect("a folder");
    let path = folder.path().join("relay").join("agents.sock");
    let socket = crate::node::bind(&path).expect("the shim binds");
    tokio::spawn(serve(socket, folder.path().join("no-node.sock"), None));
    let mut pi = UnixStream::connect(&path)
        .await
        .expect("pi reaches the shim");
    pi.write_all(turn("{}").as_bytes()).await.expect("pi asks");
    let mut heard = Vec::new();
    until(&mut pi, &mut heard, "didn't answer").await;
    assert!(String::from_utf8_lossy(&heard).starts_with("HTTP/1.1 503 "));
}

#[tokio::test]
async fn a_chunked_body_is_read_whole() {
    let folder = tempfile::tempdir().expect("a folder");
    let mut node = Node::at(folder.path());
    let path = shim(folder.path(), &node, None);
    let mut pi = UnixStream::connect(&path)
        .await
        .expect("pi reaches the shim");
    pi.write_all(
        b"POST /v1/messages HTTP/1.1\r\ntransfer-encoding: chunked\r\n\r\n\
          3\r\nabc\r\n2;x=y\r\nde\r\n0\r\n\r\n",
    )
    .await
    .expect("pi asks");
    let (lend, _, _) = node.asked().await;
    assert_eq!(lend.body, STANDARD.encode(b"abcde"));
}

#[tokio::test]
async fn a_request_that_is_not_one_is_refused_unasked() {
    for (asked, status) in [
        ("GET http://elsewhere/ HTTP/1.1\r\n\r\n", 400),
        ("GET / HTTP/2\r\n\r\n", 505),
        (
            "POST / HTTP/1.1\r\ncontent-length: 99999999999\r\n\r\n",
            413,
        ),
        ("POST / HTTP/1.1\r\ntransfer-encoding: gzip\r\n\r\n", 501),
        (
            "POST / HTTP/1.1\r\ncontent-length: 1\r\ncontent-length: 2\r\n\r\n",
            400,
        ),
    ] {
        let folder = tempfile::tempdir().expect("a folder");
        let node = Node::at(folder.path());
        let path = shim(folder.path(), &node, None);
        let mut pi = UnixStream::connect(&path)
            .await
            .expect("pi reaches the shim");
        pi.write_all(asked.as_bytes()).await.expect("pi asks");
        let mut heard = Vec::new();
        until(&mut pi, &mut heard, "mycelium_lend").await;
        assert!(
            String::from_utf8_lossy(&heard).starts_with(&format!("HTTP/1.1 {status} ")),
            "{asked:?}: {}",
            String::from_utf8_lossy(&heard)
        );
    }
}

#[tokio::test]
async fn a_head_too_long_is_refused() {
    let folder = tempfile::tempdir().expect("a folder");
    let node = Node::at(folder.path());
    let path = shim(folder.path(), &node, None);
    let mut pi = UnixStream::connect(&path)
        .await
        .expect("pi reaches the shim");
    let long = format!("GET / HTTP/1.1\r\nx: {}\r\n\r\n", "a".repeat(MOST_HEAD));
    // The shim stops reading once it refuses, so pi's write may fail.
    let _ = pi.write_all(long.as_bytes()).await;
    let mut heard = Vec::new();
    until(&mut pi, &mut heard, "mycelium_lend").await;
    assert!(String::from_utf8_lossy(&heard).starts_with("HTTP/1.1 431 "));
}
