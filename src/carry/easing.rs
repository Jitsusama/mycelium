//! A machine's node easing it, while it only says what it would do.
//!
//! Each look, the node reads the realm's balance, sees its sessions as
//! [`cells`], and asks [`ease::choose`] what it would do. It does none of
//! it: each choice to put sessions to sleep or carry one, and each change
//! in why it does nothing, is a line in its folder's file for the day,
//! which [`keep`] writes. What it would have done counts in memory alone:
//! a session it would have put to sleep counts as asleep, one it would
//! have carried as gone, and the terms that follow a carry hold as though
//! it had, while nothing of it reaches the record of carries.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::{SignedDuration, Timestamp};
use serde::Serialize;
use serde_json::{Value, json};

use super::chain::Orchestrations;
use super::ease::{self, Busy, Cell, Choice, Here, History, Moved};
use super::kept;
use super::kept::Kept;
use crate::balance::{Reading, Watch};
use crate::declared::Kind;
use crate::facts::Machine;
use crate::record::{Session, State};
use crate::room::Room;

/// How often a node looks, as a reading of the balance is taken.
pub const EVERY: Duration = Duration::from_secs(60);

/// How much a day's file holds, in bytes, past which that day's lines are
/// let go.
pub const DAY_HOLDS: u64 = 8 << 20;

/// How long a process's use of the processor is said over.
pub const CPU_OVER: SignedDuration = SignedDuration::from_mins(10);

/// How many looks may choose to sleep a session whose ask can't be
/// written, before it is passed over until next seen in a turn.
pub const WRITES_TRIED: usize = 3;

/// How many clock ticks a second a process's times are counted in: Linux
/// says them to programs in hundredths, whatever its own clock.
const TICKS_A_SECOND: f64 = 100.0;

/// Where a node keeps what it would do to ease its machine, and how often
/// it looks.
#[derive(Debug, Clone)]
pub struct Eases {
    /// The folder of its lines, a file each day.
    pub folder: PathBuf,
    /// How often it looks.
    pub every: Duration,
    /// Whether it makes the carries it chooses, under its own name, or
    /// only says them.
    pub carries: bool,
    /// The folder where it asks its harness to park the sessions it
    /// chooses to sleep (`carry::sleep`), or none where it only says
    /// them.
    pub sleeps: Option<PathBuf>,
    /// The workers it may carry to, by name: one that says it takes
    /// carries but isn't named here takes none from this node.
    pub onto: BTreeSet<String>,
}

/// What a node read for one look at its sessions.
#[derive(Debug, Clone, Copy)]
pub struct Looked<'a> {
    /// The realm's sessions, as the node sees them.
    pub sessions: &'a [Session],
    /// The strictest fence each session here was seen in, where that can
    /// be told.
    pub fences: &'a BTreeMap<String, Kind>,
    /// Each session's leases, as the mesh keeps them.
    pub holdings: &'a [Value],
    /// The sessions with work waiting or running in the machine's queue.
    pub queued: &'a BTreeSet<String>,
    /// The size of each session's log, by its id.
    pub sizes: &'a BTreeMap<String, u64>,
    /// The sessions that orchestrate others, by id, as [`orchestrators`]
    /// tells them; none where that can't be told.
    pub orchestrators: Option<&'a BTreeSet<String>>,
    /// The machine's boot, which a session parked in another can't be
    /// started again from.
    pub boot: Option<&'a str>,
    /// When the machine's boot began, in milliseconds since the epoch,
    /// where the system says: on a machine naming no boot, as a Mac names
    /// none, a lane is judged parked this boot by it.
    pub began: Option<i64>,
    /// When.
    pub now: Timestamp,
}

/// `here`'s sessions that run or are parked, with what `looked` tells of
/// each; what it can't tell is none.
#[must_use]
pub fn cells(here: &str, looked: &Looked) -> Vec<Cell> {
    looked
        .sessions
        .iter()
        .filter(|session| session.machine == here)
        .filter(|session| {
            matches!(session.state, State::Idle | State::Busy)
                || super::parked(session, looked.boot)
        })
        .map(|session| {
            let recorded = looked
                .orchestrators
                .map(|named| named.contains(id(session)) || named.contains(&session.id));
            // The record names who it can, and a lane's word only adds
            // to it, so a lane where the record can't be read still says
            // the session orchestrates, and one whose standing can't be
            // told leaves it untold, which keeps it here.
            let word = match recorded {
                Some(true) => None,
                _ => leads(here, looked, session),
            };
            let (orchestrates, lane) = match word {
                Some(Word::Counts(_, lane)) => (Some(true), Some(lane)),
                Some(Word::Untold(lane)) => (None, Some(lane)),
                None => (recorded, None),
            };
            Cell {
                session: session.clone(),
                fence: looked.fences.get(id(session)).copied(),
                alone: alone(here, looked.sessions, session),
                on_screen: on_screen(session),
                driven: session.presence.get("driven").and_then(Value::as_bool),
                asleep: super::parked(session, looked.boot),
                orchestrates,
                lane,
                busy: busy(session, looked),
                size: looked.sizes.get(id(session)).copied(),
            }
        })
        .collect()
}

/// The session's own id, as its presence, its leases and its log name
/// it.
#[must_use]
pub fn id(session: &Session) -> &str {
    super::session_id(session).unwrap_or(&session.id)
}

/// The realm's `machines` with `here`'s room the one its node `felt`
/// ([`crate::room::felt`]), so whether `here` is over is judged only from
/// what the node measures itself, never from a record a session can
/// write. Where it felt none, `here` has no room, so it is never over. A
/// machine within another has none either, since its own kernel doesn't
/// see what its host's work presses on it. The other machines' rooms are
/// as their own nodes said them over the link.
#[must_use]
pub fn felt_here(mut machines: Vec<Machine>, here: &str, felt: Option<Room>) -> Vec<Machine> {
    if let Some(machine) = machines.iter_mut().find(|machine| machine.name == here) {
        let within = machine
            .declared
            .as_ref()
            .is_some_and(|declared| declared.within.is_some());
        machine.room = felt.filter(|_| !within);
    }
    machines
}

/// The sessions, by id, that orchestrate others: each that this node's
/// `record` of orchestrations holds a claim for, and each the quests'
/// records, `quests`, name over another quest.
///
/// # Errors
///
/// Why, where the node's record or the quests' can't be read, since then
/// who orchestrates can't be told.
pub fn orchestrators(
    record: Result<&Orchestrations, String>,
    quests: Result<&[Value], String>,
) -> Result<BTreeSet<String>, String> {
    let mut named = super::ordered::orchestrating(quests?);
    // A claim naming no quest orchestrates nobody: any session may make
    // one, and would stay put by it for good.
    named.extend(
        record?
            .sessions
            .iter()
            .filter(|(_, claim)| !claim.quests.is_empty())
            .map(|(session, _)| session.clone()),
    );
    Ok(named)
}

/// What a look reads of the node's own records, to tell who orchestrates
/// and how many sessions it carried.
#[derive(Debug)]
pub struct Records {
    /// The sessions that orchestrate others, as [`orchestrators`] tells
    /// them, or why that can't be told.
    pub orchestrators: Result<BTreeSet<String>, String>,
    /// How many carries the record of carries holds as this node's within
    /// the day, as [`eased`] counts them, or why that can't be told.
    pub kept_eases: Result<usize, String>,
}

/// The [`Records`] `here`'s node reads at `now`: its record of
/// orchestrations at `orchestrations`, the quests' records it serves,
/// `quests` (or why it has none read whole), and its record of carries
/// at `carries`.
#[must_use]
pub fn records(
    orchestrations: &Path,
    quests: Result<&[Value], String>,
    carries: &Path,
    here: &str,
    now: Timestamp,
) -> Records {
    let record = Orchestrations::read(orchestrations)
        .map_err(|error| format!("the node's record of orchestrations can't be read ({error})"));
    let kept_eases = kept::read(carries, now - ease::DAY)
        .map(|kept| eased(&kept, here, now))
        .map_err(|error| {
            format!(
                "the carries kept here can't be read ({error}), so how many {here} carried \
                 within the day can't be told"
            )
        });
    Records {
        orchestrators: orchestrators(record.as_ref().map_err(Clone::clone), quests),
        kept_eases,
    }
}

