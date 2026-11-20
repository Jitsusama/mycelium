//! Starts and steers across a link. A start this machine's node places on
//! another machine is asked of it through the link that reaches it, which
//! the node keeps or which that machine dialled; one the other end asks of this machine is handed to this machine's own
//! node, pinned to this machine, so that node admits or refuses it on its
//! own facts as they are then. The far end can't start it itself: a
//! process SSH started sits outside its person's login session, where a
//! Mac keeps the model token. A steer, which is only a file written into
//! a session's inbox, the far end writes itself, and an end of a session
//! held there, which only signals its holder, it does itself too.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Semaphore, mpsc, oneshot};

use crate::client::Client;
use crate::hold::end::Asker;
use crate::hold::start::Kit;
use crate::link::dial::{Dial, Ssh};
use crate::placement::Need;
use crate::share::exchange::Places;
use crate::steer::Steer;
use crate::wire::{Ran, Reply, Request, Response, Run, Start, Started};

/// How long a start asked of another machine has to be answered: its
/// holder has ten seconds to hold, and the link carries the rest.
const ANSWERED_WITHIN: Duration = Duration::from_secs(30);

/// The words that join a session on the machine a link reaches, named
/// first, from this one, given the words that join it there, as
/// [`Dial::joins`] says.
pub type There = Arc<dyn Fn(&str, &[String]) -> Vec<String> + Send + Sync>;

/// What a link carries of starts.
#[derive(Clone)]
pub struct Starting {
    /// This machine's node's socket, which a start the other end asks of
    /// this machine is handed to.
    pub node: PathBuf,
    /// The realm's folder of held sessions on this machine, whose ids its
    /// sessions' presences carry, so the other end can join them.
    pub held: PathBuf,
    /// How the link carries what this machine's node asks of the other
    /// machine, if it does.
    pub asking: Option<Asking>,
    /// This machine's sessions' steer inboxes, which a steer the other
    /// end carries is written into, and whose talks' replies are carried
    /// back.
    pub steers: PathBuf,
    /// This machine's own health as the node keeps it, which the link
    /// carries in this machine's facts and says again each time it moves.
    /// None for a link another machine dialled, which watches it over the
    /// node's socket instead.
    pub health: Option<crate::own_health::Told>,
    /// What this machine's configuration declares, as its node reads it,
    /// which the link says in this machine's facts. None for a link
    /// another machine dialled, which runs apart from the node and asks it
    /// for this machine's facts instead, so it can't be told another file.
    pub declared: Option<PathBuf>,
    /// The realm's kit as the node was given it, which the link names in
    /// this machine's facts beside what it declares; read only with
    /// `declared`, since a dialled link asks the node for both.
    pub kit: Option<Kit>,
    /// The node's judgement of whether this machine's processors are
    /// saturated, which the link says in this machine's room as the node
    /// does, so every machine hears the one verdict; read only with
    /// `declared`, since a dialled link asks the node for its room.
    pub saturation: Option<Arc<crate::placement::Saturation>>,
}

/// How a link carries what this machine's node asks of the other machine.
#[derive(Clone)]
pub enum Asking {
    /// A link the node keeps, in its own process: where the node asks its
    /// starts of the other machine, and how a command runs there.
    Kept(Asks, There),
    /// A link the other machine dialled, which runs in a process SSH
    /// started rather than in the node, so it asks the node over its
    /// socket for what the node asks of that machine.
    Dialled,
}

/// What is asked of another machine.
pub(super) enum Asked {
    /// Start a program held there.
    Start(Start),
    /// Reopen a session there by pi's bare resume of it, asked only of an
    /// end whose node judges it.
    Reopen(Start),
    /// Put a steer into a session's inbox there.
    Steer(Steer),
    /// Update that machine to its configuration's main.
    Update,
    /// Run a command there, handing what it says to the sender as it
    /// comes; answered once the run is asked of the other end.
    Run(Run, mpsc::Sender<Ran>),
    /// Carry `quest`'s sessions off that machine to the machine `to`, as
    /// the person asks, once this machine's node `checked` the person
    /// asked.
    Carry {
        quest: String,
        to: String,
        checked: bool,
    },
    /// End the session held there as `held` for good, as `by` asks, who
    /// this machine's node judged.
    End { held: String, by: Asker },
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
    /// How a command runs on each machine the node has dialled, kept even
    /// when a link that machine dialled carries what it is asked instead.
    ways: HashMap<String, There>,
}

