//! Whether a machine over its limits while another has room eases itself,
//! and how: as its own node judges it, never on an agent's word.
//!
//! A machine eases only while its node's [`Watch`](crate::balance::Watch)
//! holds it over while another has room, and only once every reading for
//! [`HOT_FOR`] has said so. It first puts its waiting sessions to sleep
//! where they are, counting them, and carries one only once the readings
//! [`SETTLE`] after still say it is over. Then it carries one session at a
//! time to a peer with room, placed as any work is, onto ground as firm as
//! here's, never one in a turn, on the person's screen, part way through
//! something a carry would break, or whose quest runs elsewhere too; what
//! can't be told of a session keeps it here. After a carry it waits
//! [`COOLDOWN`]; a session moved by anyone stays put for [`PIN`]; and
//! nothing goes to a machine a session came here from within
//! [`NO_RETURN`]. This module only chooses; the node acts on its choice.

use jiff::{SignedDuration, Timestamp};
use serde::Serialize;

use super::ground;
use crate::balance::{Flag, Reading};
use crate::declared::{Declared, Kind};
use crate::facts::Machine;
use crate::placement::{self, Loads, Placement, Work};
use crate::record::{Session, State};

/// How long every reading must say a machine is over while another has
/// room before it eases itself.
pub const HOT_FOR: SignedDuration = SignedDuration::from_mins(10);

/// How long after putting sessions to sleep the readings must still say
/// the machine is over before it carries one.
pub const SETTLE: SignedDuration = SignedDuration::from_mins(5);

/// How long after carrying a session it carries none again: stricter than
/// three an hour.
pub const COOLDOWN: SignedDuration = SignedDuration::from_mins(30);

/// How long after a session came here from a machine nothing goes there.
pub const NO_RETURN: SignedDuration = SignedDuration::from_hours(1);

/// How long a session moved, by anyone, is never eased away.
pub const PIN: SignedDuration = SignedDuration::from_hours(24);

/// What a session is part way through that a carry would break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    /// It holds a lease.
    Lease,
    /// It is merging a change.
    Merge,
    /// A heavy run of its goes or waits its turn.
    HeavyRun,
    /// It waits on a secret's release.
    Secret,
    /// It waits on a password prompt.
    Prompt,
}

impl Busy {
    fn said(self) -> &'static str {
        match self {
            Busy::Lease => "holds a lease",
            Busy::Merge => "is merging a change",
            Busy::HeavyRun => "has a heavy run going",
            Busy::Secret => "waits on a secret's release",
            Busy::Prompt => "waits on a password prompt",
        }
    }
}

/// One of this machine's sessions, with what the node knows of it; each
/// thing unknown is none, and keeps it here.
#[derive(Debug, Clone)]
pub struct Cell {
    /// The session, as its presence says.
    pub session: Session,
    /// The fence it runs in, as judged for an ordered carry.
    pub fence: Option<Kind>,
    /// Whether its quest runs only here.
    pub alone: Option<bool>,
    /// Whether it is on the person's screen.
    pub on_screen: Option<bool>,
    /// Whether it sleeps already.
    pub asleep: bool,
    /// What it is part way through.
    pub busy: Option<Vec<Busy>>,
}

impl Cell {
    /// Why it stays here, sleeping or carried, whatever the load.
    fn stays(&self, here: &str) -> Option<String> {
        let who = &self.session.address;
        if self.session.machine != here {
            return Some(format!("{who} runs on {}", self.session.machine));
        }
        match self.session.state {
            State::Idle => {}
            State::Busy => return Some(format!("{who} is in a turn")),
            State::Ended | State::Unreachable => return Some(format!("{who} isn't running")),
        }
        match self.on_screen {
            Some(false) => {}
            Some(true) => return Some(format!("{who} is on the person's screen")),
            None => {
                return Some(format!(
                    "whether {who} is on the person's screen can't be told"
                ));
            }
        }
        match self.busy.as_deref() {
            Some([]) => None,
            Some([first, ..]) => Some(format!("{who} {}", first.said())),
            None => Some(format!("what {who} is part way through can't be told")),
        }
    }
}

