//! A quest carried to another machine of the realm, as the person takes
//! their work from one laptop to another.
//!
//! The harness does what only it knows how to: each session of the quest
//! here, told to carry (a steer act naming the machine), puts every tree
//! it holds and its own file into the quest's scratch, under
//! `handover/<this machine>/`, writes `<session>.done.json` beside them
//! saying what it put there, and ends. `pi-carry` does the same for what
//! no session runs for. This module puts those in order and says each
//! step as it goes: it tells the sessions, waits until each has carried,
//! carries the rest, shares the quest with its handover, starts each
//! carried session again on the other machine, held, once `pi-carry
//! arrive` has its trees there, marking each started so no later carry
//! starts it again, waits until the sessions here have ended, and then
//! until each runs on the other machine. A step that fails says why
//! and nothing after it runs; what was done stays done, since each
//! step before the answer can be taken again. One that fails at the
//! answer has nothing left to carry, its sessions here ended and each
//! start there marked started, though a session may still come up there.
//!
//! What it does to the world goes through [`Hands`], whose one
//! implementation is the command's, so the order is proved over a fake.
//! Who may order one session's carry other than the person, its
//! orchestrator by the quests' record, is [`ordered`]'s to say. Which of
//! a machine's sessions it carries away to ease itself, while it is over
//! and another has room, is [`ease`]'s to choose. What a carried
//! session's cell brings, its ties, is [`cell`]'s to say.

pub mod cell;
pub mod chain;
pub mod ease;
pub mod easing;
pub mod ground;
pub mod kept;
pub mod ordered;
pub mod secrets;
pub mod settle;
pub mod sleep;
pub mod through;

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use self::ground::Lent;
use crate::placement::{Need, Work};
use crate::record::{Session, State};
use crate::wire::{Start, Started, Taking};

/// The program the harness carries with and arrives with.
pub const PI_CARRY: &str = "pi-carry";

/// A step of a carry, in the order they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    /// The quest's sessions here, and the machine it goes to.
    Find,
    /// Each session's work, and the rest, put in the quest's handover.
    Push,
    /// The quest shared with its handover.
    Share,
    /// Each carried session started on the other machine.
    Start,
    /// The sessions here ended.
    Close,
    /// Each session started on the other machine seen running there.
    Answer,
}

/// How a step stands as a line says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Stood {
    /// Begun.
    Doing,
    /// Done.
    Done,
    /// Nothing to do.
    Skipped,
    /// Failed, and the carry with it.
    Failed,
}

/// One line a carry says: a step, how it stands, and one plain line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Line {
    /// The step.
    pub step: Step,
    /// How it stands.
    pub state: Stood,
    /// What happened, in one plain line.
    pub said: String,
}

/// A session started again on the other machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Arrived {
    /// Its session's id, as its done file names it.
    #[serde(skip)]
    pub session: String,
    /// The address it went by here.
    pub address: String,
    /// Its held id there.
    pub held: String,
    /// The words that join it from a terminal here.
    pub attach: Vec<String>,
}

/// The last line of a carry that went through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Carried {
    /// Always `carried`.
    pub step: &'static str,
    /// Always `done`.
    pub state: Stood,
    /// The machine it went to.
    pub machine: String,
    /// Each session started there.
    pub sessions: Vec<Arrived>,
    /// Each session of the quest here passed over, by address, each
    /// staying as it was: the others, where a session carries itself, or
    /// the parked ones an ordered carry passed over, since no quest's
    /// record holds them, so who orchestrates them can't be told.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

/// Who asks for a carry, which each session told hears it is from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked<'a> {
    /// The person, at their own login.
    Person,
    /// The quest's orchestrator, for the reason it gives.
    Ordered(&'a str),
    /// The session going by the id `session`, for the reason `why`: only
    /// that session goes, the quest's others staying where they run.
    Itself {
        /// The session's id, as its own pi names it.
        session: &'a str,
        /// Why it carries itself.
        why: &'a str,
    },
}

impl Asked<'_> {
    /// The reason it was asked for, where one is given.
    fn why(&self) -> Option<&str> {
        match self {
            Self::Person => None,
            Self::Ordered(why) | Self::Itself { why, .. } => Some(why),
        }
    }
}

/// A carry that went no further than `step`, which said why as it failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Failed {
    /// The step that failed.
    pub step: Step,
}

/// What a session, or `pi-carry`, says it carried: the session. The
/// harness says more, such as its folder and trees, which `pi-carry` on
/// the other machine reads to say where it arrives.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Done {
    /// The session's id.
    pub session: String,
    /// What stands for this session's last start on another machine from
    /// here, as the mark that said so, where it ever started: a carry
    /// taken again after its start went unanswered asks under the name
    /// it asked before, and joins that start, where one carried since
    /// asks afresh.
    #[serde(skip)]
    pub started_before: Option<String>,
}

/// A session's refusal of its carry, as it writes it beside where its
/// done file would go: holding a lease, mid-push or mid-rebase, or with
/// background work running.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Refused {
    /// The session's id.
    pub session: String,
    /// Why, in one plain line.
    pub why: String,
}

/// Who ordered a carry and why, which each session carried arrives as,
/// so none reads as the person's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ordered {
    /// The orchestrator's address, as the node proved it.
    pub by: String,
    /// Why it was ordered, as the orchestrator said.
    pub why: String,
}

/// What the node answered of a session it told to carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Told {
    /// Who the node proved ordered it, for an `ordered-carry`.
    pub by: Option<String>,
    /// Whether the node carries the rest on itself once the session has
    /// carried and ended, so the command that asked has nothing left to
    /// wait for.
    pub carries_on: bool,
    /// Whether it carries on with it every session its steer named
    /// `with`, so the command has none of them left to carry either.
    pub carries_with: bool,
}

/// Why a carry's starts weren't found taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Untaken {
    /// This machine's node would refuse them, as it says.
    Refused(String),
    /// This machine's node couldn't say, as said: one too old to know
    /// the ask, or one that didn't answer.
    Unasked(String),
}

/// What a carry does to the world.
pub trait Hands {
    /// The other machines of the realm this one reaches, by name.
    ///
    /// # Errors
    ///
    /// When the node can't say.
    fn machines(&mut self) -> Result<Vec<String>, String>;
    /// What the machine `to` is: one of the person's own, or a worker
    /// that takes carries or takes none.
    ///
    /// # Errors
    ///
    /// When the node can't say.
    fn lent(&mut self, to: &str) -> Result<Lent, String>;
    /// Every session the node knows of.
    ///
    /// # Errors
    ///
    /// When the node can't say.
    fn sessions(&mut self) -> Result<Vec<Session>, String>;
    /// This boot, which a parked session's owner names the boot it began
    /// in, where the machine says it.
    fn boot(&self) -> Option<String>;
    /// Puts `steer` in the inbox of the session going by `address`, and
    /// answers who the node proved ordered it, for an `ordered-carry`,
    /// and whether it carries the rest on itself.
    ///
    /// # Errors
    ///
    /// When the node refuses it.
    fn steer(&mut self, address: &str, steer: Value) -> Result<Told, String>;
    /// The sessions that refused this carry since it began.
    ///
    /// # Errors
    ///
    /// When a refusal can't be read.
    fn refused(&mut self) -> Result<Vec<Refused>, String>;
    /// What has been carried into the quest's handover from here and not
    /// yet started on another machine.
    ///
    /// # Errors
    ///
    /// When a done file can't be read.
    fn done(&mut self) -> Result<Vec<Done>, String>;
    /// Marks `one` started on another machine, so [`Hands::done`] answers
    /// it no more until it is carried again.
    ///
    /// # Errors
    ///
    /// When the mark can't be made.
    fn started(&mut self, one: &Done) -> Result<(), String>;
    /// Every tie the node holds, each record whole as the tie kind
    /// lists it.
    ///
    /// # Errors
    ///
    /// When the node can't say.
    fn ties(&mut self) -> Result<Vec<Value>, String>;
    /// Every cell the node holds, each record whole as the cell kind
    /// lists it, so a session that arrived here under another cell's name
    /// is carried on under that name.
    ///
    /// # Errors
    ///
    /// When the node can't say.
    fn cells(&mut self) -> Result<Vec<Value>, String>;
    /// Every `formed` record the node holds: each tree and files network
    /// formed here, with when it formed.
    ///
    /// # Errors
    ///
    /// When the node can't say.
    fn formed(&mut self) -> Result<Vec<Value>, String>;
    /// Every quest's record the node holds, each whole as the quest kind
    /// lists it, so an ordered carry passes over a parked session none
    /// holds rather than ask the node to tell it, which it would refuse.
    ///
    /// # Errors
    ///
    /// When the node can't say.
    fn quests(&mut self) -> Result<Vec<Value>, String>;
    /// Puts what a carried session's cell brings beside its done file,
    /// whole, for the node it arrives on to keep.
    ///
    /// # Errors
    ///
    /// When it can't be put there.
    fn bring(&mut self, brought: &cell::Brought) -> Result<(), String>;

    /// Takes away each cell in the handover but those of `keeping`, by
    /// session id, whose done file is gone or started: an earlier
    /// carry's. One beside a done file not yet started, as a failed carry
    /// leaves it, stays. Answers the sessions whose cells went.
    ///
    /// # Errors
    ///
    /// When the handover can't be read, or a cell can't be taken away.
    fn forget(&mut self, keeping: &BTreeSet<String>) -> Result<Vec<String>, String>;
    /// Carries what no session carried, with `pi-carry`.
    ///
    /// # Errors
    ///
    /// When it fails, with what it said.
    fn carry_rest(&mut self, quest: &str, to: &str) -> Result<(), String>;
    /// Whether the quest may be shared from here once its sessions have
    /// carried, asked before any is told, since a share refused after
    /// they have ended leaves nothing running here or there.
    ///
    /// # Errors
    ///
    /// When it may not, with why.
    fn shareable(&mut self, quest: &str) -> Result<(), String>;
    /// Shares the quest, with its handover, for the machine `to`: given
    /// to it alone where it is a worker that takes carries, once no file
    /// in the handover names a secret.
    ///
    /// # Errors
    ///
    /// When it can't be shared, or the handover holds a secret.
    fn share(&mut self, quest: &str, to: &str) -> Result<(), String>;
    /// Starts a program held where `start` says.
    ///
    /// # Errors
    ///
    /// When the node can't start it.
    fn start(&mut self, start: Start) -> Result<Started, String>;
    /// Whether `start` would be placed as [`Hands::start`] places it,
    /// starting nothing, asked before any session is told, since a start
    /// refused after they have ended leaves nothing running here or
    /// there.
    ///
    /// # Errors
    ///
    /// When it wouldn't be, with the refusal its start would answer, or
    /// when this machine's node couldn't say.
    fn takes(&mut self, taking: &Taking) -> Result<(), Untaken>;
    /// Waits a moment before what is waited for is looked at again, and
    /// says whether there is patience left for another look.
    fn wait(&mut self) -> bool;
    /// Gives what is waited for next a patience of its own, whatever the
    /// waits before it spent.
    fn wait_afresh(&mut self);
}

/// Carries `quest` from `here` to the machine `to`, saying each step to
/// `say` as it goes, and its last line once it has gone through, or
/// `Err` once a step has failed and said why. Ordered, an orchestrator
/// orders it: each session is told with `ordered-carry`, which the node
/// takes only from the session's orchestrator, and each arrives as
/// carried by it for its reason. Asked by a session itself, only that
/// session goes, told with `self-carry`, which the node takes only from
/// that session, and it arrives as carried by itself; the quest's others
/// are passed over and named, and what none of them held stays with them.
///
/// # Errors
///
/// [`Failed`] when a step fails, its line having said why.
pub fn carry(
    hands: &mut impl Hands,
    quest: &str,
    here: &str,
    to: &str,
    asked: Asked,
    say: &mut impl FnMut(&Value),
) -> Result<Carried, Failed> {
    let mut said = Saying { say };
    said.line(
        Step::Find,
        Stood::Doing,
        format!("finding {quest}'s sessions here"),
    );
    let Found { sessions, skipped } =
        said.or_failed(Step::Find, find(hands, quest, here, to, asked))?;
    let names: Vec<&str> = sessions.iter().map(|one| one.address.as_str()).collect();
    let mut found = if names.is_empty() {
        format!("no session runs {quest} here; what it left goes as it is")
    } else {
        format!("{} for {quest}, going to {to}", names.join(", "))
    };
    match (asked, skipped.as_slice()) {
        (_, []) => {}
        (Asked::Itself { .. }, [one]) => {
            found = format!("{found}; {one} stays here, as a session carries only itself");
        }
        (Asked::Itself { .. }, many) => {
            found = format!(
                "{found}; {} stay here, as a session carries only itself",
                many.join(", ")
            );
        }
        (Asked::Person | Asked::Ordered(_), _) => {
            found = format!("{found}; {}", passed_over(quest, &skipped));
        }
    }
    said.line(Step::Find, Stood::Done, found);

    said.line(
        Step::Push,
        Stood::Doing,
        "carrying each session's work".to_owned(),
    );
    said.or_failed(Step::Push, bring_cells(hands, &sessions))?;
    let (ordered, carried_on) = said.or_failed(Step::Push, tell(hands, to, asked, &sessions))?;
    if matches!(asked, Asked::Ordered(_)) && !carried_on.is_empty() {
        return handed_on(
            hands,
            (quest, here, to),
            (&sessions, &carried_on),
            ordered.as_ref(),
            (&mut said, skipped),
        );
    }
    if let Asked::Itself { .. } = asked {
        // Its own session can't end while this runs below it, so the
        // node carries on from here, once it has ended.
        said.line(Step::Push, Stood::Done, handed_said(&names, to));
        return Ok(Carried {
            step: "handed",
            state: Stood::Done,
            machine: to.to_owned(),
            sessions: Vec::new(),
            skipped,
        });
    }
    let whole = skipped.is_empty();
    let arrived = rest(
        hands,
        (quest, here, to),
        (&sessions, whole, false),
        ordered.as_ref(),
        &mut said,
    )?;
    Ok(Carried {
        step: "carried",
        state: Stood::Done,
        machine: to.to_owned(),
        sessions: arrived,
        skipped,
    })
}

