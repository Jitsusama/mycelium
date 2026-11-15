//! Turns for held sessions coming back, so a restart doesn't bring every
//! one back at once. Each holder whose program resumes a session asks its
//! node for a turn before it starts the program; the node lets a few
//! through at a time, by what each session's record said before it went.
//!
//! At the 22:22 reboot of 2026-11-14, 102 holders started in about four
//! minutes, and their sessions, 80 of 96 of them busy before it, sent 90
//! to 94% of a 6.4 Mbit/s upload, each resumed turn's first request about
//! half a megabyte; at 00:36 on 2026-11-15, 24 started in the same second
//! after the out-of-memory killer's. A few at a time, each first request
//! goes up in a second or two rather than all in minutes.
//!
//! The queue lives only in the node's memory: a node restarting lets every
//! waiting holder's connection go, and each holder takes that as its turn
//! and starts its program, as it does when no node answers at all. A
//! restart's sessions come back all at once then, as they did before.

use tokio::sync::oneshot;
use tokio::time::{Duration, Instant};

use crate::record::{Session, State};

/// How many resumed sessions run their first turn at once. Four first
/// requests of about half a megabyte go up in about three seconds of a
/// 6.4 Mbit/s upload, leaving the line to the person between them, and
/// four sessions working at once still bring a machine's 100 back within
/// half an hour.
pub const AT_ONCE: usize = 4;

/// The longest a resumed session's turn holds its place, though it is
/// still busy: past its first request, what a session sends is what it
/// would send anyway.
pub const LONGEST_TURN: Duration = Duration::from_secs(60);

/// Which sessions come back first, the first first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rank {
    /// Its record says it waits on its person, for an answer or an
    /// approval, so the person isn't left waiting on the rest.
    WaitsOnItsPerson,
    /// It was working as it went, so it resumes a turn.
    WasBusy,
    /// Anything else, which resumes waiting.
    Rest,
}

impl Rank {
    /// Where `session` comes back, by what its record said last.
    #[must_use]
    pub fn of(session: &Session) -> Self {
        let waits = session
            .wants
            .as_ref()
            .is_some_and(|wants| matches!(wants.kind.as_str(), "question" | "approval"));
        if waits {
            Self::WaitsOnItsPerson
        } else if was_busy(session) {
            Self::WasBusy
        } else {
            Self::Rest
        }
    }
}

/// Whether `session` was working when it was last heard, as its record
/// says or, for one ended, as its presence said last.
fn was_busy(session: &Session) -> bool {
    let presence = &session.presence;
    session.state == State::Busy
        || presence.get("busy").and_then(serde_json::Value::as_bool) == Some(true)
        || presence.get("state").and_then(serde_json::Value::as_str) == Some("busy")
}

/// What asking for a turn comes to.
#[derive(Debug)]
pub enum Asked {
    /// Go now: it isn't held back, or it was let through at once.
    Go,
    /// Wait for `go`, holding `ticket` to leave the queue by.
    Wait {
        ticket: u64,
        go: oneshot::Receiver<()>,
    },
}

/// The sessions waiting for a turn and those running theirs.
#[derive(Debug, Default)]
pub struct Turns {
    waiting: Vec<Waiting>,
    running: Vec<Running>,
    next: u64,
}

#[derive(Debug)]
struct Waiting {
    ticket: u64,
    session: String,
    rank: Rank,
    go: oneshot::Sender<()>,
}

#[derive(Debug)]
struct Running {
    session: String,
    since: Instant,
    /// Its record's `at` as its turn began, so only what its new process
    /// says ends the turn.
    heard: Option<String>,
    /// Whether it must be heard working before being heard idle ends its
    /// turn, as one that was working resumes a turn once it has started.
    resumes_work: bool,
    heard_working: bool,
}

impl Turns {
    /// Asks a turn for `session`, as `sessions` say each stands now. A
    /// session no record names is new, with nothing to resume, and goes
    /// at once.
    pub fn ask(&mut self, session: &str, sessions: &[Session], now: Instant) -> Asked {
        let Some(known) = sessions.iter().find(|one| one.id == session) else {
            return Asked::Go;
        };
        let (go, gone) = oneshot::channel();
        let ticket = self.next;
        self.next += 1;
        self.waiting.push(Waiting {
            ticket,
            session: session.to_owned(),
            rank: Rank::of(known),
            go,
        });
        self.pass(sessions, now);
        Asked::Wait { ticket, go: gone }
    }