struct Reaches {
    /// Which link, so one that ends lets go only of its own.
    link: u64,
    asks: mpsc::UnboundedSender<Ask>,
    there: There,
}

/// Why a machine asked through its link answered nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsent {
    /// Why, in words.
    pub why: String,
    /// Whether the ask went to the link first, so it may still have been
    /// done there.
    pub sent: bool,
}

impl From<Unsent> for String {
    fn from(unsent: Unsent) -> Self {
        unsent.why
    }
}

impl Asks {
    /// Asks `machine` to start `start`, through the link reaching it,
    /// answering as its node does, with words that join it from here, or
    /// why no answer came. It goes without the tools it would rather have,
    /// as [`Work::asked_there`] says.
    ///
    /// # Errors
    ///
    /// When `machine` answered nothing, and whether it was asked first.
    ///
    /// [`Work::asked_there`]: crate::placement::Work::asked_there
    pub async fn ask(&self, machine: &str, start: Start) -> Result<Reply, Unsent> {
        self.start(machine, start, Asked::Start).await
    }

    /// Asks `machine` to reopen a session by `start`, pi's bare resume of
    /// it, as [`Self::ask`] asks a start, but only of a link whose far end
    /// says its node judges a reopening.
    ///
    /// # Errors
    ///
    /// As [`Self::ask`]'s.
    pub async fn reopen(&self, machine: &str, start: Start) -> Result<Reply, Unsent> {
        if self.there(machine).is_none() {
            return Err(Unsent {
                why: format!("no link reaches {machine} now to judge a reopening"),
                sent: false,
            });
        }
        self.start(machine, start, Asked::Reopen).await
    }

    async fn start(
        &self,
        machine: &str,
        mut start: Start,
        asked: fn(Start) -> Asked,
    ) -> Result<Reply, Unsent> {
        let there = self.there(machine).ok_or_else(|| Unsent {
            why: format!("no link reaches {machine} now"),
            sent: false,
        })?;
        start.work = start.work.asked_there();
        Ok(
            match self
                .send(machine, asked(start), Some(ANSWERED_WITHIN))
                .await?
            {
                Ok(Response::Started(started)) => Ok(Response::Started(Started {
                    attach: there(machine, &started.attach),
                    ..started
                })),
                other => other,
            },
        )
    }

    /// Asks `machine` to run `run`, through the link reaching it, answering
    /// what it says as it goes, or why it can't be asked. It goes without
    /// the tools it would rather have, as a start does.
    pub async fn run(&self, machine: &str, mut run: Run) -> Result<mpsc::Receiver<Ran>, String> {
        run.work = run.work.asked_there();
        let (ran, said) = mpsc::channel(crate::run::BACKLOG);
        self.send(machine, Asked::Run(run, ran), Some(ANSWERED_WITHIN))
            .await??;
        Ok(said)
    }

    /// Asks `machine` to put `steer` into its session's inbox, through the
    /// link reaching it, answering as that end does, or why no answer came.
    pub async fn steer(&self, machine: &str, steer: Steer) -> Reply {
        self.send(machine, Asked::Steer(steer), Some(ANSWERED_WITHIN))
            .await?
    }

    /// Asks `machine` to update itself to its configuration's main,
    /// through the link reaching it, answering how that ended. An update
    /// takes minutes, so the answer is waited on for as long as the link
    /// stands; a switch there may end the link first, and then the
    /// machine's `updated` fact says how it ended.
    pub async fn update(&self, machine: &str) -> Reply {
        self.send(machine, Asked::Update, None).await?
    }

    /// Asks `machine` to carry `quest`'s sessions to the machine `to`, as
    /// the person asked here, through the link reaching it, answering
    /// where each went. The person's proof goes as `checked`, which this
    /// machine's node says only once it saw the person's own login ask. A
    /// carry waits for its sessions to end, so the answer is waited on for
    /// as long as the link stands.
    pub async fn carry(&self, machine: &str, quest: &str, to: &str, checked: bool) -> Reply {
        let asked = Asked::Carry {
            quest: quest.to_owned(),
            to: to.to_owned(),
            checked,
        };
        self.send(machine, asked, None).await?
    }

