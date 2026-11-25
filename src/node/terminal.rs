//! Whether a process asks from the person's own terminal, as a typed
//! answer's pi must (PROTOCOL.md's A Typed Answer): the foreground of a
//! terminal that the system's kitty serves, or that a holder keeps which
//! the person placed. A walk of forebears can't tell it, since a process
//! that calls setsid is taken in by the service manager and leaves no pi
//! above it, so this reads the terminal the kernel says a process is on,
//! who leads its session, and what binary that leader's parent runs.
//!
//! A binary is judged by the file the kernel says a process runs, owned by
//! root as the system's own are, never by its name, which any process can
//! change for itself. A holder counts only where the node kept, when it
//! was placed, that the person placed it ([`Placements`]): from their own
//! terminal, by this same rule, or from the launcher's own process. So an
//! agent's hold, a wake and a session carried in each mint nothing.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Who owns the system's binaries: root, as the store's files are.
pub const ROOT: u32 = 0;

/// The most placements kept: far more holders than a machine runs at
/// once, those that ended going first.
const PLACEMENTS_KEPT: usize = 1024;

/// What a process's `stat` file says of it that this weighs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    /// Its parent.
    pub parent: i32,
    /// Its process group.
    pub group: i32,
    /// Its session, by its leader's id.
    pub session: i32,
    /// The terminal it is on, as a device number; 0 for none.
    pub terminal: i32,
    /// The process group in its terminal's foreground.
    pub foreground: i32,
    /// When it started, in clock ticks since boot.
    pub started: u64,
}

/// What `stat` file text says, read past the name, which may itself hold
/// spaces and parentheses, so only the last `)` ends it.
#[must_use]
pub fn stat_of(text: &str) -> Option<Stat> {
    let (_, rest) = text.rsplit_once(')')?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let field = |at: usize| fields.get(at).and_then(|field| field.parse::<i64>().ok());
    Some(Stat {
        parent: i32::try_from(field(1)?).ok()?,
        group: i32::try_from(field(2)?).ok()?,
        session: i32::try_from(field(3)?).ok()?,
        terminal: i32::try_from(field(4)?).ok()?,
        foreground: i32::try_from(field(5)?).ok()?,
        started: u64::try_from(field(19)?).ok()?,
    })
}

/// Process `pid`'s `stat`, as `procs` tells it.
#[must_use]
pub fn stat(procs: &Path, pid: i32) -> Option<Stat> {
    stat_of(&fs::read_to_string(procs.join(pid.to_string()).join("stat")).ok()?)
}

/// A binary of the system's this tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binary {
    /// The terminal, kitty.
    Kitty,
    /// mycelium, whose holder keeps a held session's terminal.
    Mycelium,
    /// The launcher, Quickshell.
    Quickshell,
}

impl Binary {
    /// The binary a file of this name is, as the store names each and the
    /// wrapper Nix puts round one renames it.
    fn named(name: &str) -> Option<Self> {
        match name
            .strip_prefix('.')
            .and_then(|name| name.strip_suffix("-wrapped"))
        {
            Some(wrapped) => Self::bare(wrapped),
            None => Self::bare(name),
        }
    }

    fn bare(name: &str) -> Option<Self> {
        match name {
            "kitty" => Some(Self::Kitty),
            "mycelium" => Some(Self::Mycelium),
            "quickshell" => Some(Self::Quickshell),
            _ => None,
        }
    }
}

/// Which of the system's binaries process `pid` runs, as `procs` tells
/// it: by the file its `exe` names, which must be owned by `owner`, as
/// the system's own are by [`ROOT`]; none for a file of another name or
/// owner, one since removed, or one that can't be read.
#[must_use]
pub fn binary(procs: &Path, pid: i32, owner: u32) -> Option<Binary> {
    use std::os::unix::fs::MetadataExt as _;
    let exe = procs.join(pid.to_string()).join("exe");
    let file = fs::read_link(&exe).ok()?;
    let name = file.file_name()?.to_str()?;
    // The kernel says a file since removed with this after its path, and
    // one removed may have been anything.
    if name.ends_with(" (deleted)") {
        return None;
    }
    if fs::metadata(&exe).ok()?.uid() == owner {
        Binary::named(name)
    } else {
        None
    }
}

/// A hold the person placed: its holder, by its id and start, and the
/// program it held then, which leads its terminal's session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placed {
    /// The holder's process.
    pub holder: i32,
    /// When the holder started, in clock ticks since boot.
    pub holder_started: u64,
    /// The program it held when it was placed. A program it starts again,
    /// as a wake does, isn't this one, so it was placed by nobody.
    pub program: i32,
}

