//! What this machine is, as its node measures it: the facts work can be
//! placed by: what can be read from the machine itself, the room its own
//! queue says it has, and what its configuration declares. A fact that
//! can't be read is unknown, never a guess.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::busy;
use crate::declared::{self, Declared};
use crate::disk::{self, Band};
use crate::own_health::{self, Health};
use crate::place;
use crate::reach;
use crate::room::{self, Room};
use crate::update;
use crate::within;

/// This machine, as measured when asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    /// Its name, up to its first dot.
    pub name: String,
    /// When it was measured, as an RFC 3339 time.
    pub measured_at: String,
    /// Whether it takes work.
    pub state: Readiness,
    /// Its operating system, as it names itself, such as "NixOS 26.11
    /// (Zokor)" or "macOS 26.1".
    pub os: Option<String>,
    /// Its kind of system, as Rust names it: `linux` or `macos`; unknown
    /// from a node too old to say.
    #[serde(default)]
    pub system: Option<String>,
    /// Its architecture, as Rust names it: `x86_64` or `aarch64`.
    pub arch: String,
    /// Its maker's name for it, such as "Dell Inc. XPS 14 DA14260", or
    /// the identifier a Mac goes by, such as "Mac14,2".
    pub model: Option<String>,
    /// What its maker calls its kind, in words, such as `MacBook Air` or
    /// `XPS 14 DA14260`, where `model` may be only an identifier; unknown
    /// from a node too old to say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    /// Its processor.
    pub cpu: Cpu,
    /// Its memory, in bytes.
    pub memory: Option<u64>,
    /// The size of the disk holding its account's home, in bytes.
    pub disk: Option<u64>,
    /// How much of that disk its account may still write, in bytes;
    /// unknown from a node too old to say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_free: Option<u64>,
    /// How full that disk is, as the machine's disk guard bands it, or
    /// unknown where no guard keeps one, or where another machine's node
    /// names a band this build doesn't know.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "disk::band_or_none"
    )]
    pub disk_band: Option<Band>,
    /// Its graphics and neural processors, by the driver that runs each;
    /// unknown where they can't be listed.
    pub accelerators: Option<Vec<Accelerator>>,
    /// What it runs on now, or unknown where that can't be read.
    pub power: Option<Power>,
    /// The room it has for more work, as its queue says, or unknown where
    /// nobody keeps one.
    pub room: Option<Room>,
    /// What its configuration says of it, or unknown where it says
    /// nothing.
    pub declared: Option<Declared>,
    /// Whether its node answered that it declares nothing of itself, as
    /// one whose declaration was taken away does, where `declared` is
    /// unknown: a link's end says so, apart from an end whose node didn't
    /// answer, as one restarting doesn't, so the end hearing it drops what
    /// the machine declared before at once rather than keeping it through
    /// a silence. Unset from an end too old to say, and never kept.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub declares_nothing: bool,
    /// Whether each network its configuration probes answered when its
    /// node last probed, by name; a network not in it is unknown.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub reached: BTreeMap<String, bool>,
    /// How its last update to its configuration's main ended, as its
    /// updater keeps it ([`crate::update`]): the commit it runs after it,
    /// and whether it waits on a restart. Unknown where no updater keeps
    /// one, as since the machine last started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<serde_json::Value>,
    /// How it stands, as its own health daemon says ([`own_health`]):
    /// unknown where its node asks none, none answers, or the node is too
    /// old to say. Measuring leaves it unknown; the node and its links set
    /// it from the health they keep.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "own_health::cleaned"
    )]
    pub health: Option<Health>,
    /// The realm's kit as its node was given it: the tools each session
    /// and run it starts finds first on its path, or, where the node
    /// refused it, why, with no tools. Unknown where its node was given
    /// none, the folder it carries can no longer be listed, or the node is
    /// too old to say. Measuring leaves it unknown; the node sets it from
    /// the kit as it checked it at its start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kit: Option<Kit>,
    /// Whether the node keeping these facts reached the machine as a
    /// worker, one a provider made that may be administered by others, so
    /// all it says of itself, its costs included, is its own word. Set by
    /// that node's link alone, never taken from what the machine says;
    /// measuring leaves it unset.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub worker: bool,
    /// Whether its node starts an agent's held session through the launcher
    /// the session's fence declares there, as a Mac's node now does, so a
    /// Mac saying it may take an agent's held start. A node too old to say
    /// starts one through the login shell alone, where pi on a Mac runs
    /// outside any fence, so its Mac is refused every agent's held start.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fences_held: bool,
    /// How many sessions its node holds running, parked ones aside, each
    /// a start's whether or not it says it is a session: a fenced pi on a
    /// worker runs with no mesh, so no presence counts it. Weighed against
    /// how many it holds at most ([`crate::placement::full`]). Unknown
    /// from a node too old to say; measuring leaves it unknown, and the
    /// node and its links set it from the sessions they hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holds: Option<u32>,
}

/// A realm's kit on one machine: a folder whose `bin` holds the tools
/// every machine in the realm is to have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kit {
    /// Where it is, with any link it was given through resolved as its node
    /// checked it, so a Nix profile's link names the store path it was then,
    /// which names its version: two machines with the same path have the
    /// same tools. Where it is refused, the folder as it was given.
    pub path: String,
    /// The name of each tool in its `bin`, in order as this node lists
    /// them, though a record read from another may not be. None where it
    /// is refused.
    pub tools: Vec<String>,
    /// Why its node leaves it off the path of all it starts, where it does: its path is
    /// then the folder as given, and it names no tools, so a node too old
    /// to read why still finds none in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

impl Kit {
    /// Whether it holds the tool `name`.
    #[must_use]
    pub fn holds(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool == name)
    }
}

