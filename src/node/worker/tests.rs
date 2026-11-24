use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::super::Given;
use super::super::tests::{alone_with, bare};
use super::*;
use crate::store::memory::Memory;

/// A worker's session present as `01b2`, answering to `7WRKR1`, of the
/// quest `kp0l6a`.
fn holding() -> Holding {
    holding_of(&presence("01b2", "7WRKR1")).expect("a presence of a session")
}

fn presence(session: &str, address: &str) -> Value {
    json!({
        "session": session,
        "addresses": [address],
        "quest": "QEST-20261106-KP0L6A",
        "at": "2026-11-24T07:00:00Z",
    })
}

/// A message from `7WRKR1` sent at `now`, to `to`, saying `intent`.
fn message(now: i64, to: &[&str], intent: &str) -> Value {
    json!({
        "v": 1,
        "id": format!("{now}-7WRKR1-4242-ab12cd"),
        "at": jiff::Timestamp::from_millisecond(now).expect("a time").to_string(),
        "from": "7WRKR1",
        "to": to,
        "text": "hello",
        "intent": intent,
        "pid": 4242,
    })
}

const NOW: i64 = 1_795_000_000_000;

#[test]
fn a_worker_reaches_its_own_quest_and_peers_by_address() {
    let (id, body) = rebuilt(
        &message(NOW, &["#quest/kp0l6a", "@P9R1KI"], "question"),
        &holding(),
        NOW,
    )
    .expect("a message a worker may say");
    assert_eq!(id, format!("{NOW}-7WRKR1-4242-ab12cd"));
    assert_eq!(body["to"], json!(["#quest/kp0l6a", "@P9R1KI"]));
    assert_eq!(body["intent"], "question");
    assert_eq!(body["pid"], 4242);
}

#[test]
fn a_worker_says_no_hold_pivot_or_ended_hold_and_nothing_beyond_its_reach() {
    for (said, why) in [
        (message(NOW, &["@P9R1KI"], "hold"), "hold"),
        (message(NOW, &["@P9R1KI"], "pivot"), "pivot"),
        (message(NOW, &["#machine"], "fyi"), "beyond"),
        (message(NOW, &["#quest/wcjqsg"], "fyi"), "beyond"),
        (message(NOW, &[], "fyi"), "list of groups"),
    ] {
        let refused = rebuilt(&said, &holding(), NOW).expect_err("a message a worker can't say");
        assert!(refused.contains(why), "{refused:?} doesn't say {why}");
    }
    let mut ending = message(NOW, &["@P9R1KI"], "fyi");
    ending["endsHold"] = json!(true);
    ending["re"] = json!("1-P9R1KI-1-a");
    assert!(
        rebuilt(&ending, &holding(), NOW).is_err(),
        "a worker ended a hold"
    );
}

#[test]
fn a_worker_says_only_its_own_names_at_its_own_time() {
    let late = rebuilt(&message(NOW, &["@P9R1KI"], "fyi"), &holding(), NOW + 61_000);
    assert!(late.expect_err("a stale message").contains("minute"));
    let mut another = message(NOW, &["@P9R1KI"], "fyi");
    another["id"] = json!(format!("{NOW}-P9R1KI-4242-ab12cd"));
    another["from"] = json!("P9R1KI");
    let refused = rebuilt(&another, &holding(), NOW).expect_err("another's name");
    assert!(refused.contains("isn't a name"), "{refused:?}");
    let mut mismatched = message(NOW, &["@P9R1KI"], "fyi");
    mismatched["from"] = json!("01b2");
    assert!(
        rebuilt(&mismatched, &holding(), NOW).is_err(),
        "a sender its id doesn't name"
    );
    let mut moved = message(NOW, &["@P9R1KI"], "fyi");
    moved["at"] = json!(
        jiff::Timestamp::from_millisecond(NOW + 1)
            .expect("a time")
            .to_string()
    );
    assert!(
        rebuilt(&moved, &holding(), NOW).is_err(),
        "a time that isn't its id's"
    );
}

#[test]
fn a_worker_invites_only_to_its_own_quest_and_keeps_nothing_else_it_said() {
    let mut invite = message(NOW, &["@P9R1KI"], "fyi");
    invite["invite"] = json!("#quest/wcjqsg");
    assert!(
        rebuilt(&invite, &holding(), NOW).is_err(),
        "a foreign invite"
    );
    invite["invite"] = json!("#quest/kp0l6a");
    invite["steer"] = json!({"act": "stop"});
    invite["hosted"] = json!(true);
    let (_, body) = rebuilt(&invite, &holding(), NOW).expect("its own quest");
    assert_eq!(body["invite"], "#quest/kp0l6a");
    assert_eq!(body["hosted"], true);
    assert_eq!(
        body.get("steer"),
        None,
        "what the mesh doesn't read stays out"
    );
}

