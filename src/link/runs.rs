//! Runs across a link. A run the other end asks of this machine is
//! handed to this machine's own node, pinned to this machine, and what it
//! says is sent back a piece at a time, no faster than the asker takes
//! it; a refusal names this machine, since the asker would otherwise read
//! it as its own node's.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Semaphore, mpsc, oneshot};

use crate::lend::Lent;
use crate::placement::Need;
use crate::wire::{Ran, Reply, Request, Response, Run};

/// How many pieces of a run's output the end running it sends before the
/// asking end says it has taken one, so a run writing faster than its
/// asker reads waits rather than filling the link.
pub(super) const RUN_WINDOW: usize = 8;

/// What a link's runs tell its loop, to be sent to the other end.
pub(super) enum RunSaid {
    /// What a run asked of this machine says.
    Ran(u64, Ran),
    /// A piece of a run or a lend asked of the other machine has been
    /// taken.
    Taken(u64),
    /// A run asked of the other machine has been let go.
    Unrun(u64),
    /// What this machine's lender answers a lend the other end asked.
    Lent(u64, Lent),
    /// A lend asked of the other machine has been let go.
    Unlend(u64),
}

/// A run the other end asked of this machine, while it runs.
pub(super) struct RunningHere {
    /// How many more pieces of its output may be sent before the other
    /// end takes one.
    pub(super) window: Arc<Semaphore>,
    /// Dropped to stop it.
    pub(super) _stop: oneshot::Sender<()>,
}

/// Hands a run the other end asks of `here` to this machine's node at
/// `node`, pinned to this machine, and sends what it says to `back` as
/// `ask`'s, a piece of output only once `window` lets it, until it ends or
/// `stop` is dropped, which hangs up on the node and so stops it. A
/// refusal names `here`, since the asker reads it as its own node's
/// otherwise, and the person's run sent to a room as an agent's would
/// read as refused by the machine that sent it.
pub(super) async fn run(
    (node, here): (PathBuf, String),
    mut run: Run,
    (ask, back): (u64, mpsc::UnboundedSender<RunSaid>),
    window: Arc<Semaphore>,
    mut stop: oneshot::Receiver<()>,
) {
    run.work.needs.push(Need::Machine(here.clone()));
    let running = async {
        let (reading, mut writing) = tokio::net::UnixStream::connect(&node)
            .await
            .map_err(|error| format!("{here}'s node didn't answer: {error}"))?
            .into_split();
        let mut line =
            serde_json::to_string(&Request::Run(run)).map_err(|error| error.to_string())?;
        line.push('\n');
        writing
            .write_all(line.as_bytes())
            .await
            .map_err(|error| format!("{here}'s node didn't answer: {error}"))?;
        let mut lines = BufReader::new(reading).lines();
        match serde_json::from_str::<Reply>(&said(&mut lines).await?) {
            Ok(Ok(Response::Running { .. })) => {}
            Ok(Err(why)) => return Err(format!("{here} refused the run: {why}")),
            other => return Err(format!("{here}'s node answered {other:?}")),
        }
        loop {
            let ran = serde_json::from_str::<Ran>(&said(&mut lines).await?)
                .map_err(|error| format!("its node said what isn't a run's: {error}"))?;
            if !ran.ends() {
                // The window is never closed while the run holds it.
                if let Ok(piece) = window.acquire().await {
                    piece.forget();
                }
            }
            let ends = ran.ends();
            if back.send(RunSaid::Ran(ask, ran)).is_err() || ends {
                return Ok(());
            }
        }
    };
    tokio::select! {
        ran = running => if let Err(why) = ran {
            // A link that dropped meanwhile has nobody to tell.
            let _ = back.send(RunSaid::Ran(ask, Ran::Failed(why)));
        },
        // Hanging up on the node stops the run.
        _ = &mut stop => {}
    }
}

/// The next line a node says, or that it stopped answering.
async fn said(
    lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
) -> Result<String, String> {
    lines
        .next_line()
        .await
        .ok()
        .flatten()
        .ok_or_else(|| "its node stopped answering".to_owned())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::placement::Work;

    // Followup 535: the asker reads a run's refusal as its own node's, so
    // the room's must say it was the room's.
    #[tokio::test]
    async fn a_run_its_node_refuses_names_the_machine_that_refused() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let node = dir.path().join("node.sock");
        let listening = tokio::net::UnixListener::bind(&node).expect("the node listens");
        let refusing = tokio::spawn(async move {
            let (mut stream, _) = listening.accept().await.expect("the run is handed over");
            let (reading, mut writing) = stream.split();
            let asked = BufReader::new(reading).lines().next_line().await;
            let refused: Reply = Err("nothing here runs an agent's command".to_owned());
            let mut line = serde_json::to_string(&refused).expect("a reply is JSON");
            line.push('\n');
            writing
                .write_all(line.as_bytes())
                .await
                .expect("the node answers");
            asked
        });
        let asked_of_osprey = Run {
            work: Work {
                needs: Vec::new(),
                prefers: Vec::new(),
                tolerates: Vec::new(),
                may_wait: false,
                lasts: None,
                fence: None,
            },
            folder: None,
            command: vec!["true".to_owned()],
            by: None,
            from: Some("kestrel".to_owned()),
            asked_by: None,
        };
        let (back, mut said) = mpsc::unbounded_channel();
        let (_stop, stop) = oneshot::channel();
        let window = Arc::new(Semaphore::new(RUN_WINDOW));
        let running = run(
            (node, "osprey".to_owned()),
            asked_of_osprey,
            (7, back),
            window,
            stop,
        );
        tokio::time::timeout(Duration::from_secs(30), running)
            .await
            .expect("a refused run ends");
        let asked = refusing.await.expect("the node answers");
        assert!(
            asked.is_ok_and(|line| line.is_some_and(|line| line.contains("\"osprey\""))),
            "the run is pinned to the machine it was asked of"
        );
        let Ok(RunSaid::Ran(7, Ran::Failed(why))) = said.try_recv() else {
            panic!("the refusal is said as the run's end");
        };
        assert_eq!(
            why, "osprey refused the run: nothing here runs an agent's command",
            "the asker is told which machine refused, not left to take it as its own"
        );
    }
}
