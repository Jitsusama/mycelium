//! What a link owes in bulk, a catch-up's messages, keeps to the pace its
//! node's links share, and what a session waits on goes between, never
//! behind it.

use std::time::Instant;

use super::*;

/// 80,000 bits a second, or 10,000 bytes: slow enough that a dozen
/// messages of a kilobyte take a while to cross, fast enough that a test
/// waits only a few seconds.
const BITS_A_SECOND: u64 = 80_000;
const BYTES_A_SECOND: usize = 10_000;

/// How many messages skiff holds, of about a kilobyte each.
const MESSAGES: usize = 12;

/// Skiff's store, linked to tern and kestrel an hour ago, holding
/// `MESSAGES` messages of about a kilobyte each said since, the last
/// one's id with it.
fn skiff_with_messages() -> (Memory, String) {
    let skiff = Memory::default();
    for machine in ["tern", "kestrel"] {
        skiff
            .mark_linked(machine, now() - 60 * 60 * 1000)
            .expect("a memory store marks");
    }
    let mut last = String::new();
    for n in 0..MESSAGES {
        last = recent(
            60_000 - 1000 * i64::try_from(n).expect("a few"),
            "WREN01",
            n,
        );
        skiff.send_here(
            &last,
            json!({"id": last, "to": ["@HARB01"], "text": "a word ".repeat(150)}),
        );
    }
    (skiff, last)
}

/// Runs skiff's end of a link to `machine` on `skiff`, at `pace`, and
/// says the far end's Hello and empty Have, then `then`: the far end's
/// lines, to read what skiff says, and its writer, kept so skiff's end
/// doesn't take the far end as gone.
async fn skiff_paced_to(
    skiff: &Memory,
    machine: &str,
    pace: &pace::Pace,
    dir: &std::path::Path,
    then: Vec<Frame>,
) -> (
    tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
    WriteHalf<DuplexStream>,
) {
    let starting = Starting {
        pace: pace.clone(),
        ..steering(&dir.join(machine))
    };
    let (near, far) = tokio::io::duplex(1 << 16);
    let (reading, writing) = tokio::io::split(near);
    let store = skiff.clone();
    tokio::spawn(async move {
        let connection = connection(reading, writing);
        run(store, &realm("skiff"), connection, None, Some(starting)).await
    });
    let (far_reading, mut far_writing) = tokio::io::split(far);
    let mut frames = vec![
        Frame::Hello(realm(machine).hello()),
        Frame::Have {
            messages: Vec::new(),
            since: None,
            living: Some(Vec::new()),
        },
    ];
    frames.extend(then);
    says(&mut far_writing, &frames).await;
    (BufReader::new(far_reading).lines(), far_writing)
}

/// The bytes the messages among `heard` took on the wire, and the most
/// one took.
fn message_bytes(heard: &[Frame]) -> (usize, usize) {
    heard
        .iter()
        .filter(|frame| matches!(frame, Frame::Message { .. }))
        .map(|frame| line_of(frame).expect("a frame writes").len())
        .fold((0, 0), |(all, most), bytes| (all + bytes, most.max(bytes)))
}