#[test]
fn an_id_is_read_in_the_mesh_shape_alone() {
    assert_eq!(id_parts("17-7WRKR1-42-ab1"), Some((17, "7WRKR1", 42)));
    for unlike in [
        "17-7WRKR1-42",
        "x-7WRKR1-42-ab1",
        "17--42-ab1",
        "17-A-42-a/b",
        "17-A-x-ab",
    ] {
        assert_eq!(id_parts(unlike), None, "{unlike} was read as an id");
    }
}

/// The worker socket of a node in `dir`, answering `uid`.
fn worker_beside(dir: &Path, uid: u32) -> Given {
    Given {
        worker: Some(Worker {
            socket: dir.join("worker").join("socket"),
            uid,
        }),
        ..bare(dir.join("steer"), None)
    }
}

/// A client of the worker socket, once the node has bound it.
struct Client {
    lines: BufReader<UnixStream>,
    writing: UnixStream,
}

impl Client {
    fn connect(socket: &Path) -> Self {
        let began = Instant::now();
        loop {
            match UnixStream::connect(socket) {
                Ok(stream) => {
                    let writing = stream.try_clone().expect("the stream clones");
                    return Self {
                        lines: BufReader::new(stream),
                        writing,
                    };
                }
                Err(_) if began.elapsed() < Duration::from_secs(10) => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("the worker socket never took a client: {error}"),
            }
        }
    }

    fn ask(&mut self, asked: &Value) -> Value {
        let mut line = asked.to_string();
        line.push('\n');
        self.writing
            .write_all(line.as_bytes())
            .expect("the ask is sent");
        self.next()
    }

    fn next(&mut self) -> Value {
        let mut line = String::new();
        self.lines.read_line(&mut line).expect("the node answers");
        serde_json::from_str(&line).expect("a line of JSON")
    }
}

fn own_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

