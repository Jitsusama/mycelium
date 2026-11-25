//! Whether a process asks from the person's own terminal, as a typed
//! answer's pi must (PROTOCOL.md's A Typed Answer): the foreground of a
//! terminal that the system's kitty serves, or that a holder keeps which
//! the person placed. A walk of forebears can't tell it, since a process
//! that calls setsid is taken in by the service manager and leaves no pi
//! above it, so this reads the terminal the kernel says a process is on,
//! who leads its session, that it reaches that leader through its parents
//! without leaving the session, and what binary that leader's parent runs.
//!
//! A binary counts as the system's only where one of the system's
//! generations names its folder ([`super::binaries`]). A holder counts only where
//! the node kept, when it was placed, that the person placed it
//! ([`Placements`]): from their own terminal, by this same rule, or from
//! the launcher's own process. So an agent's hold, a wake and a session
//! carried in each mint nothing.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::binaries::{Binary, System, binary};

/// The most parents walked from a process to its session's leader: far
/// more than a shell's commands nest.
const DEEPEST: usize = 64;

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

/// A hold the person placed: its holder, by its id and start, and the
/// program it held then, by its id and start, which leads its terminal's
/// session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placed {
    /// The holder's process.
    pub holder: i32,
    /// When the holder started, in clock ticks since boot.
    pub holder_started: u64,
    /// The program it held when it was placed. A program it starts again,
    /// as a wake does, isn't this one, so it was placed by nobody.
    pub program: i32,
    /// When the program started, so another by a reused id isn't it.
    pub program_started: u64,
}

/// How the person's terminals are told: by the binaries `system` names,
/// and the holds the person `placed`.
#[derive(Debug, Clone, Copy)]
pub struct Terminals<'a> {
    /// The system's binaries.
    pub system: &'a System,
    /// The holds the person placed.
    pub placed: &'a [Placed],
}

/// Whether process `pid` asks from the foreground of the person's own
/// terminal, as `procs` tells it, by the binaries `terminals` names: one
/// the system's kitty serves, or one a holder keeps that the person
/// placed. It must reach its session's leader through its parents, each
/// in that session, so one that forked away from what started it, left
/// in its group with the service manager above it, is refused.
///
/// # Errors
///
/// Where it isn't, or can't be read, saying why.
pub fn persons_terminal(procs: &Path, pid: i32, terminals: Terminals<'_>) -> Result<(), String> {
    let unread = || "the terminal it asks from can't be read".to_owned();
    let asking = stat(procs, pid).ok_or_else(unread)?;
    if asking.terminal == 0 {
        return Err("it runs on no terminal".to_owned());
    }
    if asking.foreground != asking.group {
        return Err("it isn't in its terminal's foreground".to_owned());
    }
    reaches_its_leader(procs, pid, asking.session)?;
    let leader = stat(procs, asking.session).ok_or_else(unread)?;
    if leader.terminal != asking.terminal {
        return Err("its session's leader isn't on its terminal".to_owned());
    }
    match binary(procs, leader.parent, terminals.system) {
        Some(Binary::Kitty) => Ok(()),
        Some(Binary::Mycelium) => {
            let holder = stat(procs, leader.parent).ok_or_else(unread)?;
            let held = Placed {
                holder: leader.parent,
                holder_started: holder.started,
                program: asking.session,
                program_started: leader.started,
            };
            if terminals.placed.contains(&held) {
                Ok(())
            } else {
                Err("the holder of its terminal wasn't placed by the person".to_owned())
            }
        }
        Some(Binary::Quickshell) | None => Err("its terminal is served by neither the system's \
                                                 kitty nor a holder, as the system's \
                                                 generations name them"
            .to_owned()),
    }
}

/// Whether `pid` reaches `leader` through its parents, each in the
/// session `leader` leads.
fn reaches_its_leader(procs: &Path, pid: i32, leader: i32) -> Result<(), String> {
    let mut at = pid;
    for _ in 0..DEEPEST {
        if at == leader {
            return Ok(());
        }
        let stat =
            stat(procs, at).ok_or("a process between it and its session's leader can't be read")?;
        if stat.session != leader {
            return Err(
                "it left what started it, so it reaches its session's leader through no \
                        parent"
                    .to_owned(),
            );
        }
        at = stat.parent;
    }
    Err("it is too far below its session's leader to be told".to_owned())
}

