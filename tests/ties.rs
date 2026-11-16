//! The node's tie records made for a hundred, a thousand and ten thousand
//! cells, five ties a cell (fifty thousand at ten thousand), each end
//! drawn by a power law over the cells and each tie's messages by a power
//! law over its rank, as a few cells talk far more than the rest. The
//! target is 100 ms and 30 MB at ten thousand, each growing within 1.2
//! times per cell from a hundred; the test says what it measured and
//! holds only a ceiling for the worst, as the networks' does:
//!
//! ```sh
//! nix develop -c cargo test --release --test ties -- --nocapture
//! ```
//!
//! The time is the processor's own for this thread, not the clock's, so a
//! builder busy with other work can't fail it, and the memory is counted
//! by an allocator of the test's own, which only a program of its own may
//! hold.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::time::Duration;

use mycelium::group::Group;
use mycelium::node::{talk, tie_records};
use mycelium::record::{Intent, Message};
use serde_json::Value;

use counting::{HELD, PEAK, thread_time};

/// The system's allocator, counting what is held and the most held.
#[expect(unsafe_code, reason = "an allocator is unsafe to implement")]
#[path = "realm/counting.rs"]
mod counting;

/// The realms measured, by their cells.
const SIZES: [usize; 3] = [100, 1_000, 10_000];

/// How many ties each cell has, on average.
const TIES_EACH: usize = 5;

/// The most the records of ten thousand cells may hold while made, in
/// bytes: 70 MB, while each record is held parsed. The target is 30 MB,
/// once each record is held as the bytes it is sent as, owed by
/// 2026-11-17 12:00.
const AT_MOST: usize = 70_000_000;

/// How many times the time or the memory per cell may grow from a
/// thousand cells to ten thousand, a ceiling for the worst. The target is
/// 1.2; work growing with the square of the cells grows ten times.
const GROWS_AT_MOST: f64 = 3.0;

/// The most ten thousand cells' records may take of the processor's time,
/// a stop for the worst; the target, 100 ms on the machine the bar is set
/// for, is said, not held.
const WITHIN: Duration = Duration::from_secs(1);

/// How many times each build is timed, the sizes in turn so a busy moment
/// slows each alike, and the fastest of each taken.
const TIMINGS: usize = 5;

/// When the realm stands, in milliseconds since the epoch.
const NOW: i64 = 1_795_435_200_000;

/// A day, in milliseconds.
const DAY: u64 = 24 * 60 * 60 * 1000;

#[test]
fn ten_thousand_cells_make_their_tie_records_within_the_bar() {
    let realms: Vec<Realm> = SIZES.iter().map(|&cells| Realm::of(cells)).collect();
    let mut least = [Duration::MAX; SIZES.len()];
    let mut most_held = [0; SIZES.len()];
    for _ in 0..TIMINGS {
        for (at, realm) in realms.iter().enumerate() {
            let (took, held) = realm.made();
            least[at] = least[at].min(took);
            most_held[at] = held;
        }
    }
    for (at, cells) in SIZES.iter().enumerate() {
        eprintln!(
            "{cells} cells, {} messages: {:?}, {} bytes at most; {:?} and {} bytes a cell",
            realms[at].messages.len(),
            least[at],
            most_held[at],
            least[at] / u32::try_from(*cells).expect("a few cells"),
            most_held[at] / cells,
        );
    }

    let records = realms[2].records();
    let ties = records.keys().filter(|key| key.contains('~')).count();
    assert_eq!(ties, SIZES[2] * TIES_EACH, "five ties a cell");
    let all = least[2];
    let held = most_held[2];
    assert!(held < AT_MOST, "ten thousand cells' ties held {held} bytes");
    assert!(all < WITHIN, "ten thousand cells' ties took {all:?}");
    let per_cell = |at: usize, of: f64| of / f64::from(u32::try_from(SIZES[at]).expect("cells"));
    let grew = |of: &dyn Fn(usize) -> f64| per_cell(2, of(2)) / per_cell(1, of(1));
    let time = grew(&|at| least[at].as_secs_f64());
    #[expect(clippy::cast_precision_loss, reason = "bytes held fit a double")]
    let memory = grew(&|at| most_held[at] as f64);
    eprintln!("from a thousand cells, time a cell grew {time:.2} times, memory {memory:.2}");
    assert!(
        time < GROWS_AT_MOST,
        "the time a cell grew {time:.2} times from a thousand to ten thousand"
    );
    assert!(
        memory < GROWS_AT_MOST,
        "the memory a cell grew {memory:.2} times from a thousand to ten thousand"
    );
}