#[tokio::test]
async fn a_worker_is_present_and_speaks_within_its_reach_through_the_node() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let socket = dir.path().join("mycelium").join("work.sock");
    let store = Memory::default();
    store.start_here("01a1", presence("01a1", "4UHC5Q"), true);
    let worker = dir.path().join("worker").join("socket");
    let (stop, node) = alone_with(&store, &socket, worker_beside(dir.path(), own_uid()));
    let held = store.clone();
    tokio::task::spawn_blocking(move || {
        let mut present = Client::connect(&worker);
        let taken = present.ask(&json!({"Presence": {"presence": presence("01b2", "4UHC5Q")}}));
        assert!(
            taken["Err"]
                .as_str()
                .is_some_and(|why| why.contains("4UHC5Q")),
            "{taken}"
        );

        let mut present = Client::connect(&worker);
        let mut said = presence("01b2", "7WRKR1");
        said["machine"] = json!("tern");
        let held_now = present.ask(&json!({"Presence": {"presence": said}}));
        assert_eq!(held_now, json!({"Ok": "Present"}));
        let kept = held
            .presences()
            .into_iter()
            .find(|kept| kept.session == "01b2")
            .expect("the presence is kept");
        assert_eq!(kept.body.get("machine"), None, "only a link says a machine");
        assert!(kept.lives && kept.elsewhere.is_none(), "it stands here");

        let mut twice = Client::connect(&worker);
        let refused = twice.ask(&json!({"Presence": {"presence": presence("01b2", "7WRKR1")}}));
        assert!(
            refused["Err"].is_string(),
            "one session held twice: {refused}"
        );

        let mut watch = Client::connect(&worker);
        let watching = watch.ask(&json!({"Watch": {"session": "01b2"}}));
        assert_eq!(watching, json!({"Ok": "Watching"}));
        assert_eq!(watch.next(), json!({"MessagesChanged": {"messages": []}}));

        let now = jiff::Timestamp::now().as_millisecond();
        let sent = present.ask(&json!({"Send": {"message": message(now, &["@P9R1KI"], "fyi")}}));
        let id = format!("{now}-7WRKR1-4242-ab12cd");
        assert_eq!(sent["Ok"]["Sent"]["id"], json!(id), "{sent}");
        assert_eq!(
            held.message(&id).expect("the message is kept")["text"],
            "hello"
        );
        let again = present.ask(&json!({"Send": {"message": message(now, &["@P9R1KI"], "fyi")}}));
        assert!(
            again["Err"]
                .as_str()
                .is_some_and(|why| why.contains("said already")),
            "{again}"
        );
        let refused =
            present.ask(&json!({"Send": {"message": message(now + 1, &["@P9R1KI"], "hold")}}));
        assert!(refused["Err"].is_string(), "a worker held: {refused}");

        let to_it = message(now + 2, &["#quest/kp0l6a"], "fyi");
        let sent = present.ask(&json!({"Send": {"message": to_it}}));
        assert!(sent["Ok"].is_object(), "{sent}");
        let heard = watch.next();
        assert_eq!(
            heard["Message"]["message"]["id"],
            json!(format!("{}-7WRKR1-4242-ab12cd", now + 2)),
            "its own quest reaches its watch, not the peer it wrote to: {heard}"
        );

        drop(present);
        let began = Instant::now();
        while held.presences().iter().any(|kept| kept.session == "01b2") {
            assert!(
                began.elapsed() < Duration::from_secs(10),
                "the presence was never let go"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    })
    .await
    .expect("the worker's run");
    let _ = stop.send(());
    node.await.expect("the node's task").expect("the node ends");
}

#[tokio::test]
async fn the_worker_socket_answers_its_own_account_and_asks_alone() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let socket = dir.path().join("mycelium").join("work.sock");
    let store = Memory::default();
    let worker = dir.path().join("worker").join("socket");
    let (stop, node) = alone_with(&store, &socket, worker_beside(dir.path(), own_uid() + 1));
    let asked = worker.clone();
    tokio::task::spawn_blocking(move || {
        let mut stranger = Client::connect(&asked);
        let refused = stranger.next();
        assert!(
            refused["Err"]
                .as_str()
                .is_some_and(|why| why.contains("account")),
            "{refused}"
        );
    })
    .await
    .expect("the stranger's run");
    let _ = stop.send(());
    node.await.expect("the node's task").expect("the node ends");

    let dir = tempfile::tempdir().expect("a temporary folder");
    let socket = dir.path().join("mycelium").join("work.sock");
    let worker = dir.path().join("worker").join("socket");
    let (stop, node) = alone_with(&store, &socket, worker_beside(dir.path(), own_uid()));
    tokio::task::spawn_blocking(move || {
        let mut start = Client::connect(&worker);
        let refused = start.ask(&json!({"Start": {"program": "pi"}}));
        assert!(
            refused["Err"]
                .as_str()
                .is_some_and(|why| why.contains("not an ask")),
            "{refused}"
        );
        let mut sender = Client::connect(&worker);
        let refused = sender.ask(&json!({"Send": {"message": {}}}));
        assert!(
            refused["Err"]
                .as_str()
                .is_some_and(|why| why.contains("opens")),
            "{refused}"
        );
        let mut watch = Client::connect(&worker);
        let refused = watch.ask(&json!({"Watch": {"session": "01b2"}}));
        assert!(
            refused["Err"]
                .as_str()
                .is_some_and(|why| why.contains("present")),
            "{refused}"
        );
    })
    .await
    .expect("the worker's run");
    let _ = stop.send(());
    node.await.expect("the node's task").expect("the node ends");
}

#[test]
fn a_watch_from_a_marker_hears_what_came_after_it() {
    let watch = Watch {
        holding: holding(),
        pid: 1,
        groups: None,
        since: Some("20-7WRKR1-1-b".to_owned()),
    };
    let at = |id: &str, to: &str| -> Message {
        let group = mesh::group_for(to).expect("a group");
        serde_json::from_value(json!({
            "id": id, "at": "", "from": "P9R1KI", "groups": [group], "text": ""
        }))
        .expect("a message")
    };
    assert!(
        !watch.after(&at("19-P9R1KI-1-z", "@7WRKR1")),
        "an earlier time"
    );
    assert!(
        !watch.after(&at("20-7WRKR1-1-a", "@7WRKR1")),
        "the same time, an earlier id"
    );
    assert!(
        watch.after(&at("20-7WRKR1-1-c", "@7WRKR1")),
        "the same time, a later id"
    );
    assert!(watch.after(&at("21-P9R1KI-1-a", "@7WRKR1")), "a later time");
    assert!(
        watch.reaches(&at("21-P9R1KI-1-a", "@7wrkr1")),
        "its own address"
    );
    assert!(
        watch.reaches(&at("21-P9R1KI-1-a", "#quest/kp0l6a")),
        "its own quest"
    );
    assert!(
        !watch.reaches(&at("21-P9R1KI-1-a", "#quest/wcjqsg")),
        "another quest"
    );
    assert!(
        !watch.reaches(&at("21-P9R1KI-1-a", "@P9R1KI")),
        "another peer"
    );
}
