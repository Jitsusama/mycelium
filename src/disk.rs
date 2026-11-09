//! How full the disk holding a machine's home is, as the machine's own
//! disk guard judges it. The guard writes its status every 30 seconds,
//! each disk in a band by the room it has left, and this reads that
//! verdict rather than a threshold of mycelium's, as `room` reads the
//! queue's.

use std::fs;
use std::path::{Path, PathBuf};

use serde::de::IntoDeserializer;
use serde::{Deserialize, Deserializer, Serialize};

/// How full a disk is, as its guard bands it, least full first, in the
/// glossary's words. The guard's words before them are read too, from a
/// guard or a node that still says them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Band {
    /// It has room.
    Healthy,
    /// It is filling, and its guard clears what has waited a while.
    #[serde(alias = "pressure")]
    Filling,
    /// It is nearly full, and its guard clears what it can.
    #[serde(alias = "urgent")]
    NearlyFull,
    /// It is full or about to be.
    #[serde(alias = "critical")]
    CriticallyFull,
}

/// A band as another machine's node said it, or none where it names one
/// this build doesn't know, so that a newer node's word costs its record
/// only the band, never the whole machine.
///
/// # Errors
///
/// Fails only where the band isn't a word at all.
pub fn band_or_none<'de, D: Deserializer<'de>>(said: D) -> Result<Option<Band>, D::Error> {
    Ok(Option::<String>::deserialize(said)?.and_then(|word| {
        Band::deserialize(
            IntoDeserializer::<serde::de::value::Error>::into_deserializer(word.as_str()),
        )
        .ok()
    }))
}

/// How long the guard's status goes unwritten before nobody keeps it: the
/// desktop calls a guard silent for two minutes dead.
const UNKEPT_SECONDS: f64 = 120.0;

/// The shape of the guard's status this reads.
const SHAPE: u64 = 2;

/// The band the guard whose status is `status` gives the disk holding
/// `home`, as of `now` in epoch seconds, or none where no guard keeps one.
#[must_use]
pub fn band(status: &Path, home: &Path, now: i64) -> Option<Band> {
    // The guard names the mounts it watches, so a home reached through a
    // link is compared where it really is.
    let home = fs::canonicalize(home).unwrap_or_else(|_| home.to_owned());
    let read = || serde_json::from_slice::<Status>(&fs::read(status).ok()?).ok();
    // The guard renames each status into place whole, so one that doesn't
    // parse is read once more rather than taken for none.
    said(read().or_else(read)?, &home, now)
}

/// The guard's status, as far as this reads it.
#[derive(Deserialize)]
struct Status {
    version: u64,
    heartbeat: f64,
    filesystems: Vec<Filesystem>,
}

/// One disk the guard watches, by where it is mounted.
#[derive(Deserialize)]
struct Filesystem {
    path: PathBuf,
    band: Band,
}

/// The band `status` gives the disk holding `home`, unless it isn't a
/// shape this reads, names no disk holding it, or nobody has kept it
/// lately.
fn said(status: Status, home: &Path, now: i64) -> Option<Band> {
    #[expect(
        clippy::cast_precision_loss,
        reason = "seconds since the epoch are exact in a double for ages yet"
    )]
    let age = now as f64 - status.heartbeat;
    if status.version != SHAPE || age >= UNKEPT_SECONDS {
        return None;
    }
    // The disk holding home is the one mounted deepest above it.
    status
        .filesystems
        .into_iter()
        .filter(|disk| home.starts_with(&disk.path))
        .max_by_key(|disk| disk.path.components().count())
        .map(|disk| disk.band)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_793_262_840;

    /// What `status` says of `home` now.
    fn says(status: &[u8], home: &Path) -> Option<Band> {
        said(serde_json::from_slice(status).ok()?, home, NOW)
    }

    fn status(heartbeat: i64, disks: &[(&str, &str)]) -> Vec<u8> {
        let filesystems: Vec<_> = disks
            .iter()
            .map(|(path, band)| serde_json::json!({"path": path, "band": band, "free": 1}))
            .collect();
        serde_json::json!({
            "version": 2,
            "heartbeat": heartbeat,
            "says": "The disk has room.",
            "filesystems": filesystems,
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn the_disk_holding_home_is_banded_as_its_guard_says() {
        let home = Path::new("/home/joel");
        assert_eq!(
            says(&status(NOW, &[("/", "healthy")]), home),
            Some(Band::Healthy),
            "the root disk holds a home with none of its own"
        );
        assert_eq!(
            says(
                &status(NOW, &[("/", "healthy"), ("/home", "nearly-full")]),
                home
            ),
            Some(Band::NearlyFull),
            "a disk mounted deeper above home holds it"
        );
        assert_eq!(
            says(&status(NOW, &[("/srv", "critically-full")]), home),
            None,
            "a disk not holding home says nothing of it"
        );
    }

    #[test]
    fn a_home_reached_through_a_link_is_banded_where_it_really_is() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let real = fs::canonicalize(folder.path()).expect("it resolves");
        let mounted = real.join("disk");
        fs::create_dir(&mounted).expect("the disk's folder is made");
        let linked = real.join("home");
        std::os::unix::fs::symlink(&mounted, &linked).expect("home links to it");
        let path = real.join("status.json");
        let disk = mounted.to_str().expect("a path in text");
        fs::write(&path, status(NOW, &[("/", "healthy"), (disk, "filling")]))
            .expect("the status is written");
        assert_eq!(
            band(&path, &linked, NOW),
            Some(Band::Filling),
            "the disk the link leads to holds home"
        );
    }

    #[test]
    fn a_guard_that_stopped_or_speaks_otherwise_says_nothing() {
        let home = Path::new("/home/joel");
        assert_eq!(
            says(&status(NOW - 120, &[("/", "filling")]), home),
            None,
            "a guard silent two minutes keeps nothing"
        );
        let other = String::from_utf8(status(NOW, &[("/", "filling")]))
            .expect("text")
            .replace("\"version\":2", "\"version\":3");
        assert_eq!(
            says(other.as_bytes(), home),
            None,
            "a shape this build doesn't read says nothing"
        );
        assert_eq!(
            says(&status(NOW, &[("/", "overflowing")]), home),
            None,
            "a band this build doesn't know says nothing"
        );
        let folder = tempfile::tempdir().expect("a temporary folder");
        assert_eq!(
            band(&folder.path().join("status.json"), home, NOW),
            None,
            "no guard keeps no band"
        );
    }

    #[test]
    fn a_guard_still_saying_the_old_words_is_banded_in_the_new() {
        let home = Path::new("/home/joel");
        for (old, new) in [
            ("pressure", Band::Filling),
            ("urgent", Band::NearlyFull),
            ("critical", Band::CriticallyFull),
        ] {
            assert_eq!(
                says(&status(NOW, &[("/", old)]), home),
                Some(new),
                "{old} is read as {new:?}"
            );
        }
        assert_eq!(
            serde_json::to_value(Band::NearlyFull).expect("a band says itself"),
            "nearly-full",
            "a band is said in the glossary's words"
        );
    }
}
