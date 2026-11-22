//! Who started each of this machine's held sessions, as its node's book of
//! starts says, which an end of a link says beside their presences so the
//! other machine's records name the starter too.
//!
//! An end asks the node over its socket, as it asks what the node measures
//! this machine to be, since an end SSH started runs apart from the node
//! and its book. The ask runs in a task of its own, so a node slow to
//! answer holds back none of the link's frames: presences go out with the
//! starters known so far, and again once an answer names one more. A held
//! id the node names a starter for is kept, as a book's line for a start
//! never changes; one it names none for is asked about again, but no
//! oftener than [`ASKED_AGAIN_AFTER`], with the next presence or the next
//! minute's keep, whichever comes first.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::wire::{Reply, Request, Response};

/// How long a held id the node named no starter for waits before it is
/// asked about again. A start's line is written to the book as its holder
/// answers, which can come after its session's presence, so one asked too
/// soon is asked again; the person's sessions, which the book never names,
/// are asked about only this often.
const ASKED_AGAIN_AFTER: Duration = Duration::from_secs(30);

/// How long an ask waits for the node to answer. The node answers from the
/// book it holds, so one slower than this is stuck, and the ids asked of it
/// wait to be asked again.
pub(super) const ANSWERED_WITHIN: Duration = Duration::from_secs(5);

/// Why the node named no starters for an ask, each told once by an end so
/// a machine whose records say none can be told apart from one whose node
/// can't say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Unasked {
    /// Nothing answered on the node's socket, as while it restarts, or the
    /// node let the ask go without a word.
    NoNode,
    /// The node took the ask and said nothing within [`ANSWERED_WITHIN`].
    Unanswered,
    /// The node can't read the ask, as one older than
    /// [`Request::StartedBy`] says.
    Refused,
    /// The node answered the ask with an error of its own, or with
    /// something no node answers it with.
    Failed,
}

impl fmt::Display for Unasked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoNode => f.write_str("no node answers on its socket"),
            Self::Unanswered => write!(
                f,
                "its node took the ask and didn't answer within {} seconds",
                ANSWERED_WITHIN.as_secs()
            ),
            Self::Refused => {
                f.write_str("its node can't read the ask, as one older than this end can't")
            }
            Self::Failed => f.write_str("its node answered the ask with an error"),
        }
    }
}

/// What an end tells of an ask's answer, where it tells anything: a way
/// asks found nothing, the first time they do, or that its node names
/// starters again after one was told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Told {
    /// Asks found nothing this way, for the first time since the node
    /// last answered.
    Unasked(Unasked),
    /// The node answers again, after a way it couldn't was told.
    Again,
}

/// What one ask of the node found: the held ids it asked about, and the
/// starter of each the node named one for, or why none were named.
#[derive(Debug)]
pub(super) struct Learned {
    due: Vec<String>,
    answer: Result<BTreeMap<String, String>, Unasked>,
}

/// The starters an end knows of its machine's held sessions.
#[derive(Debug)]
pub(super) struct Starters {
    /// The starter of each held id the node named one for.
    named: BTreeMap<String, String>,
    /// When each held id the node named none for was last answered.
    unnamed: BTreeMap<String, Instant>,
    /// Whether an ask is out, so no second one goes while it is.
    asking: bool,
    /// Why asks found nothing since the node last answered, each kind
    /// told once.
    told: BTreeSet<Unasked>,
    /// Where each ask's answer goes, for the link's loop.
    learning: mpsc::UnboundedSender<Learned>,
}

impl Starters {
    /// None known yet, and where the answers to asks come out.
    pub(super) fn new() -> (Self, mpsc::UnboundedReceiver<Learned>) {
        let (learning, learned) = mpsc::unbounded_channel();
        let starters = Self {
            named: BTreeMap::new(),
            unnamed: BTreeMap::new(),
            asking: false,
            told: BTreeSet::new(),
            learning,
        };
        (starters, learned)
    }

    /// Forgets every id not among `living`, and says which of them are due
    /// an ask at `now`: those never asked about, and those named no starter
    /// at least [`ASKED_AGAIN_AFTER`] before.
    fn due(&mut self, living: &[&str], now: Instant) -> Vec<String> {
        self.named.retain(|held, _| living.contains(&held.as_str()));
        self.unnamed
            .retain(|held, _| living.contains(&held.as_str()));
        living
            .iter()
            .filter(|held| !self.named.contains_key(**held))
            .filter(|held| {
                self.unnamed
                    .get(**held)
                    .is_none_or(|asked| now.duration_since(*asked) >= ASKED_AGAIN_AFTER)
            })
            .map(|held| (*held).to_owned())
            .collect()
    }

    /// Asks the node on `node` who started the sessions held as `living`
    /// that are due an ask, in a task of its own, unless an ask is out
    /// already; its answer comes out where [`Self::new`] said.
    pub(super) fn ask(&mut self, node: &Path, living: &[&str]) {
        if self.asking {
            return;
        }
        let due = self.due(living, Instant::now());
        if due.is_empty() {
            return;
        }
        self.asking = true;
        let node = node.to_owned();
        let learning = self.learning.clone();
        tokio::spawn(async move {
            let answer = asked(&node, due.clone()).await;
            // The link has ended once nothing hears this.
            let _ = learning.send(Learned { due, answer });
        });
    }

