//! A machine within another, as a virtual machine runs inside the laptop
//! that hosts it. It reads no battery and sees only the share of the
//! processors and memory it was given, so what it measures of itself
//! would show a machine always plugged in and idle, though its host may
//! run on its battery or be busy with its person's own work. So its host
//! tells it, in a file the host's configuration shares with it, the power
//! and room the host has, and it has them as its own.
//!
//! The host's node says them each minute. A machine whose host has said
//! nothing for longer has its own reading of both, since a host gone
//! quiet may have slept or stopped saying.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::facts::Power;
use crate::room::Room;

/// How long a host's word stands, in seconds: two of its minutes, so one
/// late saying doesn't lose it.
const STANDS_SECONDS: i64 = 120;

/// What a host says of itself to a machine within it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Said {
    /// When it said it.
    pub at: jiff::Timestamp,
    /// Its power, if it reads any.
    pub power: Option<Power>,
    /// Its room, if its queue says any.
    pub room: Option<Room>,
}

/// Tells the machine reading `file` what this one, its host, says, as
/// one whole file: written beside it, then renamed into place, so it never
/// reads half of one.
pub fn tell(file: &Path, said: &Said) -> io::Result<()> {
    let mut beside = file.as_os_str().to_owned();
    beside.push(".new");
    let beside = PathBuf::from(beside);
    let written = serde_json::to_vec(said)
        .map_err(io::Error::other)
        .and_then(|text| fs::write(&beside, text))
        .and_then(|()| fs::rename(&beside, file));
    if written.is_err() {
        // A file left half written would only be written over next time.
        let _ = fs::remove_file(&beside);
    }
    written
}

/// What the host said in `file`, if it said it lately enough at `now`.
#[must_use]
pub fn told(file: &Path, now: jiff::Timestamp) -> Option<Said> {
    let said: Said = serde_json::from_slice(&fs::read(file).ok()?).ok()?;
    let age = now.as_second().saturating_sub(said.at.as_second());
    (age < STANDS_SECONDS).then_some(said)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_host_says_is_read_while_it_stands() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let file = dir.path().join("methuselah.json");
        let at: jiff::Timestamp = "2026-10-30T21:00:00Z".parse().expect("a time");
        let said = Said {
            at,
            power: Some(Power {
                on_battery: true,
                charge: Some(64),
            }),
            room: None,
        };
        tell(&file, &said).expect("told");

        assert_eq!(
            told(
                &file,
                at + jiff::SignedDuration::from_secs(STANDS_SECONDS - 1)
            ),
            Some(said),
            "what the host said stands for two of its minutes"
        );
        assert_eq!(
            told(&file, at + jiff::SignedDuration::from_secs(STANDS_SECONDS)),
            None,
            "and is gone once it has said nothing longer"
        );
        assert!(
            !dir.path().join("methuselah.json.new").exists(),
            "nothing is left beside it"
        );
    }
}