/// A session moved between machines, by anyone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Moved {
    /// The session, by id.
    pub session: String,
    /// The machine it left.
    pub from: String,
    /// The machine it went to.
    pub to: String,
    /// When.
    pub at: Timestamp,
    /// Whether a machine moved it to ease itself.
    pub eases: bool,
}

/// What a machine's node remembers for easing it: since when it has been
/// over, its sleeps, and the sessions moved from and to it.
#[derive(Debug, Default)]
pub struct History {
    hot_since: Option<Timestamp>,
    slept_at: Option<Timestamp>,
    sleeps: usize,
    moved: Vec<Moved>,
}

impl History {
    /// Takes in `reading`, the realm as `here`'s node read it: over since
    /// the first of an unbroken run of readings saying so, or not over.
    pub fn read(&mut self, here: &str, reading: &Reading) {
        let at = reading.at.parse::<Timestamp>().ok();
        let hot = reading
            .flags
            .iter()
            .any(|flag| matches!(flag, Flag::HotWhileCool { hot, .. } if hot == here));
        self.hot_since = match (hot, at) {
            (true, Some(at)) => Some(self.hot_since.unwrap_or(at)),
            _ => None,
        };
    }

    /// Notes `count` sessions put to sleep `at`.
    pub fn slept(&mut self, count: usize, at: Timestamp) {
        self.slept_at = Some(at);
        self.sleeps += count;
    }

    /// How many sessions it has put to sleep.
    #[must_use]
    pub fn sleeps(&self) -> usize {
        self.sleeps
    }

    /// Notes a session moved from or to this machine, by anyone.
    pub fn moved(&mut self, moved: Moved) {
        self.moved.push(moved);
    }

    fn pinned(&self, session: &str, now: Timestamp) -> bool {
        self.moved
            .iter()
            .any(|moved| moved.session == session && now.duration_since(moved.at) < PIN)
    }

    fn last_eased(&self, here: &str) -> Option<&Moved> {
        self.moved
            .iter()
            .filter(|moved| moved.eases && moved.from == here)
            .max_by_key(|moved| moved.at)
    }

    fn came_from(&self, here: &str, there: &str, now: Timestamp) -> bool {
        self.moved.iter().any(|moved| {
            moved.to == here && moved.from == there && now.duration_since(moved.at) < NO_RETURN
        })
    }
}

/// The machine easing itself, as its node knows the realm.
#[derive(Debug, Clone, Copy)]
pub struct Here<'a> {
    /// Its name.
    pub name: &'a str,
    /// What its configuration says of it.
    pub declared: Option<&'a Declared>,
    /// The realm's machines, this one among them.
    pub machines: &'a [Machine],
    /// What each carries, as placement weighs it.
    pub loads: &'a Loads,
}

/// What a machine does to ease itself now.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "choice")]
pub enum Choice {
    /// Nothing, saying why.
    Nothing {
        /// Why not.
        why: String,
    },
    /// Put these sessions to sleep where they are, by id.
    Sleep {
        /// The sessions.
        sessions: Vec<String>,
    },
    /// Carry `session` to `to`, saying why, with the reading it was
    /// chosen by.
    Carry {
        /// The session, by id.
        session: String,
        /// The machine it goes to.
        to: String,
        /// Why, in one line.
        why: String,
        /// The reading that chose it.
        reading: Reading,
    },
}

fn nothing(why: String) -> Choice {
    Choice::Nothing { why }
}