/// The placement of hold `id` by process `asker`, as `procs` tells it,
/// from the hold records `held`: exactly one record names it, so none
/// written beside it stands in; its holder runs a mycelium `system` names
/// and started no earlier than the asker, which started it; and its
/// program runs, a child of that holder. A record any process of the
/// account could write so names only a hold the asker itself began.
///
/// # Errors
///
/// Where any of that doesn't hold, or can't be read, saying which.
pub fn placement_of(
    procs: &Path,
    system: &System,
    held: &[crate::hold::Held],
    id: &str,
    asker: i32,
) -> Result<Placed, String> {
    let mut named = held.iter().filter(|one| one.id == id);
    let (Some(one), None) = (named.next(), named.next()) else {
        return Err(format!(
            "{id} is named by no hold record here, or by more than one"
        ));
    };
    let (Ok(holder), Ok(program)) = (i32::try_from(one.holder), i32::try_from(one.pid)) else {
        return Err(format!("{id}'s holder or program can't be named"));
    };
    if program == 0 || one.parked {
        return Err(format!("{id} is parked, so its program wasn't placed"));
    }
    let unread = || format!("{id}'s holder, its program or the one placing it can't be read");
    let (asking, holding, running) = (
        stat(procs, asker).ok_or_else(unread)?,
        stat(procs, holder).ok_or_else(unread)?,
        stat(procs, program).ok_or_else(unread)?,
    );
    if binary(procs, holder, system) != Some(Binary::Mycelium) {
        return Err(format!("{id}'s holder runs no mycelium of the system's"));
    }
    if running.parent != holder {
        return Err(format!("{id}'s program isn't its holder's own"));
    }
    if holding.started < asking.started {
        return Err(format!(
            "{id}'s holder started before the one placing it, so it began another's hold"
        ));
    }
    Ok(Placed {
        holder,
        holder_started: holding.started,
        program,
        program_started: running.started,
    })
}

/// Whether process `pid` is one the launcher started itself, as `procs`
/// tells it, by the binaries `system` names: its parent runs the system's
/// Quickshell.
#[must_use]
pub fn launchers_own(procs: &Path, pid: i32, system: &System) -> bool {
    stat(procs, pid)
        .is_some_and(|asking| binary(procs, asking.parent, system) == Some(Binary::Quickshell))
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
        // Written while the lock is held, so two keeps at once write one
        // after the other, the later holding both.
        let written = write_whole(&self.file, &text)
            .map_err(|error| format!("the placements can't be written: {error}"));
        drop(kept);
        written
    }
}