/// How the person's terminals are told: by binaries `owner` owns, as the
/// system's own are by [`ROOT`], and the holds the person `placed`.
#[derive(Debug, Clone, Copy)]
pub struct Terminals<'a> {
    /// Who owns the system's binaries.
    pub owner: u32,
    /// The holds the person placed.
    pub placed: &'a [Placed],
}

/// Whether process `pid` asks from the foreground of the person's own
/// terminal, as `procs` tells it, its binaries owned by `owner`: one the
/// system's kitty serves, or one a holder keeps that `placed` names.
///
/// # Errors
///
/// Where it isn't, or can't be read, saying why.
pub fn persons_terminal(
    procs: &Path,
    pid: i32,
    owner: u32,
    placed: &[Placed],
) -> Result<(), String> {
    let unread = || "the terminal it asks from can't be read".to_owned();
    let asking = stat(procs, pid).ok_or_else(unread)?;
    if asking.terminal == 0 {
        return Err("it runs on no terminal".to_owned());
    }
    if asking.foreground != asking.group {
        return Err("it isn't in its terminal's foreground".to_owned());
    }
    let leader = stat(procs, asking.session).ok_or_else(unread)?;
    if leader.terminal != asking.terminal {
        return Err("its session's leader isn't on its terminal".to_owned());
    }
    match binary(procs, leader.parent, owner) {
        Some(Binary::Kitty) => Ok(()),
        Some(Binary::Mycelium) => {
            let holder = stat(procs, leader.parent).ok_or_else(unread)?;
            let held = Placed {
                holder: leader.parent,
                holder_started: holder.started,
                program: asking.session,
            };
            if placed.contains(&held) {
                Ok(())
            } else {
                Err("the holder of its terminal wasn't placed by the person".to_owned())
            }
        }
        Some(Binary::Quickshell) | None => {
            Err("its terminal is served by neither the system's kitty nor a holder".to_owned())
        }
    }
}

/// Whether process `pid` is one the launcher started itself, as `procs`
/// tells it, its binaries owned by `owner`: its parent runs the system's
/// Quickshell.
#[must_use]
pub fn launchers_own(procs: &Path, pid: i32, owner: u32) -> bool {
    stat(procs, pid)
        .is_some_and(|asking| binary(procs, asking.parent, owner) == Some(Binary::Quickshell))
}

/// The holds the person placed this boot, kept in a file so a node that
/// starts again still knows them, and forgotten at a boot, when every
/// holder has gone.
pub struct Placements {
    file: PathBuf,
    boot: Option<String>,
    kept: Mutex<Vec<Placed>>,
}

/// What [`Placements`] keeps in its file.
#[derive(Serialize, Deserialize)]
struct Kept {
    boot: Option<String>,
    placed: Vec<Placed>,
}

impl Placements {
    /// Those kept in `file` for boot `boot`; none where it names another
    /// boot, or none, or can't be read.
    #[must_use]
    pub fn read(file: PathBuf, boot: Option<String>) -> Self {
        let placed = fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str::<Kept>(&text).ok())
            .filter(|kept| boot.is_some() && kept.boot == boot)
            .map(|kept| kept.placed)
            .unwrap_or_default();
        Self {
            file,
            boot,
            kept: Mutex::new(placed),
        }
    }

    /// Those kept now.
    #[must_use]
    pub fn placed(&self) -> Vec<Placed> {
        self.kept
            .lock()
            .map(|kept| kept.clone())
            .unwrap_or_default()
    }

    /// Keeps `placed`, forgetting any whose holder no longer runs as
    /// `procs` tells it, and writes them all down.
    ///
    /// # Errors
    ///
    /// Where they can't be written; the placement is still kept until the
    /// node ends.
    pub fn keep(&self, placed: Placed, procs: &Path) -> Result<(), String> {
        let mut kept = self
            .kept
            .lock()
            .map_err(|_| "the placements were left half-changed".to_owned())?;
        kept.retain(|one| {
            one.holder != placed.holder
                && stat(procs, one.holder)
                    .is_some_and(|holder| holder.started == one.holder_started)
        });
        kept.push(placed);
        let over = kept.len().saturating_sub(PLACEMENTS_KEPT);
        kept.drain(..over);
        let text = serde_json::to_string(&Kept {
            boot: self.boot.clone(),
            placed: kept.clone(),
        })
        .map_err(|error| format!("the placements can't be said: {error}"))?;
        drop(kept);
        write_whole(&self.file, &text)
            .map_err(|error| format!("the placements can't be written: {error}"))
    }
}

