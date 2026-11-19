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
        .map(|session| Cell {
            session: session.clone(),
            fence: looked.fences.get(id(session)).copied(),
            alone: alone(here, looked.sessions, session),
            on_screen: on_screen(session),
            asleep: super::parked(session, looked.boot),
            orchestrates: looked
                .orchestrators
                .map(|named| named.contains(id(session)) || named.contains(&session.id)),
            busy: busy(session, looked),
            size: looked.sizes.get(id(session)).copied(),
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
    asleep: BTreeSet<String>,
    gone: BTreeSet<String>,
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

    /// Takes `why` who orchestrates can't be told, or none once it can,
    /// for the lines that follow to say.
    pub fn doubt(&mut self, why: Option<String>) {
        self.doubt = why;
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
            }
        }
        let cells: Vec<Cell> = cells
            .into_iter()
            .filter(|cell| !self.gone.contains(&cell.session.id))
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
            Choice::Sleep { sessions } => {
                self.said = None;
                self.asleep.extend(sessions.iter().cloned());
                self.history.slept(sessions.len(), now);
            }
            Choice::Carry { session, to, .. } => {
                self.said = None;
                self.gone.insert(session.clone());
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
    use crate::carry::ease::tests::{NOW, ago, cell, declaring, now, realm};
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

    fn told(id: &str) -> Value {
        json!({"session": id, "leases": {"background": true}})
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
                    false,
                    Some(vec![Busy::HeavyRun]),
                    None
                ),
                ("s2", None, Some(false), None, false, None, Some(512)),
                ("s5", None, None, None, true, None, None),
                (
                    "s6",
                    None,
                    None,
                    None,
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
                ("s-today", "skiff", "tern", true)
            ],
            "its carries within the day, whatever day's file holds them"
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
    fn its_own_lines_seed_the_half_hour_and_pin_their_session() {
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
        assert!(
            !carried(vec![moved("s0", 10)]),
            "a carry its lines say ten minutes ago holds the half hour"
        );
        assert!(
            !carried(vec![moved("s1", 3 * 60)]),
            "a session its lines say it carried here stays pinned"
        );
    }

    #[test]
    fn a_carry_the_record_holds_and_another_only_its_own_lines_say_both_count() {
        let carry = |session: &str, hours: i64| Moved {
            session: session.to_owned(),
            from: "skiff".to_owned(),
            to: "osprey".to_owned(),
            at: ago(hours * 60),
            eases: true,
        };
        let kept: Vec<Kept> = (2..7)
            .map(|hour| Kept {
                id: format!("s-{hour}:then"),
                at: ago(hour * 60),
                line: json!({"by": "node@skiff", "session": format!("s-{hour}"),
                             "from": "skiff", "to": "osprey"}),
            })
            .collect();
        let eased: Vec<Moved> = (2..8)
            .map(|hour| carry(&format!("s-{hour}"), hour))
            .collect();
        let lines = looks_kept(&mut Easing::after(&kept, eased), &[cell("s1")], &Ok(5));
        assert_eq!(
            lines.last().expect("a line")["why"],
            json!("it carried 6 sessions within the day, and carries at most 6 a day"),
            "five in both and one in its lines alone are six"
        );

        // The same five, recorded as another machine's node's, hide none.
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
        let eased: Vec<Moved> = (2..8)
            .map(|hour| carry(&format!("s-{hour}"), hour))
            .collect();
        let lines = looks_kept(&mut Easing::after(&elsewhere, eased), &[cell("s1")], &Ok(0));
        assert_eq!(
            lines.last().expect("a line")["why"],
            json!("it carried 6 sessions within the day, and carries at most 6 a day"),
            "tern's node's carries of the same sessions leave skiff's six"
        );

        // The same five, each with no `to`, are none the record remembers,
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
        let eased: Vec<Moved> = (2..8)
            .map(|hour| carry(&format!("s-{hour}"), hour))
            .collect();
        let lines = looks_kept(&mut Easing::after(&unsaid, eased), &[cell("s1")], &Ok(0));
        assert_eq!(
            lines.last().expect("a line")["why"],
            json!("it carried 6 sessions within the day, and carries at most 6 a day"),
            "record lines saying no `to` leave its own six"
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
    fn a_restart_with_the_record_of_carries_cut_short_still_stops_the_seventh() {
        let eased: Vec<Moved> = (0..6)
            .map(|hour| Moved {
                session: format!("s-{hour}"),
                from: "skiff".to_owned(),
                to: "osprey".to_owned(),
                at: ago((hour + 2) * 60),
                eases: true,
            })
            .collect();
        let lines = looks_kept(&mut Easing::after(&[], eased), &[cell("s1")], &Ok(0));
        assert!(
            lines.iter().all(|line| line["choice"] != "carry"),
            "its own lines remember six though the record holds none: {lines:?}"
        );
        assert_eq!(
            lines.last().expect("a line")["why"],
            json!("it carried 6 sessions within the day, and carries at most 6 a day")
        );
    }

    #[test]
    fn a_carry_kept_by_its_node_holds_the_half_hour_across_a_restart() {
        let kept = |by: &str| {
            [Kept {
                id: "s0:then".to_owned(),
                at: ago(10),
                line: json!({"by": by, "session": "s0", "from": "skiff", "to": "osprey"}),
            }]
        };
        let carried = |by: &str| {
            let lines = looks(&mut Easing::after(&kept(by), Vec::new()), &[cell("s1")]);
            lines.iter().any(|line| line["choice"] == "carry")
        };
        assert!(!carried("node@skiff"), "its node's carry ten minutes ago");
        assert!(
            carried("WCJQSG"),
            "an orchestrator's carry starts no half hour"
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
    fn a_record_of_carries_cut_short_still_stops_the_seventh() {
        let kept: Vec<Kept> = (0..6)
            .map(|hour| Kept {
                id: format!("s-{hour}:then"),
                at: ago((hour + 2) * 60),
                line: json!({"by": "node@skiff", "session": format!("s-{hour}"),
                             "from": "skiff", "to": "osprey"}),
            })
            .collect();
        let lines = looks_kept(&mut Easing::after(&kept, Vec::new()), &[cell("s1")], &Ok(0));
        assert!(
            lines.iter().all(|line| line["choice"] != "carry"),
            "six carries remembered stop a seventh though the record holds none now: \
             {lines:?}"
        );
        assert_eq!(
            lines.last().expect("a line")["why"],
            json!("it carried 6 sessions within the day, and carries at most 6 a day")
        );
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

    /// Each line `easing` would keep at skiff, looking each minute from 20
    /// minutes ago, with skiff over and tern with room throughout, and no
    /// carry of its node's kept.
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
        let machines: Vec<Machine> = realm();
        let declared = declaring(&[Kind::Fence]);
        let here = Here {
            name: "skiff",
            declared: Some(&declared),
            machines: &machines,
            loads: &Loads::new(),
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
                let reading = balance::read(&machines, &[], at);
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
        let carry = lines
            .iter()
            .find(|line| line["choice"] == "carry")
            .expect("a carry");
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
        assert!(
            lines.iter().all(|line| line["reading"]["held"].is_array()),
            "each line holds the reading it was chosen by, as the watch held it"
        );
        assert!(
            lines.last().expect("a line")["why"]
                .as_str()
                .is_some_and(|why| why.starts_with("it carried s1 at")),
            "after the carry it waits out its cooldown: {lines:?}"
        );
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
}
