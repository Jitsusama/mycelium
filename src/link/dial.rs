//! Dialling: how one machine reaches another's end of a link, as a byte
//! stream each way. SSH carries it ([`Ssh`]), or a provider's program for
//! machines SSH can't reach, such as a cloud machine behind its provider's
//! gateway ([`Program`]); [`Way`] is either, so one node links over both.

use std::io;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::task::JoinHandle;

use crate::link::refused;
use crate::rejoin;

/// A link's byte streams, and whatever must live as long as they do.
pub struct Connection {
    /// What the other end sends.
    pub reading: Box<dyn AsyncRead + Unpin + Send>,
    /// What this end sends.
    pub writing: Box<dyn AsyncWrite + Unpin + Send>,
    /// Kept until the link is over, such as the process carrying it.
    pub keep: Box<dyn Send>,
    /// What the way in said of the other machine, known once it has
    /// ended: that it turned this machine away, or that its provider says
    /// it is gone; none for a way in that never says either.
    pub said: Option<JoinHandle<Option<refused::Said>>>,
    /// Whether the way in can release the machine it reaches once that
    /// machine hands over ([`Dial::release`]).
    pub releases: bool,
    /// Whether the machine reached is a worker: one a provider lent this
    /// machine, which it asks of and which asks nothing back. Its end
    /// starts, steers and updates nothing here, is sent and served nothing
    /// this machine shares, and speaks only for its own sessions, while
    /// the mesh's messages and presence cross as with any machine.
    pub worker: bool,
    /// The name the machine reached must say in its hello, where the way
    /// in pins it rather than taking the other end's word: a made
    /// machine's, known before it links, or else the first one said over
    /// that way. None for a way whose ends prove their own names, as
    /// SSH's known hosts and the realm's keys do.
    pub named: Option<Arc<OnceLock<String>>>,
    /// How long the other end has to say hello before the link gives up:
    /// [`HELLO_WITHIN`] for an end that answers at once, or none for a way
    /// in that ends itself when it can't reach the other end, however long
    /// it takes to make it, as a provider's program does.
    pub hello_within: Option<Duration>,
}

/// How long an end that answers at once, as `mycelium link` over SSH
/// does, has to say hello.
pub const HELLO_WITHIN: Duration = Duration::from_secs(30);

/// A way to reach another machine's end of a link.
pub trait Dial: Send + Sync + 'static {
    /// Where it dials, as a person reads it.
    fn describe(&self) -> String;

    /// Reaches the other end, which runs in `realm`.
    fn dial(&self, realm: &str) -> impl Future<Output = io::Result<Connection>> + Send;

    /// The words that run `words` on the machine it reaches, from a
    /// terminal here, where `mycelium` names that machine's own.
    fn there(&self, words: &[String]) -> Vec<String>;

    /// The words that join, from a terminal here, a session on `machine`,
    /// given `joins`, the words that join it there: [`Dial::there`]'s, for
    /// a way that never says it dropped, as a provider's program doesn't.
    fn joins(&self, machine: &str, joins: &[String]) -> Vec<String> {
        let _ = machine;
        self.there(joins)
    }

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
    /// Where SSH connects, such as `jsama@192.0.2.32`.
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
        for option in self
            .options
            .iter()
            .filter(|option| !names_the_links_own(option))
        {
            there.extend(["-o".to_owned(), option.clone()]);
        }
        // After the machine's own, so an option it gives wins, as SSH
        // takes the first of each.
        for option in KEPT_ALIVE {
            if !self
                .options
                .iter()
                .any(|given| named(given) == named(option))
            {
                there.extend(["-o".to_owned(), option.to_owned()]);
            }
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

    /// A `mycelium attach --on` here, joining again each time SSH ends
    /// with its code for a dropped link ([`rejoin::DROPPED`]), so a window
    /// outlives the link it joined over.
    fn joins(&self, machine: &str, joins: &[String]) -> Vec<String> {
        rejoin::served(machine, joins, self.there(joins))
    }
}

/// The SSH options that choose which key the link signs with, the node's
/// own, as `ssh_config` names keys: a person's terminal leaves them to its
/// own `ssh_config`, which names that person's way in, since an option
/// given on the line wins over any `ssh_config` gives (ISXEG8, 21
/// November).
const THE_LINKS_OWN: [&str; 2] = ["identityagent", "identitiesonly"];

/// The SSH options a person's terminal is joined with, so a way that
/// dies unsaid, as a dock's address does when it is undocked, leaving
/// what was sent unanswered and no error, ends within 15 seconds, and
/// the join is joined again ([`rejoin`]). The link's own SSH waits 45,
/// since a link dropped costs more than a window redrawn.
pub const KEPT_ALIVE: [&str; 2] = ["ServerAliveInterval=5", "ServerAliveCountMax=3"];

/// Whether `option`, as `-o` takes it, chooses the link's own key
/// ([`THE_LINKS_OWN`]).
fn names_the_links_own(option: &str) -> bool {
    let name = named(option);
    THE_LINKS_OWN
        .iter()
        .any(|own| own.eq_ignore_ascii_case(&name))
}

