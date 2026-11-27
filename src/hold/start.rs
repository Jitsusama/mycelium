//! Starting a holder in a process of its own, as `mycelium hold` does and
//! as the node does when it is asked to start a session.

use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// How long a holder has to say it holds before it is given up on.
const HOLDS_WITHIN: Duration = Duration::from_secs(10);

/// The variable a held program is told its worker socket in
/// ([`Starter::worker_socket`]), which pi speaks on the mesh through.
pub const WORKER_SOCKET_SAID: &str = "MYCELIUM_WORKER_SOCKET";

/// How holders are started.
#[derive(Debug, Clone)]
pub struct Starter {
    /// The mycelium program that becomes each holder.
    pub program: PathBuf,
    /// Where each holder starts: in a user scope of its own, or beside
    /// whatever started it.
    pub scope: Scope,
    /// The realm's kit, the folder whose `bin` holds the tools every
    /// machine in it has, put first on the path of each program started
    /// through the person's login shell ([`login_shell`]) where it is
    /// carried, or none where the machine is given none.
    pub kit: Option<Kit>,
    /// The worker socket each program it holds is told in
    /// [`WORKER_SOCKET_SAID`], where its node serves one for the account
    /// it starts them as; none otherwise, and then none is passed on from
    /// the starter's own environment, since no socket there would answer
    /// them. The node says it, so a worker machine's image need not.
    pub worker_socket: Option<PathBuf>,
}

/// The realm's kit as a node was given it, once checked
/// ([`crate::declared::kit_checked`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kit {
    /// Carried: its folder, resolved as it was checked, goes first on
    /// each path, so a link repointed since changes nothing.
    Carried(PathBuf),
    /// On the path of nothing the node starts, since an account but its
    /// declarers could change which kit it is or what its tools are, or it
    /// couldn't be read: what it was given as, and why.
    Refused {
        /// The folder as it was given.
        given: PathBuf,
        /// Why it is left off.
        why: String,
    },
}

impl Kit {
    /// The folder that goes first on each path, where it is carried.
    #[must_use]
    pub fn carried(&self) -> Option<&Path> {
        match self {
            Self::Carried(folder) => Some(folder),
            Self::Refused { .. } => None,
        }
    }

    /// The kit as the machine record says it: its tools where it is
    /// carried, and none, with why, where it is refused, so no need for a
    /// tool is met by a kit no session is given.
    #[must_use]
    pub fn said(&self) -> Option<crate::facts::Kit> {
        match self {
            Self::Carried(folder) => crate::facts::kit(folder),
            Self::Refused { given, why } => Some(crate::facts::Kit {
                path: given.display().to_string(),
                tools: Vec::new(),
                refused: Some(why.clone()),
            }),
        }
    }
}

/// Where a holder starts. A scope of its own is what a stop reaches: when
/// the service manager stops it, as it does every unit at a shutdown, its
/// holder and program are each sent the terminate signal and waited on,
/// so the program saves and ends rather than being killed with the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Beside what started it, where nothing has a service manager.
    None,
    /// In the slice the machine runs its agents in, for a node that is a
    /// systemd unit, since stopping the unit would stop every session it
    /// started. Never the slice of the person's own programs: a session
    /// held there shared their memory's floor, and a call of theirs froze
    /// while it was reclaimed (23 November).
    Apart,
    /// In a scope of its own, for a hold a terminal started. A terminal's
    /// own scope signals nothing when stopped (kitty makes its with
    /// `KillMode=process` and no main process), so a holder left in it was
    /// killed only once the shutdown gave up waiting (2026-11-11). Its
    /// scope goes in the agents' slice, as [`Scope::Apart`]'s does, never
    /// the terminal's: a terminal the person opened runs in their own
    /// programs' slice, where 18 reopened sessions held 6 GiB of its
    /// memory and its floor (25 November).
    Beside,
}

impl Scope {
    /// The scope for a hold from a terminal: one of its own in the agents'
    /// slice where a user service manager answers in `runtime`, the
    /// user's runtime folder, and none otherwise, as on a Mac. Its name
    /// is from when it went beside the terminal's own slice.
    #[must_use]
    pub fn of_a_terminal(runtime: Option<&Path>) -> Self {
        let managed = runtime.is_some_and(|runtime| runtime.join("systemd/private").exists());
        if cfg!(target_os = "linux") && managed {
            Self::Beside
        } else {
            Self::None
        }
    }

    /// The words that start `program` in this scope, as the holder `id`.
    pub(crate) fn command(self, program: &Path, id: &str) -> Command {
        let slice = match self {
            Self::None => return Command::new(program),
            Self::Apart | Self::Beside => "--slice=agents.slice",
        };
        let mut scoped = Command::new("systemd-run");
        // systemd-run expands `${...}` in the command it runs unless told
        // not to, which empties a shell's own `${4#~/}` before the shell
        // sees it: a carried session started in the home folder rather
        // than its own (14 November).
        scoped
            .args([
                "--user",
                "--scope",
                "--collect",
                "--quiet",
                "--expand-environment=no",
                slice,
            ])
            .arg(format!("--unit=mycelium-pi-{id}"))
            .arg("--")
            .arg(program);
        scoped
    }
}

