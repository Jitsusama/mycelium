//! A carry's hands as the `mycelium carry` command has them: this
//! machine's node over its socket, the quest's handover in its scratch,
//! `pi-carry` for what no session carries, and the quest's share.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::{Done, Hands, PI_CARRY};
use crate::client::Client;
use crate::record::Session;
use crate::share::exchange::Places;
use crate::wire::{Request, Response, Start, Started};

/// How long between looks at what a carry waits for.
const LOOK_EVERY: Duration = Duration::from_secs(2);

/// How long a carry waits, all told, for the sessions to carry and then
/// to end: time for a busy one to finish its turn and push.
pub const PATIENCE: Duration = Duration::from_secs(15 * 60);

/// The command's hands: the node at `socket`, and the quest's share.
pub struct Through {
    socket: PathBuf,
    shares: Places,
    quest: String,
    /// Where this machine's carries are put: the quest's scratch's
    /// `handover/<this machine's short hostname>/`, as the harness puts
    /// them.
    handover: PathBuf,
    until: Instant,
}

impl Through {
    /// Hands for carrying `quest` through the node at `socket`, with the
    /// quest's share in `shares`, waiting no longer than `patience`.
    #[must_use]
    pub fn new(socket: PathBuf, shares: Places, quest: &str, patience: Duration) -> Self {
        let host = rustix::system::uname()
            .nodename()
            .to_string_lossy()
            .into_owned();
        let handover = shares
            .scratch
            .join(quest)
            .join("handover")
            .join(short_host(&host));
        Self {
            socket,
            shares,
            quest: quest.to_owned(),
            handover,
            until: Instant::now() + patience,
        }
    }

    /// The node's answer to `request`, or why there is none.
    fn ask(&self, request: &Request) -> Result<Response, String> {
        Client::connect(&self.socket)
            .and_then(|mut client| client.ask(request))
            .map_err(|error| format!("the node didn't answer: {error}"))?
    }
}

impl Hands for Through {
    fn machines(&mut self) -> Result<Vec<String>, String> {
        match self.ask(&Request::Machines)? {
            // This one first, then each other one.
            Response::Machines(machines) => {
                Ok(machines.into_iter().skip(1).map(|one| one.name).collect())
            }
            other => Err(format!("the node answered {other:?} for its machines")),
        }
    }

    fn sessions(&mut self) -> Result<Vec<Session>, String> {
        match self.ask(&Request::Sessions)? {
            Response::Sessions(sessions) => Ok(sessions),
            other => Err(format!("the node answered {other:?} for its sessions")),
        }
    }

    fn boot(&self) -> Option<String> {
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|boot| boot.trim().to_owned())
    }

    fn steer(&mut self, address: &str, steer: Value) -> Result<(), String> {
        let asked = Request::Steer {
            address: address.to_owned(),
            steer,
        };
        self.ask(&asked).map(|_| ())
    }

    fn done(&mut self) -> Result<Vec<Done>, String> {
        let entries = match fs::read_dir(&self.handover) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("{}: {error}", self.handover.display())),
        };
        let mut done = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let named = path.file_name().and_then(OsStr::to_str).unwrap_or("");
            if !named.ends_with(".done.json") {
                continue;
            }
            let read = fs::read_to_string(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let one: Done = serde_json::from_str(&read).map_err(|error| {
                format!("{} isn't a carry's done file: {error}", path.display())
            })?;
            done.push(one);
        }
        done.sort_by(|one, other| one.session.cmp(&other.session));
        Ok(done)
    }

    fn carry_rest(&mut self, quest: &str, to: &str) -> Result<(), String> {
        let ran = Command::new(PI_CARRY)
            .args([quest, "--to", to])
            .output()
            .map_err(|error| format!("{PI_CARRY} didn't run: {error}"))?;
        if ran.status.success() {
            Ok(())
        } else {
            Err(format!(
                "{PI_CARRY} failed: {}",
                failed_why(&ran.stdout, &ran.stderr)
            ))
        }
    }

    fn share(&mut self, quest: &str) -> Result<(), String> {
        debug_assert_eq!(quest, self.quest, "a carry shares its own quest");
        self.shares
            .offer(quest, &["handover".to_owned()])
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn start(&mut self, start: Start) -> Result<Started, String> {
        match self.ask(&Request::Start(start))? {
            Response::Started(started) => Ok(started),
            other => Err(format!("the node answered {other:?} for a start")),
        }
    }

    fn wait(&mut self) -> bool {
        if Instant::now() >= self.until {
            return false;
        }
        thread::sleep(LOOK_EVERY);
        true
    }
}

/// A hostname up to its first dot, as the harness names a machine's
/// handover: a Mac calls itself `lantern.local`, and its handover goes in
/// `lantern`.
fn short_host(host: &str) -> &str {
    host.split('.')
        .next()
        .filter(|short| !short.is_empty())
        .unwrap_or(host)
}

/// Why `pi-carry` failed: what its `{"kind": "failed", "why"}` lines said
/// on its output, since it says everything there, else what it wrote to
/// its errors.
fn failed_why(output: &[u8], errors: &[u8]) -> String {
    let output = String::from_utf8_lossy(output);
    let said: Vec<String> = output
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|line| line["kind"] == "failed")
        .filter_map(|line| line["why"].as_str().map(str::to_owned))
        .collect();
    if said.is_empty() {
        String::from_utf8_lossy(errors).trim().to_owned()
    } else {
        said.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::{failed_why, short_host};

    #[test]
    fn a_handover_goes_by_the_short_hostname() {
        assert_eq!(short_host("lantern.local"), "lantern", "a Mac's");
        assert_eq!(short_host("skiff"), "skiff", "one with no dot");
        assert_eq!(short_host(".odd"), ".odd", "never an empty name");
    }

    #[test]
    fn pi_carry_failing_says_why_from_its_own_lines() {
        let output = concat!(
            "{\"kind\":\"tree\",\"path\":\"~/src/harbour\"}\n",
            "{\"kind\":\"failed\",\"why\":\"~/src/harbour has changes no commit holds\"}\n"
        );
        assert_eq!(
            failed_why(output.as_bytes(), b"node: warning"),
            "~/src/harbour has changes no commit holds",
            "its failed line, not its errors"
        );
        assert_eq!(
            failed_why(b"not a line\n", b"node: not found\n"),
            "node: not found",
            "its errors where it said nothing of its own"
        );
    }
}