/// What `here` does now to ease itself, of its `cells`, by `reading` and
/// its `history`.
#[must_use]
pub fn choose(
    here: &Here,
    cells: &[Cell],
    reading: &Reading,
    history: &History,
    now: Timestamp,
) -> Choice {
    let since = match over(here.name, reading, history, now) {
        Ok(since) => since,
        Err(why) => return nothing(why),
    };
    let sleepy: Vec<String> = cells
        .iter()
        .filter(|cell| !cell.asleep && cell.stays(here.name).is_none())
        .map(|cell| cell.session.id.clone())
        .collect();
    if !sleepy.is_empty() {
        return Choice::Sleep { sessions: sleepy };
    }
    if let Some(slept) = history.slept_at
        && slept >= since
        && now.duration_since(slept) < SETTLE
    {
        return nothing(format!(
            "it put sessions to sleep at {slept}, and the readings for {} minutes after say \
             whether that eased it",
            SETTLE.as_mins()
        ));
    }
    if let Some(last) = history.last_eased(here.name)
        && now.duration_since(last.at) < COOLDOWN
    {
        return nothing(format!(
            "it carried {} at {}, and carries none again within {} minutes",
            last.session,
            last.at,
            COOLDOWN.as_mins()
        ));
    }
    let peers = peers(here, reading, history, now);
    if peers.is_empty() {
        return nothing(
            "no machine with room is a peer it may carry to, or one a session came from within \
             the hour"
                .to_owned(),
        );
    }
    carried(here, cells, reading, history, &peers, now)
}

/// Since when `here` has been over while another has room, or why it
/// doesn't ease itself.
fn over(
    here: &str,
    reading: &Reading,
    history: &History,
    now: Timestamp,
) -> Result<Timestamp, String> {
    let held = reading.held.as_ref().ok_or_else(|| {
        format!("no watch follows {here}'s readings, so whether it stays over can't be told")
    })?;
    if !held
        .iter()
        .any(|flag| matches!(flag, Flag::HotWhileCool { hot, .. } if hot == here))
    {
        return Err(format!(
            "{here} isn't held over while another machine has room"
        ));
    }
    match history.hot_since {
        Some(since) if now.duration_since(since) >= HOT_FOR => Ok(since),
        Some(since) => Err(format!(
            "{here} has been over only since {since}, not {} minutes straight",
            HOT_FOR.as_mins()
        )),
        None => Err(format!("{here}'s last reading doesn't say it is over")),
    }
}

/// The machines `here` may carry to: those the watch holds cool while it
/// is hot, peers rather than workers, and none a session came from within
/// the hour.
fn peers(here: &Here, reading: &Reading, history: &History, now: Timestamp) -> Vec<Machine> {
    let cool: Vec<&str> = reading
        .held
        .iter()
        .flatten()
        .filter_map(|flag| match flag {
            Flag::HotWhileCool { hot, cool } if hot == here.name => Some(cool.as_str()),
            _ => None,
        })
        .collect();
    here.machines
        .iter()
        .filter(|machine| machine.name != here.name && cool.contains(&machine.name.as_str()))
        .filter(|machine| {
            machine
                .declared
                .as_ref()
                .is_some_and(|declared| !declared.costs_money && declared.leaves_at.is_none())
        })
        .filter(|machine| !history.came_from(here.name, &machine.name, now))
        .cloned()
        .collect()
}

