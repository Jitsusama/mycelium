//! Dialling: how one machine reaches another's end of a link, as a byte
//! stream each way. SSH carries it today ([`Ssh`]); a connection of the
//! node's own, or a provider's for machines such as Aquifer's, plugs in
//! behind the same trait.

use std::io;
use std::process::Stdio;

use tokio::io::{AsyncRead, AsyncWrite};

/// A link's byte streams, and whatever must live as long as they do.
pub struct Connection {
    /// What the other end sends.
    pub reading: Box<dyn AsyncRead + Unpin + Send>,
    /// What this end sends.
    pub writing: Box<dyn AsyncWrite + Unpin + Send>,
    /// Kept until the link is over, such as the process carrying it.
    pub keep: Box<dyn Send>,
}

/// A way to reach another machine's end of a link.
pub trait Dial: Send + Sync + 'static {
    /// Where it dials, as a person reads it.
    fn describe(&self) -> String;

    /// Reaches the other end, which runs in `realm`.
    fn dial(&self, realm: &str) -> impl Future<Output = io::Result<Connection>> + Send;
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
        ssh.arg(&self.destination)
            .args([self.command.as_str(), "link", "--realm", realm])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        let mut child = ssh.spawn()?;
        let (Some(writing), Some(reading)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(io::Error::other("ssh gave no pipes"));
        };
        Ok(Connection {
            reading: Box::new(reading),
            writing: Box::new(writing),
            keep: Box::new(child),
        })
    }
}