// On 27 November a node's links to about ten machines each caught up as
// fast as the pipe took it and filled a 10.8 Mbit/s uplink for about 15
// minutes. Two links sharing a pace together take as long as one would
// to send all both send.
#[tokio::test]
async fn two_links_catching_up_together_keep_to_the_pace_they_share() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let (skiff, last) = skiff_with_messages();
    let pace = pace::Pace::at_most(BITS_A_SECOND);
    let began = Instant::now();
    let (mut tern, _tern) = skiff_paced_to(&skiff, "tern", &pace, dir.path(), Vec::new()).await;
    let (mut kestrel, _kestrel) =
        skiff_paced_to(&skiff, "kestrel", &pace, dir.path(), Vec::new()).await;
    let is_last = |frame: &Frame| matches!(frame, Frame::Message { id, .. } if *id == last);
    let (to_tern, to_kestrel) = tokio::join!(
        heard_until(&mut tern, is_last),
        heard_until(&mut kestrel, is_last)
    );
    let took = began.elapsed();
    let (tern_bytes, tern_most) = message_bytes(&to_tern);
    let (kestrel_bytes, kestrel_most) = message_bytes(&to_kestrel);
    let sent = tern_bytes + kestrel_bytes;
    // The pace lets a quarter second's bytes go at once, and each link
    // may send one frame past it as two ask at once.
    let ahead = BYTES_A_SECOND / 4 + tern_most + kestrel_most;
    let floor = sent.saturating_sub(ahead) * 1000 / BYTES_A_SECOND;
    let floor = Duration::from_millis(u64::try_from(floor).expect("a few seconds"));
    eprintln!("two links caught up {sent} bytes of messages in {took:?} at {BYTES_A_SECOND} B/s");
    assert!(
        took >= floor,
        "{sent} bytes crossed in {took:?}, faster than the pace lets them, {floor:?}"
    );
}

// A catch-up's messages went in one go as the far end's Have was taken,
// and nothing else the link carried went until they all had: a model
// call's answer waited behind a week of messages.
#[tokio::test]
async fn a_lend_is_answered_while_a_catch_up_still_owes_messages() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let (skiff, last) = skiff_with_messages();
    let pace = pace::Pace::at_most(BITS_A_SECOND);
    let lend = Lend {
        budget: None,
        method: "POST".to_owned(),
        path: "/apis/anthropic/v1/messages".to_owned(),
        headers: Vec::new(),
        body: String::new(),
    };
    let asked = vec![Frame::Lend { ask: 7, lend }];
    let (mut tern, _tern) = skiff_paced_to(&skiff, "tern", &pace, dir.path(), asked).await;
    let heard = heard_until(
        &mut tern,
        |frame| matches!(frame, Frame::Message { id, .. } if *id == last),
    )
    .await;
    let answered = heard
        .iter()
        .position(|frame| matches!(frame, Frame::Lent { ask: 7, .. }));
    assert!(
        answered.is_some_and(|answered| answered < heard.len() - 1),
        "the lend is answered before the catch-up's last message: {answered:?} of {}",
        heard.len()
    );
}

// Measured, to say what a link sends as it opens and what it costs, frame
// by frame: what a catch-up sends is most of it, and goes in bulk.
#[tokio::test]
async fn a_link_opening_says_what_it_sends_by_kind() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let (skiff, last) = skiff_with_messages();
    for n in 0..20 {
        let session = format!("session-{n}");
        skiff.start_here(
            &session,
            json!({"session": session, "addresses": [format!("WREN{n:02}")], "busy": false,
                   "state": "idle", "at": "2026-11-27T21:28:00.000Z"}),
            true,
        );
    }
    let unpaced = pace::Pace::unpaced();
    let (mut tern, _tern) = skiff_paced_to(&skiff, "tern", &unpaced, dir.path(), Vec::new()).await;
    let heard = heard_until(
        &mut tern,
        |frame| matches!(frame, Frame::Message { id, .. } if *id == last),
    )
    .await;
    let mut by_kind: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for frame in &heard {
        let line = line_of(frame).expect("a frame writes");
        let value: Value = serde_json::from_slice(&line).expect("a frame reads");
        let kind = value
            .as_object()
            .and_then(|fields| fields.keys().next().cloned())
            .or_else(|| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        let counted = by_kind.entry(kind).or_default();
        counted.0 += 1;
        counted.1 += line.len();
    }
    eprintln!("a link opening sent, by kind (frames, bytes): {by_kind:?}");
    assert_eq!(
        by_kind.get("Message").map(|counted| counted.0),
        Some(MESSAGES),
        "each message crosses once"
    );
    assert_eq!(
        by_kind.get("Presence").map(|counted| counted.0),
        Some(20),
        "and each presence"
    );
}