/// The first of `cells` that may go to one of `peers`, placed as any work
/// is and onto ground as firm, or why none may.
fn carried(
    here: &Here,
    cells: &[Cell],
    reading: &Reading,
    history: &History,
    peers: &[Machine],
    now: Timestamp,
) -> Choice {
    // A session lasts as long as time can say, so no machine that leaves
    // takes it.
    let work = Work {
        needs: Vec::new(),
        prefers: Vec::new(),
        tolerates: Vec::new(),
        may_wait: false,
        lasts: Some(SignedDuration::MAX),
    };
    let mut why_not = Vec::new();
    for cell in cells {
        let who = &cell.session.address;
        if let Some(why) = cell.stays(here.name) {
            why_not.push(why);
            continue;
        }
        match cell.alone {
            Some(true) => {}
            Some(false) => {
                why_not.push(format!("{who}'s quest runs on other machines too"));
                continue;
            }
            None => {
                why_not.push(format!("where else {who}'s quest runs can't be told"));
                continue;
            }
        }
        if history.pinned(&cell.session.id, now) {
            why_not.push(format!("{who} moved within {} hours", PIN.as_hours()));
            continue;
        }
        let to = match placement::decide(&work, peers, here.loads).placement {
            Placement::Placed { machine, .. } => machine,
            Placement::Pending { misses } | Placement::Refused { misses } => {
                why_not.push(format!("{who}: {}", placement::said(&misses)));
                continue;
            }
        };
        let there = peers.iter().find(|machine| machine.name == to);
        if let Err(why) = ground::firm(cell.fence, here.declared, &to, there) {
            why_not.push(format!("{who}: {why}"));
            continue;
        }
        return Choice::Carry {
            session: cell.session.id.clone(),
            why: why(here.name, &to, reading, history),
            to,
            reading: reading.clone(),
        };
    }
    nothing(if why_not.is_empty() {
        format!("no session runs on {}", here.name)
    } else {
        why_not.join("; ")
    })
}

