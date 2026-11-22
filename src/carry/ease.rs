//! Whether a machine over its limits eases itself, and how: as its own
//! node judges it, never on an agent's word.
//!
//! A machine eases only once its readings for [`HOT_FOR`] have said it is
//! over by its own pressure, through lone readings saying otherwise,
//! whether or not another machine has room, and only while a
//! [`Watch`](crate::balance::Watch) follows them. It first puts its
//! waiting sessions to sleep where they are, counting them, which needs
//! no other machine. It carries one only while its node's watch holds it
//! over while another has room, and only once the readings [`SETTLE`]
//! after a sleep still say it is over. Then it carries one session at a
//! time to a peer with room, placed as any work is, onto ground as firm as
//! here's, never one in a turn, on the person's screen, orchestrating
//! other sessions, part way through something a carry would break, or
//! whose quest runs elsewhere too; what can't be told of a session keeps
//! it here. After a carry it waits [`COOLDOWN`], and it carries at most
//! [`DAILY`] within a [`DAY`], counted as the larger of what its node
//! remembers and what the record of carries holds; a session moved by
//! anyone stays put for [`PIN`]; and
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

/// How long the readings must say a machine is over by its own pressure
/// before it eases itself, through lone readings saying otherwise
/// each followed within [`COOL_FOR`] by one saying over.
pub const HOT_FOR: SignedDuration = SignedDuration::from_mins(10);

/// How soon a reading saying over must follow a lone one saying a
/// machine isn't for the time it has been over to go on; a second
/// reading saying otherwise in a row starts it again however soon it
/// comes. A machine as busy as it gets reads cool now and then between
/// its hot readings, a few minutes apart, and a lone one
/// starting the [`HOT_FOR`] again kept such a machine from ever easing
/// itself: on 21 November one read over for hours and carried nothing,
/// over only since its last cool reading 27 times.
pub const COOL_FOR: SignedDuration = SignedDuration::from_mins(5);

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

/// How many sessions a machine carries away to ease itself within a
/// [`DAY`], at most, beside [`COOLDOWN`].
pub const DAILY: usize = 6;

/// The span [`DAILY`] counts over.
pub const DAY: SignedDuration = SignedDuration::from_hours(24);