/// The rest of an ordered carry once the node said it carries on those
/// of `sessions` going by an address in `carried_on`. Where it carries
/// on every one, it finishes the carry whatever becomes of this command,
/// so this waits for none of it, and two never carry one session on at
/// once. The node carries on only the sessions of a quest standing here,
/// the one alone or every one told, so it carries on some but not all
/// only where another came to stand since they were found, or it is too
/// old to carry several: this carries those others on, leaving the
/// quest's own trees to the node.
fn handed_on<F: FnMut(&Value)>(
    hands: &mut impl Hands,
    (quest, here, to): (&str, &str, &str),
    (sessions, carried_on): (&[Session], &BTreeSet<String>),
    ordered: Option<&Ordered>,
    (said, skipped): (&mut Saying<'_, F>, Vec<String>),
) -> Result<Carried, Failed> {
    let (theirs, ours): (Vec<Session>, Vec<Session>) = sessions
        .iter()
        .cloned()
        .partition(|one| carried_on.contains(&one.address));
    let told: Vec<&str> = theirs.iter().map(|one| one.address.as_str()).collect();
    if ours.is_empty() {
        said.line(Step::Push, Stood::Done, handed_said(&told, to));
        return Ok(Carried {
            step: "handed",
            state: Stood::Done,
            machine: to.to_owned(),
            sessions: Vec::new(),
            skipped,
        });
    }
    said.line(
        Step::Push,
        Stood::Doing,
        format!(
            "{}: this machine's node carries the rest on",
            told.join(", ")
        ),
    );
    let arrived = rest(
        hands,
        (quest, here, to),
        (&ours, false, true),
        ordered,
        said,
    )?;
    Ok(Carried {
        step: "carried",
        state: Stood::Done,
        machine: to.to_owned(),
        sessions: arrived,
        skipped,
    })
}

/// What a carry says where it hands the rest of a carry to this machine's
/// node, those `told` being the sessions it carries on to `to`: that this
/// command is done but the carry isn't, and how to see it land.
fn handed_said(told: &[&str], to: &str) -> String {
    format!(
        "{} told; the carry goes on in this machine's node, not here: once each has \
         carried and ended, the node carries the rest and starts it on {to}. It has \
         landed when `mycelium sessions` shows it on {to}; until then the node's log \
         says each step, on lines starting with the session's address and `carried by`",
        told.join(", ")
    )
}

/// Carries on a carry a session asked of itself, once it was told, as
/// this machine's node does, since the session can't end while the
/// command that asked runs below it: waits for `session` to carry, then
/// carries the rest where it is `whole`, the quest's only session here,
/// shares, starts it on `to` as carried by `ordered`, and waits for it to
/// end here. Only its own work starts there, and the quest's where it
/// was whole, never what an earlier carry left. An orchestrator's carry of
/// several of a quest's sessions, told in a row, is carried on as one,
/// `sessions` holding each.
///
/// # Errors
///
/// [`Failed`] when a step fails, its line having said why.
pub fn carry_told(
    hands: &mut impl Hands,
    (quest, here, to): (&str, &str, &str),
    (sessions, whole): (&[Session], bool),
    ordered: &Ordered,
    say: &mut impl FnMut(&Value),
) -> Result<Carried, Failed> {
    let mut said = Saying { say };
    let arrived = rest(
        hands,
        (quest, here, to),
        (sessions, whole, true),
        Some(ordered),
        &mut said,
    )?;
    Ok(Carried {
        step: "carried",
        state: Stood::Done,
        machine: to.to_owned(),
        sessions: arrived,
        skipped: Vec::new(),
    })
}

/// A carry's steps once its `sessions` were told: waits until each has
/// carried, carries the rest where `whole`, shares, starts each on `to`
/// and waits until they have ended here. Where `own`, only the told
/// sessions' work starts, and the quest's where `whole`.
fn rest<F: FnMut(&Value)>(
    hands: &mut impl Hands,
    (quest, here, to): (&str, &str, &str),
    (sessions, whole, own): (&[Session], bool, bool),
    ordered: Option<&Ordered>,
    said: &mut Saying<'_, F>,
) -> Result<Vec<Arrived>, Failed> {
    let gathering = gathered(
        hands,
        (quest, here, to),
        (sessions, whole),
        &mut |waiting| said.line(Step::Push, Stood::Doing, waiting),
    );
    let mut done = said.or_failed(Step::Push, gathering)?;
    if own {
        let told: BTreeSet<&str> = sessions.iter().filter_map(session_id).collect();
        done.retain(|one| told.contains(one.session.as_str()) || (whole && one.session == quest));
        if done.is_empty() {
            said.or_failed(
                Step::Push,
                Err::<(), _>(format!("nothing of {quest} was carried into the handover")),
            )?;
        }
    }
    let count = plural(done.len(), "session");
    said.line(
        Step::Push,
        Stood::Done,
        format!("{count} carried into the handover"),
    );

    said.line(
        Step::Share,
        Stood::Doing,
        format!("sharing {quest} with its handover"),
    );
    said.or_failed(Step::Share, hands.share(quest, to))?;
    said.line(
        Step::Share,
        Stood::Done,
        format!("{quest} is shared, its handover with it"),
    );

    said.line(
        Step::Start,
        Stood::Doing,
        format!("starting each session on {to}"),
    );
    let arrived = said.or_failed(
        Step::Start,
        start_each(hands, quest, here, to, &done, sessions, ordered),
    )?;
    let count = plural(arrived.len(), "session");
    said.line(
        Step::Start,
        Stood::Done,
        format!("{count} started on {to}, held"),
    );

    said.line(
        Step::Close,
        Stood::Doing,
        "waiting for the sessions here to end".to_owned(),
    );
    said.or_failed(Step::Close, closed(hands, here, sessions))?;
    if sessions.is_empty() {
        said.line(
            Step::Close,
            Stood::Skipped,
            "no session ran here".to_owned(),
        );
    } else {
        said.line(
            Step::Close,
            Stood::Done,
            "the sessions here have ended".to_owned(),
        );
    }

    said.line(
        Step::Answer,
        Stood::Doing,
        format!("waiting for each session started on {to} to run there"),
    );
    // A close that waited out a long turn leaves the starts there no
    // less to do, so they get a wait of their own.
    hands.wait_afresh();
    said.or_failed(Step::Answer, answered(hands, to, &arrived))?;
    if arrived.is_empty() {
        said.line(
            Step::Answer,
            Stood::Skipped,
            "no session started there".to_owned(),
        );
    } else {
        said.line(
            Step::Answer,
            Stood::Done,
            format!("{} running on {to}", plural(arrived.len(), "session")),
        );
    }
    Ok(arrived)
}

/// Waits until each of `arrived` runs on `to`, as the node there says
/// it: its own session, busy or idle there. A start that landed only
/// holds the arrival, which may still fail before pi takes the session
/// up, so until then the session runs nowhere. Those that never run
/// are named, each with how to join its start and see why.
fn answered(hands: &mut impl Hands, to: &str, arrived: &[Arrived]) -> Result<(), String> {
    loop {
        let there = hands.sessions()?;
        let silent: Vec<&Arrived> = arrived
            .iter()
            .filter(|one| {
                !there.iter().any(|session| {
                    session.machine == to
                        && session_id(session) == Some(one.session.as_str())
                        && runs(session)
                })
            })
            .collect();
        if silent.is_empty() {
            return Ok(());
        }
        if !hands.wait() {
            let each: Vec<String> = silent
                .iter()
                .map(|one| {
                    format!(
                        "{} started on {to} but isn't running there; join its start with `{}` \
                         to see why",
                        one.address,
                        one.attach.join(" ")
                    )
                })
                .collect();
            return Err(each.join("; "));
        }
    }
}

/// What a carry's sessions were, once found: those it carries, and the
/// addresses of the parked ones it passed over.
struct Found {
    sessions: Vec<Session>,
    skipped: Vec<String>,
}

/// The parked sessions `skipped` passed over, and why, as said.
fn passed_over(quest: &str, skipped: &[String]) -> String {
    format!(
        "{} parked where {quest}'s record alone doesn't hold {}, so who orchestrates {} can't \
         be told, and {} here",
        skipped.join(", "),
        if skipped.len() == 1 { "it" } else { "them" },
        if skipped.len() == 1 { "it" } else { "them" },
        if skipped.len() == 1 {
            "it stays"
        } else {
            "they stay"
        },
    )
}

/// Where a carry says its lines.
struct Saying<'a, F> {
    say: &'a mut F,
}

impl<F: FnMut(&Value)> Saying<'_, F> {
    /// Says how `step` stands.
    fn line(&mut self, step: Step, state: Stood, said: String) {
        (self.say)(&json!(Line { step, state, said }));
    }

    /// What `done` answered, or, once it has failed, its step said failed
    /// with why, so the carry goes no further.
    fn or_failed<T>(&mut self, step: Step, done: Result<T, String>) -> Result<T, Failed> {
        done.map_err(|why| {
            self.line(step, Stood::Failed, why);
            Failed { step }
        })
    }
}

/// Starts each session `done` names again on `to`, by the address it went
/// by here, or its id where no session here named it, marking each started
/// as it goes, so neither a later carry nor this one taken again after a
/// failure starts it twice. The quest's own
/// done file, holding the trees no session held, arrives there too, but
/// no session starts for it, so it isn't among those answered. Each
/// arrives as `ordered` says, where an orchestrator ordered it. The
/// sessions told to carry start first, then the quest's own, as the
/// find asked of them, and only then those it couldn't ask of: a
/// session that had ended, or one an earlier carry left. One refused or
/// unanswered doesn't stop the rest, since each told has ended here;
/// those it names fail the step once every one was asked.
fn start_each(
    hands: &mut impl Hands,
    quest: &str,
    here: &str,
    to: &str,
    done: &[Done],
    sessions: &[Session],
    ordered: Option<&Ordered>,
) -> Result<Vec<Arrived>, String> {
    let told: BTreeSet<&str> = sessions.iter().filter_map(session_id).collect();
    let mut done: Vec<&Done> = done.iter().collect();
    done.sort_by_key(|one| {
        if told.contains(one.session.as_str()) {
            0
        } else if one.session == quest {
            1
        } else {
            2
        }
    });
    // Each is asked whatever the ones before it answered, since a session
    // told has ended here, and one whose start went unanswered may still
    // land there: asked again, under its name, it joins that start.
    let mut arrived = Vec::new();
    let mut failed = Vec::new();
    // Started there, but left marked here as not yet started.
    let mut unmarked = Vec::new();
    for one in done {
        let address = sessions
            .iter()
            .find(|session| session_id(session) == Some(one.session.as_str()))
            .map_or_else(|| one.session.clone(), |session| session.address.clone());
        let started = match hands.start(arriving(quest, here, to, one, ordered)) {
            Ok(started) => started,
            Err(why) => {
                failed.push(format!("{address} didn't start on {to}: {why}"));
                continue;
            }
        };
        // Started, it arrived whatever its mark says, and only a later
        // carry, which would start it again, needs telling.
        if let Err(why) = hands.started(one) {
            unmarked.push(format!(
                "{address} started on {to}, but its done file here couldn't be marked \
                 started, so move it out of the handover before carrying again: {why}"
            ));
        }
        if one.session != quest {
            arrived.push(Arrived {
                session: one.session.clone(),
                address,
                held: started.held,
                attach: started.attach,
            });
        }
    }
    if failed.is_empty() && unmarked.is_empty() {
        return Ok(arrived);
    }
    let mut why = failed
        .iter()
        .chain(&unmarked)
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    if !arrived.is_empty() {
        let went: Vec<&str> = arrived.iter().map(|one| one.address.as_str()).collect();
        // Writing to a string can't fail.
        let _ = write!(why, "; {} started there", went.join(", "));
    }
    if !failed.is_empty() {
        why.push_str("; what didn't start stays in the handover, for the person's carry to start");
    }
    Err(why)
}

/// The quest's sessions on `here` that can carry their own work: those
/// running, and those parked this boot, which wake for it; or why the
/// carry can't go to `to`. A carry ordered for a reason needs one at
/// least, since only the node's taking a session's steer proves who may
/// order it, and a reason that says something. An orchestrator's carry
/// passes over a parked session no quest's record holds, naming it,
/// since the node would refuse to tell it and so stop the carry over a
/// session asleep: one running stays, so its refusal is said. A
/// session's carry of itself goes with that session alone, which must
/// run the quest here, and answers the others' addresses as passed over.
fn find(
    hands: &mut impl Hands,
    quest: &str,
    here: &str,
    to: &str,
    asked: Asked,
) -> Result<Found, String> {
    let why = asked.why();
    let ordered = why.is_some();
    if why.is_some_and(|why| why.trim().is_empty()) {
        return Err("an ordered carry says why, and a blank reason says nothing".to_owned());
    }
    if to == here {
        return Err(format!("{quest} is on {here} already"));
    }
    let machines = hands.machines()?;
    if !machines.iter().any(|machine| machine == to) {
        return Err(format!(
            "{to} isn't a machine of the realm in reach; those are {}",
            if machines.is_empty() {
                "none".to_owned()
            } else {
                machines.join(", ")
            }
        ));
    }
    if hands.lent(to)? == Lent::TakesNone {
        return Err(ground::takes_none(to));
    }
    hands.shareable(quest)?;
    let boot = hands.boot();
    let sessions: Vec<Session> = hands
        .sessions()?
        .into_iter()
        .filter(|session| session.machine == here && session.quest.as_deref() == Some(quest))
        .filter(|session| runs(session) || parked(session, boot.as_deref()))
        .collect();
    let (sessions, skipped): (Vec<Session>, Vec<Session>) = match asked {
        Asked::Itself { session, .. } => {
            let (own, others): (Vec<Session>, Vec<Session>) = sessions
                .into_iter()
                .partition(|one| session_id(one) == Some(session));
            if own.is_empty() {
                return Err(format!(
                    "session {session} doesn't run {quest} here, so it can't carry itself"
                ));
            }
            (own, others)
        }
        Asked::Ordered(_) if !sessions.is_empty() => {
            let quests = hands.quests().map_err(|why| {
                format!(
                    "which quest's record holds each of {quest}'s sessions can't be told: {why}"
                )
            })?;
            sessions.into_iter().partition(|session| {
                runs(session) || ordered::held_only_by(easing::id(session), quest, &quests)
            })
        }
        Asked::Person | Asked::Ordered(_) => (sessions, Vec::new()),
    };
    let skipped: Vec<String> = skipped.into_iter().map(|one| one.address).collect();
    if ordered && sessions.is_empty() {
        let mut why = format!(
            "no session runs {quest} here, so the node can't prove who orchestrates it: \
             only the person carries what it left"
        );
        if !skipped.is_empty() {
            why = format!("{why} ({})", passed_over(quest, &skipped));
        }
        return Err(why);
    }
    // Each session's arrival and the quest's own trees', as the carry
    // would start them, the quest's own only where every session goes;
    // an ordered carry's order is proved only as it is taken, so the node
    // is told it is ordered.
    let starts = sessions
        .iter()
        .filter_map(session_id)
        .chain(skipped.is_empty().then_some(quest))
        .map(|session| Done {
            session: session.to_owned(),
            started_before: None,
        })
        .map(|one| arriving(quest, here, to, &one, None))
        .collect();
    hands
        .takes(&Taking { starts, ordered })
        .map_err(|untaken| match untaken {
            Untaken::Refused(why) => format!("{quest}'s sessions can't start on {to} now: {why}"),
            Untaken::Unasked(why) => format!(
                "{quest} isn't carried, since this machine's node couldn't say whether {to} \
                 would start its sessions: {why}"
            ),
        })?;
    Ok(Found { sessions, skipped })
}

/// Tells each session to carry its work to `to`, as the person, its
/// orchestrator or itself, as `asked` says, and answers who ordered it,
/// as the node proved, and the sessions the node carries on. An ordered
/// carry of several names the others `with` the last it tells, so the
/// node may carry them all on at once, as it does a quest's only one:
/// this command may then end before they have, as a timeout ends it,
/// without leaving the quest carried into the handover and started
/// nowhere. A node too old to read `with` carries none of them on.
fn tell(
    hands: &mut impl Hands,
    to: &str,
    asked: Asked,
    sessions: &[Session],
) -> Result<(Option<Ordered>, BTreeSet<String>), String> {
    let steer = match asked {
        Asked::Person => json!({"act": "carry", "to": to, "from": "person"}),
        Asked::Ordered(why) => json!({"act": ordered::ACT, "to": to, "why": why}),
        Asked::Itself { why, .. } => json!({"act": ordered::ITSELF, "to": to, "why": why}),
    };
    let why = asked.why();
    let together = matches!(asked, Asked::Ordered(_)) && sessions.len() > 1;
    let mut by: Option<String> = None;
    let mut carried_on = BTreeSet::new();
    for (told_before, session) in sessions.iter().enumerate() {
        let mut steer = steer.clone();
        if together && told_before == sessions.len() - 1 {
            let with: Vec<&str> = sessions[..told_before]
                .iter()
                .map(|one| one.address.as_str())
                .collect();
            steer["with"] = json!(with);
        }
        let told = hands
            .steer(&session.address, steer)
            .map_err(|why| format!("{} wasn't told: {why}", session.address))?;
        if told.carries_on {
            carried_on.insert(session.address.clone());
        }
        if told.carries_on && told.carries_with {
            carried_on.extend(sessions.iter().map(|one| one.address.clone()));
        }
        if why.is_none() {
            continue;
        }
        let proved = told.by.ok_or_else(|| {
            format!(
                "the node didn't say who ordered {}'s carry, so it can't arrive as theirs",
                session.address
            )
        })?;
        if let Some(first) = by.as_ref().filter(|first| **first != proved) {
            return Err(format!(
                "the node proved {first} ordered one session's carry and {proved} {}'s, so \
                 none arrives as either's",
                session.address
            ));
        }
        by = Some(proved);
    }
    let ordered = by.zip(why).map(|(by, why)| Ordered {
        by,
        why: why.to_owned(),
    });
    Ok((ordered, carried_on))
}