    /// Takes `ticket` out of the queue, for a holder that stopped waiting.
    pub fn leave(&mut self, ticket: u64) {
        self.waiting.retain(|waiting| waiting.ticket != ticket);
    }

    /// Whether anything waits for a turn.
    #[cfg(test)]
    fn is_waiting(&self) -> bool {
        !self.waiting.is_empty()
    }

    /// Ends each turn its session is done with, as `sessions` say each
    /// stands at `now`, then lets the next through while fewer than
    /// [`AT_ONCE`] run: those waiting on their person first, then those
    /// that were working, then the rest, each kind in the order it asked.
    pub fn pass(&mut self, sessions: &[Session], now: Instant) {
        self.running
            .retain_mut(|running| !running.done(sessions, now));
        while self.running.len() < AT_ONCE {
            let Some(next) = self
                .waiting
                .iter()
                .enumerate()
                .min_by_key(|(_, waiting)| (waiting.rank, waiting.ticket))
                .map(|(at, _)| at)
            else {
                break;
            };
            let waiting = self.waiting.remove(next);
            // A holder that hung up has left already, and takes no turn.
            if waiting.go.send(()).is_ok() {
                let known = sessions.iter().find(|one| one.id == waiting.session);
                self.running.push(Running {
                    session: waiting.session,
                    since: now,
                    heard: known.map(|one| one.at.clone()),
                    resumes_work: waiting.rank != Rank::Rest,
                    heard_working: false,
                });
            }
        }
    }
}