/// The kit in `folder`, its tools listed from its `bin`, or unknown where
/// they can't be: a kit whose tools are unknown would meet no need for
/// one, so it is none rather than one with no tools.
#[must_use]
pub fn kit(folder: &Path) -> Option<Kit> {
    // Resolved before it is listed, so a profile switched between the two
    // can't pair one generation's path with another's tools.
    let path = std::fs::canonicalize(folder).ok()?;
    let mut tools: Vec<String> = std::fs::read_dir(path.join("bin"))
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|tool| tool.file_name().into_string().ok())
        // A start-up file's dotted names are no tools a need could name.
        .filter(|tool| !tool.starts_with('.'))
        .collect();
    tools.sort_unstable();
    Some(Kit {
        path: path.display().to_string(),
        tools,
        refused: None,
    })
}

/// The system a machine measured here says it is, as Rust names it.
#[cfg(not(test))]
pub(crate) const SYSTEM: &str = std::env::consts::OS;

/// The system every machine measured in a test says it is, whatever
/// builds the tests: a Mac takes no agent's held start, so a test node
/// and its stand-ins measured on one would refuse every such start the
/// tests make. A test's Mac says its system itself.
#[cfg(test)]
pub(crate) const SYSTEM: &str = "linux";

/// Where a machine's facts are read from, beyond the machine itself.
#[derive(Debug, Clone)]
pub struct Sources {
    /// Its account's home, whose disk is the one measured.
    pub home: PathBuf,
    /// Its queue's record, if it keeps one anywhere.
    pub queue: Option<PathBuf>,
    /// Its disk guard's status, if it keeps one anywhere.
    pub disk_guard: Option<PathBuf>,
    /// What its configuration declares, or none where nothing it declares
    /// is to be trusted, so its facts say only what is measured.
    pub declared: Option<PathBuf>,
    /// Its node's record of which probed networks answered.
    pub reached: PathBuf,
    /// Its updater's last update's ending.
    pub updated: PathBuf,
}

impl Sources {
    /// Where this machine's account keeps each.
    #[must_use]
    pub fn here() -> Self {
        Self {
            home: std::env::home_dir().unwrap_or_else(std::env::temp_dir),
            queue: place::queue(),
            disk_guard: place::disk_guard(),
            declared: Some(place::declared()),
            reached: place::reached(),
            updated: place::updated(),
        }
    }

    /// Nothing beyond the machine itself: no queue, guard, declaration,
    /// reached networks or update, and the root's disk. A machine
    /// measured from them still says its own processors and memory.
    #[cfg(test)]
    pub(crate) fn nowhere() -> Self {
        Self {
            home: "/".into(),
            queue: None,
            disk_guard: None,
            declared: None,
            reached: "/nowhere".into(),
            updated: "/nowhere".into(),
        }
    }
}

/// Whether a machine takes work. A node answering for itself is ready,
/// and so is another machine while a link carries what it says; states
/// for a machine asleep or taken back come later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Readiness {
    /// It takes work.
    Ready,
    /// No link reaches it now, so what it says is as of when one last did.
    Unreachable,
}

/// A machine's processor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cpu {
    /// Its name, such as "Intel(R) Core(TM) Ultra X7 358H".
    pub model: Option<String>,
    /// How many it runs at once: the machine's, not what this process may
    /// use.
    pub cores: Option<u32>,
    /// The share of the last few seconds its cores spent busy, 0 to 1, as
    /// [`busy`](crate::busy) counts it; unknown from a node too old to say,
    /// a system that doesn't count it, or before a stretch has passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy: Option<f64>,
}

/// The count processor use is measured from, kept between measures.
static SAMPLER: std::sync::Mutex<Option<busy::Sampler>> = std::sync::Mutex::new(None);

/// How busy this machine's processors were over the last stretch, where
/// its system counts it.
fn busy_now() -> Option<f64> {
    let ticks = here::ticks()?;
    // A measure that panicked holding the count left it whole, since a
    // take writes it at once, so its count is still good to read on.
    let mut sampler = SAMPLER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    sampler
        .get_or_insert_with(busy::Sampler::default)
        .take(std::time::Instant::now(), ticks)
}

/// A graphics or neural processor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accelerator {
    /// Which kind.
    pub kind: AcceleratorKind,
    /// The driver that runs it, such as `xe` or `intel_vpu`.
    pub driver: String,
}

/// What a machine runs on, as it was measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Power {
    /// Whether it runs on its battery, with nothing plugged in.
    pub on_battery: bool,
    /// How full its battery is, in percent, or unknown where it has none.
    pub charge: Option<u8>,
}

/// The kinds of accelerator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcceleratorKind {
    /// A graphics processor.
    Gpu,
    /// A neural processor.
    Npu,
}

/// Measures this machine, named `name`, as `realm` hears it: what it
/// declares of other realms is left out.
#[must_use]
pub fn measure_in(name: &str, realm: &str, sources: &Sources) -> Machine {
    let mut machine = measure(name, sources);
    machine.declared = machine.declared.map(|said| said.in_realm(realm));
    machine
}

/// Measures this machine, named `name`, reading the rest from `sources`.
#[must_use]
pub fn measure(name: &str, sources: &Sources) -> Machine {
    let read = here::read();
    let now = jiff::Timestamp::now();
    let declared = sources.declared.as_deref().and_then(declared::read);
    // A machine given a share of its host lists the host's processors, so
    // what it declares it is given comes first.
    let cpu = Cpu {
        cores: declared
            .as_ref()
            .and_then(|declared| declared.cores)
            .or(read.cpu.cores),
        busy: busy_now(),
        ..read.cpu
    };
    let room = sources
        .queue
        .as_deref()
        .and_then(|queue| room::read(queue, now.as_second()));
    // A machine within another has its host's power and room while its
    // host's word stands, since it reads neither of its own truly.
    let host = declared
        .as_ref()
        .and_then(|declared| declared.within.as_ref())
        .and_then(|within| within::told(&within.file, now));
    let (power, room) = match host {
        Some(host) => (host.power, host.room),
        None => (read.power, room),
    };
    let disk = disk_of(&sources.home);
    Machine {
        name: name.to_owned(),
        measured_at: now.to_string(),
        state: Readiness::Ready,
        os: read.os,
        system: Some(SYSTEM.to_owned()),
        arch: std::env::consts::ARCH.to_owned(),
        model: read.model,
        model_name: read.model_name,
        cpu,
        memory: read.memory,
        disk: disk.map(|(size, _)| size),
        disk_free: disk.map(|(_, free)| free),
        disk_band: sources
            .disk_guard
            .as_deref()
            .and_then(|status| disk::band(status, &sources.home, now.as_second())),
        accelerators: read.accelerators,
        power,
        room,
        declared,
        // Only a link's end says it, of its node's answer.
        declares_nothing: false,
        reached: reach::read(&sources.reached, now.as_second()),
        updated: update::last(&sources.updated),
        health: None,
        kit: None,
        worker: false,
        fences_held: FENCES_HELD,
        holds: None,
    }
}

