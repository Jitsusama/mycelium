//! Starts and steers across a link. A start this machine's node places on
//! another machine is asked of it through the link the node keeps to it;
//! one the other end asks of this machine is handed to this machine's own
//! node, pinned to this machine, so that node admits or refuses it on its
//! own facts as they are then. The far end can't start it itself: a
//! process SSH started sits outside its person's login session, where a
//! Mac keeps the model token. A steer, which is only a file written into
//! a session's inbox, the far end writes itself.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use crate::client::Client;
use crate::placement::Need;
use crate::steer::Steer;
use crate::wire::{Reply, Request, Response, Start, Started};

/// How long a start asked of another machine has to be answered: its
/// holder has ten seconds to hold, and the link carries the rest.
const ANSWERED_WITHIN: Duration = Duration::from_secs(30);

/// The words that run a command on the machine a link reaches, from this
/// one, given the command's own words there.
pub type There = Arc<dyn Fn(&[String]) -> Vec<String> + Send + Sync>;

/// What a link carries of starts.
#[derive(Clone)]
pub struct Starting {
    /// This machine's node's socket, which a start the other end asks of
    /// this machine is handed to.
    pub node: PathBuf,
    /// The realm's folder of held sessions on this machine, whose ids its
    /// sessions' presences carry, so the other end can join them.
    pub held: PathBuf,
    /// For a link this machine's node keeps: where the node asks its
    /// starts of the other machine, and how a command runs there.
    pub asking: Option<(Asks, There)>,
    /// This machine's sessions' steer inboxes, which a steer the other
    /// end carries is written into, and whose talks' replies are carried
    /// back.
    pub steers: PathBuf,
}

/// What is asked of another machine.
pub(super) enum Asked {
    /// Start a program held there.
    Start(Start),
    /// Put a steer into a session's inbox there.
    Steer(Steer),
}

/// Something asked of another machine, and where its answer goes.
pub(super) struct Ask {
    pub(super) asked: Asked,
    pub(super) answer: oneshot::Sender<Reply>,
}

/// The links this machine's node asks starts through, by the machine
/// each reaches.
#[derive(Clone, Default)]
pub struct Asks(Arc<Mutex<Links>>);

#[derive(Default)]
struct Links {
    next: u64,
    by_machine: HashMap<String, Reaches>,
}

struct Reaches {
    /// Which link, so one that ends lets go only of its own.
    link: u64,
    asks: mpsc::UnboundedSender<Ask>,
    there: There,
}

impl Asks {
    /// Asks `machine` to start `start`, through the link reaching it,
    /// answering as its node does, with words that join it from here, or
    /// why no answer came.
    pub async fn ask(&self, machine: &str, start: Start) -> Reply {
        let there = self
            .there(machine)
            .ok_or_else(|| format!("no link reaches {machine} now"))?;
        match self.send(machine, Asked::Start(start)).await? {
            Ok(Response::Started(started)) => Ok(Response::Started(Started {
                attach: there(&started.attach),
                ..started
            })),
            other => other,
        }
    }

    /// Asks `machine` to put `steer` into its session's inbox, through the
    /// link reaching it, answering as that end does, or why no answer came.
    pub async fn steer(&self, machine: &str, steer: Steer) -> Reply {
        self.send(machine, Asked::Steer(steer)).await?
    }

    /// Sends `asked` to `machine` through the link reaching it, and waits
    /// for its answer, or says why none came.
    async fn send(&self, machine: &str, asked: Asked) -> Result<Reply, String> {
        let asks = self
            .lock()
            .by_machine
            .get(machine)
            .map(|reaches| reaches.asks.clone())
            .ok_or_else(|| format!("no link reaches {machine} now"))?;
        let (answer, answered) = oneshot::channel();
        asks.send(Ask { asked, answer })
            .map_err(|_| format!("the link to {machine} has just dropped"))?;
        tokio::time::timeout(ANSWERED_WITHIN, answered)
            .await
            .map_err(|_| format!("{machine} didn't answer within {ANSWERED_WITHIN:?}"))?
            .map_err(|_| format!("the link to {machine} dropped before it answered"))
    }

    /// How a command runs on `machine` from here, while a link reaches it.
    #[must_use]
    pub fn there(&self, machine: &str) -> Option<There> {
        let links = self.lock();
        Some(links.by_machine.get(machine)?.there.clone())
    }

    /// Takes the starts asked of `machine` for the link reaching it, until
    /// what this answers is dropped.
    pub(super) fn reach(
        &self,
        machine: &str,
        there: There,
    ) -> (mpsc::UnboundedReceiver<Ask>, Reaching) {
        let (asks, asked) = mpsc::unbounded_channel();
        let mut links = self.lock();
        let link = links.next;
        links.next += 1;
        links
            .by_machine
            .insert(machine.to_owned(), Reaches { link, asks, there });
        let reaching = Reaching {
            asks: self.clone(),
            machine: machine.to_owned(),
            link,
        };
        (asked, reaching)
    }

    fn lock(&self) -> MutexGuard<'_, Links> {
        // A panic while holding the lock leaves the links as they were
        // before it, so carrying on with them is sound.
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A link reaching a machine, for as long as it is held.
pub(super) struct Reaching {
    asks: Asks,
    machine: String,
    link: u64,
}

impl Drop for Reaching {
    fn drop(&mut self) {
        let mut links = self.asks.lock();
        if links
            .by_machine
            .get(&self.machine)
            .is_some_and(|reaches| reaches.link == self.link)
        {
            links.by_machine.remove(&self.machine);
        }
    }
}

/// Hands a start the other end asks of `here` to this machine's node at
/// `node`, pinned to this machine.
pub(super) async fn answer(node: PathBuf, here: String, mut start: Start) -> Reply {
    start.work.needs.push(Need::Machine(here));
    let asked =
        tokio::task::spawn_blocking(move || Client::connect(&node)?.ask(&Request::Start(start)))
            .await;
    match asked {
        Ok(Ok(reply)) => reply,
        Ok(Err(error)) => Err(format!("its node didn't answer: {error}")),
        Err(error) => Err(format!("its start stopped: {error}")),
    }
}
