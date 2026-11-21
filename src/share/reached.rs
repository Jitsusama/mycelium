//! Whether a shared quest has reached another machine: its node said it
//! holds every file this end holds of the quest's record and its named
//! scratch, at this end's version or newer, so a session started there
//! finds all it needs.
//!
//! What the other end holds is what its link last said, kept in the
//! quest's ledger ([`super::ledger::Ledger::theirs`]). What this end holds
//! is read as its link's next look will say it, from a copy of its ledger
//! that is never kept, so the look still finds and says each change.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::exchange::Places;
use super::ledger::shared_of;
use super::settle::State;
use super::version::Standing;
use crate::client::Client;
use crate::facts::Readiness;
use crate::wire::{Request, Response};

/// How long a wait gives a link to reach the machine, and the machine to
/// take the quest up once one does, before saying which it lacks: a link
/// looks once a second, so a few seconds is ample.
pub const SAYS_WITHIN: Duration = Duration::from_secs(5);

/// How long a wait gives the machine to hold the quest unless told.
pub const WITHIN: Duration = Duration::from_secs(120);

/// The most files a wait names as lacking.
const MOST_NAMED: usize = 20;

/// Why a quest hasn't reached a machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// It couldn't be shared at all, or not with that machine.
    Refused,
    /// No link reaches the machine.
    NoLink,
    /// The machine's node hasn't said it shares the quest.
    NotShared,
    /// The machine shares it, but lacks some of its files at this end's
    /// versions.
    Behind,
}

/// What a wait for a quest to reach a machine found, as a JSON line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reached {
    /// The quest's id.
    pub quest: String,
    /// The machine it was to reach.
    pub machine: String,
    /// Whether the machine holds every file.
    pub held: bool,
    /// How many files this end holds.
    pub files: usize,
    /// How many of them the machine holds at this end's version or newer.
    pub holding: usize,
    /// The folders of the quest's scratch that are shared.
    pub scratch: Vec<String>,
    /// How long it waited, in seconds.
    pub waited: f64,
    /// Some of the files the machine lacks, by key, at most twenty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lacking: Vec<String>,
    /// Why it isn't held.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
    /// The same, in words.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

impl Reached {
    /// A wait refused before it began, as `why` says.
    #[must_use]
    pub fn refused(quest: &str, machine: &str, why: String) -> Self {
        Self {
            quest: quest.to_owned(),
            machine: machine.to_owned(),
            held: false,
            files: 0,
            holding: 0,
            scratch: Vec::new(),
            waited: 0.0,
            lacking: Vec::new(),
            reason: Some(Reason::Refused),
            why: Some(why),
        }
    }
}

/// What a wait looks at beyond the quest's files: whether a link reaches
/// a machine, and the time.
pub trait Watching {
    /// Whether a link reaches `machine` now.
    fn linked(&mut self, machine: &str) -> bool;
    /// Whether the link withholds the quests above a shared one from
    /// `machine`, as it does from a worker, or from a machine that
    /// declares nothing of itself, so a wait counts none of their READMEs.
    fn withholds_above(&mut self, machine: &str) -> bool;
    /// How long since the wait began.
    fn waited(&self) -> Duration;
    /// Lets a moment pass before the next look.
    fn pause(&mut self);
}

/// Every file this end holds of `quest`, by key, as its link's next look
/// will say it, with the version a change not yet looked at will have.
pub fn holds_here(
    places: &Places,
    quest: &str,
    machine: &str,
) -> io::Result<BTreeMap<String, State>> {
    // Copied and let go, never kept: a change entered here would be one
    // the link's look finds nothing new in, and so never says.
    let mut ledger = places.ledgers().hold(quest)?.ledger;
    // A record waiting to move aside is one the look never says.
    let shared = places.quest(quest, &ledger).record_waits(ledger.aside);
    ledger.scan(&shared, machine)?;
    Ok(ledger
        .files
        .into_iter()
        .filter(|(key, entry)| {
            entry.hash.is_some() && shared_of(key).is_some_and(|one| shared.carries(&one))
        })
        .map(|(key, entry)| (key, entry.state()))
        .collect())
}

/// Every file a wait for `quest` counts, by key: its own, as
/// [`holds_here`], and the README of each quest above it, by that quest's
/// id and its key, as `QEST-.../record/README.md`.
pub fn awaited(places: &Places, quest: &str, machine: &str) -> io::Result<BTreeMap<String, State>> {
    let mut files = holds_here(places, quest, machine)?;
    for parent in places.above(quest) {
        if let Some(readme) = holds_here(places, &parent, machine)?.remove(README) {
            files.insert(format!("{parent}/{README}"), readme);
        }
    }
    Ok(files)
}

