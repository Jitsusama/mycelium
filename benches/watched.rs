//! What a watch of sessions sends when a launcher opens after a while
//! shut, at thousands of cells: how many bytes its held-back changes come
//! to, and how long the node takes to find them, said whole and asked
//! `deep` on each session's presence.
//!
//! A made-up mycelium's sessions each carry a presence sized as a live
//! one is, with its landings, what it works on, its groups, its quest and
//! what it has used. Between the two sets of records each session beats,
//! so its presence's time moves; most take a step and use more, and a
//! few stop working, as Joel's did over 90 s on 2026-11-15.
//!
//! The changes are found as each of a number of watches finds them from
//! one build, which works each moved record's change out once for all of
//! them, a few times each, and the median time for all of them said, with
//! the bytes one watch's serialize to.
//!
//! Then a list asking for a few fields of every session, as a view reading
//! only those asks: each record cut down to them from its bytes and the
//! answer serialized, the median time and the bytes it comes to.
//!
//! ```sh
//! nix develop -c cargo bench --bench watched -- --cells 1000,10000 --watchers 1,50
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use mycelium::daemon::{Build, Record, fields, record};
use serde_json::{Value, json};

/// The fields the list asks for: a few, as a view of cells reads them.
const LISTED: [&str; 5] = [
    "state",
    "step",
    "title",
    "presence.questInfo.title",
    "presence.usage.cost",
];

/// What to measure.
#[derive(Parser)]
struct Asked {
    /// How many cells each mycelium measured holds, apart by commas.
    #[arg(long, value_delimiter = ',', default_value = "1000,10000")]
    cells: Vec<usize>,
    /// How many watches find each change, apart by commas.
    #[arg(long, value_delimiter = ',', default_value = "1,50")]
    watchers: Vec<usize>,
    /// How many times the changes are found.
    #[arg(long, default_value_t = 5)]
    runs: usize,
    /// Given by `cargo bench` to every bench, and taken for nothing.
    #[arg(long, hide = true)]
    bench: bool,
}

fn main() {
    let asked = Asked::parse();
    println!("{}", load());
    println!(
        "{:>6} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "cells", "watches", "changed", "flat KB", "deep KB", "flat ms", "deep ms"
    );
    let mut lists = Vec::new();
    for cells in asked.cells {
        let (before, after) = made(cells);
        lists.push((cells, listed(&after, asked.runs)));
        for &watchers in &asked.watchers {
            let (flat, flat_took) = found(&before, &after, &[], watchers, asked.runs);
            let deep = ["presence".to_owned()];
            let (deep_said, deep_took) = found(&before, &after, &deep, watchers, asked.runs);
            println!(
                "{cells:>6} {watchers:>8} {:>8} {:>8.1} {:>8.1} {:>8.2} {:>8.2}",
                flat.0,
                kb(flat.1),
                kb(deep_said.1),
                millis(flat_took),
                millis(deep_took),
            );
        }
    }
    println!(
        "{:>6} {:>8} {:>8} {:>8}",
        "cells", "fields", "list KB", "list ms"
    );
    for (cells, (bytes, took)) in lists {
        println!(
            "{cells:>6} {:>8} {:>8.1} {:>8.2}",
            LISTED.len(),
            kb(bytes),
            millis(took)
        );
    }
}

/// The bytes a list of [`LISTED`] of every record of `records` comes to,
/// with the median time over `runs` to cut each down and serialize them.
fn listed(records: &Shared, runs: usize) -> (usize, Duration) {
    let paths = LISTED.map(str::to_owned);
    let mut took = Vec::new();
    let mut bytes = 0;
    for _ in 0..runs.max(1) {
        let listing = Instant::now();
        let kept: Vec<_> = records
            .values()
            .map(|record| fields::projected(record.as_str(), &paths))
            .collect();
        let said = serde_json::to_string(&kept).expect("records serialize");
        took.push(listing.elapsed());
        bytes = said.len();
    }
    took.sort();
    (bytes, took[took.len() / 2])
}

/// A kind's records by key, each shared, as a watch holds them.
type Shared = BTreeMap<String, Arc<Record>>;

/// How many records changed from `before` to `after` asked `deep`, and
/// the bytes they serialize to, with the median time over `runs` for
/// `watchers` watches, each holding `before`, to find them from one build.
fn found(
    before: &Shared,
    after: &Shared,
    deep: &[String],
    watchers: usize,
    runs: usize,
) -> ((usize, usize), Duration) {
    let mut took = Vec::new();
    let mut said = (0, 0);
    let held = Arc::new(before.clone());
    for _ in 0..runs.max(1) {
        let build = Build::after(after.clone(), Some(Arc::clone(&held)));
        let finding = Instant::now();
        let mut found = Vec::new();
        for _ in 0..watchers.max(1) {
            found = build.changes(before, None, deep).0;
        }
        took.push(finding.elapsed());
        let bytes = serde_json::to_vec(&found).map_or(0, |bytes| bytes.len());
        said = (found.len(), bytes);
    }
    took.sort();
    (said, took[took.len() / 2])
}

