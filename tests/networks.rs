//! The node's network records built for ten thousand cells, within the
//! memory the map and the launcher hold it to, 30 MB, and in time that
//! grows with the cells rather than faster. The time itself, 100 ms on the
//! machine the bar is set for, is no builder's to hold, since builders run
//! it several times slower, so the test says it rather than holds it:
//!
//! ```sh
//! nix develop -c cargo test --release --test networks -- --nocapture
//! ```
//!
//! The memory is counted by an allocator of this test's own, which only a
//! program of its own may hold, so this is a test of its own.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use mycelium::node;
use mycelium::record::Session;
use serde_json::{Value, json};

use counting::{HELD, PEAK};

/// How many cells the bar is held at.
const CELLS: usize = 10_000;

/// How many cells the time is first taken at, to see how it grows.
const FEWER: usize = CELLS / 10;

/// How many times longer ten times the cells may take. Work that grows
/// with them takes about ten times as long; work that grows with their
/// square takes more once it is a third of the build at ten thousand, as
/// a scan of every session for each repository once was.
#[cfg(not(target_env = "musl"))]
const GROWS_AT_MOST: u32 = 15;

/// How many times longer ten times the cells may take under musl, whose
/// allocator slows as the heap grows: up to about thirty on a busy
/// builder for work that grows with the cells, so only work that grows
/// with their square and is most of the build, about a hundred, is told.
#[cfg(target_env = "musl")]
const GROWS_AT_MOST: u32 = 50;

/// How many times each build is timed, the two sizes in turn so a busy
/// moment on the builder slows both alike, and the fastest of each taken.
const TIMINGS: usize = 5;

/// The most memory the build may hold beyond what it was given, in bytes.
const AT_MOST: usize = 30_000_000;

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
fn ten_thousand_cells_make_their_network_records_within_the_bar() {
    let realm = Realm::of(CELLS);
    let before = HELD.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let records = realm.records();
    let peak = PEAK.load(Ordering::Relaxed) - before;
    drop(records);

    let (fewer, all) = fastest(&Realm::of(FEWER), &realm);
    eprintln!("{FEWER} cells took {fewer:?}; {CELLS} took {all:?}, holding {peak} bytes at most");

    let records = realm.records();
    assert!(
        records.len() > CELLS / 10,
        "about one network forms per ten cells, but {} did",
        records.len()
    );
    assert!(
        peak < AT_MOST,
        "the records of {CELLS} cells held {peak} bytes at most, past {AT_MOST}"
    );
    assert!(
        all < fewer * GROWS_AT_MOST,
        "the records of {CELLS} cells took {all:?}, past {GROWS_AT_MOST} times \
         the {fewer:?} of {FEWER}"
    );
}

/// A realm of cells, as the node holds its records.
struct Realm {
    teams: BTreeMap<String, Value>,
    sessions: Vec<Session>,
    machines: BTreeMap<String, Value>,
}

impl Realm {
    /// A realm of `cells` sessions, with a team for every forty and a
    /// machine for every twenty-five.
    fn of(cells: usize) -> Self {
        Self {
            teams: teams(cells),
            sessions: (0..cells).map(|n| session(n, cells)).collect(),
            machines: machines(cells),
        }
    }

    /// Its network records, as the node makes them.
    fn records(&self) -> BTreeMap<String, Value> {
        node::network_records(
            &self.teams,
            &self.sessions,
            |_| None,
            &self.machines,
            &BTreeMap::new(),
        )
    }

    /// How long its records took to make.
    fn timed(&self) -> Duration {
        let begun = Instant::now();
        drop(self.records());
        begun.elapsed()
    }
}

/// The least time the records of `fewer` and of `all` took, of a few
/// builds each, made in turn.
fn fastest(fewer: &Realm, all: &Realm) -> (Duration, Duration) {
    (0..TIMINGS).fold((Duration::MAX, Duration::MAX), |(least, most), _| {
        (least.min(fewer.timed()), most.min(all.timed()))
    })
}

/// The teams of a realm of `cells`: one for every forty, some within
/// another's.
fn teams(cells: usize) -> BTreeMap<String, Value> {
    (0..cells / 40)
        .map(|n| {
            let leads = address(n * 40);
            let key = format!("team:{leads}");
            let team = json!({
                "key": key, "kind": "team", "heard": 1_794_694_453_319_u64,
                "lead": id(n * 40), "title": format!("The Work {leads} Leads"),
                "within": (!n.is_multiple_of(5)).then(|| address(n / 5 * 200)),
                "counts": {"moving": 2, "needs-you": 0, "quiet": 3, "stuck": 0},
            });
            (key, team)
        })
        .collect()
}

