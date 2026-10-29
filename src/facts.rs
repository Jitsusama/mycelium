//! What this machine is, as its node measures it: the facts work can be
//! placed by: what can be read from the machine itself, the room its own
//! queue says it has, and what its configuration declares. A fact that
//! can't be read is unknown, never a guess.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::declared::{self, Declared};
use crate::place;
use crate::room::{self, Room};

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
    /// Its architecture, as Rust names it: `x86_64` or `aarch64`.
    pub arch: String,
    /// Its maker's name for it, such as "Dell Inc. XPS 14 DA14260".
    pub model: Option<String>,
    /// Its processor.
    pub cpu: Cpu,
    /// Its memory, in bytes.
    pub memory: Option<u64>,
    /// The size of the disk holding its account's home, in bytes.
    pub disk: Option<u64>,
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
}

/// Where a machine's facts are read from, beyond the machine itself.
#[derive(Debug, Clone)]
pub struct Sources {
    /// Its account's home, whose disk is the one measured.
    pub home: PathBuf,
    /// Its queue's record, if it keeps one anywhere.
    pub queue: Option<PathBuf>,
    /// What its configuration declares.
    pub declared: PathBuf,
}

impl Sources {
    /// Where this machine's account keeps each.
    #[must_use]
    pub fn here() -> Self {
        Self {
            home: std::env::home_dir().unwrap_or_else(std::env::temp_dir),
            queue: place::queue(),
            declared: place::declared(),
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cpu {
    /// Its name, such as "Intel(R) Core(TM) Ultra X7 358H".
    pub model: Option<String>,
    /// How many it runs at once: the machine's, not what this process may
    /// use.
    pub cores: Option<u32>,
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

/// Measures this machine, named `name`, reading the rest from `sources`.
#[must_use]
pub fn measure(name: &str, sources: &Sources) -> Machine {
    let read = here::read();
    let now = jiff::Timestamp::now();
    Machine {
        name: name.to_owned(),
        measured_at: now.to_string(),
        state: Readiness::Ready,
        os: read.os,
        arch: std::env::consts::ARCH.to_owned(),
        model: read.model,
        cpu: read.cpu,
        memory: read.memory,
        disk: disk_of(&sources.home),
        accelerators: read.accelerators,
        power: read.power,
        room: sources
            .queue
            .as_deref()
            .and_then(|queue| room::read(queue, now.as_second())),
        declared: declared::read(&sources.declared),
    }
}

/// What the system's own files or tools say, before the parts every
/// system reads alike.
struct Read {
    os: Option<String>,
    model: Option<String>,
    cpu: Cpu,
    memory: Option<u64>,
    accelerators: Option<Vec<Accelerator>>,
    power: Option<Power>,
}

fn disk_of(home: &Path) -> Option<u64> {
    let stat = rustix::fs::statvfs(home).ok()?;
    stat.f_blocks.checked_mul(stat.f_frsize)
}

#[cfg(target_os = "linux")]
mod here {
    //! Linux says what it is in /etc, /proc and /sys.

    use std::fs;

    use super::{Accelerator, AcceleratorKind, Cpu, Read, linux};

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
            cpu: Cpu {
                model: linux::cpu_model(&cpuinfo),
                cores: linux::cores(&cpuinfo),
            },
            memory: fs::read_to_string("/proc/meminfo")
                .ok()
                .and_then(|text| linux::memory(&text)),
            accelerators: Some(
                drivers("/sys/class/drm", "card", AcceleratorKind::Gpu)
                    .chain(drivers("/sys/class/accel", "accel", AcceleratorKind::Npu))
                    .collect(),
            ),
            power: supplies().and_then(|supplies| linux::power(&supplies)),
        }
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

    use std::process::Command;

    use super::{Cpu, Read, macos};

    pub(super) fn read() -> Read {
        let said = |program: &str, args: &[&str]| {
            Command::new(program)
                .args(args)
                .output()
                .ok()
                .filter(|out| out.status.success())
                .and_then(|out| String::from_utf8(out.stdout).ok())
        };
        let sysctl = said(
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
            os: said("/usr/bin/sw_vers", &["-productVersion"])
                .map(|version| format!("macOS {}", version.trim())),
            model: value("hw.model"),
            cpu: Cpu {
                model: value("machdep.cpu.brand_string"),
                cores: value("hw.ncpu").and_then(|n| n.parse().ok()),
            },
            memory: value("hw.memsize").and_then(|n| n.parse().ok()),
            // A Mac's graphics and neural engines can't be listed without
            // a binding this crate doesn't have.
            accelerators: None,
            power: said("/usr/bin/pmset", &["-g", "batt"]).and_then(|said| macos::power(&said)),
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod here {
    //! Elsewhere, only what Rust itself knows.

    use super::{Cpu, Read};

    pub(super) fn read() -> Read {
        Read {
            os: None,
            model: None,
            cpu: Cpu {
                model: None,
                cores: None,
            },
            memory: None,
            accelerators: None,
            power: None,
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
            "a laptop plugged in, as optimus says it"
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
                declared: home.path().join("declared.json"),
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
}