/// The carries `here`'s node said in its own lines in `folder`, its
/// [`keep`]ing, that it made or would have made within the day before
/// `now`, so a restart forgets none of them though the record of
/// carries be cut short. Each day's file from yesterday's on is read, a
/// day ahead's too, since a clock set back leaves one; a day with no
/// file holds none, and a line it can't read, torn part way into a
/// letter or not, counts for none.
///
/// # Errors
///
/// Why, while the folder or a day's file is there but can't be read,
/// since a restart that took it for none could carry past
/// [`ease::DAILY`].
pub fn eased_lately(folder: &Path, here: &str, now: Timestamp) -> Result<Vec<Moved>, String> {
    let today = now.to_zoned(jiff::tz::TimeZone::UTC).date();
    let from = today.yesterday().unwrap_or(today);
    let cant = |path: &Path, error: &io::Error| {
        format!(
            "{} can't be read ({error}), so how many {here} carried within the day can't be told",
            path.display()
        )
    };
    let mut days = BTreeSet::new();
    match fs::read_dir(folder) {
        Ok(entries) => {
            for entry in entries {
                // An entry that can't be read may be a day's file.
                let entry = entry.map_err(|error| cant(folder, &error))?;
                let Ok(name) = entry.file_name().into_string() else {
                    continue;
                };
                if name
                    .strip_suffix(".jsonl")
                    .and_then(|day| day.parse::<jiff::civil::Date>().ok())
                    .is_some_and(|day| day >= from)
                {
                    days.insert(name);
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(cant(folder, &error)),
    }
    let mut texts = Vec::new();
    for day in days {
        let file = folder.join(day);
        match fs::read(&file) {
            Ok(bytes) => texts.push(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(cant(&file, &error)),
        }
    }
    Ok(texts
        .into_iter()
        .flat_map(|text| {
            kept::lines_of(&text)
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .collect::<Vec<_>>()
        })
        .filter(|line| line["choice"] == "carry")
        .filter_map(|line| {
            let at = line["at"].as_str()?.parse::<Timestamp>().ok()?;
            Some(Moved {
                session: line["session"].as_str()?.to_owned(),
                from: here.to_owned(),
                to: line["to"].as_str()?.to_owned(),
                at,
                eases: true,
            })
        })
        .filter(|moved| now.duration_since(moved.at) < ease::DAY)
        .collect())
}

/// The carries of `kept` this machine's node, `here`'s, made to ease it
/// that haven't landed yet, as `judge` says each stands: still told to
/// its session, or gone from here and not yet running where it went.
/// One refused, arrived, back or stuck holds nothing back, so a carry
/// that never lands holds easing no longer than it stands ordered.
pub fn on_way(kept: &[Kept], here: &str, judge: impl Fn(&Kept) -> kept::State) -> Vec<Moved> {
    let by = ease::by(here);
    kept.iter()
        .filter(|one| one.said("by") == by && one.said("from") == here)
        .filter(|one| matches!(judge(one), kept::State::Ordered | kept::State::Carrying))
        .map(|one| Moved {
            session: one.said("session").to_owned(),
            from: here.to_owned(),
            to: one.said("to").to_owned(),
            at: one.at,
            eases: true,
        })
        .collect()
}

/// How many of the carries `kept` within the day before `now` this
/// machine's node, `here`'s, made to ease it.
#[must_use]
pub fn eased(kept: &[Kept], here: &str, now: Timestamp) -> usize {
    let by = ease::by(here);
    kept.iter()
        .filter(|one| one.said("by") == by && one.said("from") == here)
        .filter(|one| now.duration_since(one.at) < ease::DAY)
        .count()
}

/// Whether the session's quest runs only `here`; none without a quest.
fn alone(here: &str, sessions: &[Session], session: &Session) -> Option<bool> {
    let quest = session.quest.as_deref()?;
    Some(super::runs_on(sessions, quest).is_ok_and(|on| on == here))
}

/// Another of the realm's sessions that says in its presence that
/// `session` orchestrates it, by an address `session` goes by, in any
/// case, as the reason line names it: its address, and where it stands if
/// it doesn't run. The records of who orchestrates name only the quests an
/// orchestrator claims, so a lane it started for a quest of the lane's own
/// is told only by the lane.
///
/// A lane counts while it runs, while it waits parked here this boot, as
/// its orchestrator is no less one for a lane at rest, and for
/// [`OUT_OF_REACH_LEADS`] of this node seeing its machine out of reach,
/// which says nothing of whether it still runs. A lane parked elsewhere
/// never reaches this node, whose links send only the sessions that
/// live. Of several lanes, one that runs is named first, then one parked,
/// then one out of reach, as the likeliest to be so.
///
/// Parked this boot is told by the boot its presence names, where both it
/// and the machine name one; otherwise by whether its machine wrote it
/// since the boot began, as the node's standing of each session judges a
/// park on a Mac. Out of reach is timed as [`OUT_OF_REACH_LEADS`] says,
/// by times this node's store writes on its own clock, never by when the
/// lane's machine says it last wrote it, whose clock may be another's and
/// which an idle lane leaves old. Where a lane's standing can't be
/// told, and no other lane counts, its word is [`Word::Untold`], which
/// keeps its orchestrator here as anything unknown of a cell does.
///
/// The lane's word is believed though nothing proves it, as every reason
/// a presence gives to keep a session here is, because all it can do is
/// keep one here: a lane naming the wrong session costs its machine an
/// easing, never the session its place. A claim naming no quest is
/// refused because any session may make one of itself, where a lane's
/// word is another session's, and one naming itself leads nobody.
fn leads(here: &str, looked: &Looked, session: &Session) -> Option<Word> {
    looked
        .sessions
        .iter()
        .filter(|other| other.id != session.id)
        .filter(|other| {
            other
                .orchestrated_by
                .as_deref()
                .is_some_and(|said| session.goes_by(said))
        })
        .filter_map(|other| lane_word(here, looked, other))
        // Any lane that counts before one untold, and the first of the
        // likeliest.
        .min_by_key(|word| match word {
            Word::Counts(likely, _) => *likely,
            Word::Untold(_) => u8::MAX,
        })
}

/// What a lane's word says of the session it names, as [`leads`] weighs
/// it.
#[derive(Debug, PartialEq, Eq)]
enum Word {
    /// It leads the lane, as likely as the rank says (0 running, 1 parked,
    /// 2 out of reach), named as the reason line names it.
    Counts(u8, String),
    /// It may lead the lane named, whose standing can't be told.
    Untold(String),
}

/// The word of `lane`, one of the realm's sessions saying it answers to
/// another, or none where its word doesn't count.
fn lane_word(here: &str, looked: &Looked, lane: &Session) -> Option<Word> {
    let who = &lane.address;
    match lane.state {
        State::Busy | State::Idle => Some(Word::Counts(0, who.clone())),
        State::Ended if lane.machine != here => None,
        State::Ended => match parked_this_boot(lane, looked) {
            Some(true) => Some(Word::Counts(1, format!("{who}, parked"))),
            Some(false) => None,
            None => Some(Word::Untold(format!(
                "{who}, parked, though whether in this boot can't be told"
            ))),
        },
        State::Unreachable => {
            let away = format!("{who}, out of reach on {}", lane.machine);
            let out = seen_out_of_reach(&lane.presence, looked.now.as_millisecond());
            // A drop this node's clock has since set behind it is recent.
            match out.map(SignedDuration::from_millis) {
                Some(out) if out < OUT_OF_REACH_LEADS => Some(Word::Counts(2, away)),
                Some(_) => None,
                None => Some(Word::Untold(format!(
                    "{away}, though since when can't be told"
                ))),
            }
        }
    }
}

/// How long after this node lost the link to its machine a lane out of
/// reach still says its orchestrator leads it. A link drops for minutes,
/// as a machine's network moves, and that is what the lane's word is kept
/// through; a machine gone for longer, closed or retired, may never leave
/// the realm's records, so without a bound its lane would keep its
/// orchestrator here for good.
///
/// The hour is of time a running node here has seen the machine gone, as
/// [`seen_out_of_reach`] reads it, never of time the node was down: a node
/// started after longer down marks each machine no link carries out of
/// reach since it was last linked, hours back, though it saw none of that.
/// What earlier runs saw is kept, so a node restarted again and again
/// still lets a gone machine's lanes go once it has seen them gone an hour
/// in all. A node killed rather than stopped can't put its time seen
/// away, so the time it is down counts as seen, as does a sleep of this
/// machine while the link is down.
pub const OUT_OF_REACH_LEADS: SignedDuration = SignedDuration::from_hours(1);

/// How long, in milliseconds, this node has seen a lane's machine out of
/// reach by `now`, from the marks its store keeps in the lane's presence:
/// what earlier runs saw, `unreachableSeenFor`, and since when this run has
/// seen it, `unreachableSeen`. A seen time this node's clock has since set
/// behind it adds nothing, and takes nothing from what earlier runs saw. A
/// mark this run hasn't seen, as one a link running apart from the node
/// left, or one its start couldn't mark again, is timed from its
/// `unreachableSince`, as an older node's is, so a doubt lets the lane go
/// rather than holding it; one with no time at all can't be told. Such a
/// mark is seen by the next start, so a lane let go meanwhile may count
/// again from then for what is left of the hour.
fn seen_out_of_reach(presence: &Value, now: i64) -> Option<i64> {
    let earlier = presence.get("unreachableSeenFor").and_then(Value::as_i64);
    match presence.get("unreachableSeen").and_then(Value::as_i64) {
        Some(seen) => Some(
            earlier
                .unwrap_or(0)
                .saturating_add(now.saturating_sub(seen).max(0)),
        ),
        None => presence
            .get("unreachableSince")
            .and_then(Value::as_i64)
            .map(|since| now.saturating_sub(since))
            .max(earlier),
    }
}

/// Whether `session` is parked and was parked in the machine's present
/// boot, by the boot its presence names or else by when its machine last
/// wrote it; none where it is parked but neither can be told.
fn parked_this_boot(session: &Session, looked: &Looked) -> Option<bool> {
    if !super::parked(session, None) {
        return Some(false);
    }
    let named = session
        .presence
        .pointer("/owner/bootId")
        .and_then(Value::as_str);
    if let (Some(now), Some(began)) = (looked.boot, named) {
        return Some(now == began);
    }
    let began = looked.began?;
    written(session).map(|at| at.as_millisecond() >= began)
}

/// When the session's machine last wrote its presence, where its record
/// says.
fn written(session: &Session) -> Option<Timestamp> {
    session.at.parse().ok()
}

/// Whether the session is on the person's screen: not, where a host runs
/// it in the background, and can't be told otherwise.
fn on_screen(session: &Session) -> Option<bool> {
    session
        .host
        .as_deref()
        .is_some_and(|host| host.ends_with("-background"))
        .then_some(false)
}

/// What the session is part way through, which can be told only once its
/// harness marks each background run it keeps with a lease.
fn busy(session: &Session, looked: &Looked) -> Option<Vec<Busy>> {
    if session.presence.pointer("/leases/background") != Some(&Value::Bool(true)) {
        return None;
    }
    let id = id(session);
    let mut busy = Vec::new();
    if looked.queued.contains(id) || looked.queued.contains(&session.id) {
        busy.push(Busy::HeavyRun);
    }
    let leases = looked
        .holdings
        .iter()
        .filter(|held| held.get("session").and_then(Value::as_str) == Some(id))
        .filter_map(|held| held.get("leases").and_then(Value::as_array))
        .flatten();
    for lease in leases {
        let until = lease
            .get("until")
            .and_then(Value::as_str)
            .and_then(|until| until.parse::<Timestamp>().ok());
        if until.is_some_and(|until| until <= looked.now) {
            continue;
        }
        busy.push(match lease.get("kind").and_then(Value::as_str) {
            Some("heavy") => Busy::HeavyRun,
            _ => Busy::Lease,
        });
    }
    Some(busy)
}

/// The size in bytes of the log of each of `ids` in `logs`, pi's folder
/// of a folder of logs for each working folder, where it has one.
#[must_use]
pub fn sizes(logs: &Path, ids: &BTreeSet<String>) -> BTreeMap<String, u64> {
    let mut sizes = BTreeMap::new();
    let folders = fs::read_dir(logs).into_iter().flatten().flatten();
    for log in
        folders.flat_map(|folder| fs::read_dir(folder.path()).into_iter().flatten().flatten())
    {
        let name = log.file_name();
        let Some((_, session)) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".jsonl"))
            .and_then(|name| name.rsplit_once('_'))
        else {
            continue;
        };
        if let (true, Ok(meta)) = (ids.contains(session), log.metadata()) {
            let size = sizes.entry(session.to_owned()).or_insert(0);
            *size = meta.len().max(*size);
        }
    }
    sizes
}

/// What a session's process holds of the machine, as far as can be told.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Holds {
    /// Its memory resident, in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<u64>,
    /// Its use of the processor over the last [`CPU_OVER`], in cores.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<f64>,
}

/// Each process's processor time as last counted, to say its use over
/// [`CPU_OVER`].
#[derive(Debug, Default)]
pub struct Sampler {
    counted: BTreeMap<i32, VecDeque<(Timestamp, u64)>>,
}

impl Sampler {
    /// What each of `pids`, by session, holds now, as `procs`, Linux's
    /// `/proc` or a stand-in, tells it; forgets each process no longer
    /// asked of.
    pub fn take(
        &mut self,
        procs: &Path,
        pids: &BTreeMap<String, i32>,
        now: Timestamp,
    ) -> BTreeMap<String, Holds> {
        self.counted
            .retain(|pid, _| pids.values().any(|asked| asked == pid));
        pids.iter()
            .map(|(session, &pid)| {
                let cpu = ticks(procs, pid).and_then(|ticks| self.used(pid, ticks, now));
                let memory = resident(procs, pid);
                (session.clone(), Holds { memory, cpu })
            })
            .collect()
    }

    /// The cores process `pid` used since the newest count at least
    /// [`CPU_OVER`] old, or the oldest kept, now it has counted `ticks`.
    fn used(&mut self, pid: i32, ticks: u64, now: Timestamp) -> Option<f64> {
        let counted = self.counted.entry(pid).or_default();
        counted.push_back((now, ticks));
        while counted
            .get(1)
            .is_some_and(|&(at, _)| now.duration_since(at) >= CPU_OVER)
        {
            counted.pop_front();
        }
        let &(since, then) = counted.front()?;
        let seconds = now.duration_since(since).as_secs_f64();
        let spent = ticks.checked_sub(then)?;
        #[expect(
            clippy::cast_precision_loss,
            reason = "ticks spent over ten minutes stay far below where a float loses any"
        )]
        let spent = spent as f64;
        (seconds > 0.0).then(|| (spent / TICKS_A_SECOND / seconds * 100.0).round() / 100.0)
    }
}

/// The user and system time process `pid` has run, in clock ticks: its
/// `stat`'s fourteenth and fifteenth fields, the twelfth and thirteenth
/// after its name.
fn ticks(procs: &Path, pid: i32) -> Option<u64> {
    let stat = fs::read_to_string(procs.join(pid.to_string()).join("stat")).ok()?;
    let (_, after) = stat.rsplit_once(')')?;
    let mut fields = after.split_whitespace().skip(11);
    let user: u64 = fields.next()?.parse().ok()?;
    let system: u64 = fields.next()?.parse().ok()?;
    user.checked_add(system)
}

/// The memory process `pid` holds resident, in bytes, as its `status`
/// says in kibibytes.
fn resident(procs: &Path, pid: i32) -> Option<u64> {
    let status = fs::read_to_string(procs.join(pid.to_string()).join("status")).ok()?;
    let line = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?;
    let kib: u64 = line.trim().strip_suffix("kB")?.trim().parse().ok()?;
    kib.checked_mul(1024)
}

/// What a node easing its machine remembers between looks: its watch of
/// the readings, its history, and what it would have done.
#[derive(Debug, Default)]
pub struct Easing {
    watch: Watch,
    history: History,
    /// Each session put to sleep, or chosen to be, until it is next seen
    /// in a turn or its ask is taken back.
    asleep: BTreeSet<String>,
    /// How many times each session's ask couldn't be written since it
    /// was last seen in a turn.
    unwritten: BTreeMap<String, usize>,
    /// Whether the node asks the harness to park what it chooses to
    /// sleep, so that only those it asked count as put to sleep.
    acts_on_sleep: bool,
    /// Each session a look chose to carry, with when.
    gone: BTreeMap<String, Timestamp>,
    /// Each session whose carry was refused as it was told, with when,
    /// until [`Easing::on_way`] finds it older than any carry that can
    /// still be on its way.
    refused: BTreeMap<String, Timestamp>,
    /// The reason of the last line kept for doing nothing, with the doubt
    /// it said.
    said: Option<(String, Option<String>)>,
    /// Why who orchestrates can't be told, while it can't.
    doubt: Option<String>,
}

impl Easing {
    /// One remembering each carry `kept`, so that a session moved stays
    /// put, none goes back where one came from, and the carries a node
    /// made to ease its machine count, across the node's restarts; and
    /// each carry its own lines say it made, `eased`
    /// ([`eased_lately`]), but those the record already holds as its
    /// node's: a session is carried to ease at most once within the day
    /// it stays pinned, so one session is one carry.
    #[must_use]
    pub fn after(kept: &[Kept], eased: Vec<Moved>) -> Self {
        let mut history = History::default();
        // By the machine it left, so one about another machine's node
        // hides none of this one's; and only one remembered, so one the
        // record can't say whole hides none either.
        let mut recorded: BTreeSet<(&str, &str)> = BTreeSet::new();
        for one in kept {
            let (session, from, to) = (one.said("session"), one.said("from"), one.said("to"));
            if [session, from, to].iter().any(|said| said.is_empty()) {
                continue;
            }
            if one.said("by") == ease::by(from) {
                recorded.insert((session, from));
            }
            history.moved(Moved {
                session: session.to_owned(),
                from: from.to_owned(),
                to: to.to_owned(),
                at: one.at,
                eases: one.said("by") == ease::by(from),
            });
        }
        for moved in eased {
            if !recorded.contains(&(moved.session.as_str(), moved.from.as_str())) {
                history.moved(moved);
            }
        }
        Self {
            history,
            ..Self::default()
        }
    }

