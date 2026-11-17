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
//! starts it again, and waits until the sessions here have ended. A
//! step that fails says why and nothing after it runs; what was
//! done stays done, since each step can be taken again.
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
pub mod through;

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::placement::{Need, Work};
use crate::record::{Session, State};
use crate::wire::{Start, Started};

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

/// What a carry does to the world.
pub trait Hands {
    /// The other machines of the realm this one reaches, by name.
    ///
    /// # Errors
    ///
    /// When the node can't say.
    fn machines(&mut self) -> Result<Vec<String>, String>;
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
    /// answers who the node proved ordered it, for an `ordered-carry`.
    ///
    /// # Errors
    ///
    /// When the node refuses it.
    fn steer(&mut self, address: &str, steer: Value) -> Result<Option<String>, String>;
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
    /// Shares the quest, with its handover.
    ///
    /// # Errors
    ///
    /// When it can't be shared.
    fn share(&mut self, quest: &str) -> Result<(), String>;
    /// Starts a program held where `start` says.
    ///
    /// # Errors
    ///
    /// When the node can't start it.
    fn start(&mut self, start: Start) -> Result<Started, String>;
    /// Waits a moment before what is waited for is looked at again, and
    /// says whether there is patience left for another look.
    fn wait(&mut self) -> bool;
}