/// The messages of a realm of cells, as the node holds them.
struct Realm {
    messages: Vec<Message>,
    aliases: HashMap<String, String>,
}

impl Realm {
    /// A realm of `cells`, each with [`TIES_EACH`] ties on average and a
    /// broadcast for every other cell, all said in the last day.
    fn of(cells: usize) -> Self {
        let mut draws = Draws(u64::try_from(cells).expect("a few cells"));
        let mut pairs = HashSet::new();
        let mut ties = Vec::new();
        while ties.len() < cells * TIES_EACH {
            let (one, other) = (draws.cell(cells), draws.cell(cells));
            if one != other && pairs.insert((one.min(other), one.max(other))) {
                ties.push((one, other));
            }
        }
        let head = ties.len() / 50;
        let mut messages = Vec::new();
        for (rank, (one, other)) in ties.into_iter().enumerate() {
            for each in 0..(head / (rank + 1)).max(1) {
                let (from, to) = if each % 2 == 0 {
                    (one, other)
                } else {
                    (other, one)
                };
                let mut groups = vec![format!("peer.{}", address(to).to_lowercase())];
                if each % 3 == 0 {
                    groups.push(format!("quest.q{:05}", from / 5));
                }
                let re = (each % 2 == 1).then(|| messages.len() - 1);
                let message = said(&mut draws, messages.len(), from, &groups, re, &messages);
                messages.push(message);
            }
        }
        for _ in 0..cells / 2 {
            let from = draws.cell(cells);
            let groups = [format!("orchestration.o{:04}", from / 40)];
            let message = said(&mut draws, messages.len(), from, &groups, None, &messages);
            messages.push(message);
        }
        messages.sort_by(|a, b| (&a.at, &a.id).cmp(&(&b.at, &b.id)));
        let aliases = (0..cells).map(|n| (address(n), address(n))).collect();
        Self { messages, aliases }
    }

    /// Its tie records.
    fn records(&self) -> BTreeMap<String, Value> {
        tie_records(talk(&self.messages, &self.aliases, NOW))
    }

    /// How long the processor took making its records, on this thread,
    /// and the most they held while made.
    fn made(&self) -> (Duration, usize) {
        let before = HELD.load(Ordering::Relaxed);
        PEAK.store(before, Ordering::Relaxed);
        let begun = thread_time();
        drop(self.records());
        let took = thread_time().saturating_sub(begun);
        (took, PEAK.load(Ordering::Relaxed) - before)
    }
}

/// The `n`th message, from the cell `from` to `groups` at a time in the
/// last day, answering `messages`' `re`th where it names one.
fn said(
    draws: &mut Draws,
    n: usize,
    from: usize,
    groups: &[String],
    re: Option<usize>,
    messages: &[Message],
) -> Message {
    let ago = i64::try_from(draws.next() % DAY).expect("a day fits");
    let at = jiff::Timestamp::from_millisecond(NOW - ago).expect("a time");
    let intents = [Intent::Fyi, Intent::Question, Intent::Finding];
    Message {
        id: format!("{n:012x}"),
        at: at.to_string(),
        from: address(from),
        groups: groups
            .iter()
            .map(|group| Group::new(group.as_str()).expect("a group"))
            .collect(),
        intent: Some(intents[n % intents.len()]),
        text: "What was said, which stays on the node, about as long as most.".repeat(4),
        re: re.map(|re| messages[re].id.clone()),
        supersedes: None,
        from_realm: None,
        for_realm: None,
        ends_hold: false,
    }
}

/// The `n`th cell's address.
fn address(n: usize) -> String {
    format!("C{n:05}")
}

/// Numbers drawn the same way each run (`SplitMix64`).
struct Draws(u64);

impl Draws {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A cell of `cells`, the first far likelier than the last, as a
    /// power law draws them.
    fn cell(&mut self, cells: usize) -> usize {
        #[expect(clippy::cast_precision_loss, reason = "a draw needs no more")]
        let unit = (self.next() >> 11) as f64 / (1_u64 << 53) as f64;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "a fraction of the cells is a cell"
        )]
        let cell = (unit.powi(3) * cells as f64) as usize;
        cell.min(cells - 1)
    }
}