    /// Asks `machine` to end the session held there as `held` for good,
    /// for `by`, as this machine's node judged who asks, through the link
    /// reaching it, answering how it ended. An end gives the session a
    /// grace to save what it holds, so it is waited on for as long as the
    /// link stands.
    pub async fn end(&self, machine: &str, held: &str, by: Asker) -> Reply {
        let asked = Asked::End {
            held: held.to_owned(),
            by,
        };
        self.send(machine, asked, None).await?
    }

    /// Sends `asked` to `machine` through the link reaching it, and waits
    /// for its answer, no longer than `within` if that says how long, or
    /// says why none came.
    async fn send(
        &self,
        machine: &str,
        asked: Asked,
        within: Option<Duration>,
    ) -> Result<Reply, Unsent> {
        let unsent = |why: String| Unsent { why, sent: false };
        let unanswered = |why: String| Unsent { why, sent: true };
        let asks = self
            .lock()
            .by_machine
            .get(machine)
            .map(|reaches| reaches.asks.clone())
            .ok_or_else(|| unsent(format!("no link reaches {machine} now")))?;
        let (answer, answered) = oneshot::channel();
        asks.send(Ask { asked, answer })
            .map_err(|_| unsent(format!("the link to {machine} has just dropped")))?;
        let answered = match within {
            Some(within) => tokio::time::timeout(within, answered)
                .await
                .map_err(|_| unanswered(format!("{machine} didn't answer within {within:?}")))?,
            None => answered.await,
        };
        answered
            .map_err(|_| unanswered(format!("the link to {machine} dropped before it answered")))
    }

    /// How a command runs on `machine` from here, while a link reaches it.
    #[must_use]
    pub fn there(&self, machine: &str) -> Option<There> {
        let links = self.lock();
        Some(links.by_machine.get(machine)?.there.clone())
    }

    /// Keeps `there` as how a command runs on `machine`, as a dial that
    /// reached it says, whether or not its link goes on to carry starts.
    pub(super) fn know(&self, machine: &str, there: There) {
        self.lock().ways.insert(machine.to_owned(), there);
    }