/// Carries `quest` from `here` to the machine `to`, saying each step to
/// `say` as it goes, and its last line once it has gone through, or
/// `Err` once a step has failed and said why. Given `why`, an
/// orchestrator orders it: each session is told with `ordered-carry`,
/// which the node takes only from the session's orchestrator, and each
/// arrives as carried by it for `why`.
///
/// # Errors
///
/// [`Failed`] when a step fails, its line having said why.
pub fn carry(
    hands: &mut impl Hands,
    quest: &str,
    here: &str,
    to: &str,
    why: Option<&str>,
    say: &mut impl FnMut(&Value),
) -> Result<Carried, Failed> {
    let mut said = Saying { say };
    said.line(
        Step::Find,
        Stood::Doing,
        format!("finding {quest}'s sessions here"),
    );
    let sessions = said.or_failed(Step::Find, find(hands, quest, here, to, why))?;
    let names: Vec<&str> = sessions.iter().map(|one| one.address.as_str()).collect();
    said.line(
        Step::Find,
        Stood::Done,
        if names.is_empty() {
            format!("no session runs {quest} here; what it left goes as it is")
        } else {
            format!("{} for {quest}, going to {to}", names.join(", "))
        },
    );

    said.line(
        Step::Push,
        Stood::Doing,
        "carrying each session's work".to_owned(),
    );
    said.or_failed(Step::Push, bring_cells(hands, &sessions))?;
    let (done, ordered) = said.or_failed(Step::Push, push(hands, quest, to, why, &sessions))?;
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
    said.or_failed(Step::Share, hands.share(quest))?;
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
        start_each(hands, quest, here, to, &done, &sessions, ordered.as_ref()),
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
    said.or_failed(Step::Close, closed(hands, here, &sessions))?;
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
    Ok(Carried {
        step: "carried",
        state: Stood::Done,
        machine: to.to_owned(),
        sessions: arrived,
    })
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
/// arrives as `ordered` says, where an orchestrator ordered it.
fn start_each(
    hands: &mut impl Hands,
    quest: &str,
    here: &str,
    to: &str,
    done: &[Done],
    sessions: &[Session],
    ordered: Option<&Ordered>,
) -> Result<Vec<Arrived>, String> {
    done.iter()
        .map(|one| {
            let address = sessions
                .iter()
                .find(|session| session_id(session) == Some(one.session.as_str()))
                .map_or_else(|| one.session.clone(), |session| session.address.clone());
            let started = hands
                .start(arriving(quest, here, to, one, ordered))
                .map_err(|why| format!("{address} didn't start on {to}: {why}"))?;
            hands.started(one).map_err(|why| {
                format!("{address} started on {to}, but a later carry would start it again: {why}")
            })?;
            Ok((one.session != quest).then_some(Arrived {
                address,
                held: started.held,
                attach: started.attach,
            }))
        })
        .filter_map(Result::transpose)
        .collect()
}

/// The quest's sessions on `here` that can carry their own work: those
/// running, and those parked this boot, which wake for it; or why the
/// carry can't go to `to`. A carry ordered for `why` needs one at least,
/// since only the node's taking a session's steer proves who may order
/// it, and a reason that says something.
fn find(
    hands: &mut impl Hands,
    quest: &str,
    here: &str,
    to: &str,
    why: Option<&str>,
) -> Result<Vec<Session>, String> {
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
    let boot = hands.boot();
    let sessions: Vec<Session> = hands
        .sessions()?
        .into_iter()
        .filter(|session| session.machine == here && session.quest.as_deref() == Some(quest))
        .filter(|session| runs(session) || parked(session, boot.as_deref()))
        .collect();
    if ordered && sessions.is_empty() {
        return Err(format!(
            "no session runs {quest} here, so the node can't prove who orchestrates it: \
             only the person carries what it left"
        ));
    }
    Ok(sessions)
}

/// Tells each session to carry its work to `to`, as the person or, given
/// `why`, as its orchestrator, waits until each has, then carries what
/// none of them held, and answers everything carried, with who ordered
/// it as the node proved. A session that refuses stops it at once.
fn push(
    hands: &mut impl Hands,
    quest: &str,
    to: &str,
    why: Option<&str>,
    sessions: &[Session],
) -> Result<(Vec<Done>, Option<Ordered>), String> {
    let steer = why.map_or_else(
        || json!({"act": "carry", "to": to, "from": "person"}),
        |why| json!({"act": ordered::ACT, "to": to, "why": why}),
    );
    let mut by: Option<String> = None;
    for session in sessions {
        let proved = hands
            .steer(&session.address, steer.clone())
            .map_err(|why| format!("{} wasn't told: {why}", session.address))?;
        if why.is_none() {
            continue;
        }
        let proved = proved.ok_or_else(|| {
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
    let waiting: BTreeSet<&str> = sessions.iter().filter_map(session_id).collect();
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
        let left: Vec<&str> = waiting.difference(&carried).copied().collect();
        if left.is_empty() {
            break;
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
    hands.carry_rest(quest, to)?;
    let done = hands.done()?;
    if done.is_empty() {
        return Err(format!(
            "nothing of {quest} was here to carry that hadn't started already"
        ));
    }
    Ok((done, ordered))
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
    for (id, session) in &carried {
        hands
            .bring(&cell::brought(id, &session.address, &ties))
            .map_err(|why| {
                format!(
                    "{}'s ties weren't put beside its work: {why}",
                    session.address
                )
            })?;
    }
    Ok(keeping)
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
fn arriving(quest: &str, from: &str, to: &str, one: &Done, ordered: Option<&Ordered>) -> Start {
    let arrive = if ordered.is_some() {
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
    // session. One whose fence can't be told, as from a harness that
    // can't say it, doesn't start, rather than start looser; one whose
    // done file names none, from an older harness, starts as before.
    let script = if one.session == quest {
        arrive.to_owned()
    } else {
        [
            arrive,
            r#" || exit; folder=$(pi-carry where "$1" --from "$2" --session "$3") || exit; "#,
            r#"fence=$(pi-carry fence "$1" --from "$2" --session "$3") || exit; "#,
            r#"if [ -n "$fence" ]; then export PI_SANDBOX="$fence"; fi; "#,
            r#"cd "$folder" && QUEST_WORKFLOW_AUTOLOAD_ID="$1" exec pi --session "$3" -- "$4""#,
        ]
        .concat()
    };
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
        },
        folder: None,
        command,
        may_cost: false,
    }
}

/// What a session carried from `from` is told as it starts, in the
/// words the harness opens its own turns with, so it reads as one.
fn carry_on(from: &str) -> String {
    format!(
        "[Automated message, no human input: {PI_CARRY} carried this session here from \
         {from}.] Carry on from where you left off: your plan's next open item."
    )
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

    use serde_json::{Value, json};

    use super::cell::Brought;
    use super::{Carried, Done, Failed, Hands, Ordered, PI_CARRY, Refused, Step, carry, runs_on};
    use crate::placement::Need;
    use crate::record::{Session, State};
    use crate::wire::{Start, Started};

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
        steered: Vec<(String, Value)>,
        rest: Option<String>,
        /// The done files in the handover, each with whether it was
        /// marked started.
        handover: Vec<(Done, bool)>,
        /// The session, by id, whose start there fails.
        refuses: Option<String>,
        /// Who the node proves ordered an `ordered-carry`.
        by: Option<String>,
        /// Who it proves ordered one session's instead, by its address.
        by_each: BTreeMap<String, String>,
        /// The sessions that refuse their carry once told, and why.
        refusing: Vec<Refused>,
        /// What each session told refused.
        refused: Vec<Refused>,
        shared: Vec<String>,
        started: Vec<Start>,
        /// Every tie the node holds.
        ties: Vec<Value>,
        /// Why the node can't say its ties, where it can't.
        unsaid_ties: Option<String>,
        /// How many times the ties were asked for.
        ties_asked: usize,
        /// Why what a cell brings can't be put beside its work, where not.
        unbrought: Option<String>,
        /// What each cell brought, with how many sessions were told
        /// before.
        brought: Vec<(Brought, usize)>,
        /// The sessions, by id, with a cell in the handover.
        cells: BTreeSet<String>,
        /// Why an earlier carry's cells can't be taken away, where not.
        unforgotten: Option<String>,
        /// The sessions whose old cells were taken away, with how many
        /// sessions were told before.
        forgotten: Vec<(String, usize)>,
    }

    impl Realm {
        /// Writes `session`'s done file, afresh where one was there.
        fn write(&mut self, session: &str) {
            let one = Done {
                session: session.to_owned(),
            };
            match self.handover.iter_mut().find(|(at, _)| at == &one) {
                Some(there) => there.1 = false,
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
            if self.ends && !self.started.is_empty() {
                for one in &mut sessions {
                    one.state = State::Ended;
                }
            }
            Ok(sessions)
        }
        fn boot(&self) -> Option<String> {
            self.boot.clone()
        }
        fn steer(&mut self, address: &str, steer: Value) -> Result<Option<String>, String> {
            let by = (steer["act"] == "ordered-carry")
                .then(|| self.by_each.get(address).or(self.by.as_ref()).cloned())
                .flatten();
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
            Ok(by)
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
            let there = self.handover.iter_mut().find(|(at, _)| at == one);
            there
                .map(|there| there.1 = true)
                .ok_or_else(|| format!("{} has no done file to mark", one.session))
        }
        fn ties(&mut self) -> Result<Vec<Value>, String> {
            self.ties_asked += 1;
            self.unsaid_ties
                .clone()
                .map_or_else(|| Ok(self.ties.clone()), Err)
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
                self.write("s-gone");
                // The trees no session held, as `pi-carry` names them.
                self.write(QUEST);
            }
            self.rest = Some(format!("{quest} {to}"));
            Ok(())
        }
        fn share(&mut self, quest: &str) -> Result<(), String> {
            self.shared.push(quest.to_owned());
            Ok(())
        }
        fn start(&mut self, start: Start) -> Result<Started, String> {
            if self.refuses.as_deref() == start.command.get(6).map(String::as_str) {
                return Err("the machine there refused it".to_owned());
            }
            let held = format!("h{}", self.started.len());
            self.started.push(start);
            Ok(Started {
                machine: "tern".to_owned(),
                held: held.clone(),
                folder: "/home/joel".into(),
                attach: vec!["mycelium".to_owned(), "attach".to_owned(), held],
            })
        }
        fn wait(&mut self) -> bool {
            let more = self.patience > 0;
            self.patience = self.patience.saturating_sub(1);
            more
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
            ..Realm::default()
        }
    }

    /// Carries HARBOR from skiff to `to` in `realm`, with each line said.
    fn carried(realm: &mut Realm, to: &str) -> (Result<Carried, Failed>, Vec<Value>) {
        let mut lines = Vec::new();
        let done = carry(realm, QUEST, "skiff", to, None, &mut |line: &Value| {
            lines.push(line.clone());
        });
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
            Some("it needs the camera"),
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
        let mut realm = realm();
        realm.ties = vec![
            json!({"key": "tie:BUSY01~KESTRL", "kind": "tie", "strength": 2.5}),
            json!({"key": "tie:AAAAAA~NAPS02", "kind": "tie", "strength": 1.0}),
            json!({"key": "tie:BUSY01", "kind": "tie", "ties": 1}),
            json!({"key": "tie:OTHER4~ZZZZZZ", "kind": "tie"}),
        ];
        let (done, lines) = carried(&mut realm, "tern");
        assert!(done.is_ok(), "carried: {}", last_said(&lines));
        let cell = |session: &str, cell: &str, ties: &[usize], talk: Option<usize>| Brought {
            session: session.to_owned(),
            cell: cell.to_owned(),
            ties: ties.iter().map(|at| realm.ties[*at].clone()).collect(),
            talk: talk.map(|at| realm.ties[at].clone()),
        };
        assert_eq!(
            realm.brought,
            [
                (cell("s-busy01", "BUSY01", &[0], Some(2)), 0),
                (cell("s-naps02", "NAPS02", &[1], None), 0),
            ],
            "each session to be carried brings its own ties before any session is told"
        );
        assert_eq!(realm.ties_asked, 1, "the ties are asked for once");
    }

    #[test]
    fn an_earlier_carrys_cells_go_before_any_session_is_told_and_a_failed_ones_stay() {
        let done = |session: &str| Done {
            session: session.to_owned(),
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
            (realm.ties_asked, realm.brought.len()),
            (0, 0),
            "a quest with no session here asks nothing and brings nothing"
        );
        let mut unnamed = session("BUSY01", "skiff", QUEST, "busy", &json!({}));
        if let Some(presence) = std::sync::Arc::make_mut(&mut unnamed.presence).as_object_mut() {
            presence.remove("session");
        }
        assert_eq!(
            super::bring_cells(&mut realm, &[unnamed]),
            Ok(BTreeSet::new())
        );
        assert_eq!(
            (realm.ties_asked, realm.brought.len()),
            (0, 0),
            "nor one whose presence names no session to carry"
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
                    unbrought: Some("the disk is full".to_owned()),
                    ..realm()
                },
                "BUSY01's ties weren't put beside its work: the disk is full",
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
                "close done"
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
        let trees = &realm.started[3];
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
        arrive_fenced(home, where_, arrives, Some(""))
    }

    /// The same, with `pi-carry fence` printing `fence`, or failing where
    /// it's none, and `pi` writing the `PI_SANDBOX` it started under, or
    /// `unset`, to `sandbox`.
    fn arrive_fenced(
        home: &std::path::Path,
        where_: Option<&str>,
        arrives: bool,
        fence: Option<&str>,
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
        };
        let start = super::arriving(QUEST, "skiff", "tern", &one, None);
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        std::process::Command::new(&start.command[0])
            .args(&start.command[1..])
            .env("HOME", home)
            .env("PATH", path)
            .env_remove("PI_SANDBOX")
            .status()
            .expect("the shell runs");
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
            arrive_fenced(&home, Some(&at), true, Some(kind)).expect("pi started");
            assert_eq!(sandbox(), kind, "pi starts as the done file says it ran");
        }
        arrive_fenced(&home, Some(&at), true, Some("")).expect("pi started");
        assert_eq!(
            sandbox(),
            "unset",
            "a done file naming no fence leaves pi as the node starts it"
        );
        std::fs::remove_file(home.join("ran")).expect("forgotten");
        assert_eq!(
            arrive_fenced(&home, Some(&at), true, None),
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
                Some(why),
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
        assert_eq!(
            arrivals(&realm.started),
            ["s-busy01"],
            "the one before it started"
        );
        realm.refuses = None;
        let (done, lines) = carried(&mut realm, "tern");
        done.expect("taken again, it goes through");
        assert_eq!(
            arrivals(&realm.started),
            ["s-busy01", "s-naps02", "s-gone", QUEST],
            "each carried session starts once, those not yet started now: {lines:?}"
        );
    }
}
