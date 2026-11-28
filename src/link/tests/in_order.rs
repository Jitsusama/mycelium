//! What a worker's end says of its sessions and their messages, in the
//! order they were said, over a link to the machine that made it.

use super::*;

/// A presence of `session`, answering to `address`.
fn presence(session: &str, address: &str) -> Value {
    json!({"session": session, "addresses": [address]})
}

/// A message `id` from `from`, as a worker's session sends one.
fn said_by(id: &str, from: &str) -> Value {
    json!({"id": id, "from": from, "to": ["@WCJQSG"], "text": "the last part"})
}

/// Skiff's and tern's stores joined by a link, skiff taking tern as the
/// worker it made.
fn worker_linked(
    skiff: &Memory,
    tern: &Memory,
) -> (
    tokio::task::JoinHandle<io::Result<Ending>>,
    tokio::task::JoinHandle<io::Result<()>>,
) {
    let (near, far) = tokio::io::duplex(1 << 16);
    let (reading, writing) = tokio::io::split(near);
    let store = skiff.clone();
    let skiff_end = tokio::spawn(async move {
        let worker = Connection {
            worker: true,
            ..connection(reading, writing)
        };
        run(store, &realm("skiff"), worker, None, None).await
    });
    (skiff_end, end(tern, "tern", far))
}

/// Whether `store` holds a presence of `session`.
fn holds(store: &Memory, session: &str) -> bool {
    store.presences().iter().any(|p| p.session == session)
}

// KP0L6A (27 Nov): a room's session sent its last patch part and parked,
// and its maker's node refused the message, as from a session the room
// held no presence for. Another presence had changed just before, and
// the room's end, reading its sessions only once it came to that change,
// found the session gone and said its end ahead of its words.
#[tokio::test]
async fn a_worker_sessions_last_words_cross_before_its_end() {
    let skiff = Memory::default();
    let tern = Memory::default();
    tern.start_here("sak9fz", presence("sak9fz", "SAK9FZ"), true);
    tern.start_here("wren01", presence("wren01", "WREN01"), true);
    let _link = worker_linked(&skiff, &tern);
    until(&skiff, |store| {
        holds(store, "sak9fz") && holds(store, "wren01")
    })
    .await;

    // All before tern's end reads a change, as on a busy room: another
    // session's presence changes, then sak9fz says its last and parks.
    let busy = json!({"session": "wren01", "addresses": ["WREN01"], "busy": true});
    tern.start_here("wren01", busy, true);
    let last = recent(0, "SAK9FZ", 0);
    tern.send_here(&last, said_by(&last, "SAK9FZ"));
    tern.start_here("sak9fz", presence("sak9fz", "SAK9FZ"), false);

    until(&skiff, |store| !holds(store, "sak9fz")).await;
    assert!(
        skiff.message(&last).is_some(),
        "what a session said before it parked was refused as said by nobody tern holds"
    );
}

// The other way round: a session begins and speaks at once, and tern's
// end catches up on what a watch may have missed before it comes to the
// new presence, so the message must not go ahead of the presence.
#[tokio::test]
async fn a_worker_sessions_first_words_cross_after_its_presence() {
    let skiff = Memory::default();
    let tern = Memory::default();
    tern.start_here("wren01", presence("wren01", "WREN01"), true);
    let _link = worker_linked(&skiff, &tern);
    until(&skiff, |store| holds(store, "wren01")).await;

    // A watch that overflowed asks for everything to be read again, and
    // before tern's end does, sak9fz begins and speaks.
    tern.overflow();
    tern.start_here("sak9fz", presence("sak9fz", "SAK9FZ"), true);
    let first = recent(0, "SAK9FZ", 0);
    tern.send_here(&first, said_by(&first, "SAK9FZ"));

    until(&skiff, |store| holds(store, "sak9fz")).await;
    // A later word from the same session, said and told after both, is
    // taken in its turn, so once it is here the first has had its turn too.
    let next = recent(0, "SAK9FZ", 1);
    tern.send_here(&next, said_by(&next, "SAK9FZ"));
    until(&skiff, |store| store.message(&next).is_some()).await;
    assert!(
        skiff.message(&first).is_some(),
        "what a session said as it began went ahead of its presence and was refused"
    );
}