/// Whether a holder started in `scope`, by a node or not, says what it
/// has to say on its starter's standard error. Where there is no
/// journal, as on a Mac, a holder says there how each run of its program
/// ended ([`super::journal`]), so a node's holder must say it on the
/// node's log: a carried session that never arrived on a Mac left no
/// line anywhere, its holder's end gone to nothing (23 November). A
/// holder `mycelium hold` started stays silent, a terminal's or a pipe's
/// alike, since it outlives the command: on a terminal it would write
/// over whatever runs there next, and holding a pipe open it would keep
/// whatever reads that pipe waiting until the hold ends. One in a scope
/// of its own says why the scope couldn't be made.
fn says_on_its_log(scope: Scope, by_a_node: bool) -> bool {
    scope == Scope::Apart || by_a_node
}

impl Starter {
    /// Starts holding `command` in `folder` for `realm`, answering its id
    /// once a terminal can join it. With `login`, each start of it, the
    /// first and every one after a park, runs through the person's login
    /// shell ([`login_shell`]), as a node's must, since the holder has only
    /// the node's environment. Each runs `within` a fence's launcher too,
    /// where one is named, outside the login shell, as a run is, so a
    /// command the program names for its next start, or its profile,
    /// runs inside the fence as well.
    ///
    /// # Errors
    ///
    /// When the holder can't be started, ends without holding, or says
    /// nothing within ten seconds, saying why where it can.
    pub fn start(
        &self,
        realm: &str,
        folder: &Path,
        (within, command): (&[String], &[String]),
        login: bool,
    ) -> io::Result<String> {
        let id = super::new_id();
        let mut holder = self.scope.command(&self.program, &id);
        // A session and its tools are given the limit on open files the
        // node was started with, not the node's own.
        crate::open_files::as_started(&mut holder);
        self.tell_worker_socket(&mut holder);
        self.name(&mut holder, realm, &id, folder, login);
        if !within.is_empty() {
            // One word, so none of the launcher's is read as the holder's.
            let words = serde_json::to_string(within).map_err(io::Error::other)?;
            holder.arg("--within").arg(words);
        }
        let mut holder = holder
            .arg("--")
            .args(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // Only a node's start runs through the login shell, so
            // `login` says who started it.
            .stderr(if says_on_its_log(self.scope, login) {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .spawn()?;
        let out = holder.stdout.take();
        let (told, said) = mpsc::channel();
        std::thread::spawn(move || {
            let mut said = String::new();
            if let Some(out) = out {
                // A holder whose pipe fails says nothing, as one that ends.
                let _ = io::BufReader::new(out).read_line(&mut said);
            }
            // Nobody hears it once the start has given up.
            let _ = told.send(said);
        });
        let said = said.recv_timeout(HOLDS_WITHIN);
        if said.is_err() {
            // It is given up on either way; one already gone can't be killed.
            let _ = holder.kill();
        }
        // The holder stays this process's child for as long as it holds,
        // so it is waited on, or every session ended would leave a zombie.
        std::thread::spawn(move || holder.wait());
        match said.as_deref().map(str::trim) {
            Ok("held") => Ok(id),
            Ok("") => Err(io::Error::other("its holder ended without holding it")),
            Ok(why) => Err(io::Error::other(format!(
                "couldn't hold {}: {why}",
                command.join(" ")
            ))),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("its holder said nothing within {HOLDS_WITHIN:?}"),
            )),
        }
    }

    /// Tells `holder` the worker socket its program speaks through, or
    /// takes away any it was left, which the holder passes on to each
    /// start of its program.
    fn tell_worker_socket(&self, holder: &mut Command) {
        match &self.worker_socket {
            Some(socket) => holder.env(WORKER_SOCKET_SAID, socket),
            None => holder.env_remove(WORKER_SOCKET_SAID),
        };
    }

    /// Gives `holder` the words that make it the holder `id` of a program
    /// in `folder` for `realm`, through the login shell with `login`, up
    /// to the program's own.
    fn name(&self, holder: &mut Command, realm: &str, id: &str, folder: &Path, login: bool) {
        holder
            .args(["--realm", realm, "holder", "--id", id, "--folder"])
            .arg(folder);
        if login {
            holder.arg("--login");
            // A terminal's hold has the person's own path already.
            if let Some(kit) = self.kit.as_ref().and_then(Kit::carried) {
                holder.arg("--kit").arg(kit);
            }
        }
    }
}

/// The folder a start asking for `folder` begins in on a machine whose
/// home is `home`: a full path as it is, a path within home, or home when
/// it names none or names no folder here, so a checkout kept at the same
/// place under each machine's home is found on either.
#[must_use]
pub fn begins_in(folder: Option<&Path>, home: &Path) -> PathBuf {
    found(folder, home).unwrap_or_else(|| home.to_owned())
}

/// The folder `folder` names on a machine whose home is `home`, where it
/// is one there; one that can't be looked at is none.
fn found(folder: Option<&Path>, home: &Path) -> Option<PathBuf> {
    // Joining a full path gives that path, so both kinds read alike.
    folder
        .map(|folder| home.join(folder))
        .filter(|wanted| wanted.is_dir())
}

/// What a start needs of the folder it begins in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Needs {
    /// Nothing: home does where it names none here.
    Nothing,
    /// The folder it names, where it names one, since the work it's for
    /// is there; home still does where it names none.
    ItsFolder,
    /// A folder it names, and not home, since it runs where home can't
    /// be written, as within the strict fence's launcher, and begun there
    /// it would only die.
    AwayFromHome,
}