/// A quest's README, by its key.
const README: &str = "record/README.md";

/// What `to` last said it holds of `quest`, if it said it shares it, and of
/// the READMEs of the quests above it, keyed as [`awaited`] keys them.
fn held_there(
    places: &Places,
    quest: &str,
    to: &str,
    above: bool,
) -> io::Result<Option<BTreeMap<String, State>>> {
    let theirs = |id: &str| -> io::Result<Option<BTreeMap<String, State>>> {
        Ok(places.ledgers().hold(id)?.ledger.theirs.remove(to))
    };
    let Some(mut files) = theirs(quest)? else {
        return Ok(None);
    };
    if !above {
        return Ok(Some(files));
    }
    for parent in places.above(quest) {
        if let Some(readme) = theirs(&parent)?.and_then(|mut held| held.remove(README)) {
            files.insert(format!("{parent}/{README}"), readme);
        }
    }
    Ok(Some(files))
}

/// The keys of `here` that `there` doesn't hold at that version or newer.
#[must_use]
pub fn lacking(here: &BTreeMap<String, State>, there: &BTreeMap<String, State>) -> Vec<String> {
    here.iter()
        .filter(|(key, ours)| {
            !there.get(*key).is_some_and(|theirs| {
                matches!(
                    theirs.version.against(&ours.version),
                    Standing::Same | Standing::Newer
                )
            })
        })
        .map(|(key, _)| key.clone())
        .collect()
}

/// Waits, as long as `within`, for `quest`, shared from `machine`, to
/// reach `to`, saying so within [`SAYS_WITHIN`] when no link reaches it
/// or it doesn't take the quest up. The READMEs of the quests above it
/// are waited for only while the link serves them to `to`, so a worker,
/// served the quest alone, holds it once it holds the quest's own files.
pub fn reach(
    places: &Places,
    quest: &str,
    (machine, to): (&str, &str),
    within: Duration,
    watching: &mut impl Watching,
) -> io::Result<Reached> {
    let whole = awaited(places, quest, machine)?;
    let own = holds_here(places, quest, machine)?;
    let scratch: Vec<String> = places
        .ledgers()
        .hold(quest)?
        .ledger
        .scratch
        .into_iter()
        .collect();
    // When a link last didn't reach `to`, and when one first did.
    let mut unlinked_since = None;
    let mut linked_since = None;
    loop {
        let waited = watching.waited();
        let above = !watching.withholds_above(to);
        let here = if above { &whole } else { &own };
        let theirs = held_there(places, quest, to, above)?;
        let lacks = lacking(here, theirs.as_ref().unwrap_or(&BTreeMap::new()));
        let reached = |reason: Option<Reason>, why: Option<String>| Reached {
            quest: quest.to_owned(),
            machine: to.to_owned(),
            held: reason.is_none(),
            files: here.len(),
            holding: here.len() - lacks.len(),
            scratch: scratch.clone(),
            waited: (waited.as_secs_f64() * 10.0).round() / 10.0,
            lacking: lacks.iter().take(MOST_NAMED).cloned().collect(),
            reason,
            why,
        };
        if theirs.is_some() && lacks.is_empty() {
            return Ok(reached(None, None));
        }
        if watching.linked(to) {
            unlinked_since = None;
            linked_since.get_or_insert(waited);
        } else {
            unlinked_since.get_or_insert(waited);
        }
        let past = |since: Option<Duration>, bound: Duration| {
            since.is_some_and(|since| waited.saturating_sub(since) >= bound.min(within))
        };
        if past(unlinked_since, SAYS_WITHIN) {
            let why = format!("no link reaches {to} from {machine}, so {quest} can't reach it");
            return Ok(reached(Some(Reason::NoLink), Some(why)));
        }
        if theirs.is_none() && past(linked_since, SAYS_WITHIN) {
            let why = format!(
                "{to}'s node hasn't taken {quest} up within {} s of its link, so it may run \
                 a mycelium too old to share it",
                SAYS_WITHIN.min(within).as_secs()
            );
            return Ok(reached(Some(Reason::NotShared), Some(why)));
        }
        if waited >= within {
            let first = lacks.first().map_or("nothing", String::as_str);
            let more = match lacks.len() {
                0 | 1 => String::new(),
                n => format!(" and {} more", n - 1),
            };
            let why = format!(
                "{to} holds {} of {quest}'s {} files at {machine}'s versions after {} s; it \
                 lacks {first}{more}",
                here.len() - lacks.len(),
                here.len(),
                waited.as_secs()
            );
            return Ok(reached(Some(Reason::Behind), Some(why)));
        }
        watching.pause();
    }
}

