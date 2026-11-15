//! How `mycelium networks` and `mycelium cells` hold up as the mycelium
//! grows to thousands of cells: each view's wall time and peak memory,
//! the whole command timed, the node's answer read and parsed included.
//!
//! A stand-in node serves a made-up mycelium on a socket of its own in a
//! temporary folder, speaking the daemon interface as the node does: a
//! hello, then the teams, sessions and machines each `list` asks for,
//! only the fields the list names where it names them, as a node does,
//! or every field, as a node before `fields` did, with `--older`.
//! Its records are shaped and sized as live ones are, about 4 KB a
//! session, since the bytes the node sends are most of the work. About
//! one network forms per ten cells, across every shape: orchestrations,
//! projects shared by a few cells, repositories some cells share many of,
//! named groups, and the machines. Most cells are quiet or moving, a few
//! need you or are stuck, and some sleep.
//!
//! Each view runs as the built `mycelium` would for a person, its output
//! into a pipe this drains, a few times each, and says the median time
//! and the most memory any run held. Peak memory is the process's high
//! water mark, read from `/proc` every millisecond while it runs, so it
//! is said only on Linux, and growth in a run's last millisecond is
//! missed.
//!
//! ```sh
//! nix develop -c cargo bench --bench many -- --cells 1000,3000,10000
//! ```

use std::error::Error;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use std::{env, fs};

use clap::Parser;
use mycelium::daemon::fields;
use serde_json::{Value, json};

/// How often a running view's memory is read.
const SAMPLED_EVERY: Duration = Duration::from_millis(1);

/// The day every made-up session began on, in milliseconds since the
/// epoch: 14 November 2026.
const BEGAN: u64 = 1_794_614_400_000;

/// Cells to a machine.
const CELLS_A_MACHINE: usize = 25;

/// Cells to an orchestration.
const CELLS_AN_ORCHESTRATION: usize = 40;

/// Cells to a shared project.
const CELLS_A_PROJECT: usize = 5;

/// Cells to a repository, though a few hold far more.
const CELLS_A_REPOSITORY: usize = 30;

/// Cells to a named group.
const CELLS_A_GROUP: usize = 200;

/// What to measure.
#[derive(Parser)]
struct Asked {
    /// How many cells each mycelium measured holds, apart by commas.
    #[arg(long, value_delimiter = ',', default_value = "1000,3000,10000")]
    cells: Vec<usize>,
    /// How many times each view runs.
    #[arg(long, default_value_t = 5)]
    runs: usize,
    /// The `mycelium` to run, if not the one built beside this.
    #[arg(long)]
    mycelium: Option<PathBuf>,
    /// Send every field whatever a list names, as a node before `fields`.
    #[arg(long)]
    older: bool,
    /// Given by `cargo bench` to every bench, and taken for nothing.
    #[arg(long, hide = true)]
    bench: bool,
}

/// What one view measured, over its runs.
struct Measured {
    /// The median run's wall time.
    took: Duration,
    /// The most memory any run held, in bytes, where it could be read.
    peak: Option<u64>,
    /// How much it printed.
    printed: usize,
}

fn main() -> Result<(), Box<dyn Error>> {
    let asked = Asked::parse();
    let mycelium = asked
        .mycelium
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_mycelium")));
    let folder = tempfile::tempdir()?;
    println!("{}", load());
    println!(
        "{:>6} {:>5} {:>6}  {:<22} {:>9} {:>8} {:>9}",
        "cells", "nets", "whole", "view", "ms", "peak MB", "printed"
    );
    for cells in asked.cells {
        let made = Made::of(cells);
        let socket = folder.path().join(format!("{cells}.sock"));
        let whole = made.serve(&socket, asked.older)?;
        let network = made.busiest.clone();
        let cell = made.addresses[cells / 2].clone();
        let views: [(&str, Vec<&str>); 7] = [
            ("networks", vec!["networks"]),
            ("networks NAME", vec!["networks", &network]),
            ("networks --cells", vec!["networks", "--cells"]),
            ("cells", vec!["cells"]),
            ("cells NAME", vec!["cells", &cell]),
            ("networks -o json", vec!["networks", "-o", "json"]),
            ("cells -o json", vec!["cells", "-o", "json"]),
        ];
        for (view, words) in views {
            let measured = measure(&mycelium, &socket, &words, asked.runs)?;
            println!(
                "{cells:>6} {:>5} {:>5}M  {view:<22} {:>9.1} {:>8} {:>9}",
                made.networks,
                whole / 1_000_000,
                measured.took.as_secs_f64() * 1000.0,
                measured
                    .peak
                    .map_or_else(|| "-".to_owned(), |peak| format!("{:.1}", mb(peak))),
                measured.printed,
            );
        }
    }
    Ok(())
}