    /// Keeps what one ask found, at `now`, saying what the end tells of
    /// it: why it found nothing, the first time asks find nothing that way
    /// since the node last answered, or that the node answers again.
    pub(super) fn learned(&mut self, learned: Learned, now: Instant) -> Option<Told> {
        self.asking = false;
        let Learned { due, answer } = learned;
        let (named, told) = match answer {
            Ok(named) => {
                let again = !self.told.is_empty();
                self.told.clear();
                (named, again.then_some(Told::Again))
            }
            Err(why) => (
                BTreeMap::new(),
                self.told.insert(why).then_some(Told::Unasked(why)),
            ),
        };
        for held in due {
            match named.get(&held) {
                Some(by) => {
                    self.unnamed.remove(&held);
                    self.named.insert(held, by.clone());
                }
                None => {
                    self.unnamed.insert(held, now);
                }
            }
        }
        told
    }

    /// The starter of the session held as `held`, as the node last named.
    pub(super) fn of(&self, held: &str) -> Option<&str> {
        self.named.get(held).map(String::as_str)
    }
}

/// The starter of each of `held` that the node on `node` names one for, or
/// why it named none.
async fn asked(node: &PathBuf, held: Vec<String>) -> Result<BTreeMap<String, String>, Unasked> {
    let asking = async {
        let (reading, mut writing) = tokio::net::UnixStream::connect(node)
            .await
            .map_err(|_| Unasked::NoNode)?
            .into_split();
        let mut ask = serde_json::to_vec(&Request::StartedBy(held)).map_err(|_| Unasked::Failed)?;
        ask.push(b'\n');
        writing.write_all(&ask).await.map_err(|_| Unasked::NoNode)?;
        let line = BufReader::new(reading)
            .lines()
            .next_line()
            .await
            .map_err(|_| Unasked::NoNode)?
            .ok_or(Unasked::NoNode)?;
        match serde_json::from_str::<Reply>(&line) {
            Ok(Ok(Response::StartedBy(named))) => Ok(named),
            // What a node says of an ask it can't read.
            Ok(Err(why)) if why.starts_with("not a request") => Err(Unasked::Refused),
            _ => Err(Unasked::Failed),
        }
    };
    tokio::time::timeout(ANSWERED_WITHIN, asking)
        .await
        .unwrap_or(Err(Unasked::Unanswered))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// How a stand-in node on a socket answers each ask.
    #[derive(Clone)]
    enum Answers {
        /// With the starters it holds as the ask comes.
        Naming(Arc<Mutex<BTreeMap<String, String>>>),
        /// As a node older than the ask does.
        Refusing,
        /// With an error of its own.
        Failing,
        /// By letting the ask go without a word.
        HangingUp,
        /// Never, though it takes the ask.
        Silent,
    }

    /// A node on `socket` answering each ask as `answers` says.
    fn node(socket: &Path, answers: Answers) {
        let listener = tokio::net::UnixListener::bind(socket).expect("the socket binds");
        tokio::spawn(async move {
            let mut kept = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                let (reading, mut writing) = stream.into_split();
                let mut lines = BufReader::new(reading).lines();
                let Ok(Some(line)) = lines.next_line().await else {
                    continue;
                };
                let answer: Reply = match (&answers, serde_json::from_str(&line)) {
                    (Answers::Silent, _) => {
                        // Held open, so the asker waits on an answer.
                        kept.push(writing);
                        continue;
                    }
                    (Answers::HangingUp, _) => continue,
                    (Answers::Failing, _) => Err("the book can't be read".to_owned()),
                    (Answers::Naming(named), Ok(Request::StartedBy(held))) => {
                        let named = named.lock().expect("the stand-in's starters");
                        Ok(Response::StartedBy(
                            held.into_iter()
                                .filter_map(|one| Some((one.clone(), named.get(&one)?.clone())))
                                .collect(),
                        ))
                    }
                    // What an older node says of an ask it can't read.
                    _ => Err(format!("not a request: {line}")),
                };
                let mut said = serde_json::to_vec(&answer).expect("an answer");
                said.push(b'\n');
                let _ = writing.write_all(&said).await;
            }
        });
    }

    /// What `starters` tells, asking the node on `socket` of `living`, as
    /// the link's loop takes the answer, kept as answered long enough ago
    /// that each id it names no starter for is due again.
    async fn learn(
        starters: &mut Starters,
        learned: &mut mpsc::UnboundedReceiver<Learned>,
        socket: &Path,
        living: &[&str],
    ) -> Option<Told> {
        starters.ask(socket, living);
        let answer = learned.recv().await.expect("the ask is answered");
        starters.learned(answer, Instant::now() - ASKED_AGAIN_AFTER)
    }

    #[test]
    fn an_unnamed_id_is_due_again_only_once_asked_again_after_has_passed() {
        let (mut starters, _learned) = Starters::new();
        let now = Instant::now();
        assert_eq!(starters.due(&["h-1"], now), ["h-1"], "never asked about");
        let answer = Learned {
            due: vec!["h-1".to_owned(), "h-2".to_owned()],
            answer: Ok(BTreeMap::from([("h-2".to_owned(), "OAR001".to_owned())])),
        };
        starters.learned(answer, now);
        let soon = now + ASKED_AGAIN_AFTER - Duration::from_secs(1);
        assert!(
            starters.due(&["h-1", "h-2"], soon).is_empty(),
            "one named is kept, and one unnamed waits"
        );
        assert_eq!(
            starters.due(&["h-1", "h-2"], now + ASKED_AGAIN_AFTER),
            ["h-1"],
            "the unnamed one is due again once the wait has passed"
        );
        starters.due(&["h-1"], now);
        assert_eq!(
            starters.of("h-2"),
            None,
            "an id no longer living is forgotten"
        );
    }

    #[tokio::test]
    async fn a_starter_booked_after_its_first_ask_is_named_by_the_next() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("work.sock");
        let named = Arc::new(Mutex::new(BTreeMap::new()));
        node(&socket, Answers::Naming(Arc::clone(&named)));
        let (mut starters, mut learned) = Starters::new();
        let why = learn(&mut starters, &mut learned, &socket, &["h-1"]).await;
        assert_eq!(
            (starters.of("h-1"), why),
            (None, None),
            "the book names none yet"
        );
        named
            .lock()
            .expect("the stand-in's starters")
            .insert("h-1".to_owned(), "OAR001".to_owned());
        learn(&mut starters, &mut learned, &socket, &["h-1"]).await;
        assert_eq!(
            starters.of("h-1"),
            Some("OAR001"),
            "the book's line, once written"
        );
    }

    #[tokio::test]
    async fn one_ask_is_out_at_a_time() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("work.sock");
        node(&socket, Answers::Naming(Arc::default()));
        let (mut starters, mut learned) = Starters::new();
        starters.ask(&socket, &["h-1"]);
        starters.ask(&socket, &["h-1", "h-2"]);
        let first = learned.recv().await.expect("the first ask is answered");
        assert_eq!(first.due, ["h-1"], "the second waits for the first");
        starters.learned(first, Instant::now());
        starters.ask(&socket, &["h-1", "h-2"]);
        let next = learned.recv().await.expect("the next ask is answered");
        assert_eq!(
            next.due,
            ["h-2"],
            "then asks only of the one not asked about"
        );
    }

    #[tokio::test]
    async fn a_node_that_cant_say_names_no_starter_and_says_why_once_until_it_says_again() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut starters, mut learned) = Starters::new();
        let nobody = dir.path().join("nobody.sock");
        let first = learn(&mut starters, &mut learned, &nobody, &["h-1"]).await;
        assert_eq!(
            (starters.of("h-1"), first),
            (None, Some(Told::Unasked(Unasked::NoNode)))
        );
        let again = learn(&mut starters, &mut learned, &nobody, &["h-1"]).await;
        assert_eq!(again, None, "told once, not with every ask");

        for (answers, why, said) in [
            (
                Answers::Refusing,
                Unasked::Refused,
                "an older node can't read the ask",
            ),
            (
                Answers::Failing,
                Unasked::Failed,
                "a node erring is told apart",
            ),
        ] {
            let socket = dir.path().join(format!("{why:?}.sock"));
            node(&socket, answers);
            let told = learn(&mut starters, &mut learned, &socket, &["h-2"]).await;
            assert_eq!(
                (starters.of("h-2"), told),
                (None, Some(Told::Unasked(why))),
                "{said}"
            );
        }
        let hanging_up = dir.path().join("hanging-up.sock");
        node(&hanging_up, Answers::HangingUp);
        let told = learn(&mut starters, &mut learned, &hanging_up, &["h-2"]).await;
        assert_eq!(
            told, None,
            "a node letting the ask go is as no node, told already"
        );

        let back = dir.path().join("back.sock");
        node(&back, Answers::Naming(Arc::default()));
        let told = learn(&mut starters, &mut learned, &back, &["h-2"]).await;
        assert_eq!(told, Some(Told::Again), "a node answering again is told");
        let told = learn(&mut starters, &mut learned, &nobody, &["h-2"]).await;
        assert_eq!(
            told,
            Some(Told::Unasked(Unasked::NoNode)),
            "and a failure after it is told again"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_node_that_takes_the_ask_and_never_answers_is_given_up_on() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("work.sock");
        node(&socket, Answers::Silent);
        let asked_at = Instant::now();
        let answer = asked(&socket, vec!["h-1".to_owned()]).await;
        assert_eq!(answer, Err(Unasked::Unanswered));
        assert!(
            asked_at.elapsed() >= ANSWERED_WITHIN,
            "given up on only once the wait has passed"
        );
    }
}
