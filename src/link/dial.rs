//! Dialling: how one machine reaches another's end of a link, as a byte
//! stream each way. SSH carries it ([`Ssh`]), or a provider's program for
//! machines SSH can't reach, such as a cloud machine behind its provider's
//! gateway ([`Program`]); [`Way`] is either, so one node links over both.

use std::io;
use std::process::Stdio;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::task::JoinHandle;

use crate::link::refused;

/// A link's byte streams, and whatever must live as long as they do.
pub struct Connection {
    /// What the other end sends.
    pub reading: Box<dyn AsyncRead + Unpin + Send>,
    /// What this end sends.
    pub writing: Box<dyn AsyncWrite + Unpin + Send>,
    /// Kept until the link is over, such as the process carrying it.
    pub keep: Box<dyn Send>,
    /// Why the way in turned this machine away, if it did, known once it
    /// has ended; none for a way in that never refuses.
    pub refused: Option<JoinHandle<Option<String>>>,
    /// Whether the way in can release the machine it reaches once that
    /// machine hands over ([`Dial::release`]).
    pub releases: bool,
}

/// A way to reach another machine's end of a link.
pub trait Dial: Send + Sync + 'static {
    /// Where it dials, as a person reads it.
    fn describe(&self) -> String;

    /// Reaches the other end, which runs in `realm`.
    fn dial(&self, realm: &str) -> impl Future<Output = io::Result<Connection>> + Send;

    /// The words that run `words` on the machine it reaches, from a
    /// terminal here, where `mycelium` names that machine's own.
    fn there(&self, words: &[String]) -> Vec<String>;

    /// Lets the machine it reaches go, once that machine has handed over,
    /// as a provider destroys a machine it lent; none for a way that can't,
    /// which SSH never does.
    fn release(&self) -> impl Future<Output = Option<io::Result<()>>> + Send {
        std::future::ready(None)
    }
}

/// A machine reached over SSH, whose end of the link is `mycelium link`
/// run there, speaking over the session's pipes.
#[derive(Debug, Clone)]
pub struct Ssh {
    /// Where SSH connects, such as `joel.gerber@192.168.4.32`.
    pub destination: String,
    /// SSH options, each as `-o` takes it.
    pub options: Vec<String>,
    /// The `mycelium` command on that machine.
    pub command: String,
}

impl Dial for Ssh {
    fn describe(&self) -> String {
        format!("ssh {}", self.destination)
    }

    fn dial(&self, realm: &str) -> impl Future<Output = io::Result<Connection>> + Send {
        // SSH is running once it is spawned, so there is nothing to wait
        // on here: the link's first line is the wait.
        std::future::ready(self.spawn(realm))
    }

    fn there(&self, words: &[String]) -> Vec<String> {
        let mut there = vec!["ssh".to_owned(), "-t".to_owned()];
        for option in &self.options {
            there.extend(["-o".to_owned(), option.clone()]);
        }
        there.extend(["--".to_owned(), self.destination.clone()]);
        // SSH hands the other machine one line its shell reads, so each
        // word is quoted for that shell.
        there.extend(words.iter().enumerate().map(|(at, word)| {
            if at == 0 && word == "mycelium" {
                quoted(&self.command)
            } else {
                quoted(word)
            }
        }));
        there
    }
}

