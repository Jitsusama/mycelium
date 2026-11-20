//! Letting a carried session's old trees go on the machine it left.
//!
//! A session an orchestrator carried leaves its worktrees behind here.
//! Once the node judges the carry arrived, its session seen running on the
//! machine it went to, and a day has passed since it was ordered, the node
//! asks `pi-carry settle <quest> --session <session>`, which lets go of
//! each tree holding nothing the carry didn't take, and keeps every one
//! otherwise, saying why. What it answered is kept beside the carries as a
//! line of `settles.jsonl`, so no carry is settled twice, even by a node
//! started afresh. Trees `pi-carry` kept are asked for again a day after,
//! while the carry is within its week, since what held them, a shell or
//! an editor standing in one, may have let them go; what the node kept
//! without asking stays for good, as does one held whose session is seen
//! back here. Where trees stay, the carried session is told so,
//! wherever it runs, with why, each time what kept them changes. A line
//! fades a fortnight after it was kept, twice the week
//! a carry is looked at for, as another is kept. An answer lost, to a
//! signal ending the settling or a line that can't be kept, is asked
//! again only by a node started afresh, which settling allows. A carry
//! whose session is seen back here isn't due, nor is any but each
//! session's newest carry, so a session carried back and on again is
//! settled by its later carry alone.

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use super::PI_CARRY;
use super::kept::{self, Kept, State};

/// How long the node waits on one settling before it says in its log that
/// it is waiting on it still.
pub const ANSWERS_WITHIN: Duration = Duration::from_secs(60);

/// How far back the carries ordered are looked at to settle: one not
/// arrived within a week stays as it is.
pub const LOOKED_FOR: jiff::SignedDuration = jiff::SignedDuration::from_hours(7 * 24);

/// How long after it was kept a line of `settles.jsonl` fades: twice the
/// week settling looks back, so a clock stepped back by less than a week
/// brings no carry back into the week without its line.
pub const FADES_AFTER: jiff::SignedDuration = jiff::SignedDuration::from_hours(14 * 24);

/// How long after `pi-carry` kept a carry's trees they are asked for again.
pub const ASKED_AGAIN_AFTER: jiff::SignedDuration = jiff::SignedDuration::from_hours(24);

/// Where a node keeps what each settling answered, and what it asks.
#[derive(Debug, Clone)]
pub struct Settles {
    /// The file of what each settling answered, a line each.
    pub kept: PathBuf,
    /// The program asked, `pi-carry` on the node's `PATH`.
    pub program: PathBuf,
    /// The realm the node serves, told to the program in `MYCELIUM_REALM`
    /// as each session the node starts is, so it reads that realm's
    /// records, not the home realm's, where a node serves another.
    pub realm: String,
    /// How long one settling may take before the log says it is waited on.
    pub answers_within: Duration,
}

/// What one settling came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Every tree the carry left here was let go, or was gone already;
    /// `let_go` is how many it let go.
    Settled {
        /// How many trees it let go.
        let_go: u64,
    },
    /// The trees stay for good, saying why.
    Kept(String),
    /// `pi-carry` kept the trees, saying why, so they are asked for again
    /// `ASKED_AGAIN_AFTER` it kept them.
    Held(String),
}

impl Outcome {
    /// What a carry's record says of it, as `settle`.
    #[must_use]
    pub fn said(&self) -> Value {
        match self {
            Self::Settled { let_go } => json!({"did": "settled", "let": let_go}),
            Self::Kept(why) => json!({"did": "kept", "why": why}),
            Self::Held(why) => json!({"did": "kept", "why": why, "again": true}),
        }
    }
}

/// Whether what settling a carry came to, `said` as its line, still stands
/// at `now`: trees `pi-carry` kept stand until `ASKED_AGAIN_AFTER` they
/// were kept, and every other answer stands for good, as does one whose
/// time can't be read.
#[must_use]
pub fn stands_at(said: &Value, now: jiff::Timestamp) -> bool {
    if said["again"] != true {
        return true;
    }
    said["at"]
        .as_str()
        .and_then(|at| at.parse::<jiff::Timestamp>().ok())
        .and_then(|at| at.checked_add(ASKED_AGAIN_AFTER).ok())
        .is_none_or(|again| again > now)
}

/// The lines of `settles.jsonl` at `path`, a character a write cut short
/// read as a stand-in, so a torn line can't make the rest unreadable.
fn read_lines(path: &std::path::Path) -> io::Result<String> {
    fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// What becomes of a line of `settles.jsonl` as another is kept.
#[derive(Debug, PartialEq, Eq)]
enum Fading {
    /// Kept more than `FADES_AFTER` ago, since its carry was ordered
    /// before then and is no longer looked at, or torn by a write cut
    /// short, which is read as nothing already: it goes.
    Faded,
    /// Kept within `FADES_AFTER`: it stays, and fades in its time.
    Stays,
    /// Its time can't be read, or lies more than `FADES_AFTER` ahead, so
    /// it won't fade by itself: it stays, and is named.
    Stuck,
}

/// What becomes of `line` at `now`.
fn fading(line: &str, now: jiff::Timestamp) -> Fading {
    let Ok(line) = serde_json::from_str::<Value>(line) else {
        return Fading::Faded;
    };
    let Some(at) = line["at"]
        .as_str()
        .and_then(|at| at.parse::<jiff::Timestamp>().ok())
    else {
        return Fading::Stuck;
    };
    let (Ok(fades), Ok(ahead)) = (at.checked_add(FADES_AFTER), now.checked_add(FADES_AFTER)) else {
        return Fading::Stuck;
    };
    if fades < now {
        Fading::Faded
    } else if at > ahead {
        Fading::Stuck
    } else {
        Fading::Stays
    }
}

/// The lines of `before` that stay at `now`, each ending its line, and
/// the names of those stuck (`Fading::Stuck`): each one's carry, quoted
/// and escaped, or its line's number where it names no carry.
fn fade(before: &str, now: jiff::Timestamp) -> (String, Vec<String>) {
    let mut kept = String::new();
    let mut stuck = Vec::new();
    for (number, line) in (1..).zip(before.lines()) {
        match fading(line, now) {
            Fading::Faded => continue,
            Fading::Stays => {}
            Fading::Stuck => stuck.push(
                serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|line| line["carry"].as_str().map(|carry| format!("{carry:?}")))
                    .unwrap_or_else(|| format!("line {number}")),
            ),
        }
        kept.push_str(line);
        kept.push('\n');
    }
    (kept, stuck)
}

/// Whether `carry`, standing as `state`, is settled now on machine
/// `here`: only one carried from here, judged arrived, and ordered more
/// than a day before `now`.
#[must_use]
pub fn due(carry: &Kept, state: &State, here: &str, now: jiff::Timestamp) -> bool {
    let a_day_since = now
        .checked_sub(kept::SHOWN_FOR)
        .is_ok_and(|then| carry.at < then);
    carry.said("from") == here && *state == State::Arrived && a_day_since
}