/// What a machine, named before it, answers a start [`begins_strictly`]
/// won't begin at home, said once so an asker can tell it began nothing.
pub const AT_HOME_STRICTLY: &str = " would begin this start at home, which its strict fence won't \
     let it write, so the start must name the folder of the work it's for";

/// The folder a start begins in on `machine`, as [`begins_in`] finds it,
/// or why it's refused, as it `needs`: it names a folder there is none of
/// there, which the work it's for is in, so home won't do; or it would
/// begin at home where it can't write there. The folder is looked at
/// once, so one gone in between can't send a strict start home; one it
/// can't look at counts as none.
///
/// # Errors
///
/// A start needing its folder naming one `machine` hasn't, and one
/// needing to begin away from home that would begin there.
pub fn begins_strictly(
    folder: Option<&Path>,
    needs: Needs,
    home: &Path,
    machine: &str,
) -> Result<PathBuf, String> {
    let at_home = || Err(format!("{machine}{AT_HOME_STRICTLY}"));
    match (found(folder, home), folder) {
        (Some(wanted), _) if needs == Needs::AwayFromHome && is_home(&wanted, home) => at_home(),
        (Some(wanted), _) => Ok(wanted),
        (None, Some(named)) if needs != Needs::Nothing => Err(no_folder(machine, named)),
        (None, None) if needs == Needs::AwayFromHome => at_home(),
        _ => Ok(home.to_owned()),
    }
}

/// What `machine` answers a start needing the `folder` it names, which it
/// hasn't, as [`begins_strictly`] refuses it, before anything starts.
#[must_use]
pub fn no_folder(machine: &str, folder: &Path) -> String {
    format!(
        "{machine} has no folder {}, which the start names",
        folder.display()
    )
}

/// Whether `said`, the answer to a start naming `folder` asked of
/// `machine`, is that machine's [`no_folder`], alone or as the last words
/// of a refusal wrapping it, as a relay taken as an agent's says it.
#[must_use]
pub fn lacks_folder(said: &str, machine: &str, folder: &Path) -> bool {
    let lacking = no_folder(machine, folder);
    said == lacking || said.ends_with(&format!(": {lacking}"))
}

/// Whether `folder` is `home` itself, however it's named.
fn is_home(folder: &Path, home: &Path) -> bool {
    folder == home
        || folder
            .canonicalize()
            .is_ok_and(|folder| home.canonicalize().is_ok_and(|home| folder == home))
}

/// The words that run a session on a Mac at Utility quality of service, so
/// it yields the processor to what the person is doing, as the sessions
/// mac-pi starts do. Linux has no such words: the node's service sits
/// below the person's programs already.
const YIELDING_ON_A_MAC: [&str; 3] = ["/usr/sbin/taskpolicy", "-c", "utility"];

/// What the login shell runs once the person's profile has: it says the
/// realm the program is held in, after it, since a profile says the
/// machine's own realm and would put a session started in another into
/// the wrong one; puts the kit's tools, if it is given one, first on the
/// path, after it too, since a profile (or a Mac's `path_helper`) sets
/// the path afresh and a non-interactive one may never read the file that
/// adds the person's own; names again the agent a holder said in
/// `MYCELIUM_AGENT`, after it as well, since a profile names the
/// person's own agent, and takes away every other (none where it says
/// none, and nothing changed where nothing said one); then runs the
/// program.
const IN_ITS_REALM: &str = r#"export MYCELIUM_REALM="$1"; if [ -n "$2" ]; then PATH="$2/bin${PATH:+:$PATH}"; export PATH; fi; if [ "${MYCELIUM_AGENT+said}" = said ]; then if [ -n "$MYCELIUM_AGENT" ]; then SSH_AUTH_SOCK="$MYCELIUM_AGENT"; export SSH_AUTH_SOCK; else unset SSH_AUTH_SOCK; fi; unset SSH_TPM_AUTH_SOCK MYCELIUM_AGENT; fi; shift 2; exec "$@""#;

/// The shell's script that takes off its path, before [`IN_ITS_REALM`],
/// every folder in its home, by name or by where it leads, and every one
/// named relatively, which the run's folder in that home would hold: a
/// service's path may name the home's own tools (`~/.local/bin`, a
/// profile's `bin`), which whoever wrote the home could make shadow a
/// command. Each folder it keeps it names by where it leads, so no link
/// the home holds along the way can be pointed elsewhere before the
/// command is looked up, and one it can't enter, which may not be there
/// yet, goes too. A path left with nothing is `/usr/bin:/bin`, never
/// empty, which a shell reads as the run's folder. The home is the one
/// its first word names, the node's own, and the `HOME` it is given as
/// well, so a launcher that takes `HOME` away or points it elsewhere
/// leaves none of either on the path. It shifts that word off. With no
/// home, or `/` for one, only the relative and the missing go.
const OUT_OF_ITS_HOME: &str = r#"homes=; for at in "$1" "${HOME-}"; do case $at in /?*) at=${at%/}; homes=$homes$at:; led=$(cd -P "$at" 2>/dev/null && pwd -P) && homes=$homes$led:;; esac; done; shift; kept=; set -f; IFS=:; for dir in ${PATH-}; do case $dir in /*) ;; *) continue;; esac; led=$(cd -P "$dir" 2>/dev/null && pwd -P) || continue; for at in $homes; do case $dir/ in "$at"/*) led=;; esac; case $led/ in "$at"/*) led=;; esac; done; [ -z "$led" ] || kept=${kept:+$kept:}$led; done; unset IFS; set +f; PATH=${kept:-/usr/bin:/bin}; export PATH; "#;