/// `word` as a shell reads it back whole.
fn quoted(word: &str) -> String {
    let plain = |byte: u8| byte.is_ascii_alphanumeric() || b"-_./=:@%+,".contains(&byte);
    if !word.is_empty() && word.bytes().all(plain) {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// A machine reached through a program of its provider's, which mycelium
/// knows nothing of but this: run as `PROGRAM link --realm REALM PLACE`, it
/// carries the link over its own input and output to `mycelium link` on
/// the machine it calls `PLACE`; run as `PROGRAM run PLACE -- WORDS` from a
/// terminal, it runs `WORDS` there in that terminal; run as `PROGRAM
/// release PLACE`, it lets that machine go. What it says on its error
/// output is passed on, and a line saying the other end "turned this
/// machine away" is a refusal, waited out as SSH's are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// The program, by its path or as found on `PATH`.
    pub program: String,
    /// Which machine it reaches, in the program's own words.
    pub place: String,
}

impl Program {
    /// Reads `PROGRAM:PLACE`, as `--via` takes it, such as
    /// `aquifer-link:sandbox-1`.
    ///
    /// # Errors
    ///
    /// When either half is missing.
    pub fn parse(via: &str) -> Result<Self, String> {
        match via.split_once(':') {
            Some((program, place)) if !program.is_empty() && !place.is_empty() => Ok(Self {
                program: program.to_owned(),
                place: place.to_owned(),
            }),
            _ => Err(format!(
                "{via} isn't a program and a place, written PROGRAM:PLACE, such as aquifer-link:sandbox-1"
            )),
        }
    }
}

impl Dial for Program {
    fn describe(&self) -> String {
        format!("{} {}", self.program, self.place)
    }

    fn dial(&self, realm: &str) -> impl Future<Output = io::Result<Connection>> + Send {
        let mut program = tokio::process::Command::new(&self.program);
        program.args(["link", "--realm", realm, self.place.as_str()]);
        // As with SSH, the link's first line is the wait.
        std::future::ready(carried(program).map(|connection| Connection {
            releases: true,
            ..connection
        }))
    }

    fn there(&self, words: &[String]) -> Vec<String> {
        [self.program.as_str(), "run", self.place.as_str(), "--"]
            .map(str::to_owned)
            .into_iter()
            .chain(words.iter().cloned())
            .collect()
    }

    async fn release(&self) -> Option<io::Result<()>> {
        let ran = tokio::process::Command::new(&self.program)
            .args(["release", self.place.as_str()])
            .stdin(Stdio::null())
            .status()
            .await;
        Some(ran.and_then(|status| {
            if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "{} release {} ended {status}",
                    self.program, self.place
                )))
            }
        }))
    }
}

/// Any way a node dials, so one node links over SSH and programs at once.
#[derive(Debug, Clone)]
pub enum Way {
    /// Over SSH.
    Ssh(Ssh),
    /// Through a provider's program.
    Program(Program),
}

impl Dial for Way {
    fn describe(&self) -> String {
        match self {
            Self::Ssh(ssh) => ssh.describe(),
            Self::Program(program) => program.describe(),
        }
    }

    async fn dial(&self, realm: &str) -> io::Result<Connection> {
        match self {
            Self::Ssh(ssh) => ssh.dial(realm).await,
            Self::Program(program) => program.dial(realm).await,
        }
    }

    fn there(&self, words: &[String]) -> Vec<String> {
        match self {
            Self::Ssh(ssh) => ssh.there(words),
            Self::Program(program) => program.there(words),
        }
    }

    async fn release(&self) -> Option<io::Result<()>> {
        match self {
            Self::Ssh(ssh) => ssh.release().await,
            Self::Program(program) => program.release().await,
        }
    }
}

/// Runs `command` with the link over its input and output, passing on
/// what it says on its error output and reading that for a refusal.
fn carried(mut command: tokio::process::Command) -> io::Result<Connection> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let (Some(writing), Some(reading)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(io::Error::other("the link's program gave no pipes"));
    };
    let refused = child
        .stderr
        .take()
        .map(|said| tokio::spawn(refused::turned_away(said)));
    Ok(Connection {
        reading: Box::new(reading),
        writing: Box::new(writing),
        keep: Box::new(child),
        refused,
        releases: false,
    })
}

