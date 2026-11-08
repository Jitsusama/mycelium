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
//! arrive` has its trees there, and waits until the sessions here have
//! ended. A step that fails says why and nothing after it runs; what was
//! done stays done, since each step can be taken again.
//!
//! What it does to the world goes through [`Hands`], whose one
//! implementation is the command's, so the order is proved over a fake.

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

/// What a session, or `pi-carry`, says it carried: the session and the
/// folder it ran in, within its home, since the other machine's home is
/// elsewhere. The harness says more, which the other machine reads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Done {
    /// The session's id.
    pub session: String,
    /// The folder it ran in, such as `~/src/mycelium`.
    pub cwd: String,
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
    /// Puts `steer` in the inbox of the session going by `address`.
    ///
    /// # Errors
    ///
    /// When the node refuses it.
    fn steer(&mut self, address: &str, steer: Value) -> Result<(), String>;
    /// What has been carried into the quest's handover from here so far.
    ///
    /// # Errors
    ///
    /// When a done file can't be read.
    fn done(&mut self) -> Result<Vec<Done>, String>;
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
/// `Err` once a step has failed and said why.
///
/// # Errors
///
/// [`Failed`] when a step fails, its line having said why.
pub fn carry(
    hands: &mut impl Hands,
    quest: &str,
    here: &str,
    to: &str,
    say: &mut impl FnMut(&Value),
) -> Result<Carried, Failed> {
    let mut said = Saying { say };
    said.line(
        Step::Find,
        Stood::Doing,
        format!("finding {quest}'s sessions here"),
    );
    let sessions = said.or_failed(Step::Find, find(hands, quest, here, to))?;
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
    let done = said.or_failed(Step::Push, push(hands, quest, to, &sessions))?;
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
        start_each(hands, quest, here, to, &done, &sessions),
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
/// by here, or its id where no session here named it.
fn start_each(
    hands: &mut impl Hands,
    quest: &str,
    here: &str,
    to: &str,
    done: &[Done],
    sessions: &[Session],
) -> Result<Vec<Arrived>, String> {
    done.iter()
        .map(|one| {
            let address = sessions
                .iter()
                .find(|session| session_id(session) == Some(one.session.as_str()))
                .map_or_else(|| one.session.clone(), |session| session.address.clone());
            let started = hands
                .start(arriving(quest, here, to, one))
                .map_err(|why| format!("{address} didn't start on {to}: {why}"))?;
            Ok(Arrived {
                address,
                held: started.held,
                attach: started.attach,
            })
        })
        .collect()
}

/// The quest's sessions on `here` that can carry their own work: those
/// running, and those parked this boot, which wake for it; or why the
/// carry can't go to `to`.
fn find(hands: &mut impl Hands, quest: &str, here: &str, to: &str) -> Result<Vec<Session>, String> {
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
    Ok(hands
        .sessions()?
        .into_iter()
        .filter(|session| session.machine == here && session.quest.as_deref() == Some(quest))
        .filter(|session| runs(session) || parked(session, boot.as_deref()))
        .collect())
}

/// Tells each session to carry its work to `to`, waits until each has,
/// then carries what none of them held, and answers everything carried.
fn push(
    hands: &mut impl Hands,
    quest: &str,
    to: &str,
    sessions: &[Session],
) -> Result<Vec<Done>, String> {
    for session in sessions {
        hands
            .steer(
                &session.address,
                json!({"act": "carry", "to": to, "from": "person"}),
            )
            .map_err(|why| format!("{} wasn't told: {why}", session.address))?;
    }
    let waiting: BTreeSet<&str> = sessions.iter().filter_map(session_id).collect();
    loop {
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
        return Err(format!("nothing of {quest} was here to carry"));
    }
    Ok(done)
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
/// in place, and only then pi takes it up in its own folder. Its words go
/// to the shell apart from the script, so none is read as the shell's.
fn arriving(quest: &str, from: &str, to: &str, one: &Done) -> Start {
    let script = concat!(
        r#"pi-carry arrive "$1" --from "$2" --session "$3" && "#,
        r#"cd "$HOME/${4#\~/}" && exec pi --session "$3""#
    );
    Start {
        work: Work {
            needs: vec![Need::Machine(to.to_owned())],
            prefers: Vec::new(),
            tolerates: Vec::new(),
            may_wait: false,
            lasts: None,
        },
        folder: None,
        command: [
            "sh",
            "-c",
            script,
            PI_CARRY,
            quest,
            from,
            &one.session,
            &one.cwd,
        ]
        .map(str::to_owned)
        .to_vec(),
        may_cost: false,
    }
}

/// Whether a session's process runs.
fn runs(session: &Session) -> bool {
    matches!(session.state, State::Busy | State::Idle)
}

/// Whether a session is parked this boot: its process ended while idle,
/// for its holder to start again, which a holder can only do in the boot
/// it began in. Where the machine names no boot, any park is taken as
/// this boot's.
fn parked(session: &Session, boot: Option<&str>) -> bool {
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

/// The session's own id, as its presence names it and its done file does.
fn session_id(session: &Session) -> Option<&str> {
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
    use std::collections::BTreeSet;

    use serde_json::{Value, json};

    use super::{Carried, Done, Failed, Hands, PI_CARRY, Step, carry};
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
        shared: Vec<String>,
        started: Vec<Start>,
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
        fn steer(&mut self, address: &str, steer: Value) -> Result<(), String> {
            self.steered.push((address.to_owned(), steer));
            Ok(())
        }
        fn done(&mut self) -> Result<Vec<Done>, String> {
            let told: BTreeSet<&str> = self.steered.iter().map(|(at, _)| at.as_str()).collect();
            let mut done: Vec<Done> = self
                .sessions
                .iter()
                .filter(|one| told.contains(one.address.as_str()))
                .filter_map(|one| one.presence.get("session")?.as_str())
                .filter(|id| self.carries.contains(*id))
                .map(|id| Done {
                    session: id.to_owned(),
                    cwd: "~/src/harbour".to_owned(),
                })
                .collect();
            if self.rest.is_some() {
                done.push(Done {
                    session: "s-gone".to_owned(),
                    cwd: "~/src/gone".to_owned(),
                });
            }
            Ok(done)
        }
        fn carry_rest(&mut self, quest: &str, to: &str) -> Result<(), String> {
            self.rest = Some(format!("{quest} {to}"));
            Ok(())
        }
        fn share(&mut self, quest: &str) -> Result<(), String> {
            self.shared.push(quest.to_owned());
            Ok(())
        }
        fn start(&mut self, start: Start) -> Result<Started, String> {
            let held = format!("h{}", self.started.len());
            self.started.push(start);
            Ok(Started {
                machine: "penelope".to_owned(),
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

    /// A realm of optimus and penelope, where HARBOR runs busy and is
    /// parked this boot on optimus, was parked a boot before there, runs on
    /// penelope already, and another quest runs beside it.
    fn realm() -> Realm {
        let none = json!({});
        let parked = |boot: &str| json!({"state": "parked", "owner": {"bootId": boot}});
        Realm {
            machines: vec!["penelope".to_owned()],
            sessions: vec![
                session("BUSY01", "optimus", QUEST, "busy", &none),
                session("NAPS02", "optimus", QUEST, "ended", &parked("this-boot")),
                session(
                    "OLDNAP",
                    "optimus",
                    QUEST,
                    "ended",
                    &parked("a-boot-before"),
                ),
                session("THERE3", "penelope", QUEST, "idle", &none),
                session("OTHER4", "optimus", "QEST-20261107-OTHERS", "idle", &none),
            ],
            boot: Some("this-boot".to_owned()),
            carries: ["s-busy01", "s-naps02"].map(str::to_owned).into(),
            ends: true,
            patience: 3,
            ..Realm::default()
        }
    }

    /// Carries HARBOR from optimus to `to` in `realm`, with each line said.
    fn carried(realm: &mut Realm, to: &str) -> (Result<Carried, Failed>, Vec<Value>) {
        let mut lines = Vec::new();
        let done = carry(realm, QUEST, "optimus", to, &mut |line: &Value| {
            lines.push(line.clone());
        });
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
    fn a_quest_is_carried_step_by_step_with_its_sessions_here() {
        let mut realm = realm();
        let (done, lines) = carried(&mut realm, "penelope");
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
        assert_eq!(realm.steered[0].1["to"], "penelope", "to the machine named");
        assert_eq!(
            realm.rest.as_deref(),
            Some("QEST-20261107-HARBOR penelope"),
            "pi-carry takes what no session carried"
        );
        assert_eq!(realm.shared, [QUEST], "shared once, after the push");
        assert_eq!(realm.started.len(), 3, "each carried session starts there");
        let first = &realm.started[0];
        assert!(
            matches!(first.work.needs.as_slice(), [Need::Machine(on)] if on == "penelope"),
            "on the machine named: {:?}",
            first.work.needs
        );
        assert_eq!(
            &first.command[3..],
            [PI_CARRY, QUEST, "optimus", "s-busy01", "~/src/harbour"],
            "arriving with its own session and folder"
        );
        assert!(
            first.command[2].starts_with("pi-carry arrive"),
            "arrives before pi starts"
        );
        assert_eq!(done.machine, "penelope", "said where it went");
        let addresses: Vec<&str> = done
            .sessions
            .iter()
            .map(|one| one.address.as_str())
            .collect();
        assert_eq!(
            addresses,
            ["BUSY01", "NAPS02", "s-gone"],
            "each by its address here"
        );
        assert_eq!(
            done.sessions[0].attach,
            ["mycelium", "attach", "h0"],
            "with words to join it"
        );
    }

    #[test]
    fn a_machine_out_of_reach_or_this_one_is_refused_before_anything_moves() {
        for to in ["lighthouse", "optimus"] {
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
        let (done, lines) = carried(&mut realm, "penelope");
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
    }

    #[test]
    fn a_session_still_running_here_after_it_carried_is_said() {
        let mut realm = Realm {
            ends: false,
            ..realm()
        };
        let (done, lines) = carried(&mut realm, "penelope");
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
        assert_eq!(realm.started.len(), 3, "what started there stays started");
    }
}