/// The words that run a program held in `realm` through the person's
/// login shell, as a terminal would run it, so it has their environment
/// rather than a service's: a pi started without it has no model token
/// and ends at once, and a bare `pi` isn't found on a service's path.
/// With a `kit`, the kit's tools come first on its path. On a Mac it runs
/// yielding to the person's own programs.
#[must_use]
pub fn login_shell(realm: &str, kit: Option<&Path>) -> Vec<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| {
        if cfg!(target_os = "macos") {
            "/bin/zsh".to_owned()
        } else {
            "/bin/sh".to_owned()
        }
    });
    let mut through: Vec<String> = if cfg!(target_os = "macos") {
        YIELDING_ON_A_MAC.map(str::to_owned).to_vec()
    } else {
        Vec::new()
    };
    through.extend(in_its_realm(
        shell,
        "-lic",
        IN_ITS_REALM.to_owned(),
        realm,
        kit,
    ));
    through
}

/// The words that run a program in `realm` through `shell` with `flags`
/// and `script`, which ends as [`IN_ITS_REALM`] does, with the `kit`'s
/// tools first on its path.
fn in_its_realm(
    shell: String,
    flags: &str,
    script: String,
    realm: &str,
    kit: Option<&Path>,
) -> [String; 6] {
    [
        shell,
        flags.to_owned(),
        script,
        "mycelium".to_owned(),
        realm.to_owned(),
        // Always a word, empty for none, so the program's own start where
        // the script shifts to.
        kit.map(|kit| kit.display().to_string()).unwrap_or_default(),
    ]
}

/// The words that run a command in `realm` as [`login_run`] does, but
/// through `/bin/sh`, which run so reads no profile, for a run whose
/// home another may have written: the person's run sent to a room an
/// agent's make booked, whose profile that agent could make shadow the
/// command or fake its end. It has the environment its launcher gives
/// it, but with no folder of `home`, the home it runs in, nor of the
/// `HOME` it is given, on its path ([`OUT_OF_ITS_HOME`]), and the kit's
/// tools first. A command found only in the home isn't found, rather
/// than found as the home's writer left it. `HOME` still names the
/// home, though, so a program that reads its own files from there, as
/// an interpreter may, reads what that writer left.
#[must_use]
pub fn plain_run(realm: &str, kit: Option<&Path>, home: &Path) -> Vec<String> {
    let script = format!("{OUT_OF_ITS_HOME}{IN_ITS_REALM}");
    let mut words = in_its_realm("/bin/sh".to_owned(), "-c", script, realm, kit).to_vec();
    // After the shell's own name, as the script's first word.
    words.insert(4, home.display().to_string());
    words
}

/// The words that run a command in `realm` through the person's login
/// shell, as [`login_shell`] does, but not as a terminal would: nothing
/// is held, so the shell runs without a terminal, and an interactive one
/// would complain of having none. Someone waits on its output, so on a
/// Mac it doesn't yield as a held session does.
#[must_use]
pub fn login_run(realm: &str, kit: Option<&Path>) -> Vec<String> {
    let mut words = login_shell(realm, kit);
    // By where they stand, not what they say, so a realm or a kit that
    // happens to read as one of them is kept.
    if cfg!(target_os = "macos") {
        words.drain(..YIELDING_ON_A_MAC.len());
    }
    if let Some(flags) = words.iter_mut().find(|word| *word == "-lic") {
        "-lc".clone_into(flags);
    }
    words
}