/// Whether this build's node starts an agent's held session through the
/// launcher its fence declares: on a Mac, where pi lifts its own fence for
/// want of Landlock, so a held start needs the launcher round it. A Linux
/// node leaves it to pi's own fence, as it always has. A node is given it
/// to do and says what it was given, so what it says and what it does are
/// one value.
pub const FENCES_HELD: bool = cfg!(target_os = "macos");

/// What this machine runs on now, read alone and as [`measure`] reads it:
/// a machine within another has its host's power while its host's word
/// stands. Not cheap: a laptop's battery is read from its embedded
/// controller, and each read blocks until it answers, so the node reads it
/// once a minute, never as each session changes.
#[must_use]
pub fn power(sources: &Sources) -> Option<Power> {
    let host = sources
        .declared
        .as_deref()
        .and_then(declared::read)
        .and_then(|declared| declared.within)
        .and_then(|within| within::told(&within.file, jiff::Timestamp::now()));
    match host {
        Some(host) => host.power,
        None => here::power(),
    }
}

/// What the system's own files or tools say, before the parts every
/// system reads alike.
struct Read {
    os: Option<String>,
    model: Option<String>,
    model_name: Option<String>,
    cpu: Cpu,
    memory: Option<u64>,
    accelerators: Option<Vec<Accelerator>>,
    power: Option<Power>,
}

/// The size of the disk holding `home` and how much of it is free to its
/// account, which leaves out what only root may write.
fn disk_of(home: &Path) -> Option<(u64, u64)> {
    let stat = rustix::fs::statvfs(home).ok()?;
    Some((
        stat.f_blocks.checked_mul(stat.f_frsize)?,
        stat.f_bavail.checked_mul(stat.f_frsize)?,
    ))
}

#[cfg(target_os = "linux")]
mod here {
    //! Linux says what it is in /etc, /proc and /sys.

    use std::fs;

    use super::{Accelerator, AcceleratorKind, Cpu, Power, Read, linux};

    const SUPPLIES: &str = "/sys/class/power_supply";

    pub(super) fn read() -> Read {
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        Read {
            os: fs::read_to_string("/etc/os-release")
                .ok()
                .and_then(|text| linux::os(&text)),
            model: linux::model(
                fs::read_to_string("/sys/class/dmi/id/sys_vendor")
                    .ok()
                    .as_deref(),
                fs::read_to_string("/sys/class/dmi/id/product_name")
                    .ok()
                    .as_deref(),
            ),
            model_name: linux::model(
                None,
                fs::read_to_string("/sys/class/dmi/id/product_name")
                    .ok()
                    .as_deref(),
            ),
            cpu: Cpu {
                model: linux::cpu_model(&cpuinfo),
                cores: linux::cores(&cpuinfo),
                busy: None,
            },
            memory: fs::read_to_string("/proc/meminfo")
                .ok()
                .and_then(|text| linux::memory(&text)),
            accelerators: Some(
                drivers("/sys/class/drm", "card", AcceleratorKind::Gpu)
                    .chain(drivers("/sys/class/accel", "accel", AcceleratorKind::Npu))
                    .collect(),
            ),
            power: power(),
        }
    }

    /// What it runs on now, as its power supplies say.
    pub(super) fn power() -> Option<Power> {
        supplies().and_then(|supplies| linux::power(&supplies))
    }

    /// What the kernel has counted of its cores' time.
    pub(super) fn ticks() -> Option<crate::busy::Ticks> {
        crate::busy::linux(&fs::read_to_string("/proc/stat").ok()?)
    }

    /// Each power supply the kernel lists, or none where it can't list
    /// them.
    fn supplies() -> Option<Vec<linux::Supply>> {
        let said = |entry: &fs::DirEntry, file: &str| {
            fs::read_to_string(entry.path().join(file))
                .map(|text| text.trim().to_owned())
                .unwrap_or_default()
        };
        Some(
            fs::read_dir(SUPPLIES)
                .ok()?
                .filter_map(Result::ok)
                .map(|entry| linux::Supply {
                    kind: said(&entry, "type"),
                    scope: said(&entry, "scope"),
                    online: said(&entry, "online") == "1",
                    capacity: said(&entry, "capacity").parse().ok(),
                })
                .collect(),
        )
    }

    /// The driver of each device in `class` named `prefix` and a number:
    /// `card0`, not its outputs, `card0-eDP-1`.
    fn drivers(
        class: &str,
        prefix: &str,
        kind: AcceleratorKind,
    ) -> impl Iterator<Item = Accelerator> {
        let mut found: Vec<_> = fs::read_dir(class)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    name.strip_prefix(prefix)
                        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                })
            })
            .filter_map(|entry| {
                let driver = fs::read_link(entry.path().join("device/driver")).ok()?;
                Some(Accelerator {
                    kind,
                    driver: driver.file_name()?.to_str()?.to_owned(),
                })
            })
            .collect();
        found.sort_by(|a, b| a.driver.cmp(&b.driver));
        found.into_iter()
    }
}

#[cfg(target_os = "macos")]
mod here {
    //! macOS says what it is through `sysctl` and `sw_vers`.