/// Writes `text` to `file` whole, through a file beside it of its own,
/// made new, renamed into place, so a reader never finds half of it.
fn write_whole(file: &Path, text: &str) -> std::io::Result<()> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let mut beside = file.as_os_str().to_owned();
    let write = WRITES.fetch_add(1, Ordering::Relaxed);
    beside.push(format!(".{}.{write}", std::process::id()));
    let beside = PathBuf::from(beside);
    let mut written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&beside)?;
    let done = written
        .write_all(text.as_bytes())
        .and_then(|()| written.sync_all())
        .and_then(|()| fs::rename(&beside, file));
    if done.is_err() {
        let _ = fs::remove_file(&beside);
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    /// Who owns the files a test makes, standing in for root.
    fn me() -> u32 {
        let made = tempfile::NamedTempFile::new().expect("a file is made");
        fs::metadata(made.path()).expect("it is read").uid()
    }

    /// A stand-in for `/proc` and the binaries its processes run, each in
    /// a folder the system names for it.
    struct Machine {
        procs: tempfile::TempDir,
        binaries: tempfile::TempDir,
        system: System,
    }

    impl Machine {
        fn new() -> Self {
            let binaries = tempfile::tempdir().expect("a stand-in for the store");
            let system = Binary::ALL
                .into_iter()
                .fold(System::default(), |system, binary| {
                    system.with(binary, binaries.path().to_owned())
                });
            Self {
                procs: tempfile::tempdir().expect("a stand-in for /proc"),
                binaries,
                system,
            }
        }

        fn procs(&self) -> &Path {
            self.procs.path()
        }

        fn terminals<'a>(&'a self, placed: &'a [Placed]) -> Terminals<'a> {
            Terminals {
                system: &self.system,
                placed,
            }
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
        assert_eq!(
            persons_terminal(machine.procs(), 58, machine.terminals(&[])),
            Ok(())
        );
    }

    #[test]
    fn one_that_forked_away_from_what_started_it_reaches_its_leader_through_no_parent() {
        let machine = Machine::new();
        machine.runs(1, "systemd", (0, 1, 1), (0, -1), 1);
        machine.runs(40, "kitty", (1, 40, 40), (0, -1), 100);
        machine.runs(41, "zsh", (40, 41, 41), (TTY, 58), 200);
        machine.runs(58, "pi", (41, 58, 41), (TTY, 58), 500);
        // 59 a command 58 ran in its own group; 61 a process that forked
        // twice from under 58 without setsid, left in its group and
        // session with the service manager its parent.
        machine.runs(59, "bash", (58, 58, 41), (TTY, 58), 510);
        machine.runs(61, "mycelium", (1, 58, 41), (TTY, 58), 520);
        let judged = |pid| persons_terminal(machine.procs(), pid, machine.terminals(&[]));
        assert_eq!(
            judged(59),
            Ok(()),
            "the terminal's rule alone; pi above is the asker's"
        );
        assert!(
            judged(61).is_err_and(|why| why.contains("left what started it")),
            "{:?}",
            judged(61)
        );
    }

    #[test]
    fn a_pi_in_the_background_or_on_no_terminal_asks_from_none_of_the_persons() {
        let machine = Machine::new();
        machine.runs(40, "kitty", (1, 40, 40), (0, -1), 100);
        machine.runs(41, "zsh", (40, 41, 41), (TTY, 41), 200);
        machine.runs(58, "pi", (41, 58, 41), (TTY, 41), 500);
        machine.runs(59, "pi", (1, 59, 59), (0, -1), 600);
        let judged = |pid| persons_terminal(machine.procs(), pid, machine.terminals(&[]));
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
        let judged = |pid| persons_terminal(machine.procs(), pid, machine.terminals(&[]));
        for pid in [58, 71] {
            assert!(
                judged(pid).is_err_and(|why| why.contains("served by neither")),
                "{pid}: {:?}",
                judged(pid)
            );
        }
    }

    #[test]
    fn a_kitty_by_name_alone_or_in_a_folder_the_system_names_for_none_serves_nothing() {
        let machine = Machine::new();
        // An agent's program that renamed itself, and a file named kitty
        // in a folder of the store no generation names, as any account
        // can put there.
        machine.runs(40, "agents-program", (1, 40, 40), (0, -1), 100);
        fs::write(machine.procs().join("40").join("comm"), ".kitty-wrapped\n").expect("named");
        machine.runs(58, "pi", (40, 58, 58), (TTY, 58), 500);
        let judged = || persons_terminal(machine.procs(), 58, machine.terminals(&[]));
        assert!(judged().is_err());
        machine.runs(40, "kitty", (1, 40, 40), (0, -1), 100);
        assert_eq!(judged(), Ok(()));
        let planted = machine.binaries.path().join("planted");
        fs::create_dir(&planted).expect("a folder of its own");
        fs::write(planted.join("kitty"), "").expect("a kitty of its own");
        let exe = machine.procs().join("40").join("exe");
        fs::remove_file(&exe).expect("unlinked");
        std::os::unix::fs::symlink(planted.join("kitty"), &exe).expect("linked");
        assert!(
            judged().is_err(),
            "a kitty no generation names is not the system's"
        );
    }

    #[test]
    fn the_system_names_the_folders_its_generations_profiles_name_and_no_other() {
        let root = tempfile::tempdir().expect("a stand-in for the store");
        let store = root.path().join("store");
        let profiles = root.path().join("profiles");
        // Two generations, the older's kitty another build; a profile's
        // bin names its binary by a link into the store, as Nix's do.
        let package = |name: &str, file: &str| {
            let bin = store.join(name).join("bin");
            fs::create_dir_all(&bin).expect("a package");
            fs::write(bin.join(file), "").expect("its binary");
            bin
        };
        let (kitty_new, kitty_old) = (package("k2", "kitty"), package("k1", "kitty"));
        let quickshell = package("q", "quickshell");
        let unnamed = package("planted", "kitty");
        let generation = |name: &str, sw: &Path, account: (&str, &Path, &str)| {
            let made = store.join(format!("{name}-system"));
            fs::create_dir_all(made.join("sw/bin")).expect("its system profile");
            std::os::unix::fs::symlink(sw.join("kitty"), made.join("sw/bin/kitty")).expect("kitty");
            let (who, package, file) = account;
            let bin = made.join("etc/profiles/per-user").join(who).join("bin");
            fs::create_dir_all(&bin).expect("an account's profile");
            std::os::unix::fs::symlink(package.join(file), bin.join(file)).expect("linked");
            made
        };
        fs::create_dir(&profiles).expect("the profiles");
        fs::set_permissions(&profiles, fs::Permissions::from_mode(0o755)).expect("closed");
        let newer = generation("2", &kitty_new, ("skiff", &quickshell, "quickshell"));
        let older = generation("1", &kitty_old, ("tern", &quickshell, "quickshell"));
        std::os::unix::fs::symlink(&newer, profiles.join("system-2-link")).expect("linked");
        std::os::unix::fs::symlink(&older, profiles.join("system-1-link")).expect("linked");
        std::os::unix::fs::symlink(store.join("planted"), profiles.join("other")).expect("linked");
        let system = System::read(&profiles, me()).expect("read");
        for (binary, folder) in [
            (Binary::Kitty, &kitty_new),
            (Binary::Kitty, &kitty_old),
            (Binary::Quickshell, &quickshell),
        ] {
            let folder = fs::canonicalize(folder).expect("canonical");
            assert!(
                system.holds(binary, &folder),
                "{binary:?} in {}",
                folder.display()
            );
        }
        let unnamed = fs::canonicalize(unnamed).expect("canonical");
        assert!(
            !system.holds(Binary::Kitty, &unnamed),
            "no generation names it"
        );
        assert!(
            System::read(&profiles, me() + 1).is_err(),
            "profiles another owns"
        );
        fs::set_permissions(&profiles, fs::Permissions::from_mode(0o777)).expect("opened");
        assert!(
            System::read(&profiles, me()).is_err(),
            "profiles anyone may change"
        );
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
            program_started: 500,
        };
        let judged =
            |placed: &[Placed]| persons_terminal(machine.procs(), 58, machine.terminals(placed));
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
        let reused = Placed {
            program_started: 499,
            ..placed
        };
        assert_eq!(
            judged(&[reused]),
            unplaced,
            "a program by an old one's id wasn't placed"
        );
    }

    #[test]
    fn a_placement_names_only_a_hold_the_one_placing_it_began() {
        let machine = Machine::new();
        // 45 places; 30 holds 31, started after it; 32 a holder started
        // before 45; 34 runs no mycelium; 36's record names a program not
        // its own.
        machine.runs(45, "mycelium", (41, 45, 41), (TTY, 45), 450);
        machine.runs(30, "mycelium", (1, 30, 30), (0, -1), 460);
        machine.runs(31, "pi", (30, 31, 31), (TTY + 1, 31), 470);
        machine.runs(32, "mycelium", (1, 32, 32), (0, -1), 400);
        machine.runs(33, "pi", (32, 33, 33), (TTY + 2, 33), 480);
        machine.runs(34, "agents-program", (1, 34, 34), (0, -1), 460);
        machine.runs(35, "pi", (34, 35, 35), (TTY + 3, 35), 480);
        machine.runs(36, "mycelium", (1, 36, 36), (0, -1), 460);
        let record = |id: &str, holder: u32, pid: u32| crate::hold::Held {
            id: id.to_owned(),
            command: vec!["pi".to_owned()],
            folder: PathBuf::from("/"),
            holder,
            pid,
            parked: false,
            since: String::new(),
            windows: Vec::new(),
        };
        let records = [
            record("a1", 30, 31),
            record("b2", 32, 33),
            record("c3", 34, 35),
            record("d4", 36, 31),
            record("e5", 30, 31),
            record("e5", 36, 31),
            record("f6", 30, 0),
        ];
        let placed = |id| placement_of(machine.procs(), &machine.system, &records, id, 45);
        assert_eq!(
            placed("a1"),
            Ok(Placed {
                holder: 30,
                holder_started: 460,
                program: 31,
                program_started: 470,
            })
        );
        for (id, says) in [
            ("b2", "started before the one placing it"),
            ("c3", "runs no mycelium"),
            ("d4", "isn't its holder's own"),
            ("e5", "or by more than one"),
            ("f6", "is parked"),
            ("g7", "named by no hold record"),
        ] {
            assert!(
                placed(id).as_ref().is_err_and(|why| why.contains(says)),
                "{id}: {:?}",
                placed(id)
            );
        }
    }

    #[test]
    fn the_launchers_own_process_is_one_quickshell_started() {
        let machine = Machine::new();
        machine.runs(20, ".quickshell-wrapped", (1, 20, 20), (0, -1), 100);
        machine.runs(21, "mycelium", (20, 21, 21), (0, -1), 200);
        machine.runs(22, "mycelium", (1, 22, 22), (0, -1), 300);
        assert!(launchers_own(machine.procs(), 21, &machine.system));
        assert!(!launchers_own(machine.procs(), 22, &machine.system));
        assert!(!launchers_own(machine.procs(), 21, &System::default()));
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
            program_started: 580,
        };
        let gone = Placed {
            holder: 32,
            holder_started: 320,
            program: 60,
            program_started: 600,
        };
        let second = Placed {
            holder: 31,
            holder_started: 310,
            program: 59,
            program_started: 590,
        };
        for placed in [first, gone, second] {
            placements.keep(placed, machine.procs()).expect("kept");
        }
        assert_eq!(
            Placements::read(file.clone(), boot).placed(),
            vec![first, second],
            "one whose holder ended is forgotten, the rest read again"
        );
        assert_eq!(
            Placements::read(file.clone(), Some("boot-b".to_owned())).placed(),
            Vec::<Placed>::new(),
            "none is read from another boot"
        );
        assert_eq!(
            Placements::read(file, None).placed(),
            Vec::<Placed>::new(),
            "none is read where the boot can't be told"
        );
    }
}
