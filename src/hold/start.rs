//! Starting a holder in a process of its own, as `mycelium hold` does and
//! as the node does when it is asked to start a session.

use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// How long a holder has to say it holds before it is given up on.
const HOLDS_WITHIN: Duration = Duration::from_secs(10);

/// How holders are started.
#[derive(Debug, Clone)]
pub struct Starter {
    /// The mycelium program that becomes each holder.
    pub program: PathBuf,
    /// Where each holder starts: in a user scope of its own, or beside
    /// whatever started it.
    pub scope: Scope,
}

/// Where a holder starts. A scope of its own is what a stop reaches: when
/// the service manager stops it, as it does every unit at a shutdown, its
/// holder and program are each sent the terminate signal and waited on,
/// so the program saves and ends rather than being killed with the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Beside what started it, where nothing has a service manager.
    None,
    /// In the slice the person's own programs run in, for a node that is a
    /// systemd unit, since stopping the unit would stop every session it
    /// started.
    Apart,
    /// In the slice of the terminal that started it. A terminal's own
    /// scope signals nothing when stopped (kitty makes its with
    /// `KillMode=process` and no main process), so a holder left in it was
    /// killed only once the shutdown gave up waiting (2026-11-11).
    Beside,
}

impl Scope {
    /// The scope for a hold from a terminal: beside its slice where a user
    /// service manager answers in `runtime`, the user's runtime folder,
    /// and none otherwise, as on a Mac.
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
    fn command(self, program: &Path, id: &str) -> Command {
        let slice = match self {
            Self::None => return Command::new(program),
            Self::Apart => "--slice=app.slice",
            Self::Beside => "--slice-inherit",
        };
        let mut scoped = Command::new("systemd-run");
        scoped
            .args(["--user", "--scope", "--collect", "--quiet", slice])
            .arg(format!("--unit=mycelium-pi-{id}"))
            .arg("--")
            .arg(program);
        scoped
    }
}

impl Starter {
    /// Starts holding `command` in `folder` for `realm`, answering its id
    /// once a terminal can join it. With `login`, each start of it, the
    /// first and every one after a park, runs through the person's login
    /// shell ([`login_shell`]), as a node's must, since the holder has only
    /// the node's environment.
    ///
    /// # Errors
    ///
    /// When the holder can't be started, ends without holding, or says
    /// nothing within ten seconds, saying why where it can.
    pub fn start(
        &self,
        realm: &str,
        folder: &Path,
        command: &[String],
        login: bool,
    ) -> io::Result<String> {
        let id = super::new_id();
        let mut holder = self.scope.command(&self.program, &id);
        holder
            .args(["--realm", realm, "holder", "--id", &id, "--folder"])
            .arg(folder);
        if login {
            holder.arg("--login");
        }
        let mut holder = holder
            .arg("--")
            .args(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // A scope that can't be made says why on the node's own log.
            .stderr(if self.scope == Scope::Apart {
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
}

/// The folder a start asking for `folder` begins in on a machine whose
/// home is `home`: a full path as it is, a path within home, or home when
/// it names none or names no folder here, so a checkout kept at the same
/// place under each machine's home is found on either.
#[must_use]
pub fn begins_in(folder: Option<&Path>, home: &Path) -> PathBuf {
    // Joining a full path gives that path, so both kinds read alike.
    match folder.map(|folder| home.join(folder)) {
        Some(wanted) if wanted.is_dir() => wanted,
        _ => home.to_owned(),
    }
}

/// The words that run a session on a Mac at Utility quality of service, so
/// it yields the processor to what the person is doing, as the sessions
/// mac-pi starts do. Linux has no such words: the node's service sits
/// below the person's programs already.
const YIELDING_ON_A_MAC: [&str; 3] = ["/usr/sbin/taskpolicy", "-c", "utility"];

/// What the login shell runs once the person's profile has: it says the
/// realm the program is held in, after it, since a profile says the
/// machine's own realm and would put a session started in another into
/// the wrong one, then runs the program.
const IN_ITS_REALM: &str = r#"export MYCELIUM_REALM="$1"; shift; exec "$@""#;

/// The words that run a program held in `realm` through the person's
/// login shell, as a terminal would run it, so it has their environment
/// rather than a service's: a pi started without it has no model token
/// and ends at once, and a bare `pi` isn't found on a service's path. On
/// a Mac it runs yielding to the person's own programs.
#[must_use]
pub fn login_shell(realm: &str) -> Vec<String> {
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
    through.extend([
        shell,
        "-lic".to_owned(),
        IN_ITS_REALM.to_owned(),
        "mycelium".to_owned(),
        realm.to_owned(),
    ]);
    through
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
    fn a_holder_beside_its_terminal_keeps_the_terminals_slice() {
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
            scoped("--slice=app.slice"),
            "a node's go apart"
        );
        assert_eq!(
            words(Scope::Beside),
            scoped("--slice-inherit"),
            "a terminal's stays in the terminal's slice, weighed as it was"
        );
    }

    #[test]
    fn a_program_runs_through_the_login_shell_with_its_words_kept_apart() {
        let mut words = login_shell("harbour");
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
                "pi",
                "say \"hi\" $HOME"
            ],
            "the shell execs the words as given, never reading them as its own"
        );
    }

    #[test]
    fn a_program_is_in_the_realm_it_is_held_in_whatever_the_profile_says() {
        let words = login_shell("harbour");
        let script = words
            .iter()
            .position(|word| word == "-lic")
            .expect("the words run a login shell");
        // `sh` stands for the person's shell, and the realm it is given
        // for the profile's, which a profile says before the script runs.
        let said = Command::new("sh")
            .arg("-c")
            .args(&words[script + 1..])
            .args(["sh", "-c", r#"printf %s "$MYCELIUM_REALM""#])
            .env("MYCELIUM_REALM", "lighthouse")
            .output()
            .expect("the stand-in shell runs");
        assert_eq!(
            String::from_utf8_lossy(&said.stdout),
            "harbour",
            "the program is in the realm it is held in"
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
}