/// Waits until each of `sessions`, told, has carried its work, then,
/// where `whole`, carries what none of them held, and answers everything
/// carried. A session that refuses stops it at once.
fn gathered(
    hands: &mut impl Hands,
    (quest, here, to): (&str, &str, &str),
    (sessions, whole): (&[Session], bool),
    say_waiting: &mut impl FnMut(String),
) -> Result<Vec<Done>, String> {
    let waiting: BTreeSet<&str> = sessions.iter().filter_map(session_id).collect();
    let mut said_left: Option<Vec<&str>> = None;
    loop {
        let refused = hands.refused()?;
        let refusal = sessions.iter().find_map(|session| {
            let id = session_id(session)?;
            let refusal = refused.iter().find(|one| one.session == id)?;
            Some((session, refusal))
        });
        if let Some((session, refusal)) = refusal {
            let failed = format!("{} refused its carry: {}", session.address, refusal.why);
            // Only the refusers' cells go: another told may still be
            // carrying, its done file not written yet.
            let keeping: BTreeSet<String> = sessions
                .iter()
                .filter_map(session_id)
                .filter(|id| !refused.iter().any(|one| one.session == *id))
                .map(str::to_owned)
                .collect();
            return Err(match hands.forget(&keeping) {
                Ok(_) => failed,
                Err(why) => format!("{failed}; a refuser's cell wasn't taken away: {why}"),
            });
        }
        let done = hands.done()?;
        let carried: BTreeSet<&str> = done.iter().map(|one| one.session.as_str()).collect();
        let left: Vec<&str> = waiting
            .iter()
            .copied()
            .filter(|id| !carried.contains(id))
            .collect();
        if left.is_empty() {
            break;
        }
        // Said once, and again as each arrives, so whoever watches a
        // carry sees what it waits on and where to look for it.
        if said_left.as_ref() != Some(&left) {
            say_waiting(waiting_on(sessions, &left, (quest, here)));
            said_left = Some(left.clone());
        }
        if !hands.wait() {
            let names: Vec<&str> = sessions
                .iter()
                .filter(|one| session_id(one).is_some_and(|id| left.contains(&id)))
                .map(|one| one.address.as_str())
                .collect();
            return Err(format!(
                "{} didn't carry its work in time",
                names.join(", ")
            ));
        }
    }
    // What none of them held stays with the sessions that stay.
    if whole {
        hands.carry_rest(quest, to)?;
    }
    let done = hands.done()?;
    if done.is_empty() {
        return Err(format!(
            "nothing of {quest} was here to carry that hadn't started already"
        ));
    }
    Ok(done)
}

/// What a carry says it waits on: each of `sessions` whose id is `left`,
/// by its address and id, and the done file it writes in `quest`'s
/// handover from `here` once it has carried.
fn waiting_on(sessions: &[Session], left: &[&str], (quest, here): (&str, &str)) -> String {
    let each: Vec<String> = sessions
        .iter()
        .filter_map(|one| Some((one.address.as_str(), session_id(one)?)))
        .filter(|(_, id)| left.contains(id))
        .map(|(address, id)| format!("{address} (handover/{here}/{id}{})", through::DONE))
        .collect();
    format!(
        "waiting for {} to carry into {quest}'s handover",
        each.join(", ")
    )
}

/// Puts in the handover what each session to be carried brings of its
/// cell, where its done file will be, before any session is told to
/// carry, having taken away every cell an earlier carry left beside a
/// done file gone or started: so a cell beside a done file not started is
/// the newest carry's for that session, and what fails here stops the carry while every
/// session still runs here. A carry taken again brings them afresh.
/// Answers the ids of the sessions whose cells it brought.
fn bring_cells(hands: &mut impl Hands, sessions: &[Session]) -> Result<BTreeSet<String>, String> {
    let carried: Vec<(&str, &Session)> = sessions
        .iter()
        .filter_map(|session| Some((session_id(session)?, session)))
        .collect();
    let keeping: BTreeSet<String> = carried.iter().map(|(id, _)| (*id).to_owned()).collect();
    hands
        .forget(&keeping)
        .map_err(|why| format!("an earlier carry's cells weren't taken away: {why}"))?;
    if carried.is_empty() {
        return Ok(BTreeSet::new());
    }
    let ties = hands
        .ties()
        .map_err(|why| format!("the node didn't say its ties: {why}"))?;
    let cells = hands
        .cells()
        .map_err(|why| format!("the node didn't say its cells: {why}"))?;
    let in_a_network = carried
        .iter()
        .any(|(_, session)| !crate::networks::formed_networks(&session.presence).is_empty());
    let formed = if in_a_network {
        hands
            .formed()
            .map_err(|why| format!("the node didn't say which networks formed: {why}"))?
    } else {
        Vec::new()
    };
    for (id, session) in &carried {
        let name = cell_named(&cells, &session.id).unwrap_or(&session.address);
        let brought = cell::Brought {
            formed: cell::formed_of(&session.presence, &formed),
            ..cell::brought(id, name, &ties)
        };
        hands.bring(&brought).map_err(|why| {
            format!(
                "{}'s cell wasn't put beside its work: {why}",
                session.address
            )
        })?;
    }
    Ok(keeping)
}

/// The name of the cell `cells` hold for the session `id`: the cell it
/// arrived as, where it arrived here, else its own address.
fn cell_named<'cells>(cells: &'cells [Value], id: &str) -> Option<&'cells str> {
    cells
        .iter()
        .find(|cell| cell["id"].as_str() == Some(id))
        .and_then(|cell| cell["name"].as_str())
        .filter(|name| !name.is_empty())
}

/// Waits until none of `sessions` runs on `here` any more.
fn closed(hands: &mut impl Hands, here: &str, sessions: &[Session]) -> Result<(), String> {
    let carried: BTreeSet<&str> = sessions.iter().map(|one| one.id.as_str()).collect();
    loop {
        let running: Vec<String> = hands
            .sessions()?
            .into_iter()
            .filter(|one| one.machine == here && carried.contains(one.id.as_str()) && runs(one))
            .map(|one| one.address)
            .collect();
        if running.is_empty() {
            return Ok(());
        }
        if !hands.wait() {
            return Err(format!(
                "{} carried but still runs here; close it from its window",
                running.join(", ")
            ));
        }
    }
}

/// The start of `one` again on `to`: in its home there, where `pi-carry
/// arrive` waits for its handover, fetches its trees and puts its file
/// in place, and only then pi takes it up in the folder `pi-carry where`
/// says it arrived in. The quest's own done file names no session, so
/// its arrival ends with its trees in place. Its words go to the shell
/// apart from the script, so none is read as the shell's. Pi is told
/// first to carry on, as a reopened session is, since one taken up again
/// with no first prompt sits idle until a message wakes it. One an
/// orchestrator ordered arrives as carried by it, for its reason, so it
/// never reads as the person's own.
pub(crate) fn arriving(
    quest: &str,
    from: &str,
    to: &str,
    one: &Done,
    ordered: Option<&Ordered>,
) -> Start {
    let script = arrival_script(one.session == quest, ordered.is_some());
    let told = carry_on(from);
    let mut command = [
        "sh",
        "-c",
        &script,
        PI_CARRY,
        quest,
        from,
        &one.session,
        &told,
    ]
    .map(str::to_owned)
    .to_vec();
    if let Some(ordered) = ordered {
        command.extend([ordered.by.clone(), ordered.why.clone()]);
    }
    Start {
        work: Work {
            needs: vec![Need::Machine(to.to_owned())],
            prefers: Vec::new(),
            tolerates: Vec::new(),
            may_wait: false,
            lasts: None,
            fence: None,
        },
        folder: None,
        strict: false,
        command,
        may_cost: false,
        carries: false,
        once: Some(arrival_name(quest, to, one)),
    }
}

/// The script a session's start on its new machine runs, as [`arriving`]
/// says: the quest's `own` trees' arriving alone, or a session's taken up
/// after; one an orchestrator `ordered` saying who and why.
fn arrival_script(own: bool, ordered: bool) -> String {
    let arrive = if ordered {
        // Each joined to its value, so a reason opening with a dash
        // isn't read as an option of pi-carry's.
        r#"pi-carry arrive "$1" --from "$2" --session "$3" --by="$5" --why="$6""#
    } else {
        r#"pi-carry arrive "$1" --from "$2" --session "$3""#
    };
    // The harness alone knows which trees came and where a quest's
    // scratch is, so it says the folder: the session's own where a tree
    // it brought holds it, else its quest's scratch, never a folder of
    // the person's that no tree brought. That folder is entered as it
    // stands, never made here. A session taken up again loads its quest
    // only when told, as the shell's starts tell it, so pi is told which.
    // It starts in the fence it ran in, as its done file says it, since
    // the node's own environment names none: a move never loosens a
    // session. Where the node started it in a fence of its own, the
    // tighter of the two holds, so a done file never loosens the fence
    // the node placed it in. One whose fence can't be told, as from a
    // harness that can't say it, doesn't start, rather than start
    // looser; one whose done file names none, from an older harness,
    // starts as the node started it.
    if own {
        arrive.to_owned()
    } else {
        [
            arrive,
            r#" || exit; folder=$(pi-carry where "$1" --from "$2" --session "$3") || exit; "#,
            r#"fence=$(pi-carry fence "$1" --from "$2" --session "$3") || exit; "#,
            r#"case "${PI_SANDBOX-}" in strict) ;; "#,
            r#"fence) if [ "$fence" = strict ]; then export PI_SANDBOX=strict; fi ;; "#,
            r#"*) if [ -n "$fence" ]; then export PI_SANDBOX="$fence"; fi ;; esac; "#,
            r#"cd "$folder" && QUEST_WORKFLOW_AUTOLOAD_ID="$1" exec pi --session "$3" -- "$4""#,
        ]
        .concat()
    }
}

/// The name `one`'s start on `to` is asked under: the same for a carry
/// taken again before it started, so the node answers that start where
/// it landed unanswered, and another once it has started, so a later
/// carry starts it again. Its hash is blake3's, the same from every
/// build, so a carry taken again after an update names the same start.
fn arrival_name(quest: &str, to: &str, one: &Done) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in [quest, to, &one.session] {
        hasher.update(&part.len().to_le_bytes());
        hasher.update(part.as_bytes());
    }
    if let Some(before) = &one.started_before {
        hasher.update(b"started");
        hasher.update(before.as_bytes());
    }
    format!("carry-{}", &hasher.finalize().to_hex()[..16])
}

/// A session's arrival, as a start's command says it: the quest it was
/// carried in, the machine it left, as its done file's folder is named,
/// and its session.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Arrives<'a> {
    /// The quest it was carried in.
    pub(crate) quest: &'a str,
    /// The machine it left.
    pub(crate) from: &'a str,
    /// The session that arrives.
    pub(crate) session: &'a str,
}

/// The arrival `command` starts, when it has the shape [`arriving`]
/// gives a carried session's start, or none: none for a quest's own
/// trees, since no session arrives with them.
pub(crate) fn arrives(command: &[String]) -> Option<Arrives<'_>> {
    let [sh, c, script, name, quest, from, session, ..] = command else {
        return None;
    };
    let shaped = sh == "sh"
        && c == "-c"
        && name == PI_CARRY
        && script.starts_with(r#"pi-carry arrive "$1" --from "$2" --session "$3""#)
        && script.contains("exec pi --session \"$3\"");
    (shaped && session != quest).then_some(Arrives {
        quest,
        from,
        session,
    })
}

/// A session's arrival an orchestrator ordered, as a start's command
/// says it, with who it says ordered it and why.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct OrderedArrival<'a> {
    /// The arrival itself.
    pub(crate) arrives: Arrives<'a>,
    /// The orchestrator it says carried it, by its address.
    pub(crate) by: &'a str,
    /// Why it says it was carried.
    pub(crate) why: &'a str,
}

/// How long after a carry was ordered its arrival may start uncounted:
/// an arrival follows its order within minutes, so a carry line older
/// than this frees nothing more.
pub(crate) const ORDERED_ARRIVES_WITHIN: jiff::SignedDuration = jiff::SignedDuration::from_hours(1);

/// The ordered arrival `command` starts, where it is word for word what
/// [`arriving`] gives a start for a carry an orchestrator ordered from
/// `from`, and nothing more, so no other work hides behind one: a
/// session's, or the quest's own trees', whose session is the quest; or
/// none. Its words prove nothing of who ordered it: its caller holds
/// them to the carry it kept.
pub(crate) fn ordered_arrival(command: &[String]) -> Option<OrderedArrival<'_>> {
    let [sh, c, script, name, quest, from, session, told, by, why] = command else {
        return None;
    };
    let exact = sh == "sh"
        && c == "-c"
        && name == PI_CARRY
        && *script == arrival_script(session == quest, true)
        && *told == carry_on(from);
    exact.then_some(OrderedArrival {
        arrives: Arrives {
            quest,
            from,
            session,
        },
        by,
        why,
    })
}

/// What a session carried from `from` is told as it starts, in the
/// words the harness opens its own turns with, so it reads as one.
fn carry_on(from: &str) -> String {
    format!(
        "[Automated message, no human input: {PI_CARRY} carried this session here from \
         {from}.] Carry on from where you left off: your plan's next open item."
    )
}

/// The arrival `command` starts, where it is word for word what
/// [`arriving`] gives a start for a carry nobody ordered from `from`,
/// and nothing more: a session's, or the quest's own trees', whose
/// session is the quest; or none.
pub(crate) fn unordered_arrival(command: &[String]) -> Option<Arrives<'_>> {
    let [sh, c, script, name, quest, from, session, told] = command else {
        return None;
    };
    let exact = sh == "sh"
        && c == "-c"
        && name == PI_CARRY
        && *script == arrival_script(session == quest, false)
        && *told == carry_on(from);
    exact.then_some(Arrives {
        quest,
        from,
        session,
    })
}

/// Whether a session's process runs.
fn runs(session: &Session) -> bool {
    matches!(session.state, State::Busy | State::Idle)
}

/// Whether a session is parked this boot: its process ended while idle,
/// for its holder to start again, which a holder can only do in the boot
/// it began in. Where the machine names no boot, any park is taken as
/// this boot's.
pub(crate) fn parked(session: &Session, boot: Option<&str>) -> bool {
    let presence = &session.presence;
    session.state == State::Ended
        && presence.get("state").and_then(Value::as_str) == Some("parked")
        && match (
            boot,
            presence.pointer("/owner/bootId").and_then(Value::as_str),
        ) {
            (Some(boot), Some(began)) => boot == began,
            _ => true,
        }
}

