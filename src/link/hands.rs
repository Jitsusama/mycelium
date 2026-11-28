//! A lend handed over a link: the end of a machine that lends to the
//! other end's hands it the budget its lend file names for it, the key
//! with it, so the other machine's sessions call their model straight
//! from there rather than through this machine. The handing end sends it
//! as the link comes up and again every [`EVERY`], so a changed file or a
//! new key reaches it within that, and takes it back within
//! [`LOOKED_EVERY`] of a cut or of `hands` leaving its lend file. The
//! handed end takes one only over a link its far key proved, and gives
//! it to its node the way the node's asks come: from the node's own
//! process, for a link it keeps, or over the connection carrying its
//! asks, for one the other machine dialled ([`super::carried`]). The node
//! takes it only from there, and only from the machine it borrows from,
//! and keeps it in memory alone, for
//! [`crate::lend::lender::HANDED_LASTS`] past the last it was handed.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};

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

/// How often the handing end reads whether it still hands the other end
/// a budget, its cuts and its lend file both, without having the key: so
/// a cut, or `hands` dropped from its lend, takes back what was handed
/// within a minute rather than at the next [`EVERY`].
pub const LOOKED_EVERY: Duration = Duration::from_secs(60);

/// Handing a lend to the other end of one link, for as long as it is
/// held: dropping it stops it, as the link ends.
pub(super) struct Handing(tokio::task::JoinHandle<()>);

impl Handing {
    /// Hands `machine` what `lender` says it hands it, through `saying`
    /// to the link's loop, now and every [`EVERY`] after, looking every
    /// [`LOOKED_EVERY`] whether it still does, until the loop is gone or
    /// this is dropped.
    pub(super) fn start(
        lender: Arc<Lender>,
        machine: String,
        saying: mpsc::UnboundedSender<RunSaid>,
    ) -> Self {
        Self::start_paced(lender, machine, saying, (LOOKED_EVERY, EVERY))
    }

    /// Hands as [`Self::start`] does, looking and handing again at the
    /// paces `(looked, every)`. Where the lender hands `machine` nothing
    /// now, as a cut or its file says, it takes back what it handed, once;
    /// a file that can't be read takes it back too, since what it would
    /// hand can't be known, and why is said in the log each time it
    /// changes, never once a turn.
    pub(super) fn start_paced(
        lender: Arc<Lender>,
        machine: String,
        saying: mpsc::UnboundedSender<RunSaid>,
        (looked, every): (Duration, Duration),
    ) -> Self {
        Self(tokio::spawn(async move {
            let mut looking = tokio::time::interval(looked);
            let mut said_why = None;
            // When it last handed a budget that stands, and whether it
            // has said anything yet, so a link that comes up with nothing
            // to hand takes back what an older link handed.
            let mut handed: Option<tokio::time::Instant> = None;
            let mut said_any = false;
            loop {
                looking.tick().await;
                let said = match lender.hands_to(&machine) {
                    Ok(true) if handed.is_some_and(|at| at.elapsed() < every) => None,
                    Ok(true) => match lender.handing(&machine).await {
                        Ok(Some(hands)) => {
                            said_why = None;
                            handed = Some(tokio::time::Instant::now());
                            Some(hands)
                        }
                        Ok(None) => taken_back(&mut handed, said_any),
                        // A key not had leaves what was handed standing,
                        // and is asked again at the next look.
                        Err(why) => {
                            note(&machine, &mut said_why, why);
                            None
                        }
                    },
                    Ok(false) => {
                        said_why = None;
                        taken_back(&mut handed, said_any)
                    }
                    Err(why) => {
                        note(&machine, &mut said_why, why);
                        taken_back(&mut handed, said_any)
                    }
                };
                if let Some(said) = said {
                    said_any = true;
                    if saying.send(RunSaid::Hands(said)).is_err() {
                        return;
                    }
                }
            }
        }))
    }
}

/// What takes back a budget `handed` at some time, once, or at the first
/// look where nothing was `said_any` yet; none where nothing stands.
fn taken_back(handed: &mut Option<tokio::time::Instant>, said_any: bool) -> Option<Hands> {
    (handed.take().is_some() || !said_any).then(Hands::taken_back)
}

/// Says in the log why `machine` is handed nothing, where it differs from
/// what was `said_why` last.
fn note(machine: &str, said_why: &mut Option<String>, why: String) {
    if said_why.as_ref() != Some(&why) {
        eprintln!("mycelium: {machine} is handed no lend: {why}");
    }
    *said_why = Some(why);
}

impl Drop for Handing {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Handing what `from`, the machine at the other end, last handed this
/// one on to this machine's node at `node`, from the node's own process,
/// for as long as it is held: dropping it stops it, as the link ends.
pub(super) struct ToNode(tokio::task::JoinHandle<()>);

impl Drop for ToNode {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Gives what `from` last `handed` this machine to its node at `node`,
/// for a link the node keeps in its own process, which is the one asker
/// of its socket it takes a lend from. One at a time and the last alone,
/// so an older lend never reaches the node after a newer one; it runs on
/// its own, so the link's loop never waits on the node, and what the node
/// refuses is said in the log.
pub(super) fn to_node(
    node: PathBuf,
    from: &str,
    mut handed: watch::Receiver<Option<Hands>>,
) -> ToNode {
    let from = from.to_owned();
    ToNode(tokio::spawn(async move {
        while handed.changed().await.is_ok() {
            let Some(hands) = handed.borrow_and_update().clone() else {
                continue;
            };
            let asked = Request::Hands {
                from: from.clone(),
                hands,
            };
            let node = node.clone();
            let reply =
                tokio::task::spawn_blocking(move || Client::connect(&node)?.ask(&asked)).await;
            let why = match reply {
                Ok(Ok(Ok(Response::Handed))) => continue,
                Ok(Ok(Ok(other))) => format!("its node answered {other:?}"),
                Ok(Ok(Err(why))) => why,
                Ok(Err(error)) => format!("its node didn't answer: {error}"),
                Err(error) => format!("handing it on stopped: {error}"),
            };
            eprintln!("mycelium: what {from} handed this machine isn't taken: {why}");
        }
    }))
}
