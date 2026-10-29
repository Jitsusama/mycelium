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
    /// Whether each holder starts in a user scope of its own, in the slice
    /// the person's own programs run in. A node that is a systemd unit
    /// needs it, or stopping the unit would stop every session it started.
    pub scoped: bool,
}

impl Starter {
    /// Starts holding `command` in `folder` for `realm`, answering its id
    /// once a terminal can join it.
    ///
    /// # Errors
    ///
    /// When the holder can't be started, ends without holding, or says
    /// nothing within ten seconds, saying why where it can.
    pub fn start(&self, realm: &str, folder: &Path, command: &[String]) -> io::Result<String> {
        let id = super::new_id();
        let mut holder = if self.scoped {
            let mut scoped = Command::new("systemd-run");
            scoped
                .args(["--user", "--scope", "--collect", "--quiet"])
                .arg("--slice=app.slice")
                .arg(format!("--unit=mycelium-pi-{id}"))
                .arg("--")
                .arg(&self.program);
            scoped
        } else {
            Command::new(&self.program)
        };
        let mut holder = holder
            .args(["--realm", realm, "holder", "--id", &id, "--folder"])
            .arg(folder)
            .arg("--")
            .args(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // A scope that can't be made says why on the node's own log.
            .stderr(if self.scoped {
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

/// `command` run through the person's login shell, as a terminal would
/// run it, so it has their environment rather than a service's: a pi
/// started without it has no model token and ends at once.
#[must_use]
pub fn through_login_shell(command: &[String]) -> Vec<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| {
        if cfg!(target_os = "macos") {
            "/bin/zsh".to_owned()
        } else {
            "/bin/sh".to_owned()
        }
    });
    let mut through = vec![
        shell,
        "-lic".to_owned(),
        r#"exec "$@""#.to_owned(),
        "mycelium".to_owned(),
    ];
    through.extend_from_slice(command);
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

    #[test]
    fn a_program_runs_through_the_login_shell_with_its_words_kept_apart() {
        let words = through_login_shell(&["pi".to_owned(), "say \"hi\" $HOME".to_owned()]);
        assert_eq!(
            words[1..],
            ["-lic", r#"exec "$@""#, "mycelium", "pi", "say \"hi\" $HOME"],
            "the shell execs the words as given, never reading them as its own"
        );
    }
}
