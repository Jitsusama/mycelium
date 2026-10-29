//! Dialling: how one machine reaches another's end of a link, as a byte
//! stream each way. SSH carries it today ([`Ssh`]); a connection of the
//! node's own, or a provider's for machines such as Aquifer's, plugs in
//! behind the same trait.

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
        ssh.arg("--")
            .arg(&self.destination)
            .args([self.command.as_str(), "link", "--realm", realm])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Read to tell a refused key from a machine out of reach, and
            // passed on as SSH would have said it.
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = ssh.spawn()?;
        let (Some(writing), Some(reading)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(io::Error::other("ssh gave no pipes"));
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
        })
    }
}
