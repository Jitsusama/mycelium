//! A lend handed over a link: the end of a machine that lends to the
//! other end's hands it the budget its lend file names for it, the key
//! with it, so the other machine's sessions call their model straight
//! from there rather than through this machine. The handing end sends it
//! as the link comes up and again every [`EVERY`], so a changed file or a
//! new key reaches it within that; the handed end gives it to its node,
//! which takes it only from the machine it borrows from
//! ([`crate::wire::Request::Hands`]) and keeps it in memory alone.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use super::starts::RunSaid;
use crate::client::Client;
use crate::lend::Hands;
use crate::lend::lender::Lender;
use crate::wire::{Request, Response};

/// How often a lend is handed again over a link that stays up: soon
/// enough that a key turned over at its source reaches every machine
/// within the hour, and seldom enough that running the key's command
/// costs nothing worth counting.
pub const EVERY: Duration = Duration::from_secs(10 * 60);

/// Handing a lend to the other end of one link, for as long as it is
/// held: dropping it stops it, as the link ends.
pub(super) struct Handing(tokio::task::JoinHandle<()>);

impl Handing {
    /// Hands `machine` what `lender` says it hands it, through `saying`
    /// to the link's loop, now and every [`EVERY`] after, until the loop
    /// is gone or this is dropped. A file that hands it nothing sends
    /// nothing, and one that can't be read is said in the log each time
    /// why it can't changes, never once a turn.
    pub(super) fn start(
        lender: Arc<Lender>,
        machine: String,
        saying: mpsc::UnboundedSender<RunSaid>,
    ) -> Self {
        Self(tokio::spawn(async move {
            let mut every = tokio::time::interval(EVERY);
            let mut said_why = None;
            loop {
                every.tick().await;
                match lender.handing(&machine).await {
                    Ok(Some(handed)) => {
                        said_why = None;
                        if saying.send(RunSaid::Hands(handed)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => said_why = None,
                    Err(why) => {
                        if said_why.as_ref() != Some(&why) {
                            eprintln!("mycelium: {machine} is handed no lend: {why}");
                        }
                        said_why = Some(why);
                    }
                }
            }
        }))
    }
}

impl Drop for Handing {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Gives what `from`, the machine at the other end, `handed` this one to
/// this machine's node at `node`, which takes it only where `from` is the
/// machine it borrows from; a link carrying nothing to a node hands
/// nothing on. It runs on its own, so the link's loop never waits on the
/// node, and what the node refuses is said in the log.
pub(super) fn take(node: Option<PathBuf>, from: &str, handed: Hands) {
    let Some(node) = node else {
        eprintln!("mycelium: {from} handed this machine a lend, which no node here takes");
        return;
    };
    let from = from.to_owned();
    tokio::spawn(async move {
        let asked = Request::Hands {
            from: from.clone(),
            hands: handed,
        };
        let reply = tokio::task::spawn_blocking(move || Client::connect(&node)?.ask(&asked)).await;
        let why = match reply {
            Ok(Ok(Ok(Response::Handed))) => return,
            Ok(Ok(Ok(other))) => format!("its node answered {other:?}"),
            Ok(Ok(Err(why))) => why,
            Ok(Err(error)) => format!("its node didn't answer: {error}"),
            Err(error) => format!("handing it on stopped: {error}"),
        };
        eprintln!("mycelium: what {from} handed this machine isn't taken: {why}");
    });
}