impl Running {
    /// Whether this turn is over: its time is up, or its session, heard
    /// from since it began, idles or ends, one that was working only once
    /// it has been heard working again.
    fn done(&mut self, sessions: &[Session], now: Instant) -> bool {
        if now.duration_since(self.since) >= LONGEST_TURN {
            return true;
        }
        let Some(session) = sessions.iter().find(|one| one.id == self.session) else {
            return true;
        };
        if self.heard.as_deref() == Some(session.at.as_str()) {
            return false;
        }
        match session.state {
            State::Busy => {
                self.heard_working = true;
                false
            }
            State::Idle => self.heard_working || !self.resumes_work,
            State::Ended => true,
            State::Unreachable => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::record::Want;

    /// A session `id`, standing as `state`, last heard `at`, its presence
    /// saying it was `busy`.
    fn session(id: &str, state: State, at: &str, busy: bool) -> Session {
        Session {
            id: id.to_owned(),
            address: id.to_uppercase(),
            machine: "skiff".to_owned(),
            title: None,
            working_on: None,
            groups: Vec::new(),
            state,
            step: None,
            since: None,
            orchestrated_by: None,
            quest: None,
            wants: None,
            landings: Vec::new(),
            window: None,
            held: None,
            attach: None,
            host: None,
            conversation: None,
            at: at.to_owned(),
            presence: json!({"session": id, "busy": busy}).into(),
        }
    }

    /// The 24 sessions a restart took away as each was working.
    fn taken_away() -> Vec<Session> {
        (0..24)
            .map(|each| session(&format!("s{each:02}"), State::Ended, "before", true))
            .collect()
    }

    /// Asks a turn for each of `ids`, keeping each wait.
    fn asking(
        turns: &mut Turns,
        ids: &[String],
        sessions: &[Session],
        now: Instant,
    ) -> Vec<(String, oneshot::Receiver<()>)> {
        ids.iter()
            .map(|id| match turns.ask(id, sessions, now) {
                Asked::Wait { go, .. } => (id.clone(), go),
                Asked::Go => panic!("{id} resumes a session, so waits its turn"),
            })
            .collect()
    }

    /// The ids let through since last asked.
    fn let_through(waits: &mut [(String, oneshot::Receiver<()>)]) -> Vec<String> {
        waits
            .iter_mut()
            .filter_map(|(id, go)| go.try_recv().is_ok().then(|| id.clone()))
            .collect()
    }

    #[test]
    fn a_restarts_sessions_come_back_a_few_at_a_time() {
        let mut sessions = taken_away();
        let ids: Vec<String> = sessions.iter().map(|one| one.id.clone()).collect();
        let mut turns = Turns::default();
        let began = Instant::now();
        let mut waits = asking(&mut turns, &ids, &sessions, began);
        let first = let_through(&mut waits);
        assert_eq!(
            first,
            ["s00", "s01", "s02", "s03"],
            "four go at once, as asked"
        );
        turns.pass(&sessions, began + Duration::from_secs(59));
        assert!(
            let_through(&mut waits).is_empty(),
            "none more before one idles or a minute passes"
        );

        sessions[0] = session("s00", State::Idle, "starting", false);
        turns.pass(&sessions, began + Duration::from_secs(59));
        assert!(
            let_through(&mut waits).is_empty(),
            "one that was working isn't done as it starts, idle"
        );
        sessions[0] = session("s00", State::Busy, "working", true);
        turns.pass(&sessions, began + Duration::from_secs(59));
        sessions[0] = session("s00", State::Idle, "done", false);
        turns.pass(&sessions, began + Duration::from_secs(59));
        assert_eq!(
            let_through(&mut waits),
            ["s04"],
            "the next goes once one is heard working, then idle"
        );

        turns.pass(&sessions, began + LONGEST_TURN);
        assert_eq!(
            let_through(&mut waits),
            ["s05", "s06", "s07"],
            "and the rest of the first's places once their minute is up"
        );
    }

    #[test]
    fn a_session_waiting_on_its_person_goes_first_though_it_asked_last() {
        let mut sessions = taken_away();
        sessions[23].wants = Some(Want {
            kind: "question".to_owned(),
            since: "before".to_owned(),
            said: "which tide?".to_owned(),
        });
        sessions[22] = session("s22", State::Ended, "before", false);
        sessions[21].wants = Some(Want {
            kind: "approval".to_owned(),
            since: "before".to_owned(),
            said: "may I sail?".to_owned(),
        });
        let ids: Vec<String> = sessions.iter().map(|one| one.id.clone()).collect();
        let mut turns = Turns::default();
        let began = Instant::now();
        // The first four take the places before the rest have asked.
        let mut first = asking(&mut turns, &ids[..4], &sessions, began);
        assert_eq!(let_through(&mut first).len(), AT_ONCE, "four go at once");
        let mut waits = asking(&mut turns, &ids[4..], &sessions, began);
        turns.pass(&sessions, began + LONGEST_TURN);
        assert_eq!(
            let_through(&mut waits),
            ["s04", "s05", "s21", "s23"],
            "those waiting on their person go first, then the working in order"
        );
        let mut last = Vec::new();
        for minute in 3..12 {
            turns.pass(&sessions, began + LONGEST_TURN * minute);
            last.extend(let_through(&mut waits));
        }
        assert_eq!(
            last.last().map(String::as_str),
            Some("s22"),
            "and the one that wasn't working comes last"
        );
    }

    #[test]
    fn a_new_session_is_never_held_back() {
        let sessions = taken_away();
        let ids: Vec<String> = sessions.iter().map(|one| one.id.clone()).collect();
        let mut turns = Turns::default();
        let began = Instant::now();
        let _waits = asking(&mut turns, &ids, &sessions, began);
        assert!(
            matches!(turns.ask("s-new", &sessions, began), Asked::Go),
            "a session no record names has nothing to resume, so goes at once"
        );
    }

    #[test]
    fn a_holder_that_stops_waiting_takes_no_turn() {
        let sessions = taken_away();
        let ids: Vec<String> = sessions.iter().map(|one| one.id.clone()).collect();
        let mut turns = Turns::default();
        let began = Instant::now();
        let mut waits = asking(&mut turns, &ids[..6], &sessions, began);
        let _ = let_through(&mut waits);
        // s04 hangs up, as a holder its person started by hand does.
        drop(waits.remove(4));
        turns.pass(&sessions, began + LONGEST_TURN);
        assert_eq!(
            let_through(&mut waits),
            ["s05"],
            "the next waiting takes its place"
        );
        let later = began + LONGEST_TURN;
        let mut more = asking(&mut turns, &ids[6..9], &sessions, later);
        assert_eq!(
            let_through(&mut more),
            ["s06", "s07", "s08"],
            "three places were left"
        );
        let Asked::Wait { ticket, .. } = turns.ask("s09", &sessions, later) else {
            panic!("s09 waits, the four places taken");
        };
        turns.leave(ticket);
        assert!(!turns.is_waiting(), "one that leaves waits no more");
    }
}
