//! Letting a carried session's old trees go on the machine it left.
//!
//! A session an orchestrator carried leaves its worktrees behind here.
//! Once the node judges the carry arrived, its session seen running on the
//! machine it went to, and a day has passed since it was ordered, the node
//! asks `pi-carry settle <quest> --session <session>` once, which lets go
//! of each tree holding nothing the carry didn't take, and keeps every one
//! otherwise, saying why. What it answered is kept beside the carries as a
//! line of `settles.jsonl`, so no carry is settled twice, even by a node
//! started afresh. An answer lost, to a signal ending the settling or a
//! line that can't be kept, is asked again only by a node started afresh,
//! which settling allows. A carry whose session is seen back here isn't
//! due, nor is any but each session's newest carry, so a session carried
//! back and on again is settled by its later carry alone.

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, OpenOptions};
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

/// Where a node keeps what each settling answered, and what it asks.
#[derive(Debug, Clone)]
pub struct Settles {
    /// The file of what each settling answered, a line each.
    pub kept: PathBuf,
    /// The program asked, `pi-carry` on the node's `PATH`.
    pub program: PathBuf,
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
    /// The trees stay, saying why.
    Kept(String),
}

impl Outcome {
    /// What a carry's record says of it, as `settle`.
    #[must_use]
    pub fn said(&self) -> Value {
        match self {
            Self::Settled { let_go } => json!({"did": "settled", "let": let_go}),
            Self::Kept(why) => json!({"did": "kept", "why": why}),
        }
    }
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
/// of each session's, carried from here, and not `settled` yet, by id.
#[must_use]
pub fn to_look_at(carries: Vec<Kept>, settled: impl Fn(&str) -> bool, here: &str) -> Vec<Kept> {
    let mut carries = newest_each(carries);
    carries.retain(|carry| !settled(&carry.id) && carry.said("from") == here);
    carries
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
                || Outcome::Kept(format!("{PI_CARRY} settle ended saying nothing it settled")),
                |let_go| Outcome::Settled { let_go },
            ),
        1 => Outcome::Kept(
            last("failed")
                .and_then(|line| line["why"].as_str())
                .map_or_else(
                    || format!("{PI_CARRY} settle kept the trees without saying why"),
                    str::to_owned,
                ),
        ),
        code => Outcome::Kept(format!("{PI_CARRY} settle ended with {code}")),
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
        }
    }

    /// How `carry`, judged `state` now, stands for settling: arrived once
    /// its session was seen where it went, though it runs nowhere now, as
    /// when it ended there, until its session is seen back here. A node
    /// started afresh has seen none arrive.
    pub fn stands(&self, carry: &Kept, state: State) -> State {
        let mut arrived = self.arrived.lock().unwrap_or_else(PoisonError::into_inner);
        match state {
            State::Arrived => {
                arrived.insert(carry.id.clone());
                State::Arrived
            }
            State::Carrying if arrived.contains(&carry.id) => State::Arrived,
            State::Back => {
                arrived.remove(&carry.id);
                State::Back
            }
            state => state,
        }
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
        let lines = match fs::read_to_string(&self.settles.kept) {
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
    /// already or what settling it came to is kept, calling `kept` once
    /// what it came to is kept. Says whether it began.
    pub fn settle(self: &Arc<Self>, carry: &Kept, kept: impl FnOnce() + Send + 'static) -> bool {
        if !self.settling().insert(carry.id.clone()) {
            return false;
        }
        // A settling keeps its line before it lets its id go, so one that
        // ended since the caller read the lines is seen here; where they
        // can't be read, nothing is settled.
        let already = match self.settled() {
            Ok(settled) => settled.contains_key(&carry.id),
            Err(_) => true,
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
            match settler
                .ask(&carry)
                .map(|outcome| settler.keep(&carry.id, &outcome))
            {
                Some(Ok(())) => {
                    settler.settling().remove(&carry.id);
                }
                Some(Err(error)) => eprintln!(
                    "what settling {}'s carry came to can't be kept ({error}); it is asked \
                     again once the node starts afresh",
                    carry.said("address")
                ),
                None => {}
            }
            kept();
        });
        true
    }

    fn settling(&self) -> MutexGuard<'_, BTreeSet<String>> {
        self.settling.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Asks `pi-carry` to settle `carry`, in the node's own environment,
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
        if code == 2 {
            // A pi-carry from before settling refuses the form as asked
            // wrongly, so nothing was asked.
            eprintln!(
                "{PI_CARRY} here can't settle {}'s carry; it is asked again once the node \
                 starts afresh",
                carry.said("address")
            );
            return None;
        }
        Some(judged(code, &output))
    }

    /// Keeps what settling the carry `id` came to, as a line of its own.
    fn keep(&self, id: &str, outcome: &Outcome) -> io::Result<()> {
        let mut line = outcome.said();
        if let Some(fields) = line.as_object_mut() {
            fields.insert("carry".to_owned(), Value::from(id));
            fields.insert(
                "at".to_owned(),
                Value::from(jiff::Timestamp::now().to_string()),
            );
        }
        if let Some(folder) = self.settles.kept.parent() {
            fs::create_dir_all(folder)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.settles.kept)?;
        // One write a line, so two settlings kept at once never interleave.
        file.write_all(format!("{line}\n").as_bytes())
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

    use super::{ANSWERS_WITHIN, Outcome, Settler, Settles, due, judged, newest_each, to_look_at};
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
            answers_within,
        }))
    }

    /// Settles `carry` through `settler`, waiting until what it came to is
    /// kept.
    fn settled(settler: &Arc<Settler>, carry: &Kept) -> Value {
        let (told, kept) = mpsc::channel();
        assert!(
            settler.settle(carry, move || told.send(()).expect("the test waits")),
            "it begins"
        );
        kept.recv_timeout(PATIENCE).expect("it is kept in time");
        settler.settled().expect("it reads")[&carry.id].clone()
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
            !settler.settle(&carried(30), || {}),
            "so no carry begins settling"
        );
        assert!(!given.exists(), "and pi-carry is never asked");
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
        let settled = other.id.clone();
        assert_eq!(
            ids(to_look_at(
                vec![earlier, other, later.clone(), elsewhere],
                |id| id == settled,
                "skiff"
            )),
            ids(vec![later]),
            "the node looks only at each session's newest, from here and not settled yet"
        );
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
            Outcome::Kept("/trees/b holds work since the carry".to_owned())
        );
        for (code, output, why) in [
            (0, tree, "pi-carry settle ended saying nothing it settled"),
            (1, tree, "pi-carry settle kept the trees without saying why"),
            (3, "", "pi-carry settle ended with 3"),
        ] {
            assert_eq!(
                judged(code, output.as_bytes()),
                Outcome::Kept(why.to_owned()),
                "ending {code:?}"
            );
        }
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
        assert!(!afresh.settle(&carry, || ()), "and never settles it again");
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
            settler.settle(&carry, move || told.send(()).expect("the test waits")),
            "it begins"
        );
        assert!(
            !settler.settle(&carry, || ()),
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
            running.settle(&carry, move || told.send(()).expect("the test waits")),
            "it begins"
        );
        ended.recv_timeout(PATIENCE).expect("it ends in time");
        assert!(
            !running.settled().expect("it reads").contains_key(&carry.id),
            "what {} came to isn't known, so nothing is kept for good",
            program.display()
        );
        assert!(
            !running.settle(&carry, || ()),
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
            settler(dir.path(), program, PATIENCE).settle(&carry, || ()),
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
                (&said["did"], &said["why"]),
                (
                    &json!("kept"),
                    &json!(format!("its {field} {value} would be read as a flag"))
                ),
                "{said}"
            );
        }
        for field in ["session", "quest"] {
            let mut carry = carried(25);
            carry.line[field] = json!("");
            carry.id = format!("no-{field}:{}", carry.at);
            let said = settled(&settler(dir.path(), program.clone(), PATIENCE), &carry);
            assert_eq!(
                (&said["did"], &said["why"]),
                (&json!("kept"), &json!(format!("it names no {field}"))),
                "{said}"
            );
        }
        assert!(!given.exists(), "pi-carry is never asked");
    }

    #[test]
    fn a_settling_that_asks_nothing_keeps_no_line_and_is_asked_again_afresh() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        // One that can't run, and one from before settling, which refuses
        // the form as asked wrongly.
        let (too_old, _) = stand_in(dir.path(), "exit 2");
        for program in [dir.path().join("no-such"), too_old] {
            let carry = ends_keeping_nothing(dir.path(), &program);
            assert!(
                settler(dir.path(), program, PATIENCE).settle(&carry, || ()),
                "a node started afresh asks it again"
            );
        }
    }

    #[test]
    fn a_carry_seen_arrived_stands_arrived_once_its_session_runs_nowhere() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let settler = settler(dir.path(), dir.path().join("pi-carry"), PATIENCE);
        let carry = carried(25);
        assert_eq!(
            settler.stands(&carry, State::Carrying),
            State::Carrying,
            "one never seen arrived is still carrying"
        );
        assert_eq!(settler.stands(&carry, State::Arrived), State::Arrived);
        assert_eq!(
            settler.stands(&carry, State::Carrying),
            State::Arrived,
            "once seen arrived, its session ending there leaves it arrived"
        );
        assert_eq!(
            settler.stands(&carry, State::Back),
            State::Back,
            "but its session seen back here is back"
        );
        assert_eq!(
            settler.stands(&carry, State::Carrying),
            State::Carrying,
            "and once back, its session ending here is no arrival"
        );
        assert_eq!(
            settler.stands(&carried(30), State::Carrying),
            State::Carrying,
            "and another carry is judged on its own"
        );
    }
}
