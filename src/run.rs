//! Running a command on this machine for someone waiting on it, here or on
//! another machine of the realm: its output is handed on as it comes, a
//! bounded piece at a time, so a command writing faster than its reader
//! takes is slowed by its own pipes rather than growing the node, and its
//! exit comes last. A reader that lets go stops it, and everything it
//! started, with the terminate signal.

use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;

use crate::wire::Ran;

/// The most of a command's output one [`Ran`] carries.
pub const PIECE: usize = 64 * 1024;

/// How many [`Ran`]s wait between a run and its reader at most, at each
/// step from the command to whoever asked for it.
pub const BACKLOG: usize = 4;

/// Runs `words`, the first the program, in `folder`, handing what it says
/// to `ran` as it says it and then how it ended; stops it once `ran`'s
/// reader lets go.
pub async fn here(words: Vec<String>, folder: PathBuf, ran: mpsc::Sender<Ran>) {
    let ended = match spawn(&words, folder) {
        Ok(child) => follow(child, &ran).await,
        Err(error) => Some(Ran::Failed(format!("it didn't start: {error}"))),
    };
    if let Some(ended) = ended {
        // A reader that let go meanwhile hears nothing.
        let _ = ran.send(ended).await;
    }
}

/// Starts `words` in a process group of its own, so a stop reaches all it
/// starts, with nothing on its standard input.
fn spawn(words: &[String], folder: PathBuf) -> io::Result<tokio::process::Child> {
    let (program, arguments) = words
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "nothing to run"))?;
    tokio::process::Command::new(program)
        .args(arguments)
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
}

/// Hands on what `child` says until it ends, and answers how it did, or
/// nothing once `ran`'s reader has let go and it is stopped.
async fn follow(mut child: tokio::process::Child, ran: &mpsc::Sender<Ran>) -> Option<Ran> {
    let group = child.id();
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    let (mut out_piece, mut err_piece) = (vec![0; PIECE], vec![0; PIECE]);
    while out.is_some() || err.is_some() {
        let said = tokio::select! {
            read = read_some(&mut out, &mut out_piece) => {
                let Some(read) = read else { out = None; continue };
                Ran::Out(STANDARD.encode(read))
            },
            read = read_some(&mut err, &mut err_piece) => {
                let Some(read) = read else { err = None; continue };
                Ran::Err(STANDARD.encode(read))
            },
            () = ran.closed() => return stop(group, child).await,
        };
        if ran.send(said).await.is_err() {
            return stop(group, child).await;
        }
    }
    tokio::select! {
        status = child.wait() => Some(match status {
            Ok(status) => Ran::Exited(code(status)),
            Err(error) => Ran::Failed(format!("its end couldn't be told: {error}")),
        }),
        () = ran.closed() => stop(group, child).await,
    }
}

/// What a pipe says next, or nothing once it has closed or there is none.
async fn read_some<'a>(
    pipe: &mut Option<impl AsyncRead + Unpin>,
    piece: &'a mut [u8],
) -> Option<&'a [u8]> {
    let Some(pipe) = pipe else {
        return std::future::pending().await;
    };
    match pipe.read(piece).await {
        Ok(0) | Err(_) => None,
        Ok(read) => Some(&piece[..read]),
    }
}

/// Stops the process group `group` leads, which `child` began, and waits
/// for `child` to end so it leaves no zombie.
async fn stop(group: Option<u32>, mut child: tokio::process::Child) -> Option<Ran> {
    if let Some(pid) = group
        .and_then(|group| i32::try_from(group).ok())
        .and_then(rustix::process::Pid::from_raw)
    {
        // A group that has already ended has nothing to stop.
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
    }
    let _ = child.wait().await;
    None
}

/// The exit code a shell would say for `status`: its own, or 128 and the
/// signal that ended it.
fn code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(255)
}