    use std::collections::BTreeMap;
    use std::io::Read as _;
    use std::process::{Command, Stdio};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use super::{Cpu, Power, Read, macos};

    /// How long a program asked what this Mac is has to answer. A node
    /// measures on the thread that serves it, so one that hangs, as one
    /// did for 35 minutes under a load of 168 on 2026-11-21, stops the
    /// node answering anything.
    const SAID_WITHIN: Duration = Duration::from_secs(3);

    /// What Apple calls this Mac's kind, as `system_profiler` says it.
    /// It is kept once said, since it never changes and the asking takes a
    /// good part of a second, and asked again only while it hasn't been.
    fn model_name() -> Option<String> {
        static NAME: OnceLock<String> = OnceLock::new();
        said_once(
            &NAME,
            "/usr/sbin/system_profiler",
            &["-json", "SPHardwareDataType"],
        )
        .and_then(|said| macos::model_name(&said))
    }

    /// What `program` says, asked with `args`, kept in `kept` from the
    /// first time it answers, for what doesn't change while the node runs.
    fn said_once(kept: &OnceLock<String>, program: &'static str, args: &[&str]) -> Option<String> {
        if let Some(said) = kept.get() {
            return Some(said.clone());
        }
        let answer = said_lately(program, args)?;
        Some(kept.get_or_init(|| answer).clone())
    }

    /// What `program` says, asked with `args`, as [`said`] asks it, but
    /// where it failed lately, nothing without asking: one that failed
    /// isn't asked again for [`ASKED_AGAIN_AFTER`], so a loaded Mac waits
    /// out its deadline once in that while rather than at every measuring.
    fn said_lately(program: &'static str, args: &[&str]) -> Option<String> {
        static FAILED: Mutex<BTreeMap<&str, Instant>> = Mutex::new(BTreeMap::new());
        let lately = |failed: &BTreeMap<&str, Instant>| {
            failed
                .get(program)
                .is_some_and(|at| at.elapsed() < ASKED_AGAIN_AFTER)
        };
        if FAILED.lock().is_ok_and(|failed| lately(&failed)) {
            return None;
        }
        let answer = said(program, args);
        if answer.is_none()
            && let Ok(mut failed) = FAILED.lock()
        {
            failed.insert(program, Instant::now());
        }
        answer
    }

    /// How long a fact that failed to be said is left unasked.
    const ASKED_AGAIN_AFTER: Duration = Duration::from_secs(600);

    /// How long a program that ended is given to finish saying what it
    /// said, however near its deadline it ended.
    const SAID_AFTER_ENDING: Duration = Duration::from_millis(200);