/// The machines of a realm of `cells`: one for every twenty-five.
fn machines(cells: usize) -> BTreeMap<String, Value> {
    (0..cells / 25)
        .map(|n| {
            let key = format!("machine:machine-{n:04}");
            let machine = json!({
                "key": key, "kind": "machine", "heard": 1_794_694_471_264_u64,
                "name": format!("machine-{n:04}"), "state": "ready",
                "model_name": "A Workstation", "room": {"pressure": {"cpu": 12.5}},
            });
            (key, machine)
        })
        .collect()
}

/// The `n`th session of a realm of `cells`, about the size of a live one:
/// an orchestration's member, sharing a project with a few others, in a
/// few repositories some cells share many of, and now and then in a named
/// group.
fn session(n: usize, cells: usize) -> Session {
    let orchestration = address(n % (cells / 40) * 40).to_lowercase();
    let repositories = cells * 3 / 100;
    let address = address(n);
    let mut groups = vec![
        format!("orchestration.{orchestration}"),
        format!("quest.p{:05}", n / 5),
        format!("repo.repository-{:04}", n % repositories),
        format!("repo.repository-{:04}", n % 3),
        format!("peer.{}", address.to_lowercase()),
    ];
    if n.is_multiple_of(10) {
        groups.push(format!("group-{:03}", n % 50));
    }
    let id = id(n);
    let quest = format!("QEST-20261114-P{:05}", n / 5);
    let title = format!("A Quest Worked On by {address} and Others Who Share It");
    let landings = json!([
        {"at": "2026-11-14T14:49:29.591Z", "ref": "world#2448790",
         "url": "https://example.com/world/pull/2448790",
         "what": "Give the Nightly Repack Its Own Gate"},
        {"at": "2026-11-13T23:29:29.884Z", "ref": "world#2445775",
         "url": "https://example.com/world/pull/2445775",
         "person": "Nothing to try: a stalled fetch now gives up within half an hour",
         "what": "Give Up a Stalled Prefetch After Ten Minutes"},
    ]);
    let hashed: Vec<String> = groups.iter().map(|group| format!("#{group}")).collect();
    let presence = json!({
        "addresses": [address, &id[id.len() - 8..]],
        "at": "2026-11-14T22:14:25.782Z",
        "busy": n.is_multiple_of(3),
        "cwd": format!("/home/someone/src/repository/trees/{}", address.to_lowercase()),
        "groups": hashed,
        "landings": landings,
        "owner": {"bootId": "925e3151-b224-4649-b4d2-d837ac4195dc", "pid": 2_350_329,
                  "startTicks": 8_523_576, "startedAt": 1_794_690_558_760_u64},
        "owns": [quest, "repository"],
        "paused": n.is_multiple_of(50),
        "questInfo": {"id": quest, "short": format!("P{:05}", n / 5), "status": "active",
                      "title": title},
        "repo": "repository",
        "session": id,
        "state": "listening",
        "title": format!("pi - {address} - the work at hand"),
        "usage": {
            "context": {"limit": 1_000_000, "used": n * 37 % 900_000},
            "day": "2026-11-14",
            "gen_ai.provider.name": "anthropic",
            "gen_ai.request.model": "claude-opus-5-5",
            "last": {"gen_ai.usage.cache_read.input_tokens": 413_222,
                     "gen_ai.usage.input_tokens": 2, "gen_ai.usage.output_tokens": 2210},
            "pi.cost.usd": {"last": 0.039_42, "today": 15.599_615_4},
            "today": {"gen_ai.usage.cache_read.input_tokens": 41_322_118,
                      "gen_ai.usage.input_tokens": 801, "gen_ai.usage.output_tokens": 231_002},
            "turns": {"last": {"at": "2026-11-14T21:13:38.913Z", "duration_ms": 94479},
                      "today": n % 400},
        },
        "worksOn": title,
    });
    serde_json::from_value(json!({
        "id": id,
        "address": address,
        "machine": format!("machine-{:04}", n % (cells / 25)),
        "title": format!("pi - {address} - the work at hand"),
        "groups": groups,
        "state": if n.is_multiple_of(3) { "busy" } else { "idle" },
        "step": "reading the code",
        "quest": quest,
        "landings": [
            {"at": "2026-11-14T14:49:29.591Z", "ref": "world#2448790",
             "what": "Give the Nightly Repack Its Own Gate"},
        ],
        "at": "2026-11-14T22:14:25.782Z",
        "presence": presence,
    }))
    .expect("a session reads as one")
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

/// The `n`th cell's id, a UUID version 7 for a second of 14 November 2026.
fn id(n: usize) -> String {
    let millis = 1_794_614_400_000_u64 + n as u64 * 1000;
    let hex = format!("{millis:012x}7000800000000000{n:04x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}
