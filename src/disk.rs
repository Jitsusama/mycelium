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
    /// It is full: under the room its store keeps free, or past all its
    /// guard can clear, so what starts there may fail to write.
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
    /// Its bytes free, where the guard says them.
    #[serde(default, deserialize_with = "room_said")]
    free: Option<f64>,
    /// The bytes its store keeps free by collecting its own garbage.
    #[serde(default, deserialize_with = "room_said")]
    nix_floor: Option<f64>,
    /// Whether the guard has cleared all it can.
    #[serde(default, deserialize_with = "room_said")]
    exhausted: Option<bool>,
}

/// A figure beside a band as the guard said it, or none where it says
/// it in another shape, so an odd figure costs only itself and never the
/// band beside it.
fn room_said<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(
    said: D,
) -> Result<Option<T>, D::Error> {
    Ok(serde_json::from_value(serde_json::Value::deserialize(said)?).ok())
}

impl Filesystem {
    /// Its band as mycelium means it. The guard bands a disk critically
    /// full by how hard it clears, which it may reach by forecast hours
    /// before the disk fills, so one it says is above its store's floor
    /// with more to clear is nearly full here. Only a guard saying all
    /// three, its bytes free, its store's floor and that it isn't
    /// exhausted, is read so; one saying less is taken at its word.
    fn band(&self) -> Band {
        let room_to_write = matches!(
            (self.free, self.nix_floor, self.exhausted),
            (Some(free), Some(floor), Some(false)) if free > floor
        );
        if self.band == Band::CriticallyFull && room_to_write {
            Band::NearlyFull
        } else {
            self.band
        }
    }
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
        .map(|disk| disk.band())
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
    fn a_disk_critically_full_by_forecast_alone_is_nearly_full() {
        let home = Path::new("/home/joel");
        // A real guard's figures, in bytes: 42 GiB free over a 20 GiB floor.
        let (free, floor) = (45_115_994_112_u64, 21_474_836_480_u64);
        let banded = |band: &str, said: serde_json::Value| {
            let mut disk = serde_json::json!({"path": "/", "band": band});
            disk.as_object_mut()
                .expect("a disk is an object")
                .extend(said.as_object().expect("what it says is an object").clone());
            let status = serde_json::json!({
                "version": 2,
                "heartbeat": NOW,
                "filesystems": [disk],
            });
            says(status.to_string().as_bytes(), home)
        };
        let critically = |said| banded("critically-full", said);
        let all = |free: u64, exhausted| {
            serde_json::json!({
                "free": free,
                "nix_floor": floor,
                "exhausted": exhausted,
            })
        };
        assert_eq!(
            critically(all(free, false)),
            Some(Band::NearlyFull),
            "above its store's floor with more to clear, it has room to write yet"
        );
        for (said, why) in [
            (all(floor - 1, false), "under its store's floor, it is full"),
            (all(floor, false), "at its store's floor, it is full"),
            (all(free, true), "past all its guard can clear, it is full"),
            (
                serde_json::json!({"free": 0, "exhausted": false}),
                "with no floor said, its guard's word stands",
            ),
            (
                serde_json::json!({"free": free, "nix_floor": floor}),
                "with nothing said of what it cleared, its guard's word stands",
            ),
            (
                serde_json::json!({"free": "42 GiB", "nix_floor": floor, "exhausted": false}),
                "a figure in another shape is none, and its guard's word stands",
            ),
            (
                serde_json::json!({}),
                "a guard saying nothing more is taken at its word",
            ),
        ] {
            assert_eq!(critically(said), Some(Band::CriticallyFull), "{why}");
        }
        assert_eq!(
            banded("healthy", all(floor - 1, true)),
            Some(Band::Healthy),
            "a band short of critically full is the guard's, never raised"
        );
        assert_eq!(
            banded("nearly-full", serde_json::json!({"exhausted": 1})),
            Some(Band::NearlyFull),
            "an odd figure costs only itself, never its band"
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