    /// Says the node asks the harness to park the sessions its looks
    /// choose to sleep, so a look no longer counts them put to sleep
    /// itself; [`Easing::slept`] counts those asked.
    pub fn acts_on_sleep(&mut self) {
        self.acts_on_sleep = true;
    }

    /// Notes `asked` sessions put to sleep `at`, where any were, so the
    /// next sleep waits its settling only after one; and passes over
    /// no more those in `retry`, by their record's id, whose ask
    /// couldn't be written, so a later look may choose them again, but
    /// only [`WRITES_TRIED`] times, so a folder that always refuses
    /// stops no carry. One refused for good, such as one kept by no
    /// holder, stays passed over until it is seen in a turn, so looks
    /// move on past it.
    pub fn slept(&mut self, asked: usize, retry: &[String], at: Timestamp) {
        for id in retry {
            let tried = self.unwritten.entry(id.clone()).or_default();
            *tried += 1;
            if *tried < WRITES_TRIED {
                self.asleep.remove(id);
            }
        }
        if asked > 0 {
            self.history.slept(asked, at);
        }
    }

    /// How many sessions it has put to sleep.
    #[must_use]
    pub fn sleeps(&self) -> usize {
        self.history.sleeps()
    }

    /// Forgets `woken`, by their record's id, as asleep, their asks taken
    /// back once the machine was no longer short of memory, so a later
    /// spell short may ask for them again.
    pub fn woken(&mut self, woken: &BTreeSet<String>) {
        self.asleep.retain(|id| !woken.contains(id));
    }

    /// Whether its last look still held the machine short of memory, as
    /// [`History::still_short`] says.
    #[must_use]
    pub fn still_short(&self) -> bool {
        self.history.still_short()
    }

    /// Takes `why` who orchestrates can't be told, or none once it can,
    /// for the lines that follow to say.
    pub fn doubt(&mut self, why: Option<String>) {
        self.doubt = why;
    }

    /// Takes the carries its node made to ease the machine that haven't
    /// landed yet, as [`on_way`] says, for the looks that follow.
    /// One refused as its session was told of it, as [`Easing::refused`]
    /// notes, isn't, though its record still stands ordered.
    ///
    /// None ordered longer than [`kept::STUCK_AFTER`] before `now` is
    /// on its way, as it stands stuck, so a refusal from before then
    /// can drop none of them and is forgotten.
    pub fn on_way(&mut self, mut on_way: Vec<Moved>, now: Timestamp) {
        let oldest = now - kept::STUCK_AFTER;
        self.refused.retain(|_, refused| *refused >= oldest);
        on_way.retain(|one| {
            self.refused
                .get(&one.session)
                .is_none_or(|refused| *refused < one.at)
        });
        self.history.on_way(on_way);
    }

    /// Notes that the carry of `session` its look chose was refused as
    /// the session was told of it, `at`, so that carry holds no other
    /// once the cooldown is out; a later carry of it still does. A
    /// restart forgets it, so the carry holds again until it is stuck.
    /// [`Easing::on_way`] forgets it once it is older than
    /// [`kept::STUCK_AFTER`], when its carry stands stuck anyway.
    pub fn refused(&mut self, session: &str, at: Timestamp) {
        self.refused.insert(session.to_owned(), at);
    }

    /// What `here` would do now of its `cells`, by `reading`, as the line
    /// to keep for it; none where it would do nothing for the same reason
    /// as last. A carry's line says what the session `holds` of the
    /// machine, where that can be told. `kept_eases` is how many carries
    /// the record of carries says `here`'s node made within the day, as
    /// [`eased`] counts them, or why that can't be told; the larger of
    /// it and what the node remembers is what counts against
    /// [`ease::DAILY`], so a record cut short lifts nothing.
    pub fn look(
        &mut self,
        here: &Here,
        mut reading: Reading,
        cells: Vec<Cell>,
        holds: &BTreeMap<String, Holds>,
        kept_eases: Result<usize, String>,
        now: Timestamp,
    ) -> Option<Value> {
        reading.held = Some(self.watch.take(&reading));
        self.history.read(here.name, &reading);
        self.history.kept_eases(kept_eases);
        // A session in a turn is awake, whatever it would have been put
        // to sleep for.
        for cell in &cells {
            if cell.session.state == State::Busy {
                self.asleep.remove(&cell.session.id);
                self.unwritten.remove(&cell.session.id);
            }
        }
        // One chosen to carry is passed over while its carry pins it, and
        // may be chosen again once that is past, though its carry was
        // refused and it never left.
        self.gone
            .retain(|_, chosen| now.duration_since(*chosen) < ease::PIN);
        let cells: Vec<Cell> = cells
            .into_iter()
            .filter(|cell| !self.gone.contains_key(&cell.session.id))
            .map(|mut cell| {
                cell.asleep |= self.asleep.contains(&cell.session.id);
                cell
            })
            .collect();
        let choice = ease::choose(here, &cells, &reading, &self.history, now);
        let mut line = json!({"at": now.to_string(), "would": true});
        match &choice {
            Choice::Nothing { why } => {
                let said = (why.clone(), self.doubt.clone());
                if self.said.as_ref() == Some(&said) {
                    return None;
                }
                self.said = Some(said);
            }
            Choice::Sleep { sessions, why } => {
                // A sleep that says why no carry went has said it, as a
                // line doing nothing for that reason would have.
                self.said = why.clone().map(|why| (why, self.doubt.clone()));
                self.asleep.extend(sessions.iter().cloned());
                if !self.acts_on_sleep {
                    self.history.slept(sessions.len(), now);
                }
            }
            Choice::Carry { session, to, .. } => {
                self.said = None;
                self.gone.insert(session.clone(), now);
                self.history.moved(Moved {
                    session: session.clone(),
                    from: here.name.to_owned(),
                    to: to.clone(),
                    at: now,
                    eases: true,
                });
                if let Some(holds) = holds.get(session) {
                    line["holds"] = json!(holds);
                }
            }
        }
        if let (Value::Object(line), Ok(Value::Object(chose))) =
            (&mut line, serde_json::to_value(&choice))
        {
            line.extend(chose);
            line.entry("reading").or_insert_with(|| json!(reading));
        }
        // A cell says only that who orchestrates can't be told; the line
        // says why, for whoever reads it to put right.
        if let Some(why) = &self.doubt {
            line["orchestrators_unknown"] = json!(why);
        }
        Some(line)
    }
}

/// Says where a carry the node chose to ease its machine came from, for
/// its record.
pub const SOURCE: &str = "this node's own reading of its machine's room, against the realm's \
                          rooms as its links carry them";

/// A carry a look chose, to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    /// The session, by id.
    pub session: String,
    /// The machine it goes to.
    pub to: String,
    /// Why, in one line.
    pub why: String,
}

/// The carry `line` chose, where it chose one.
#[must_use]
pub fn chosen(line: &Value) -> Option<Chosen> {
    if line["choice"] != "carry" {
        return None;
    }
    let text = |key: &str| line[key].as_str().map(str::to_owned);
    Some(Chosen {
        session: text("session")?,
        to: text("to")?,
        why: text("why")?,
    })
}

/// Says on `line` how acting on its carry went: `would` false where the
/// node acted, with why it `refused` where the carry didn't reach the
/// session.
pub fn acted(line: &mut Value, done: &Result<(), String>) {
    line["would"] = json!(false);
    if let Err(why) = done {
        line["refused"] = json!(why);
    }
}