    /// What `program` says, asked with `args`, or nothing where it fails
    /// or hasn't answered within [`SAID_WITHIN`], when it is killed. What
    /// it says is read beside the wait, so a long answer never fills the
    /// pipe and stalls it; a reader still held open by something it
    /// started is left to end with that, as a killed program is left to
    /// be reaped beside, since one stuck in the kernel can't end at once.
    /// Where no thread can be started beside, nothing is said.
    fn said(program: &str, args: &[&str]) -> Option<String> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut stdout = child.stdout.take()?;
        let (read, reading) = std::sync::mpsc::channel();
        // Its output is waited on no longer than the program, since a
        // program it left behind may hold it open after it ends.
        let reader = std::thread::Builder::new().spawn(move || {
            let mut out = Vec::new();
            let _ = read.send(stdout.read_to_end(&mut out).map(|_| out));
        });
        if reader.is_err() {
            let _ = child.kill();
            reap(child);
            return None;
        }
        let deadline = Instant::now() + SAID_WITHIN;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    let _ = child.kill();
                    reap(child);
                    return None;
                }
                Ok(Some(_)) | Err(_) => return None,
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let out = reading
            .recv_timeout(left.max(SAID_AFTER_ENDING))
            .ok()?
            .ok()?;
        String::from_utf8(out).ok()
    }

    /// Waits beside for the killed `child` to end, so the node's thread
    /// never waits on it and it leaves no zombie. Where no thread can be
    /// started it is let go unreaped, which costs a zombie, not the node.
    fn reap(mut child: std::process::Child) {
        let reaping = std::thread::Builder::new().spawn(move || child.wait());
        if let Err(error) = reaping {
            eprintln!("a killed probe couldn't be reaped beside: {error}");
        }
    }

    /// What it runs on now, as `pmset` says.
    pub(super) fn power() -> Option<Power> {
        said_lately("/usr/bin/pmset", &["-g", "batt"]).and_then(|said| macos::power(&said))
    }

    /// What the Mach host has counted of its cores' time.
    pub(super) fn ticks() -> Option<crate::busy::Ticks> {
        mach::cores().map(|cores| crate::busy::mac(&cores))
    }

    mod mach {
        //! The Mach host's count of each core's time. A Mac says it
        //! nowhere else: no file holds it, and `top` or `iostat` would
        //! have to watch for a second each time asked.

        use std::ffi::c_int;
        use std::ptr;

        use crate::busy::{self, MachCore};

        /// `PROCESSOR_CPU_LOAD_INFO`: each core's ticks by state.
        const CPU_LOAD_INFO: c_int = 2;
        /// `KERN_SUCCESS`.
        const SUCCEEDED: c_int = 0;

        #[expect(
            unsafe_code,
            reason = "the Mach calls a core's ticks are said through, which no crate here binds"
        )]
        unsafe extern "C" {
            static mach_task_self_: u32;
            fn mach_host_self() -> u32;
            fn mach_port_deallocate(task: u32, name: u32) -> c_int;
            fn host_processor_info(
                host: u32,
                flavor: c_int,
                processors: *mut u32,
                info: *mut *mut c_int,
                words: *mut u32,
            ) -> c_int;
            fn vm_deallocate(task: u32, address: usize, size: usize) -> c_int;
        }

        /// Each core's ticks, or none where the host won't say. The host
        /// port it asks through is given back, and so is the array the
        /// kernel lent the answer in, so asking every few seconds keeps
        /// nothing.
        #[expect(
            unsafe_code,
            reason = "Mach says a core's ticks only through these calls, and no crate here binds them"
        )]
        pub(in super::super) fn cores() -> Option<Vec<MachCore>> {
            // SAFETY: a process's own task port, set before main runs.
            let task = unsafe { mach_task_self_ };
            // SAFETY: takes no arguments; the right it hands back is given
            // back below.
            let host = unsafe { mach_host_self() };
            let mut processors: u32 = 0;
            let mut info: *mut c_int = ptr::null_mut();
            let mut words: u32 = 0;
            // SAFETY: each out pointer is to a live local of the type the
            // call writes.
            let said = unsafe {
                host_processor_info(
                    host,
                    CPU_LOAD_INFO,
                    &raw mut processors,
                    &raw mut info,
                    &raw mut words,
                )
            };
            // SAFETY: gives back the send right mach_host_self made. A
            // failure leaves one right more, which nothing can mend.
            unsafe { mach_port_deallocate(task, host) };
            if said != SUCCEEDED || info.is_null() {
                return None;
            }
            // Widened, never narrowed, on a Mac's 64 bits, so nothing from
            // here returns before the array is given back.
            let (words, processors) = (words as usize, processors as usize);
            // SAFETY: on success the kernel lent `words` ints at `info`,
            // read here before they are given back.
            let counts = unsafe { std::slice::from_raw_parts(info.cast::<u32>(), words) };
            let cores = busy::mach_cores(counts, processors);
            // SAFETY: gives back the whole array lent above, once, after
            // its last read.
            unsafe { vm_deallocate(task, info.addr(), words * size_of::<c_int>()) };
            Some(cores)
        }

        #[cfg(test)]
        mod tests {
            #[test]
            fn the_mach_host_counts_every_core_it_has() {
                let cores = super::cores().expect("the host says its cores' ticks");
                let had = std::thread::available_parallelism().expect("a core count");
                assert_eq!(cores.len(), had.get(), "one count for each core");
                assert!(
                    cores.iter().any(|&[user, system, idle, nice]| {
                        u64::from(user) + u64::from(system) + u64::from(idle) + u64::from(nice) > 0
                    }),
                    "a running host has counted some time: {cores:?}"
                );
            }
        }
    }

    pub(super) fn read() -> Read {
        // Neither changes while the node runs, so each is asked until it
        // answers once, and only `pmset` is asked each time.
        static SYSCTL: OnceLock<String> = OnceLock::new();
        static VERSION: OnceLock<String> = OnceLock::new();
        let sysctl = said_once(
            &SYSCTL,
            "/usr/sbin/sysctl",
            &[
                "hw.model",
                "machdep.cpu.brand_string",
                "hw.ncpu",
                "hw.memsize",
            ],
        )
        .unwrap_or_default();
        let value = |key| macos::sysctl(&sysctl, key);
        Read {
            os: said_once(&VERSION, "/usr/bin/sw_vers", &["-productVersion"])
                .map(|version| format!("macOS {}", version.trim())),
            model: value("hw.model"),
            model_name: model_name(),
            cpu: Cpu {
                model: value("machdep.cpu.brand_string"),
                cores: value("hw.ncpu").and_then(|n| n.parse().ok()),
                busy: None,
            },
            memory: value("hw.memsize").and_then(|n| n.parse().ok()),
            // A Mac's graphics and neural engines can't be listed without
            // a binding this crate doesn't have.
            accelerators: None,
            power: power(),
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod here {
    //! Elsewhere, only what Rust itself knows.

    use super::{Cpu, Power, Read};

    /// Nothing says what it runs on.
    pub(super) fn power() -> Option<Power> {
        None
    }

    /// Nothing counts its cores' time.
    pub(super) fn ticks() -> Option<crate::busy::Ticks> {
        None
    }

    pub(super) fn read() -> Read {
        Read {
            os: None,
            model: None,
            model_name: None,
            cpu: Cpu {
                model: None,
                cores: None,
                busy: None,
            },
            memory: None,
            accelerators: None,
            power: power(),
        }
    }
}

/// Reading what Linux's files say.
#[cfg_attr(
    not(any(target_os = "linux", test)),
    expect(dead_code, reason = "read on Linux only, tested everywhere")
)]
mod linux {
    /// The system's name from os-release: its pretty name, else its name
    /// and version.
    pub(super) fn os(release: &str) -> Option<String> {
        let field = |key: &str| {
            release.lines().find_map(|line| {
                let value = line.strip_prefix(key)?.strip_prefix('=')?;
                let value = value.trim().trim_matches('"').trim();
                (!value.is_empty()).then(|| value.to_owned())
            })
        };
        field("PRETTY_NAME").or_else(|| {
            let name = field("NAME")?;
            Some(match field("VERSION") {
                Some(version) => format!("{name} {version}"),
                None => name,
            })
        })
    }