/// `bytes` in megabytes of a million bytes, as a figure to print.
#[expect(
    clippy::cast_precision_loss,
    reason = "a figure printed to a tenth of a megabyte"
)]
fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1e6
}

/// The machine's load as this starts, since a figure is only as quiet as
/// the machine was.
fn load() -> String {
    fs::read_to_string("/proc/loadavg").map_or_else(
        |_| "load: unknown".to_owned(),
        |load| format!("load: {}", load.trim()),
    )
}

/// Runs `mycelium` with `words` against the node at `socket`, `runs`
/// times.
fn measure(mycelium: &Path, socket: &Path, words: &[&str], runs: usize) -> io::Result<Measured> {
    let mut took = Vec::new();
    let mut peak = None;
    let mut printed = 0;
    for _ in 0..runs.max(1) {
        let begun = Instant::now();
        let mut child = Command::new(mycelium)
            .arg("--socket")
            .arg(socket)
            .args(["--realm", "bench"])
            .args(words)
            .env_remove("MYCELIUM_REALM")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdout = child.stdout.take().expect("its output is piped");
        let drained = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes.len())
        });
        let pid = child.id();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if let Some(now) = high_water(pid) {
                peak = peak.max(Some(now));
            }
            thread::sleep(SAMPLED_EVERY);
        };
        took.push(begun.elapsed());
        printed = drained.join().expect("the drain doesn't panic")?;
        if !status.success() {
            let mut why = String::new();
            if let Some(mut stderr) = child.stderr.take() {
                stderr.read_to_string(&mut why)?;
            }
            return Err(io::Error::other(format!(
                "mycelium {} failed: {why}",
                words.join(" ")
            )));
        }
    }
    took.sort();
    Ok(Measured {
        took: took[took.len() / 2],
        peak,
        printed,
    })
}

/// The most memory process `pid` has held, in bytes, as Linux says; none
/// while it is still this bench, forked and not yet running what it was
/// spawned to run, since until then it reports this bench's own memory.
fn high_water(pid: u32) -> Option<u64> {
    let running = fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    if env::current_exe().is_ok_and(|bench| bench == running) {
        return None;
    }
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kilobytes: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kilobytes * 1024)
}