/// Only the newest of each session's `carries`, newest first: a session
/// carried again is settled by its later carry alone, since an earlier
/// one judges arrived again once the later takes it back where it went,
/// and would let its trees go before the later carry's day had passed.
#[must_use]
pub fn newest_each(mut carries: Vec<Kept>) -> Vec<Kept> {
    carries.sort_by(|one, other| other.at.cmp(&one.at).then(one.id.cmp(&other.id)));
    let mut seen = BTreeSet::new();
    carries.retain(|carry| seen.insert(carry.said("session").to_owned()));
    carries
}

/// The carries of `carries` that may settle on machine `here`: the newest
/// of each session's, carried from here, with no answer kept for good in
/// `settled`, by id. One whose trees `pi-carry` kept is still looked at,
/// so where its session runs is seen while its answer stands.
#[must_use]
pub fn to_look_at<S: std::hash::BuildHasher>(
    carries: Vec<Kept>,
    settled: &HashMap<String, Value, S>,
    here: &str,
) -> Vec<Kept> {
    let mut carries = newest_each(carries);
    carries.retain(|carry| {
        carry.said("from") == here
            && settled
                .get(&carry.id)
                .is_none_or(|said| said["again"] == true)
    });
    carries
}

/// What to tell `carry`'s session, where it went, once settling it came
/// to `outcome`, kept after `before`: that its trees stay where it left
/// them, and why, as `pi-carry` said it, so it takes what's in them or
/// lets them go; or, where they had stayed, that they are let go since.
/// Nothing where they were let go at first, where its session came back
/// to them, or where `before` said the same.
#[must_use]
pub fn telling(carry: &Kept, before: Option<&Value>, outcome: &Outcome) -> Option<String> {
    let from = shown(carry.said("from"));
    let to = shown(carry.said("to"));
    let (why, after) =
        match outcome {
            Outcome::Settled { let_go } => {
                return before.is_some_and(|before| before["did"] == "kept").then(|| {
                format!(
                    "The trees your carry from {from} to {to} left on {from}, told as kept, \
                     are let go since: {let_go} of them. Nothing of them is left to take."
                )
            });
            }
            _ if *outcome == came_back() => return None,
            Outcome::Held(why) => (
                why,
                "pi-carry is asked again a day after, until a week has passed since the carry; \
             take what's in them, or let them go there, before then.",
            ),
            Outcome::Kept(why) => (
                why,
                "They stay for good: take what's in them, or let them go there by hand.",
            ),
        };
    let said = outcome.said();
    let same = |field: &str| before.is_some_and(|before| before[field] == said[field]);
    if same("did") && same("why") && same("again") {
        return None;
    }
    Some(format!(
        "The trees your carry from {from} to {to} left on {from} weren't let go; pi-carry \
         said: {}. {after}",
        shown(why)
    ))
}