    /// The maker and product, as the firmware names them.
    pub(super) fn model(vendor: Option<&str>, product: Option<&str>) -> Option<String> {
        let words: Vec<&str> = [vendor, product]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|word| !word.is_empty())
            .collect();
        (!words.is_empty()).then(|| words.join(" "))
    }

    /// The first processor's model name.
    pub(super) fn cpu_model(cpuinfo: &str) -> Option<String> {
        cpuinfo.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == "model name").then(|| value.trim().to_owned())
        })
    }

    /// How many processors the machine runs at once.
    pub(super) fn cores(cpuinfo: &str) -> Option<u32> {
        let count = cpuinfo
            .lines()
            .filter(|line| {
                line.split_once(':')
                    .is_some_and(|(key, _)| key.trim() == "processor")
            })
            .count();
        u32::try_from(count).ok().filter(|&n| n > 0)
    }

    /// The machine's memory from meminfo, which counts in KiB.
    pub(super) fn memory(meminfo: &str) -> Option<u64> {
        meminfo.lines().find_map(|line| {
            let kib = line.strip_prefix("MemTotal:")?.trim().strip_suffix("kB")?;
            kib.trim().parse::<u64>().ok()?.checked_mul(1024)
        })
    }

    /// A power supply, as the kernel says it in its folder's files.
    pub(super) struct Supply {
        /// `Mains`, `USB`, `Battery` and the like.
        pub(super) kind: String,
        /// `Device` for a peripheral's own, such as a mouse's battery.
        pub(super) scope: String,
        /// Whether it supplies power now.
        pub(super) online: bool,
        /// A battery's charge, in percent.
        pub(super) capacity: Option<u8>,
    }

    /// What the machine runs on: its battery when it has one and nothing
    /// else supplies it, its batteries' charge being their mean.
    /// Peripherals' batteries are none of its own.
    pub(super) fn power(supplies: &[Supply]) -> Option<super::Power> {
        let own = |supply: &&Supply| supply.scope != "Device";
        let batteries: Vec<&Supply> = supplies
            .iter()
            .filter(own)
            .filter(|supply| supply.kind == "Battery")
            .collect();
        let plugged = supplies
            .iter()
            .filter(own)
            .any(|supply| supply.kind != "Battery" && supply.online);
        let charges: Vec<u32> = batteries
            .iter()
            .filter_map(|battery| battery.capacity.map(u32::from))
            .collect();
        let count = u32::try_from(charges.len()).ok()?;
        Some(super::Power {
            on_battery: !batteries.is_empty() && !plugged,
            charge: charges
                .iter()
                .sum::<u32>()
                .checked_div(count)
                .and_then(|mean| u8::try_from(mean).ok()),
        })
    }
}