/// The one machine `quest`'s sessions run on, or are parked on, among
/// `sessions`, the realm's as a node sees them, which is where a carry of
/// it starts.
///
/// # Errors
///
/// When no session of the quest runs anywhere, or they run on more than
/// one machine, so which to carry it from can't be told.
pub fn runs_on(sessions: &[Session], quest: &str) -> Result<String, String> {
    let mut machines: Vec<&str> = sessions
        .iter()
        .filter(|session| session.quest.as_deref() == Some(quest))
        .filter(|session| runs(session) || parked(session, None))
        .map(|session| session.machine.as_str())
        .collect();
    machines.sort_unstable();
    machines.dedup();
    match machines.as_slice() {
        [one] => Ok((*one).to_owned()),
        [] => Err(format!(
            "no session of {quest} runs on any machine in reach, so there's nothing to carry"
        )),
        several => Err(format!(
            "{quest}'s sessions run on {}, so which machine to carry it from can't be told",
            several.join(" and ")
        )),
    }
}

/// `text` as a finding or a log line shows it: as it was written, but for
/// each control character, escaped, so none moves the terminal it is read
/// in or writes a line of its own.
pub(crate) fn shown(text: &str) -> String {
    let mut shown = String::new();
    for character in text.chars() {
        if character.is_control() {
            shown.extend(character.escape_default());
        } else {
            shown.push(character);
        }
    }
    shown
}

/// The session's own id, as its presence names it and its done file does.
pub(crate) fn session_id(session: &Session) -> Option<&str> {
    session.presence.get("session").and_then(Value::as_str)
}