/// The name `option`, as `-o` takes it, gives, in lower case as SSH
/// reads names.
#[must_use]
pub fn named(option: &str) -> String {
    option
        .trim_start()
        .split(|at: char| at == '=' || at.is_whitespace())
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
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
/// machine away" is a refusal, waited out as SSH's are. It names no code
/// for a way that dropped, as SSH's 255 is, so a window joined through it
/// isn't joined again when its way drops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// The program, by its path or as found on `PATH`.
    pub program: String,
    /// Which machine it reaches, in the program's own words.
    pub place: String,
    /// The name the machine it reaches says for itself: the one its
    /// provider said it made, or else the first it says, kept across
    /// redials so a machine can't come back as another.
    pub named: Arc<OnceLock<String>>,
    /// Whether the machine it reaches is a worker ([`Connection::worker`]),
    /// as every one is but a machine the node made for the person, which
    /// is theirs as any machine of the realm is.
    pub worker: bool,
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
                named: Arc::default(),
                worker: true,
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
        // As with SSH, the link's first line is the wait, but as long as
        // the program runs: it may first make the machine and put the far
        // end there, which takes minutes, and it ends when it can't.
        std::future::ready(carried(program).map(|connection| Connection {
            releases: true,
            worker: self.worker,
            named: Some(self.named.clone()),
            hello_within: None,
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
            // A release given up on ends with its asker, rather than
            // running on beside the one asked after it.
            .kill_on_drop(true)
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

    fn joins(&self, machine: &str, joins: &[String]) -> Vec<String> {
        match self {
            Self::Ssh(ssh) => ssh.joins(machine, joins),
            Self::Program(program) => program.joins(machine, joins),
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
    let said = child
        .stderr
        .take()
        .map(|said| tokio::spawn(refused::turned_away(said)));
    Ok(Connection {
        reading: Box::new(reading),
        writing: Box::new(writing),
        keep: Box::new(child),
        said,
        releases: false,
        worker: false,
        named: None,
        hello_within: Some(HELLO_WITHIN),
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
                named: Arc::default(),
                worker: true,
            }),
            "the place is everything after the first colon"
        );
        for wrong in ["aquifer-link", ":sandbox-1", "aquifer-link:"] {
            assert!(Program::parse(wrong).is_err(), "{wrong} is refused");
        }
    }

    // WCJQSG (2026-11-21): a person's window to a Mac was refused, as the
    // node's own agent, named on the line, beat the one `ssh_config` names
    // for that Mac.
    #[test]
    fn a_terminal_leaves_the_key_to_its_own_ssh_config_but_keeps_the_rest() {
        let ssh = Ssh {
            destination: "skiff".to_owned(),
            options: [
                "IdentityAgent=SSH_AUTH_SOCK",
                "AddKeysToAgent=no",
                "identitiesonly no",
                "ForwardAgent=no",
            ]
            .map(str::to_owned)
            .to_vec(),
            command: "mycelium".to_owned(),
        };
        assert_eq!(
            ssh.there(&[
                "mycelium".to_owned(),
                "attach".to_owned(),
                "5e55".to_owned()
            ]),
            [
                "ssh",
                "-t",
                "-o",
                "AddKeysToAgent=no",
                "-o",
                "ForwardAgent=no",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=3",
                "--",
                "skiff",
                "mycelium",
                "attach",
                "5e55"
            ],
            "no key of the link's, and still no agent forwarded or added to"
        );
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
            named: Arc::default(),
            worker: true,
        });

        let mut connection = way.dial("work").await.expect("the program starts");
        assert_eq!(
            connection.hello_within, None,
            "the link waits on the program as long as it runs, setting the machine up"
        );
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
        let said = connection
            .said
            .expect("a program's error output is read")
            .await
            .expect("the reading ends");
        assert_eq!(
            said,
            Some(refused::Said::TurnedAway(
                "the gateway turned this machine away: no login".to_owned()
            )),
            "a program says a refusal as SSH does"
        );
        assert_eq!(
            fs::read_to_string(&asked).expect("the provider was asked"),
            "link\n--realm\nwork\nsandbox-1\n",
            "the program is asked to link in the realm, to its place"
        );
    }

    /// A provider program at `dir`/provider running `body`.
    fn provider(dir: &std::path::Path, body: &str) -> String {
        let program = dir.join("provider");
        crate::stand_in::program(&program, &format!("#!/bin/sh\n{body}"));
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
            named: Arc::default(),
            worker: true,
        });

        let released = way.release().await.expect("a program can release");

        assert!(released.is_ok(), "it is released: {released:?}");
        assert_eq!(
            fs::read_to_string(&asked).expect("the provider was asked"),
            "release\nsandbox-1\n",
            "the program is asked to release its place"
        );
        let ssh = Way::Ssh(Ssh {
            destination: "kestrel".to_owned(),
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
            named: Arc::default(),
            worker: true,
        });

        let released = way.release().await.expect("a program can release");

        assert!(released.is_err(), "a release ending 3 failed");
    }
}
