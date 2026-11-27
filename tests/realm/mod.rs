//! A realm of ten thousand cells as the node holds its records, and the
//! bar what the node makes of them is held to: within the memory each
//! test names, and in time that grows with the cells rather than faster.
//! Its networks are held within the 30 MB the map and the launcher hold
//! them to, each cell holding two trees, and three in ten saying
//! subjects, from one to as many as a cell may: more than four times a
//! live realm's on the whole, each a little longer than a live cell's.
//! Its cells, a record each held as the bytes it is sent as, are held
//! within 30 MB too. The time is the processor's own for this thread,
//! not the clock's, so a builder busy with other work, which only makes
//! this thread wait its turn, can't fail it. The time itself, 100 ms on
//! the machine the bar is set for, is no builder's to hold, since
//! builders run it several times slower, so each test says it, and holds
//! it only to a ceiling a builder meets, where the test names one:
//!
//! ```sh
//! nix develop -c cargo test --release --test networks --test cells -- --nocapture
//! ```
//!
//! The memory is counted by an allocator of the test's own, which only a
//! program of its own may hold, so each test using this is a test of its
//! own.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

use mycelium::daemon::Record;
use mycelium::node::{Formed, Holding};
use mycelium::record::Session;
use serde_json::{Value, json};

use counting::{HELD, PEAK, thread_time};

/// How many cells the bar is held at.
pub const CELLS: usize = 10_000;

/// How the time to make the records may grow: from `from` cells to
/// [`CELLS`], at most `times` as long, and at [`CELLS`] within `within`
/// of the processor's time where it is named, so a slow constant can't
/// hide behind a good ratio.
pub struct Growth {
    /// How many cells the time is first taken at.
    pub from: usize,
    /// How many times longer [`CELLS`] may take.
    pub times: u32,
    /// The most [`CELLS`] may take, if any.
    pub within: Option<Duration>,
}

/// How many times each build is timed at least, the sizes in turn so a
/// busy moment on the builder slows each alike, and the fastest of each
/// taken.
const TIMINGS: usize = 5;

/// How many times each build is timed at most, while the growth isn't yet
/// within its bar. A machine as loaded as a Mac at a load of 45 slows the
/// cells past any cache more than those within it, so more turns find it
/// a quieter moment for each (04TNWT, 2026-11-16: 3.2 times against 3).
const MOST_TIMINGS: usize = 25;

/// The system's allocator, counting what is held and the most held,
/// which a test measuring more than one making reads too.
pub mod counting;