/// `count` of `thing`, as a person says it.
fn plural(count: usize, thing: &str) -> String {
    if count == 1 {
        format!("1 {thing}")
    } else {
        format!("{count} {thing}s")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use serde_json::{Value, json};

    use super::cell::Brought;
    use super::{
        Asked, Carried, Done, Failed, Hands, Lent, Ordered, PI_CARRY, Refused, Step, Told, Untaken,
        carry, carry_told, ground, runs_on, session_id, unordered_arrival,
    };
    use crate::placement::Need;
    use crate::record::{Session, State};
    use crate::wire::{Start, Started, Taking};

    const QUEST: &str = "QEST-20261107-HARBOR";

    /// A realm as a carry sees it: its machines, its sessions, and what
    /// each step did, kept to be looked at after.
    #[derive(Default)]
    struct Realm {
        machines: Vec<String>,
        sessions: Vec<Session>,
        boot: Option<String>,
        /// The sessions, by id, that carry once told to.
        carries: BTreeSet<String>,
        /// Whether the sessions here end once one has started there.
        ends: bool,
        /// How many more looks it waits for.
        patience: usize,
        /// How many looks it waits for once told to wait afresh.
        afresh: usize,
        /// How many looks at the sessions pass after a start before the
        /// sessions here end, where they do.
        ends_after: usize,
        steered: Vec<(String, Value)>,
        rest: Option<String>,
        /// Whether what no session held is the quest's own trees alone,
        /// with no ended session's work beside them.
        rest_trees_alone: bool,
        /// The done files in the handover, each with whether it was
        /// marked started.
        handover: Vec<(Done, bool)>,
        /// How many times each session, by id, was marked started.
        marks: BTreeMap<String, usize>,
        /// The session, by id, whose start there fails.
        refuses: Option<String>,
        /// The session, by id, whose done file can't be marked started.
        unmarkable: Option<String>,
        /// The session, by id, whose start there lands but whose answer
        /// never comes back, as a loaded node's over a link.
        unanswered: Option<String>,
        /// The sessions, by id, that start there but never run, as one
        /// whose arrival fails after it was held.
        silent: BTreeSet<String>,
        /// The sessions, by id, whose record once started reads on wren
        /// rather than the machine its start needs.
        astray: BTreeSet<String>,
        /// The sessions, by id, whose record once started reads ended
        /// there, as one whose pi exited as it was taken up.
        ended_there: BTreeSet<String>,
        /// The sessions, by id, listed once started under another
        /// session's id there, as a session the node there mistook.
        misnamed: BTreeSet<String>,
        /// How many looks at the sessions pass after a start before the
        /// sessions started run there.
        slow: usize,
        /// How many looks at the sessions there have been since the first
        /// start.
        looks: usize,
        /// Who the node proves ordered an `ordered-carry`.
        by: Option<String>,
        /// Who it proves ordered one session's instead, by its address.
        by_each: BTreeMap<String, String>,
        /// The sessions, by address, whose ordered carry the node says it
        /// carries on itself.
        carrying_on: BTreeSet<String>,
        /// Whether the node carries on, with the last session told, every
        /// session its steer names `with`, as one new enough to does.
        carrying_with: bool,
        /// The sessions, by id, no quest's record holds; the record of
        /// the quest holds every other.
        unheld: BTreeSet<String>,
        /// Other quests' records, beside the quest's own.
        other_records: Vec<Value>,
        /// Why the node can't say its quests' records, where it can't.
        unsaid_quests: Option<String>,
        /// The sessions that refuse their carry once told, and why.
        refusing: Vec<Refused>,
        /// What each session told refused.
        refused: Vec<Refused>,
        shared: Vec<String>,
        /// What each machine is that isn't one of the person's own.
        lent: BTreeMap<String, Lent>,
        /// The workers each share was given to, as it was.
        given: Vec<String>,
        started: Vec<Start>,
        /// Every tie the node holds.
        ties: Vec<Value>,
        /// Why the node can't say its ties, where it can't.
        unsaid_ties: Option<String>,
        /// How many times the ties were asked for.
        ties_asked: usize,
        /// Every cell the node holds, as the cell kind lists it.
        cell_records: Vec<Value>,
        /// Every formed network the node holds.
        formed: Vec<Value>,
        /// Why the node can't say its formed networks, where it can't.
        unsaid_formed: Option<String>,
        /// How many times the formed networks were asked for.
        formed_asked: usize,
        /// Why what a cell brings can't be put beside its work, where not.
        unbrought: Option<String>,
        /// What each cell brought, with how many sessions were told
        /// before.
        brought: Vec<(Brought, usize)>,
        /// The sessions, by id, with a cell in the handover.
        cells: BTreeSet<String>,
        /// Why the quest may not be shared from here, where it may not.
        unshareable: Option<String>,
        /// Why `to` wouldn't take the sessions' starts, where it wouldn't.
        untaken: Option<Untaken>,
        /// Each start asked whether it would be taken.
        asked_takes: Vec<Taking>,
        /// Why an earlier carry's cells can't be taken away, where not.
        unforgotten: Option<String>,
        /// The sessions whose old cells were taken away, with how many
        /// sessions were told before.
        forgotten: Vec<(String, usize)>,
    }

    impl Realm {
        /// Each session started, as the node it started on lists it once
        /// pi runs it there: by the address it went by here, or its id, on
        /// the machine its start needs, idle, but on wren for one `astray`
        /// and ended for one `ended_there`, and under another id for one
        /// `misnamed`. None for the quest's own
        /// trees, which no pi takes up, nor for one `silent`.
        fn arrived_there(&self) -> Vec<Session> {
            self.started
                .iter()
                .filter_map(|start| {
                    let id = start.command.get(6)?;
                    let [Need::Machine(on)] = start.work.needs.as_slice() else {
                        return None;
                    };
                    if id == QUEST || self.silent.contains(id) {
                        return None;
                    }
                    let address = self
                        .sessions
                        .iter()
                        .find(|one| session_id(one) == Some(id.as_str()))
                        .map_or_else(|| id.clone(), |one| one.address.clone());
                    let machine = if self.astray.contains(id) {
                        "wren"
                    } else {
                        on.as_str()
                    };
                    let state = if self.ended_there.contains(id) {
                        "ended"
                    } else {
                        "idle"
                    };
                    let mut there = session(&address, machine, QUEST, state, &json!({}));
                    let listed = if self.misnamed.contains(id) {
                        format!("{id}-other")
                    } else {
                        id.clone()
                    };
                    there.presence = Arc::new(json!({"session": listed}));
                    Some(there)
                })
                .collect()
        }

        /// Writes `session`'s done file, afresh where one was there.
        fn write(&mut self, session: &str) {
            let one = Done {
                session: session.to_owned(),
                started_before: self.marks.get(session).map(usize::to_string),
            };
            match self
                .handover
                .iter_mut()
                .find(|(at, _)| at.session == one.session)
            {
                Some(there) => *there = (one, false),
                None => self.handover.push((one, false)),
            }
        }
    }

    impl Hands for Realm {
        fn machines(&mut self) -> Result<Vec<String>, String> {
            Ok(self.machines.clone())
        }
        fn sessions(&mut self) -> Result<Vec<Session>, String> {
            let mut sessions = self.sessions.clone();
            if !self.started.is_empty() {
                self.looks += 1;
            }
            if self.ends && !self.started.is_empty() && self.looks > self.ends_after {
                for one in &mut sessions {
                    one.state = State::Ended;
                }
            }
            if self.looks > self.slow {
                sessions.extend(self.arrived_there());
            }
            Ok(sessions)
        }
        fn boot(&self) -> Option<String> {
            self.boot.clone()
        }
        fn steer(&mut self, address: &str, steer: Value) -> Result<Told, String> {
            // A session's carry of itself is proved as its own, by the
            // address it is put to, as the node proves the asker.
            let by = if steer["act"] == "self-carry" {
                self.by_each
                    .get(address)
                    .cloned()
                    .or(Some(address.to_owned()))
            } else {
                (steer["act"] == "ordered-carry")
                    .then(|| self.by_each.get(address).or(self.by.as_ref()).cloned())
                    .flatten()
            };
            let steer_is_ordered = steer["act"] == "ordered-carry";
            let carries_with =
                steer_is_ordered && self.carrying_with && steer.get("with").is_some();
            self.steered.push((address.to_owned(), steer));
            let id = self
                .sessions
                .iter()
                .filter(|one| one.address == address)
                .find_map(|one| one.presence.get("session")?.as_str())
                .map(str::to_owned);
            if let Some(id) = id {
                if self.carries.contains(&id) {
                    self.write(&id);
                }
                let refusing = self.refusing.iter().find(|one| one.session == id);
                self.refused.extend(refusing.cloned());
            }
            let carries_on =
                carries_with || (steer_is_ordered && self.carrying_on.contains(address));
            Ok(Told {
                by,
                carries_on,
                carries_with,
            })
        }
        fn refused(&mut self) -> Result<Vec<Refused>, String> {
            Ok(self.refused.clone())
        }
        fn done(&mut self) -> Result<Vec<Done>, String> {
            Ok(self
                .handover
                .iter()
                .filter(|(_, started)| !started)
                .map(|(one, _)| one.clone())
                .collect())
        }
        fn started(&mut self, one: &Done) -> Result<(), String> {
            if self.unmarkable.as_deref() == Some(one.session.as_str()) {
                return Err("the handover is read-only".to_owned());
            }
            let there = self.handover.iter_mut().find(|(at, _)| at == one);
            there
                .map(|there| there.1 = true)
                .ok_or_else(|| format!("{} has no done file to mark", one.session))?;
            *self.marks.entry(one.session.clone()).or_default() += 1;
            Ok(())
        }
        fn ties(&mut self) -> Result<Vec<Value>, String> {
            self.ties_asked += 1;
            self.unsaid_ties
                .clone()
                .map_or_else(|| Ok(self.ties.clone()), Err)
        }
        fn cells(&mut self) -> Result<Vec<Value>, String> {
            Ok(self.cell_records.clone())
        }
        fn quests(&mut self) -> Result<Vec<Value>, String> {
            if let Some(why) = &self.unsaid_quests {
                return Err(why.clone());
            }
            let held: Vec<&str> = self
                .sessions
                .iter()
                .filter_map(session_id)
                .filter(|id| !self.unheld.contains(*id))
                .collect();
            let own = json!({"id": QUEST, "orchestrator": "WCJQSG", "sessions": held});
            Ok(std::iter::once(own)
                .chain(self.other_records.iter().cloned())
                .collect())
        }
        fn formed(&mut self) -> Result<Vec<Value>, String> {
            self.formed_asked += 1;
            self.unsaid_formed
                .clone()
                .map_or_else(|| Ok(self.formed.clone()), Err)
        }
        fn bring(&mut self, brought: &Brought) -> Result<(), String> {
            if let Some(why) = &self.unbrought {
                return Err(why.clone());
            }
            self.brought.push((brought.clone(), self.steered.len()));
            self.cells.insert(brought.session.clone());
            Ok(())
        }
        fn forget(&mut self, keeping: &BTreeSet<String>) -> Result<Vec<String>, String> {
            if let Some(why) = &self.unforgotten {
                return Err(why.clone());
            }
            let unstarted: BTreeSet<&str> = self
                .handover
                .iter()
                .filter(|(_, started)| !started)
                .map(|(one, _)| one.session.as_str())
                .collect();
            let stale: Vec<String> = self
                .cells
                .iter()
                .filter(|one| !keeping.contains(*one) && !unstarted.contains(one.as_str()))
                .cloned()
                .collect();
            for one in &stale {
                self.cells.remove(one);
                self.forgotten.push((one.clone(), self.steered.len()));
            }
            Ok(stale)
        }
        fn carry_rest(&mut self, quest: &str, to: &str) -> Result<(), String> {
            // What no session held goes the first time; nothing is left
            // of it for a later carry.
            if self.rest.is_none() {
                if !self.rest_trees_alone {
                    self.write("s-gone");
                }
                // The trees no session held, as `pi-carry` names them.
                self.write(QUEST);
            }
            self.rest = Some(format!("{quest} {to}"));
            Ok(())
        }
        fn shareable(&mut self, _quest: &str) -> Result<(), String> {
            self.unshareable.clone().map_or(Ok(()), Err)
        }
        fn lent(&mut self, to: &str) -> Result<Lent, String> {
            Ok(self.lent.get(to).copied().unwrap_or(Lent::Not))
        }
        fn share(&mut self, quest: &str, to: &str) -> Result<(), String> {
            self.shared.push(quest.to_owned());
            if self.lent(to)? == Lent::TakesCarries {
                self.given.push(to.to_owned());
            }
            Ok(())
        }
        fn start(&mut self, start: Start) -> Result<Started, String> {
            let session = start.command.get(6).cloned();
            if self.refuses.is_some() && self.refuses == session {
                return Err("the machine there refused it".to_owned());
            }
            // As the node does, an ask under a name it started already
            // is answered with that start, never a second.
            let joined = start.once.as_ref().and_then(|once| {
                self.started
                    .iter()
                    .position(|one| one.once.as_ref() == Some(once))
            });
            let held = format!("h{}", joined.unwrap_or(self.started.len()));
            if joined.is_none() {
                self.started.push(start);
            }
            if self.unanswered.is_some() && self.unanswered == session {
                self.unanswered = None;
                return Err("the node answered nothing in 35 s".to_owned());
            }
            Ok(Started {
                machine: "tern".to_owned(),
                held: held.clone(),
                folder: "/home/joel".into(),
                attach: vec!["mycelium".to_owned(), "attach".to_owned(), held],
            })
        }
        fn takes(&mut self, taking: &Taking) -> Result<(), Untaken> {
            self.asked_takes.push(taking.clone());
            self.untaken.clone().map_or(Ok(()), Err)
        }
        fn wait(&mut self) -> bool {
            let more = self.patience > 0;
            self.patience = self.patience.saturating_sub(1);
            more
        }
        fn wait_afresh(&mut self) {
            self.patience = self.afresh;
        }
    }

    /// A session of `quest` on `machine`, standing as `state` says, its
    /// presence with what `presence` adds.
    fn session(
        address: &str,
        machine: &str,
        quest: &str,
        state: &str,
        presence: &Value,
    ) -> Session {
        let mut whole = json!({"session": format!("s-{}", address.to_lowercase())});
        if let (Some(whole), Some(more)) = (whole.as_object_mut(), presence.as_object()) {
            whole.extend(more.clone());
        }
        serde_json::from_value(json!({
            "id": format!("r-{address}"), "address": address, "machine": machine,
            "groups": [], "state": state, "quest": quest, "at": "2026-11-07T21:00:00Z",
            "presence": whole,
        }))
        .expect("a session")
    }

    /// A realm of skiff and tern, where HARBOR runs busy and is
    /// parked this boot on skiff, was parked a boot before there, runs on
    /// tern already, and another quest runs beside it.
    fn realm() -> Realm {
        let none = json!({});
        let parked = |boot: &str| json!({"state": "parked", "owner": {"bootId": boot}});
        Realm {
            machines: vec!["tern".to_owned()],
            sessions: vec![
                session("BUSY01", "skiff", QUEST, "busy", &none),
                session("NAPS02", "skiff", QUEST, "ended", &parked("this-boot")),
                session("OLDNAP", "skiff", QUEST, "ended", &parked("a-boot-before")),
                session("THERE3", "tern", QUEST, "idle", &none),
                session("OTHER4", "skiff", "QEST-20261107-OTHERS", "idle", &none),
            ],
            boot: Some("this-boot".to_owned()),
            carries: ["s-busy01", "s-naps02"].map(str::to_owned).into(),
            ends: true,
            patience: 3,
            afresh: 3,
            ..Realm::default()
        }
    }

    /// Carries HARBOR from skiff to `to` in `realm`, with each line said.
    fn carried(realm: &mut Realm, to: &str) -> (Result<Carried, Failed>, Vec<Value>) {
        let mut lines = Vec::new();
        let done = carry(
            realm,
            QUEST,
            "skiff",
            to,
            Asked::Person,
            &mut |line: &Value| {
                lines.push(line.clone());
            },
        );
        (done, lines)
    }

    /// Carries HARBOR from skiff to tern in `realm` as its
    /// orchestrator orders it, for a camera, with each line said.
    fn ordered(realm: &mut Realm) -> (Result<Carried, Failed>, Vec<Value>) {
        let mut lines = Vec::new();
        let done = carry(
            realm,
            QUEST,
            "skiff",
            "tern",
            Asked::Ordered("it needs the camera"),
            &mut |line: &Value| lines.push(line.clone()),
        );
        (done, lines)
    }

    /// Carries the session `session` of HARBOR from skiff to tern in
    /// `realm` as it asks of itself, for load, with each line said.
    fn itself(realm: &mut Realm, session: &str) -> (Result<Carried, Failed>, Vec<Value>) {
        let mut lines = Vec::new();
        let done = carry(
            realm,
            QUEST,
            "skiff",
            "tern",
            Asked::Itself {
                session,
                why: "skiff is short of memory",
            },
            &mut |line: &Value| lines.push(line.clone()),
        );
        (done, lines)
    }

    /// Each line's step and how it stood, as "push done".
    fn steps(lines: &[Value]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                let word = |field: &str| line[field].as_str().unwrap_or("").to_owned();
                format!("{} {}", word("step"), word("state"))
            })
            .collect()
    }

    /// What the last line said.
    fn last_said(lines: &[Value]) -> String {
        lines
            .last()
            .and_then(|line| line["said"].as_str())
            .unwrap_or("")
            .to_owned()
    }

    #[test]
    fn a_quest_is_carried_from_the_one_machine_its_sessions_run_on() {
        let none = json!({});
        let parked = json!({"state": "parked"});
        let only = |sessions: &[Session]| runs_on(sessions, QUEST);
        assert_eq!(
            only(&[
                session("GONE01", "skiff", QUEST, "ended", &none),
                session("THERE2", "tern", QUEST, "busy", &none),
                session("NAPS03", "tern", QUEST, "ended", &parked),
                session("OTHER4", "kestrel", "QEST-20261107-OTHERS", "idle", &none),
            ]),
            Ok("tern".to_owned()),
            "it runs, or is parked, on tern alone; an ended one and another quest don't count"
        );
        assert_eq!(
            only(&[session("GONE01", "skiff", QUEST, "ended", &none)]),
            Err(format!(
                "no session of {QUEST} runs on any machine in reach, so there's nothing to carry"
            )),
            "a quest nothing runs is carried from nowhere"
        );
        assert_eq!(
            only(&[
                session("THERE2", "tern", QUEST, "busy", &none),
                session("HERE01", "skiff", QUEST, "idle", &none),
            ]),
            Err(format!(
                "{QUEST}'s sessions run on skiff and tern, so which machine to carry it from \
                 can't be told"
            )),
            "a quest split across machines isn't guessed at"
        );
    }

    #[test]
    fn each_carried_sessions_cell_brings_its_ties_before_any_is_told() {
        let mut realm = realm_in_a_tree();
        let formed = |name: &str, at: i64| {
            json!({"key": format!("formed:{name}"), "kind": "formed",
                   "name": name, "at": at, "left": null})
        };
        realm.formed = vec![
            formed("tree.github:harbour/oars:jsama/tides", 7),
            formed("tree.github:harbour/sails:main", 8),
        ];
        realm.ties = vec![
            json!({"key": "tie:BUSY01~KESTRL", "kind": "tie", "strength": 2.5}),
            json!({"key": "tie:AAAAAA~NAPS02", "kind": "tie", "strength": 1.0}),
            json!({"key": "tie:BUSY01", "kind": "tie", "ties": 1}),
            json!({"key": "tie:OTHER4~ZZZZZZ", "kind": "tie"}),
        ];
        let (done, lines) = carried(&mut realm, "tern");
        assert!(done.is_ok(), "carried: {}", last_said(&lines));
        let cell =
            |session: &str, cell: &str, ties: &[usize], talk: Option<usize>, formed: &[usize]| {
                Brought {
                    session: session.to_owned(),
                    cell: cell.to_owned(),
                    ties: ties.iter().map(|at| realm.ties[*at].clone()).collect(),
                    talk: talk.map(|at| realm.ties[at].clone()),
                    formed: formed.iter().map(|at| realm.formed[*at].clone()).collect(),
                }
            };
        assert_eq!(
            realm.brought,
            [
                (cell("s-busy01", "BUSY01", &[0], Some(2), &[0]), 0),
                (cell("s-naps02", "NAPS02", &[1], None, &[]), 0),
            ],
            "each session to be carried brings its own ties, and the networks its trees \
             formed, before any session is told"
        );
        assert_eq!(
            (realm.ties_asked, realm.formed_asked),
            (1, 1),
            "the ties and the formed networks are asked for once"
        );
    }

    #[test]
    fn a_session_that_arrived_as_another_cell_is_carried_on_as_that_cell() {
        let mut realm = realm();
        realm.ties = vec![
            json!({"key": "tie:HARBOR~KELP01", "kind": "tie", "strength": 2.0}),
            json!({"key": "tie:BUSY01~HARBOR", "kind": "tie", "strength": 1.0}),
        ];
        // BUSY01 arrived here as KELP01, so the node names its cell so; a
        // cell of another session's, or with no name, names none.
        realm.cell_records = vec![
            json!({"key": "cell:KELP01", "kind": "cell", "name": "KELP01", "id": "r-BUSY01"}),
            json!({"key": "cell:OTHER4", "kind": "cell", "name": "OTHER4", "id": "r-OTHER4"}),
            json!({"key": "cell:", "kind": "cell", "name": "", "id": "r-NAPS02"}),
        ];
        let (done, lines) = carried(&mut realm, "tern");
        assert!(done.is_ok(), "carried: {}", last_said(&lines));
        let named: Vec<(&str, &str, Vec<&Value>)> = realm
            .brought
            .iter()
            .map(|(brought, _)| {
                (
                    brought.session.as_str(),
                    brought.cell.as_str(),
                    brought.ties.iter().collect(),
                )
            })
            .collect();
        assert_eq!(
            named,
            [
                ("s-busy01", "KELP01", vec![&realm.ties[0]]),
                ("s-naps02", "NAPS02", Vec::new()),
            ],
            "a second carry brings the cell the session arrived as, with its ties"
        );
    }

    #[test]
    fn an_earlier_carrys_cells_go_before_any_session_is_told_and_a_failed_ones_stay() {
        let done = |session: &str| Done {
            session: session.to_owned(),
            started_before: None,
        };
        let mut realm = realm();
        // An earlier carry's, which finished; one whose session refused
        // it; and a failed attempt's, beside its done file not started.
        realm.cells = ["s-gone", "s-refuse", "s-tried"].map(str::to_owned).into();
        realm.handover = vec![(done("s-gone"), true), (done("s-tried"), false)];
        let (outcome, lines) = carried(&mut realm, "tern");
        assert!(outcome.is_ok(), "carried: {}", last_said(&lines));
        assert_eq!(
            realm.forgotten,
            [("s-gone".to_owned(), 0), ("s-refuse".to_owned(), 0)],
            "an earlier carry's cells go before any session is told, though pi-carry \
             writes s-gone's done file afresh"
        );
        assert_eq!(
            realm.cells,
            ["s-busy01", "s-naps02", "s-tried"]
                .map(str::to_owned)
                .into(),
            "a failed attempt's cell stays beside its work, with this carry's own"
        );
    }

    #[test]
    fn ties_are_asked_for_only_when_a_session_is_carried() {
        let mut realm = realm();
        assert_eq!(super::bring_cells(&mut realm, &[]), Ok(BTreeSet::new()));
        assert_eq!(
            (realm.ties_asked, realm.formed_asked, realm.brought.len()),
            (0, 0, 0),
            "a quest with no session here asks nothing and brings nothing"
        );
        let mut unnamed = session("BUSY01", "skiff", QUEST, "busy", &json!({}));
        if let Some(presence) = Arc::make_mut(&mut unnamed.presence).as_object_mut() {
            presence.remove("session");
        }
        assert_eq!(
            super::bring_cells(&mut realm, &[unnamed]),
            Ok(BTreeSet::new())
        );
        assert_eq!(
            (realm.ties_asked, realm.formed_asked, realm.brought.len()),
            (0, 0, 0),
            "nor one whose presence names no session to carry"
        );
        let treeless = realm.sessions[0].clone();
        assert!(super::bring_cells(&mut realm, &[treeless]).is_ok());
        assert_eq!(
            (realm.ties_asked, realm.formed_asked, realm.brought.len()),
            (1, 0, 1),
            "a session whose trees make no network asks no formed networks"
        );
    }

    #[test]
    fn ties_not_said_or_not_put_stop_the_carry_before_any_session_is_told() {
        for (realm, why) in [
            (
                Realm {
                    unsaid_ties: Some("no answer".to_owned()),
                    ..realm()
                },
                "the node didn't say its ties: no answer",
            ),
            (
                Realm {
                    unsaid_formed: Some("no answer".to_owned()),
                    ..realm_in_a_tree()
                },
                "the node didn't say which networks formed: no answer",
            ),
            (
                Realm {
                    unsaid_formed: Some("no answer".to_owned()),
                    ..realm_sharing_a_folder()
                },
                "the node didn't say which networks formed: no answer",
            ),
            (
                Realm {
                    unbrought: Some("the disk is full".to_owned()),
                    ..realm()
                },
                "BUSY01's cell wasn't put beside its work: the disk is full",
            ),
            (
                Realm {
                    unforgotten: Some("the disk is gone".to_owned()),
                    ..realm()
                },
                "an earlier carry's cells weren't taken away: the disk is gone",
            ),
        ] {
            let mut realm = realm;
            let (done, lines) = carried(&mut realm, "tern");
            assert_eq!(done, Err(Failed { step: Step::Push }), "{why}");
            assert!(last_said(&lines).contains(why), "{}", last_said(&lines));
            assert!(
                realm.steered.is_empty() && realm.shared.is_empty() && realm.started.is_empty(),
                "no session told, and nothing shared or started: {why}"
            );
            assert!(realm.brought.is_empty(), "no cell brought: {why}");
        }
    }

    /// The realm, its first session holding a worktree, so its trees
    /// make a network.
    fn realm_in_a_tree() -> Realm {
        let mut realm = realm();
        let tides = json!({"trees": [
            {"key": "oars-tides", "repo": "github:harbour/oars", "branch": "jsama/tides"},
        ]});
        realm.sessions[0] = session("BUSY01", "skiff", QUEST, "busy", &tides);
        realm
    }

    /// The realm, its first session sharing a folder and holding no
    /// tree, so only its share makes a network.
    fn realm_sharing_a_folder() -> Realm {
        let mut realm = realm();
        let harbour = json!({"shares": [
            {"key": "quest:QEST-20261112-HARBOR", "for": "Harbour", "folder": "/h"},
        ]});
        realm.sessions[0] = session("BUSY01", "skiff", QUEST, "busy", &harbour);
        realm
    }

    // Round 1 (2026-11-20): an ordered carry's every start, the quest's
    // own trees' too, reads back as the arrival it is, and one shaped
    // otherwise by a single word reads as none.
    #[test]
    fn an_ordered_arrival_is_read_from_its_start_word_for_word() {
        let ordered = Ordered {
            by: "WCJQSG".to_owned(),
            why: "it needs the lens".to_owned(),
        };
        let done = |session: &str| Done {
            session: session.to_owned(),
            started_before: None,
        };
        for session in ["s-busy01", QUEST] {
            let start = super::arriving(QUEST, "skiff", "tern", &done(session), Some(&ordered));
            assert_eq!(
                super::ordered_arrival(&start.command),
                Some(super::OrderedArrival {
                    arrives: super::Arrives {
                        quest: QUEST,
                        from: "skiff",
                        session,
                    },
                    by: "WCJQSG",
                    why: "it needs the lens",
                }),
                "{session}'s arrival reads back"
            );
            let mut other = start.command.clone();
            other[2] = other[2].replace("--why=", "--reason=");
            assert_eq!(
                super::ordered_arrival(&other),
                None,
                "{session}'s, a word off"
            );
            // Pi takes word 7 as its first prompt, so one of the cell's
            // own choosing is other work, however right the script.
            let mut prompted = start.command.clone();
            prompted[7] = "build the whole thing".to_owned();
            assert_eq!(
                super::ordered_arrival(&prompted),
                None,
                "{session}'s, told something else"
            );
            let unordered = super::arriving(QUEST, "skiff", "tern", &done(session), None);
            assert_eq!(
                super::ordered_arrival(&unordered.command),
                None,
                "{session}'s, unordered"
            );
            let arrives = super::Arrives {
                quest: QUEST,
                from: "skiff",
                session,
            };
            assert_eq!(
                unordered_arrival(&unordered.command),
                Some(arrives),
                "{session}'s unordered arrival reads back as one"
            );
            assert_eq!(
                unordered_arrival(&start.command),
                None,
                "{session}'s, ordered"
            );
            let mut prompted = unordered.command.clone();
            prompted[7] = "build the whole thing".to_owned();
            assert_eq!(
                unordered_arrival(&prompted),
                None,
                "{session}'s unordered, told something else"
            );
        }
    }

    #[test]
    fn a_quest_is_carried_step_by_step_with_its_sessions_here() {
        let mut realm = realm();
        let (done, lines) = carried(&mut realm, "tern");
        let done = done.expect("carried");
        assert_eq!(
            steps(&lines),
            [
                "find doing",
                "find done",
                "push doing",
                "push done",
                "share doing",
                "share done",
                "start doing",
                "start done",
                "close doing",
                "close done",
                "answer doing",
                "answer done"
            ],
            "each step said as it begins and as it ends: {lines:?}"
        );
        let told: Vec<&str> = realm.steered.iter().map(|(at, _)| at.as_str()).collect();
        assert_eq!(
            told,
            ["BUSY01", "NAPS02"],
            "only those of the quest here that can carry"
        );
        assert_eq!(realm.steered[0].1["act"], "carry", "told to carry");
        assert_eq!(realm.steered[0].1["to"], "tern", "to the machine named");
        assert_eq!(
            realm.rest.as_deref(),
            Some("QEST-20261107-HARBOR tern"),
            "pi-carry takes what no session carried"
        );
        assert_eq!(realm.shared, [QUEST], "shared once, after the push");
        assert_eq!(
            realm.started.len(),
            4,
            "each carried session starts there, and the quest's trees arrive"
        );
        // After the sessions told, and before those found only after the
        // ask, as start_each orders them.
        let trees = &realm.started[2];
        assert_eq!(
            &trees.command[3..7],
            [PI_CARRY, QUEST, "skiff", QUEST],
            "the quest's own trees arrive by its done file"
        );
        assert!(
            !trees.command[2].contains("exec pi"),
            "and no session starts for them: {}",
            trees.command[2]
        );
        assert_eq!(super::arrives(&trees.command), None, "so none arrives");
        let first = &realm.started[0];
        assert!(
            matches!(first.work.needs.as_slice(), [Need::Machine(on)] if on == "tern"),
            "on the machine named: {:?}",
            first.work.needs
        );
        assert_eq!(
            &first.command[3..],
            [
                PI_CARRY,
                QUEST,
                "skiff",
                "s-busy01",
                &super::carry_on("skiff")
            ],
            "arriving with its own session, told to carry on, its folder left to pi-carry"
        );
        assert!(
            first.command[2].starts_with("pi-carry arrive"),
            "arrives before pi starts"
        );
        assert_eq!(
            super::arrives(&first.command),
            Some(super::Arrives {
                quest: QUEST,
                from: "skiff",
                session: "s-busy01",
            }),
            "its start says which session arrives, from where"
        );
        assert_eq!(done.machine, "tern", "said where it went");
        let addresses: Vec<&str> = done
            .sessions
            .iter()
            .map(|one| one.address.as_str())
            .collect();
        assert_eq!(
            addresses,
            ["BUSY01", "NAPS02", "s-gone"],
            "each session by its address here, and not the quest's trees"
        );
        assert_eq!(
            done.sessions[0].attach,
            ["mycelium", "attach", "h0"],
            "with words to join it"
        );
    }

    /// Runs a carried session's arrival as the shell there would, with
    /// `pi-carry` and `pi` stood in for under `home`: `pi-carry arrive`
    /// exiting as `arrives` says and `pi-carry where` printing `where`,
    /// or failing where it names none; `pi` writing its folder, its words
    /// and the quest it's told to load to `ran`, and its first prompt, the
    /// words after `--`, to `told`. The session ran in `~/checkout/src`
    /// where it left, which is here too, and no tree brought; its done
    /// file, as an older harness wrote it, names no fence.
    fn arrive(home: &std::path::Path, where_: Option<&str>, arrives: bool) -> Option<String> {
        arrive_fenced(home, where_, arrives, (Some(""), None))
    }

    /// The same, with `pi-carry fence` printing `fence`, or failing where
    /// it's none, the node starting it with `PI_SANDBOX` as `node` says,
    /// and `pi` writing the `PI_SANDBOX` it started under, or `unset`, to
    /// `sandbox`.
    fn arrive_fenced(
        home: &std::path::Path,
        where_: Option<&str>,
        arrives: bool,
        (fence, node): (Option<&str>, Option<&str>),
    ) -> Option<String> {
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let ran = home.join("ran");
        let stand_in = |name: &str, body: &str| {
            crate::stand_in::program(&bin.join(name), &format!("#!/bin/sh\n{body}\n"));
        };
        std::fs::create_dir_all(home.join("checkout/src")).expect("a checkout");
        stand_in(
            PI_CARRY,
            &format!(
                "case \"$1\" in arrive) exit {} ;; where) {} ;; fence) {} ;; *) exit 2 ;; esac",
                if arrives { 0 } else { 3 },
                where_.map_or_else(|| "exit 1".to_owned(), |at| format!("echo '{at}'")),
                fence.map_or_else(|| "exit 1".to_owned(), |kind| format!("echo '{kind}'")),
            ),
        );
        stand_in(
            "pi",
            &format!(
                "echo \"$(pwd) $1 $2 for $QUEST_WORKFLOW_AUTOLOAD_ID\" > {}; \
                 echo \"${{PI_SANDBOX-unset}}\" > {}; \
                 [ \"$3\" = -- ] && printf '%s' \"$4\" > {}",
                ran.display(),
                home.join("sandbox").display(),
                home.join("told").display()
            ),
        );
        let one = Done {
            session: "s-busy01".to_owned(),
            started_before: None,
        };
        let start = super::arriving(QUEST, "skiff", "tern", &one, None);
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut start = {
            let mut command = std::process::Command::new(&start.command[0]);
            command
                .args(&start.command[1..])
                .env("HOME", home)
                .env("PATH", path)
                .env_remove("PI_SANDBOX");
            command
        };
        if let Some(node) = node {
            start.env("PI_SANDBOX", node);
        }
        start.status().expect("the shell runs");
        std::fs::read_to_string(ran)
            .ok()
            .map(|said| said.trim().to_owned())
    }

    #[test]
    fn a_carried_session_starts_where_its_arrival_says_once_it_has_arrived() {
        let home = tempfile::tempdir().expect("a home");
        let home = home.path().canonicalize().expect("its path");
        let tree = home.join("src/harbour");
        std::fs::create_dir_all(&tree).expect("a tree");
        let tree = tree.display().to_string();
        assert_eq!(
            arrive(&home, Some(&tree), true),
            Some(format!("{tree} --session s-busy01 for {QUEST}")),
            "in the folder pi-carry says it arrived in, its quest loaded"
        );
        std::fs::remove_file(home.join("ran")).expect("forgotten");
        assert_eq!(
            arrive(&home, Some(&tree), false),
            None,
            "never once its arrival failed"
        );
        assert_eq!(
            arrive(&home, None, true),
            None,
            "nor when pi-carry names no folder"
        );
    }

    #[test]
    fn a_carried_session_never_enters_a_folder_no_tree_brought() {
        let home = tempfile::tempdir().expect("a home");
        let home = home.path().canonicalize().expect("its path");
        let scratch = home.join("quest-workspace").join(QUEST);
        std::fs::create_dir_all(&scratch).expect("its scratch");
        let scratch = scratch.display().to_string();
        assert_eq!(
            arrive(&home, Some(&scratch), true),
            Some(format!("{scratch} --session s-busy01 for {QUEST}")),
            "its checkout is here, but no tree brought it, so it arrives in its scratch"
        );
        std::fs::remove_file(home.join("ran")).expect("forgotten");
        let missing = home.join("src/never-brought");
        assert_eq!(
            arrive(&home, Some(&missing.display().to_string()), true),
            None,
            "and nothing starts in a folder that isn't there"
        );
        assert!(!missing.exists(), "which is never made");
    }

    #[test]
    fn a_carried_session_is_told_to_carry_on_where_it_left_off() {
        let home = tempfile::tempdir().expect("a home");
        let home = home.path().canonicalize().expect("its path");
        let at = home.display().to_string();
        arrive(&home, Some(&at), true).expect("pi started");
        let told = std::fs::read_to_string(home.join("told")).unwrap_or_default();
        assert_eq!(
            told,
            "[Automated message, no human input: pi-carry carried this session here \
             from skiff.] Carry on from where you left off: your plan's next open item.",
            "its first prompt says where it came from and sets it to work, \
             so it never sits idle until a message wakes it"
        );
    }

    #[test]
    fn a_carried_session_starts_in_the_fence_it_ran_in() {
        let home = tempfile::tempdir().expect("a home");
        let home = home.path().canonicalize().expect("its path");
        let at = home.display().to_string();
        let sandbox = || {
            std::fs::read_to_string(home.join("sandbox"))
                .map(|said| said.trim().to_owned())
                .unwrap_or_default()
        };
        for kind in ["strict", "fence", "off"] {
            arrive_fenced(&home, Some(&at), true, (Some(kind), None)).expect("pi started");
            assert_eq!(sandbox(), kind, "pi starts as the done file says it ran");
        }
        arrive_fenced(&home, Some(&at), true, (Some(""), None)).expect("pi started");
        assert_eq!(
            sandbox(),
            "unset",
            "a done file naming no fence leaves pi as the node starts it"
        );
        for (node, done, starts) in [
            ("strict", "off", "strict"),
            ("strict", "fence", "strict"),
            ("strict", "", "strict"),
            ("fence", "off", "fence"),
            ("fence", "", "fence"),
            ("fence", "strict", "strict"),
        ] {
            arrive_fenced(&home, Some(&at), true, (Some(done), Some(node))).expect("pi started");
            assert_eq!(
                sandbox(),
                starts,
                "the tighter of the node's fence ({node}) and the done file's ({done:?})"
            );
        }
        std::fs::remove_file(home.join("ran")).expect("forgotten");
        assert_eq!(
            arrive_fenced(&home, Some(&at), true, (None, None)),
            None,
            "and none starts where its fence can't be told, rather than start looser"
        );
    }

    #[test]
    fn a_machine_out_of_reach_or_this_one_is_refused_before_anything_moves() {
        for to in ["lighthouse", "skiff"] {
            let mut realm = realm();
            let (done, lines) = carried(&mut realm, to);
            assert_eq!(done, Err(Failed { step: Step::Find }), "{to} is refused");
            assert_eq!(
                steps(&lines),
                ["find doing", "find failed"],
                "{to}: {lines:?}"
            );
            assert!(
                realm.steered.is_empty() && realm.shared.is_empty(),
                "{to}: nothing moved"
            );
        }
    }

    #[test]
    fn a_worker_that_takes_no_carries_is_refused_before_anything_moves() {
        let mut realm = Realm {
            lent: [("tern".to_owned(), Lent::TakesNone)].into(),
            ..realm()
        };
        let (done, lines) = carried(&mut realm, "tern");
        assert_eq!(done, Err(Failed { step: Step::Find }), "refused");
        assert_eq!(
            lines.last().and_then(|line| line["said"].as_str()),
            Some(ground::takes_none("tern").as_str()),
            "saying why: {lines:?}"
        );
        assert!(
            realm.steered.is_empty() && realm.shared.is_empty() && realm.started.is_empty(),
            "nothing moved"
        );
    }

    #[test]
    fn a_worker_that_takes_carries_is_given_the_quest_as_it_is_shared() {
        let mut worker = Realm {
            lent: [("tern".to_owned(), Lent::TakesCarries)].into(),
            ..realm()
        };
        let (done, _) = carried(&mut worker, "tern");
        done.expect("carried");
        assert_eq!(worker.shared, [QUEST], "shared once");
        assert_eq!(worker.given, ["tern"], "and given to the worker alone");
        let mut own = realm();
        carried(&mut own, "tern").0.expect("carried");
        assert!(
            own.given.is_empty(),
            "one of the person's own is given nothing"
        );
    }

    #[test]
    fn a_session_that_never_carries_stops_the_carry_before_its_share() {
        let mut realm = Realm {
            carries: ["s-busy01"].map(str::to_owned).into(),
            ..realm()
        };
        let (done, lines) = carried(&mut realm, "tern");
        assert_eq!(
            done,
            Err(Failed { step: Step::Push }),
            "it fails at the push"
        );
        let said = last_said(&lines);
        assert!(
            said.contains("NAPS02"),
            "naming the one that didn't: {said}"
        );
        assert!(
            realm.shared.is_empty() && realm.started.is_empty(),
            "nothing shared or started"
        );
        assert!(
            realm.cells.contains("s-naps02"),
            "and the slow one keeps its cell, since it may still be carrying: {:?}",
            realm.cells
        );
    }

    #[test]
    fn a_session_started_there_that_never_runs_fails_the_carry() {
        // 27 November: a carry onto a room was held there, but its
        // arrival exited at once, as the room had no pi-carry, so its
        // session ran nowhere.
        let mut realm = Realm {
            silent: ["s-naps02"].map(str::to_owned).into(),
            ..realm()
        };
        let (done, lines) = carried(&mut realm, "tern");
        assert_eq!(
            done,
            Err(Failed { step: Step::Answer }),
            "a start there isn't a carry until the session runs there"
        );
        let said = last_said(&lines);
        assert!(
            said.contains("NAPS02") && !said.contains("BUSY01"),
            "naming the one that doesn't run, not one that does: {said}"
        );
        assert!(
            said.contains("mycelium attach h1"),
            "with how to join its start, to see why: {said}"
        );
    }

    #[test]
    fn a_session_found_running_on_another_machine_fails_the_carry() {
        let mut realm = Realm {
            astray: ["s-naps02"].map(str::to_owned).into(),
            ..realm()
        };
        let (done, lines) = carried(&mut realm, "tern");
        assert_eq!(
            done,
            Err(Failed { step: Step::Answer }),
            "a session runs there only on the machine it was started on"
        );
        let said = last_said(&lines);
        assert!(
            said.contains("NAPS02") && !said.contains("BUSY01"),
            "naming the one running elsewhere, not one running there: {said}"
        );
    }

    #[test]
    fn a_session_whose_record_there_has_ended_fails_the_carry() {
        let mut realm = Realm {
            ended_there: ["s-naps02"].map(str::to_owned).into(),
            ..realm()
        };
        let (done, lines) = carried(&mut realm, "tern");
        assert_eq!(
            done,
            Err(Failed { step: Step::Answer }),
            "a session there that has ended doesn't run there"
        );
        let said = last_said(&lines);
        assert!(
            said.contains("NAPS02") && !said.contains("BUSY01"),
            "naming the one that ended, not one running there: {said}"
        );
    }

    #[test]
    fn a_session_running_there_under_another_id_fails_the_carry() {
        let mut realm = Realm {
            misnamed: ["s-naps02"].map(str::to_owned).into(),
            ..realm()
        };
        let (done, lines) = carried(&mut realm, "tern");
        assert_eq!(
            done,
            Err(Failed { step: Step::Answer }),
            "a session runs there only as its own id, not another running there"
        );
        let said = last_said(&lines);
        assert!(
            said.contains("NAPS02") && !said.contains("BUSY01"),
            "naming the one listed under another id, not one running there: {said}"
        );
    }

    #[test]
    fn a_carry_that_starts_only_the_quests_own_trees_skips_its_answer() {
        let none = json!({});
        let mut realm = Realm {
            sessions: vec![
                session("THERE3", "tern", QUEST, "idle", &none),
                session("OTHER4", "skiff", "QEST-20261107-OTHERS", "idle", &none),
            ],
            rest_trees_alone: true,
            ..realm()
        };
        let (done, lines) = carried(&mut realm, "tern");
        let done = done.expect("carried, its trees started there");
        assert_eq!(
            arrivals(&realm.started),
            [QUEST],
            "its trees alone start there"
        );
        assert!(done.sessions.is_empty(), "and no session: {done:?}");
        let stood = steps(&lines);
        assert!(
            stood.contains(&"close skipped".to_owned())
                && stood.contains(&"answer skipped".to_owned()),
            "neither a session here to end nor one there to wait for: {lines:?}"
        );
        assert_eq!(last_said(&lines), "no session started there");
    }

    #[test]
    fn a_session_started_there_is_waited_for_until_it_runs() {
        let mut realm = Realm { slow: 2, ..realm() };
        let (done, lines) = carried(&mut realm, "tern");
        let done = done.expect("carried, once each runs there");
        assert_eq!(
            done.sessions.len(),
            3,
            "each told, and the one found after: {done:?}"
        );
        assert_eq!(
            last_said(&lines),
            "3 sessions running on tern",
            "said once each runs there"
        );
    }

    #[test]
    fn a_session_started_there_is_waited_for_after_a_close_that_spent_the_wait() {
        // A busy session may spend the whole wait finishing its turn
        // here, and its start there still needs a moment to run. Each
        // patience is just what its step needs, so a fresh wait given
        // before the close, which it would then spend, fails the close.
        let mut realm = Realm {
            patience: 2,
            afresh: 1,
            ends_after: 2,
            slow: 4,
            ..realm()
        };
        let (done, lines) = carried(&mut realm, "tern");
        let done = done.expect("carried, once each runs there");
        assert_eq!(
            realm.looks, 5,
            "three looks until the sessions here end, two until they run there: {lines:?}"
        );
        assert_eq!(
            done.sessions.len(),
            3,
            "each told, and the one found after: {done:?}"
        );
        assert_eq!(
            last_said(&lines),
            "3 sessions running on tern",
            "said once each runs there"
        );
    }

    #[test]
    fn a_session_still_running_here_after_it_carried_is_said() {
        let mut realm = Realm {
            ends: false,
            ..realm()
        };
        let (done, lines) = carried(&mut realm, "tern");
        assert_eq!(
            done,
            Err(Failed { step: Step::Close }),
            "it fails at the close"
        );
        let said = last_said(&lines);
        assert!(
            said.contains("BUSY01"),
            "naming the one still running: {said}"
        );
        assert_eq!(
            realm.started.len(),
            4,
            "what started there, the quest's own trees' arrival too, stays started"
        );
    }

    #[test]
    fn an_ordered_carry_arrives_carried_by_its_orchestrator_for_its_reason() {
        let mut unproved = realm();
        let mut realm = Realm {
            by: Some("WCJQSG".to_owned()),
            ..realm()
        };
        let (done, lines) = ordered(&mut realm);
        done.expect("carried");
        assert_eq!(
            realm.steered[0].1,
            json!({"act": "ordered-carry", "to": "tern", "why": "it needs the camera"}),
            "each session told by the act only its orchestrator may order, \
             saying nothing of who, which the node stamps: {lines:?}"
        );
        for start in &realm.started {
            assert_eq!(
                &start.command[8..],
                ["WCJQSG", "it needs the camera"],
                "each arrival, the quest's own trees' too, carried by the orchestrator \
                 the node proved, for its reason"
            );
            assert!(
                start.command[2].contains(r#"--by="$5" --why="$6""#),
                "which pi-carry arrive is told: {}",
                start.command[2]
            );
        }
        let (done, lines) = ordered(&mut unproved);
        assert_eq!(
            done,
            Err(Failed { step: Step::Push }),
            "a node that doesn't say who ordered it carries nothing"
        );
        assert!(
            unproved.started.is_empty(),
            "and nothing arrives as anyone's: {lines:?}"
        );
    }

    /// Carries on, as the node does, the carry of HARBOR's `session` from
    /// skiff to tern it asked of itself, once told, ordered by `by`.
    fn told(
        realm: &mut Realm,
        session: &str,
        whole: bool,
        by: &str,
    ) -> (Result<Carried, Failed>, Vec<Value>) {
        let one = realm
            .sessions
            .iter()
            .find(|one| session_id(one) == Some(session))
            .cloned()
            .expect("a session of the realm");
        let ordered = Ordered {
            by: by.to_owned(),
            why: "skiff is short of memory".to_owned(),
        };
        let mut lines = Vec::new();
        let done = carry_told(
            realm,
            (QUEST, "skiff", "tern"),
            (std::slice::from_ref(&one), whole),
            &ordered,
            &mut |line: &Value| lines.push(line.clone()),
        );
        (done, lines)
    }

    #[test]
    fn a_session_carrying_itself_hands_the_rest_to_the_node_once_told() {
        let mut realm = realm();
        let (done, lines) = itself(&mut realm, "s-busy01");
        let carried = done.expect("handed");
        assert_eq!(carried.step, "handed", "its last line says so: {lines:?}");
        assert_eq!(carried.sessions, [], "none has arrived yet");
        assert!(
            realm.started.is_empty() && realm.shared.is_empty(),
            "nothing is shared or started below the session, which can't end while \
             the command runs: {lines:?}"
        );
        assert!(
            last_said(&lines).contains("node carries the rest"),
            "and the push says who carries on: {lines:?}"
        );
    }

    #[test]
    fn the_node_carrying_on_starts_only_the_sessions_own_work() {
        for (whole, own) in [(false, vec!["s-busy01"]), (true, vec!["s-busy01", QUEST])] {
            let mut realm = realm();
            // An earlier carry left NAPS02's done file unstarted.
            realm.write("s-naps02");
            let _ = itself(&mut realm, "s-busy01");
            let (done, lines) = told(&mut realm, "s-busy01", whole, "BUSY01");
            done.expect("carried");
            assert_eq!(
                arrivals(&realm.started),
                own,
                "never what an earlier carry left nor an ended session's work, and the \
                 quest's own trees only where it was the quest's only session here: {lines:?}"
            );
            assert_eq!(
                realm.rest.is_some(),
                whole,
                "the rest goes only with it whole"
            );
        }
    }

    #[test]
    fn a_session_carries_itself_alone_and_arrives_carried_by_itself() {
        let mut realm = realm();
        let (done, lines) = itself(&mut realm, "s-busy01");
        let carried = done.expect("handed");
        assert_eq!(
            realm.steered,
            [(
                "BUSY01".to_owned(),
                json!({"act": "self-carry", "to": "tern", "why": "skiff is short of memory"})
            )],
            "only the session asking is told, by the act only it may ask: {lines:?}"
        );
        assert_eq!(
            carried.skipped,
            ["NAPS02"],
            "the quest's other session here stays"
        );
        assert!(
            lines[1]["said"]
                .as_str()
                .is_some_and(|said| said.contains("NAPS02 stays here")),
            "and the find says so: {}",
            lines[1]
        );
        let (done, _) = told(&mut realm, "s-busy01", false, "BUSY01");
        done.expect("carried");
        assert_eq!(
            realm
                .started
                .iter()
                .map(|start| start.command[8..].to_vec())
                .collect::<Vec<_>>(),
            [["BUSY01", "skiff is short of memory"]],
            "it alone arrives, carried by itself for its reason, and not the quest's own \
             trees, which stay with the session staying"
        );
        assert!(
            realm
                .asked_takes
                .iter()
                .all(|taking| taking.ordered && taking.starts.len() == 1),
            "the node is asked about its start alone, as an agent's"
        );
    }

    #[test]
    fn a_session_carries_itself_only_where_it_runs_the_quest_here() {
        for session in ["s-there3", "s-other4", "s-oldnap"] {
            let mut realm = realm();
            let (done, lines) = itself(&mut realm, session);
            assert_eq!(
                done,
                Err(Failed { step: Step::Find }),
                "{session} doesn't run HARBOR on skiff now: {lines:?}"
            );
            assert!(realm.steered.is_empty(), "so no session is told");
        }
    }

    #[test]
    fn a_session_carrying_itself_arrives_as_the_node_proved_it() {
        let mut realm = realm();
        let _ = itself(&mut realm, "s-busy01");
        let (done, _) = told(&mut realm, "s-busy01", false, "NAPS02");
        done.expect("the carry goes, as the node said who");
        assert_eq!(
            realm.started[0].command[8], "NAPS02",
            "it arrives as the node proved, never as the client says"
        );
    }

    #[test]
    fn a_reason_opening_with_a_dash_reaches_pi_carry_as_the_reason() {
        let dir = tempfile::tempdir().expect("a folder");
        let said = dir.path().join("said");
        // By its whole path, since the arrival runs on a PATH holding only
        // the stand-in.
        let shell = std::env::var_os("BASH")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::split_paths(&std::env::var_os("PATH")?)
                    .map(|folder| folder.join("sh"))
                    .find(|sh| sh.is_file())
            })
            .expect("a shell");
        let fake = dir.path().join("pi-carry");
        crate::stand_in::program(
            &fake,
            &format!(
                "#!{}\nprintf '%s\\n' \"$@\" > {}\n",
                shell.display(),
                said.display()
            ),
        );
        let one = Done {
            session: QUEST.to_owned(),
            started_before: None,
        };
        let ordered = Ordered {
            by: "WCJQSG".to_owned(),
            why: "--force: it needs the camera".to_owned(),
        };
        let start = super::arriving(QUEST, "skiff", "tern", &one, Some(&ordered));
        let ran = std::process::Command::new(&shell)
            .args(&start.command[1..])
            .env("PATH", dir.path())
            .status()
            .expect("the arrival runs");
        assert!(ran.success(), "it ran: {ran}");
        let said = std::fs::read_to_string(&said).expect("pi-carry was run");
        assert_eq!(
            said.lines().skip(6).collect::<Vec<_>>(),
            ["--by=WCJQSG", "--why=--force: it needs the camera"],
            "each value joined to its option: {said}"
        );
    }

    #[test]
    fn a_carry_ordered_without_a_reason_is_refused() {
        for why in ["", "  \t"] {
            let mut realm = Realm {
                by: Some("WCJQSG".to_owned()),
                ..realm()
            };
            let mut lines = Vec::new();
            let done = carry(
                &mut realm,
                QUEST,
                "skiff",
                "tern",
                Asked::Ordered(why),
                &mut |line: &Value| {
                    lines.push(line.clone());
                },
            );
            assert_eq!(done, Err(Failed { step: Step::Find }), "{why:?} is refused");
            assert_eq!(
                last_said(&lines),
                "an ordered carry says why, and a blank reason says nothing",
                "saying so"
            );
            assert!(realm.steered.is_empty(), "before any session is told");
        }
    }

    #[test]
    fn a_carry_of_a_quest_that_may_not_be_shared_is_refused_before_any_session_is_told() {
        let why = "HARBOR is shared from kestrel, so the copy here moves aside as that one's comes";
        let mut realm = Realm {
            unshareable: Some(why.to_owned()),
            ..realm()
        };
        let (done, lines) = carried(&mut realm, "tern");
        assert_eq!(
            done,
            Err(Failed { step: Step::Find }),
            "refused at the find"
        );
        assert_eq!(last_said(&lines), why, "saying why");
        assert!(
            realm.steered.is_empty() && realm.shared.is_empty() && realm.started.is_empty(),
            "no session told, and nothing shared or started"
        );
        assert!(realm.rest.is_none(), "and nothing carried");
    }

    // A2VLJN (2026-11-21): a start refused only once every session had
    // ended here left them running nowhere; the carry asks first.
    #[test]
    fn a_carry_whose_starts_would_be_refused_there_tells_no_session() {
        let why = "this asks as a fenced agent, so it starts nothing held on a Mac: tern is a Mac";
        let mut realm = Realm {
            untaken: Some(Untaken::Refused(why.to_owned())),
            ..realm()
        };
        let (done, lines) = carried(&mut realm, "tern");
        assert_eq!(
            done,
            Err(Failed { step: Step::Find }),
            "refused at the find"
        );
        assert_eq!(
            last_said(&lines),
            format!("{QUEST}'s sessions can't start on tern now: {why}"),
            "saying why"
        );
        assert!(
            realm.steered.is_empty() && realm.shared.is_empty() && realm.started.is_empty(),
            "no session told, and nothing shared or started"
        );
        assert!(realm.rest.is_none(), "and nothing carried");
    }

    #[test]
    fn a_carry_this_node_cant_judge_tells_no_session_and_blames_no_machine() {
        let why = "it runs a mycelium too old to know the ask, so update it and carry again";
        let mut realm = Realm {
            untaken: Some(Untaken::Unasked(why.to_owned())),
            ..realm()
        };
        let (done, lines) = carried(&mut realm, "tern");
        assert_eq!(
            done,
            Err(Failed { step: Step::Find }),
            "refused at the find"
        );
        assert_eq!(
            last_said(&lines),
            format!(
                "{QUEST} isn't carried, since this machine's node couldn't say whether tern \
                 would start its sessions: {why}"
            ),
            "saying it was this machine's node that couldn't say"
        );
        assert!(
            realm.steered.is_empty() && realm.shared.is_empty() && realm.started.is_empty(),
            "no session told, and nothing shared or started"
        );
    }

    // A session that had ended before the carry is found only as
    // `pi-carry` carries what no running session held, after the ask: its
    // start isn't among those asked of, so it starts after them all.
    fn ended_before(start: &Start) -> bool {
        start
            .command
            .get(6)
            .is_some_and(|session| session == "s-gone")
    }

    #[test]
    fn a_carry_asks_whether_its_starts_are_taken_as_they_would_be_asked() {
        let mut realm = realm();
        let (done, _) = carried(&mut realm, "tern");
        assert!(done.is_ok(), "it carries: {done:?}");
        let [asked] = realm.asked_takes.as_slice() else {
            panic!("asked once: {:?}", realm.asked_takes);
        };
        assert!(!asked.ordered, "for a carry nobody ordered");
        assert!(
            realm.started.iter().any(ended_before),
            "a session that had ended started too"
        );
        let first_unasked = realm
            .started
            .iter()
            .position(ended_before)
            .expect("one unasked");
        assert!(
            realm.started[first_unasked..].iter().all(ended_before),
            "what wasn't asked of starts after all that was"
        );
        for start in &realm.started {
            assert!(unordered_arrival(&start.command).is_some(), "as an arrival");
            if ended_before(start) {
                continue;
            }
            assert!(
                asked.starts.iter().any(|one| one.command == start.command),
                "each running session's and the quest's own asked first, word for word: \
                 {start:?} in {:?}",
                asked.starts
            );
        }
        assert_eq!(
            asked.starts.len(),
            realm
                .started
                .iter()
                .filter(|one| !ended_before(one))
                .count(),
            "nothing asked that doesn't start"
        );
        let mut ordering = Realm {
            by: Some("WCJQSG".to_owned()),
            ..super::tests::realm()
        };
        ordered(&mut ordering).0.expect("carried");
        let [asked] = ordering.asked_takes.as_slice() else {
            panic!("asked once: {:?}", ordering.asked_takes);
        };
        assert!(asked.ordered, "an ordered carry says so");
        for start in ordering.started.iter().filter(|one| !ended_before(one)) {
            let unordered = &start.command[..8];
            assert!(
                asked
                    .starts
                    .iter()
                    .any(|one| one.command[3..] == unordered[3..] && one.work == start.work),
                "each asked as its arrival were nobody to order it: {start:?}"
            );
        }
    }

    #[test]
    fn a_carry_the_node_proves_two_orchestrators_ordered_is_refused() {
        let mut realm = Realm {
            by: Some("WCJQSG".to_owned()),
            by_each: [("NAPS02".to_owned(), "OSPHT4".to_owned())].into(),
            ..realm()
        };
        let (done, lines) = ordered(&mut realm);
        assert_eq!(
            done,
            Err(Failed { step: Step::Push }),
            "it fails at the push"
        );
        let said = last_said(&lines);
        assert!(
            said.contains("WCJQSG") && said.contains("OSPHT4"),
            "naming both rather than stamping the last: {said}"
        );
        assert!(realm.started.is_empty(), "and nothing arrives as either's");
    }

    #[test]
    fn an_ordered_carry_the_node_carries_on_is_handed_to_it() {
        let mut realm = Realm {
            by: Some("WCJQSG".to_owned()),
            carrying_on: ["BUSY01", "NAPS02"].map(str::to_owned).into(),
            ..realm()
        };
        let (done, lines) = ordered(&mut realm);
        let carried = done.expect("handed");
        assert_eq!(carried.step, "handed", "its last line says so: {lines:?}");
        assert_eq!(carried.sessions, [], "none has arrived yet");
        assert!(
            realm.started.is_empty() && realm.shared.is_empty(),
            "nothing is shared or started from the command, whose end can't leave the \
             quest half carried: {lines:?}"
        );
        assert!(
            last_said(&lines).contains("node carries the rest"),
            "and the push says who carries on: {lines:?}"
        );
    }

    #[test]
    fn an_ordered_carry_of_several_is_handed_whole_to_a_node_carrying_them_with_the_last() {
        let mut realm = Realm {
            by: Some("WCJQSG".to_owned()),
            carrying_with: true,
            ..realm()
        };
        let (done, lines) = ordered(&mut realm);
        let carried = done.expect("handed");
        assert_eq!(carried.step, "handed", "its last line says so: {lines:?}");
        let with: Vec<Option<&Value>> = realm
            .steered
            .iter()
            .map(|(_, steer)| steer.get("with"))
            .collect();
        assert_eq!(
            with,
            [None, Some(&json!(["BUSY01"]))],
            "only the last told names those told before it"
        );
        assert!(
            realm.started.is_empty() && realm.shared.is_empty(),
            "nothing is shared or started from the command, whose end can't leave the \
             quest half carried: {lines:?}"
        );
    }

    #[test]
    fn an_ordered_carry_of_several_a_node_carries_none_of_is_carried_from_the_command() {
        let mut realm = Realm {
            by: Some("WCJQSG".to_owned()),
            ..realm()
        };
        let (done, lines) = ordered(&mut realm);
        let carried = done.expect("carried");
        assert_eq!(
            carried.step, "carried",
            "a node too old to read `with` leaves every one to the command: {lines:?}"
        );
        assert_eq!(
            arrivals(&realm.started),
            ["s-busy01", "s-naps02", QUEST, "s-gone"],
            "the sessionless one beside them too: {lines:?}"
        );
    }

    #[test]
    fn an_ordered_carry_the_node_carries_on_in_part_carries_the_rest_itself() {
        let mut realm = Realm {
            by: Some("WCJQSG".to_owned()),
            carrying_on: ["BUSY01".to_owned()].into(),
            ..realm()
        };
        let (done, lines) = ordered(&mut realm);
        let carried = done.expect("carried");
        assert_eq!(carried.step, "carried", "{lines:?}");
        assert_eq!(
            arrivals(&realm.started),
            ["s-naps02"],
            "only the session the node doesn't carry on is started from here, the one \
             it carries on never twice, and the quest's trees left to it: {lines:?}"
        );
    }

    #[test]
    fn a_carry_waiting_names_each_session_and_its_done_file_once() {
        let mut realm = Realm {
            by: Some("WCJQSG".to_owned()),
            // NAPS02 is told and never carries.
            carries: ["s-busy01".to_owned()].into(),
            ..realm()
        };
        let (done, lines) = ordered(&mut realm);
        assert_eq!(
            done,
            Err(Failed { step: Step::Push }),
            "it waits out its patience"
        );
        let waiting: Vec<&str> = lines
            .iter()
            .filter_map(|line| line["said"].as_str())
            .filter(|said| said.starts_with("waiting for"))
            .collect();
        assert_eq!(
            waiting,
            [format!(
                "waiting for NAPS02 (handover/skiff/s-naps02.done.json) to carry into \
                 {QUEST}'s handover"
            )],
            "said once, over every look it waits: {lines:?}"
        );
    }

    #[test]
    fn a_carry_a_session_refuses_stops_at_once_with_its_reason() {
        let mut realm = Realm {
            by: Some("WCJQSG".to_owned()),
            // BUSY01 is told and still carrying, with no done file yet.
            carries: BTreeSet::new(),
            refusing: vec![Refused {
                session: "s-naps02".to_owned(),
                why: "it holds a lease on src/node".to_owned(),
            }],
            ..realm()
        };
        let (done, lines) = ordered(&mut realm);
        assert_eq!(
            done,
            Err(Failed { step: Step::Push }),
            "it fails at the push"
        );
        assert_eq!(
            last_said(&lines),
            "NAPS02 refused its carry: it holds a lease on src/node",
            "naming the session and its reason"
        );
        assert_eq!(realm.patience, 3, "without waiting out its patience");
        assert!(
            realm.rest.is_none() && realm.shared.is_empty() && realm.started.is_empty(),
            "and nothing else is carried, shared or started"
        );
        assert!(
            !realm.cells.contains("s-naps02"),
            "the refusing session's cell is taken away: {:?}",
            realm.cells
        );
        assert!(
            realm.cells.contains("s-busy01"),
            "while the cell of one told and still carrying stays: {:?}",
            realm.cells
        );
    }

    #[test]
    fn every_session_that_refused_loses_its_cell_not_only_the_one_named() {
        let refusing = |session: &str| Refused {
            session: session.to_owned(),
            why: "it holds a lease".to_owned(),
        };
        let mut realm = Realm {
            by: Some("WCJQSG".to_owned()),
            carries: BTreeSet::new(),
            refusing: vec![refusing("s-busy01"), refusing("s-naps02")],
            ..realm()
        };
        let (done, _) = ordered(&mut realm);
        assert_eq!(done, Err(Failed { step: Step::Push }));
        assert!(
            realm.cells.is_empty(),
            "both refusers' cells are taken away: {:?}",
            realm.cells
        );
    }

    #[test]
    fn an_ordered_carry_with_no_session_here_is_refused() {
        let mut realm = Realm {
            by: Some("WCJQSG".to_owned()),
            ..realm()
        };
        realm
            .sessions
            .retain(|one| one.machine != "skiff" || one.address == "OTHER4");
        let (done, lines) = ordered(&mut realm);
        assert_eq!(
            done,
            Err(Failed { step: Step::Find }),
            "refused as it finds"
        );
        assert_eq!(
            last_said(&lines),
            "no session runs QEST-20261107-HARBOR here, so the node can't prove who \
             orchestrates it: only the person carries what it left",
            "since only a session's steer proves who may order it"
        );
        assert!(
            realm.rest.is_none() && realm.shared.is_empty(),
            "nothing moved"
        );
    }

    #[test]
    fn an_ordered_carry_passes_over_a_parked_session_no_quest_holds_and_names_it() {
        let mut mixed = Realm {
            by: Some("WCJQSG".to_owned()),
            unheld: ["s-busy01", "s-naps02"].map(str::to_owned).into(),
            ..realm()
        };
        let (done, lines) = ordered(&mut mixed);
        let carried = done.expect("carried");
        let steered: Vec<&str> = mixed.steered.iter().map(|(at, _)| at.as_str()).collect();
        assert_eq!(
            steered,
            ["BUSY01"],
            "the parked one isn't told, and one running is, so a refusal of it is said: \
             {lines:?}"
        );
        assert_eq!(carried.skipped, ["NAPS02"], "the carry's output names it");
        let found = lines
            .iter()
            .find(|line| line["step"] == "find" && line["state"] == "done")
            .and_then(|line| line["said"].as_str())
            .unwrap_or_default();
        assert_eq!(
            found,
            "BUSY01 for QEST-20261107-HARBOR, going to tern; NAPS02 parked where \
             QEST-20261107-HARBOR's record alone doesn't hold it, so who orchestrates it can't \
             be told, and it stays here",
            "and so does its find"
        );

        let mut alone = Realm {
            by: Some("WCJQSG".to_owned()),
            unheld: ["s-naps02"].map(str::to_owned).into(),
            ..realm()
        };
        alone
            .sessions
            .retain(|one| one.machine != "skiff" || one.address == "NAPS02");
        let (done, lines) = ordered(&mut alone);
        assert_eq!(
            done,
            Err(Failed { step: Step::Find }),
            "none proved is refused"
        );
        assert_eq!(
            last_said(&lines),
            "no session runs QEST-20261107-HARBOR here, so the node can't prove who \
             orchestrates it: only the person carries what it left (NAPS02 parked where \
             QEST-20261107-HARBOR's record alone doesn't hold it, so who orchestrates it \
             can't be told, and it stays here)",
            "naming the one passed over"
        );
        assert!(alone.steered.is_empty(), "and nobody told");
    }

    #[test]
    fn an_ordered_carry_passes_over_a_parked_session_another_quests_record_holds() {
        let other = |sessions: &[&str]| {
            json!({"id": "QEST-20261107-OTHERS", "orchestrator": "WCJQSG",
                   "sessions": sessions})
        };
        // Held by its quest's record and another's, the node's steer can't
        // tell which orchestrates it; held by another's alone, the steer
        // would prove that quest's orchestrator, not this one's.
        for (held, by) in [
            (BTreeSet::new(), "two records"),
            (
                ["s-naps02".to_owned()].into(),
                "another quest's record alone",
            ),
        ] {
            let mut realm = Realm {
                by: Some("WCJQSG".to_owned()),
                unheld: held,
                other_records: vec![other(&["s-naps02"])],
                ..realm()
            };
            let (done, lines) = ordered(&mut realm);
            let carried = done.expect("carried");
            let steered: Vec<&str> = realm.steered.iter().map(|(at, _)| at.as_str()).collect();
            let skipped: Vec<&str> = carried.skipped.iter().map(String::as_str).collect();
            assert_eq!(
                (steered, skipped),
                (vec!["BUSY01"], vec!["NAPS02"]),
                "a parked session held by {by} is passed over: {lines:?}"
            );
        }
    }

    #[test]
    fn a_carry_by_the_person_never_asks_which_quest_holds_a_session() {
        let mut person = Realm {
            unheld: ["s-naps02"].map(str::to_owned).into(),
            unsaid_quests: Some("the node serves no quests' record".to_owned()),
            ..realm()
        };
        let (done, lines) = carried(&mut person, "tern");
        let carried = done.expect("carried");
        let steered: Vec<&str> = person.steered.iter().map(|(at, _)| at.as_str()).collect();
        assert_eq!(steered, ["BUSY01", "NAPS02"], "each told: {lines:?}");
        assert!(carried.skipped.is_empty(), "none passed over");

        let mut unsaid = Realm {
            by: Some("WCJQSG".to_owned()),
            unsaid_quests: Some("the node serves no quests' record".to_owned()),
            ..realm()
        };
        let (done, lines) = ordered(&mut unsaid);
        assert_eq!(
            (done, last_said(&lines).as_str()),
            (
                Err(Failed { step: Step::Find }),
                "which quest's record holds each of QEST-20261107-HARBOR's sessions can't be \
                 told: the node serves no quests' record"
            ),
            "an ordered carry that can't tell is refused before telling any"
        );
    }

    /// The session, or quest, each start arrives with.
    fn arrivals(started: &[Start]) -> Vec<&str> {
        started
            .iter()
            .filter_map(|one| one.command.get(6).map(String::as_str))
            .collect()
    }

    #[test]
    fn a_later_carry_starts_only_what_was_carried_since() {
        let mut realm = realm();
        carried(&mut realm, "tern").0.expect("the first carry");
        realm.started.clear();
        realm.sessions = vec![session("FRESH5", "skiff", QUEST, "busy", &json!({}))];
        realm.carries.insert("s-fresh5".to_owned());
        let (done, lines) = carried(&mut realm, "tern");
        done.expect("the later carry");
        assert_eq!(
            arrivals(&realm.started),
            ["s-fresh5"],
            "what the first carry started there starts no more: {lines:?}"
        );
    }

    #[test]
    fn a_carry_taken_again_after_a_start_went_unanswered_joins_the_start_that_landed() {
        let mut realm = Realm {
            unanswered: Some("s-naps02".to_owned()),
            ..realm()
        };
        let (done, _) = carried(&mut realm, "tern");
        assert_eq!(
            done,
            Err(Failed { step: Step::Start }),
            "it fails at the start, which landed all the same"
        );
        let (done, lines) = carried(&mut realm, "tern");
        done.expect("taken again, it goes through");
        assert_eq!(
            arrivals(&realm.started),
            ["s-busy01", "s-naps02", QUEST, "s-gone"],
            "the one that landed unanswered starts once, joined when asked again: {lines:?}"
        );
    }

    #[test]
    fn a_session_carried_again_after_it_started_asks_its_start_afresh() {
        let mut realm = realm();
        carried(&mut realm, "tern").0.expect("the first carry");
        let first: Vec<Start> = std::mem::take(&mut realm.started);
        assert!(
            first.iter().all(|one| one.once.is_some()),
            "each start is asked under a name: {first:?}"
        );
        // Back on skiff, and carried to tern again.
        realm.sessions = vec![session("BUSY01", "skiff", QUEST, "busy", &json!({}))];
        carried(&mut realm, "tern").0.expect("the second carry");
        assert_eq!(arrivals(&realm.started), ["s-busy01"], "it starts again");
        assert!(
            first.iter().all(|one| one.once != realm.started[0].once),
            "under a name the first carry's starts didn't have: {:?}",
            realm.started[0].once
        );
    }

    #[test]
    fn a_carry_taken_again_after_a_start_failed_starts_only_what_had_not() {
        let mut realm = Realm {
            refuses: Some("s-naps02".to_owned()),
            ..realm()
        };
        let (done, lines) = carried(&mut realm, "tern");
        assert_eq!(
            done,
            Err(Failed { step: Step::Start }),
            "it fails at the start"
        );
        let said = last_said(&lines);
        assert!(said.contains("NAPS02"), "naming the one refused: {said}");
        assert!(
            said.contains("BUSY01, s-gone started there"),
            "and those that did: {said}"
        );
        assert_eq!(
            arrivals(&realm.started),
            ["s-busy01", QUEST, "s-gone"],
            "every other was asked all the same, since each told has ended here"
        );
        realm.refuses = None;
        let (done, lines) = carried(&mut realm, "tern");
        done.expect("taken again, it goes through");
        assert_eq!(
            arrivals(&realm.started),
            ["s-busy01", QUEST, "s-gone", "s-naps02"],
            "each carried session starts once, those not yet started now: {lines:?}"
        );
    }

    #[test]
    fn a_start_that_fails_first_leaves_the_rest_asked() {
        let mut realm = Realm {
            refuses: Some("s-busy01".to_owned()),
            ..realm()
        };
        let (done, lines) = carried(&mut realm, "tern");
        assert_eq!(done, Err(Failed { step: Step::Start }));
        assert_eq!(
            arrivals(&realm.started),
            ["s-naps02", QUEST, "s-gone"],
            "the others start though the first was refused: {lines:?}"
        );
        let said = last_said(&lines);
        assert!(
            said.contains("BUSY01 didn't start on tern") && said.contains("stays in the handover"),
            "naming the one refused and where its work is: {said}"
        );
        assert!(
            !steps(&lines).contains(&"close doing".to_owned()),
            "and nothing after the start runs: {lines:?}"
        );
    }

    // 8855BD77's council (23 November): a start whose mark failed was
    // said not to have started, sending the person to start it again.
    #[test]
    fn a_start_whose_mark_fails_is_said_to_have_started() {
        let mut realm = Realm {
            unmarkable: Some("s-busy01".to_owned()),
            ..realm()
        };
        let (done, lines) = carried(&mut realm, "tern");
        assert_eq!(done, Err(Failed { step: Step::Start }));
        let said = last_said(&lines);
        assert!(
            said.contains("BUSY01 started on tern, but its done file here couldn't be marked")
                && said.contains("move it out of the handover before carrying again"),
            "naming what to do about its mark: {said}"
        );
        assert!(
            said.contains("BUSY01, NAPS02, s-gone started there"),
            "and counting it among those that started: {said}"
        );
        assert!(
            !said.contains("for the person's carry to start"),
            "never sending the person to start it again: {said}"
        );
    }
}