/// The bytes a piece of output carries.
///
/// # Errors
///
/// When it isn't base64.
pub fn bytes(piece: &str) -> Result<Vec<u8>, base64::DecodeError> {
    STANDARD.decode(piece)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// How long a test waits on a run before failing.
    const PATIENCE: Duration = Duration::from_secs(10);

    fn sh(script: &str) -> Vec<String> {
        vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()]
    }

    /// Everything a run says, its output and errors joined, and its end.
    async fn all_of(words: Vec<String>) -> (Vec<u8>, Vec<u8>, Ran) {
        let (ran, mut said) = mpsc::channel(BACKLOG);
        let folder = std::env::temp_dir();
        tokio::spawn(here(words, folder, ran));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        loop {
            let next = tokio::time::timeout(PATIENCE, said.recv())
                .await
                .expect("the run says more")
                .expect("the run ends with its end");
            match next {
                Ran::Out(piece) => out.extend(bytes(&piece).expect("base64")),
                Ran::Err(piece) => err.extend(bytes(&piece).expect("base64")),
                ended => return (out, err, ended),
            }
        }
    }

    #[tokio::test]
    async fn a_run_hands_on_its_output_and_errors_apart_then_its_own_exit_code() {
        let (out, err, ended) = all_of(sh("echo said; echo wrong >&2; exit 3")).await;
        assert_eq!(out, b"said\n");
        assert_eq!(err, b"wrong\n");
        assert_eq!(ended, Ran::Exited(3), "its exit code is its own");
    }

    #[tokio::test]
    async fn a_run_reads_nothing_and_a_signal_that_ends_it_says_so_as_a_shell_would() {
        let (out, _, ended) = all_of(sh("cat; echo read")).await;
        assert_eq!(out, b"read\n", "its input is empty, so it reads nothing");
        assert_eq!(ended, Ran::Exited(0));
        let (_, _, killed) = all_of(sh("kill -KILL $$")).await;
        assert_eq!(killed, Ran::Exited(128 + 9));
    }

    #[tokio::test]
    async fn a_program_that_isnt_there_says_it_didnt_start() {
        let (_, _, ended) = all_of(vec!["/nowhere/at/all".to_owned()]).await;
        assert!(
            matches!(&ended, Ran::Failed(why) if why.starts_with("it didn't start")),
            "{ended:?}"
        );
    }

    #[tokio::test]
    async fn a_flood_of_output_comes_whole_in_bounded_pieces() {
        let (ran, mut said) = mpsc::channel(BACKLOG);
        tokio::spawn(here(
            sh("head -c 8388608 /dev/zero"),
            std::env::temp_dir(),
            ran,
        ));
        let mut got = 0;
        loop {
            match tokio::time::timeout(PATIENCE, said.recv())
                .await
                .expect("the run says more")
                .expect("the run ends with its end")
            {
                Ran::Out(piece) => {
                    let piece = bytes(&piece).expect("base64");
                    assert!(piece.len() <= PIECE, "no piece is larger than {PIECE}");
                    got += piece.len();
                }
                Ran::Exited(code) => {
                    assert_eq!(code, 0);
                    break;
                }
                other => panic!("nothing else is said: {other:?}"),
            }
        }
        assert_eq!(got, 8 * 1024 * 1024, "all of it comes");
    }

    #[tokio::test]
    async fn a_reader_that_lets_go_stops_the_run_and_all_it_started() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let mark = folder.path().join("pid");
        let (ran, mut said) = mpsc::channel(BACKLOG);
        // A child of the shell, in its group, says its pid, then sleeps.
        let script = format!(
            "sleep 600 & echo $! > {}; echo started; wait",
            mark.display()
        );
        let run = tokio::spawn(here(sh(&script), folder.path().to_owned(), ran));
        let first = tokio::time::timeout(PATIENCE, said.recv())
            .await
            .expect("the run says something");
        assert!(matches!(first, Some(Ran::Out(_))), "{first:?}");
        let pid: i32 = std::fs::read_to_string(&mark)
            .expect("its child said its pid")
            .trim()
            .parse()
            .expect("a pid");
        drop(said);
        tokio::time::timeout(PATIENCE, run)
            .await
            .expect("the run stops once its reader lets go")
            .expect("the run ends cleanly");
        let pid = rustix::process::Pid::from_raw(pid).expect("a real pid");
        tokio::time::timeout(PATIENCE, async {
            while rustix::process::test_kill_process(pid).is_ok() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("its child is stopped too");
    }
}