/// A made-up mycelium's records, as the node would send them.
struct Made {
    /// Each kind's records, by the kind `list` asks for.
    records: Vec<(&'static str, Vec<Value>)>,
    /// Every cell's address, in the order made.
    addresses: Vec<String>,
    /// The network with the most cells among the orchestrations.
    busiest: String,
    /// About how many networks form.
    networks: usize,
}

impl Made {
    /// A mycelium of `cells` session cells.
    fn of(cells: usize) -> Self {
        let mut random = Random(0x9e37_79b9_7f4a_7c15 ^ cells as u64);
        let machines: Vec<String> = (0..cells.div_ceil(CELLS_A_MACHINE))
            .map(|n| format!("machine-{n:04}"))
            .collect();
        let orchestrations = cells.div_ceil(CELLS_AN_ORCHESTRATION);
        let projects = cells / 2 / CELLS_A_PROJECT;
        let repositories = cells.div_ceil(CELLS_A_REPOSITORY);
        let groups = cells.div_ceil(CELLS_A_GROUP);
        let addresses: Vec<String> = (0..cells).map(address).collect();
        let ids: Vec<String> = (0..cells).map(|n| id(n, &mut random)).collect();
        let sessions: Vec<Value> = (0..cells)
            .map(|n| {
                let orchestration =
                    &addresses[(n % orchestrations) * CELLS_AN_ORCHESTRATION % cells.max(1)];
                let mut groups_of = vec![format!("orchestration.{}", orchestration.to_lowercase())];
                // Half the cells share a project with a few others; the
                // rest work on one of their own, which is no network.
                let quest = if n % 2 == 0 && projects > 0 {
                    format!("P{:05}", n / 2 % projects)
                } else {
                    addresses[n].clone()
                };
                groups_of.push(format!(
                    "quest.{}.{}",
                    orchestration.to_lowercase(),
                    quest.to_lowercase()
                ));
                // A few repositories hold most cells, as World does.
                let held = 1 + random.below(3);
                for _ in 0..held {
                    let r = random.below(repositories);
                    let r = if random.below(4) == 0 { r % 3 } else { r };
                    groups_of.push(format!("repo.repository-{r:04}"));
                }
                if random.below(10) == 0 {
                    groups_of.push(format!("group-{:03}", random.below(groups)));
                }
                groups_of.push(format!("peer.{}", addresses[n].to_lowercase()));
                groups_of.sort();
                groups_of.dedup();
                session(
                    &addresses[n],
                    &ids[n],
                    &machines[n % machines.len()],
                    &quest,
                    &groups_of,
                    &mut random,
                )
            })
            .collect();
        let teams: Vec<Value> = (0..orchestrations)
            .map(|t| {
                let lead = t * CELLS_AN_ORCHESTRATION % cells.max(1);
                let members: Vec<&String> = (0..cells)
                    .filter(|n| n % orchestrations == t)
                    .map(|n| &ids[n])
                    .collect();
                let within = (t % 5 != 0).then(|| {
                    addresses[(t / 5 * 5) * CELLS_AN_ORCHESTRATION % cells.max(1)].clone()
                });
                team(&addresses[lead], &ids[lead], &members, within.as_deref())
            })
            .collect();
        let machines: Vec<Value> = machines
            .iter()
            .map(|name| machine(name, &mut random))
            .collect();
        let busiest = format!("orchestration.{}", addresses[0].to_lowercase());
        Self {
            records: vec![
                ("team", teams),
                ("session", sessions),
                ("machine", machines),
            ],
            addresses,
            busiest,
            networks: orchestrations + projects + repositories + groups + 1,
        }
    }

    /// Serves these records on `socket` for as long as this runs, every
    /// field of them if `older`, and says how many bytes the three lists
    /// send whole.
    fn serve(&self, socket: &Path, older: bool) -> io::Result<usize> {
        let listener = UnixListener::bind(socket)?;
        let serving = Arc::new(Serving {
            records: self.records.clone(),
            older,
            sent: Mutex::default(),
        });
        let whole = serving.list("team", None).len()
            + serving.list("session", None).len()
            + serving.list("machine", None).len();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let serving = Arc::clone(&serving);
                thread::spawn(move || answer(stream, &serving));
            }
        });
        Ok(whole)
    }
}

