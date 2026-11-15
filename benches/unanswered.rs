//! How the node's view of what waits on the mesh holds up at thousands
//! of cells: how long each message takes to judge as it comes, and how
//! long the `unanswered` records take to make from what it judged, as a
//! `list` or a watch asks for them, with the memory it all held.
//!
//! A made-up mycelium's two hours of messages go into the node's own
//! state one at a time, as its mesh folders bring them: questions put to
//! a peer or two by address, holds on a group, answers to earlier
//! questions from one of the peers asked, under either address its
//! session goes by, and words to groups that ask nothing. Each cell's
//! session goes by two addresses, as a pi session does.
//!
//! The records are made as the node makes them, its sessions' addresses
//! gathered first, a few times each, and the median time said. Peak
//! memory is this bench's high water mark, read from `/proc`, so it is
//! said only on Linux.
//!
//! ```sh
//! nix develop -c cargo bench --bench unanswered -- --cells 1000,10000
//! ```

use std::collections::HashMap;
use std::fs;
use std::time::{Duration, Instant};

use clap::Parser;
use mycelium::state::State;
use serde_json::json;

/// Two hours, in milliseconds: how long a question or a hold is judged.
const TWO_HOURS: i64 = 2 * 60 * 60 * 1000;

/// What to measure.
#[derive(Parser)]
struct Asked {
    /// How many cells each mycelium measured holds, apart by commas.
    #[arg(long, value_delimiter = ',', default_value = "1000,10000")]
    cells: Vec<usize>,
    /// How many messages each cell sends in the two hours.
    #[arg(long, default_value_t = 2)]
    each: usize,
    /// How many times the records are made.
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
        "{:>6} {:>8} {:>9} {:>11} {:>8} {:>8} {:>8}",
        "cells", "messages", "add µs", "slowest µs", "open", "list ms", "peak MB"
    );
    for cells in asked.cells {
        let made = Made::of(cells, cells * asked.each);
        let mut state = State::default();
        let mut slowest = Duration::ZERO;
        let began = Instant::now();
        for message in made.messages {
            let adding = Instant::now();
            state.add_message(message);
            slowest = slowest.max(adding.elapsed());
        }
        let added = began.elapsed();
        let mut took = Vec::new();
        let mut open = 0;
        for _ in 0..asked.runs {
            let listing = Instant::now();
            let records = made.addresses.records(&state);
            took.push(listing.elapsed());
            open = records.len();
        }
        took.sort();
        println!(
            "{cells:>6} {:>8} {:>9.2} {:>11} {open:>8} {:>8.1} {:>8}",
            made.sent,
            micros(added) / f64::from(u32::try_from(made.sent.max(1)).unwrap_or(u32::MAX)),
            slowest.as_micros(),
            took[took.len() / 2].as_secs_f64() * 1000.0,
            high_water().map_or_else(|| "-".to_owned(), |peak| format!("{:.1}", mb(peak))),
        );
    }
}

/// A made-up mycelium's two hours of messages, and its sessions'
/// addresses.
struct Made {
    /// Its messages, oldest first.
    messages: Vec<mycelium::record::Message>,
    /// How many it sent.
    sent: usize,
    /// Each session's addresses.
    addresses: Addresses,
}

impl Made {
    fn of(cells: usize, sent: usize) -> Self {
        let mut random = Random(0x9e37_79b9_7f4a_7c15 ^ cells as u64);
        let addresses = Addresses(
            (0..cells)
                .map(|n| {
                    vec![
                        format!("C{n:05X}"),
                        format!("{:08X}", random.next() & 0xFFFF_FFFF),
                    ]
                })
                .collect(),
        );
        let now = jiff::Timestamp::now().as_millisecond();
        let whole = i64::try_from(sent.max(1)).unwrap_or(i64::MAX);
        let mut questions: Vec<(String, Vec<usize>)> = Vec::new();
        let mut messages = Vec::with_capacity(sent);
        for n in 0..sent {
            let at = now - TWO_HOURS + TWO_HOURS * i64::try_from(n).unwrap_or(0) / whole;
            let cell = random.below(cells);
            let mut from = addresses.0[cell][0].clone();
            let kind = random.below(100);
            let (to, intent, re): (Vec<String>, &str, Option<String>) = match kind {
                0..25 => {
                    let asked: Vec<usize> =
                        (0..=random.below(2)).map(|_| random.below(cells)).collect();
                    let to = asked
                        .iter()
                        .map(|it| format!("@{}", addresses.0[*it][0]))
                        .collect();
                    questions.push((format!("{at}-{from}-1-{n:08x}"), asked));
                    (to, "question", None)
                }
                25..30 => (vec!["#machine".to_owned()], "hold", None),
                30..70 if !questions.is_empty() => {
                    let (question, asked) = &questions[random.below(questions.len())];
                    let answering = asked[random.below(asked.len())];
                    from.clone_from(&addresses.0[answering][random.below(2)]);
                    (
                        vec![format!("#quest/q{}", cell / 40)],
                        "fyi",
                        Some(question.clone()),
                    )
                }
                _ => (vec![format!("#quest/q{}", cell / 40)], "fyi", None),
            };
            let id = format!("{at}-{from}-1-{n:08x}");
            let mut body = json!({
                "v": 1, "id": id, "from": from, "to": to, "intent": intent, "pid": 1,
                "at": jiff::Timestamp::from_millisecond(at).expect("a time").to_string(),
                "text": "a line about as long as one a session sends to another",
            });
            if let Some(re) = re {
                body["re"] = json!(re);
            }
            messages.extend(mycelium::mesh::message(&body));
        }
        Self {
            sent: messages.len(),
            messages,
            addresses,
        }
    }
}

/// Each session's addresses, as its presence says them.
struct Addresses(Vec<Vec<String>>);

impl Addresses {
    /// The `unanswered` records `state` holds now, made as the node makes
    /// them: every address each session goes by gathered first.
    fn records(&self, state: &State) -> Vec<(String, serde_json::Value)> {
        let mut aliases: HashMap<String, Vec<String>> = HashMap::new();
        for addresses in &self.0 {
            for address in addresses {
                aliases
                    .entry(address.to_ascii_uppercase())
                    .or_default()
                    .extend(addresses.iter().cloned());
            }
        }
        let now = jiff::Timestamp::now().as_millisecond();
        state
            .unanswered(
                |address| {
                    aliases
                        .get(&address.to_ascii_uppercase())
                        .cloned()
                        .unwrap_or_default()
                },
                now,
            )
            .iter()
            .map(mycelium::unanswered::Open::fields)
            .collect()
    }
}

/// `took` in microseconds.
fn micros(took: Duration) -> f64 {
    took.as_secs_f64() * 1e6
}

/// `bytes` in megabytes of a million bytes, as a figure to print.
#[expect(
    clippy::cast_precision_loss,
    reason = "a figure printed to a tenth of a megabyte"
)]
fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1e6
}

/// The most memory this bench has held, in bytes, as Linux says.
fn high_water() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kilobytes: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kilobytes * 1024)
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
