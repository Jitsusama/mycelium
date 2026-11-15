//! Whether a builder answers, found as cheaply as can be: SSH is asked
//! where its host is, as it would reach it (`ssh -G`, which reads its
//! configuration and opens nothing), and that address must send SSH's
//! greeting within a few seconds. Nothing logs in, nothing builds, and
//! nothing is sent but the connection itself. A host reached only through
//! another, by a jump or a proxy command, isn't dialled, since reaching
//! the one between proves nothing of the builder and may wake it.

use std::time::Duration;

use tokio::io::AsyncReadExt;

/// How long a builder has to send its greeting, from the connection
/// being asked for; one that answers at all answers well within it.
const ANSWERED_WITHIN: Duration = Duration::from_secs(3);

/// How long SSH may take to say where a host is.
const RESOLVED_WITHIN: Duration = Duration::from_secs(3);

/// Where SSH reaches a host, as its configuration says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Straight to this name or address and port.
    Direct {
        /// The name or address dialled.
        hostname: String,
        /// Its port.
        port: u16,
    },
    /// Through this host first, by a jump.
    Through(String),
    /// By a command SSH runs, which isn't run here.
    Proxied,
}

/// Whether a builder answered, or why that isn't known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answered {
    /// It sent SSH's greeting in time.
    Yes,
    /// It didn't: it refused, sent something else, or said nothing in
    /// time, as said.
    No(String),
    /// It wasn't dialled, for the reason said.
    Unknown(String),
}

/// The route `said`, what `ssh -G` prints, gives: a jump or a proxy
/// command before the host itself, which SSH would take first.
#[must_use]
pub fn route(said: &str) -> Option<Route> {
    let mut hostname = None;
    let mut port = 22;
    for line in said.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        let value = value.trim();
        match key.to_ascii_lowercase().as_str() {
            "proxycommand" if value != "none" => return Some(Route::Proxied),
            "proxyjump" if value != "none" => {
                let first = value.split(',').next().unwrap_or(value);
                let host = first.rsplit('@').next().unwrap_or(first);
                let host = host.split(':').next().unwrap_or(host);
                return Some(Route::Through(host.to_owned()));
            }
            "hostname" => hostname = Some(value.to_owned()),
            "port" => port = value.parse().ok()?,
            _ => {}
        }
    }
    Some(Route::Direct {
        hostname: hostname?,
        port,
    })
}

/// Whether `hostname`'s `port` sends SSH's greeting in time.
pub async fn greets(hostname: &str, port: u16) -> Answered {
    let reading = async {
        let mut stream = tokio::net::TcpStream::connect((hostname, port))
            .await
            .map_err(|error| format!("it refused: {error}"))?;
        let mut greeting = [0; 4];
        stream
            .read_exact(&mut greeting)
            .await
            .map_err(|error| format!("it hung up: {error}"))?;
        if &greeting == b"SSH-" {
            Ok(())
        } else {
            Err("it answered, but not as SSH does".to_owned())
        }
    };
    match tokio::time::timeout(ANSWERED_WITHIN, reading).await {
        Ok(Ok(())) => Answered::Yes,
        Ok(Err(why)) => Answered::No(why),
        Err(_) => Answered::No(format!(
            "it said nothing within {} seconds",
            ANSWERED_WITHIN.as_secs()
        )),
    }
}