/// Why `here` carries a session to `to`, in one line.
fn why(here: &str, to: &str, reading: &Reading, history: &History) -> String {
    let load = reading
        .machines
        .iter()
        .find(|load| load.machine == here)
        .and_then(|load| load.load)
        .map_or_else(String::new, |load| format!(" at load {load:.2}"));
    let since = history
        .hot_since
        .map_or_else(String::new, |since| format!(" since {since}"));
    format!("{here} over{load}{since} while {to} has room")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::balance;
    use crate::declared::Doors;
    use crate::facts::{self, Readiness, Sources};
    use crate::room::{Limit, Pressure, Room, Takes};

    const NOW: &str = "2026-11-16T08:00:00Z";

    fn now() -> Timestamp {
        NOW.parse().expect("a time")
    }

    fn ago(mins: i64) -> Timestamp {
        now() - SignedDuration::from_mins(mins)
    }

    fn declaring(kinds: &[Kind]) -> Declared {
        let open = Doors {
            builders: ["big".to_owned()].into(),
            forges: ["github".to_owned()].into(),
            secrets: ["forge".to_owned()].into(),
            runs: Vec::new(),
        };
        Declared {
            reach: Vec::new(),
            probes: BTreeMap::new(),
            outlives: Vec::new(),
            costs_money: false,
            taints: Vec::new(),
            cores: None,
            agents: BTreeMap::new(),
            leaves_at: None,
            within: None,
            hosts: Vec::new(),
            fence: Some(kinds.iter().map(|&kind| (kind, open.clone())).collect()),
        }
    }

    /// A ready machine of 16 cores and 64 GiB, with processor pressure
    /// `cpu`, declaring a session fenced.
    fn machine(name: &str, cpu: f64) -> Machine {
        let nowhere = Sources {
            home: "/".into(),
            queue: None,
            disk_guard: None,
            declared: "/nowhere".into(),
            reached: "/nowhere".into(),
            updated: "/nowhere".into(),
        };
        let mut machine = facts::measure(name, &nowhere);
        machine.state = Readiness::Ready;
        machine.cpu.cores = Some(16);
        machine.cpu.busy = None;
        machine.memory = Some(64 << 30);
        machine.measured_at = NOW.to_owned();
        machine.declared = Some(declaring(&[Kind::Fence]));
        machine.room = Some(Room {
            said_at: NOW.to_owned(),
            memory_available: 40 << 30,
            pressure: Pressure {
                cpu,
                memory: 0.0,
                io: 0.0,
            },
            takes: Takes {
                builds: 1,
                vm_tests: 1,
            },
            over: Some(if cpu >= balance::OVER_CPU_PRESSURE {
                vec![Limit::Cpu]
            } else {
                Vec::new()
            }),
        });
        machine
    }

    /// Skiff over, tern with room.
    fn realm() -> Vec<Machine> {
        vec![machine("skiff", 86.0), machine("tern", 4.6)]
    }

    /// The realm read at `at`, held as the watch holds it.
    fn reading(machines: &[Machine], at: Timestamp) -> Reading {
        let mut reading = balance::read(machines, &[], at);
        reading.held = Some(reading.flags.clone());
        reading
    }

    /// What skiff's node remembers after `mins` minutes over.
    fn hot_for(machines: &[Machine], mins: i64) -> History {
        let mut history = History::default();
        history.read("skiff", &reading(machines, ago(mins)));
        history.read("skiff", &reading(machines, now()));
        history
    }

    /// A session of skiff's waiting, asleep, off the screen, part way
    /// through nothing, its quest only here.
    fn cell(id: &str) -> Cell {
        Cell {
            session: serde_json::from_value(json!({
                "id": id, "address": id.to_uppercase(), "machine": "skiff",
                "groups": [], "state": "idle", "at": NOW,
            }))
            .expect("a session"),
            fence: Some(Kind::Fence),
            alone: Some(true),
            on_screen: Some(false),
            asleep: true,
            busy: Some(Vec::new()),
        }
    }

    fn chose(machines: &[Machine], cells: &[Cell], history: &History) -> Choice {
        let declared = declaring(&[Kind::Fence]);
        let here = Here {
            name: "skiff",
            declared: Some(&declared),
            machines,
            loads: &Loads::new(),
        };
        choose(&here, cells, &reading(machines, now()), history, now())
    }

    fn carries(choice: &Choice) -> Option<(&str, &str)> {
        match choice {
            Choice::Carry { session, to, .. } => Some((session, to)),
            _ => None,
        }
    }

    fn why_not(choice: &Choice) -> &str {
        match choice {
            Choice::Nothing { why } => why,
            other => panic!("expected nothing, chose {other:?}"),
        }
    }

    #[test]
    fn a_machine_eases_only_while_held_over_while_another_has_room() {
        let realm = realm();
        let history = hot_for(&realm, 11);
        assert_eq!(
            carries(&chose(&realm, &[cell("s1")], &history)),
            Some(("s1", "tern")),
            "held over while tern has room, it carries"
        );
        let calm = [machine("skiff", 4.6), machine("tern", 4.6)];
        assert_eq!(
            why_not(&chose(&calm, &[cell("s1")], &history)),
            "skiff isn't held over while another machine has room"
        );
        let declared = declaring(&[Kind::Fence]);
        let here = Here {
            name: "skiff",
            declared: Some(&declared),
            machines: &realm,
            loads: &Loads::new(),
        };
        let mut unwatched = reading(&realm, now());
        unwatched.held = None;
        assert_eq!(
            why_not(&choose(&here, &[cell("s1")], &unwatched, &history, now())),
            "no watch follows skiff's readings, so whether it stays over can't be told"
        );
    }

    #[test]
    fn a_machine_eases_only_once_over_ten_minutes_straight() {
        let realm = realm();
        assert!(
            why_not(&chose(&realm, &[cell("s1")], &hot_for(&realm, 9)))
                .ends_with("not 10 minutes straight"),
            "nine minutes over isn't ten"
        );
        let mut broken = hot_for(&realm, 30);
        broken.read(
            "skiff",
            &reading(&[machine("skiff", 4.6), machine("tern", 4.6)], ago(5)),
        );
        broken.read("skiff", &reading(&realm, now()));
        assert!(
            why_not(&chose(&realm, &[cell("s1")], &broken)).ends_with("not 10 minutes straight"),
            "a reading not over between starts the ten minutes again"
        );
        assert!(
            carries(&chose(&realm, &[cell("s1")], &hot_for(&realm, 10))).is_some(),
            "ten minutes straight eases"
        );
    }

    #[test]
    fn a_hot_machine_puts_its_waiting_cells_to_sleep_before_it_carries_any() {
        let realm = realm();
        let history = hot_for(&realm, 11);
        let awake = |id: &str| Cell {
            asleep: false,
            ..cell(id)
        };
        let turning = Cell {
            session: Session {
                state: State::Busy,
                ..awake("s3").session
            },
            ..awake("s3")
        };
        assert_eq!(
            chose(
                &realm,
                &[awake("s1"), cell("s2"), turning, awake("s4")],
                &history
            ),
            Choice::Sleep {
                sessions: vec!["s1".to_owned(), "s4".to_owned()]
            },
            "the waiting cells awake sleep first, and not one in a turn"
        );
    }

    #[test]
    fn after_sleeping_it_carries_only_once_the_readings_after_still_say_over() {
        let realm = realm();
        let mut history = hot_for(&realm, 20);
        history.slept(2, ago(4));
        assert!(
            why_not(&chose(&realm, &[cell("s1")], &history)).ends_with("say whether that eased it"),
            "four minutes after sleeping is too soon to tell"
        );
        assert_eq!(history.sleeps(), 2, "the sleeps are counted");
        let mut settled = hot_for(&realm, 20);
        settled.slept(2, ago(5));
        assert_eq!(
            carries(&chose(&realm, &[cell("s1")], &settled)),
            Some(("s1", "tern")),
            "still over five minutes after, it carries"
        );
    }

    #[test]
    fn a_cell_in_a_turn_on_screen_or_part_way_through_something_stays() {
        let realm = realm();
        let history = hot_for(&realm, 11);
        let with = |change: &dyn Fn(&mut Cell)| {
            let mut one = cell("s1");
            change(&mut one);
            why_not(&chose(&realm, &[one], &history)).to_owned()
        };
        assert_eq!(
            with(&|one| one.session.state = State::Busy),
            "S1 is in a turn"
        );
        assert_eq!(
            with(&|one| one.on_screen = Some(true)),
            "S1 is on the person's screen"
        );
        assert_eq!(
            with(&|one| one.on_screen = None),
            "whether S1 is on the person's screen can't be told"
        );
        for (busy, said) in [
            (Busy::Lease, "holds a lease"),
            (Busy::Merge, "is merging a change"),
            (Busy::HeavyRun, "has a heavy run going"),
            (Busy::Secret, "waits on a secret's release"),
            (Busy::Prompt, "waits on a password prompt"),
        ] {
            assert_eq!(
                with(&|one| one.busy = Some(vec![busy])),
                format!("S1 {said}")
            );
        }
        assert_eq!(
            with(&|one| one.busy = None),
            "what S1 is part way through can't be told"
        );
        assert_eq!(
            with(&|one| one.alone = Some(false)),
            "S1's quest runs on other machines too"
        );
        assert_eq!(
            with(&|one| one.alone = None),
            "where else S1's quest runs can't be told"
        );
        assert_eq!(
            carries(&chose(&realm, &[with_turn(), cell("s2")], &history)),
            Some(("s2", "tern")),
            "one that stays leaves the next free to go"
        );
    }

    fn with_turn() -> Cell {
        let mut one = cell("s1");
        one.session.state = State::Busy;
        one
    }

    #[test]
    fn a_session_moved_within_the_day_stays() {
        let realm = realm();
        // Carried here by the person from osprey, which is no machine of
        // this realm now.
        let moved = |hours: i64| {
            let mut history = hot_for(&realm, 11);
            history.moved(Moved {
                session: "s1".to_owned(),
                from: "osprey".to_owned(),
                to: "skiff".to_owned(),
                at: ago(hours * 60),
                eases: false,
            });
            chose(&realm, &[cell("s1")], &history)
        };
        assert_eq!(why_not(&moved(23)), "S1 moved within 24 hours");
        assert!(carries(&moved(24)).is_some(), "a day on, it may go");
    }

    #[test]
    fn none_is_carried_within_half_an_hour_of_the_last() {
        let realm = realm();
        let eased = |mins| {
            let mut history = hot_for(&realm, 60);
            history.moved(Moved {
                session: "s0".to_owned(),
                from: "skiff".to_owned(),
                to: "osprey".to_owned(),
                at: ago(mins),
                eases: true,
            });
            chose(&realm, &[cell("s1")], &history)
        };
        assert!(
            why_not(&eased(29)).ends_with("carries none again within 30 minutes"),
            "29 minutes after the last"
        );
        assert!(carries(&eased(30)).is_some(), "30 minutes after the last");
    }

    #[test]
    fn nothing_goes_to_a_machine_a_session_came_from_within_the_hour() {
        let realm = realm();
        let came = |mins| {
            let mut history = hot_for(&realm, 11);
            history.moved(Moved {
                session: "s0".to_owned(),
                from: "tern".to_owned(),
                to: "skiff".to_owned(),
                at: ago(mins),
                eases: true,
            });
            chose(&realm, &[cell("s1")], &history)
        };
        assert!(
            why_not(&came(59)).starts_with("no machine with room is a peer"),
            "s0 came from tern 59 minutes ago"
        );
        assert!(carries(&came(60)).is_some(), "an hour on, tern takes one");
    }

    #[test]
    fn a_worker_with_room_is_never_where_a_session_goes() {
        let history = hot_for(&realm(), 11);
        let mut paid = realm();
        paid[1].declared.as_mut().expect("declared").costs_money = true;
        assert!(
            why_not(&chose(&paid, &[cell("s1")], &history))
                .starts_with("no machine with room is a peer"),
            "a machine that costs money is a worker"
        );
        let mut lent = realm();
        lent[1].declared.as_mut().expect("declared").leaves_at = Some(now() + PIN);
        assert!(
            why_not(&chose(&lent, &[cell("s1")], &history))
                .starts_with("no machine with room is a peer"),
            "a machine that leaves is a worker"
        );
        let mut untold = realm();
        untold[1].declared = None;
        assert!(
            why_not(&chose(&untold, &[cell("s1")], &history))
                .starts_with("no machine with room is a peer"),
            "a machine that says nothing of itself can't be told a peer"
        );
    }

    #[test]
    fn a_session_lands_only_on_ground_as_firm_as_here() {
        let realm = realm();
        let history = hot_for(&realm, 11);
        let mut strict = realm.clone();
        strict[1].declared = Some(declaring(&[Kind::Strict]));
        assert_eq!(
            why_not(&chose(&strict, &[cell("s1")], &history)),
            "S1: tern runs no fenced session, so the session would land looser there"
        );
        let mut unsaid = cell("s1");
        unsaid.fence = None;
        assert_eq!(
            why_not(&chose(&realm, &[unsaid], &history)),
            "S1: the session doesn't say which fence it runs in, so where it lands can't be \
             judged"
        );
    }

    #[test]
    fn a_carry_says_who_where_and_why_with_the_reading_that_chose_it() {
        let realm = realm();
        let history = hot_for(&realm, 11);
        let Choice::Carry {
            session,
            to,
            why,
            reading: chosen_by,
        } = chose(&realm, &[cell("s1")], &history)
        else {
            panic!("expected a carry");
        };
        assert_eq!((session.as_str(), to.as_str()), ("s1", "tern"));
        assert!(
            why.starts_with("skiff over at load ")
                && why.ends_with(" since 2026-11-16T07:49:00Z while tern has room"),
            "{why}"
        );
        assert_eq!(
            chosen_by,
            reading(&realm, now()),
            "the reading it was chosen by"
        );
    }
}
