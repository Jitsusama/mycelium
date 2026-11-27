//! The node's hub records made for ten thousand cells within a bar of
//! their own, as a program of its own, since only one may hold the
//! allocator counting its memory. Both realms are measured in one test,
//! since two tests at once would count each other's memory.
//!
//! The realm `realm` builds is held to its bar and its growth, and a
//! realm of every cell saying as many subjects as it may, each as long
//! as it may be and outside ASCII, to a bar of its own, so a needless
//! copy of each subject shows. Each says what it held at peak once its
//! holders were read and once its hubs were made, each as its bytes:
//!
//! ```sh
//! nix develop -c cargo test --release --test hubs -- --nocapture
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use mycelium::daemon::Record;
use mycelium::node::{HUB_KIND, HubHolder, hub_records};
use mycelium::record::{Session, State};
use serde_json::Value;

use realm::counting::{HELD, PEAK};

mod realm;

/// Half the cells to all of them at most three times as long, as the
/// networks are held: the hubs sort each subject's holders and the
/// subjects, which grows a little faster than the cells, far short of
/// twice.
#[cfg(not(any(target_env = "musl", target_os = "macos")))]
const GROWTH: realm::Growth = realm::Growth {
    from: realm::CELLS / 2,
    times: 3,
    within: None,
};

/// On macOS and under musl, whose allocators slow as the heap grows, a
/// tenth of the cells to all of them at most fifty times as long, as
/// the networks are held under musl, since neither is measured here.
#[cfg(any(target_env = "musl", target_os = "macos"))]
const GROWTH: realm::Growth = realm::Growth {
    from: realm::CELLS / 10,
    times: 50,
    within: None,
};

/// The most the realm's hubs may hold, in bytes: 42,909,892 measured in
/// the room's Nix sandbox on 27 November 2026, plus about 10%. It is
/// above the networks' 30 MB, so the hub kind costs more than the
/// networks do on the same realm.
const REALM_AT_MOST: usize = 47_200_000;

/// The most the hubs of every cell saying [`mycelium::networks::MOST_SUBJECTS`]
/// of the longest subjects may hold, in bytes: 840,622,368 measured in
/// the room's Nix sandbox on 27 November 2026, plus about 10%, so one
/// more copy of each subject fails it.
const LONGEST_AT_MOST: usize = 925_000_000;

/// The characters of a longest subject, each four bytes, past ASCII.
const LONGEST: usize = 120;

#[test]
fn ten_thousand_cells_make_their_hub_records_within_the_bar() {
    let sessions = realm::sessions(realm::CELLS);
    let (peak, records) = staged("the realm", &sessions);
    let hub = format!("{HUB_KIND}:");
    assert!(
        records.keys().all(|key| key.starts_with(&hub)),
        "each record is a hub"
    );
    drop(records);
    assert!(
        peak < REALM_AT_MOST,
        "the realm's hubs held {peak} bytes at most, past {REALM_AT_MOST}"
    );
    realm::holds_the_bar(
        realm::CELLS,
        "each subject the realm's cells say has its hubs",
        REALM_AT_MOST,
        &GROWTH,
        |holding| hubs_of(holding.sessions),
    );
    drop(sessions);

    let mut sessions = realm::sessions(realm::CELLS);
    for (n, session) in sessions.iter_mut().enumerate() {
        Arc::make_mut(&mut session.presence)["subjects"] = longest(n);
    }
    let (peak, records) = staged("the longest subjects", &sessions);
    let subjects = realm::CELLS * mycelium::networks::MOST_SUBJECTS;
    assert_eq!(
        records.len(),
        subjects,
        "each cell's every subject has its hubs"
    );
    drop(records);
    assert!(
        peak < LONGEST_AT_MOST,
        "the hubs of the longest subjects held {peak} bytes at most, past {LONGEST_AT_MOST}"
    );
}

/// The hub records made of `sessions` as the node makes them, told at
/// each step: the most held beyond what was given once the holders are
/// read, and once the hubs are made, each as its bytes, which is the
/// peak given back with the records.
fn staged(realm: &str, sessions: &[Session]) -> (usize, BTreeMap<String, Record>) {
    let before = HELD.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let holders = holders_of(sessions);
    let read = PEAK.load(Ordering::Relaxed) - before;
    let said: usize = holders.iter().map(|holder| holder.subjects.len()).sum();
    let records = hub_records(&holders);
    drop(holders);
    let peak = PEAK.load(Ordering::Relaxed) - before;
    eprintln!(
        "{realm}: {said} subjects said, {} hubbed; at most {read} bytes held once the \
         holders were read and {peak} once the hubs were made, {} bytes a subject said",
        records.len(),
        peak / said.max(1),
    );
    (peak, records)
}

/// The hubs of `sessions`, as the node makes them of those it holds.
fn hubs_of(sessions: &[Session]) -> BTreeMap<String, Record> {
    hub_records(&holders_of(sessions))
}

/// The holders of `sessions`, as the node reads them: each running and
/// named by an address, saying a subject, heard not at all.
fn holders_of(sessions: &[Session]) -> Vec<HubHolder> {
    sessions
        .iter()
        .filter(|session| matches!(session.state, State::Busy | State::Idle))
        .filter(|session| mycelium::forage::is_address(&session.address))
        .map(|session| HubHolder {
            heard: 0.0,
            subjects: mycelium::networks::subjects(&session.presence["subjects"]),
            address: session.address.clone(),
        })
        .filter(|holder| !holder.subjects.is_empty())
        .collect()
}

/// The `n`th cell's subjects, as many as a cell may say, each [`LONGEST`]
/// characters of four bytes: the cell and the subject's place in bold
/// digits, then a Fraktur letter, so no two cells share one.
fn longest(n: usize) -> Value {
    let said: Vec<String> = (0..mycelium::networks::MOST_SUBJECTS)
        .map(|nth| {
            let place = format!("{n:05}{nth:02}");
            let digits: String = place.chars().map(bold).collect();
            let rest = LONGEST - digits.chars().count();
            format!("{digits}{}", "\u{1D51E}".repeat(rest))
        })
        .collect();
    Value::from(said)
}

/// `digit` as a mathematical bold digit, four bytes outside ASCII.
fn bold(digit: char) -> char {
    let value = digit.to_digit(10).unwrap_or_default();
    char::from_u32(0x1D7CE + value).unwrap_or('\u{1D7CE}')
}