/// Who a carry made to ease `machine` is recorded as by: its node, never
/// the person or an orchestrator.
#[must_use]
pub fn by(machine: &str) -> String {
    format!("node@{machine}")
}

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
    /// Whether it sleeps already: parked, its process ended until it is
    /// started again.
    pub asleep: bool,
    /// Whether it orchestrates other sessions.
    pub orchestrates: Option<bool>,
    /// What it is part way through.
    pub busy: Option<Vec<Busy>>,
    /// How much of it a carry takes, in bytes: its log's size.
    pub size: Option<u64>,
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
            // A session put to sleep has ended its process, to start
            // again where a carry takes it.
            State::Ended if self.asleep => {}
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
        match self.orchestrates {
            Some(false) => {}
            Some(true) => return Some(format!("{who} orchestrates other sessions")),
            None => {
                return Some(format!(
                    "whether {who} orchestrates other sessions can't be told"
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
/// over, its sleeps, the sessions moved from and to it, and how many
/// carries of its own the record of carries last held.
#[derive(Debug)]
pub struct History {
    hot_since: Option<Timestamp>,
    cool_since: Option<Timestamp>,
    /// When the latest reading saying over came.
    last_hot: Option<Timestamp>,
    /// When the latest reading of any came.
    last_at: Option<Timestamp>,
    slept_at: Option<Timestamp>,
    sleeps: usize,
    moved: Vec<Moved>,
    kept_eases: Result<usize, String>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            hot_since: None,
            cool_since: None,
            last_hot: None,
            last_at: None,
            slept_at: None,
            sleeps: 0,
            moved: Vec::new(),
            kept_eases: Err(
                "the carries kept here haven't been read, so how many this machine \
                             carried within the day can't be told"
                    .to_owned(),
            ),
        }
    }
}

impl History {
    /// Takes in `reading`, the realm as `here`'s node read it: over, by
    /// its own pressure whether or not another machine has room, since the
    /// first of a run of readings saying so, or not over. A lone reading
    /// saying otherwise keeps the run, but a second in a row ends it, as
    /// does a reading [`COOL_FOR`] or more after the lone one, whichever
    /// that says, so a gap after a reading saying otherwise counts as
    /// cool; one after a hot reading doesn't. A sleep changes none of
    /// this, since a lone cool reading after one may be a session parking
    /// for a moment as easily as the machine eased.
    pub fn read(&mut self, here: &str, reading: &Reading) {
        let Ok(at) = reading.at.parse::<Timestamp>() else {
            // A reading whose time can't be told can't say for how long,
            // nor end what was asked to park, which waits on readings
            // saying for five minutes that it isn't over.
            self.hot_since = None;
            self.cool_since = None;
            return;
        };
        let hot = pressed(here, reading);
        self.last_at = Some(at);
        if hot {
            self.last_hot = Some(at);
        }
        let cooled = self
            .cool_since
            .is_some_and(|cool| at.duration_since(cool) >= COOL_FOR);
        let again = !hot && self.cool_since.is_some();
        if cooled || again {
            self.hot_since = None;
            self.cool_since = None;
        }
        if hot {
            self.hot_since.get_or_insert(at);
            self.cool_since = None;
        } else if self.hot_since.is_some() {
            self.cool_since.get_or_insert(at);
        }
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

    /// Whether the machine still counts as over for what it asked to
    /// park: until [`COOL_FOR`] has passed from its latest reading saying
    /// it is over by its own pressure to its latest timed reading of any,
    /// judged apart from the run, so a reading or two saying otherwise
    /// takes nothing back.
    #[must_use]
    pub fn over(&self) -> bool {
        self.last_hot
            .zip(self.last_at)
            .is_some_and(|(hot, at)| at.duration_since(hot) < COOL_FOR)
    }

    /// Notes a session moved from or to this machine, by anyone.
    pub fn moved(&mut self, moved: Moved) {
        self.moved.push(moved);
    }

    /// Notes how many carries the record of carries says this machine's
    /// node made to ease it within the day, or why that can't be told.
    pub fn kept_eases(&mut self, count: Result<usize, String>) {
        self.kept_eases = count;
    }

    /// How many sessions `here` carried away to ease itself within the
    /// day before `now`.
    /// The larger of the carries this node remembers and those the record
    /// of carries holds, so a record cut short lifts nothing.
    fn eased_today(&self, here: &str, now: Timestamp) -> Result<usize, String> {
        let kept = self.kept_eases.clone()?;
        let remembered = self
            .moved
            .iter()
            .filter(|moved| moved.eases && moved.from == here && now.duration_since(moved.at) < DAY)
            .count();
        Ok(kept.max(remembered))
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

impl<'a> Here<'a> {
    /// The machine named `name` among the realm's `machines`, as its own
    /// configuration says of it.
    #[must_use]
    pub fn of(name: &'a str, machines: &'a [Machine], loads: &'a Loads) -> Self {
        Self {
            name,
            declared: machines
                .iter()
                .find(|one| one.name == name)
                .and_then(|one| one.declared.as_ref()),
            machines,
            loads,
        }
    }
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
    match history.eased_today(here.name, now) {
        Ok(count) if count >= DAILY => {
            return nothing(format!(
                "it carried {count} sessions within the day, and carries at most {DAILY} a day"
            ));
        }
        Ok(_) => {}
        Err(why) => return nothing(why),
    }
    // A sleep needs no other machine, but a carry needs one with room,
    // held so over the last readings rather than in one.
    if !reading
        .held
        .iter()
        .flatten()
        .any(|flag| matches!(flag, Flag::HotWhileCool { hot, .. } if hot == here.name))
    {
        return nothing(format!(
            "{} has no session left to put to sleep, and isn't held over while another \
             machine has room to carry one to",
            here.name
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

/// Whether `reading` says `here` is over by its own pressure, whether or
/// not another machine has room: it takes work, and its queue says it is
/// past a limit or its processor pressure is at the line.
fn pressed(here: &str, reading: &Reading) -> bool {
    reading
        .machines
        .iter()
        .any(|load| load.machine == here && load.ready && load.over)
}

/// Since when `here` has been over by its own pressure, or why it doesn't
/// ease itself.
fn over(
    here: &str,
    reading: &Reading,
    history: &History,
    now: Timestamp,
) -> Result<Timestamp, String> {
    if reading
        .machines
        .iter()
        .find(|load| load.machine == here)
        .is_none_or(|load| load.stall.is_none())
    {
        return Err(format!(
            "{here}'s own pressure can't be felt, so it never eases itself"
        ));
    }
    if reading.held.is_none() {
        return Err(format!(
            "no watch follows {here}'s readings, so whether it stays over can't be told"
        ));
    }
    // A lone cool reading keeps the run, but nothing is done on one: the
    // reading that chooses must itself say over.
    if !pressed(here, reading) {
        return Err(format!(
            "{here}'s latest reading doesn't say it is over, so it waits for one that does"
        ));
    }
    match history.hot_since {
        Some(since) if now.duration_since(since) >= HOT_FOR => Ok(since),
        Some(since) => Err(format!(
            "{here} has been over only since {since}, not {} minutes",
            HOT_FOR.as_mins()
        )),
        // Only a reading whose time can't be told leaves no run behind a
        // reading saying over.
        None => Err(format!(
            "the time of {here}'s latest reading can't be told, so how long it has been over \
             can't either"
        )),
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
                .is_some_and(|declared| !declared.is_worker())
        })
        .filter(|machine| !history.came_from(here.name, &machine.name, now))
        .cloned()
        .collect()
}

/// The first of `cells` that may go to one of `peers`, placed as any work
/// is and onto ground as firm, or why none may. The smallest go first,
/// since they take least to carry; those whose size can't be told go
/// last.
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
        fence: None,
    };
    // The carry refuses a machine the start bringing a session back can't
    // run on, whatever the session, so each is left out before placing
    // and the next machine is placed instead.
    let mut left_out = Vec::new();
    let peers: Vec<Machine> = peers
        .iter()
        .filter(|machine| {
            let back = ground::brought_back(
                &machine.name,
                machine.declared.as_ref(),
                machine.system.as_deref(),
            );
            match back {
                Some(why) => {
                    left_out.push(why);
                    false
                }
                None => true,
            }
        })
        .cloned()
        .collect();
    // With none left, placement would say the realm knows no machine, so
    // the look says why each was left out instead.
    if peers.is_empty() {
        return nothing(left_out.join("; "));
    }
    let mut smallest_first: Vec<&Cell> = cells.iter().collect();
    smallest_first.sort_by_key(|cell| cell.size.map_or((1, 0), |size| (0, size)));
    let mut why_not = Vec::new();
    for cell in smallest_first {
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
        let to = match placement::decide(&work, &peers, here.loads).placement {
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
        why_not.extend(left_out);
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
pub(super) mod tests {
    use serde_json::json;

    use super::*;
    use crate::balance;
    use crate::declared::Doors;
    use crate::facts::{self, Readiness, Sources};
    use crate::room::{Limit, Pressure, Room, Takes};

    pub(in crate::carry) const NOW: &str = "2026-11-16T08:00:00Z";

    pub(in crate::carry) fn now() -> Timestamp {
        NOW.parse().expect("a time")
    }

    pub(in crate::carry) fn ago(mins: i64) -> Timestamp {
        now() - SignedDuration::from_mins(mins)
    }

    pub(in crate::carry) fn declaring(kinds: &[Kind]) -> Declared {
        let open = Doors {
            builders: ["big".to_owned()].into(),
            forges: ["github".to_owned()].into(),
            secrets: ["forge".to_owned()].into(),
            runs: vec![
                "/nix/store/0000-fence/bin/fence".to_owned(),
                "--".to_owned(),
            ],
        };
        Declared {
            fence: Some(kinds.iter().map(|&kind| (kind, open.clone())).collect()),
            ..Declared::default()
        }
    }

    /// A ready machine of 16 cores and 64 GiB, with processor pressure
    /// `cpu`, declaring a session fenced.
    fn machine(name: &str, cpu: f64) -> Machine {
        let mut machine = facts::measure(name, &Sources::nowhere());
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
            saturated: None,
        });
        machine
    }

    /// Skiff over, tern with room.
    pub(in crate::carry) fn realm() -> Vec<Machine> {
        vec![machine("skiff", 86.0), machine("tern", 4.6)]
    }

    /// The realm read at `at`, held as the watch holds it.
    fn reading(machines: &[Machine], at: Timestamp) -> Reading {
        let mut reading = balance::read(machines, &[], at);
        reading.held = Some(reading.flags.clone());
        reading
    }

    /// What skiff's node remembers after reading each realm the minutes
    /// ago beside it, oldest first.
    fn read_all(readings: &[(&[Machine], i64)]) -> History {
        let mut history = History::default();
        for (machines, mins) in readings {
            history.read("skiff", &reading(machines, ago(*mins)));
        }
        history.kept_eases(Ok(0));
        history
    }

    #[test]
    fn a_cool_spell_runs_to_the_next_reading_and_a_sleep_changes_nothing() {
        let realm = realm();
        let calm = [machine("skiff", 4.6), machine("tern", 4.6)];
        let lone = read_all(&[
            (&realm, 20),
            (&calm, 17),
            (&realm, 14),
            (&calm, 11),
            (&realm, 8),
            (&realm, 0),
        ]);
        assert!(
            carries(&chose(&realm, &[cell("s1")], &lone)).is_some(),
            "each hot reading ends the spell before it, so two lone cool ones break nothing"
        );
        let gap = read_all(&[(&realm, 30), (&calm, 20), (&realm, 0)]);
        assert!(
            why_not(&chose(&realm, &[cell("s1")], &gap)).ends_with("not 10 minutes"),
            "twenty minutes with no reading after a cool one is a cool spell"
        );
        // The hot reading comes within five minutes of the cool one, so
        // the run holds, the sleep before it or not.
        let mut slept = read_all(&[(&realm, 20), (&realm, 9)]);
        slept.slept(1, ago(8));
        slept.read("skiff", &reading(&calm, ago(6)));
        slept.read("skiff", &reading(&realm, ago(2)));
        slept.read("skiff", &reading(&realm, now()));
        assert!(
            carries(&chose(&realm, &[cell("s1")], &slept)).is_some(),
            "a lone cool reading after a sleep may be a session parking, so it breaks nothing"
        );
        // A sleep in a run before this one eased that run alone.
        let mut earlier = read_all(&[(&realm, 40), (&realm, 36)]);
        earlier.slept(1, ago(35));
        for (machines, mins) in [
            (&calm[..], 30),
            (&realm[..], 15),
            (&calm[..], 8),
            (&realm[..], 7),
        ] {
            earlier.read("skiff", &reading(machines, ago(mins)));
        }
        earlier.read("skiff", &reading(&realm, now()));
        assert!(
            carries(&chose(&realm, &[cell("s1")], &earlier)).is_some(),
            "a lone cool reading after an earlier run's sleep breaks nothing"
        );
    }

    /// What skiff's node remembers after `mins` minutes over.
    fn hot_for(machines: &[Machine], mins: i64) -> History {
        let mut history = History::default();
        history.read("skiff", &reading(machines, ago(mins)));
        history.read("skiff", &reading(machines, now()));
        history.kept_eases(Ok(0));
        history
    }

    /// A session of skiff's waiting, asleep, off the screen, part way
    /// through nothing, its quest only here.
    pub(in crate::carry) fn cell(id: &str) -> Cell {
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
            orchestrates: Some(false),
            busy: Some(Vec::new()),
            size: None,
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
    fn a_machine_sleeps_by_its_own_pressure_and_carries_only_while_another_has_room() {
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
            "skiff's latest reading doesn't say it is over, so it waits for one that does"
        );
        let full = [machine("skiff", 86.0), machine("tern", 86.0)];
        let crowded = hot_for(&full, 11);
        let awake = Cell {
            asleep: false,
            ..cell("s2")
        };
        assert_eq!(
            chose(&full, &[cell("s1"), awake], &crowded),
            Choice::Sleep {
                sessions: vec!["s2".to_owned()]
            },
            "over with no machine to spare, it still puts its waiting session to sleep"
        );
        assert_eq!(
            why_not(&chose(&full, &[cell("s1")], &crowded)),
            "skiff has no session left to put to sleep, and isn't held over while another \
             machine has room to carry one to"
        );
        let declared = declaring(&[Kind::Fence]);
        let here = Here {
            name: "skiff",
            declared: Some(&declared),
            machines: &realm,
            loads: &Loads::new(),
        };
        let mut unfelt = realm.clone();
        unfelt[0].room = None;
        assert_eq!(
            why_not(&chose(&unfelt, &[cell("s1")], &history)),
            "skiff's own pressure can't be felt, so it never eases itself"
        );
        let mut unwatched = reading(&realm, now());
        unwatched.held = None;
        assert_eq!(
            why_not(&choose(&here, &[cell("s1")], &unwatched, &history, now())),
            "no watch follows skiff's readings, so whether it stays over can't be told"
        );
    }

    // What it asked to park is taken back only once the machine is no
    // longer over, so a lone cool reading takes back nothing.
    #[test]
    fn a_machine_stays_over_until_its_readings_say_otherwise_for_five_minutes() {
        let realm = realm();
        let calm = [machine("skiff", 4.6), machine("tern", 4.6)];
        let mut history = hot_for(&realm, 11);
        assert!(history.over(), "over while its readings say so");
        let full = [machine("skiff", 86.0), machine("tern", 86.0)];
        history.read(
            "skiff",
            &reading(&full, now() + SignedDuration::from_secs(30)),
        );
        assert!(
            history.over(),
            "over with no machine to spare is still over"
        );
        history.slept(1, now());
        history.read(
            "skiff",
            &reading(&calm, now() + SignedDuration::from_mins(1)),
        );
        assert!(
            history.over(),
            "a lone reading not over after a sleep doesn't end it"
        );
        let mut untimed = reading(&calm, now());
        untimed.at = "later".to_owned();
        history.read("skiff", &untimed);
        assert!(history.over(), "nor does one whose time can't be told");
        history.read(
            "skiff",
            &reading(&calm, now() + SignedDuration::from_mins(2)),
        );
        let later = now() + COOL_FOR - SignedDuration::from_secs(1);
        history.read("skiff", &reading(&calm, later));
        assert!(
            history.over(),
            "nor do readings not over in a row for under five minutes"
        );
        history.read("skiff", &reading(&calm, now() + COOL_FOR));
        assert!(!history.over(), "five minutes of them do");
        history.read("skiff", &reading(&calm, now() + COOL_FOR * 2));
        assert!(!history.over(), "and one never over isn't");
    }

    #[test]
    fn a_machine_is_judged_by_what_its_own_configuration_says() {
        let mut tern = machine("tern", 4.6);
        tern.declared = Some(declaring(&[Kind::None]));
        let mut skiff = machine("skiff", 4.6);
        skiff.declared = Some(declaring(&[Kind::Fence]));
        let machines = [tern, skiff];
        let loads = Loads::new();
        let here = Here::of("skiff", &machines, &loads);
        assert_eq!(
            here.declared,
            machines[1].declared.as_ref(),
            "skiff, listed second, by its own facts"
        );
        assert!(Here::of("osprey", &machines, &loads).declared.is_none());
    }

    #[test]
    fn a_spell_of_five_minutes_ends_the_run_and_one_a_second_short_does_not() {
        let realm = realm();
        let calm = [machine("skiff", 4.6), machine("tern", 4.6)];
        let spell = |cool_for: SignedDuration| {
            let mut history = History::default();
            history.kept_eases(Ok(0));
            history.read("skiff", &reading(&realm, ago(20)));
            history.read("skiff", &reading(&calm, ago(10)));
            history.read("skiff", &reading(&realm, ago(10) + cool_for));
            history.read("skiff", &reading(&realm, now()));
            chose(&realm, &[cell("s1")], &history)
        };
        assert!(
            why_not(&spell(COOL_FOR)).ends_with("not 10 minutes"),
            "five minutes from the cool reading to the next ends the run"
        );
        assert!(
            carries(&spell(COOL_FOR - SignedDuration::from_secs(1))).is_some(),
            "a second short of five keeps it"
        );
    }

    #[test]
    fn a_reading_whose_time_cannot_be_told_ends_the_run() {
        let realm = realm();
        let mut history = read_all(&[(&realm, 20), (&realm, 9)]);
        let mut untimed = reading(&realm, ago(8));
        untimed.at = "later".to_owned();
        history.read("skiff", &untimed);
        assert_eq!(
            why_not(&chose(&realm, &[cell("s1")], &history)),
            "the time of skiff's latest reading can't be told, so how long it has been over \
             can't either",
            "chosen on the untimed reading itself, it says why rather than that it isn't over"
        );
        history.read("skiff", &reading(&realm, now()));
        assert!(
            why_not(&chose(&realm, &[cell("s1")], &history)).ends_with("not 10 minutes"),
            "a reading that can't say when breaks the run rather than being skipped"
        );
    }

    #[test]
    fn nothing_is_done_on_a_reading_that_does_not_itself_say_over() {
        let realm = realm();
        let calm = [machine("skiff", 4.6), machine("tern", 4.6)];
        let history = hot_for(&realm, 11);
        let declared = declaring(&[Kind::Fence]);
        let here = Here {
            name: "skiff",
            declared: Some(&declared),
            machines: &realm,
            loads: &Loads::new(),
        };
        // The watch still holds it over, as it does just after one cool
        // reading among hot ones.
        let mut lone = reading(&calm, now());
        lone.held = reading(&realm, now()).held;
        assert_eq!(
            why_not(&choose(&here, &[cell("s1")], &lone, &history, now())),
            "skiff's latest reading doesn't say it is over, so it waits for one that does"
        );
    }

    #[test]
    fn a_machine_eases_only_once_over_ten_minutes_through_short_cool_spells() {
        let realm = realm();
        assert!(
            why_not(&chose(&realm, &[cell("s1")], &hot_for(&realm, 9))).ends_with("not 10 minutes"),
            "nine minutes over isn't ten"
        );
        let calm = [machine("skiff", 4.6), machine("tern", 4.6)];
        let mut flickered = History::default();
        flickered.kept_eases(Ok(0));
        flickered.read("skiff", &reading(&realm, ago(14)));
        flickered.read("skiff", &reading(&calm, ago(9)));
        flickered.read("skiff", &reading(&realm, ago(8)));
        flickered.read("skiff", &reading(&realm, now()));
        assert!(
            carries(&chose(&realm, &[cell("s1")], &flickered)).is_some(),
            "a lone reading not over between hot ones, a minute apart as easing looks, breaks \
             nothing"
        );
        let broken = read_all(&[(&realm, 30), (&calm, 9), (&calm, 4), (&realm, 0)]);
        assert!(
            why_not(&chose(&realm, &[cell("s1")], &broken)).ends_with("not 10 minutes"),
            "readings not over for five minutes start the ten minutes again"
        );
        // Over one minute in three keeps a watch holding it over, so only
        // the run can tell such a machine from a busy one.
        let thirds = read_all(&[
            (&realm, 15),
            (&realm, 14),
            (&calm, 13),
            (&calm, 12),
            (&realm, 11),
            (&calm, 10),
            (&calm, 9),
            (&realm, 8),
            (&realm, 0),
        ]);
        assert!(
            why_not(&chose(&realm, &[cell("s1")], &thirds)).ends_with("not 10 minutes"),
            "two readings not over in a row, a minute apart, start the ten minutes again"
        );
        assert!(
            carries(&chose(&realm, &[cell("s1")], &hot_for(&realm, 10))).is_some(),
            "ten minutes over eases"
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
            with(&|one| one.orchestrates = Some(true)),
            "S1 orchestrates other sessions"
        );
        assert_eq!(
            with(&|one| one.orchestrates = None),
            "whether S1 orchestrates other sessions can't be told"
        );
        let awake_orchestrator = Cell {
            asleep: false,
            orchestrates: Some(true),
            ..cell("s1")
        };
        assert_eq!(
            why_not(&chose(&realm, &[awake_orchestrator], &history)),
            "S1 orchestrates other sessions",
            "nor is an orchestrator put to sleep, since asleep it steers nothing"
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

    // The start that brings a carried session back is an agent's, so a
    // look never chooses a machine whose fence runs no command, which the
    // carry would refuse after spending the session's move for the day.
    #[test]
    fn a_look_never_chooses_a_machine_its_start_back_cant_run_on() {
        // Tern, the roomiest, declares both fences, and one runs nothing.
        // The session runs fenced, but the start back may be either kind
        // of asker's, so either refuses tern.
        for (kind, runs_as) in [(Kind::Fence, "fenced"), (Kind::Strict, "strictly fenced")] {
            let mut alone = realm();
            let mut declared = declaring(&[Kind::Fence, Kind::Strict]);
            declared
                .fence
                .as_mut()
                .and_then(|fences| fences.get_mut(&kind))
                .expect("both fences")
                .runs
                .clear();
            alone[1].declared = Some(declared);
            let history = hot_for(&alone, 11);
            assert_eq!(
                why_not(&chose(&alone, &[cell("s1")], &history)),
                format!(
                    "tern declares no way to run a command as its {runs_as} sessions run that \
                     its launcher accepts, or its node is too old to say one"
                ),
                "with no other machine, nothing goes, saying why"
            );
            let mut beside = alone.clone();
            beside.push(machine("osprey", 9.0));
            let history = hot_for(&beside, 11);
            assert_eq!(
                carries(&chose(&beside, &[cell("s1")], &history)),
                Some(("s1", "osprey")),
                "the next machine is placed instead"
            );
        }
    }

    #[test]
    fn a_session_put_to_sleep_goes_though_its_process_has_ended() {
        let realm = realm();
        let history = hot_for(&realm, 11);
        let parked = |id: &str| {
            let mut one = cell(id);
            one.session.state = State::Ended;
            one
        };
        assert_eq!(
            carries(&chose(&realm, &[parked("s1")], &history)),
            Some(("s1", "tern")),
            "a parked session is asleep, and goes"
        );
        let mut elsewhere = parked("s1");
        elsewhere.alone = Some(false);
        assert_eq!(
            why_not(&chose(&realm, &[elsewhere], &history)),
            "S1's quest runs on other machines too"
        );
        let mut ended = parked("s1");
        ended.asleep = false;
        assert_eq!(
            why_not(&chose(&realm, &[ended], &history)),
            "S1 isn't running",
            "one ended without parking isn't asleep, and stays"
        );
    }

    #[test]
    fn the_smallest_session_to_carry_goes_first() {
        let realm = realm();
        let history = hot_for(&realm, 11);
        let sized = |id: &str, size: Option<u64>| Cell { size, ..cell(id) };
        let cells = [
            sized("s1", None),
            sized("s2", Some(900 << 10)),
            sized("s3", Some(100 << 10)),
        ];
        assert_eq!(
            carries(&chose(&realm, &cells, &history)),
            Some(("s3", "tern")),
            "the smallest record goes first, whatever order they came in"
        );
        assert_eq!(
            carries(&chose(&realm, &cells[..2], &history)),
            Some(("s2", "tern")),
            "one whose size can't be told goes after those whose can"
        );
        assert_eq!(
            carries(&chose(&realm, &cells[..1], &history)),
            Some(("s1", "tern")),
            "and goes when it is the only one"
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
    fn a_machine_carries_at_most_six_sessions_a_day() {
        let realm = realm();
        // `remembered` carries the node made, the last two hours ago, and
        // `kept` the record of carries says it made.
        let eased = |remembered: i64, kept: Result<usize, String>| {
            let mut history = hot_for(&realm, 11);
            for hour in 0..remembered {
                history.moved(Moved {
                    session: format!("s-{hour}"),
                    from: "skiff".to_owned(),
                    to: "tern".to_owned(),
                    at: ago((hour + 2) * 60),
                    eases: true,
                });
            }
            history.kept_eases(kept);
            chose(&realm, &[cell("s1")], &history)
        };
        assert!(carries(&eased(5, Ok(5))).is_some(), "five, so a sixth");
        assert_eq!(
            why_not(&eased(6, Ok(6))),
            "it carried 6 sessions within the day, and carries at most 6 a day"
        );
        assert_eq!(
            why_not(&eased(6, Ok(0))),
            "it carried 6 sessions within the day, and carries at most 6 a day",
            "a record of carries cut short still stops the seventh"
        );
        assert_eq!(
            why_not(&eased(0, Ok(6))),
            "it carried 6 sessions within the day, and carries at most 6 a day",
            "carries the record holds count though the node forgot them"
        );
        assert_eq!(
            why_not(&eased(0, Err("the record is unreadable".to_owned()))),
            "the record is unreadable"
        );
        let mut unread = History::default();
        unread.read("skiff", &reading(&realm, ago(11)));
        unread.read("skiff", &reading(&realm, now()));
        assert!(
            why_not(&chose(&realm, &[cell("s1")], &unread))
                .starts_with("the carries kept here haven't been read"),
            "a node that hasn't read the record carries nothing"
        );
        // Five within the day, and a sixth `at` ago from `from`.
        let with_sixth = |at: Timestamp, from: &str| {
            let mut history = hot_for(&realm, 11);
            for (hour, from) in (0..5).map(|hour| (hour, "skiff")).chain([(5, from)]) {
                history.moved(Moved {
                    session: format!("s-{hour}"),
                    from: from.to_owned(),
                    to: "osprey".to_owned(),
                    at: if hour == 5 { at } else { ago((hour + 2) * 60) },
                    eases: true,
                });
            }
            chose(&realm, &[cell("s1")], &history)
        };
        assert!(
            carries(&with_sixth(now() - DAY, "skiff")).is_some(),
            "a carry a day old counts no longer"
        );
        assert_eq!(
            why_not(&with_sixth(
                now() - DAY + SignedDuration::from_secs(1),
                "skiff"
            )),
            "it carried 6 sessions within the day, and carries at most 6 a day",
            "a second short of a day it still counts"
        );
        assert!(
            carries(&with_sixth(ago(60), "tern")).is_some(),
            "another machine's easing counts for none of skiff's"
        );
        let mut others = hot_for(&realm, 11);
        for hour in 0..6 {
            others.moved(Moved {
                session: format!("s-{hour}"),
                from: "skiff".to_owned(),
                to: "osprey".to_owned(),
                at: ago((hour + 2) * 60),
                eases: false,
            });
        }
        assert!(
            carries(&chose(&realm, &[cell("s1")], &others)).is_some(),
            "six carried off skiff by others count for none of its node's six"
        );
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

    // No session goes to a Mac while a start bringing it back would run
    // there held, outside any fence. Lifting this must still keep strict
    // sessions off a Mac, whose strict fence reads the rest of its home,
    // until that fence is narrowed; this tests only a plain fence.
    #[test]
    fn a_mac_with_room_is_passed_over_for_the_next_machine() {
        let mut realm = realm();
        realm[1].system = Some("macos".to_owned());
        // The Mac has clearly the most room, so were it not passed over
        // first, placement would always pick it.
        realm[1].room.as_mut().expect("room").pressure.cpu = 0.0;
        let history = hot_for(&realm, 11);
        assert_eq!(
            why_not(&chose(&realm, &[cell("s1")], &history)),
            "tern is a Mac, where a session brought back held would run outside any fence"
        );
        // Less room than the Mac, but room: under a quarter's stall.
        realm.push(machine("osprey", 20.0));
        let history = hot_for(&realm, 11);
        assert_eq!(
            carries(&chose(&realm, &[cell("s1")], &history)),
            Some(("s1", "osprey")),
            "the next machine with room takes it"
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
