//! The node's murmur records read for ten thousand cells, within the
//! memory the map and the launcher hold it to, 30 MB, in time that grows
//! with the cells rather than faster, and read again after one more
//! murmur in a small share of the time, as only what changed is read.
//! The time is the processor's own for this thread, not the clock's, so
//! a builder busy with other work can't fail it. The time itself, 100 ms
//! on the machine the bar is set for, is no builder's to hold, so the
//! test says it rather than holds it:
//!
//! ```sh
//! nix develop -c cargo test --release --test murmurs -- --nocapture
//! ```
//!
//! The memory is counted by an allocator of this test's own, which only a
//! program of its own may hold, so this is a test of its own.

use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use mycelium::murmur::{self, Webs};
use mycelium::web::{self, Event, Said};

use counting::{HELD, PEAK};

/// How many cells the bar is held at, each murmuring once.
const CELLS: usize = 10_000;

/// How many cells the time is first taken at, to see how it grows.
const FEWER: usize = CELLS / 10;

/// How many cells murmur in each quest's web.
const EACH: usize = 100;

/// How many times longer ten times the cells may take: work that grows
/// with them takes about ten times as long, and under musl, whose
/// allocator slows as the heap grows, up to about thirty.
#[cfg(not(target_env = "musl"))]
const GROWS_AT_MOST: u32 = 15;

/// How many times longer ten times the cells may take under musl.
#[cfg(target_env = "musl")]
const GROWS_AT_MOST: u32 = 50;

/// How many times each read is timed, and the fastest taken.
const TIMINGS: usize = 5;

/// The most memory the read may hold, in bytes.
const AT_MOST: usize = 30_000_000;

/// A time the murmurs are said near, in whole seconds.
const NOW: i64 = 1_794_800_000;

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
fn ten_thousand_cells_murmurs_are_read_within_the_bar_and_read_again_by_what_changed() {
    let all = tempfile::tempdir().expect("a temporary folder");
    let fewer = tempfile::tempdir().expect("a temporary folder");
    said(all.path(), CELLS);
    said(fewer.path(), FEWER);

    let before = HELD.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let mut webs = Webs::new(all.path().to_owned(), "skiff".to_owned());
    webs.read();
    let records = murmur::records(webs.said_since(NOW - murmur::KEPT_FOR), &purposes());
    let peak = PEAK.load(Ordering::Relaxed) - before;
    assert_eq!(records.len(), CELLS, "each cell's murmur is a record");
    drop(records);

    let (least, most) = (0..TIMINGS).fold((Duration::MAX, Duration::MAX), |(least, most), _| {
        (least.min(timed(fewer.path())), most.min(timed(all.path())))
    });
    let again = (0..TIMINGS)
        .map(|turn| {
            let line = event(CELLS + turn, NOW + 1);
            let file = web::events_in(&all.path().join("QEST-00000").join(web::FOLDER))
                .join("skiff.jsonl");
            let mut open = fs::OpenOptions::new()
                .append(true)
                .open(file)
                .expect("a writer's file");
            open.write_all(line.as_bytes()).expect("one more murmur");
            let begun = thread_time();
            assert!(webs.read(), "one more murmur is read");
            thread_time().saturating_sub(begun)
        })
        .min()
        .unwrap_or(Duration::MAX);
    eprintln!(
        "{FEWER} cells took {least:?}; {CELLS} took {most:?}, holding {peak} bytes at most; \
         one more murmur took {again:?}"
    );
    assert!(
        peak < AT_MOST,
        "the murmurs of {CELLS} cells held {peak} bytes at most, past {AT_MOST}"
    );
    assert!(
        most < least * GROWS_AT_MOST,
        "the murmurs of {CELLS} cells took {most:?}, past {GROWS_AT_MOST} times the \
         {least:?} of {FEWER}"
    );
    assert!(
        again < most / 4,
        "reading one more murmur took {again:?}, near the {most:?} of reading every one"
    );
    assert_eq!(
        webs.said_since(NOW - murmur::KEPT_FOR).count(),
        CELLS + TIMINGS,
        "each murmur more is held"
    );
}

/// How long the processor took reading the murmurs of the webs in
/// `scratch` from nothing, and making their records, on this thread.
fn timed(scratch: &Path) -> Duration {
    let begun = thread_time();
    let mut webs = Webs::new(scratch.to_owned(), "skiff".to_owned());
    webs.read();
    drop(murmur::records(
        webs.said_since(NOW - murmur::KEPT_FOR),
        &purposes(),
    ));
    thread_time().saturating_sub(begun)
}

/// How long the processor has run this thread, which waiting for it
/// doesn't count towards.
fn thread_time() -> Duration {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
    Duration::new(
        u64::try_from(now.tv_sec).unwrap_or(0),
        u32::try_from(now.tv_nsec).unwrap_or(0),
    )
}

/// What every hundredth cell says it works on.
fn purposes() -> HashMap<String, String> {
    (0..CELLS)
        .step_by(100)
        .map(|n| (address(n), format!("the work {} leads", address(n))))
        .collect()
}

/// The webs of `cells` cells in `scratch`, a hundred cells to each
/// quest's, each cell's murmur tied to a repository and a change, written
/// as the machine's file is.
fn said(scratch: &Path, cells: usize) {
    for quest in 0..cells / EACH {
        let folder = web::events_in(&scratch.join(format!("QEST-{quest:05}")).join(web::FOLDER));
        fs::create_dir_all(&folder).expect("a web's folder");
        let lines: String = (quest * EACH..(quest + 1) * EACH)
            .map(|n| event(n, NOW - i64::try_from(n % 3600).unwrap_or(0)))
            .collect();
        fs::write(folder.join("skiff.jsonl"), lines).expect("a writer's file");
    }
}

/// The `n`th cell's murmur, said at `at`, and its tie, as two lines.
fn event(n: usize, at: i64) -> String {
    let seq = u64::try_from(n).unwrap_or(0) * 2 + 1;
    let murmur = Event {
        id: format!("skiff-{seq}"),
        writer: "skiff".to_owned(),
        seq,
        at,
        by: Some(address(n)),
        said: Said::Murmur {
            words: format!(
                "the launcher's Landed row is slow to open while {} lands its change",
                address(n)
            ),
        },
    };
    let tie = Event {
        id: format!("skiff-{}", seq + 1),
        seq: seq + 1,
        by: None,
        said: Said::Tie {
            from: murmur.id.clone(),
            to: format!("repository-{:04}#{n}", n % 300),
            how: Some("about".to_owned()),
        },
        ..murmur.clone()
    };
    [murmur, tie]
        .iter()
        .map(|event| serde_json::to_string(event).expect("an event writes") + "\n")
        .collect()
}

/// The `n`th cell's address: six letters and digits, as a session's are.
fn address(n: usize) -> String {
    const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let mut left = n + 36_usize.pow(5);
    let mut letters = Vec::new();
    while left > 0 {
        letters.push(DIGITS[left % 36]);
        left /= 36;
    }
    letters.reverse();
    String::from_utf8(letters).expect("ascii")
}