/// Writes `text` to `file` whole, through a file beside it renamed into
/// place, so a reader never finds half of it.
fn write_whole(file: &Path, text: &str) -> std::io::Result<()> {
    let mut beside = file.as_os_str().to_owned();
    beside.push(format!(".{}", std::process::id()));
    let beside = PathBuf::from(beside);
    let mut written = fs::File::create(&beside)?;
    written.write_all(text.as_bytes())?;
    written.sync_all()?;
    fs::rename(&beside, file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt as _;

    /// Who owns the files a test makes, standing in for root.
    fn me() -> u32 {
        let made = tempfile::NamedTempFile::new().expect("a file is made");
        fs::metadata(made.path()).expect("it is read").uid()
    }

    /// A stand-in for `/proc` and the binaries its processes run.
    struct Machine {
        procs: tempfile::TempDir,
        binaries: tempfile::TempDir,
    }

    impl Machine {
        fn new() -> Self {
            Self {
                procs: tempfile::tempdir().expect("a stand-in for /proc"),
                binaries: tempfile::tempdir().expect("a stand-in for the store"),
            }
        }

        fn procs(&self) -> &Path {
            self.procs.path()
        }

        /// Process `pid`, running the binary named `runs`, its parent,
        /// group, session, terminal and foreground group, started at
        /// `ticks`, as `stat` and `exe` tell it.
        fn runs(
            &self,
            pid: i32,
            runs: &str,
            (parent, group, session): (i32, i32, i32),
            on: (i32, i32),
            ticks: u64,
        ) {
            let (terminal, foreground) = on;
            let process = self.procs().join(pid.to_string());
            fs::create_dir_all(&process).expect("its folder is made");
            let between = "0 ".repeat(13);
            fs::write(
                process.join("stat"),
                format!(
                    "{pid} (a (name) like this) S {parent} {group} {session} {terminal} \
                     {foreground} {between}{ticks} 0 0\n"
                ),
            )
            .expect("its stat is written");
            let file = self.binaries.path().join(runs);
            fs::write(&file, "").expect("the binary is made");
            let _ = fs::remove_file(process.join("exe"));
            std::os::unix::fs::symlink(&file, process.join("exe")).expect("linked");
        }
    }

    /// A terminal's device number.
    const TTY: i32 = 34_816;

    #[test]
    fn a_stat_is_read_past_a_name_holding_spaces_and_parentheses() {
        assert_eq!(
            stat_of("58 (pi (x) y) S 41 58 58 34816 58 0 0 0 0 0 0 0 0 0 0 0 0 0 500 0 0"),
            Some(Stat {
                parent: 41,
                group: 58,
                session: 58,
                terminal: TTY,
                foreground: 58,
                started: 500,
            })
        );
        assert_eq!(stat_of("58 (pi) S 41"), None, "a short one says nothing");
    }

    #[test]
    fn a_pi_in_the_foreground_of_kittys_terminal_asks_from_the_persons_own() {
        let machine = Machine::new();
        machine.runs(40, ".kitty-wrapped", (1, 40, 40), (0, -1), 100);
        machine.runs(41, "zsh", (40, 41, 41), (TTY, 58), 200);
        machine.runs(58, "pi", (41, 58, 41), (TTY, 58), 500);
        assert_eq!(persons_terminal(machine.procs(), 58, me(), &[]), Ok(()));
    }

    #[test]
    fn a_pi_in_the_background_or_on_no_terminal_asks_from_none_of_the_persons() {
        let machine = Machine::new();
        machine.runs(40, "kitty", (1, 40, 40), (0, -1), 100);
        machine.runs(41, "zsh", (40, 41, 41), (TTY, 41), 200);
        machine.runs(58, "pi", (41, 58, 41), (TTY, 41), 500);
        machine.runs(59, "pi", (1, 59, 59), (0, -1), 600);
        let judged = |pid| persons_terminal(machine.procs(), pid, me(), &[]);
        assert_eq!(
            judged(58),
            Err("it isn't in its terminal's foreground".to_owned())
        );
        assert_eq!(judged(59), Err("it runs on no terminal".to_owned()));
    }

    #[test]
    fn a_pi_that_called_setsid_leads_a_session_no_kitty_serves() {
        let machine = Machine::new();
        // It took a terminal of its own, as script(1) gives one, and the
        // service manager took it in.
        machine.runs(1, "systemd", (0, 1, 1), (0, -1), 1);
        machine.runs(58, "pi", (1, 58, 58), (TTY, 58), 500);
        machine.runs(70, "script", (1, 70, 70), (0, -1), 600);
        machine.runs(71, "pi", (70, 71, 71), (TTY + 1, 71), 700);
        let judged = |pid| persons_terminal(machine.procs(), pid, me(), &[]);
        let neither =
            Err("its terminal is served by neither the system's kitty nor a holder".to_owned());
        assert_eq!(judged(58), neither);
        assert_eq!(judged(71), neither);
    }

    #[test]
    fn a_kitty_by_name_alone_or_owned_by_another_serves_nothing() {
        let machine = Machine::new();
        // An agent's program that renamed itself, and a real kitty's file
        // owned by an account other than the system's.
        machine.runs(40, "agents-program", (1, 40, 40), (0, -1), 100);
        fs::write(machine.procs().join("40").join("comm"), ".kitty-wrapped\n").expect("named");
        machine.runs(58, "pi", (40, 58, 58), (TTY, 58), 500);
        assert!(persons_terminal(machine.procs(), 58, me(), &[]).is_err());
        machine.runs(40, "kitty", (1, 40, 40), (0, -1), 100);
        assert_eq!(persons_terminal(machine.procs(), 58, me(), &[]), Ok(()));
        assert!(persons_terminal(machine.procs(), 58, me() + 1, &[]).is_err());
    }

    #[test]
    fn a_held_pi_asks_from_the_persons_own_only_where_the_person_placed_its_hold() {
        let machine = Machine::new();
        machine.runs(30, "mycelium", (1, 30, 30), (0, -1), 300);
        machine.runs(58, "pi", (30, 58, 58), (TTY, 58), 500);
        let placed = Placed {
            holder: 30,
            holder_started: 300,
            program: 58,
        };
        let judged = |placed: &[Placed]| persons_terminal(machine.procs(), 58, me(), placed);
        assert_eq!(judged(&[placed]), Ok(()));
        let unplaced = Err("the holder of its terminal wasn't placed by the person".to_owned());
        assert_eq!(judged(&[]), unplaced, "an agent's hold is placed by nobody");
        let woken = Placed {
            program: 57,
            ..placed
        };
        assert_eq!(
            judged(&[woken]),
            unplaced,
            "a program it started again wasn't placed"
        );
        let earlier = Placed {
            holder_started: 299,
            ..placed
        };
        assert_eq!(
            judged(&[earlier]),
            unplaced,
            "a holder by an old one's id wasn't placed"
        );
    }

    #[test]
    fn the_launchers_own_process_is_one_quickshell_started() {
        let machine = Machine::new();
        machine.runs(20, ".quickshell-wrapped", (1, 20, 20), (0, -1), 100);
        machine.runs(21, "mycelium", (20, 21, 21), (0, -1), 200);
        machine.runs(22, "mycelium", (1, 22, 22), (0, -1), 300);
        assert!(launchers_own(machine.procs(), 21, me()));
        assert!(!launchers_own(machine.procs(), 22, me()));
        assert!(!launchers_own(machine.procs(), 21, me() + 1));
    }

    #[test]
    fn placements_are_kept_through_a_restart_and_forgotten_at_a_boot() {
        let machine = Machine::new();
        machine.runs(30, "mycelium", (1, 30, 30), (0, -1), 300);
        machine.runs(31, "mycelium", (1, 31, 31), (0, -1), 310);
        let file = machine.binaries.path().join("placed.json");
        let boot = Some("boot-a".to_owned());
        let placements = Placements::read(file.clone(), boot.clone());
        let first = Placed {
            holder: 30,
            holder_started: 300,
            program: 58,
        };
        let gone = Placed {
            holder: 32,
            holder_started: 320,
            program: 60,
        };
        let second = Placed {
            holder: 31,
            holder_started: 310,
            program: 59,
        };
        for placed in [first, gone, second] {
            placements.keep(placed, machine.procs()).expect("kept");
        }
        assert_eq!(
            Placements::read(file.clone(), boot).placed(),
            vec![first, second],
            "one whose holder ended is forgotten, the rest read again"
        );
        assert!(
            Placements::read(file.clone(), Some("boot-b".to_owned()))
                .placed()
                .is_empty()
        );
        assert!(Placements::read(file, None).placed().is_empty());
    }
}