/// Whether the builder at `host` and `port`, as a machines file names it,
/// answers, asking `ssh`, SSH's program, where it is.
pub async fn answered(ssh: &str, host: &str, port: Option<u16>) -> Answered {
    let mut asking = tokio::process::Command::new(ssh);
    asking.arg("-G");
    if let Some(port) = port {
        asking.arg("-p").arg(port.to_string());
    }
    asking
        .arg(host)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let said = match tokio::time::timeout(RESOLVED_WITHIN, asking.output()).await {
        Ok(Ok(output)) if output.status.success() => output.stdout,
        Ok(Ok(_)) => return Answered::Unknown(format!("{ssh} couldn't say where it is")),
        Ok(Err(error)) => return Answered::Unknown(format!("{ssh} didn't run: {error}")),
        Err(_) => return Answered::Unknown(format!("{ssh} took too long to say where it is")),
    };
    match route(&String::from_utf8_lossy(&said)) {
        Some(Route::Direct { hostname, port }) => greets(&hostname, port).await,
        Some(Route::Through(between)) => Answered::Unknown(format!(
            "it is reached through {between}, which isn't dialled, so as not to wake it"
        )),
        Some(Route::Proxied) => {
            Answered::Unknown("it is reached by a proxy command, which isn't run".to_owned())
        }
        None => Answered::Unknown(format!("{ssh} couldn't say where it is")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn ssh_says_a_route_straight_there_through_a_jump_or_by_a_command() {
        assert_eq!(
            route("user builder\nhostname 192.0.2.7\nport 2222\nproxycommand none\n"),
            Some(Route::Direct {
                hostname: "192.0.2.7".to_owned(),
                port: 2222
            }),
        );
        assert_eq!(
            route("hostname 127.0.0.1\nport 31022\nproxyjump builder@kestrel:2200,skiff\n"),
            Some(Route::Through("kestrel".to_owned())),
            "the first hop, the one dialled first"
        );
        assert_eq!(
            route("hostname skiff\nproxycommand nc %h %p\n"),
            Some(Route::Proxied)
        );
        assert_eq!(route("port 22\n"), None, "no host said");
    }

    /// A port bound but never listened on, which refuses, and stays this
    /// test's.
    fn refusing() -> (tokio::net::TcpSocket, u16) {
        let socket = tokio::net::TcpSocket::new_v4().expect("a socket");
        socket
            .bind("127.0.0.1:0".parse().expect("an address"))
            .expect("it binds");
        let port = socket.local_addr().expect("it has an address").port();
        (socket, port)
    }

    #[tokio::test]
    async fn a_builder_answers_only_when_it_greets_as_ssh_does() {
        let listening = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a listener binds");
        let port = listening.local_addr().expect("an address").port();
        tokio::spawn(async move {
            let (mut ssh, _) = listening.accept().await.expect("it is dialled");
            ssh.write_all(b"SSH-2.0-OpenSSH_9.9\r\n")
                .await
                .expect("it greets");
            let (mut other, _) = listening.accept().await.expect("it is dialled again");
            other.write_all(b"HTTP/1.1 400\r\n").await.expect("it says");
        });
        assert_eq!(greets("127.0.0.1", port).await, Answered::Yes);
        assert!(
            matches!(greets("127.0.0.1", port).await, Answered::No(_)),
            "something else answering isn't a builder"
        );
        let (_held, closed) = refusing();
        assert!(matches!(greets("127.0.0.1", closed).await, Answered::No(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn a_builder_saying_nothing_in_time_hasnt_answered() {
        let listening = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a listener binds");
        let port = listening.local_addr().expect("an address").port();
        let _held = tokio::spawn(async move {
            let kept = listening.accept().await;
            std::future::pending::<()>().await;
            drop(kept);
        });
        assert_eq!(
            greets("127.0.0.1", port).await,
            Answered::No("it said nothing within 3 seconds".to_owned())
        );
    }

    /// A stand-in for SSH that prints `said` for `-G`, in `folder`.
    fn ssh_saying(folder: &std::path::Path, said: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let program = folder.join("ssh");
        std::fs::write(&program, format!("#!/bin/sh\nprintf '{said}'\n")).expect("it is written");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("it runs");
        program.display().to_string()
    }

    #[tokio::test]
    async fn a_builder_reached_through_another_is_never_dialled() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let jumped = ssh_saying(folder.path(), "hostname 127.0.0.1\\nproxyjump kestrel\\n");
        assert_eq!(
            answered(&jumped, "skiff", None).await,
            Answered::Unknown(
                "it is reached through kestrel, which isn't dialled, so as not to wake it"
                    .to_owned()
            )
        );
        let unsaid = folder.path().join("nowhere").display().to_string();
        assert!(matches!(
            answered(&unsaid, "skiff", None).await,
            Answered::Unknown(_)
        ));
    }

    #[tokio::test]
    async fn a_builder_straight_there_is_dialled_where_ssh_says_it_is() {
        let listening = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a listener binds");
        let port = listening.local_addr().expect("an address").port();
        tokio::spawn(async move {
            let (mut ssh, _) = listening.accept().await.expect("it is dialled");
            ssh.write_all(b"SSH-2.0-x\r\n").await.expect("it greets");
        });
        let folder = tempfile::tempdir().expect("a temporary folder");
        let ssh = ssh_saying(
            folder.path(),
            &format!("hostname 127.0.0.1\\nport {port}\\n"),
        );
        assert_eq!(answered(&ssh, "skiff", None).await, Answered::Yes);
    }
}