    /// How a command runs on `machine` from here: as a dial of the node's
    /// that reached it says, or else over SSH to the machine by its name.
    pub(super) fn way(&self, machine: &str) -> There {
        if let Some(there) = self.lock().ways.get(machine) {
            return there.clone();
        }
        let ssh = Ssh {
            destination: machine.to_owned(),
            options: Vec::new(),
            command: "mycelium".to_owned(),
        };
        Arc::new(move |machine: &str, words: &[String]| ssh.joins(machine, words))
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

/// Hands a start the other end, the machine `from`, asks of `here` to
/// this machine's node at `node`, pinned to this machine, so nothing is
/// made for it here, and as relayed, since `from`'s node already judged,
/// counted and booked it.
pub(super) async fn answer(node: PathBuf, here: String, from: String, mut start: Start) -> Reply {
    start.work.needs.push(Need::Machine(here));
    start.may_cost = false;
    let relayed = Request::Relayed { from, start };
    let asked = tokio::task::spawn_blocking(move || Client::connect(&node)?.ask(&relayed)).await;
    match asked {
        Ok(Ok(reply)) => reply,
        Ok(Err(error)) => Err(format!("its node didn't answer: {error}")),
        Err(error) => Err(format!("its start stopped: {error}")),
    }
}

/// How many pieces of a run's output the end running it sends before the
/// asking end says it has taken one, so a run writing faster than its
/// asker reads waits rather than filling the link.
pub(super) const RUN_WINDOW: usize = 8;

/// What a link's runs tell its loop, to be sent to the other end.
pub(super) enum RunSaid {
    /// What a run asked of this machine says.
    Ran(u64, Ran),
    /// A piece of a run asked of the other machine has been taken.
    Taken(u64),
    /// A run asked of the other machine has been let go.
    Unrun(u64),
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
/// `stop` is dropped, which hangs up on the node and so stops it.
pub(super) async fn run(
    (node, here): (PathBuf, String),
    mut run: Run,
    (ask, back): (u64, mpsc::UnboundedSender<RunSaid>),
    window: Arc<Semaphore>,
    mut stop: oneshot::Receiver<()>,
) {
    run.work.needs.push(Need::Machine(here));
    let running = async {
        let (reading, mut writing) = tokio::net::UnixStream::connect(&node)
            .await
            .map_err(|error| format!("its node didn't answer: {error}"))?
            .into_split();
        let mut line =
            serde_json::to_string(&Request::Run(run)).map_err(|error| error.to_string())?;
        line.push('\n');
        writing
            .write_all(line.as_bytes())
            .await
            .map_err(|error| format!("its node didn't answer: {error}"))?;
        let mut lines = BufReader::new(reading).lines();
        match serde_json::from_str::<Reply>(&said(&mut lines).await?) {
            Ok(Ok(Response::Running { .. })) => {}
            Ok(Err(why)) => return Err(why),
            other => return Err(format!("its node answered {other:?}")),
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

/// Carries `quest`'s sessions from `here` to `to`, as the other end asks
/// for the person, through this machine's node at `node`, answering where
/// each went.
pub(super) async fn carry(
    node: PathBuf,
    shares: Places,
    here: String,
    (quest, to): (String, String),
) -> Reply {
    let carried = tokio::task::spawn_blocking(move || {
        crate::carry::through::for_the_person(node, shares, (&quest, &here, &to))
    })
    .await;
    match carried {
        Ok(carried) => carried.map(Response::Carried),
        Err(error) => Err(format!("its carry stopped: {error}")),
    }
}

/// Hands an update the other end asks of `here` to this machine's node at
/// `node`, which asks its updater, and answers how the update ended. A
/// switch restarts the node, which then answers nothing; the machine's
/// `updated` fact says how it ended.
pub(super) async fn update(node: PathBuf, here: String) -> Reply {
    let asked = tokio::task::spawn_blocking(move || {
        Client::connect(&node)?.ask(&Request::Update { machine: here })
    })
    .await;
    match asked {
        Ok(Ok(reply)) => reply,
        Ok(Err(error)) => Err(format!(
            "its node stopped before the update ended, as a switch restarts it, \
             and its `updated` fact says how it ended: {error}"
        )),
        Err(error) => Err(format!("its update stopped: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::placement::Work;

    /// Work that needs and would rather have `gcloud`, and would rather
    /// run on Linux.
    fn wanting_gcloud() -> Work {
        let gcloud = Need::Tool("gcloud".to_owned());
        Work {
            needs: vec![gcloud.clone()],
            prefers: vec![gcloud, Need::System("linux".to_owned())],
            tolerates: Vec::new(),
            may_wait: false,
            lasts: None,
        }
    }

    /// What a start or a run is asked as of kestrel, through a link this
    /// test holds in its place, once `ask` asks it.
    async fn asked_of_kestrel(ask: impl AsyncFnOnce(Asks)) -> Work {
        let asks = Asks::default();
        let there: There = Arc::new(|_: &str, words: &[String]| words.to_vec());
        let (mut asked, _reaching) = asks.reach("kestrel", there);
        let ((), came) = tokio::join!(ask(asks.clone()), async {
            let Ask { asked, answer } = asked.recv().await.expect("kestrel is asked");
            let _ = answer.send(Err("only looked at".to_owned()));
            asked
        });
        match came {
            Asked::Start(start) => start.work,
            Asked::Run(run, _) => run.work,
            _ => panic!("neither a start nor a run was asked"),
        }
    }

    #[tokio::test]
    async fn a_start_or_a_run_asked_of_another_machine_prefers_no_tool_but_still_needs_one() {
        let start = Start {
            work: wanting_gcloud(),
            folder: None,
            command: vec!["true".to_owned()],
            may_cost: false,
            carries: false,
            once: None,
        };
        let run = Run {
            work: wanting_gcloud(),
            folder: None,
            command: vec!["true".to_owned()],
            by: None,
            from: None,
        };
        let started = asked_of_kestrel(async |asks: Asks| {
            let _ = asks.ask("kestrel", start).await;
        })
        .await;
        let ran = asked_of_kestrel(async |asks: Asks| {
            let _ = asks.run("kestrel", run).await;
        })
        .await;
        for (asked, work) in [("start", started), ("run", ran)] {
            assert_eq!(
                work.prefers,
                [Need::System("linux".to_owned())],
                "a node too old to read a tool may have been preferred for a {asked}"
            );
            assert_eq!(
                work.needs,
                [Need::Tool("gcloud".to_owned())],
                "only a node naming its kit can meet a {asked}'s need for a tool"
            );
        }
    }
}
