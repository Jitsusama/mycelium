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
#[cfg(not(target_env = "musl"))]
const GROWS_AT_MOST: u32 = 15;

/// How many times longer ten times the cells may take under musl, whose
/// allocator slows as the heap grows.
#[cfg(target_env = "musl")]
const GROWS_AT_MOST: u32 = 50;

/// How many times each is timed, the two sizes in turn, the fastest taken.
const TIMINGS: usize = 5;

/// The most memory the records and what was read for them may hold, in
/// bytes.
const AT_MOST: usize = 64_000_000;

/// The system's allocator, counting what is held and the most held.
#[expect(unsafe_code, reason = "an allocator is unsafe to implement")]
mod counting {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The system's allocator, counting.
    struct Counting;

    /// How many bytes are held now.
    pub static HELD: AtomicUsize = AtomicUsize::new(0);

    /// The most bytes held since it was last reset.
    pub static PEAK: AtomicUsize = AtomicUsize::new(0);

    /// That `bytes` more are held.
    fn took(bytes: usize) {
        let held = HELD.fetch_add(bytes, Ordering::Relaxed) + bytes;
        PEAK.fetch_max(held, Ordering::Relaxed);
    }

    /// That `bytes` fewer are held.
    fn gave(bytes: usize) {
        HELD.fetch_sub(bytes, Ordering::Relaxed);
    }

    // SAFETY: each call is the system allocator's own, only counted.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: the caller keeps `alloc`'s contract, which this passes on.
            let made = unsafe { System.alloc(layout) };
            if !made.is_null() {
                took(layout.size());
            }
            made
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: the caller keeps `dealloc`'s contract, which this passes on.
            unsafe { System.dealloc(ptr, layout) };
            gave(layout.size());
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            // SAFETY: the caller keeps `realloc`'s contract, which this passes on.
            let made = unsafe { System.realloc(ptr, layout, new_size) };
            if !made.is_null() {
                took(new_size);
                gave(layout.size());
            }
            made
        }
    }

    /// Every allocation this test makes, counted.
    #[global_allocator]
    static COUNTING: Counting = Counting;
}

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
    let (records, unread) = forage::records(&mut shelf, &cells, "kestrel");
    let peak = PEAK.load(Ordering::Relaxed) - before;
    assert_eq!(records.len(), CELLS, "every cell has its forage");
    assert_eq!(unread, Vec::<String>::new(), "every quest was read");
    drop(records);

    let (least_fewer, least_all) = fastest(&mut shelf, &fewer, &cells);
    eprintln!(
        "{FEWER} cells took {least_fewer:?}; {CELLS} took {least_all:?}, \
         holding {peak} bytes at most"
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
/// made in turn.
fn fastest(shelf: &mut Shelf, fewer: &[Cell], all: &[Cell]) -> (Duration, Duration) {
    let mut timed = |cells: &[Cell]| {
        let begun = Instant::now();
        drop(forage::records(shelf, cells, "kestrel"));
        begun.elapsed()
    };
    (0..TIMINGS).fold((Duration::MAX, Duration::MAX), |(least, most), _| {
        (least.min(timed(fewer)), most.min(timed(all)))
    })
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