/// [`Watching`] through this machine's node: a link reaches a machine the
/// node holds as ready.
pub struct Node {
    socket: PathBuf,
    begun: Instant,
}

impl Node {
    /// Watching through the node at `socket`, from now.
    #[must_use]
    pub fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            begun: Instant::now(),
        }
    }
}

/// How long a wait lets pass between looks.
const LOOK_EVERY: Duration = Duration::from_millis(250);

impl Watching for Node {
    fn linked(&mut self, machine: &str) -> bool {
        let answer =
            Client::connect(&self.socket).and_then(|mut client| client.ask(&Request::Machines));
        matches!(answer, Ok(Ok(Response::Machines(machines)))
            if machines.iter().skip(1).any(|one| one.name == machine && one.state == Readiness::Ready))
    }

    fn withholds_above(&mut self, machine: &str) -> bool {
        let answer =
            Client::connect(&self.socket).and_then(|mut client| client.ask(&Request::Machines));
        // As the link judges it: unknown, or a worker, is withheld.
        !matches!(answer, Ok(Ok(Response::Machines(machines)))
            if machines.iter().skip(1).any(|one| one.name == machine
                && one.declared.as_ref().is_some_and(|said| !said.is_worker())))
    }

    fn waited(&self) -> Duration {
        self.begun.elapsed()
    }

    fn pause(&mut self) {
        thread::sleep(LOOK_EVERY);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const QUEST: &str = "QEST-20261115-5XF89V";

    fn places(dir: &std::path::Path) -> Places {
        Places {
            quests: dir.join("quests"),
            scratch: dir.join("scratch"),
            home: dir.join("home"),
            state: dir.join("state"),
        }
    }

    fn put(at: PathBuf, text: &str) {
        fs::create_dir_all(at.parent().expect("a file has a folder")).expect("made");
        fs::write(at, text).expect("written");
    }

    /// Kestrel, sharing a quest with a README and a handover file.
    fn kestrel(dir: &std::path::Path) -> Places {
        let places = places(dir);
        put(places.record(QUEST).join("README.md"), "# Shared\n");
        put(
            places
                .scratch
                .join(QUEST)
                .join("handover/kestrel/one.done.json"),
            "{}\n",
        );
        places.offer(QUEST, &[]).expect("shared");
        places
    }

    fn says_skiff_holds(places: &Places, files: BTreeMap<String, State>) {
        skiff_holds_of(places, QUEST, files);
    }

    fn skiff_holds_of(places: &Places, quest: &str, files: BTreeMap<String, State>) {
        let mut held = places.ledgers().hold(quest).expect("held");
        held.ledger.theirs.insert("skiff".to_owned(), files);
        held.keep().expect("kept");
    }

    #[test]
    fn a_quest_whose_record_waits_holds_its_scratch_here_and_none_of_its_record() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let places = kestrel(dir.path());
        put(
            places.record(QUEST).join("plans/PLAN-20261026-IZH11J.md"),
            "mine\n",
        );
        {
            // Shared from another machine, whose copy hasn't come.
            let mut held = places.ledgers().hold(QUEST).expect("held");
            held.ledger.aside = true;
            held.keep().expect("kept");
        }
        assert_eq!(
            holds_here(&places, QUEST, "kestrel")
                .expect("read")
                .keys()
                .collect::<Vec<_>>(),
            ["scratch/handover/kestrel/one.done.json"],
            "its scratch, and nothing of the record waiting to move aside"
        );
    }

    #[test]
    fn a_wait_counts_the_readmes_of_the_quests_above() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let places = places(dir.path());
        let parent = "QEST-20261013-4UHC5Q";
        put(
            places.record(parent).join("README.md"),
            "---\nparent: null\n---\n# Parent\n",
        );
        put(places.record(parent).join("JOURNEY.md"), "# Journey\n");
        put(
            places.record(QUEST).join("README.md"),
            &format!("---\nparent: {parent}\n---\n# Child\n"),
        );
        places.offer(QUEST, &[]).expect("shared");
        let above = format!("{parent}/record/README.md");
        assert_eq!(
            awaited(&places, QUEST, "kestrel")
                .expect("read")
                .keys()
                .collect::<Vec<_>>(),
            [&above, "record/README.md"],
            "the quest's files and its parent's README"
        );