/// What a stand-in node serves: its records, and each list it has sent,
/// kept serialized so the views measured pay for reading an answer, not
/// for this making it.
struct Serving {
    /// Each kind's records.
    records: Vec<(&'static str, Vec<Value>)>,
    /// Whether a list is sent whole whatever fields it names.
    older: bool,
    /// Each list sent, by its kind and the fields it named.
    sent: Mutex<Vec<(Listed, Arc<String>)>>,
}

/// A list as asked: its kind, and the fields it named, if any.
type Listed = (String, Option<Vec<String>>);

impl Serving {
    /// The records of `kind`, of only `paths` if named, as a JSON array.
    fn list(&self, kind: &str, paths: Option<Vec<String>>) -> Arc<String> {
        let paths = paths.filter(|_| !self.older);
        let asked = (kind.to_owned(), paths);
        let mut sent = self.sent.lock().expect("no list panics");
        if let Some((_, json)) = sent.iter().find(|(each, _)| *each == asked) {
            return Arc::clone(json);
        }
        let records = self.records.iter().find(|(each, _)| *each == kind);
        let records = records
            .map(|(_, records)| records.clone())
            .unwrap_or_default();
        let records: Vec<Value> = match &asked.1 {
            Some(paths) => records
                .into_iter()
                .map(|record| fields::projected(record, paths))
                .collect(),
            None => records,
        };
        let json = Arc::new(Value::from(records).to_string());
        sent.push((asked, Arc::clone(&json)));
        json
    }
}

/// Answers each line asked on `stream`, as the node does: a hello, and
/// each kind's records.
fn answer(stream: UnixStream, serving: &Serving) -> io::Result<()> {
    let mut writing = stream.try_clone()?;
    for line in BufReader::new(stream).lines() {
        let asked: Value = serde_json::from_str(&line?).map_err(io::Error::other)?;
        let id = &asked["id"];
        if asked["ask"] == "list" {
            let kind = asked["kind"].as_str().unwrap_or_default();
            let paths = asked.get("fields").map(fields::read).transpose();
            let json = serving.list(kind, paths.map_err(io::Error::other)?);
            writeln!(writing, "{{\"id\":{id},\"records\":{json}}}")?;
        } else {
            writeln!(writing, "{}", json!({"id": id, "hello": {"version": 1}}))?;
        }
    }
    Ok(())
}

/// A session record as the node keeps one, about the size of a live one.
fn session(
    address: &str,
    id: &str,
    machine: &str,
    quest: &str,
    groups: &[String],
    random: &mut Random,
) -> Value {
    let (state, standing, paused, wants) = match random.below(100) {
        0..2 => (
            "idle",
            "idle",
            false,
            json!({"said": "Say whether to merge"}),
        ),
        2..3 => ("idle", "idle", true, Value::Null),
        3..33 => ("busy", "busy", false, Value::Null),
        33..83 => ("idle", "idle", false, Value::Null),
        _ => ("idle", "parked", false, Value::Null),
    };
    let quest_id = format!("QEST-20261114-{quest}");
    let title = format!("A Quest Worked On by {address} and Others Who Share It");
    let landings = json!([
        {"at": "2026-11-14T14:49:29.591Z", "ref": "world#2448790",
         "url": "https://example.com/world/pull/2448790",
         "what": "Give the Nightly Repack Its Own Gate"},
        {"at": "2026-11-13T23:29:29.884Z", "ref": "world#2445775",
         "url": "https://example.com/world/pull/2445775",
         "person": "Nothing to try: a stalled fetch now gives up within half an hour and tries again, rather than hanging",
         "what": "Give Up a Stalled Prefetch After Ten Minutes"},
    ]);
    let hashed: Vec<String> = groups.iter().map(|group| format!("#{group}")).collect();
    let usage = json!({
        "context": {"limit": 1_000_000, "used": random.below(900_000)},
        "day": "2026-11-14",
        "gen_ai.provider.name": "anthropic",
        "gen_ai.request.model": "claude-opus-5-5",
        "gen_ai.response.model": "claude-opus-5-5",
        "last": {
            "gen_ai.usage.cache_creation.input_tokens": random.below(5000),
            "gen_ai.usage.cache_read.input_tokens": random.below(500_000),
            "gen_ai.usage.input_tokens": 2,
            "gen_ai.usage.output_tokens": random.below(3000),
        },
        "pi.cost.usd": {"last": 0.039_42, "today": 15.599_615_399_999_98},
        "today": {
            "gen_ai.usage.cache_creation.input_tokens": random.below(2_000_000),
            "gen_ai.usage.cache_read.input_tokens": random.below(50_000_000),
            "gen_ai.usage.input_tokens": random.below(1000),
            "gen_ai.usage.output_tokens": random.below(300_000),
        },
        "turns": {
            "last": {"at": "2026-11-14T21:13:38.913Z", "duration_ms": 94479, "first_token_ms": 94437},
            "today": random.below(400),
        },
    });
    json!({
        "address": address,
        "at": "2026-11-14T22:14:25.782Z",
        "attach": ["mycelium", "--realm", "bench", "attach", &id[..8]],
        "groups": groups,
        "heard": 1_794_694_465_782_u64,
        "held": &id[..8],
        "here": true,
        "id": id,
        "key": format!("session:{id}"),
        "kind": "session",
        "landings": landings,
        "machine": machine,
        "presence": {
            "addresses": [address, &id[id.len() - 8..]],
            "at": "2026-11-14T22:14:25.782Z",
            "busy": state == "busy",
            "cwd": format!("/home/someone/src/repository/trees/{}", address.to_lowercase()),
            "driven": true,
            "groups": hashed,
            "joined": [],
            "landings": landings,
            "owner": {"bootId": "925e3151-b224-4649-b4d2-d837ac4195dc", "pid": 2_350_329, "startTicks": 8_523_576, "startedAt": 1_794_690_558_760_u64},
            "owns": [quest_id, "repository"],
            "paused": paused,
            "questInfo": {"id": quest_id, "short": quest, "status": "active", "title": title},
            "repo": "repository",
            "session": id,
            "state": if standing == "parked" { "parked" } else { state },
            "title": format!("pi - {address} - the work at hand"),
            "usage": usage,
            "worksOn": title,
        },
        "quest": quest_id,
        "standing": standing,
        "state": state,
        "step": "reading the code",
        "title": format!("pi - {address} - the work at hand"),
        "wake": format!("/run/user/1000/mycelium/bench.held/{}.wake", &id[..8]),
        "wants": wants,
        "usage": usage,
    })
}

/// A team record, led by the session `lead` of `address`.
fn team(address: &str, lead: &str, members: &[&String], within: Option<&str>) -> Value {
    json!({
        "bridges": [],
        "counts": {"moving": 2, "needs-you": 0, "quiet": 3, "stuck": 0},
        "heard": 1_794_694_453_319_u64,
        "key": format!("team:{address}"),
        "kind": "team",
        "landed": {"at": "2026-11-14T17:52:59.468Z", "ref": "world#2447848", "session": lead, "what": "Carry Up to Their Live Releases"},
        "lead": lead,
        "liaisons": [],
        "members": members,
        "swarm": null,
        "title": format!("The Work {address} Leads"),
        "within": within,
        "worst": "moving",
    })
}

/// A machine record named `name`, about the size of a live one.
fn machine(name: &str, random: &mut Random) -> Value {
    json!({
        "accelerators": null,
        "arch": "x86_64",
        "cpu": {"cores": 16, "model": "AMD Ryzen 9"},
        "declared": {"agents": {"bench": "pi"}, "costs_money": false, "outlives": [], "probes": {}, "reach": ["internet"], "taints": []},
        "disk": 994_662_584_320_u64,
        "disk_band": "healthy",
        "disk_free": 383_953_088_512_u64,
        "heard": 1_794_694_471_264_u64,
        "here": false,
        "key": format!("machine:{name}"),
        "kind": "machine",
        "measured_at": "2026-11-14T22:14:31.264451Z",
        "memory": 51_539_607_552_u64,
        "model": "Workstation",
        "model_name": "A Workstation",
        "name": name,
        "os": "NixOS 26.05",
        "power": null,
        "reached": {"home": true},
        "room": {"memory_available": 16_213_501_542_u64, "over": [], "pressure": {"cpu": random.below(100), "io": 13.9, "memory": 50.65}, "said_at": "2026-11-14T22:14:09.756Z", "takes": {"builds": 0, "vm_tests": 0}},
        "state": "ready",
        "system": "linux",
        "updated": {"at": "2026-11-14T22:10:00Z", "commit": "550f37cac5548e21be81a9b9b85be3be820cf78c", "step": "unchanged"},
    })
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

/// The `n`th cell's id, a UUID version 7 for a time on the day it began.
fn id(n: usize, random: &mut Random) -> String {
    let millis = BEGAN + n as u64 * 1000;
    let hex = format!(
        "{millis:012x}7{:03x}8{:03x}{:012x}",
        random.below(0x1000),
        random.below(0x1000),
        random.next() & 0xffff_ffff_ffff
    );
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// A fixed sequence of numbers that look random, so each run of this
/// makes the same mycelium.
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
