//! The node's hub records made for ten thousand cells within a bar of
//! their own, as a program of its own, since only one may hold the
//! allocator counting its memory. Both realms are measured in one test,
//! since two tests at once would count each other's memory.
//!
//! The realm `realm` builds is held to its bars and its growth, and a
//! realm of every cell saying as many subjects as it may, each as long
//! as it may be and outside ASCII, by addresses as long as they may be,
//! to bars of its own. Each realm is held at two stages, the most held
//! once its holders are read and once its hubs are made, each as its
//! bytes, so a needless copy of each subject kept beside what a stage
//! holds shows. A debug build held what a release build did, to the
//! byte, so the bars hold in both. Each says what it held at both:
//!
//! ```sh
//! nix develop -c cargo test --release --test hubs -- --nocapture
//! ```

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use mycelium::daemon::Record;
use mycelium::forage::{ADDRESS_MOST, is_address};
use mycelium::networks::{MOST_SUBJECTS, SUBJECT_MOST, subject_key};
use mycelium::node::{hub_holders, hub_records};
use mycelium::record::Session;
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

/// The most the realm's holders may hold once read, in bytes. It was
/// 17,357,480 measured in a release build in a Nix sandbox on 28
/// November 2026, and a copy of each presence's subjects kept while the
/// holders are read held 19,409,256. Set halfway to that, lower than
/// the peak plus 10%. Set at the flake's rustc 1.99.0 and `serde_json`
/// 1.0.154, and measured again when either moves.
const REALM_READ_AT_MOST: usize = 18_380_000;

/// The most the realm's hubs may hold once made, in bytes. It was
/// 17,880,556 measured as the holders were, and a copy of each subject
/// kept beside the records until they are made held 19,253,828. Set
/// halfway to that, lower than the peak plus 10%. Set at the flake's
/// rustc 1.99.0 and `serde_json` 1.0.154, and measured again when either
/// moves.
const REALM_AT_MOST: usize = 18_560_000;

/// The most the holders of every cell saying [`MOST_SUBJECTS`] of the
/// longest subjects may hold once read, in bytes. It was 138,079,992
/// measured as the realm's were, and a copy of each presence's subjects
/// kept while the holders are read held 259,039,992. Set at the peak
/// plus about 10%, lower than halfway to that. Set at the flake's rustc
/// 1.99.0 and `serde_json` 1.0.154, and measured again when either moves.
const LONGEST_READ_AT_MOST: usize = 152_000_000;

/// The most the hubs of every cell saying [`MOST_SUBJECTS`] of the
/// longest subjects may hold once made, in bytes. It was 556,284,696
/// measured as the realm's were, and a copy of each subject kept beside
/// the records until they are made held 677,244,696. Set at the peak
/// plus about 10%, lower than halfway to that. Set at the flake's rustc
/// 1.99.0 and `serde_json` 1.0.154, and measured again when either moves.
const LONGEST_AT_MOST: usize = 612_000_000;

#[test]
fn ten_thousand_cells_make_their_hub_records_within_the_bar() {
    let sessions = realm::sessions(realm::CELLS);
    let made = staged("the realm", &sessions);
    assert_eq!(
        made.records, made.subjects,
        "each different subject the realm's cells say has its hubs"
    );
    assert!(
        made.read < REALM_READ_AT_MOST,
        "the realm's holders held {} bytes at most once read, past {REALM_READ_AT_MOST}",
        made.read
    );
    // The peak once the hubs are made, told above, is held here, with
    // the time.
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
        session.address = longest_address(n);
        Arc::make_mut(&mut session.presence)["subjects"] = longest(n);
    }
    let made = staged("the longest subjects", &sessions);
    assert_eq!(
        made.records,
        realm::CELLS * MOST_SUBJECTS,
        "each cell's every subject has its hubs"
    );
    assert!(
        made.read < LONGEST_READ_AT_MOST,
        "the holders of the longest subjects held {} bytes at most once read, past \
         {LONGEST_READ_AT_MOST}",
        made.read
    );
    assert!(
        made.peak < LONGEST_AT_MOST,
        "the hubs of the longest subjects held {} bytes at most once made, past \
         {LONGEST_AT_MOST}",
        made.peak
    );
}

/// What making the hubs of a realm came to.
struct Staged {
    /// The most held beyond what was given once the holders were read.
    read: usize,
    /// The most held beyond what was given once the hubs were made, each
    /// as its bytes.
    peak: usize,
    /// How many hub records were made.
    records: usize,
    /// How many different subjects the holders say, as the cap folds
    /// them.
    subjects: usize,
}

/// The hub records made of `sessions` as the node makes them, told at
/// each stage. The sessions are cloned within what is measured, as the
/// node clones them out of what it holds; the fold of the ties that says
/// how much each cell was heard stays outside it, as here none was.
fn staged(realm: &str, sessions: &[Session]) -> Staged {
    let heard = HashMap::new();
    let before = HELD.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let holders = hub_holders(sessions.to_vec(), &heard);
    let read = PEAK.load(Ordering::Relaxed) - before;
    let records = hub_records(&holders);
    let peak = PEAK.load(Ordering::Relaxed) - before;
    let said: usize = holders.iter().map(|holder| holder.subjects.len()).sum();
    let subjects = holders
        .iter()
        .flat_map(|holder| &holder.subjects)
        .map(|subject| subject_key(subject))
        .collect::<BTreeSet<String>>()
        .len();
    drop(holders);
    eprintln!(
        "{realm}: {said} subjects said, {} hubbed; at most {read} bytes held once the \
         holders were read and {peak} once the hubs were made, {} bytes a subject said",
        records.len(),
        peak / said.max(1),
    );
    Staged {
        read,
        peak,
        records: records.len(),
        subjects,
    }
}

/// The hubs of `sessions`, as the node makes them of those it holds.
fn hubs_of(sessions: &[Session]) -> BTreeMap<String, Record> {
    hub_records(&hub_holders(sessions.to_vec(), &HashMap::new()))
}

/// The `n`th cell's address, as long as one may be, still read as one.
fn longest_address(n: usize) -> String {
    let address = format!("CELL{n:0>width$}", width = ADDRESS_MOST - 4);
    assert!(
        address.len() == ADDRESS_MOST && is_address(&address),
        "{address} is an address as long as one may be"
    );
    address
}

/// The `n`th cell's subjects, as many as a cell may say, each as many
/// characters as the cap lets one hold, [`SUBJECT_MOST`], of four bytes
/// each, past ASCII: the cell and the subject's place in bold digits,
/// then a Fraktur letter, so no two cells share one.
fn longest(n: usize) -> Value {
    let said: Vec<String> = (0..MOST_SUBJECTS)
        .map(|nth| {
            let place = format!("{n:05}{nth:02}");
            let digits: String = place.chars().map(bold).collect();
            let rest = SUBJECT_MOST - digits.chars().count();
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