        says_skiff_holds(
            &places,
            holds_here(&places, QUEST, "kestrel").expect("read"),
        );
        let within = Duration::from_secs(3);
        let reached = reach(
            &places,
            QUEST,
            ("kestrel", "skiff"),
            within,
            &mut clock(true),
        )
        .expect("waited");
        assert_eq!(reached.reason, Some(Reason::Behind), "{reached:?}");
        assert_eq!(reached.lacking, std::slice::from_ref(&above));
        assert_eq!((reached.files, reached.holding), (2, 1));

        skiff_holds_of(
            &places,
            parent,
            holds_here(&places, parent, "kestrel").expect("read"),
        );
        let reached = reach(
            &places,
            QUEST,
            ("kestrel", "skiff"),
            within,
            &mut clock(true),
        )
        .expect("waited");
        assert!(
            reached.held,
            "held once its parent's README is: {reached:?}"
        );
    }

    #[test]
    fn a_worker_holds_a_quest_without_the_readmes_above_it() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let places = places(dir.path());
        let parent = "QEST-20261013-4UHC5Q";
        // The parent's README is here, so a peer would be waited on for it.
        put(
            places.record(parent).join("README.md"),
            "---\nparent: null\n---\n# Parent\n",
        );
        put(
            places.record(QUEST).join("README.md"),
            &format!("---\nparent: {parent}\n---\n# Child\n"),
        );
        places.offer(QUEST, &[]).expect("shared");
        says_skiff_holds(
            &places,
            holds_here(&places, QUEST, "kestrel").expect("read"),
        );

        let mut worker = Clock {
            worker: true,
            ..clock(true)
        };
        let reached = reach(
            &places,
            QUEST,
            ("kestrel", "skiff"),
            Duration::from_secs(3),
            &mut worker,
        )
        .expect("waited");
        assert!(
            reached.held,
            "a worker, never served its parent's README, holds the quest: {reached:?}"
        );
        assert_eq!(
            (reached.files, reached.holding),
            (1, 1),
            "only the quest's own file is counted"
        );
    }

    /// A clock that moves on `step` at each pause, with a link to skiff
    /// or not, to a worker or not, doing `between` at each pause.
    struct Clock<F: FnMut(Duration)> {
        at: Duration,
        step: Duration,
        linked: bool,
        worker: bool,
        between: F,
    }

    impl<F: FnMut(Duration)> Watching for Clock<F> {
        fn linked(&mut self, machine: &str) -> bool {
            self.linked && machine == "skiff"
        }

        fn withholds_above(&mut self, _machine: &str) -> bool {
            self.worker
        }

        fn waited(&self) -> Duration {
            self.at
        }

        fn pause(&mut self) {
            self.at += self.step;
            (self.between)(self.at);
        }
    }

    fn clock(linked: bool) -> Clock<impl FnMut(Duration)> {
        Clock {
            at: Duration::ZERO,
            step: Duration::from_secs(1),
            linked,
            worker: false,
            between: |_| {},
        }
    }

    #[test]
    fn a_handover_is_shared_unnamed() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let places = kestrel(dir.path());
        assert!(
            places
                .ledgers()
                .hold(QUEST)
                .expect("held")
                .ledger
                .scratch
                .contains("handover"),
            "the scratch's handover is shared with no flag"
        );
        let bare = tempfile::tempdir().expect("a temporary folder");
        let without = self::places(bare.path());
        put(without.record(QUEST).join("README.md"), "# Bare\n");
        without
            .offer(QUEST, &[])
            .expect("a quest with no handover shares");
    }

    #[test]
    fn a_quest_has_reached_a_machine_holding_each_file_at_this_ends_version_or_newer() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let places = kestrel(dir.path());
        let here = holds_here(&places, QUEST, "kestrel").expect("read");
        assert_eq!(
            here.keys().collect::<Vec<_>>(),
            ["record/README.md", "scratch/handover/kestrel/one.done.json"],
            "the record and the named scratch"
        );
        let readme = here["record/README.md"].clone();
        let newer = State {
            hash: Some("changed there".to_owned()),
            version: readme.version.changed_on("skiff"),
        };
        let apart = State {
            hash: Some("written apart".to_owned()),
            version: super::super::version::Version::default().changed_on("skiff"),
        };
        let there = BTreeMap::from([("record/README.md".to_owned(), newer)]);
        assert_eq!(
            lacking(&here, &there),
            ["scratch/handover/kestrel/one.done.json"],
            "a newer version holds what this end has; a file not there doesn't"
        );
        let there = BTreeMap::from([("record/README.md".to_owned(), apart)]);
        assert_eq!(
            lacking(&here, &there),
            ["record/README.md", "scratch/handover/kestrel/one.done.json"],
            "one written apart doesn't"
        );

        says_skiff_holds(&places, here);
        let reached = reach(
            &places,
            QUEST,
            ("kestrel", "skiff"),
            WITHIN,
            &mut clock(true),
        )
        .expect("waited");
        assert_eq!(
            reached,
            Reached {
                quest: QUEST.to_owned(),
                machine: "skiff".to_owned(),
                held: true,
                files: 2,
                holding: 2,
                scratch: vec!["handover".to_owned()],
                waited: 0.0,
                lacking: Vec::new(),
                reason: None,
                why: None,
            }
        );
    }

    #[test]
    fn a_wait_writes_nothing_and_counts_a_change_at_its_next_version() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let places = kestrel(dir.path());
        let ledger = places.ledgers().hold(QUEST).expect("held").ledger;
        let here = holds_here(&places, QUEST, "kestrel").expect("read");
        assert_eq!(
            places.ledgers().hold(QUEST).expect("held").ledger,
            ledger,
            "the link's look still finds each change, to say it"
        );
        assert_eq!(
            here["record/README.md"].version,
            super::super::version::Version::default().changed_on("kestrel"),
            "a file the look hasn't seen counts as it will"
        );
    }

    #[test]
    fn a_machine_catching_up_during_the_wait_holds_it() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let places = kestrel(dir.path());
        let here = holds_here(&places, QUEST, "kestrel").expect("read");
        says_skiff_holds(&places, BTreeMap::new());
        let mut watching = Clock {
            at: Duration::ZERO,
            step: Duration::from_secs(1),
            linked: true,
            worker: false,
            between: |at: Duration| {
                if at == Duration::from_secs(3) {
                    says_skiff_holds(&places, here.clone());
                }
            },
        };
        let reached =
            reach(&places, QUEST, ("kestrel", "skiff"), WITHIN, &mut watching).expect("waited");
        assert!(reached.held, "held once skiff says so: {reached:?}");
        assert!((reached.waited - 3.0).abs() < f64::EPSILON, "{reached:?}");
    }

    #[test]
    fn a_machine_behind_past_the_bound_says_what_it_lacks() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let places = kestrel(dir.path());
        says_skiff_holds(&places, BTreeMap::new());
        let reached = reach(
            &places,
            QUEST,
            ("kestrel", "skiff"),
            Duration::from_secs(10),
            &mut clock(true),
        )
        .expect("waited");
        assert_eq!(reached.reason, Some(Reason::Behind));
        assert_eq!(
            reached.lacking,
            ["record/README.md", "scratch/handover/kestrel/one.done.json"]
        );
        assert!((reached.waited - 10.0).abs() < f64::EPSILON, "{reached:?}");
        assert_eq!(
            reached.why.as_deref(),
            Some(
                "skiff holds 0 of QEST-20261115-5XF89V's 2 files at kestrel's versions after \
                 10 s; it lacks record/README.md and 1 more"
            )
        );
    }

    #[test]
    fn no_link_or_a_quest_not_taken_up_is_said_within_seconds() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let places = kestrel(dir.path());
        let reached = reach(
            &places,
            QUEST,
            ("kestrel", "skiff"),
            WITHIN,
            &mut clock(false),
        )
        .expect("waited");
        assert_eq!(reached.reason, Some(Reason::NoLink), "{reached:?}");
        assert!(reached.waited <= SAYS_WITHIN.as_secs_f64(), "{reached:?}");
        assert_eq!(
            reached.why.as_deref(),
            Some("no link reaches skiff from kestrel, so QEST-20261115-5XF89V can't reach it")
        );

        let reached = reach(
            &places,
            QUEST,
            ("kestrel", "skiff"),
            WITHIN,
            &mut clock(true),
        )
        .expect("waited");
        assert_eq!(reached.reason, Some(Reason::NotShared), "{reached:?}");
        assert!(reached.waited <= SAYS_WITHIN.as_secs_f64(), "{reached:?}");
        assert_eq!(
            reached.why.as_deref(),
            Some(
                "skiff's node hasn't taken QEST-20261115-5XF89V up within 5 s of its link, \
                 so it may run a mycelium too old to share it"
            )
        );
    }
}