/// `text` as a finding shows it: as it was written, but for each control
/// character, escaped, so none moves the terminal it is read in.
fn shown(text: &str) -> String {
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

/// What a carry `pi-carry` held the trees of comes to once its session is
/// seen back here: kept for good, since the trees are its own again.
fn came_back() -> Outcome {
    Outcome::Kept("its session came back here".to_owned())
}

/// What `pi-carry settle`, ending with `code`, came to by what it wrote,
/// `output`: a JSON line a tree, then `settled` with how many it let go,
/// or `failed` saying why every tree stays.
#[must_use]
pub fn judged(code: i32, output: &[u8]) -> Outcome {
    let output = String::from_utf8_lossy(output);
    let lines: Vec<Value> = output
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let last = |kind: &str| lines.iter().rev().find(|line| line["kind"] == kind);
    match code {
        0 => last("settled")
            .and_then(|line| line["let"].as_u64())
            .map_or_else(
                || Outcome::Held(format!("{PI_CARRY} settle ended saying nothing it settled")),
                |let_go| Outcome::Settled { let_go },
            ),
        1 => Outcome::Held(
            last("failed")
                .and_then(|line| line["why"].as_str())
                .map_or_else(
                    || format!("{PI_CARRY} settle kept the trees without saying why"),
                    str::to_owned,
                ),
        ),
        code => Outcome::Held(format!("{PI_CARRY} settle ended with {code}")),
    }
}

/// Settles the carries its node finds due, each on a thread of its own off
/// the node's loop and never twice at once, keeping what each came to.
#[derive(Debug)]
pub struct Settler {
    settles: Settles,
    /// The carries being settled now, by id, so none is asked twice at
    /// once, and those whose answer was lost, which stay until the node
    /// starts afresh.
    settling: Mutex<BTreeSet<String>>,
    /// Where each session of a carry not settled yet ran when it was last
    /// looked at, by session, so only its moving asks to look again.
    watched: Mutex<HashMap<String, Option<String>>>,
    /// The carries whose session was seen where it went, by id, so one
    /// whose session then ends there still settles.
    arrived: Mutex<BTreeSet<String>>,
    /// The carries whose session was seen back here, by id, so one asked
    /// of before is no longer taken as arrived.
    back: Mutex<BTreeSet<String>>,
}

impl Settler {
    /// A settler keeping and asking as `settles` says.
    #[must_use]
    pub fn new(settles: Settles) -> Self {
        Self {
            settles,
            settling: Mutex::default(),
            watched: Mutex::default(),
            arrived: Mutex::default(),
            back: Mutex::default(),
        }
    }

    /// How `carry`, judged `state` now, stands for settling: arrived once
    /// its session was seen where it went, though it runs nowhere now, as
    /// when it ended there, until its session is seen back here. A node
    /// started afresh has seen none arrive, but takes one with a line
    /// `said` of what settling it came to, which was judged arrived then,
    /// as arrived until its session is seen back here. One seen back whose
    /// line says `pi-carry` held its trees is kept for good, at each look
    /// until that is kept, though its session has ended here since, as
    /// what's seen back is kept only in memory.
    pub fn stands(&self, carry: &Kept, state: State, said: Option<&Value>) -> State {
        let (stands, closes) = {
            let mut arrived = self.arrived.lock().unwrap_or_else(PoisonError::into_inner);
            let mut back = self.back.lock().unwrap_or_else(PoisonError::into_inner);
            match state {
                State::Arrived => {
                    arrived.insert(carry.id.clone());
                    back.remove(&carry.id);
                    (State::Arrived, false)
                }
                State::Carrying
                    if arrived.contains(&carry.id)
                        || (said.is_some() && !back.contains(&carry.id)) =>
                {
                    (State::Arrived, false)
                }
                State::Back => {
                    arrived.remove(&carry.id);
                    back.insert(carry.id.clone());
                    (State::Back, true)
                }
                state => (state, back.contains(&carry.id)),
            }
        };
        if closes && said.is_some_and(|line| line["again"] == true) {
            self.close_held(carry);
        }
        stands
    }

    /// Watches `places`: where each session of a carry not settled yet
    /// runs now, by session, or none where it runs nowhere.
    pub fn watch(&self, places: HashMap<String, Option<String>>) {
        *self.watched.lock().unwrap_or_else(PoisonError::into_inner) = places;
    }

    /// Whether a watched session runs elsewhere in `runs_on` than it did
    /// when watched, so a carry may be due.
    #[must_use]
    pub fn moved(&self, runs_on: &HashMap<String, String>) -> bool {
        self.watched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|(session, was)| runs_on.get(session) != was.as_ref())
    }

    /// What each carry settled came to, by its id, as its record says it,
    /// with `at` when it was kept.
    ///
    /// # Errors
    ///
    /// When the file is there but can't be read.
    pub fn settled(&self) -> io::Result<HashMap<String, Value>> {
        let lines = match read_lines(&self.settles.kept) {
            Ok(lines) => lines,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(error) => return Err(error),
        };
        // A line torn by a write cut short is passed over, as a lost answer.
        Ok(lines
            .lines()
            .filter_map(|line| {
                let mut line: Value = serde_json::from_str(line).ok()?;
                let carry = line.as_object_mut()?.remove("carry")?;
                Some((carry.as_str()?.to_owned(), line))
            })
            .collect())
    }

    /// Settles `carry` on a thread of its own, unless it is being settled
    /// already or what settling it came to is kept and stands
    /// (`stands_at`), calling `kept` once what it came to is kept, with
    /// what to tell its session of it (`telling`). Says whether it began.
    pub fn settle(
        self: &Arc<Self>,
        carry: &Kept,
        kept: impl FnOnce(Option<String>) + Send + 'static,
    ) -> bool {
        if !self.settling().insert(carry.id.clone()) {
            return false;
        }
        // A settling keeps its line before it lets its id go, so one that
        // ended since the caller read the lines is seen here; where they
        // can't be read, nothing is settled.
        let (already, before) = match self.settled() {
            Ok(mut settled) => {
                let before = settled.remove(&carry.id);
                (
                    before
                        .as_ref()
                        .is_some_and(|said| Self::stands_yet(carry, said)),
                    before,
                )
            }
            Err(_) => (true, None),
        };
        if already {
            self.settling().remove(&carry.id);
            return false;
        }
        let settler = Arc::clone(self);
        let carry = carry.clone();
        thread::spawn(move || {
            // An answer lost stays among those settling, so it is asked
            // again only by a node started afresh, never each minute.
            let mut told = None;
            match settler.ask(&carry).map(|outcome| {
                let outcome = settler.once_back(&carry.id, outcome);
                settler.keep(&carry.id, &outcome).map(|()| outcome)
            }) {
                Some(Ok(outcome)) => {
                    settler.settling().remove(&carry.id);
                    told = telling(&carry, before.as_ref(), &outcome);
                }
                Some(Err(error)) => eprintln!(
                    "what settling {}'s carry came to can't be kept ({error}); it is asked \
                     again once the node starts afresh",
                    carry.said("address")
                ),
                None => {}
            }
            kept(told);
        });
        true
    }

    /// Whether what settling `carry` came to, `said`, stands yet, saying
    /// in the log where it doesn't, since it is asked again.
    fn stands_yet(carry: &Kept, said: &Value) -> bool {
        let stands = stands_at(said, jiff::Timestamp::now());
        if !stands {
            eprintln!(
                "asking {PI_CARRY} again to settle {}'s carry, a day after it kept its \
                         trees: {}",
                carry.said("address"),
                said["why"]
                    .as_str()
                    .unwrap_or("it said not why")
                    .escape_debug()
            );
        }
        stands
    }

    fn settling(&self) -> MutexGuard<'_, BTreeSet<String>> {
        self.settling.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Asks `pi-carry` to settle `carry`, in the node's own environment
    /// and its realm,
    /// saying in the node's log when it takes longer than it may and
    /// waiting on, never ending it part way, since a settling cut short
    /// could leave some trees gone and others kept. None where what it
    /// came to can't be told, as when a signal ended it.
    fn ask(&self, carry: &Kept) -> Option<Outcome> {
        for field in ["quest", "session"] {
            let value = carry.said(field);
            if value.is_empty() {
                return Some(Outcome::Kept(format!("it names no {field}")));
            }
            if value.starts_with('-') {
                return Some(Outcome::Kept(format!(
                    "its {field} {value} would be read as a flag"
                )));
            }
        }
        let asked = Command::new(&self.settles.program)
            .args([
                "settle",
                carry.said("quest"),
                "--session",
                carry.said("session"),
            ])
            .env("MYCELIUM_REALM", &self.settles.realm)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // What it says beside its lines goes to the node's own log.
            .stderr(Stdio::inherit())
            .spawn();
        // What asked nothing keeps no line, so a node started afresh asks
        // again once pi-carry runs and settles.
        let mut asked = match asked {
            Ok(asked) => asked,
            Err(error) => {
                eprintln!(
                    "{PI_CARRY} didn't run to settle {}'s carry: {error}; it is asked again once \
                     the node starts afresh",
                    carry.said("address")
                );
                return None;
            }
        };
        let out = asked.stdout.take();
        let (told, said) = mpsc::channel();
        thread::spawn(move || {
            let mut output = Vec::new();
            if let Some(mut out) = out {
                // A pipe that fails says nothing more, as one that ends.
                let _ = out.read_to_end(&mut output);
            }
            // Nobody hears it once the settling has been given up on.
            let _ = told.send(output);
        });
        let within = self.settles.answers_within;
        let output = said.recv_timeout(within).unwrap_or_else(|_| {
            eprintln!(
                "{PI_CARRY} settle hasn't answered for {}'s carry within {within:?}; waiting on it",
                carry.said("address")
            );
            // The reader sends once its pipe ends, which a lost one does too.
            said.recv().unwrap_or_default()
        });
        let Some(code) = asked.wait().ok().and_then(|ended| ended.code()) else {
            eprintln!(
                "{PI_CARRY} settle of {}'s carry was ended by a signal; it is asked again once \
                 the node starts afresh",
                carry.said("address")
            );
            return None;
        };
        if code == 2 || code == 64 {
            // A pi-carry that refuses the form as asked wrongly asked
            // nothing: one from before settling exits 2, and once pi-carry
            // keeps core.nix's exit table it gives 64 for that refusal.
            eprintln!(
                "{PI_CARRY} here can't settle {}'s carry, exiting {code}; it is asked again \
                 once the node starts afresh",
                carry.said("address")
            );
            return None;
        }
        Some(judged(code, &output))
    }

    /// What `outcome` of settling `id`'s carry comes to as it is kept: a
    /// held one is kept for good once its session was seen back here while
    /// `pi-carry` was asked, since nothing would close it otherwise.
    fn once_back(&self, id: &str, outcome: Outcome) -> Outcome {
        let back = self.back.lock().unwrap_or_else(PoisonError::into_inner);
        match outcome {
            Outcome::Held(_) if back.contains(id) => came_back(),
            outcome => outcome,
        }
    }

    /// Keeps `id`'s carry for good, since its session came back here, or
    /// says in the log that it can't, to be tried again at the next look.
    fn close_held(&self, carry: &Kept) {
        if let Err(error) = self.keep(&carry.id, &came_back()) {
            eprintln!(
                "{}'s carry, its session back here, can't be kept for good ({error}); it is \
                 tried again at the next look",
                carry.said("address").escape_debug()
            );
        }
    }

    /// Keeps what settling the carry `id` came to, as a line of its own,
    /// and lets each line kept more than `FADES_AFTER` ago fade (`faded`),
    /// so the file doesn't grow for good.
    fn keep(&self, id: &str, outcome: &Outcome) -> io::Result<()> {
        let now = jiff::Timestamp::now();
        let mut line = outcome.said();
        if let Some(fields) = line.as_object_mut() {
            fields.insert("carry".to_owned(), Value::from(id));
            fields.insert("at".to_owned(), Value::from(now.to_string()));
        }
        if let Some(folder) = self.settles.kept.parent() {
            fs::create_dir_all(folder)?;
        }
        // One keeping at a time, between this node's threads and any node
        // given the same state folder, so a line kept beside a rewrite
        // isn't lost. Each realm's node has a folder of its own. The lock
        // is a file of its own, since the rename replaces the lines'.
        let keeping = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.settles.kept.with_extension("jsonl.lock"))?;
        keeping.lock()?;
        let before = match read_lines(&self.settles.kept) {
            Ok(lines) => lines,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        let (mut lines, stuck) = fade(&before, now);
        lines.push_str(&line.to_string());
        lines.push('\n');
        // Written aside and moved into place, so a reader or a node
        // stopped part way sees the old lines or the new, never half.
        let aside = self.settles.kept.with_extension("jsonl.new");
        let written = File::create(&aside).and_then(|mut file| {
            file.write_all(lines.as_bytes())?;
            file.sync_all()
        });
        if let Err(error) = written.and_then(|()| fs::rename(&aside, &self.settles.kept)) {
            // What was set aside is half the lines at most; the old stand.
            let _ = fs::remove_file(&aside);
            return Err(error);
        }
        if let Some(folder) = self.settles.kept.parent() {
            // The lines are in place; a folder that won't sync only leaves
            // the rename to the system's own time.
            let _ = File::open(folder).and_then(|folder| folder.sync_all());
        }
        drop(keeping);
        if !stuck.is_empty() {
            // Said once the line is kept, and never at its cost.
            let _ = writeln!(
                io::stderr(),
                "{} keeps {} line(s) whose time can't be read or lies more than a fortnight \
                 ahead, so they don't fade; removing them by hand clears this: {}",
                self.settles.kept.display(),
                stuck.len(),
                stuck.join(", ")
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use serde_json::{Value, json};

    use super::{
        ANSWERS_WITHIN, FADES_AFTER, Fading, LOOKED_FOR, Outcome, Settler, Settles, came_back, due,
        fade, fading, judged, newest_each, stands_at, telling, to_look_at,
    };
    use crate::carry::kept::{Kept, State};

    /// How long a test waits on a settling before it fails.
    const PATIENCE: Duration = Duration::from_secs(30);

    /// HARBOR's carry from skiff to tern, ordered `hours` ago.
    fn carried(hours: i64) -> Kept {
        let at = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(hours);
        Kept {
            id: format!("s-harbour:{at}"),
            at,
            line: json!({"at": at.to_string(), "session": "s-harbour",
                         "quest": "QEST-20261115-HARBOR", "from": "skiff", "to": "tern"}),
        }
    }

    /// A `pi-carry` in `dir` running `body`, after writing down each time
    /// what it was given, a line each, in the file it answers beside it.
    fn stand_in(dir: &Path, body: &str) -> (PathBuf, PathBuf) {
        let given = dir.join("given");
        let text = dir.join("pi-carry.sh");
        let program = dir.join("pi-carry");
        let shell = std::env::var("BASH").unwrap_or_else(|_| "/bin/sh".to_owned());
        fs::write(
            &text,
            format!("#!{shell}\necho \"$*\" >> {}\n{body}\n", given.display()),
        )
        .expect("the stand-in is written");
        // A child process puts it in place, since another test's fork
        // holding the file open for writing would keep it from running.
        let put = std::process::Command::new("install")
            .args(["-m", "755"])
            .arg(&text)
            .arg(&program)
            .status()
            .expect("install runs");
        assert!(put.success(), "the stand-in is put in place");
        (program, given)
    }

    fn settler(dir: &Path, program: PathBuf, answers_within: Duration) -> Arc<Settler> {
        Arc::new(Settler::new(Settles {
            kept: dir.join("settles.jsonl"),
            program,
            realm: "lighthouse".to_owned(),
            answers_within,
        }))
    }

    #[test]
    fn pi_carry_settles_in_the_nodes_own_realm() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let realm = dir.path().join("realm");
        let (program, _given) = stand_in(
            dir.path(),
            &format!(
                "printf %s \"$MYCELIUM_REALM\" > {}\n\
                 echo '{{\"kind\":\"settled\",\"session\":\"s-harbour\",\"let\":1}}'",
                realm.display()
            ),
        );
        let settler = settler(dir.path(), program, ANSWERS_WITHIN);
        settled(&settler, &carried(30));
        assert_eq!(
            fs::read_to_string(&realm).expect("pi-carry said its realm"),
            "lighthouse",
            "pi-carry reads the realm the node serves, as the sessions it starts do"
        );
    }

    /// Settles `carry` through `settler`, waiting until what it came to is
    /// kept.
    fn settled(settler: &Arc<Settler>, carry: &Kept) -> Value {
        settled_telling(settler, carry).0
    }

    /// Settles `carry` as `settled` does, with what its session is told.
    fn settled_telling(settler: &Arc<Settler>, carry: &Kept) -> (Value, Option<String>) {
        let (told, kept) = mpsc::channel();
        assert!(
            settler.settle(carry, move |telling| told
                .send(telling)
                .expect("the test waits")),
            "it begins"
        );
        let telling = kept.recv_timeout(PATIENCE).expect("it is kept in time");
        (
            settler.settled().expect("it reads")[&carry.id].clone(),
            telling,
        )
    }

    #[test]
    fn a_session_is_told_where_its_trees_stay_and_why_once_for_each_why() {
        let carry = carried(30);
        let held = Outcome::Held("Joel's \"shell\" stands in /trees/a\u{1b}[2J".to_owned());
        let told = telling(&carry, None, &held).expect("held trees are told");
        for said in [
            "from skiff to tern",
            "left on skiff",
            "pi-carry said: Joel's \"shell\" stands in /trees/a\\u{1b}[2J.",
        ] {
            assert!(
                told.contains(said),
                "it says {said}, as written but for control characters: {told}"
            );
        }
        assert!(
            told.contains("asked again"),
            "and that they're asked for again: {told}"
        );
        assert_eq!(
            telling(&carry, Some(&held.said()), &held),
            None,
            "the same answer again isn't told again"
        );

        let kept = Outcome::Kept("a shell stands in /trees/a\u{1b}[2J".to_owned());
        let told = telling(&carry, Some(&held.said()), &kept).expect("kept for good is told");
        assert!(told.contains("for good"), "once they stay for good: {told}");
        assert!(
            telling(&carry, Some(&kept.said()), &held).is_some(),
            "as is one held again after another answer"
        );
        assert_eq!(
            telling(&carry, None, &Outcome::Settled { let_go: 2 }),
            None,
            "trees let go at first aren't told"
        );
        let since = telling(&carry, Some(&held.said()), &Outcome::Settled { let_go: 2 })
            .expect("trees told as kept and let go since are told");
        assert!(since.contains("let go since: 2"), "{since}");
        assert_eq!(
            telling(&carry, Some(&held.said()), &came_back()),
            None,
            "nor ones its session came back to"
        );
    }

    #[test]
    fn what_settling_came_to_is_told_as_it_is_kept() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (program, _given) = stand_in(
            dir.path(),
            "echo '{\"kind\":\"failed\",\"why\":\"a shell stands in /trees/a\"}'\nexit 1",
        );
        let settler = settler(dir.path(), program, PATIENCE);
        let carry = carried(50);
        let (said, told) = settled_telling(&settler, &carry);
        let told = told.expect("the session is told");
        assert!(
            told.contains("pi-carry said: a shell stands in /trees/a."),
            "the trees pi-carry kept are told as kept: {said} {told}"
        );

        // A day on, pi-carry is asked again, by this node or one started
        // afresh, and reads the earlier line from the file.
        let kept = dir.path().join("settles.jsonl");
        let a_day_ago = (jiff::Timestamp::now() - jiff::SignedDuration::from_hours(25)).to_string();
        let day_old = |why: &str| {
            format!(
                "{}\n",
                json!({"carry": carry.id, "did": "kept", "why": why, "again": true,
                       "at": a_day_ago})
            )
        };
        fs::write(&kept, day_old("a shell stands in /trees/a")).expect("a day-old line");
        let (_, again) = settled_telling(&settler, &carry);
        assert_eq!(again, None, "the same answer again isn't told again");

        fs::write(&kept, day_old("an editor stands in /trees/a")).expect("a day-old line");
        let (_, other) = settled_telling(&settler, &carry);
        assert!(
            other.is_some_and(|other| other.contains("a shell stands in")),
            "a new answer is told"
        );
    }

    #[test]
    fn nothing_is_settled_where_what_settling_came_to_cant_be_read() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (program, given) = stand_in(
            dir.path(),
            r#"echo '{"kind":"settled","session":"s-harbour","let":1}'"#,
        );
        let settler = settler(dir.path(), program, ANSWERS_WITHIN);
        // A folder where the file goes can't be read as lines.
        fs::create_dir(dir.path().join("settles.jsonl")).expect("a folder in its place");
        assert!(
            settler.settled().is_err(),
            "what settling came to can't be read"
        );
        assert!(
            !settler.settle(&carried(30), |_| {}),
            "so no carry begins settling"
        );
        assert!(!given.exists(), "and pi-carry is never asked");
    }

    #[test]
    fn a_line_kept_past_its_fading_fades_as_another_is_kept() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (program, _given) = stand_in(
            dir.path(),
            r#"echo '{"kind":"settled","session":"s-harbour","let":1}'"#,
        );
        let settler = settler(dir.path(), program, ANSWERS_WITHIN);
        let now = jiff::Timestamp::now();
        let ago = |hours: i64| (now - jiff::SignedDuration::from_hours(hours)).to_string();
        let lines = [
            json!({"carry": "s-old:1", "did": "kept", "why": "dirty",
                   "at": ago(FADES_AFTER.as_hours() + 1)}),
            json!({"carry": "s-past-the-week:1", "did": "settled", "let": 2,
                   "at": ago(LOOKED_FOR.as_hours() + 1)}),
            json!({"carry": "s-untimed:1", "did": "kept", "why": "dirty", "at": "soon"}),
        ];
        let mut text: Vec<u8> = lines
            .iter()
            .flat_map(|line| (line.to_string() + "\n").into_bytes())
            .collect();
        // A line torn part way through a character, as a write cut short
        // leaves it.
        text.extend_from_slice(b"{\"carry\":\"s-torn:1\",\"why\":\"caf\xC3");
        fs::write(dir.path().join("settles.jsonl"), text).expect("lines kept before");

        let carry = carried(30);
        settled(&settler, &carry);
        let mut kept: Vec<String> = settler.settled().expect("it reads").into_keys().collect();
        kept.sort();
        let mut expected = vec![
            carry.id.clone(),
            "s-past-the-week:1".to_owned(),
            "s-untimed:1".to_owned(),
        ];
        expected.sort();
        assert_eq!(
            kept, expected,
            "a line kept past its fading is gone; one past the week alone stays, so a clock \
             stepped back under a week brings no carry back without its line, as does one \
             whose time can't be read"
        );
        let on_disk = fs::read(dir.path().join("settles.jsonl")).expect("it is there");
        assert!(
            String::from_utf8(on_disk).is_ok_and(|text| !text.contains("s-torn")),
            "the torn line is gone with what it broke"
        );
        assert!(
            !dir.path().join("settles.jsonl.new").exists(),
            "nothing is left aside"
        );
    }

    #[test]
    fn a_line_fades_stays_or_is_stuck_by_its_time() {
        let now = jiff::Timestamp::now();
        let hours = |hours: i64| jiff::SignedDuration::from_hours(hours);
        let at = |at: String| json!({"carry": "s-harbour:1", "at": at}).to_string();
        let fortnight = FADES_AFTER.as_hours();
        for (line, becomes) in [
            ("{\"carry\":\"s-torn:1\",\"at\"".to_owned(), Fading::Faded),
            (at((now - hours(fortnight + 1)).to_string()), Fading::Faded),
            (at((now - hours(1)).to_string()), Fading::Stays),
            (at((now + hours(1)).to_string()), Fading::Stays),
            (at((now + hours(fortnight + 1)).to_string()), Fading::Stuck),
            (at(jiff::Timestamp::MAX.to_string()), Fading::Stuck),
            (at("soon".to_owned()), Fading::Stuck),
            (json!({"carry": "s-untold:1"}).to_string(), Fading::Stuck),
            (json!({"at": 7}).to_string(), Fading::Stuck),
        ] {
            assert_eq!(
                fading(&line, now),
                becomes,
                "a torn or old line fades, one kept lately stays, and one whose time can't be \
                 read, or lies past a fortnight ahead, is stuck: {line}"
            );
        }
    }

    #[test]
    fn the_lines_that_stay_are_kept_and_the_stuck_named() {
        let now = jiff::Timestamp::now();
        let old = (now - jiff::SignedDuration::from_hours(FADES_AFTER.as_hours() + 1)).to_string();
        let lately = json!({"carry": "s-lately:1", "at": now.to_string()}).to_string();
        let marked = json!({"carry": "s-\u{1b}[31m:1", "at": "soon"}).to_string();
        let unnamed = json!({"at": "soon"}).to_string();
        let before = [
            json!({"carry": "s-old:1", "at": old}).to_string(),
            lately.clone(),
            marked.clone(),
            unnamed.clone(),
        ]
        .join("\n");

        let (kept, stuck) = fade(&before, now);
        assert_eq!(
            kept,
            [lately, marked, unnamed].map(|line| line + "\n").concat(),
            "every line but the faded stays, each ending its line"
        );
        assert_eq!(
            stuck,
            ["\"s-\\u{1b}[31m:1\"", "line 4"],
            "a stuck line is named by its carry, escaped, or by its line where it names none"
        );
    }

    #[test]
    fn lines_two_nodes_keep_at_once_are_all_kept() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let program = dir.path().join("pi-carry");
        // Two nodes given one state folder keep their lines in one file.
        let nodes = [
            settler(dir.path(), program.clone(), ANSWERS_WITHIN),
            settler(dir.path(), program, ANSWERS_WITHIN),
        ];
        let keeping: Vec<_> = (0..8)
            .map(|each| {
                let node = Arc::clone(&nodes[each % 2]);
                std::thread::spawn(move || {
                    for one in 0..10 {
                        node.keep(&format!("s-{each}:{one}"), &Outcome::Settled { let_go: 1 })
                            .expect("it is kept");
                    }
                })
            })
            .collect();
        for each in keeping {
            each.join().expect("it keeps");
        }
        assert_eq!(
            nodes[0].settled().expect("it reads").len(),
            80,
            "no line is lost to another kept beside it"
        );
    }

    #[test]
    fn a_session_carried_again_is_settled_by_its_later_carry_alone() {
        let earlier = carried(50);
        let later = carried(2);
        let mut other = carried(30);
        other.line["session"] = json!("s-reef");
        other.id = format!("s-reef:{}", other.at);
        let ids = |carries: Vec<Kept>| -> Vec<String> {
            carries.into_iter().map(|carry| carry.id).collect()
        };
        assert_eq!(
            ids(newest_each(vec![
                earlier.clone(),
                other.clone(),
                later.clone()
            ])),
            ids(vec![later.clone(), other.clone()]),
            "the earlier carry, back where its later one went, waits on the later one's day"
        );
        assert!(
            due(&earlier, &State::Arrived, "skiff", jiff::Timestamp::now()),
            "which it would be due before, judged arrived again"
        );
        let mut elsewhere = carried(40);
        elsewhere.line["session"] = json!("s-tide");
        elsewhere.line["from"] = json!("kestrel");
        elsewhere.id = format!("s-tide:{}", elsewhere.at);
        let settled = HashMap::from([(other.id.clone(), json!({"did": "settled"}))]);
        assert_eq!(
            ids(to_look_at(
                vec![earlier, other, later.clone(), elsewhere],
                &settled,
                "skiff"
            )),
            ids(vec![later]),
            "the node looks only at each session's newest, from here and not settled yet"
        );
    }

    #[test]
    fn a_carry_whose_trees_pi_carry_kept_is_still_looked_at() {
        let held = carried(30);
        let mut kept = carried(40);
        kept.line["session"] = json!("s-reef");
        kept.id = format!("s-reef:{}", kept.at);
        let settled = HashMap::from([
            (held.id.clone(), json!({"did": "kept", "again": true})),
            (kept.id.clone(), json!({"did": "kept"})),
        ]);
        let looked_at: Vec<String> = to_look_at(vec![held.clone(), kept], &settled, "skiff")
            .into_iter()
            .map(|carry| carry.id)
            .collect();
        assert_eq!(
            looked_at,
            vec![held.id],
            "one pi-carry held is looked at, one the node kept for good isn't"
        );
    }

    #[test]
    fn a_kept_answer_stands_a_day_unless_it_was_for_good() {
        let now = jiff::Timestamp::now();
        let ago = |hours: i64| (now - jiff::SignedDuration::from_hours(hours)).to_string();
        for (said, stands, why) in [
            (
                json!({"again": true, "at": ago(23)}),
                true,
                "within its day",
            ),
            (json!({"again": true, "at": ago(25)}), false, "a day after"),
            (
                json!({"again": true, "at": ago(24)}),
                false,
                "a day to the moment",
            ),
            (
                json!({"again": true, "at": "a while"}),
                true,
                "an unreadable time",
            ),
            (json!({"again": true}), true, "no time"),
            (json!({"at": ago(25)}), true, "kept for good"),
        ] {
            assert_eq!(stands_at(&said, now), stands, "{why}: {said}");
        }
    }

    #[test]
    fn only_a_watched_session_moving_asks_to_look_again() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let settler = settler(dir.path(), dir.path().join("pi-carry"), ANSWERS_WITHIN);
        let runs_on = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs
                .iter()
                .map(|(session, machine)| ((*session).to_owned(), (*machine).to_owned()))
                .collect()
        };
        settler.watch(HashMap::from([("s-harbour".to_owned(), None)]));
        assert!(
            !settler.moved(&runs_on(&[("s-other", "tern")])),
            "another session coming asks nothing"
        );
        assert!(
            settler.moved(&runs_on(&[("s-harbour", "tern")])),
            "the carried session arriving asks to look again"
        );
        settler.watch(HashMap::from([(
            "s-harbour".to_owned(),
            Some("tern".to_owned()),
        )]));
        assert!(
            !settler.moved(&runs_on(&[("s-harbour", "tern"), ("s-other", "kestrel")])),
            "another session moving asks nothing"
        );
        assert!(
            settler.moved(&runs_on(&[])),
            "the carried session ending asks to look again"
        );
    }

    #[test]
    fn only_a_carry_from_here_arrived_and_ordered_a_day_since_is_settled() {
        let now = jiff::Timestamp::now();
        let day_old = carried(25);
        assert!(
            due(&day_old, &State::Arrived, "skiff", now),
            "arrived on tern, and ordered a day since"
        );
        for (carry, state, here, why) in [
            (carried(23), State::Arrived, "skiff", "within its day"),
            (
                carried(25),
                State::Arrived,
                "tern",
                "carried from elsewhere",
            ),
            (carried(25), State::Back, "skiff", "back here"),
            (
                carried(25),
                State::Refused("it holds a lease".to_owned()),
                "skiff",
                "refused",
            ),
            (carried(25), State::Ordered, "skiff", "never gone"),
            (carried(25), State::Carrying, "skiff", "not seen arriving"),
        ] {
            assert!(!due(&carry, &state, here, now), "not settled: {why}");
        }
    }

    #[test]
    fn what_a_settling_came_to_is_read_from_its_lines_and_its_end() {
        let tree = r#"{"kind":"tree","path":"/trees/a","did":"let go"}"#;
        let settled =
            format!("{tree}\n{{\"kind\":\"settled\",\"session\":\"s-harbour\",\"let\":2}}\n");
        assert_eq!(
            judged(0, settled.as_bytes()),
            Outcome::Settled { let_go: 2 }
        );
        let failed = format!(
            "{tree}\n{{\"kind\":\"tree\",\"path\":\"/trees/b\",\"did\":\"stays\",\"why\":\"a commit since\"}}\n\
             {{\"kind\":\"failed\",\"why\":\"/trees/b holds work since the carry\"}}\n"
        );
        assert_eq!(
            judged(1, failed.as_bytes()),
            Outcome::Held("/trees/b holds work since the carry".to_owned())
        );
        assert_eq!(
            judged(1, tree.as_bytes()),
            Outcome::Held("pi-carry settle kept the trees without saying why".to_owned())
        );
        for (code, output, why) in [
            (0, tree, "pi-carry settle ended saying nothing it settled"),
            (3, "", "pi-carry settle ended with 3"),
        ] {
            assert_eq!(
                judged(code, output.as_bytes()),
                Outcome::Held(why.to_owned()),
                "ending {code:?}, asked again a day after"
            );
        }
    }

    #[test]
    fn trees_pi_carry_kept_are_asked_for_again_a_day_after_and_others_never() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (program, given) = stand_in(
            dir.path(),
            "echo '{\"kind\":\"failed\",\"why\":\"a shell stands in /trees/a\"}'\nexit 1",
        );
        let settler = settler(dir.path(), program, PATIENCE);
        let carry = carried(50);
        let said = settled(&settler, &carry);
        assert_eq!(
            (&said["did"], &said["why"], &said["again"]),
            (
                &json!("kept"),
                &json!("a shell stands in /trees/a"),
                &json!(true)
            ),
            "{said}"
        );
        assert!(
            !settler.settle(&carry, |_| ()),
            "within a day of being kept, it isn't asked again"
        );
        let asked = || fs::read_to_string(&given).expect("it ran").lines().count();
        assert_eq!(asked(), 1, "asked once so far");

        let kept_at = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(25);
        let line = |again: bool| {
            let mut line = json!({"carry": carry.id, "did": "kept", "why": "held",
                                  "at": kept_at.to_string()});
            if again {
                line["again"] = json!(true);
            }
            format!("{line}\n")
        };
        let kept = dir.path().join("settles.jsonl");
        fs::write(&kept, line(true)).expect("a day-old line");
        let again = settled(&settler, &carry);
        assert_eq!(
            asked(),
            2,
            "a day after pi-carry kept them, it is asked again"
        );
        assert_ne!(again["at"], json!(kept_at.to_string()), "{again}");
        assert!(
            !settler.settle(&carry, |_| ()),
            "its newest answer stands, not the day-old one"
        );
        assert_eq!(asked(), 2, "so it isn't asked a third time");

        fs::write(&kept, line(false)).expect("a day-old line of the node's own");
        assert!(
            !settler.settle(&carry, |_| ()),
            "trees the node kept without asking are never asked for again"
        );
        assert_eq!(asked(), 2, "and pi-carry isn't asked");
    }

    #[test]
    fn a_carry_is_settled_with_its_quest_and_session_and_kept_for_good() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (program, given) = stand_in(
            dir.path(),
            r#"echo '{"kind":"settled","session":"s-harbour","let":2}'"#,
        );
        let carry = carried(25);
        let said = settled(&settler(dir.path(), program.clone(), PATIENCE), &carry);
        assert_eq!(
            (&said["did"], &said["let"]),
            (&json!("settled"), &json!(2)),
            "{said}"
        );
        assert!(said["at"].as_str().is_some(), "said when: {said}");
        assert_eq!(
            fs::read_to_string(&given).expect("it ran"),
            "settle QEST-20261115-HARBOR --session s-harbour\n",
            "asked with the quest and --session only"
        );

        let afresh = settler(dir.path(), program, PATIENCE);
        assert_eq!(
            afresh.settled().expect("it reads")[&carry.id],
            said,
            "a node started afresh reads it as kept"
        );
        assert!(!afresh.settle(&carry, |_| ()), "and never settles it again");
        assert_eq!(
            fs::read_to_string(&given).expect("it ran").lines().count(),
            1,
            "asked once"
        );
    }

    #[test]
    fn a_settling_that_doesnt_answer_in_time_is_waited_on_and_never_asked_twice_at_once() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        // It answers well after its time, as a slow disk would let it.
        let (program, given) = stand_in(
            dir.path(),
            r#"sleep 0.5
 echo '{"kind":"settled","session":"s-harbour","let":1}'"#,
        );
        let settler = settler(dir.path(), program, Duration::from_millis(50));
        let carry = carried(25);
        let (told, kept) = mpsc::channel();
        assert!(
            settler.settle(&carry, move |_| told.send(()).expect("the test waits")),
            "it begins"
        );
        assert!(
            !settler.settle(&carry, |_| ()),
            "it isn't begun again while it runs"
        );
        kept.recv_timeout(PATIENCE).expect("its answer is kept");
        let said = settler.settled().expect("it reads")[&carry.id].clone();
        assert_eq!(
            (&said["did"], &said["let"]),
            (&json!("settled"), &json!(1)),
            "what it came to, since it was waited on rather than ended: {said}"
        );
        assert_eq!(
            fs::read_to_string(&given).expect("it ran").lines().count(),
            1,
            "asked once"
        );
    }

    /// Settles a fresh carry with `program` in `dir` to its end, and
    /// finds nothing of it kept for good, nor it asked again while this
    /// node runs. Answers the carry, for a node started afresh to ask.
    fn ends_keeping_nothing(dir: &Path, program: &Path) -> Kept {
        let running = settler(dir, program.to_owned(), PATIENCE);
        let carry = carried(25);
        let (told, ended) = mpsc::channel();
        assert!(
            running.settle(&carry, move |_| told.send(()).expect("the test waits")),
            "it begins"
        );
        ended.recv_timeout(PATIENCE).expect("it ends in time");
        assert!(
            !running.settled().expect("it reads").contains_key(&carry.id),
            "what {} came to isn't known, so nothing is kept for good",
            program.display()
        );
        assert!(
            !running.settle(&carry, |_| ()),
            "nor is it asked again while this node runs"
        );
        carry
    }

    #[test]
    fn a_settling_ended_by_a_signal_is_kept_as_nothing_and_asked_again_only_afresh() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        // It may have let some trees go, as a node stopping would end it.
        let (program, given) = stand_in(dir.path(), "kill -9 $$");
        let carry = ends_keeping_nothing(dir.path(), &program);
        assert_eq!(
            fs::read_to_string(&given).expect("it ran").lines().count(),
            1,
            "asked once"
        );
        assert!(
            settler(dir.path(), program, PATIENCE).settle(&carry, |_| ()),
            "a node started afresh asks it again, which settling allows"
        );
    }

    #[test]
    fn a_carry_naming_a_flag_or_nothing_is_kept_unasked() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (program, given) = stand_in(
            dir.path(),
            r#"echo '{"kind":"settled","session":"s-harbour","let":2}'"#,
        );
        for (field, value) in [("session", "--all"), ("quest", "-f")] {
            let mut carry = carried(25);
            carry.line[field] = json!(value);
            carry.id = format!("{value}:{}", carry.at);
            let said = settled(&settler(dir.path(), program.clone(), PATIENCE), &carry);
            assert_eq!(
                (&said["did"], &said["why"], &said["again"]),
                (
                    &json!("kept"),
                    &json!(format!("its {field} {value} would be read as a flag")),
                    &Value::Null
                ),
                "kept for good: {said}"
            );
        }
        for field in ["session", "quest"] {
            let mut carry = carried(25);
            carry.line[field] = json!("");
            carry.id = format!("no-{field}:{}", carry.at);
            let said = settled(&settler(dir.path(), program.clone(), PATIENCE), &carry);
            assert_eq!(
                (&said["did"], &said["why"], &said["again"]),
                (
                    &json!("kept"),
                    &json!(format!("it names no {field}")),
                    &Value::Null
                ),
                "kept for good: {said}"
            );
        }
        assert!(!given.exists(), "pi-carry is never asked");
    }

    #[test]
    fn a_settling_that_asks_nothing_keeps_no_line_and_is_asked_again_afresh() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        // One that can't run, and two that refuse the form as asked
        // wrongly: with 2, as one from before settling does, and with 64,
        // as core.nix's exit table spells it. Each keeps a folder of its own.
        let mut programs = vec![(dir.path().join("no-such"), None)];
        for code in [2, 64] {
            let own = dir.path().join(format!("exits-{code}"));
            fs::create_dir(&own).expect("the stand-in's folder is made");
            let (program, given) = stand_in(&own, &format!("exit {code}"));
            programs.push((program, Some(given)));
        }
        for (program, given) in programs {
            let carry = ends_keeping_nothing(dir.path(), &program);
            // A stand-in that never ran would end the same way as one
            // that can't run, so each that refuses must have been asked.
            if let Some(given) = given {
                assert!(given.exists(), "{} ran", program.display());
            }
            assert!(
                settler(dir.path(), program, PATIENCE).settle(&carry, |_| ()),
                "a node started afresh asks it again"
            );
        }
    }

    #[test]
    fn a_settling_ending_with_another_code_is_held_saying_it() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (program, given) = stand_in(dir.path(), "exit 3");
        let said = settled(&settler(dir.path(), program, PATIENCE), &carried(40));
        assert!(given.exists(), "pi-carry is asked");
        assert_eq!(
            (&said["did"], &said["why"], &said["again"]),
            (
                &json!("kept"),
                &json!("pi-carry settle ended with 3"),
                &json!(true)
            ),
            "only 2 and 64 ask nothing: {said}"
        );
    }

    #[test]
    fn a_carry_seen_arrived_stands_arrived_once_its_session_runs_nowhere() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let settler = settler(dir.path(), dir.path().join("pi-carry"), PATIENCE);
        let carry = carried(25);
        assert_eq!(
            settler.stands(&carry, State::Carrying, None),
            State::Carrying,
            "one never seen arrived is still carrying"
        );
        assert_eq!(settler.stands(&carry, State::Arrived, None), State::Arrived);
        assert_eq!(
            settler.stands(&carry, State::Carrying, None),
            State::Arrived,
            "once seen arrived, its session ending there leaves it arrived"
        );
        assert_eq!(
            settler.stands(&carry, State::Back, None),
            State::Back,
            "but its session seen back here is back"
        );
        assert_eq!(
            settler.stands(&carry, State::Carrying, None),
            State::Carrying,
            "and once back, its session ending here is no arrival"
        );
        assert_eq!(
            settler.stands(&carried(30), State::Carrying, None),
            State::Carrying,
            "and another carry is judged on its own"
        );
    }

    #[test]
    fn a_carry_asked_of_before_stands_arrived_to_a_node_started_afresh() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let settler = settler(dir.path(), dir.path().join("pi-carry"), PATIENCE);
        let carry = carried(25);
        let settled_for_good = carried(30);
        settler
            .keep(&carry.id, &Outcome::Held("a tree holds work".into()))
            .expect("its held line is kept");
        settler
            .keep(&settled_for_good.id, &Outcome::Settled { let_go: 2 })
            .expect("its settled line is kept");
        let lines = settler.settled().expect("it reads");
        assert_eq!(
            settler.stands(&carry, State::Carrying, lines.get(&carry.id)),
            State::Arrived,
            "its session ended where it went, as when it was asked of"
        );
        assert_eq!(
            settler.stands(
                &settled_for_good,
                State::Back,
                lines.get(&settled_for_good.id)
            ),
            State::Back
        );
        assert_eq!(
            settler.settled().expect("it reads")[&settled_for_good.id]["did"],
            json!("settled"),
            "a carry settled for good keeps its line though its session came back"
        );
        assert_eq!(
            settler.stands(&carry, State::Back, lines.get(&carry.id)),
            State::Back
        );
        assert_eq!(
            settler.stands(&carry, State::Carrying, lines.get(&carry.id)),
            State::Carrying,
            "once seen back here, it is no arrival"
        );
        let said = settler.settled().expect("it reads")[&carry.id].clone();
        assert_eq!(
            (&said["did"], &said["again"]),
            (&json!("kept"), &Value::Null),
            "its session back here is kept for good, so a restart doesn't forget it: {said}"
        );
        assert!(
            to_look_at(vec![carry], &settler.settled().expect("it reads"), "skiff").is_empty(),
            "and it is looked at no more"
        );
    }

    #[test]
    fn a_held_carry_seen_back_is_kept_for_good_though_its_session_ended_here_since() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let settler = settler(dir.path(), dir.path().join("pi-carry"), PATIENCE);
        let carry = carried(25);
        // Seen back before its held line was read, as when the line is
        // kept just after that look.
        assert_eq!(settler.stands(&carry, State::Back, None), State::Back);
        settler
            .keep(&carry.id, &Outcome::Held("a tree holds work".into()))
            .expect("its held line is kept");
        let lines = settler.settled().expect("it reads");
        assert_eq!(
            settler.stands(&carry, State::Carrying, lines.get(&carry.id)),
            State::Carrying,
            "its session ended here"
        );
        let said = settler.settled().expect("it reads")[&carry.id].clone();
        assert_eq!(
            (&said["did"], &said["again"]),
            (&json!("kept"), &Value::Null),
            "kept for good at the next look: {said}"
        );
    }

    #[test]
    fn a_held_answer_kept_once_its_session_came_back_is_kept_for_good() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let gate = dir.path().join("gate");
        let made = std::process::Command::new("mkfifo")
            .arg(&gate)
            .status()
            .expect("mkfifo runs");
        assert!(made.success(), "the gate is made");
        // It answers only once the test opens the gate, so its session is
        // seen back here while it is asked.
        let (program, _given) = stand_in(
            dir.path(),
            &format!(
                "cat {} > /dev/null\necho '{{\"kind\":\"failed\",\"why\":\"a tree holds work\"}}'\nexit 1",
                gate.display()
            ),
        );
        let settler = settler(dir.path(), program, PATIENCE);
        let carry = carried(25);
        let (told, kept) = mpsc::channel();
        assert!(
            settler.settle(&carry, move |_| told.send(()).expect("the test waits")),
            "it begins"
        );
        assert_eq!(settler.stands(&carry, State::Back, None), State::Back);
        // Opened off the test's thread, so a pi-carry that never reads the
        // gate fails the test in time rather than hanging it.
        std::thread::spawn(move || {
            drop(fs::OpenOptions::new().write(true).open(&gate));
        });
        kept.recv_timeout(PATIENCE).expect("it is kept in time");
        let said = settler.settled().expect("it reads")[&carry.id].clone();
        assert_eq!(
            (&said["did"], &said["why"], &said["again"]),
            (
                &json!("kept"),
                &json!("its session came back here"),
                &Value::Null
            ),
            "its trees are its session's again, so never asked for: {said}"
        );
    }
}