/// `cells` sessions' records, and the same after each has beaten once.
fn made(cells: usize) -> (Shared, Shared) {
    let mut random = Random(0x9e37_79b9_7f4a_7c15 ^ cells as u64);
    let now = jiff::Timestamp::now().as_millisecond();
    let mut before = Shared::new();
    let mut after = Shared::new();
    for n in 0..cells {
        let id = format!("c{n:05x}");
        let busy = random.below(3) > 0;
        let step = random.below(40);
        let used = random.below(400_000);
        let quest = random.below(cells / 4 + 1);
        let was = session(&id, now, busy, step, used, quest);
        // Every session beats; 19 of 20 take a step, 4 of 5 use more, and
        // 1 of 20 stops or starts working.
        let beat = now + 1_000 + i64::try_from(random.below(89_000)).unwrap_or(0);
        let stepped = if random.below(20) < 19 {
            step + 1
        } else {
            step
        };
        let more = if random.below(5) < 4 {
            used + random.below(9_000)
        } else {
            used
        };
        let working = if random.below(20) == 0 { !busy } else { busy };
        let is = session(&id, beat, working, stepped, more, quest);
        let key = mycelium::daemon::key("session", &id);
        before.insert(key.clone(), Arc::new(Record::from(was)));
        after.insert(key, Arc::new(Record::from(is)));
    }
    (before, after)
}

/// A session's record, its presence sized as a live one is.
fn session(id: &str, at: i64, busy: bool, step: usize, used: usize, quest: usize) -> Value {
    let when =
        jiff::Timestamp::from_millisecond(at).map_or_else(|_| String::new(), |at| at.to_string());
    let landings: Vec<Value> = (0..5)
        .map(|n| {
            json!({
                "title": format!("Land the kestrel's {n}th tide chart beside the skiff's"),
                "url": format!("https://example.org/skiff/pull/{}", 4_000 + n),
                "at": "2026-11-15T12:00:00Z",
                "state": "merged",
            })
        })
        .collect();
    let presence = json!({
        "session": id,
        "addresses": [id.to_uppercase()],
        "groups": ["#harness", format!("#quest/q{quest:05x}"), "#repo/skiff", "#repo/kestrel",
                   "#orchestration/tide", "#machine"],
        "busy": busy,
        "state": if busy { "busy" } else { "idle" },
        "step": format!("step {step}"),
        "stepDetail": format!("reading the skiff's tide tables, part {step}"),
        "at": when,
        "since": "2026-11-15T09:00:00Z",
        "title": format!("Hear the Tide in Quest {quest}"),
        "landings": landings,
        "worksOn": [
            {"repo": "skiff", "branch": format!("kestrel/tide-{quest}"), "tree": "/work/trees/skiff-tide"},
            {"repo": "kestrel", "branch": "main", "tree": "/work/trees/kestrel"},
            {"repo": "harbour", "branch": "main", "tree": "/work/trees/harbour"},
        ],
        "questInfo": {
            "id": format!("QEST-20261115-{quest:06X}"),
            "title": format!("Chart the Tide, Part {quest}"),
            "kind": "subquest",
            "priority": "active",
            "parent": "QEST-20261007-TIDE00",
            "plan": "PLAN-20261112-TIDE00",
        },
        "owns": ["skiff's tide tables", "kestrel's charts", "harbour's bells"],
        "owner": {"bootId": "0f3c-skiff-kestrel-boot", "pid": 4_242, "host": "osprey",
                  "cwd": "/home/kestrel/src/skiff"},
        "usage": {
            "input": used,
            "output": used / 8,
            "cacheRead": used * 12,
            "cacheWrite": used / 3,
            "cost": 1.25,
            "contextTokens": 120_000 + used / 10,
            "contextWindow": 200_000,
            "model": "a model",
        },
    });
    record(
        "session",
        id,
        at,
        json!({
            "title": format!("Hear the Tide in Quest {quest}"),
            "state": if busy { "busy" } else { "idle" },
            "step": format!("step {step}"),
            "presence": presence,
            "standing": "works",
        }),
    )
}

/// `took` in milliseconds.
fn millis(took: Duration) -> f64 {
    took.as_secs_f64() * 1000.0
}

/// `bytes` in kilobytes of a thousand bytes, as a figure to print.
#[expect(
    clippy::cast_precision_loss,
    reason = "a figure printed to a tenth of a kilobyte"
)]
fn kb(bytes: usize) -> f64 {
    bytes as f64 / 1e3
}

/// The machine's load as this starts, since a figure is only as quiet as
/// the machine was.
fn load() -> String {
    fs::read_to_string("/proc/loadavg").map_or_else(
        |_| "load: unknown".to_owned(),
        |load| format!("load: {}", load.trim()),
    )
}

/// Numbers that look random and come the same each run.
struct Random(u64);

impl Random {
    /// The next number, by xorshift.
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A number below `bound`.
    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % bound.max(1) as u64).expect("below a usize")
    }
}