impl Ssh {
    fn spawn(&self, realm: &str) -> io::Result<Connection> {
        let mut ssh = tokio::process::Command::new("ssh");
        // Never a prompt, and a dead link found within 45 seconds.
        ssh.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
        ]);
        for option in &self.options {
            ssh.args(["-o", option]);
        }
        // Nothing after `--` is read as an option, whatever its name.
        ssh.arg("--").arg(&self.destination).args([
            self.command.as_str(),
            "link",
            "--realm",
            realm,
        ]);
        carried(ssh)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[test]
    fn a_way_through_a_program_names_the_program_and_its_place() {
        assert_eq!(
            Program::parse("aquifer-link:sandbox-1:2"),
            Ok(Program {
                program: "aquifer-link".to_owned(),
                place: "sandbox-1:2".to_owned(),
            }),
            "the place is everything after the first colon"
        );
        for wrong in ["aquifer-link", ":sandbox-1", "aquifer-link:"] {
            assert!(Program::parse(wrong).is_err(), "{wrong} is refused");
        }
    }

    #[test]
    fn a_program_runs_words_there_as_given() {
        let program = Program::parse("aquifer-link:sandbox-1").expect("a way");
        assert_eq!(
            program.there(&["mycelium".to_owned(), "attach".to_owned(), "a b".to_owned()]),
            [
                "aquifer-link",
                "run",
                "sandbox-1",
                "--",
                "mycelium",
                "attach",
                "a b"
            ],
            "the program is given each word apart, never a line for a shell"
        );
    }

    #[tokio::test]
    async fn a_program_carries_the_link_and_says_when_it_was_turned_away() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let asked = dir.path().join("asked");
        let program = provider(
            dir.path(),
            &format!(
                "printf '%s\\n' \"$@\" > '{}'\n\
                 echo 'the gateway turned this machine away: no login' >&2\nexec cat\n",
                asked.display()
            ),
        );
        let way = Way::Program(Program {
            program,
            place: "sandbox-1".to_owned(),
        });

        let mut connection = way.dial("work").await.expect("the program starts");
        connection
            .writing
            .write_all(b"hello\n")
            .await
            .expect("the link takes words");
        drop(connection.writing);
        let mut heard = String::new();
        connection
            .reading
            .read_to_string(&mut heard)
            .await
            .expect("the link gives words back");
        assert_eq!(
            heard, "hello\n",
            "the link runs over the program's own pipes"
        );
        let refused = connection
            .refused
            .expect("a program's error output is read")
            .await
            .expect("the reading ends");
        assert_eq!(
            refused.as_deref(),
            Some("the gateway turned this machine away: no login"),
            "a program says a refusal as SSH does"
        );
        assert_eq!(
            fs::read_to_string(&asked).expect("the provider was asked"),
            "link\n--realm\nwork\nsandbox-1\n",
            "the program is asked to link in the realm, to its place"
        );
    }

    /// A provider program at `dir`/provider running `body`, put in place by
    /// a child process, since another test's fork holding the file open
    /// for writing would keep it from running.
    fn provider(dir: &std::path::Path, body: &str) -> String {
        let text = dir.join("provider.sh");
        let program = dir.join("provider");
        fs::write(&text, format!("#!/bin/sh\n{body}")).expect("the provider is written");
        let put = std::process::Command::new("install")
            .args(["-m", "755"])
            .arg(&text)
            .arg(&program)
            .status()
            .expect("install runs");
        assert!(put.success(), "the provider is put in place");
        program.display().to_string()
    }

    #[tokio::test]
    async fn a_program_releases_the_machine_it_reaches_and_ssh_never_does() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let asked = dir.path().join("asked");
        let program = provider(
            dir.path(),
            &format!("printf '%s\\n' \"$@\" > '{}'\n", asked.display()),
        );
        let way = Way::Program(Program {
            program,
            place: "sandbox-1".to_owned(),
        });

        let released = way.release().await.expect("a program can release");

        assert!(released.is_ok(), "it is released: {released:?}");
        assert_eq!(
            fs::read_to_string(&asked).expect("the provider was asked"),
            "release\nsandbox-1\n",
            "the program is asked to release its place"
        );
        let ssh = Way::Ssh(Ssh {
            destination: "equilibrius".to_owned(),
            options: Vec::new(),
            command: "mycelium".to_owned(),
        });
        assert!(ssh.release().await.is_none(), "SSH releases nothing");
    }

    #[tokio::test]
    async fn a_release_that_fails_says_so() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let way = Way::Program(Program {
            program: provider(dir.path(), "exit 3\n"),
            place: "sandbox-1".to_owned(),
        });

        let released = way.release().await.expect("a program can release");

        assert!(released.is_err(), "a release ending 3 failed");
    }
}
