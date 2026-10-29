//! Whether the networks a machine reaches only some of the time are
//! reached now. A laptop reaches its home only at home, and a company's
//! services only while its tunnel is up, so its configuration names a
//! probe for each such network rather than declaring it (`declared`).
//! The machine's node tries each probe, opening its connection or running
//! its command, and writes which answered beside its socket, where every end of a link on the
//! machine reads it, as the queue's record is read. A record its node has
//! stopped writing says nothing, never what was once so.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;

use crate::declared::{self, Probe};

/// How often a node probes again: often enough that work placed a minute
/// after a tunnel drops isn't sent through it.
pub const PROBE_EVERY: Duration = Duration::from_secs(30);

/// How long a probe has to answer before its network counts as out of
/// reach; a network that answers at all answers well within it.
const ANSWERED_WITHIN: Duration = Duration::from_secs(3);

/// How old a record may be, in seconds, before it says nothing: three
/// rounds, so one slow round doesn't blank it.
const SAYS_FOR: i64 = 90;

/// What a node writes of its last round of probes.
#[derive(Debug, Serialize, Deserialize)]
struct Record {
    /// When the round ended, in seconds since the epoch.
    at: i64,
    /// Whether each probed network answered.
    reached: BTreeMap<String, bool>,
}

/// Whether each network of `probes` answers now, by name.
pub async fn probe(probes: &BTreeMap<String, Probe>) -> BTreeMap<String, bool> {
    let mut asking = JoinSet::new();
    for (network, probe) in probes {
        let (network, probe) = (network.clone(), probe.clone());
        asking.spawn(async move { (network, answers(&probe).await) });
    }
    let mut reached = BTreeMap::new();
    while let Some(answer) = asking.join_next().await {
        if let Ok((network, answered)) = answer {
            reached.insert(network, answered);
        }
    }
    reached
}

/// Whether `probe` answers in time.
async fn answers(probe: &Probe) -> bool {
    match probe {
        Probe::Connect(address) => opens(address).await,
        Probe::Run(command) => succeeds(command).await,
    }
}

/// Whether a connection to `address`, a `host:port`, opens in time.
async fn opens(address: &str) -> bool {
    let opening = tokio::net::TcpStream::connect(address);
    matches!(
        tokio::time::timeout(ANSWERED_WITHIN, opening).await,
        Ok(Ok(_))
    )
}

/// Whether `command`, a program and its words, exits 0 in time. One that
/// can't start, or runs too long, is killed and hasn't answered.
async fn succeeds(command: &[String]) -> bool {
    let Some((program, words)) = command.split_first() else {
        return false;
    };
    let running = tokio::process::Command::new(program)
        .args(words)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status();
    matches!(
        tokio::time::timeout(ANSWERED_WITHIN, running).await,
        Ok(Ok(status)) if status.success()
    )
}

/// Probes what the configuration at `declared` names, every
/// [`PROBE_EVERY`], writing each round to `record`, for as long as it is
/// left to run.
pub async fn keep(declared: PathBuf, record: PathBuf) {
    loop {
        // Read each round, so a switch that changes the probes is heard
        // without restarting the node.
        let probes = declared::read(&declared)
            .map(|declared| declared.probes)
            .unwrap_or_default();
        let reached = probe(&probes).await;
        if let Err(error) = write(&record, &reached, jiff::Timestamp::now().as_second()) {
            eprintln!("what this machine reaches can't be kept: {error}");
        }
        tokio::time::sleep(PROBE_EVERY).await;
    }
}

/// Writes `reached` to `record` as of `at`, whole, so a reader never sees
/// half a round.
pub fn write(record: &Path, reached: &BTreeMap<String, bool>, at: i64) -> io::Result<()> {
    let text = serde_json::to_vec(&Record {
        at,
        reached: reached.clone(),
    })?;
    let staged = record.with_extension("json.new");
    if let Some(folder) = record.parent() {
        fs::create_dir_all(folder)?;
    }
    fs::write(&staged, text)?;
    fs::rename(&staged, record)
}

/// Whether each probed network answered, as the record at `record` says at
/// `now`, or nothing where no record is kept, or none kept lately.
#[must_use]
pub fn read(record: &Path, now: i64) -> BTreeMap<String, bool> {
    let Ok(text) = fs::read(record) else {
        return BTreeMap::new();
    };
    match serde_json::from_slice::<Record>(&text) {
        Ok(kept) if now - kept.at <= SAYS_FOR => kept.reached,
        _ => BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_network_is_reached_while_its_probe_answers() {
        let listening = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a listener binds");
        let open = listening.local_addr().expect("it has an address");
        // A port just let go of answers nothing.
        let closed = {
            let gone = std::net::TcpListener::bind("127.0.0.1:0").expect("a listener binds");
            gone.local_addr().expect("it has an address")
        };
        let probes = BTreeMap::from([
            ("home".to_owned(), Probe::Connect(open.to_string())),
            ("office".to_owned(), Probe::Connect(closed.to_string())),
        ]);
        assert_eq!(
            probe(&probes).await,
            BTreeMap::from([("home".to_owned(), true), ("office".to_owned(), false)]),
            "a probe that opens is reached, one refused isn't"
        );
    }

    #[tokio::test]
    async fn a_network_is_reached_while_its_command_succeeds() {
        let run = |words: &[&str]| Probe::Run(words.iter().map(|&word| word.to_owned()).collect());
        let probes = BTreeMap::from([
            ("home".to_owned(), run(&["true"])),
            ("office".to_owned(), run(&["false"])),
            ("lab".to_owned(), run(&["mycelium-has-no-such-program"])),
            ("nowhere".to_owned(), Probe::Run(Vec::new())),
        ]);
        assert_eq!(
            probe(&probes).await,
            BTreeMap::from([
                ("home".to_owned(), true),
                ("lab".to_owned(), false),
                ("nowhere".to_owned(), false),
                ("office".to_owned(), false),
            ]),
            "only a command that exits 0 reaches its network"
        );
    }

    #[test]
    fn a_record_says_what_was_reached_only_while_it_is_kept() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let record = folder.path().join("reached.json");
        assert!(read(&record, 0).is_empty(), "no record says nothing");
        let reached = BTreeMap::from([("home".to_owned(), false)]);
        write(&record, &reached, 1_000).expect("the record is written");
        assert_eq!(read(&record, 1_030), reached, "a fresh record says it");
        assert!(
            read(&record, 1_000 + SAYS_FOR + 1).is_empty(),
            "a record its node stopped keeping says nothing"
        );
    }
}