/// Reading what macOS's tools say.
#[cfg_attr(
    not(any(target_os = "macos", test)),
    expect(dead_code, reason = "read on macOS only, tested everywhere")
)]
mod macos {
    /// The value `sysctl` gave for `key`, from lines of `key: value`.
    pub(super) fn sysctl(said: &str, key: &str) -> Option<String> {
        said.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            let value = value.trim();
            (name.trim() == key && !value.is_empty()).then(|| value.to_owned())
        })
    }

    /// What Apple calls the Mac's kind, such as `MacBook Air`, from what
    /// `system_profiler -json SPHardwareDataType` says.
    pub(super) fn model_name(said: &str) -> Option<String> {
        let said: serde_json::Value = serde_json::from_str(said).ok()?;
        let name = said["SPHardwareDataType"][0]["machine_name"]
            .as_str()?
            .trim();
        (!name.is_empty()).then(|| name.to_owned())
    }

    /// What the machine runs on, as `pmset -g batt` says it: what it
    /// draws from first, then a line for its battery, if it has one.
    pub(super) fn power(said: &str) -> Option<super::Power> {
        let mut lines = said.lines();
        let drawing = lines.next()?.strip_prefix("Now drawing from ")?;
        let charge = lines.find_map(|line| {
            let (_, after) = line.split_once("-InternalBattery")?;
            let (_, percent) = after.split_once('\t')?;
            percent.split_once('%')?.0.trim().parse().ok()
        });
        Some(super::Power {
            on_battery: drawing.contains("'Battery Power'"),
            charge,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_naming_a_band_this_build_doesnt_know_keeps_the_rest() {
        let mut said = serde_json::to_value(Machine {
            disk_band: Some(Band::Filling),
            ..measure("osprey", &Sources::nowhere())
        })
        .expect("a machine says itself");
        assert_eq!(said["disk_band"], "filling", "its band is said");
        said["disk_band"] = "overflowing".into();
        let heard: Machine = serde_json::from_value(said).expect("the machine is still heard");
        assert_eq!(heard.name, "osprey", "the rest of the machine stays");
        assert_eq!(heard.disk_band, None, "only the band it can't read goes");
    }

    #[test]
    fn a_kit_lists_the_tools_in_its_bin_in_order() {
        let folder = tempfile::tempdir().expect("a temporary kit");
        assert_eq!(
            kit(folder.path()),
            None,
            "a kit with no bin to list has tools unknown, so it is none"
        );
        let bin = folder.path().join("bin");
        std::fs::create_dir(&bin).expect("its bin");
        for tool in ["websocat", "gh", "git", ".keep"] {
            std::fs::write(bin.join(tool), "").expect("a tool");
        }
        let listed = kit(folder.path()).expect("a kit whose bin is listed");
        let resolved = std::fs::canonicalize(folder.path()).expect("the kit resolves");
        assert_eq!(
            listed,
            Kit {
                path: resolved.display().to_string(),
                tools: ["gh", "git", "websocat"].map(str::to_owned).to_vec(),
                refused: None,
            },
            "its tools, sorted, and no start-up file among them"
        );
        assert!(
            listed.holds("gh") && listed.holds("websocat"),
            "it holds each"
        );
        assert!(!listed.holds("gcloud"), "and only those");

        let link = tempfile::tempdir().expect("a folder for a link");
        let profile = link.path().join("profile");
        std::os::unix::fs::symlink(folder.path(), &profile).expect("a link to the kit");
        assert_eq!(
            kit(&profile).map(|linked| linked.path),
            Some(resolved.display().to_string()),
            "a kit given through a link names what it is now"
        );
    }

    #[test]
    fn a_machine_says_its_kit_only_where_it_has_one_as_an_older_node_reads_it() {
        let nowhere = Sources {
            home: "/".into(),
            queue: None,
            disk_guard: None,
            declared: None,
            reached: "/nowhere".into(),
            updated: "/nowhere".into(),
        };
        let mut machine = measure("osprey", &nowhere);
        let said = serde_json::to_value(&machine).expect("it says itself");
        assert!(
            said.get("kit").is_none(),
            "no kit is said as an older node says it"
        );
        let heard: Machine = serde_json::from_value(said).expect("an older record is heard");
        assert_eq!(heard.kit, None, "with no kit");

        machine.kit = Some(Kit {
            path: "/nix/store/kit".to_owned(),
            tools: vec!["gh".to_owned()],
            refused: None,
        });
        let said = serde_json::to_value(&machine).expect("it says itself");
        assert_eq!(
            said["kit"],
            serde_json::json!({"path": "/nix/store/kit", "tools": ["gh"]}),
            "a kit is said as PROTOCOL.md says it"
        );
        let heard: Machine = serde_json::from_value(said).expect("a record with a kit is heard");
        assert_eq!(heard.kit, machine.kit, "and heard as it was said");

        machine.kit = Some(Kit {
            path: "/home/kit".to_owned(),
            tools: Vec::new(),
            refused: Some("its bin may be written by others".to_owned()),
        });
        let said = serde_json::to_value(&machine).expect("it says itself");
        assert_eq!(
            said["kit"],
            serde_json::json!({
                "path": "/home/kit",
                "tools": [],
                "refused": "its bin may be written by others"
            }),
            "a refused kit is said with why, and no tools"
        );
        let heard: Machine = serde_json::from_value(said).expect("a refused kit is heard");
        assert_eq!(heard.kit, machine.kit, "and heard as it was said");
    }

    #[test]
    fn linux_says_what_it_is_in_its_own_words() {
        let release = "NAME=NixOS\nID=nixos\nVERSION=\"26.11 (Zokor)\"\nPRETTY_NAME=\"NixOS 26.11 (Zokor)\"\n";
        assert_eq!(
            linux::os(release).as_deref(),
            Some("NixOS 26.11 (Zokor)"),
            "the pretty name is taken"
        );
        assert_eq!(
            linux::os("NAME=Debian\nVERSION=\"13\"\n").as_deref(),
            Some("Debian 13"),
            "without one, the name and version"
        );
        assert_eq!(linux::os("ID=x\n"), None, "nothing named is unknown");

        assert_eq!(
            linux::model(Some("Dell Inc.\n"), Some("XPS 14 DA14260\n")).as_deref(),
            Some("Dell Inc. XPS 14 DA14260"),
            "maker and product"
        );
        assert_eq!(
            linux::model(None, Some(" \n")),
            None,
            "blank firmware is unknown"
        );

        let cpuinfo = "processor\t: 0\nmodel name\t: Intel(R) Core(TM) Ultra X7 358H\n\nprocessor\t: 1\nmodel name\t: Intel(R) Core(TM) Ultra X7 358H\n";
        assert_eq!(
            linux::cpu_model(cpuinfo).as_deref(),
            Some("Intel(R) Core(TM) Ultra X7 358H"),
            "the processor's name"
        );
        assert_eq!(linux::cores(cpuinfo), Some(2), "one per processor listed");
        assert_eq!(linux::cores(""), None, "none listed is unknown, not zero");

        assert_eq!(
            linux::memory("MemTotal:       65011712 kB\nMemFree: 1 kB\n"),
            Some(65_011_712 * 1024),
            "meminfo's KiB as bytes"
        );
    }

    #[test]
    fn a_machine_runs_on_its_battery_only_with_nothing_plugged_in() {
        let supply = |kind: &str, scope: &str, online, capacity| linux::Supply {
            kind: kind.to_owned(),
            scope: scope.to_owned(),
            online,
            capacity,
        };
        let power = |on_battery, charge| Some(Power { on_battery, charge });
        assert_eq!(
            linux::power(&[
                supply("Mains", "", true, None),
                supply("Battery", "", false, Some(56)),
                supply("USB", "System", true, None),
            ]),
            power(false, Some(56)),
            "a laptop plugged in, as skiff says it"
        );
        assert_eq!(
            linux::power(&[
                supply("Mains", "", false, None),
                supply("Battery", "", false, Some(80)),
                supply("Battery", "", false, Some(40)),
                supply("Battery", "Device", false, Some(5)),
                supply("USB", "System", false, None),
            ]),
            power(true, Some(60)),
            "unplugged, its own batteries' mean, not a mouse's"
        );
        assert_eq!(
            linux::power(&[supply("Battery", "Device", true, Some(5))]),
            power(false, None),
            "a desktop with a wireless mouse has no battery of its own"
        );

        let unplugged = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=27525219)\t73%; discharging; 5:12 remaining present: true\n";
        assert_eq!(
            macos::power(unplugged),
            power(true, Some(73)),
            "a Mac on its battery"
        );
        assert_eq!(
            macos::power("Now drawing from 'AC Power'\n"),
            power(false, None),
            "a Mac with no battery"
        );
        assert_eq!(macos::power(""), None, "nothing said is unknown");
    }

    #[test]
    fn a_mac_is_named_in_words_as_system_profiler_says() {
        let said = r#"{"SPHardwareDataType": [{"machine_model": "Mac14,2",
            "machine_name": "MacBook Air", "chip_type": "Apple M2"}]}"#;
        assert_eq!(
            macos::model_name(said).as_deref(),
            Some("MacBook Air"),
            "its kind in words, not its identifier"
        );
        assert_eq!(
            macos::model_name(r#"{"SPHardwareDataType": []}"#),
            None,
            "no hardware said is unknown"
        );
        assert_eq!(macos::model_name(""), None, "nothing said is unknown");
    }

    #[test]
    fn macos_says_what_it_is_through_sysctl() {
        let said = "hw.model: Mac16,8\nmachdep.cpu.brand_string: Apple M4 Pro\nhw.ncpu: 14\nhw.memsize: 51539607552\n";
        assert_eq!(
            macos::sysctl(said, "hw.model").as_deref(),
            Some("Mac16,8"),
            "a value by its key"
        );
        assert_eq!(
            macos::sysctl(said, "hw.ncpu").as_deref(),
            Some("14"),
            "a number as said"
        );
        assert_eq!(
            macos::sysctl(said, "hw.cpu"),
            None,
            "a key not said is unknown"
        );
    }

    #[test]
    fn this_machine_is_measured_without_guessing() {
        let home = tempfile::tempdir().expect("a temporary folder");
        let machine = measure(
            "trial",
            &Sources {
                home: home.path().to_owned(),
                queue: Some(home.path().join("queue.json")),
                disk_guard: Some(home.path().join("status.json")),
                declared: Some(home.path().join("declared.json")),
                reached: home.path().join("reached.json"),
                updated: home.path().join("last.json"),
            },
        );
        assert_eq!(machine.name, "trial", "it carries the name it was given");
        assert_eq!(
            machine.arch,
            std::env::consts::ARCH,
            "the architecture Rust was built for"
        );
        assert!(
            machine.disk.is_some_and(|bytes| bytes > 0),
            "a home's disk has a size"
        );
        assert!(
            machine
                .disk_free
                .zip(machine.disk)
                .is_some_and(|(free, size)| free <= size),
            "what's free of a home's disk is said, and no more than its size"
        );
        if cfg!(target_os = "linux") {
            assert!(
                machine.cpu.cores.is_some_and(|n| n > 0),
                "Linux lists its processors"
            );
            assert!(
                machine.memory.is_some_and(|bytes| bytes > 0),
                "Linux says its memory"
            );
        }
        let said = serde_json::to_value(&machine).expect("it serializes");
        assert!(
            said["os"].is_string() || said["os"].is_null(),
            "an unknown is said as null"
        );
        assert_eq!(
            said["state"], "ready",
            "a node answering for itself is ready"
        );
        assert!(
            said["room"].is_null(),
            "with no queue kept, its room is unknown"
        );
        assert!(
            said["declared"].is_null(),
            "with no configuration, nothing declared is known"
        );
    }

    #[test]
    fn a_machine_declaring_its_processors_is_taken_at_its_word() {
        let home = tempfile::tempdir().expect("a temporary folder");
        let declared = home.path().join("declared.json");
        std::fs::write(
            &declared,
            r#"{"reach": [], "outlives": [], "costs_money": true, "taints": [],
                "cores": 3}"#,
        )
        .expect("the configuration is written");
        let machine = measure(
            "trial",
            &Sources {
                home: home.path().to_owned(),
                queue: None,
                disk_guard: None,
                declared: Some(declared),
                reached: home.path().join("reached.json"),
                updated: home.path().join("last.json"),
            },
        );
        assert_eq!(
            machine.cpu.cores,
            Some(3),
            "the processors it is given, not those it lists"
        );
    }

    #[test]
    fn a_machine_within_another_has_its_hosts_power_while_its_host_says_it() {
        let home = tempfile::tempdir().expect("a temporary folder");
        let host_says = home.path().join("osprey.json");
        let declared = home.path().join("declared.json");
        std::fs::write(
            &declared,
            serde_json::json!({
                "reach": [], "outlives": [], "costs_money": false, "taints": [],
                "within": {"machine": "osprey", "file": host_says}
            })
            .to_string(),
        )
        .expect("declared");
        let sources = Sources {
            home: home.path().to_owned(),
            queue: None,
            disk_guard: None,
            declared: Some(declared),
            reached: home.path().join("reached.json"),
            updated: home.path().join("last.json"),
        };
        // A charge no machine running these tests reads of itself.
        let draining = Power {
            on_battery: true,
            charge: Some(37),
        };
        let says = |at| within::Said {
            at,
            power: Some(draining),
            room: None,
        };

        within::tell(&host_says, &says(jiff::Timestamp::now())).expect("told");
        assert_eq!(
            measure("osprey-linux", &sources).power,
            Some(draining),
            "it is on its battery while its host is"
        );
        assert_eq!(
            power(&sources),
            Some(draining),
            "and its power read alone says so too"
        );
        let long_ago = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1);
        within::tell(&host_says, &says(long_ago)).expect("told");
        assert_ne!(
            measure("osprey-linux", &sources).power,
            Some(draining),
            "and reads its own once its host has long said nothing"
        );
        assert_ne!(power(&sources), Some(draining), "read alone too");
    }

    #[test]
    fn a_machine_says_how_its_last_update_ended() {
        let home = tempfile::tempdir().expect("a temporary folder");
        let sources = Sources {
            home: home.path().to_owned(),
            queue: None,
            disk_guard: None,
            declared: Some(home.path().join("declared.json")),
            reached: home.path().join("reached.json"),
            updated: home.path().join("last.json"),
        };
        assert_eq!(
            measure("tern", &sources).updated,
            None,
            "unknown before its updater keeps an ending"
        );
        // Its updater says when it ended, so the file's time isn't asked.
        let done = serde_json::json!({"step": "done", "commit": "c0ffee",
            "applied": "next-boot", "restart": "a new kernel", "restarted": [],
            "at": "2026-11-08T23:40:00Z"});
        std::fs::write(&sources.updated, done.to_string()).expect("kept");
        assert_eq!(
            measure("tern", &sources).updated,
            Some(done),
            "then as its updater kept it, a restart it waits on included"
        );
    }

    #[test]
    fn a_realm_measures_nothing_a_machine_declares_of_another() {
        let home = tempfile::tempdir().expect("a temporary folder");
        let declared = home.path().join("declared.json");
        std::fs::write(
            &declared,
            r#"{"reach": [], "outlives": [], "costs_money": false, "taints": [],
                "agents": {"work": "pi", "personal": "pi-claude"}}"#,
        )
        .expect("the configuration is written");
        let sources = Sources {
            home: home.path().to_owned(),
            queue: None,
            disk_guard: None,
            declared: Some(declared),
            reached: home.path().join("reached.json"),
            updated: home.path().join("last.json"),
        };
        let agents = measure_in("skiff", "work", &sources)
            .declared
            .map(|said| said.agents);
        assert_eq!(
            agents,
            Some(BTreeMap::from([("work".to_owned(), "pi".to_owned())])),
            "the work realm hears what work's sessions start with, and of no other realm"
        );
    }
}