/// Keeps `line`, said `at`, in `folder`'s file for its day, the folder
/// and file each its owner's alone; let go, answering false, where the
/// day's file would hold more than `holds` bytes with it, unless the line
/// carries a session, which a restart counts by ([`eased_lately`]). A
/// line kept after one cut short starts a line of its own. A new day's
/// file lets go of the days a fortnight before it, as placements do.
///
/// # Errors
///
/// Where the folder can't be made or the line can't be written.
pub fn keep(folder: &Path, at: Timestamp, line: &Value, holds: u64) -> io::Result<bool> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(folder)?;
    fs::set_permissions(folder, fs::Permissions::from_mode(0o700))?;
    let day = at.to_zoned(jiff::tz::TimeZone::UTC).date();
    let file = folder.join(format!("{day}.jsonl"));
    if !file.exists() {
        crate::placements::let_go_before(folder, day);
    }
    let mut text = serde_json::to_string(line)?;
    text.push('\n');
    let had = fs::metadata(&file).map_or(0, |meta| meta.len());
    if line["choice"] != "carry" && had.saturating_add(text.len() as u64) > holds {
        return Ok(false);
    }
    let mut kept = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .mode(0o600)
        .open(&file)?;
    if kept::cut_short(&mut kept)? {
        text.insert(0, '\n');
    }
    kept.write_all(text.as_bytes())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;
    use crate::balance;
    use crate::balance::Flag;
    use crate::carry::chain;
    use crate::carry::ease::tests::{NOW, ago, cell, declaring, now, realm, short_realm};
    use crate::placement::Loads;
    use crate::room::{Limit, Pressure, Takes};

    /// A room its node felt, at processor pressure `cpu`.
    fn felt(cpu: f64) -> Room {
        Room {
            said_at: NOW.to_owned(),
            memory_available: 40 << 30,
            pressure: Pressure {
                cpu,
                memory: 0.0,
                io: 0.0,
            },
            takes: Takes {
                builds: 0,
                vm_tests: 0,
            },
            over: Some(if cpu >= balance::OVER_CPU_PRESSURE {
                vec![Limit::Cpu]
            } else {
                Vec::new()
            }),
            saturated: None,
            cpu_minute: None,
            memory_full_minute: None,
        }
    }

    /// Whether `machines` read with `sessions` hold skiff hot.
    fn skiff_hot(machines: &[Machine], sessions: &[Session]) -> bool {
        balance::read(machines, sessions, now())
            .flags
            .iter()
            .any(|flag| matches!(flag, Flag::HotWhileCool { hot, .. } if hot == "skiff"))
    }

    #[test]
    fn a_planted_record_saying_over_moves_nothing() {
        let planted = realm();
        assert!(skiff_hot(&planted, &[]), "as its queue's record says it");
        let machines = felt_here(planted, "skiff", Some(felt(4.6)));
        assert!(
            !skiff_hot(&machines, &[]),
            "skiff's node felt it calm, whatever the record said"
        );
    }

    #[test]
    fn a_planted_record_saying_calm_stops_nothing() {
        let mut planted = realm();
        planted[0].room = realm()[1].room.clone();
        assert!(!skiff_hot(&planted, &[]), "as its queue's record says it");
        let machines = felt_here(planted, "skiff", Some(felt(86.0)));
        assert!(
            skiff_hot(&machines, &[]),
            "skiff's node felt it over, whatever the record said"
        );
        assert_eq!(
            machines[1].room,
            realm()[1].room,
            "tern's room is as its own node said it"
        );
    }

    #[test]
    fn a_machine_its_node_cant_feel_is_never_over() {
        let machines = felt_here(realm(), "skiff", None);
        assert_eq!(machines[0].room, None);
        assert!(!skiff_hot(&machines, &[]));
    }

    #[test]
    fn a_machine_within_another_is_never_felt() {
        let mut machines = realm();
        let declared = machines[0].declared.get_or_insert_default();
        declared.within = Some(crate::declared::Within {
            machine: "tern".to_owned(),
            file: "/nowhere/within.json".into(),
        });
        let machines = felt_here(machines, "skiff", Some(felt(86.0)));
        assert_eq!(
            machines[0].room, None,
            "its kernel doesn't see its host's work"
        );
    }

    /// Presences say only busy or idle, which balance never judges a
    /// machine over by; this keeps it so.
    #[test]
    fn planted_presences_move_nothing() {
        let busy: Vec<Session> = (0..64)
            .map(|n| session(&format!("s{n}"), "skiff", &json!({"state": "busy"})))
            .collect();
        let machines = felt_here(realm(), "skiff", Some(felt(4.6)));
        assert!(
            !skiff_hot(&machines, &busy),
            "sessions saying they're busy don't make skiff over"
        );
    }

    /// Two hours before [`NOW`], an hour before the boot [`began`].
    const EARLIER: &str = "2026-11-16T06:00:00Z";

    /// When the machine's boot began: an hour before [`NOW`].
    fn began() -> i64 {
        "2026-11-16T07:00:00Z"
            .parse::<Timestamp>()
            .expect("a time")
            .as_millisecond()
    }

    fn session(id: &str, machine: &str, more: &Value) -> Session {
        let mut said = json!({
            "id": id, "address": id.to_uppercase(), "machine": machine,
            "groups": [], "state": "idle", "at": NOW,
            "presence": {"session": id},
        });
        if let (Value::Object(said), Value::Object(more)) = (&mut said, more) {
            for (key, value) in more {
                said.insert(key.clone(), value.clone());
            }
        }
        serde_json::from_value(said).expect("a session")
    }

    /// A presence of `id`'s as this node's store marks it once the link to
    /// its machine dropped `mins` minutes before [`NOW`], while it ran.
    fn dropped(id: &str, mins: i64) -> Value {
        let at = ago(mins).as_millisecond();
        json!({"session": id, "unreachableSince": at, "unreachableSeen": at})
    }

    fn told(id: &str) -> Value {
        json!({"session": id, "leases": {"background": true}, "driven": false})
    }

    #[test]
    fn a_cell_says_only_what_the_node_can_tell_of_its_session() {
        let sessions = [
            session(
                "s1",
                "skiff",
                &json!({"quest": "q1", "host": "work-background", "presence": told("s1")}),
            ),
            session("s2", "skiff", &json!({"quest": "q2"})),
            session("s3", "tern", &json!({"quest": "q2"})),
            session("s4", "skiff", &json!({"state": "ended"})),
            session(
                "s5",
                "skiff",
                &json!({"state": "ended", "presence": {"session": "s5", "state": "parked"}}),
            ),
            session("s6", "skiff", &json!({"presence": told("s6")})),
        ];
        let holdings = [
            json!({"session": "s1", "leases": [
                {"kind": "change", "until": ago(1).to_string()},
                {"kind": "heavy", "until": (now() + SignedDuration::from_mins(5)).to_string()},
            ]}),
            json!({"session": "s2", "leases": [{"kind": "answer"}]}),
        ];
        let looked = Looked {
            sessions: &sessions,
            fences: &[("s1".to_owned(), Kind::Fence)].into(),
            holdings: &holdings,
            queued: &["s6".to_owned()].into(),
            sizes: &[("s2".to_owned(), 512)].into(),
            orchestrators: None,
            boot: None,
            began: None,
            now: now(),
        };
        let cells = cells("skiff", &looked);
        let told: Vec<_> = cells
            .iter()
            .map(|cell| {
                (
                    cell.session.id.as_str(),
                    cell.fence,
                    cell.alone,
                    cell.on_screen,
                    cell.driven,
                    cell.asleep,
                    cell.busy.clone(),
                    cell.size,
                )
            })
            .collect();
        assert_eq!(
            told,
            [
                (
                    "s1",
                    Some(Kind::Fence),
                    Some(true),
                    Some(false),
                    Some(false),
                    false,
                    Some(vec![Busy::HeavyRun]),
                    None
                ),
                ("s2", None, Some(false), None, None, false, None, Some(512)),
                ("s5", None, None, None, None, true, None, None),
                (
                    "s6",
                    None,
                    None,
                    None,
                    Some(false),
                    false,
                    Some(vec![Busy::HeavyRun]),
                    None
                ),
            ],
            "only this machine's running or parked sessions, each unknown left none: \
             busy only once its harness marks background runs, an expired lease let go"
        );
    }

    #[test]
    fn a_cell_orchestrates_where_its_session_is_named_by_either_id() {
        let sessions = [
            session("s1", "skiff", &json!({})),
            session("s2", "skiff", &json!({"presence": {"session": "pi-s2"}})),
            session("s3", "skiff", &json!({"presence": {"session": "pi-s3"}})),
        ];
        let orchestrating = |orchestrators: Option<&BTreeSet<String>>| {
            let looked = Looked {
                sessions: &sessions,
                fences: &BTreeMap::new(),
                holdings: &[],
                queued: &BTreeSet::new(),
                sizes: &BTreeMap::new(),
                orchestrators,
                boot: None,
                began: None,
                now: now(),
            };
            cells("skiff", &looked)
                .iter()
                .map(|cell| cell.orchestrates)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            orchestrating(Some(&["pi-s2".to_owned(), "s3".to_owned()].into())),
            [Some(false), Some(true), Some(true)],
            "s2 by its session's own id, s3 by the mesh's"
        );
        assert_eq!(
            orchestrating(None),
            [None, None, None],
            "where who orchestrates can't be told, of none"
        );
    }

    // A lane started for a quest of its own is in no orchestrator's claim
    // and no quest's record of orchestration, yet says whose it is: two
    // orchestrators would have been put to sleep in shadow for want of it.
    #[test]
    fn a_cell_orchestrates_where_another_session_says_it_answers_to_it() {
        let sessions = vec![
            session("s1", "skiff", &json!({"address": "OSPREY"})),
            session(
                "s2",
                "skiff",
                &json!({"presence": {"session": "s2", "addresses": ["S2", "STEWARD"]}}),
            ),
            session("s3", "skiff", &json!({"orchestrated_by": "osprey"})),
            session(
                "s4",
                "skiff",
                &json!({"orchestrated_by": "steward", "state": "ended",
                        "presence": {"session": "s4", "state": "parked",
                                     "owner": {"bootId": "b2"}}}),
            ),
            session("s5", "skiff", &json!({"orchestrated_by": "S5"})),
            session("s6", "skiff", &json!({})),
            session(
                "s7",
                "tern",
                &json!({"orchestrated_by": "S6", "state": "ended"}),
            ),
            session("s8", "skiff", &json!({})),
            session(
                "s9",
                "tern",
                &json!({"orchestrated_by": "S8", "state": "unreachable",
                        "presence": dropped("s9", 10)}),
            ),
            session("s10", "skiff", &json!({})),
            session(
                "s11",
                "skiff",
                &json!({"orchestrated_by": "S10", "state": "ended",
                        "presence": {"session": "s11", "state": "parked",
                                     "owner": {"bootId": "b1"}}}),
            ),
            session("s12", "skiff", &json!({"address": "WREN"})),
            session("s13", "skiff", &json!({"orchestrated_by": "s12"})),
        ];
        let looked = Looked {
            sessions: &sessions,
            fences: &BTreeMap::new(),
            holdings: &[],
            queued: &BTreeSet::new(),
            sizes: &BTreeMap::new(),
            orchestrators: Some(&BTreeSet::new()),
            boot: Some("b2"),
            began: Some(began()),
            now: now(),
        };
        let lane = |id: &str, said: &str| (id.to_owned(), Some(true), Some(said.to_owned()));
        assert_eq!(
            leading(&looked),
            [
                lane("s1", "S3"),
                lane("s2", "S4, parked"),
                lane("s8", "S9, out of reach on tern"),
            ],
            "s1 by its address in any case; s2 by another it goes by, from a lane parked \
             here this boot; s8 by a lane out of reach lately. None for s6, whose lane \
             ended, s10, whose lane parked before this boot, s12, named by its id and not \
             its address, or s5, which names itself"
        );
    }

    // A machine closed or retired stays in the realm's records, and a Mac
    // names no boot, so a lane counts only where it can be told live, and a
    // running one is named before the rest, then a parked one.
    #[test]
    fn a_lane_counts_only_where_it_can_be_told_live() {
        let sessions = vec![
            session("s14", "skiff", &json!({})),
            // Written just now, by its machine's clock, but its link
            // dropped two hours since by this node's.
            session(
                "s15",
                "tern",
                &json!({"orchestrated_by": "S14", "state": "unreachable",
                        "presence": dropped("s15", 120)}),
            ),
            session("s27", "skiff", &json!({})),
            session(
                "s28",
                "tern",
                &json!({"orchestrated_by": "S27", "state": "unreachable",
                        "presence": dropped("s28", 59)}),
            ),
            session(
                "s29",
                "skiff",
                &json!({"orchestrated_by": "S27", "state": "ended",
                        "presence": {"session": "s29", "state": "parked",
                                     "owner": {"bootId": "b2"}}}),
            ),
            session("s30", "skiff", &json!({})),
            session(
                "s31",
                "tern",
                &json!({"orchestrated_by": "S30", "state": "unreachable",
                        "presence": dropped("s31", 61)}),
            ),
            session("s16", "skiff", &json!({})),
            session(
                "s17",
                "tern",
                &json!({"orchestrated_by": "S16", "state": "ended",
                        "presence": {"session": "s17", "state": "parked",
                                     "owner": {"bootId": "b2"}}}),
            ),
            session("s18", "skiff", &json!({})),
            session(
                "s19",
                "skiff",
                &json!({"orchestrated_by": "S18", "state": "ended",
                        "presence": {"session": "s19", "state": "parked"}}),
            ),
            session("s20", "skiff", &json!({})),
            session(
                "s21",
                "skiff",
                &json!({"orchestrated_by": "S20", "state": "ended", "at": EARLIER,
                        "presence": {"session": "s21", "state": "parked"}}),
            ),
            session("s22", "skiff", &json!({})),
            session(
                "s23",
                "tern",
                &json!({"orchestrated_by": "S22", "state": "unreachable"}),
            ),
            session("s24", "tern", &json!({"orchestrated_by": "S22"})),
            session("s25", "skiff", &json!({})),
            session("s26", "skiff", &json!({"orchestrated_by": "S25"})),
        ];
        let recorded = BTreeSet::from(["s25".to_owned()]);
        let looked = Looked {
            sessions: &sessions,
            fences: &BTreeMap::new(),
            holdings: &[],
            queued: &BTreeSet::new(),
            sizes: &BTreeMap::new(),
            orchestrators: Some(&recorded),
            boot: Some("b2"),
            began: Some(began()),
            now: now(),
        };
        let lane = |id: &str, said: &str| (id.to_owned(), Some(true), Some(said.to_owned()));
        assert_eq!(
            leading(&looked),
            [
                lane("s27", "S29, parked"),
                lane("s18", "S19, parked"),
                lane("s22", "S24"),
                ("s25".to_owned(), Some(true), None),
            ],
            "s27 by its lane parked here before its lane out of reach 59 minutes; s18 by a \
             lane parked here, naming no boot, since the boot began; s22 by its running \
             lane on tern before its lane out of reach since a time not said; s25 by the \
             record, which names no lane. None for s14 and s30, whose lanes' links \
             dropped two hours and 61 minutes since, s16, whose lane parked on tern names \
             this machine's boot, or s20, whose lane naming no boot was parked before the \
             boot began"
        );
    }

    // Our council, round 1 of #393 (2026-11-23): a Mac names no boot, so
    // whether a lane parked there this boot is told by when it was written.
    #[test]
    fn a_node_naming_no_boot_tells_a_parked_lane_by_when_it_was_written() {
        let parked = |id: &str, by: &str, more: &Value| {
            let mut presence = json!({"session": id, "state": "parked"});
            if let (Value::Object(presence), Value::Object(more)) = (&mut presence, more) {
                presence.extend(more.clone());
            }
            json!({"orchestrated_by": by, "state": "ended", "presence": presence})
        };
        let with_at = |mut said: Value, at: &str| {
            said["at"] = json!(at);
            said
        };
        let sessions = vec![
            session("s40", "skiff", &json!({})),
            session("s41", "skiff", &parked("s41", "S40", &json!({}))),
            session("s42", "skiff", &json!({})),
            session(
                "s43",
                "skiff",
                &with_at(parked("s43", "S42", &json!({})), EARLIER),
            ),
            session("s44", "skiff", &json!({})),
            session(
                "s45",
                "skiff",
                &parked("s45", "S44", &json!({"owner": {"bootId": "b1"}})),
            ),
            session("s46", "skiff", &json!({})),
            session(
                "s47",
                "skiff",
                &with_at(
                    parked("s47", "S46", &json!({"owner": {"bootId": "b1"}})),
                    EARLIER,
                ),
            ),
        ];
        let looked = Looked {
            sessions: &sessions,
            fences: &BTreeMap::new(),
            holdings: &[],
            queued: &BTreeSet::new(),
            sizes: &BTreeMap::new(),
            orchestrators: Some(&BTreeSet::new()),
            boot: None,
            began: Some(began()),
            now: now(),
        };
        let lane = |id: &str, said: &str| (id.to_owned(), Some(true), Some(said.to_owned()));
        assert_eq!(
            leading(&looked),
            [lane("s40", "S41, parked"), lane("s44", "S45, parked")],
            "s40 and s44 by lanes written since the boot began, whatever boot a lane names \
             where the machine names none. None for s42 or s46, whose lanes were written \
             before it"
        );
    }

    // Each cell on skiff that orchestrates or can't be told not to, with
    // the lane its reason line would name.
    fn leading(looked: &Looked<'_>) -> Vec<(String, Option<bool>, Option<String>)> {
        cells("skiff", looked)
            .iter()
            .filter(|cell| cell.orchestrates != Some(false))
            .map(|cell| {
                (
                    cell.session.id.clone(),
                    cell.orchestrates,
                    cell.lane.clone(),
                )
            })
            .collect()
    }

    // Our council, round 2 of #393 (2026-11-23): an idle lane's own time is
    // as old as its last change, so only when its link dropped bounds it.
    #[test]
    fn an_idle_lane_out_of_reach_counts_by_when_its_link_dropped() {
        let away = |id: &str, by: &str, dropped_mins: i64| {
            json!({"orchestrated_by": by, "state": "unreachable", "at": EARLIER,
                   "presence": dropped(id, dropped_mins)})
        };
        let sessions = [
            session("s1", "skiff", &json!({})),
            session("s2", "tern", &away("s2", "S1", 10)),
            session("s3", "skiff", &json!({})),
            session("s4", "tern", &away("s4", "S3", 90)),
        ];
        let looked = Looked {
            sessions: &sessions,
            fences: &BTreeMap::new(),
            holdings: &[],
            queued: &BTreeSet::new(),
            sizes: &BTreeMap::new(),
            orchestrators: Some(&BTreeSet::new()),
            boot: Some("b2"),
            began: Some(began()),
            now: now(),
        };
        assert_eq!(
            leading(&looked),
            [(
                "s1".to_owned(),
                Some(true),
                Some("S2, out of reach on tern".to_owned())
            )],
            "both lanes were last written two hours since, but only s4's link dropped \
             more than the hour before"
        );
    }

    // Our council, on #393's fix-forward and on #401 and #408 (2026-11-23):
    // a node back from long down marks a machine out of reach since it was
    // last linked, hours back, though it saw that machine gone only from
    // its start; a node restarted again and again must not see it gone
    // afresh each time, or a gone machine's lanes would hold for good; and
    // the time a node was stopped is not time it saw.
    #[test]
    fn a_lane_counts_by_how_long_this_node_has_seen_its_machine_gone() {
        // Out of reach since `since` minutes back, seen so by this run from
        // `seen` minutes back, and by earlier runs for `earlier` minutes.
        let lane = |since: i64, seen: Option<i64>, earlier: Option<i64>| {
            let mut presence = json!({"session": "s2",
                                      "unreachableSince": ago(since).as_millisecond()});
            if let Some(mins) = seen {
                presence["unreachableSeen"] = json!(ago(mins).as_millisecond());
            }
            if let Some(mins) = earlier {
                presence["unreachableSeenFor"] = json!(mins * 60_000);
            }
            session(
                "s2",
                "tern",
                &json!({"orchestrated_by": "S1", "state": "unreachable",
                        "presence": presence}),
            )
        };
        let (fences, sizes) = (BTreeMap::new(), BTreeMap::new());
        let (queued, orchestrators) = (BTreeSet::new(), BTreeSet::new());
        let leads = |lane: Session| {
            let sessions = [session("s1", "skiff", &json!({})), lane];
            let looked = Looked {
                sessions: &sessions,
                fences: &fences,
                holdings: &[],
                queued: &queued,
                sizes: &sizes,
                orchestrators: Some(&orchestrators),
                boot: Some("b2"),
                began: Some(began()),
                now: now(),
            };
            leading(&looked)
        };
        // Counted as out of reach within the hour, not kept as untold.
        let counts = [(
            "s1".to_owned(),
            Some(true),
            Some("S2, out of reach on tern".to_owned()),
        )];
        assert_eq!(
            leads(lane(120, Some(10), None)),
            counts,
            "dropped two hours since, while this node was down, but seen gone ten minutes"
        );
        assert!(
            leads(lane(120, Some(61), None)).is_empty(),
            "seen gone over the hour, whatever restarts came after"
        );
        assert_eq!(
            leads(lane(10, Some(10), None)),
            counts,
            "dropped and seen ten minutes since, while this node ran"
        );
        assert_eq!(
            leads(lane(300, Some(2), Some(55))),
            counts,
            "seen 55 minutes before a stop and 2 since the start: the stop's hours aren't seen"
        );
        assert!(
            leads(lane(300, Some(10), Some(55))).is_empty(),
            "and 55 before with 10 since is over the hour in all"
        );
        assert!(
            leads(lane(120, Some(60), None)).is_empty(),
            "seen gone the whole hour is no longer within it"
        );
        assert!(
            leads(lane(300, None, Some(55))).is_empty(),
            "put away by a stop and not seen again by a start: from the drop, so it goes"
        );
        assert!(
            leads(lane(70, None, Some(5))).is_empty(),
            "not seen again after a stop that saw five minutes: an hour since the drop"
        );
        assert_eq!(
            leads(lane(-5, None, None)),
            counts,
            "a drop the clock has since set behind reads as within the hour"
        );
        assert!(
            leads(lane(120, Some(-5), Some(61))).is_empty(),
            "a clock set behind this run's sight takes nothing from what earlier runs saw"
        );
        assert!(
            leads(lane(10, None, Some(61))).is_empty(),
            "and seen over the hour before the stop, however recent the drop says"
        );
        assert_eq!(
            leads(lane(10, None, None)),
            counts,
            "marked by an older node with no time seen: by when it dropped"
        );
        assert!(
            leads(lane(120, None, None)).is_empty(),
            "and over the hour since it dropped, it no longer leads"
        );
    }

    /// The lane S4, parked in a boot that can't be told, as a reason names it.
    const PARKED_UNTOLD: &str = "S4, parked, though whether in this boot can't be told";

    // Our council, round 1 of #393 (2026-11-23): a lane whose standing
    // can't be told counted as no lane, so its orchestrator could be
    // carried or slept where the record names none.
    #[test]
    fn a_lane_that_cant_be_told_keeps_its_orchestrator_here() {
        let sessions = [
            session("s1", "skiff", &json!({})),
            session("s2", "skiff", &json!({"orchestrated_by": "S1"})),
            session("s3", "skiff", &json!({})),
            session(
                "s4",
                "skiff",
                &json!({"orchestrated_by": "S3", "state": "ended",
                        "presence": {"session": "s4", "state": "parked",
                                     "owner": {"bootId": "b2"}}}),
            ),
            session("s5", "skiff", &json!({})),
            session(
                "s6",
                "tern",
                &json!({"orchestrated_by": "S5", "state": "unreachable"}),
            ),
        ];
        let looked = Looked {
            sessions: &sessions,
            fences: &BTreeMap::new(),
            holdings: &[],
            queued: &BTreeSet::new(),
            sizes: &BTreeMap::new(),
            orchestrators: Some(&BTreeSet::new()),
            boot: None,
            began: None,
            now: now(),
        };
        let untold = |id: &str, said: &str| (id.to_owned(), None, Some(said.to_owned()));
        assert_eq!(
            leading(&looked),
            [
                ("s1".to_owned(), Some(true), Some("S2".to_owned())),
                untold("s3", PARKED_UNTOLD),
                untold(
                    "s5",
                    "S6, out of reach on tern, though since when can't be told"
                ),
            ],
            "though the record names none, s3's lane is parked in a boot this machine \
             can't tell, and s5's out of reach since a drop no store marked, so neither \
             is told not to orchestrate"
        );
    }

    #[test]
    fn a_node_that_cant_tell_its_boot_or_its_record_still_believes_a_lane() {
        let sessions = [
            session("s1", "skiff", &json!({})),
            session("s2", "skiff", &json!({"orchestrated_by": "S1"})),
            session("s3", "skiff", &json!({})),
            session(
                "s4",
                "skiff",
                &json!({"orchestrated_by": "S3", "state": "ended",
                        "presence": {"session": "s4", "state": "parked",
                                     "owner": {"bootId": "b2"}}}),
            ),
            session("s5", "skiff", &json!({})),
        ];
        let looked = Looked {
            sessions: &sessions,
            fences: &BTreeMap::new(),
            holdings: &[],
            queued: &BTreeSet::new(),
            sizes: &BTreeMap::new(),
            orchestrators: None,
            boot: None,
            began: None,
            now: now(),
        };
        let told: Vec<_> = cells("skiff", &looked)
            .iter()
            .map(|cell| {
                (
                    cell.session.id.clone(),
                    cell.orchestrates,
                    cell.lane.clone(),
                )
            })
            .collect();
        assert_eq!(
            told,
            [
                ("s1".to_owned(), Some(true), Some("S2".to_owned())),
                ("s2".to_owned(), None, None),
                ("s3".to_owned(), None, Some(PARKED_UNTOLD.to_owned())),
                ("s4".to_owned(), None, None),
                ("s5".to_owned(), None, None),
            ],
            "s1 orchestrates by its running lane though the record can't be read; s3's \
             lane s4 is parked in a boot this machine can't tell, so it can't be told"
        );
    }

    #[test]
    fn a_cell_is_the_persons_or_not_only_where_its_presence_says_so() {
        let said = |id: &str, driven: Value| {
            session(
                id,
                "skiff",
                &json!({"presence": {"session": id, "driven": driven}}),
            )
        };
        let sessions = [
            said("s1", json!(true)),
            said("s2", json!(false)),
            said("s3", json!("yes")),
            said("s4", json!(1)),
            said("s5", Value::Null),
        ];
        let looked = Looked {
            sessions: &sessions,
            fences: &BTreeMap::new(),
            holdings: &[],
            queued: &BTreeSet::new(),
            sizes: &BTreeMap::new(),
            orchestrators: Some(&BTreeSet::new()),
            boot: None,
            began: None,
            now: now(),
        };
        let driven: Vec<_> = cells("skiff", &looked)
            .iter()
            .map(|cell| cell.driven)
            .collect();
        assert_eq!(
            driven,
            [Some(true), Some(false), None, None, None],
            "only a bool says, either way"
        );
    }

    #[test]
    fn the_nodes_record_and_the_quests_each_name_orchestrators() {
        let claim = |quests: &[&str]| chain::Claim {
            address: "STEWARD".to_owned(),
            heads: "QEST-20261007-WCJQSG".to_owned(),
            quests: quests.iter().map(|quest| (*quest).to_owned()).collect(),
            at: now(),
        };
        // A claim naming no quest orchestrates none: any session may make
        // one, and would stay put by it for good.
        let record = Orchestrations {
            sessions: [
                ("s-told".to_owned(), claim(&["QEST-20261115-HARBOR"])),
                ("s-idle".to_owned(), claim(&[])),
            ]
            .into(),
        };
        let quests = [
            json!({"id": "QEST-20261007-WCJQSG", "address": "WCJQSG", "sessions": ["s-named"]}),
            json!({"id": "QEST-20261115-HARBOR", "orchestrator": "WCJQSG",
                   "sessions": ["s-harbour"]}),
        ];
        assert_eq!(
            orchestrators(Ok(&record), Ok(&quests)),
            Ok(["s-named".to_owned(), "s-told".to_owned()].into()),
            "the node's claim to a quest and the quests' orchestrator each"
        );
        assert_eq!(
            orchestrators(Err("its record is cut short".to_owned()), Ok(&quests)),
            Err("its record is cut short".to_owned()),
            "the node's record unread tells nothing, saying why"
        );
        assert_eq!(
            orchestrators(Ok(&record), Err("no quests' record".to_owned())),
            Err("no quests' record".to_owned()),
            "no quests' record read tells nothing, saying why"
        );
    }

    #[test]
    fn a_look_reads_the_nodes_records_and_says_why_one_cant_be_read() {
        let state = tempfile::tempdir().expect("the node's state");
        let (orchestrations, carries) = (
            state.path().join("orchestrations.json"),
            state.path().join("carries.jsonl"),
        );
        let none = records(&orchestrations, Ok(&[]), &carries, "skiff", now());
        assert_eq!(
            (none.orchestrators, none.kept_eases),
            (Ok(BTreeSet::new()), Ok(0)),
            "neither kept yet: none orchestrates, none was carried"
        );
        fs::write(
            &orchestrations,
            serde_json::to_string(&json!({"sessions": {"s-told": {
                "address": "STEWARD", "heads": "QEST-20261007-WCJQSG",
                "quests": ["QEST-20261115-HARBOR"], "at": NOW,
            }}}))
            .expect("json"),
        )
        .expect("kept");
        let carry = |by: &str, hours: i64| {
            json!({"id": format!("s-{hours}:then"), "at": ago(hours * 60).to_string(),
                   "by": by, "session": format!("s-{hours}"), "from": "skiff", "to": "tern"})
        };
        let lines = [
            carry("node@skiff", 1),
            carry("node@skiff", 25),
            carry("WCJQSG", 2),
        ];
        let text = lines.iter().fold(String::new(), |mut text, line| {
            text.push_str(&line.to_string());
            text.push('\n');
            text
        });
        fs::write(&carries, text).expect("kept");
        let read = records(&orchestrations, Ok(&[]), &carries, "skiff", now());
        assert_eq!(
            (read.orchestrators, read.kept_eases),
            (Ok(["s-told".to_owned()].into()), Ok(1)),
            "the claim, and skiff's node's one carry within the day"
        );
        assert_eq!(
            records(
                &orchestrations,
                Err("unserved".to_owned()),
                &carries,
                "skiff",
                now()
            )
            .orchestrators,
            Err("unserved".to_owned())
        );
        fs::write(&orchestrations, "{").expect("kept");
        fs::remove_file(&carries).expect("gone");
        fs::create_dir(&carries).expect("a folder where the record was");
        let unread = records(&orchestrations, Ok(&[]), &carries, "skiff", now());
        assert!(
            unread.orchestrators.as_ref().is_err_and(
                |why| why.starts_with("the node's record of orchestrations can't be read (")
            ),
            "a record half kept tells nothing, saying why: {:?}",
            unread.orchestrators
        );
        assert!(
            unread.kept_eases.as_ref().is_err_and(|why| {
                why.starts_with("the carries kept here can't be read (")
                    && why.ends_with("so how many skiff carried within the day can't be told")
            }),
            "{:?}",
            unread.kept_eases
        );
    }

    #[test]
    fn a_nodes_own_lines_say_what_it_carried_within_the_day() {
        let state = tempfile::tempdir().expect("the node's state");
        let folder = state.path().join("eases");
        fs::create_dir_all(&folder).expect("the folder");
        let carry = |mins: i64, session: &str| {
            json!({"at": ago(mins).to_string(), "would": true, "choice": "carry",
                   "session": session, "to": "tern", "why": "over"})
            .to_string()
        };
        let today = [
            carry(60, "s-today"),
            json!({"at": ago(30).to_string(), "would": false, "choice": "carry",
                   "session": "s-refused", "to": "tern", "why": "over",
                   "refused": "tern isn't one of this realm's machines"})
            .to_string(),
            json!({"at": NOW, "choice": "nothing", "why": "cool"}).to_string(),
            "not a line".to_owned(),
        ];
        fs::write(folder.join("2026-11-16.jsonl"), today.join("\n")).expect("kept");
        let yesterday = [carry(20 * 60, "s-yesterday"), carry(25 * 60, "s-older")];
        fs::write(folder.join("2026-11-15.jsonl"), yesterday.join("\n")).expect("kept");
        fs::write(folder.join("2026-11-14.jsonl"), carry(30 * 60, "s-gone")).expect("kept");
        let eased = eased_lately(&folder, "skiff", now()).expect("its lines read");
        let said: Vec<(&str, &str, &str, bool)> = eased
            .iter()
            .map(|moved| {
                let Moved {
                    session,
                    from,
                    to,
                    eases,
                    ..
                } = moved;
                (session.as_str(), from.as_str(), to.as_str(), *eases)
            })
            .collect();
        assert_eq!(
            said,
            [
                ("s-yesterday", "skiff", "tern", true),
                ("s-today", "skiff", "tern", true),
                ("s-refused", "skiff", "tern", true)
            ],
            "its carries within the day, whatever day's file holds them, a refused one too"
        );
        assert_eq!(
            eased_lately(&state.path().join("nowhere"), "skiff", now()),
            Ok(Vec::<Moved>::new()),
            "no lines kept yet are no carries"
        );
    }

    #[test]
    fn a_torn_line_counts_for_none_and_a_day_ahead_still_counts() {
        let state = tempfile::tempdir().expect("the node's state");
        let folder = state.path().join("eases");
        fs::create_dir_all(&folder).expect("the folder");
        let carry = |at: Timestamp, session: &str| {
            json!({"at": at.to_string(), "would": true, "choice": "carry",
                   "session": session, "to": "tern", "why": "over"})
            .to_string()
        };
        // A line half written as the node stopped can end part way into a
        // letter, leaving the file no longer UTF-8.
        let mut today = carry(ago(60), "s-whole").into_bytes();
        today.extend_from_slice(b"\n{\"choice\": \"carry\", \"session\": \"s-\xc3");
        fs::write(folder.join("2026-11-16.jsonl"), today).expect("kept");
        // Kept while the clock ran a day ahead, then set back.
        fs::write(
            folder.join("2026-11-17.jsonl"),
            carry(ago(-20 * 60), "s-ahead"),
        )
        .expect("kept");
        let eased = eased_lately(&folder, "skiff", now()).expect("its lines read");
        let sessions: Vec<&str> = eased.iter().map(|moved| moved.session.as_str()).collect();
        assert_eq!(
            sessions,
            ["s-whole", "s-ahead"],
            "the torn line counts for none, the rest of its day counts, and a day ahead holds \
             carries back"
        );
    }

    #[test]
    fn a_line_that_acts_is_kept_past_its_days_room_and_a_line_that_doesnt_is_let_go() {
        let state = tempfile::tempdir().expect("the node's state");
        let folder = state.path().join("eases");
        let line = |choice: &str| json!({"at": NOW, "would": true, "choice": choice});
        assert_eq!(
            keep(&folder, now(), &line("nothing"), 1).ok(),
            Some(false),
            "a look that does nothing is let go once the day is full"
        );
        assert_eq!(
            keep(&folder, now(), &line("sleep"), 1).ok(),
            Some(false),
            "nor is a sleep kept then, since nothing reads one back"
        );
        assert_eq!(
            keep(&folder, now(), &line("carry"), 1).ok(),
            Some(true),
            "a carry counts after a restart, so it is kept however full"
        );
    }

    #[test]
    fn a_line_kept_after_a_torn_one_starts_a_line_of_its_own() {
        let state = tempfile::tempdir().expect("the node's state");
        let folder = state.path().join("eases");
        fs::create_dir_all(&folder).expect("the folder");
        fs::write(
            folder.join("2026-11-16.jsonl"),
            b"{\"choice\": \"carry\", \"session\": \"s-\xc3",
        )
        .expect("a line cut short");
        let line = json!({"at": ago(5).to_string(), "would": true, "choice": "carry",
                          "session": "s-after", "to": "tern", "why": "over"});
        assert_eq!(keep(&folder, now(), &line, DAY_HOLDS).ok(), Some(true));
        let eased = eased_lately(&folder, "skiff", now()).expect("its lines read");
        assert_eq!(
            eased
                .iter()
                .map(|moved| moved.session.as_str())
                .collect::<Vec<_>>(),
            ["s-after"],
            "the carry kept after a torn line is read, not joined to it"
        );
    }

    #[test]
    fn only_the_days_from_yesterday_on_are_read() {
        let state = tempfile::tempdir().expect("the node's state");
        let folder = state.path().join("eases");
        // Neither can be read as a day's file, so either one read would
        // stop easing.
        fs::create_dir_all(folder.join("notes.jsonl")).expect("a stray name");
        fs::create_dir_all(folder.join("2026-11-14.jsonl")).expect("a day gone by");
        fs::write(
            folder.join("2026-11-15.jsonl"),
            json!({"at": ago(60).to_string(), "would": true, "choice": "carry",
                   "session": "s-yesterday", "to": "tern", "why": "over"})
            .to_string(),
        )
        .expect("kept");
        let eased = eased_lately(&folder, "skiff", now());
        assert_eq!(
            eased.map(|eased| eased
                .into_iter()
                .map(|moved| moved.session)
                .collect::<Vec<_>>()),
            Ok(vec!["s-yesterday".to_owned()]),
            "a name that isn't a day, and a day before yesterday, are passed over"
        );
    }

    #[test]
    fn a_new_doubt_of_who_orchestrates_or_its_clearing_is_said() {
        let machines: Vec<Machine> = realm();
        let declared = declaring(&[Kind::Fence]);
        let here = Here {
            name: "skiff",
            declared: Some(&declared),
            machines: &machines,
            loads: &Loads::new(),
            workers: &BTreeSet::new(),
        };
        let look = |easing: &mut Easing| {
            let reading = balance::read(&machines, &[], now());
            easing
                .look(&here, reading, Vec::new(), &BTreeMap::new(), Ok(0), now())
                .map(|line| line.get("orchestrators_unknown").cloned())
        };
        let mut easing = Easing::default();
        assert_eq!(look(&mut easing), Some(None), "the first look is said");
        assert_eq!(look(&mut easing), None, "looked again the same, nothing is");
        let why = "the quest records have not been read yet";
        easing.doubt(Some(why.to_owned()));
        assert_eq!(
            look(&mut easing),
            Some(Some(json!(why))),
            "a doubt arising is said, though nothing else changed"
        );
        assert_eq!(look(&mut easing), None, "once");
        easing.doubt(None);
        assert_eq!(look(&mut easing), Some(None), "and so is its clearing");
    }

    #[test]
    fn its_own_lines_seed_the_cooldown_and_pin_their_session() {
        let moved = |session: &str, mins: i64| Moved {
            session: session.to_owned(),
            from: "skiff".to_owned(),
            to: "osprey".to_owned(),
            at: ago(mins),
            eases: true,
        };
        let carried = |eased: Vec<Moved>| {
            looks(&mut Easing::after(&[], eased), &[cell("s1")])
                .iter()
                .any(|line| line["choice"] == "carry")
        };
        assert!(carried(Vec::new()), "with nothing seeded, s1 is carried");
        let deciding = first_carry_ago();
        let first_after = |mins: i64| {
            let eased = vec![moved("s0", deciding + mins)];
            first_carry(&looks(&mut Easing::after(&[], eased), &[cell("s1")]))
        };
        assert!(
            first_after(within_cooldown()).is_none_or(|carried| carried < deciding),
            "a carry its lines say a minute short of the cooldown before the look that \
             would carry holds that look"
        );
        assert_eq!(
            first_after(ease::COOLDOWN.as_mins()),
            Some(deciding),
            "and one the whole cooldown before it holds none"
        );
        assert!(
            !carried(vec![moved("s1", 3 * 60)]),
            "a session its lines say it carried here stays pinned"
        );
    }

    #[test]
    fn a_carry_the_record_holds_and_another_only_its_own_lines_say_both_count() {
        let carry = |each: i64| Moved {
            session: format!("s-{each}"),
            from: "skiff".to_owned(),
            to: "osprey".to_owned(),
            at: ago(apart(each)),
            eases: true,
        };
        let kept: Vec<Kept> = (0..daily() - 1)
            .map(|each| Kept {
                id: format!("s-{each}:then"),
                at: ago(apart(each)),
                line: json!({"by": "node@skiff", "session": format!("s-{each}"),
                             "from": "skiff", "to": "osprey"}),
            })
            .collect();
        let eased: Vec<Moved> = (0..daily()).map(carry).collect();
        let lines = looks_kept(
            &mut Easing::after(&kept, eased),
            &[cell("s1")],
            &Ok(ease::DAILY - 1),
        );
        assert_eq!(
            lines.last().expect("a line")["why"],
            json!(full_day()),
            "one short of the day's in both and one in its lines alone are the day's"
        );

        // The same, recorded as another machine's node's, hide none.
        let elsewhere: Vec<Kept> = kept
            .iter()
            .map(|one| {
                let mut line = one.line.clone();
                line["by"] = json!("node@tern");
                line["from"] = json!("tern");
                Kept {
                    line,
                    ..one.clone()
                }
            })
            .collect();
        let eased: Vec<Moved> = (0..daily()).map(carry).collect();
        let lines = looks_kept(&mut Easing::after(&elsewhere, eased), &[cell("s1")], &Ok(0));
        assert_eq!(
            lines.last().expect("a line")["why"],
            json!(full_day()),
            "tern's node's carries of the same sessions leave skiff's day"
        );

        // The same, each with no `to`, are none the record remembers,
        // so they hide none of its own lines either.
        let unsaid: Vec<Kept> = kept
            .iter()
            .map(|one| {
                let mut line = one.line.clone();
                line.as_object_mut().expect("an object").remove("to");
                Kept {
                    line,
                    ..one.clone()
                }
            })
            .collect();
        let eased: Vec<Moved> = (0..daily()).map(carry).collect();
        let lines = looks_kept(&mut Easing::after(&unsaid, eased), &[cell("s1")], &Ok(0));
        assert_eq!(
            lines.last().expect("a line")["why"],
            json!(full_day()),
            "record lines saying no `to` leave its own day"
        );
    }

    #[test]
    fn only_its_nodes_easing_carries_not_yet_landed_are_on_their_way() {
        let carry = |session: &str, by: &str, from: &str| Kept {
            id: format!("{session}:then"),
            at: ago(3),
            line: json!({"by": by, "session": session, "from": from, "to": "tern"}),
        };
        let kept = [
            carry("s1", "node@skiff", "skiff"),
            carry("s2", "node@skiff", "skiff"),
            carry("s3", "node@skiff", "skiff"),
            carry("s4", "node@skiff", "skiff"),
            carry("s5", "node@skiff", "skiff"),
            carry("s6", "node@skiff", "skiff"),
            carry("s7", "WCJQSG", "skiff"),
            carry("s8", "node@tern", "tern"),
        ];
        let judge = |one: &Kept| match one.said("session") {
            "s2" => kept::State::Carrying,
            "s3" => kept::State::Arrived,
            "s4" => kept::State::Back,
            "s5" => kept::State::Refused("it holds a lease".to_owned()),
            "s6" => kept::State::Stuck("still ordered".to_owned()),
            _ => kept::State::Ordered,
        };
        assert_eq!(
            on_way(&kept, "skiff", judge),
            ["s1", "s2"].map(|session| Moved {
                session: session.to_owned(),
                from: "skiff".to_owned(),
                to: "tern".to_owned(),
                at: ago(3),
                eases: true,
            }),
            "skiff's node's own, ordered or carrying, and no other's"
        );
    }

    #[test]
    fn a_carry_the_record_dates_ahead_counts() {
        let kept = [Kept {
            id: "s1:ahead".to_owned(),
            at: ago(-120),
            line: json!({"by": "node@skiff", "session": "s1", "from": "skiff", "to": "tern"}),
        }];
        assert_eq!(
            eased(&kept, "skiff", now()),
            1,
            "which can only hold carries back"
        );
    }

    #[test]
    fn a_day_of_its_own_lines_it_cant_read_says_why_rather_than_none() {
        let state = tempfile::tempdir().expect("the node's state");
        let folder = state.path().join("eases");
        // A folder where today's file goes can't be read as one.
        fs::create_dir_all(folder.join("2026-11-16.jsonl")).expect("made");
        let read = eased_lately(&folder, "skiff", now());
        assert!(
            matches!(&read, Err(why) if why.contains("2026-11-16.jsonl")),
            "{read:?}"
        );
    }

    #[test]
    fn a_folder_of_its_own_lines_it_cant_list_says_why_rather_than_none() {
        let state = tempfile::tempdir().expect("the node's state");
        // A file where the folder goes can't be listed as one.
        let folder = state.path().join("eases");
        fs::write(&folder, "").expect("written");
        let read = eased_lately(&folder, "skiff", now());
        assert!(
            matches!(&read, Err(why) if why.contains("eases")),
            "{read:?}"
        );
    }

    #[test]
    fn a_carry_in_the_record_and_its_nodes_own_line_counts_once() {
        let (sessions, hours) = (["s-0", "s-1", "s-2"], [2, 3, 4]);
        let kept: Vec<Kept> = sessions
            .iter()
            .zip(hours)
            .map(|(session, hour)| Kept {
                id: format!("{session}:then"),
                at: ago(hour * 60),
                line: json!({"by": "node@skiff", "session": session,
                             "from": "skiff", "to": "osprey"}),
            })
            .collect();
        let eased: Vec<Moved> = sessions
            .iter()
            .zip(hours)
            .map(|(session, hour)| Moved {
                session: (*session).to_owned(),
                from: "skiff".to_owned(),
                to: "osprey".to_owned(),
                at: ago(hour * 60 + 1),
                eases: true,
            })
            .collect();
        let lines = looks_kept(&mut Easing::after(&kept, eased), &[cell("s1")], &Ok(0));
        assert!(
            lines.iter().any(|line| line["choice"] == "carry"),
            "three carries said twice are three, not six: {lines:?}"
        );
    }

    #[test]
    fn a_restart_with_the_record_of_carries_cut_short_still_stops_the_next() {
        let eased: Vec<Moved> = (0..daily())
            .map(|each| Moved {
                session: format!("s-{each}"),
                from: "skiff".to_owned(),
                to: "osprey".to_owned(),
                at: ago(apart(each)),
                eases: true,
            })
            .collect();
        let lines = looks_kept(&mut Easing::after(&[], eased), &[cell("s1")], &Ok(0));
        assert!(
            lines.iter().all(|line| line["choice"] != "carry"),
            "its own lines remember the day's though the record holds none: {lines:?}"
        );
        assert_eq!(lines.last().expect("a line")["why"], json!(full_day()));
    }

    #[test]
    fn a_carry_kept_by_its_node_holds_the_cooldown_across_a_restart() {
        let deciding = first_carry_ago();
        let kept = |by: &str| {
            [Kept {
                id: "s0:then".to_owned(),
                at: ago(deciding + within_cooldown()),
                line: json!({"by": by, "session": "s0", "from": "skiff", "to": "osprey"}),
            }]
        };
        let first = |by: &str| {
            first_carry(&looks(
                &mut Easing::after(&kept(by), Vec::new()),
                &[cell("s1")],
            ))
        };
        assert!(
            first("node@skiff").is_none_or(|carried| carried < deciding),
            "its node's carry a minute short of the cooldown before the look that would \
             carry holds that look"
        );
        assert_eq!(
            first("WCJQSG"),
            Some(deciding),
            "an orchestrator's carry starts no cooldown"
        );
    }

    #[test]
    fn a_node_counts_only_the_carries_it_made_within_the_day() {
        let carry = |by: &str, from: &str, hours: i64| Kept {
            id: format!("{by}:{from}:{hours}"),
            at: ago(hours * 60),
            line: json!({"by": by, "session": "s1", "from": from, "to": "tern"}),
        };
        let kept = [
            carry("node@skiff", "skiff", 1),
            carry("node@skiff", "skiff", 23),
            carry("node@skiff", "skiff", 24),
            carry("node@skiff", "tern", 1),
            carry("node@tern", "tern", 1),
            carry("node@tern", "skiff", 1),
            carry("WCJQSG", "skiff", 1),
            carry("person", "skiff", 1),
        ];
        assert_eq!(
            eased(&kept, "skiff", now()),
            2,
            "skiff's node's own, from skiff, within the day"
        );
    }

    #[test]
    fn a_record_of_carries_cut_short_still_stops_the_next() {
        let kept: Vec<Kept> = (0..daily())
            .map(|each| Kept {
                id: format!("s-{each}:then"),
                at: ago(apart(each)),
                line: json!({"by": "node@skiff", "session": format!("s-{each}"),
                             "from": "skiff", "to": "osprey"}),
            })
            .collect();
        let lines = looks_kept(&mut Easing::after(&kept, Vec::new()), &[cell("s1")], &Ok(0));
        assert!(
            lines.iter().all(|line| line["choice"] != "carry"),
            "a day's carries remembered stop the next though the record holds none now: \
             {lines:?}"
        );
        assert_eq!(lines.last().expect("a line")["why"], json!(full_day()));
    }

    #[test]
    fn a_cells_size_is_its_logs() {
        let logs = tempfile::tempdir().expect("pi's logs");
        let folder = logs.path().join("--home-skiff--");
        fs::create_dir_all(&folder).expect("made");
        fs::write(
            folder.join("2026-11-16T07-00-00-000Z_s1.jsonl"),
            [b'x'; 300],
        )
        .expect("kept");
        fs::write(folder.join("2026-11-16T07-00-00-000Z_s9.jsonl"), b"x").expect("kept");
        assert_eq!(
            sizes(logs.path(), &["s1".to_owned(), "s2".to_owned()].into()),
            [("s1".to_owned(), 300)].into(),
            "each session asked of whose log is there, by its bytes"
        );
    }

    #[test]
    fn a_process_holds_its_memory_and_its_use_over_ten_minutes() {
        let procs = tempfile::tempdir().expect("a stand-in for /proc");
        let process = procs.path().join("58");
        fs::create_dir_all(&process).expect("made");
        let counted = |ticks: u64| {
            let stat = format!("58 (pi (a) b) S 1 58 58 0 -1 0 0 0 0 0 {ticks} 0 0 0 20 0 1 0 500");
            fs::write(process.join("stat"), stat).expect("kept");
        };
        fs::write(process.join("status"), "Name:\tpi\nVmRSS:\t  2048 kB\n").expect("kept");
        let pids = [("s1".to_owned(), 58)].into();
        let mut sampler = Sampler::default();
        counted(1000);
        let first = sampler.take(procs.path(), &pids, ago(20));
        assert_eq!(
            first["s1"],
            Holds {
                memory: Some(2 << 20),
                cpu: None
            },
            "its use needs two counts"
        );
        counted(4000);
        assert_eq!(
            sampler.take(procs.path(), &pids, ago(15))["s1"].cpu,
            Some(0.1),
            "3000 ticks over five minutes is a tenth of a core"
        );
        counted(4000 + 60 * 100 * 10);
        assert_eq!(
            sampler.take(procs.path(), &pids, ago(5))["s1"].cpu,
            Some(1.0),
            "over the last ten minutes, not since it was first counted"
        );
        assert!(
            sampler
                .take(procs.path(), &BTreeMap::new(), now())
                .is_empty()
                && sampler.counted.is_empty(),
            "a process no longer asked of is forgotten"
        );
    }

    /// How many carries a day stops at, to count them by.
    fn daily() -> i64 {
        i64::try_from(ease::DAILY).expect("a day's carries are counted in an i64")
    }

    /// What a look says once a day's carries are made.
    fn full_day() -> String {
        let daily = ease::DAILY;
        format!("it carried {daily} sessions within the day, and carries at most {daily} a day")
    }

    /// How many minutes ago the `each`th of a day's carries was made:
    /// the latest two hours ago, or once the cooldown is out if that is
    /// longer, and the rest spread evenly over the day before it, so a
    /// day's fit within it and none holds the cooldown.
    fn apart(each: i64) -> i64 {
        let latest = ease::COOLDOWN.as_mins().max(119) + 1;
        let step = ((ease::DAY.as_mins() - latest) / daily()).max(1);
        latest + each * step
    }

    /// A minute short of the cooldown, in minutes.
    fn within_cooldown() -> i64 {
        ease::COOLDOWN.as_mins() - 1
    }

    /// How many minutes ago the first carry of `lines` was, if any.
    fn first_carry(lines: &[Value]) -> Option<i64> {
        let line = lines.iter().find(|line| line["choice"] == "carry")?;
        let at: Timestamp = line["at"]
            .as_str()
            .expect("a time")
            .parse()
            .expect("a time");
        Some(now().duration_since(at).as_mins())
    }

    /// How many minutes ago [`looks`] first carries s1 with nothing
    /// seeded, so a carry seeded by it is in the past of the look it
    /// holds back, not ahead of it.
    fn first_carry_ago() -> i64 {
        first_carry(&looks(&mut Easing::after(&[], Vec::new()), &[cell("s1")]))
            .expect("a carry with nothing seeded")
    }

    /// Each line `easing` would keep at skiff, looking each minute from 20
    /// minutes ago, with skiff past its memory limit and tern with room
    /// throughout, and no carry of its node's kept.
    fn looks(easing: &mut Easing, cells: &[Cell]) -> Vec<Value> {
        looks_kept(easing, cells, &Ok(0))
    }

    /// As [`looks`], with the record of carries holding `kept_eases` of
    /// skiff's node's.
    fn looks_kept(
        easing: &mut Easing,
        cells: &[Cell],
        kept_eases: &Result<usize, String>,
    ) -> Vec<Value> {
        looks_in(&short_realm(), easing, cells, kept_eases)
    }

    /// As [`looks_kept`], in a realm of `machines` throughout.
    fn looks_in(
        machines: &[Machine],
        easing: &mut Easing,
        cells: &[Cell],
        kept_eases: &Result<usize, String>,
    ) -> Vec<Value> {
        let declared = declaring(&[Kind::Fence]);
        let here = Here {
            name: "skiff",
            declared: Some(&declared),
            machines,
            loads: &Loads::new(),
            workers: &BTreeSet::new(),
        };
        let holds = [(
            "s1".to_owned(),
            Holds {
                memory: Some(1 << 30),
                cpu: Some(2.5),
            },
        )]
        .into();
        (0..=20)
            .filter_map(|minute| {
                let at = ago(20 - minute);
                let reading = balance::read(machines, &[], at);
                easing.look(
                    &here,
                    reading,
                    cells.to_vec(),
                    &holds,
                    kept_eases.clone(),
                    at,
                )
            })
            .collect()
    }

    fn chosen(lines: &[Value]) -> Vec<&str> {
        lines
            .iter()
            .map(|line| line["choice"].as_str().expect("a choice"))
            .collect()
    }

    #[test]
    fn a_session_chosen_to_carry_is_passed_over_only_within_the_day() {
        let waiting = Cell {
            asleep: false,
            ..cell("s1")
        };
        for (minutes, carries) in [(60, 0), (25 * 60, 1)] {
            let mut easing = Easing::default();
            easing.gone.insert("s1".to_owned(), ago(minutes));
            let lines = looks(&mut easing, std::slice::from_ref(&waiting));
            let chose = chosen(&lines);
            assert_eq!(
                chose.iter().filter(|&&one| one == "carry").count(),
                carries,
                "chosen {minutes} minutes since: {chose:?}"
            );
        }
    }

    #[test]
    fn a_carry_refused_as_it_was_told_holds_back_no_other() {
        let going = |at: Timestamp| Moved {
            session: "s0".to_owned(),
            from: "skiff".to_owned(),
            to: "tern".to_owned(),
            at,
            eases: true,
        };
        let carried = |refused: Option<Timestamp>| {
            let mut easing = Easing::after(&[], vec![going(ago(30))]);
            if let Some(at) = refused {
                easing.refused("s0", at);
            }
            // As the node does before each look, from the record that
            // still stands it ordered.
            easing.on_way(vec![going(ago(30))], ago(20));
            first_carry(&looks(&mut easing, &[cell("s1")])).is_some()
        };
        assert!(!carried(None), "one on its way holds the next");
        assert!(
            carried(Some(ago(29))),
            "one refused as it was told isn't on its way"
        );
        assert!(
            !carried(Some(ago(31))),
            "a refusal before it holds no later carry of the session"
        );
    }

    #[test]
    fn a_refusal_is_forgotten_once_no_carry_on_its_way_can_be_older() {
        let stuck = kept::STUCK_AFTER.as_mins();
        let going = |at: Timestamp| Moved {
            session: "s0".to_owned(),
            from: "skiff".to_owned(),
            to: "tern".to_owned(),
            at,
            eases: true,
        };
        let mut easing = Easing::after(&[], vec![going(ago(stuck - 1))]);
        easing.refused("s0", ago(stuck - 2));
        easing.refused("s9", ago(stuck + 1));
        // As the node does before each look: one ordered longer ago than
        // kept::STUCK_AFTER stands stuck, so none older is on its way.
        easing.on_way(vec![going(ago(stuck - 1))], now());
        assert!(
            !easing.refused.contains_key("s9"),
            "a refusal older than any carry on its way is forgotten: {:?}",
            easing.refused
        );
        assert!(
            easing.refused.contains_key("s0"),
            "a newer refusal is kept: {:?}",
            easing.refused
        );
        assert!(
            first_carry(&looks(&mut easing, &[cell("s1")])).is_some(),
            "the carry a newer refusal names still holds back no other"
        );
    }

    #[test]
    fn a_node_says_what_it_would_do_once_each_and_counts_it_as_done() {
        let waiting = Cell {
            asleep: false,
            ..cell("s1")
        };
        let lines = looks(&mut Easing::default(), &[waiting]);
        let chose = chosen(&lines);
        assert_eq!(
            chose.iter().filter(|&&one| one == "sleep").count(),
            1,
            "a session it would have put to sleep counts as asleep: {chose:?}"
        );
        assert_eq!(
            chose.iter().filter(|&&one| one == "carry").count(),
            1,
            "a session it would have carried counts as gone: {chose:?}"
        );
        assert!(
            lines
                .windows(2)
                .all(|two| two[0]["why"].is_null() || two[0]["why"] != two[1]["why"]),
            "nothing is said twice for the same reason in a row"
        );
        let carried = lines
            .iter()
            .position(|line| line["choice"] == "carry")
            .expect("a carry");
        let carry = &lines[carried];
        assert_eq!(
            (&carry["would"], &carry["at"], &carry["to"], &carry["holds"]),
            (
                &json!(true),
                &json!(ago(5).to_string()),
                &json!("tern"),
                &json!({"memory": 1 << 30, "cpu": 2.5})
            ),
            "once five minutes after the sleep still read over, saying what it holds"
        );
        let busy = looks_in(
            &realm(),
            &mut Easing::default(),
            &[Cell {
                asleep: false,
                ..cell("s1")
            }],
            &Ok(0),
        );
        assert_eq!(
            chosen(&busy).iter().find(|&&one| one != "nothing"),
            Some(&"carry"),
            "over on its processor alone, it carries with no sleep before: {busy:?}"
        );
        assert!(
            lines.iter().all(|line| line["reading"]["held"].is_array()),
            "each line holds the reading it was chosen by, as the watch held it"
        );
        assert!(
            lines.get(carried + 1).expect("a line after the carry")["why"]
                .as_str()
                .is_some_and(|why| why.starts_with("it carried s1 at")),
            "after the carry it waits out its cooldown: {lines:?}"
        );
    }

    #[test]
    fn sessions_going_idle_each_minute_hold_no_carry_off() {
        let machines: Vec<Machine> = short_realm();
        let declared = declaring(&[Kind::Fence]);
        let here = Here {
            name: "skiff",
            declared: Some(&declared),
            machines: &machines,
            loads: &Loads::new(),
            workers: &BTreeSet::new(),
        };
        let mut easing = Easing::default();
        let doubt = "the quest records have not been read yet";
        let cooldown = ease::COOLDOWN.as_mins();
        // One more goes idle each minute until the 30th, listed newest
        // first so one never slept would be the first carried, and who
        // orchestrates comes into doubt once none has gone idle for a few
        // minutes, halfway through the first cooldown begun from then.
        // The first carry is at the 15th, and one follows each cooldown.
        let first_carried = 15;
        let doubted = first_carried
            + cooldown * ((30 - first_carried + cooldown - 1) / cooldown)
            + (cooldown + 1) / 2;
        let lines: Vec<Value> = (0..=50)
            .filter_map(|minute: i64| {
                let at = ago(50 - minute);
                if minute == doubted {
                    easing.doubt(Some(doubt.to_owned()));
                }
                let cells = (0..=minute.min(30))
                    .rev()
                    .map(|one| Cell {
                        asleep: false,
                        ..cell(&format!("s{one}"))
                    })
                    .collect();
                easing.look(
                    &here,
                    balance::read(&machines, &[], at),
                    cells,
                    &BTreeMap::new(),
                    Ok(0),
                    at,
                )
            })
            .collect();
        let carries: Vec<usize> = (0..lines.len())
            .filter(|&at| lines[at]["choice"] == "carry")
            .collect();
        let [first, second, ..] = carries[..] else {
            panic!("two carries among {:?}", chosen(&lines));
        };
        assert_eq!(
            [&lines[first]["at"], &lines[second]["at"]],
            [
                &json!(ago(35).to_string()),
                &json!(ago(35 - cooldown).to_string())
            ],
            "five minutes after the first sleep, though one slept each minute \
             since, and again once the cooldown is out"
        );
        for carry in [first, second] {
            let session = &lines[carry]["session"];
            assert!(
                lines[..carry].iter().any(|line| line["choice"] == "sleep"
                    && line["sessions"]
                        .as_array()
                        .is_some_and(|slept| slept.contains(session))),
                "{session} slept before it was carried: {lines:?}"
            );
        }
        assert!(
            lines[..first]
                .iter()
                .filter(|line| line["choice"] == "sleep")
                .all(|line| line.get("why").is_none()),
            "a sleep while it settles says no why: {lines:?}"
        );
        let cooling = &lines[first + 1..second];
        let said = format!(
            "it carried {} at",
            lines[first]["session"].as_str().expect("a session")
        );
        let (sleeping, said_again): (Vec<&Value>, Vec<&Value>) =
            cooling.iter().partition(|line| line["choice"] == "sleep");
        assert!(
            !sleeping.is_empty()
                && sleeping.iter().all(|line| line["why"]
                    .as_str()
                    .is_some_and(|why| why.starts_with(&said))),
            "within the cooldown each sleep says why: {cooling:?}"
        );
        assert!(
            said_again.is_empty(),
            "within the first cooldown, with no doubt, each line is a sleep: {cooling:?}"
        );
        let held = cooldown_holding(&lines, &carries, doubt);
        assert_eq!(
            held.iter()
                .filter(|line| line["choice"] != "sleep")
                .map(|line| (&line["at"], &line["orchestrators_unknown"]))
                .collect::<Vec<_>>(),
            [(&json!(ago(50 - doubted).to_string()), &json!(doubt))],
            "nothing says a sleep's why again until a doubt arises: {held:?}"
        );
    }

    /// The lines after the carry before the first saying `doubt`, up to
    /// the next carry or the end.
    fn cooldown_holding<'a>(lines: &'a [Value], carries: &[usize], doubt: &str) -> &'a [Value] {
        let doubting = lines
            .iter()
            .position(|line| line["orchestrators_unknown"] == json!(doubt))
            .expect("a line saying the doubt");
        let since = carries
            .iter()
            .rev()
            .find(|&&carry| carry < doubting)
            .expect("a carry before the doubt");
        let until = carries
            .iter()
            .find(|&&carry| carry > doubting)
            .map_or(lines.len(), |&carry| carry);
        &lines[since + 1..until]
    }

    #[test]
    fn a_session_carried_before_the_node_began_stays_put() {
        let kept = [Kept {
            id: "s1:then".to_owned(),
            at: ago(120),
            line: json!({"session": "s1", "from": "tern", "to": "skiff"}),
        }];
        let waiting = ["s1", "s2"].map(|id| Cell {
            asleep: false,
            ..cell(id)
        });
        let lines = looks(&mut Easing::after(&kept, Vec::new()), &waiting);
        let carried: Vec<&Value> = lines
            .iter()
            .filter(|line| line["choice"] == "carry")
            .map(|line| &line["session"])
            .collect();
        assert_eq!(
            carried,
            [&json!("s2")],
            "the record of carries holds s1 here for a day, so s2 goes: {lines:?}"
        );
    }

    #[test]
    fn a_days_lines_are_its_owners_alone_and_bounded() {
        let state = tempfile::tempdir().expect("the node's state");
        let folder = state.path().join("eases");
        let line = json!({"at": NOW, "would": true, "choice": "nothing", "why": "cool"});
        assert!(keep(&folder, now(), &line, 200).expect("kept"));
        assert!(keep(&folder, now(), &line, 200).expect("kept"));
        assert!(
            !keep(&folder, now(), &line, 200).expect("let go"),
            "a line past the day's bound is let go"
        );
        let file = folder.join("2026-11-16.jsonl");
        let kept = fs::read_to_string(&file).expect("read");
        assert_eq!(kept.lines().count(), 2, "{kept}");
        let mode = |path: &Path| fs::metadata(path).expect("there").mode() & 0o777;
        assert_eq!((mode(&folder), mode(&file)), (0o700, 0o600));
    }

    #[test]
    fn a_new_day_lets_go_of_the_days_past_a_fortnight() {
        let state = tempfile::tempdir().expect("the node's state");
        let folder = state.path().join("eases");
        fs::create_dir_all(&folder).expect("the folder");
        for name in ["2026-11-01.jsonl", "2026-11-02.jsonl", "notes.jsonl"] {
            fs::write(folder.join(name), b"{}\n").expect("an old day");
        }
        let line = json!({"at": NOW, "would": true, "choice": "nothing", "why": "cool"});
        assert!(keep(&folder, now(), &line, 200).expect("kept"));
        let mut left: Vec<String> = fs::read_dir(&folder)
            .expect("read")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["2026-11-02.jsonl", "2026-11-16.jsonl", "notes.jsonl"],
            "the day 15 days back goes; 14 days back and what isn't a day stay"
        );
    }

    #[test]
    fn only_a_carry_a_look_chose_is_acted_on() {
        let carry = json!({"at": NOW, "would": true, "choice": "carry",
                           "session": "s-harbour", "to": "tern", "why": "skiff is over"});
        assert_eq!(
            super::chosen(&carry),
            Some(Chosen {
                session: "s-harbour".to_owned(),
                to: "tern".to_owned(),
                why: "skiff is over".to_owned(),
            }),
            "a carry is acted on"
        );
        for line in [
            json!({"at": NOW, "would": true, "choice": "sleep", "sessions": ["s-harbour"]}),
            json!({"at": NOW, "would": true, "choice": "nothing", "why": "cool"}),
            json!({"at": NOW, "would": true, "choice": "carry", "session": "s-harbour",
                   "why": "skiff is over"}),
            json!({"at": NOW, "would": true, "choice": "carry", "session": 7,
                   "to": "tern", "why": "skiff is over"}),
        ] {
            assert_eq!(super::chosen(&line), None, "nor anything else: {line}");
        }
    }

    #[test]
    fn a_line_says_how_its_carry_went() {
        let line = || json!({"at": NOW, "would": true, "choice": "carry"});
        let mut carried = line();
        acted(&mut carried, &Ok(()));
        assert_eq!(
            carried,
            json!({"at": NOW, "would": false, "choice": "carry"}),
            "a carry the node made no longer says it would"
        );
        let mut refused = line();
        acted(
            &mut refused,
            &Err("tern would land looser there".to_owned()),
        );
        assert_eq!(
            refused,
            json!({"at": NOW, "would": false, "choice": "carry",
                   "refused": "tern would land looser there"}),
            "and one refused says why"
        );
    }

    #[test]
    fn a_session_whose_ask_cant_be_written_is_chosen_again_only_so_often() {
        let mut easing = Easing::default();
        let at = now();
        let retry = ["s1".to_owned()];
        for tried in 1..=WRITES_TRIED {
            easing.asleep.insert("s1".to_owned());
            easing.slept(0, &retry, at);
            assert_eq!(
                easing.asleep.contains("s1"),
                tried == WRITES_TRIED,
                "passed over only once tried {WRITES_TRIED} times, at {tried}"
            );
        }
        assert_eq!(easing.sleeps(), 0, "none asked counts none");
    }
}