/// That the records `making` makes of ten thousand cells hold to the bar,
/// more than `more_than` of them, as `formed` says they should be, the
/// build holding at most `at_most` bytes beyond what it was given and
/// taking as long as `growth` lets it. Each cell's time at a tenth of the
/// cells is told beside it, held or not, so a step at the cache's edge
/// shows.
pub fn holds_the_bar(
    more_than: usize,
    formed: &str,
    at_most: usize,
    growth: &Growth,
    making: impl Fn(&Holding<'_>) -> BTreeMap<String, Record>,
) {
    let realm = Realm::of(CELLS);
    let before = HELD.load(Ordering::Relaxed);
    PEAK.store(before, Ordering::Relaxed);
    let records = making(&realm.holding());
    let peak = PEAK.load(Ordering::Relaxed) - before;
    let kept = HELD.load(Ordering::Relaxed) - before;
    drop(records);

    let from = growth.from;
    let tenth = CELLS / 10;
    let [least, fewer, all] = fastest(
        [&Realm::of(tenth), &Realm::of(from), &realm],
        &making,
        |[_, fewer, all]| *all < *fewer * growth.times,
    );
    eprintln!(
        "{tenth} cells took {least:?}; {from} took {fewer:?}; {CELLS} took {all:?}, holding \
         {peak} bytes at most, {kept} of them the records themselves"
    );
    eprintln!(
        "each cell took {} at {tenth}, {} at {from} and {} at {CELLS}: {CELLS} took {:.1} \
         times as long as {tenth}, which is told, and {:.1} times as long as {from}, which \
         is held",
        per_cell(least, tenth),
        per_cell(fewer, from),
        per_cell(all, CELLS),
        all.as_secs_f64() / least.as_secs_f64(),
        all.as_secs_f64() / fewer.as_secs_f64(),
    );

    let records = making(&realm.holding());
    assert!(
        records.len() > more_than,
        "{formed}, but {} did",
        records.len()
    );
    assert!(
        peak < at_most,
        "the records of {CELLS} cells held {peak} bytes at most, past {at_most}"
    );
    assert!(
        all < fewer * growth.times,
        "the records of {CELLS} cells took {all:?}, past {} times the {fewer:?} of {from}",
        growth.times
    );
    if let Some(within) = growth.within {
        assert!(
            all < within,
            "the records of {CELLS} cells took {all:?}, past {within:?}"
        );
    } else {
        eprintln!("{all:?} for {CELLS} cells is held by growth alone, as a debug build");
    }
}

/// A realm of cells, as the node holds its records.
struct Realm {
    teams: BTreeMap<String, Value>,
    sessions: Vec<Session>,
    machines: BTreeMap<String, Value>,
    receptors: BTreeMap<String, Value>,
    builders: BTreeMap<String, Value>,
    /// The tree networks formed, kept in a folder of its own.
    formed: Formed,
    _keeps: tempfile::TempDir,
}

impl Realm {
    /// A realm of `cells` sessions, with a team for every forty and a
    /// machine for every twenty-five, a receptor sensing Slack, and a
    /// builder.
    fn of(cells: usize) -> Self {
        let keeps = tempfile::tempdir().expect("a folder");
        Self {
            teams: teams(cells),
            sessions: sessions(cells),
            machines: machines(cells),
            receptors: receptors(),
            builders: builders(),
            formed: Formed::at(keeps.path().join("formed.json")),
            _keeps: keeps,
        }
    }

    /// What the node holds of it.
    fn holding(&self) -> Holding<'_> {
        Holding {
            machine: "",
            teams: &self.teams,
            sessions: &self.sessions,
            machines: &self.machines,
            receptors: &self.receptors,
            builders: &self.builders,
            formed: &self.formed,
        }
    }

    /// How long the processor took making the records `making` makes of
    /// it, on this thread.
    fn timed(&self, making: &impl Fn(&Holding<'_>) -> BTreeMap<String, Record>) -> Duration {
        let holding = self.holding();
        let begun = thread_time();
        drop(making(&holding));
        thread_time().saturating_sub(begun)
    }
}

/// The least time the records `making` makes of each of `realms` took,
/// of [`TIMINGS`] builds each, made in turn, and more up to
/// [`MOST_TIMINGS`] until the least times are `within` their bar.
fn fastest<const N: usize>(
    realms: [&Realm; N],
    making: &impl Fn(&Holding<'_>) -> BTreeMap<String, Record>,
    within: impl Fn(&[Duration; N]) -> bool,
) -> [Duration; N] {
    let mut least = [Duration::MAX; N];
    for turn in 0..MOST_TIMINGS {
        if turn >= TIMINGS && within(&least) {
            break;
        }
        for (fastest, realm) in least.iter_mut().zip(realms) {
            *fastest = (*fastest).min(realm.timed(making));
        }
    }
    least
}

/// The sessions of a realm of `cells`, as [`holds_the_bar`] builds its
/// records from, for a test that makes them otherwise too.
pub fn sessions(cells: usize) -> Vec<Session> {
    (0..cells).map(|n| session(n, cells)).collect()
}

/// `took` for `cells`, said for one of them.
fn per_cell(took: Duration, cells: usize) -> String {
    let cells = u32::try_from(cells).unwrap_or(u32::MAX).max(1);
    format!("{:?}", took / cells)
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

/// A realm's receptors: one, sensing Slack on the first machine, as the
/// node holds its own.
fn receptors() -> BTreeMap<String, Value> {
    let key = "receptor:slack".to_owned();
    let receptor = json!({
        "key": key, "kind": "receptor", "heard": 1_794_694_471_264_u64,
        "source": "slack", "realm": "work", "state": "reading",
        "since": "2026-11-14T21:00:00Z", "read_at": "2026-11-14T22:14:31.264Z",
        "every": 300, "pid": 4242, "machine": "machine-0000",
        "counts": {"hour": 2, "today": 5, "addressed": 1},
    });
    BTreeMap::from([(key, receptor)])
}

/// A realm's builders: one, which no cell is.
fn builders() -> BTreeMap<String, Value> {
    let key = "builder:builder-0000".to_owned();
    let builder = json!({
        "key": key, "kind": "builder", "heard": 1_794_694_471_264_u64,
        "name": "builder-0000", "state": "ready", "systems": ["x86_64-linux"],
        "jobs": 8, "lines": ["standing"], "lent": false,
    });
    BTreeMap::from([(key, builder)])
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

/// The subjects the `n`th cell says, of 27 and 34 characters, where a
/// live cell's are 28 on the whole: a third of them areas of a
/// repository a third of the cells share, the rest areas of its own.
/// Three cells in ten say any, from one to as many as a cell may, about
/// 12.3 on the whole, so a cell says about 3.7 where a live one says
/// 0.84: on 26 Nov, 28 of 191 sessions said 161, from 1 to 12 each.
fn subjects(n: usize, repositories: usize) -> Value {
    let shared = n % 3;
    let own = n % repositories;
    let says = if n % 10 < 3 {
        1 + n * 7 % mycelium::networks::MOST_SUBJECTS
    } else {
        0
    };
    let said: Vec<String> = (0..says)
        .map(|nth| match nth % 3 {
            0 => format!("repository-{shared:04} lib/area-{nth:02}"),
            _ => format!("repository-{own:04} src/n{n:05}/area-{nth:02}"),
        })
        .collect();
    json!(said)
}

/// The `n`th session of a realm of `cells`, about the size of a live one:
/// an orchestration's member, sharing a project with a few others, in a
/// few repositories some cells share many of, and now and then in a named
/// group, holding a tree its project's cells share and one of its own.
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
    let shared = format!("repository-{:04}", n / 5 % repositories);
    let own = format!("repository-{:04}", n % repositories);
    let trees = json!([
        {"key": format!("worktree-github-owner-{shared}-someone-p{:05}", n / 5),
         "repo": format!("github:owner/{shared}"),
         "branch": format!("someone/p{:05}", n / 5)},
        {"key": format!("snapshot-github-owner-{own}-{n:012x}"),
         "repo": format!("github:owner/{own}"),
         "commit": format!("{n:040x}")},
    ]);
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
        "subjects": subjects(n, repositories),
        "title": format!("pi - {address} - the work at hand"),
        "trees": trees,
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
