//! The node's forage records made for ten thousand cells, within 64 MB,
//! and in time that grows with the cells rather than faster. Each record
//! carries its quest's documents, about 5 KB a cell, and is made whole for
//! the reader that asks, never held; what the node holds between asks is
//! its shelf of quests, a small part of that. As in `networks.rs`, the
//! time is said rather than held, since builders run it several times
//! slower:
//!
//! ```sh
//! nix develop -c cargo test --release --test forages -- --nocapture
//! ```
//!
//! The memory is counted by an allocator of this test's own, which only a
//! program of its own may hold, so this is a test of its own.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use mycelium::forage::{self, Cell, Shelf};

use counting::{HELD, PEAK};

/// How many cells the bar is held at.
const CELLS: usize = 10_000;

/// How many cells the time is first taken at, to see how it grows.
const FEWER: usize = CELLS / 10;

/// How many cells work on each quest, as an orchestration's do.
const SHARING: usize = 5;

/// How many times longer ten times the cells may take, as in
/// `networks.rs`.
#[cfg(not(any(target_env = "musl", target_os = "macos")))]
const GROWS_AT_MOST: u32 = 15;

/// How many times longer ten times the cells may take on macOS, whose
/// allocator and a Mac lent to builds took 15.5 on the networks at a load
/// of 95 to 130 (2026-11-16). Work that grows with the square of the
/// cells and is a third of the build takes about forty, so this still
/// tells it.
#[cfg(target_os = "macos")]
const GROWS_AT_MOST: u32 = 25;

/// How many times longer ten times the cells may take under musl, whose
/// allocator slows as the heap grows.
#[cfg(target_env = "musl")]
const GROWS_AT_MOST: u32 = 50;

/// How many times each is timed, the two sizes in turn, the fastest taken.
const TIMINGS: usize = 5;

/// How many times each may be timed while the growth misses the bar, as
/// a loaded machine's turns are slow at random and a few more find its
/// quiet ones (a Mac at a high load took 51 times, 2026-11-16).
const MOST_TIMINGS: usize = 25;

/// The most memory the records and what was read for them may hold, in
/// bytes.
const AT_MOST: usize = 64_000_000;

/// The system's allocator, counting what is held and the most held.
#[path = "realm/counting.rs"]
#[expect(
    dead_code,
    reason = "the forages are timed by the clock, not the thread"
)]
mod counting;

#[test]
fn ten_thousand_cells_make_their_forage_records_within_the_bar() {
    let store = tempfile::tempdir().expect("a quest store");
    let quests = CELLS / SHARING;
    for n in 0..quests {
        write_quest(store.path(), n);
    }
    let cells: Vec<Cell> = (0..CELLS).map(cell).collect();
    let fewer: Vec<Cell> = cells[..FEWER].to_vec();

    let before = HELD.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let mut shelf = Shelf::default();
    for n in 0..quests {
        shelf.read(store.path(), &quest(n));
    }
    let shelved = HELD.load(Ordering::Relaxed) - before;
    let (records, unread) = forage::records(&mut shelf, &cells, "kestrel");
    let peak = PEAK.load(Ordering::Relaxed) - before;
    assert_eq!(records.len(), CELLS, "every cell has its forage");
    assert_eq!(unread, Vec::<String>::new(), "every quest was read");
    drop(records);

    let (least_fewer, least_all) = fastest(&mut shelf, &fewer, &cells);
    eprintln!(
        "{FEWER} cells took {least_fewer:?}; {CELLS} took {least_all:?}, \
         holding {peak} bytes at most, {shelved} of them the shelf"
    );
    assert!(
        peak < AT_MOST,
        "the forages of {CELLS} cells held {peak} bytes at most, past {AT_MOST}"
    );
    assert!(
        least_all < least_fewer * GROWS_AT_MOST,
        "the forages of {CELLS} cells took {least_all:?}, past {GROWS_AT_MOST} times \
         the {least_fewer:?} of {FEWER}"
    );
}

/// The least time the records of `fewer` and of `all` took, of a few
/// made in turn, and more while the growth misses the bar.
fn fastest(shelf: &mut Shelf, fewer: &[Cell], all: &[Cell]) -> (Duration, Duration) {
    let mut timed = |cells: &[Cell]| {
        let begun = Instant::now();
        drop(forage::records(shelf, cells, "kestrel"));
        begun.elapsed()
    };
    let (mut least_fewer, mut least_all) = (Duration::MAX, Duration::MAX);
    for turn in 1..=MOST_TIMINGS {
        least_fewer = least_fewer.min(timed(fewer));
        least_all = least_all.min(timed(all));
        if turn >= TIMINGS && least_all < least_fewer * GROWS_AT_MOST {
            break;
        }
    }
    (least_fewer, least_all)
}

/// The `n`th quest's id.
fn quest(n: usize) -> String {
    format!("QEST-20261116-K{n:05}")
}

/// The `n`th cell, working on a quest it shares with a few others.
fn cell(n: usize) -> Cell {
    Cell {
        address: format!("K{n:05}"),
        session: format!("0000-{n:08}"),
        quest: quest(n / SHARING),
        started_by: Some(format!("K{:05}", n / SHARING * SHARING)),
        started_by_book: true,
        split_from: None,
        split_into: Vec::new(),
        serves: Vec::new(),
        relies: Vec::new(),
    }
}

/// The `n`th quest's record, about the size of a live one: a README and
/// a document of each kind, the plan with forty steps.
fn write_quest(store: &Path, n: usize) {
    let id = quest(n);
    let folder = store.join(&id);
    let readme = format!(
        "---\nid: {id}\nkind: subquest\nparent: QEST-20261101-OSPREY\nstatus: active\n---\n\
         # Teach Kestrel {n} to Hover\n\n## 📜 Summary\n\n\
         Kestrel {n} learns to hover over one field and hold there in a crosswind. \
         It then learns a second field.\n"
    );
    fs::create_dir_all(&folder).expect("its folder");
    fs::write(folder.join("README.md"), readme).expect("its README");
    for (kind, folder_name) in [
        ("plan", "plans"),
        ("research", "research"),
        ("brief", "briefs"),
        ("report", "reports"),
    ] {
        let steps = (0..40).fold(String::new(), |mut steps, step| {
            let ticked = if step < 25 { 'x' } else { ' ' };
            // Writing to a String never fails.
            let _ = writeln!(steps, "- [{ticked}] step {step} of the hover");
            steps
        });
        let document = format!(
            "---\nid: {}-{n:05}\nkind: {kind}\nquest: {id}\nstage: build\n---\n\
             # Hover Over Field {n}\n\n## Steps\n\n{steps}",
            kind.to_uppercase()
        );
        let documents = folder.join(folder_name);
        fs::create_dir_all(&documents).expect("its documents' folder");
        fs::write(
            documents.join(format!("{}-{n:05}.md", kind.to_uppercase())),
            document,
        )
        .expect("its document");
    }
}
