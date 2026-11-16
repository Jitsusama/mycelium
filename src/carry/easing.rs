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

use super::ease::{self, Busy, Cell, Choice, Here, History, Moved};
use super::kept::Kept;
use crate::balance::{Reading, Watch};
use crate::declared::Kind;
use crate::record::{Session, State};

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
    said: Option<String>,
}

impl Easing {
    /// One remembering each carry `kept`, so that a session moved stays
    /// put, and none goes back where one came from, across the node's
    /// restarts.
    #[must_use]
    pub fn after(kept: &[Kept]) -> Self {
        let mut history = History::default();
        for one in kept {
            let (session, from, to) = (one.said("session"), one.said("from"), one.said("to"));
            if [session, from, to].iter().any(|said| said.is_empty()) {
                continue;
            }
            history.moved(Moved {
                session: session.to_owned(),
                from: from.to_owned(),
                to: to.to_owned(),
                at: one.at,
                eases: false,
            });
        }
        Self {
            history,
            ..Self::default()
        }
    }

    /// What `here` would do now of its `cells`, by `reading`, as the line
    /// to keep for it; none where it would do nothing for the same reason
    /// as last. A carry's line says what the session `holds` of the
    /// machine, where that can be told.
    pub fn look(
        &mut self,
        here: &Here,
        mut reading: Reading,
        cells: Vec<Cell>,
        holds: &BTreeMap<String, Holds>,
        now: Timestamp,
    ) -> Option<Value> {
        reading.held = Some(self.watch.take(&reading));
        self.history.read(here.name, &reading);
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
                if self.said.as_deref() == Some(why) {
                    return None;
                }
                self.said = Some(why.clone());
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
        Some(line)
    }
}

/// Keeps `line`, said `at`, in `folder`'s file for its day, the folder
/// and file each its owner's alone; let go, answering false, where the
/// day's file would hold more than `holds` bytes with it.
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
    let file = folder.join(format!("{}.jsonl", at.strftime("%Y-%m-%d")));
    let mut text = serde_json::to_string(line)?;
    text.push('\n');
    let had = fs::metadata(&file).map_or(0, |meta| meta.len());
    if had.saturating_add(text.len() as u64) > holds {
        return Ok(false);
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&file)?
        .write_all(text.as_bytes())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;
    use crate::balance;
    use crate::carry::ease::tests::{NOW, ago, cell, declaring, now, realm};
    use crate::facts::Machine;
    use crate::placement::Loads;

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
    /// minutes ago, with skiff over and tern with room throughout.
    fn looks(easing: &mut Easing, cells: &[Cell]) -> Vec<Value> {
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
                easing.look(&here, reading, cells.to_vec(), &holds, at)
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
        let lines = looks(&mut Easing::after(&kept), &waiting);
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
}