/// The words that join the session held as `id` in `realm` on this
/// machine, from a terminal here.
#[must_use]
pub fn attach_here(realm: &str, id: &str) -> Vec<String> {
    ["mycelium", "--realm", realm, "attach", id]
        .map(str::to_owned)
        .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn a_nodes_holder_says_how_its_runs_end_on_the_nodes_log() {
        assert!(
            says_on_its_log(Scope::None, true),
            "a Mac's node logs to a file, where its holders' ends must land"
        );
        assert!(
            says_on_its_log(Scope::Apart, false),
            "a holder in a unit of its own says why its scope failed, whoever started it"
        );
        assert!(
            !says_on_its_log(Scope::None, false),
            "one `mycelium hold` started holds nothing of its caller's open"
        );
        assert!(
            !says_on_its_log(Scope::Beside, false),
            "nor does one a terminal started"
        );
    }

    #[test]
    fn a_terminals_hold_has_a_scope_of_its_own_where_a_service_manager_answers() {
        let runtime = tempfile::tempdir().expect("a temporary folder");
        assert_eq!(
            Scope::of_a_terminal(Some(runtime.path())),
            Scope::None,
            "with no user service manager, a holder starts beside its terminal"
        );
        assert_eq!(
            Scope::of_a_terminal(None),
            Scope::None,
            "nor with no runtime folder"
        );
        fs::create_dir(runtime.path().join("systemd")).expect("the manager's folder");
        fs::write(runtime.path().join("systemd/private"), "").expect("its socket");
        let managed = if cfg!(target_os = "linux") {
            Scope::Beside
        } else {
            Scope::None
        };
        assert_eq!(
            Scope::of_a_terminal(Some(runtime.path())),
            managed,
            "a shutdown's stop reaches a holder only in a scope of its own"
        );
    }

    #[test]
    fn a_holder_goes_among_the_agents_whatever_slice_started_it() {
        let words = |scope: Scope| -> Vec<String> {
            let command = scope.command(Path::new("/bin/mycelium"), "f00d");
            std::iter::once(command.get_program())
                .chain(command.get_args())
                .map(|word| word.to_string_lossy().into_owned())
                .collect()
        };
        let scoped = |slice: &str| {
            [
                "systemd-run",
                "--user",
                "--scope",
                "--collect",
                "--quiet",
                "--expand-environment=no",
                slice,
            ]
            .into_iter()
            .chain(["--unit=mycelium-pi-f00d", "--", "/bin/mycelium"])
            .map(str::to_owned)
            .collect::<Vec<_>>()
        };
        assert_eq!(
            words(Scope::None),
            ["/bin/mycelium"],
            "unscoped, it runs as itself"
        );
        assert_eq!(
            words(Scope::Apart),
            scoped("--slice=agents.slice"),
            "a node's go apart, among the agents"
        );
        assert_eq!(
            words(Scope::Beside),
            scoped("--slice=agents.slice"),
            "a terminal's goes among the agents too, never the person's programs' slice"
        );
    }

    #[test]
    fn a_scoped_holders_words_reach_its_shell_unexpanded() {
        for scope in [Scope::Apart, Scope::Beside] {
            let command = scope.command(Path::new("/bin/mycelium"), "f00d");
            let words: Vec<_> = command
                .get_args()
                .map(|word| word.to_string_lossy())
                .collect();
            let program = words.iter().position(|word| word == "--");
            let kept = words
                .iter()
                .position(|word| word == "--expand-environment=no");
            assert!(
                kept.is_some() && kept < program,
                "systemd-run expands ${{...}} in what it runs unless told not to, which \
                 emptied a carried session's folder (\"${{4#~/}}\"): {words:?}"
            );
        }
    }

    #[test]
    fn a_program_runs_through_the_login_shell_with_its_words_kept_apart() {
        let mut words = login_shell("harbour", None);
        words.extend(["pi".to_owned(), "say \"hi\" $HOME".to_owned()]);
        let yielding = if cfg!(target_os = "macos") {
            YIELDING_ON_A_MAC.len()
        } else {
            0
        };
        assert_eq!(
            words[..yielding],
            YIELDING_ON_A_MAC[..yielding],
            "a Mac's session yields to the person's programs"
        );
        assert_eq!(
            words[yielding + 1..],
            [
                "-lic",
                IN_ITS_REALM,
                "mycelium",
                "harbour",
                "",
                "pi",
                "say \"hi\" $HOME"
            ],
            "the shell execs the words as given, never reading them as its own"
        );
    }

    /// What `program`, given to `sh -c`, prints when run through `words`
    /// by `sh` standing for the person's shell, with `profile`'s
    /// variables standing for what a profile sets before the script runs.
    fn run_through(words: &[String], program: &str, profile: &[(&str, &str)]) -> String {
        // A login shell's flags first, since a Mac's yielding words hold
        // a `-c` of their own before them.
        let script = words
            .iter()
            .position(|word| word == "-lic" || word == "-lc")
            .or_else(|| words.iter().position(|word| word == "-c"))
            .expect("the words run a shell");
        let said = Command::new("sh")
            .arg("-c")
            .args(&words[script + 1..])
            .args(["sh", "-c", program])
            // Whatever this test runs in, only the profile says an agent.
            .env_remove("MYCELIUM_AGENT")
            .envs(profile.iter().copied())
            .output()
            .expect("the stand-in shell runs");
        String::from_utf8_lossy(&said.stdout).into_owned()
    }

    #[test]
    fn a_program_is_in_the_realm_it_is_held_in_whatever_the_profile_says() {
        let said = run_through(
            &login_shell("harbour", None),
            r#"printf %s "$MYCELIUM_REALM""#,
            &[("MYCELIUM_REALM", "lighthouse")],
        );
        assert_eq!(said, "harbour", "the program is in the realm it is held in");
    }

    #[test]
    fn a_program_finds_the_kits_tools_first_whatever_the_profile_says() {
        let kit = tempfile::tempdir().expect("a temporary kit");
        let bin = kit.path().join("bin");
        fs::create_dir(&bin).expect("the kit's bin");
        let profile = [("PATH", "/usr/bin:/bin")];
        for words in [
            login_shell("harbour", Some(kit.path())),
            login_run("harbour", Some(kit.path())),
        ] {
            assert_eq!(
                run_through(&words, r#"printf %s "$PATH""#, &profile),
                format!("{}:/usr/bin:/bin", bin.display()),
                "the kit's tools come before the profile's, which set the path afresh"
            );
        }
    }

    #[test]
    fn a_kit_on_an_empty_path_never_puts_the_runs_folder_on_it() {
        let kit = tempfile::tempdir().expect("a temporary kit");
        let bin = kit.path().join("bin");
        fs::create_dir(&bin).expect("the kit's bin");
        let sh = std::env::split_paths(&std::env::var_os("PATH").expect("a PATH"))
            .map(|folder| folder.join("sh"))
            .find(|sh| sh.is_file())
            .expect("sh is on the PATH");
        // The kit holds the shell the run starts, since nothing else is on
        // an empty path.
        std::os::unix::fs::symlink(&sh, bin.join("sh")).expect("the kit's shell");
        let words = login_run("harbour", Some(kit.path()));
        let script = words
            .iter()
            .position(|word| word == "-lc")
            .expect("the words run a login shell");
        let said = Command::new(&sh)
            .arg("-c")
            .args(&words[script + 1..])
            .args(["sh", "-c", r#"printf %s "$PATH""#])
            .env("PATH", "")
            .output()
            .expect("the stand-in shell runs");
        assert!(
            said.status.success(),
            "the run started through the kit's shell: {}",
            String::from_utf8_lossy(&said.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&said.stdout),
            bin.display().to_string(),
            "an empty entry would search the folder the run starts in"
        );
    }

    #[test]
    fn a_run_keeps_a_realm_and_a_kit_that_read_as_a_macs_yielding_words() {
        let words = login_run("utility", Some(Path::new("/usr/sbin/taskpolicy")));
        assert_eq!(
            words[1..],
            [
                "-lc",
                IN_ITS_REALM,
                "mycelium",
                "utility",
                "/usr/sbin/taskpolicy"
            ],
            "only the words a held session yields with are left out of a run"
        );
    }

    #[test]
    fn a_held_program_signs_with_the_agent_its_holder_said_whatever_the_profile_names() {
        let program = r#"printf '%s|%s|%s' "${SSH_AUTH_SOCK-none}" "${SSH_TPM_AUTH_SOCK-none}" "${MYCELIUM_AGENT-gone}""#;
        // As a profile names the person's own agent before the script.
        let person = [
            ("SSH_AUTH_SOCK", "/run/user/4242/every-key.sock"),
            ("SSH_TPM_AUTH_SOCK", "/run/user/4242/every-key.sock"),
        ];
        let with = |agent: &'static str| {
            let mut profile = person.to_vec();
            profile.push(("MYCELIUM_AGENT", agent));
            profile
        };
        for words in [login_shell("harbour", None), login_run("harbour", None)] {
            assert_eq!(
                run_through(&words, program, &with("/run/user/4242/pi-agent.sock")),
                "/run/user/4242/pi-agent.sock|none|gone",
                "the proxy the holder said, no other agent, and the word itself gone"
            );
            assert_eq!(
                run_through(&words, program, &with("")),
                "none|none|gone",
                "no agent at all where the holder said none"
            );
            assert_eq!(
                run_through(&words, program, &person),
                "/run/user/4242/every-key.sock|/run/user/4242/every-key.sock|gone",
                "what said no agent, as a Mac's hold, is left as the profile has it"
            );
        }
    }

    // Followup 487: the person's run sent to a room an agent wrote the
    // home of runs through a shell reading no profile there, yet in its
    // realm with the kit's tools first, as a login run is.
    #[test]
    fn a_plain_run_reads_no_profile_and_keeps_its_realm_and_kit() {
        let words = plain_run(
            "harbour",
            Some(Path::new("/nix/store/kit")),
            Path::new("/home/kestrel"),
        );
        assert_eq!(
            words[..2],
            ["/bin/sh".to_owned(), "-c".to_owned()],
            "a plain shell, never a login one, whatever this login's shell is"
        );
        assert!(
            !words.iter().any(|word| word == "-lc" || word == "-lic"),
            "{words:?}"
        );
        assert_eq!(
            run_through(
                &words,
                r#"printf '%s|%s' "$MYCELIUM_REALM" "${PATH%%:*}""#,
                &[("MYCELIUM_REALM", "lighthouse")]
            ),
            "harbour|/nix/store/kit/bin",
            "in the realm it runs in, with the kit's tools first"
        );
    }

    #[test]
    fn a_plain_run_finds_nothing_in_its_home() {
        let made = tempfile::tempdir().expect("a temporary folder");
        // Where it leads, as a Mac's temporary folder is behind a link.
        let at = fs::canonicalize(made.path()).expect("the folder's place");
        let real = at.join("real");
        let tools = real.join(".local/bin");
        fs::create_dir_all(&tools).expect("the home's tools");
        // The home by a link to it, as a service may be told it, and a
        // folder outside that leads into it.
        let home = at.join("home");
        std::os::unix::fs::symlink(&real, &home).expect("a home by a link");
        let leads = at.join("leads");
        std::os::unix::fs::symlink(&tools, &leads).expect("a folder leading in");
        // The system's folders, made here, since a sandbox may lack the
        // real ones.
        let (first, last) = (at.join("first"), at.join("last"));
        let around = at.join("around");
        let beside = at.join("homex/bin");
        // Outside the home: a folder by a link, kept by where it leads, and
        // one whose way runs through a link in the home, which the home's
        // writer could point elsewhere, kept only where it leads now.
        let store = at.join("store");
        for folder in [&first, &last, &around, &beside, &store.join("bin")] {
            fs::create_dir_all(folder).expect("a folder outside");
        }
        let named = at.join("named");
        std::os::unix::fs::symlink(&around, &named).expect("a folder by a link");
        std::os::unix::fs::symlink(&store, real.join(".nix-profile")).expect("a profile");
        let through = at.join("through");
        std::os::unix::fs::symlink(home.join(".nix-profile"), &through).expect("a way in");
        let shown = |folder: &Path| folder.display().to_string();
        let [first, last, around, beside, store] =
            [&first, &last, &around, &beside, &store].map(shown);
        let [home, real, leads] = [&home, &real, &leads].map(shown);
        let path = [
            format!("{home}/.local/bin"),
            first.clone(),
            format!("{real}/.nix-profile/bin"),
            "bin".to_owned(),
            String::new(),
            leads,
            home.clone(),
            shown(&named),
            shown(&through.join("bin")),
            // Not there yet, so it may yet be made a way in.
            shown(&at.join("missing")),
            beside.clone(),
            last.clone(),
        ]
        .join(":");
        let print = r#"printf %s "$PATH""#;
        let kept = format!("/nix/store/kit/bin:{first}:{around}:{store}/bin:{beside}:{last}");
        let plain = plain_run(
            "harbour",
            Some(Path::new("/nix/store/kit")),
            Path::new(&format!("{home}/")),
        );
        assert_eq!(
            run_through(
                &plain,
                print,
                &[("HOME", &format!("{home}/")), ("PATH", &path)]
            ),
            kept,
            "the home's folders go, by name or by where they lead, and relative and missing \
             ones, and each kept is named by where it leads"
        );
        // A launcher may point HOME elsewhere; the home the run is in
        // still goes, by name or by where it leads.
        let elsewhere = at.join("elsewhere").display().to_string();
        for given in [&home, &real] {
            let plain = plain_run(
                "harbour",
                Some(Path::new("/nix/store/kit")),
                Path::new(given),
            );
            assert_eq!(
                run_through(&plain, print, &[("HOME", &elsewhere), ("PATH", &path)]),
                kept,
                "the home it runs in, {given}, goes whatever HOME says"
            );
        }
        // And the HOME it is given goes too, though the node's is another.
        let plain = plain_run(
            "harbour",
            Some(Path::new("/nix/store/kit")),
            Path::new(&elsewhere),
        );
        assert_eq!(
            run_through(&plain, print, &[("HOME", &home), ("PATH", &path)]),
            kept,
            "the home HOME names goes as well"
        );
        assert_eq!(
            run_through(
                &login_run("harbour", None),
                print,
                &[("HOME", &home), ("PATH", &path)]
            ),
            path,
            "the person's own run keeps its path"
        );
    }

    #[test]
    fn a_plain_run_left_with_no_folder_has_the_systems_alone() {
        let made = tempfile::tempdir().expect("a temporary folder");
        let at = fs::canonicalize(made.path()).expect("the folder's place");
        let (home, first, last) = (at.join("home"), at.join("first"), at.join("last"));
        for folder in [&home.join(".local/bin"), &first, &last] {
            fs::create_dir_all(folder).expect("a folder");
        }
        let shown = |folder: &Path| folder.display().to_string();
        let (home, first, last) = (shown(&home), shown(&first), shown(&last));
        let print = r#"printf %s "$PATH""#;
        let plain = plain_run("harbour", None, Path::new("/"));
        assert_eq!(
            run_through(
                &plain,
                print,
                &[("HOME", "/"), ("PATH", &format!("{first}:.:{last}"))]
            ),
            format!("{first}:{last}"),
            "a home of / takes off only the relative and the missing"
        );
        // A path left with nothing, and no kit, is never empty, which a
        // shell would read as the run's folder, in the home.
        let plain = plain_run("harbour", None, Path::new(&home));
        for given in [
            format!("{home}/.local/bin"),
            String::new(),
            ":bin:".to_owned(),
        ] {
            assert_eq!(
                run_through(&plain, print, &[("HOME", &home), ("PATH", &given)]),
                "/usr/bin:/bin",
                "a path of {given:?} left with nothing is the system's alone"
            );
        }
    }

    #[test]
    fn a_program_given_no_kit_keeps_the_path_the_profile_set() {
        let profile = [("PATH", "/usr/bin:/bin")];
        for words in [login_shell("harbour", None), login_run("harbour", None)] {
            assert_eq!(
                run_through(&words, r#"printf %s "$PATH""#, &profile),
                "/usr/bin:/bin",
                "a machine with no kit runs as it did"
            );
        }
    }

    #[test]
    fn a_node_hold_names_its_kit_to_the_holder_and_a_terminals_does_not() {
        let mut starter = Starter {
            program: PathBuf::from("/bin/mycelium"),
            scope: Scope::None,
            kit: Some(Kit::Carried(PathBuf::from("/nix/store/kit"))),
            worker_socket: None,
        };
        // The words the holder is started with, as `start` gives them.
        let words = |login: bool| {
            let mut holder = starter.scope.command(&starter.program, "f00d");
            starter.name(&mut holder, "harbour", "f00d", Path::new("/home"), login);
            holder
                .get_args()
                .map(|word| word.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        let kit = ["--kit".to_owned(), "/nix/store/kit".to_owned()];
        assert!(
            words(true).windows(2).any(|pair| pair == kit),
            "a node's hold runs through the login shell, with its kit: {:?}",
            words(true)
        );
        assert!(
            !words(false).contains(&kit[0]),
            "a terminal's has the person's own path already: {:?}",
            words(false)
        );
        starter.kit = Some(Kit::Refused {
            given: PathBuf::from("/home/kit"),
            why: "its bin may be written by others than its owner".to_owned(),
        });
        let words = {
            let mut holder = starter.scope.command(&starter.program, "f00d");
            starter.name(&mut holder, "harbour", "f00d", Path::new("/home"), true);
            holder
                .get_args()
                .map(|word| word.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert!(
            !words.contains(&kit[0]),
            "a refused kit is named to no holder: {words:?}"
        );
    }

    #[test]
    fn a_holder_is_told_the_worker_socket_only_where_its_node_serves_one() {
        let told = |worker_socket: Option<&str>| {
            let starter = Starter {
                program: PathBuf::from("/bin/mycelium"),
                scope: Scope::None,
                kit: None,
                worker_socket: worker_socket.map(PathBuf::from),
            };
            let mut holder = starter.scope.command(&starter.program, "f00d");
            starter.tell_worker_socket(&mut holder);
            holder
                .get_envs()
                .find(|(name, _)| *name == WORKER_SOCKET_SAID)
                .map(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()))
        };
        assert_eq!(
            told(Some("/run/mycelium/work-worker.sock")),
            Some(Some("/run/mycelium/work-worker.sock".to_owned())),
            "a node serving a worker socket tells each holder it"
        );
        assert_eq!(
            told(None),
            Some(None),
            "one serving none takes away whatever its own environment named"
        );
    }

    #[test]
    fn a_start_begins_in_the_folder_it_names_under_home_or_else_at_home() {
        let home = tempfile::tempdir().expect("a temporary home");
        let home = home.path();
        let src = home.join("src").join("core.nix");
        fs::create_dir_all(&src).expect("a checkout is made");
        let cases = [
            (Some(Path::new("src/core.nix")), src.clone()),
            (Some(src.as_path()), src.clone()),
            (Some(Path::new("src/elsewhere")), home.to_owned()),
            (Some(Path::new("/no/such/folder")), home.to_owned()),
            (None, home.to_owned()),
        ];
        for (asked, begins) in cases {
            assert_eq!(begins_in(asked, home), begins, "asked for {asked:?}");
        }
    }

    #[test]
    fn a_strict_start_refuses_a_folder_the_machine_hasnt_naming_both() {
        let home = tempfile::tempdir().expect("a temporary home");
        let home = home.path();
        let src = home.join("src").join("core.nix");
        fs::create_dir_all(&src).expect("a checkout is made");
        let begun = |folder: Option<&Path>, strict| {
            let needs = if strict {
                Needs::ItsFolder
            } else {
                Needs::Nothing
            };
            begins_strictly(folder, needs, home, "tern")
        };
        assert_eq!(
            begun(Some(Path::new("src/core.nix")), true),
            Ok(src.clone())
        );
        assert_eq!(
            begun(None, true),
            Ok(home.to_owned()),
            "none named, it begins at home"
        );
        assert_eq!(
            begun(Some(Path::new("src/elsewhere")), false),
            Ok(home.to_owned()),
            "one not strict begins at home"
        );
        assert_eq!(
            begun(Some(Path::new("src/core.nix")), false),
            Ok(src.clone()),
            "one not strict begins in a folder that's there"
        );
        fs::write(home.join("notes"), "").expect("a file is made");
        assert_eq!(
            begun(Some(Path::new("notes")), true),
            Err("tern has no folder notes, which the start names".to_owned()),
            "a file is no folder"
        );
        assert_eq!(
            begun(Some(&src), true),
            Ok(src.clone()),
            "a full path is that path"
        );
        assert_eq!(
            begun(Some(Path::new("src/elsewhere")), true),
            Err("tern has no folder src/elsewhere, which the start names".to_owned()),
            "the refusal names the machine and the folder"
        );
        let gone = home.join("gone");
        assert_eq!(
            begun(Some(&gone), true),
            Err(format!(
                "tern has no folder {}, which the start names",
                gone.display()
            )),
            "a full path it hasn't, too"
        );
    }

    #[test]
    fn a_start_that_cant_write_home_is_refused_beginning_there() {
        let home = tempfile::tempdir().expect("a temporary home");
        let home = home.path();
        let src = home.join("src").join("core.nix");
        fs::create_dir_all(&src).expect("a checkout is made");
        let begun =
            |folder: Option<&Path>| begins_strictly(folder, Needs::AwayFromHome, home, "tern");
        let at_home = Err(
            "tern would begin this start at home, which its strict fence won't let \
                           it write, so the start must name the folder of the work it's for"
                .to_owned(),
        );
        assert_eq!(begun(Some(Path::new("src/core.nix"))), Ok(src));
        assert_eq!(begun(None), at_home, "none named would begin at home");
        assert_eq!(begun(Some(home)), at_home, "so would home named");
        assert_eq!(begun(Some(Path::new("."))), at_home, "however it's named");
        assert_eq!(
            begun(Some(Path::new("src/elsewhere"))),
            Err("tern has no folder src/elsewhere, which the start names".to_owned()),
            "and one it hasn't is refused naming it"
        );
    }

    #[test]
    fn a_machine_refusing_a_start_for_its_folder_alone_is_told_however_wrapped() {
        let home = tempfile::tempdir().expect("a home");
        let folder = Path::new("src/rows");
        let refused = begins_strictly(Some(folder), Needs::ItsFolder, home.path(), "tern")
            .expect_err("tern hasn't it");
        assert!(lacks_folder(&refused, "tern", folder));
        let relayed =
            format!("tern takes a relayed start as an agent's, and refused it: {refused}");
        assert!(lacks_folder(&relayed, "tern", folder), "wrapped: {relayed}");
        assert!(
            !lacks_folder(&refused, "ern", folder),
            "another machine's refusal is no answer about this one"
        );
        assert!(
            !lacks_folder(&refused, "tern", Path::new("src/other")),
            "nor another folder's"
        );
        let silent = "tern didn't answer within 90s";
        assert!(!lacks_folder(silent, "tern", folder), "nor no answer");
    }
}
