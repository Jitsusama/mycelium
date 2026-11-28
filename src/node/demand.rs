//! Machines made from demand: how long a room made for agents is kept,
//! what budget a make is held to, and the marks its maker gave it.
//!
//! A room is a shared node: cells are packed onto it up to its ceiling, so
//! it isn't let go as its first work hands over, but once nothing has run
//! there for [`EMPTY_FOR`]. A make is held to the budget the person writes
//! in the file named by `--makes-within`, read again at each make. And
//! what a machine reaches and refuses is taken from its maker's word, kept
//! in its make's line or written with `--via-marks`, so no machine draws
//! the cells that need a network such as [`WORLD`] by saying it reaches
//! it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use serde::Deserialize;

use crate::declared::{Declared, Effect, Taint};
use crate::facts;
use crate::handover;
use crate::link::dial::Program;
use crate::made::{Book, Doubt, Made, Marks};
use crate::offer::Costs;

/// How long a room made for agents stands with no session running there
/// and no start on its way before it is let go: long enough that a cell
/// between two pieces of work, or one whose start is a while coming, finds
/// it still there, and short enough that an hour of an idle machine is
/// never paid for.
pub const EMPTY_FOR: Duration = Duration::from_secs(20 * 60);

/// How often a room's sessions are looked at, to tell when it has stood
/// empty for [`EMPTY_FOR`]: a fraction of it, so it is let go within a
/// minute of that.
const EMPTY_LOOKED_EVERY: Duration = Duration::from_secs(30);

/// How often a make past its budget looks again for room, as a machine
/// made before is let go, while its start waits.
const BUDGET_LOOKED_EVERY: Duration = Duration::from_secs(1);

/// How long a make past its budget waits for room before its start is
/// refused, as a start short of room waits ([`super::START_WAITS_MOST`]).
const BUDGET_WAITS_MOST: Duration = Duration::from_millis(super::START_WAITS_MOST.unsigned_abs());

/// The network only a machine's maker may say it reaches, and the taint
/// that keeps every other cell off it. It is ISXEG8's term: only cells
/// naming `world` land on a world node, and a cell naming it lands only on
/// a node its maker marked. A machine's own word can't be taken for it,
/// since a cell able to say it of its machine would draw world work onto
/// ground nobody marked.
pub const WORLD: &str = "world";

/// What a node knows of its machines' makers: the marks of each machine
/// it made, the `--via-marks` the person gave, the machines seen carrying
/// the world taint, and the budget its makes are held to.
pub struct Demand {
    /// Each `--via` machine's name, once its hello says it, with the marks
    /// its `--via-marks` gave it.
    via: Vec<(Arc<OnceLock<String>>, Marks)>,
    /// What changes as machines are made and let go.
    kept: Mutex<Kept>,
    /// The budget makes are held to.
    pub makes: Arc<Makes>,
}

/// The marks of each machine made, by name, with the place it was made
/// at, so letting one go never forgets the marks of a make of the same
/// name since; and the machines seen carrying the world taint, which
/// carry it till they are let go.
#[derive(Default)]
struct Kept {
    marks: BTreeMap<String, (String, Marks)>,
    world_tainted: BTreeSet<String>,
}

/// What a node is given of its makes' demand: each `--via` machine's
/// marks, by the name its dial learns, and the budget file, if any.
#[derive(Default)]
pub struct Given {
    /// Each `--via` machine's name, as its dial learns it, and its marks.
    pub via_marks: Vec<(Arc<OnceLock<String>>, Marks)>,
    /// The file `--makes-within` names.
    pub makes_within: Option<PathBuf>,
}

/// Each `--via-marks` given, by the name its `--via` machine's dial learns
/// as it links, so the marks follow the machine rather than its place.
///
/// # Errors
///
/// When one names a place no `--via` of `programs` dials, as a mistyped
/// place would leave the machine it meant unmarked.
pub fn via_marks(
    programs: &[Program],
    marks: Vec<(String, Marks)>,
) -> Result<Vec<(Arc<OnceLock<String>>, Marks)>, String> {
    marks
        .into_iter()
        .map(|(place, marks)| {
            let program = programs
                .iter()
                .find(|program| program.place == place)
                .ok_or_else(|| format!("--via-marks names {place}, but no --via dials it"))?;
            Ok((program.named.clone(), marks))
        })
        .collect()
}

impl Demand {
    /// What a node of `realm` keeps, from what it was given.
    pub fn of(given: Given, realm: &str) -> Self {
        Self {
            via: given.via_marks,
            kept: Mutex::default(),
            makes: Arc::new(Makes::at(given.makes_within, realm)),
        }
    }

    /// Keeps `made`'s marks, as its make's line says them, while it is
    /// booked, which is how this node knows it made it.
    pub fn booked(&self, made: &Made) {
        self.lock().marks.insert(
            made.machine.clone(),
            (made.place.clone(), made.marks.clone().unwrap_or_default()),
        );
    }

    /// Forgets `made`'s marks, and that it carried the world taint, as it
    /// is let go.
    pub fn unbooked(&self, made: &Made) {
        let mut kept = self.lock();
        if kept
            .marks
            .get(&made.machine)
            .is_some_and(|(place, _)| *place == made.place)
        {
            kept.marks.remove(&made.machine);
        }
        kept.world_tainted.remove(&made.machine);
    }

    /// `machine` as its maker marked it: as [`marked`] reads it, with the
    /// marks of its make where this node made it and hasn't let it go,
    /// those of its `--via-marks` where it came in by `--via`, and none
    /// otherwise, as `doubt` leaves the book's word.
    pub fn mark(&self, machine: &mut facts::Machine, doubt: &Doubt) {
        let via = self
            .via
            .iter()
            .find(|(named, _)| named.get() == Some(&machine.name))
            .map(|(_, marks)| marks.clone());
        let mut kept = self.lock();
        let made_here = kept
            .marks
            .get(&machine.name)
            .map(|(_, marks)| marks.clone());
        let booked = made_here.is_some();
        let marks = made_here.or(via);
        // A booked machine's own line read, so only a doubt naming it, or
        // one that may hide any make, leaves its marks unknown; one not
        // booked is in doubt only where what is left of a line names it.
        let doubted = match doubt {
            Doubt::None => false,
            Doubt::Of { machines, .. } => machines.contains_key(&machine.name),
            Doubt::Any(_) => booked,
        };
        let Kept { world_tainted, .. } = &mut *kept;
        marked(machine, marks.as_ref(), doubted, world_tainted);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Kept> {
        // A panic while holding the lock leaves the marks as they were
        // before the change that panicked, so carrying on with them is
        // sound.
        self.kept.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Holds `machine` to its maker's word: it reaches [`WORLD`] only where
/// `marks` say so, whatever it says of itself, and carries the taints of
/// `marks` beside its own, since a taint only narrows what lands there.
/// Where its make is `doubted`, its marks unknown, it carries the world
/// taint, so nothing that doesn't name world lands on what may be a world
/// node. Once it carries the world taint, `world_tainted` keeps it, so it
/// carries it till it is let go, whatever it says since.
pub fn marked(
    machine: &mut facts::Machine,
    marks: Option<&Marks>,
    doubted: bool,
    world_tainted: &mut BTreeSet<String>,
) {
    let marks = marks.filter(|_| !doubted);
    let adds_nothing = marks.is_none_or(|marks| marks.reach.is_empty() && marks.taints.is_empty())
        && !doubted
        && !world_tainted.contains(&machine.name);
    let declared = match &mut machine.declared {
        Some(declared) => declared,
        None if adds_nothing => return,
        None => machine.declared.insert(said_nothing()),
    };
    declared.reach.retain(|one| one != WORLD);
    declared.probes.remove(WORLD);
    if let Some(marks) = marks {
        for network in &marks.reach {
            if !declared.reach.contains(network) {
                declared.reach.push(network.clone());
            }
        }
        for taint in &marks.taints {
            if !declared.taints.contains(taint) {
                declared.taints.push(taint.clone());
            }
        }
    }
    if doubted {
        declared.taints.push(Taint {
            key: WORLD.to_owned(),
            effect: Effect::Never,
            why: "its make's line can't be read, so it may be a world machine".to_owned(),
        });
    }
    if declared.taints.iter().any(|taint| taint.key == WORLD) {
        world_tainted.insert(machine.name.clone());
    } else if world_tainted.contains(&machine.name) {
        declared.taints.push(Taint {
            key: WORLD.to_owned(),
            effect: Effect::Never,
            why: "it carried the world taint, which stays till it is let go".to_owned(),
        });
    }
}

/// A machine's word that says nothing, to carry its maker's marks where
/// it has said nothing of itself.
fn said_nothing() -> Declared {
    Declared {
        reach: Vec::new(),
        probes: BTreeMap::new(),
        outlives: Vec::new(),
        costs_money: false,
        taints: Vec::new(),
        cores: None,
        sessions_at_most: None,
        agents: BTreeMap::new(),
        leaves_at: None,
        within: None,
        hosts: Vec::new(),
        fence: None,
        pi_fences_itself: false,
        takes_carries: false,
        basin: None,
        kept_lively: None,
        needs_in: Vec::new(),
    }
}

/// Whether a machine made, whose make ends at `until`, is kept at `now`
/// though its work has handed over: a room made for agents (`room`) is,
/// while cells may still come to it, so till it is near its end, when it
/// is let go as any machine made is.
pub fn kept_past_handover(room: bool, now: jiff::Timestamp, until: jiff::Timestamp) -> bool {
    room && now < until - handover::HANDOVER
}

/// How a machine made is used, as [`emptied`] looks at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Use {
    /// It is no room made for agents, so it is let go as its work hands
    /// over, never for standing empty.
    NoRoom,
    /// A session runs there, or a start is on its way there.
    Used,
    /// It is a room, and nothing runs there or is on its way.
    Empty,
}

/// Ends once `look` has said a room stood empty for [`EMPTY_FOR`], looked
/// at each [`EMPTY_LOOKED_EVERY`]: any look finding it used, or no room,
/// starts the wait over. It never ends for a machine that is no room.
pub async fn emptied(look: impl Fn() -> Use) {
    let mut since = tokio::time::Instant::now();
    loop {
        tokio::time::sleep(EMPTY_LOOKED_EVERY).await;
        match look() {
            Use::Empty if since.elapsed() >= EMPTY_FOR => return,
            Use::Empty => {}
            Use::NoRoom | Use::Used => since = tokio::time::Instant::now(),
        }
    }
}

/// Why a machine stood empty is let go, as the book keeps it.
pub fn emptied_why() -> String {
    format!(
        "no session ran there for {} minutes",
        EMPTY_FOR.as_secs() / 60
    )
}

/// The budget a realm's makes are held to, from the file `--makes-within`
/// names, read again at each make, and the makes under way it hasn't yet
/// seen in the book.
pub struct Makes {
    file: Option<PathBuf>,
    realm: String,
    /// Each make under way, by a number of its own, with its hourly cost
    /// in nano-dollars, or none where it isn't in dollars.
    reserved: Mutex<(u64, BTreeMap<u64, Option<u128>>)>,
    /// What the journal last said of the budget being unbounded, so it
    /// says so once each time that changes.
    said: Mutex<Option<String>>,
}

/// One realm's budget, as the file writes it: either limit unwritten
/// holds nothing back.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Budget {
    #[serde(default)]
    machines_at_most: Option<usize>,
    #[serde(default)]
    usd_an_hour_at_most: Option<String>,
}

/// A make under way, counted against the budget till it is dropped, as it
/// is once the book keeps its line, or it failed.
pub struct Reserved {
    makes: Arc<Makes>,
    number: u64,
}

impl Drop for Reserved {
    fn drop(&mut self) {
        self.makes.reserved().1.remove(&self.number);
    }
}

/// What the budget says of one more make.
enum Judged {
    /// It is within the budget, reserved.
    Room(Reserved),
    /// It would pass the budget, for this reason, so it waits.
    Past(String),
    /// The budget, or what was made, can't be read, so nothing is made.
    Refused(String),
}

impl Makes {
    /// The budget in `file`, if any, for `realm`'s makes.
    pub fn at(file: Option<PathBuf>, realm: &str) -> Self {
        Self {
            file,
            realm: realm.to_owned(),
            reserved: Mutex::default(),
            said: Mutex::default(),
        }
    }

    /// Room for one more make at `costs` within the budget, as `book` says
    /// what stands made, reserved till it is dropped. Past the budget, it
    /// looks again each [`BUDGET_LOOKED_EVERY`], as a machine made before
    /// may be let go meanwhile, for [`BUDGET_WAITS_MOST`] at most.
    ///
    /// # Errors
    ///
    /// When it would still pass the budget then, saying which limit and
    /// the counts, or when the budget or the book can't be read.
    pub async fn room_for(
        self: &Arc<Self>,
        book: &Book,
        costs: &Costs,
    ) -> Result<Reserved, String> {
        let waiting = tokio::time::Instant::now();
        loop {
            match self.judged(book, costs) {
                Judged::Room(reserved) => return Ok(reserved),
                Judged::Refused(why) => return Err(why),
                Judged::Past(why) if waiting.elapsed() >= BUDGET_WAITS_MOST => {
                    return Err(format!(
                        "{why}, and no machine was let go in the {} seconds it waited",
                        BUDGET_WAITS_MOST.as_secs()
                    ));
                }
                Judged::Past(_) => tokio::time::sleep(BUDGET_LOOKED_EVERY).await,
            }
        }
    }

    /// What the budget says now of one more make at `costs`.
    fn judged(self: &Arc<Self>, book: &Book, costs: &Costs) -> Judged {
        let budget = match self.budget() {
            Ok(Some(budget)) => budget,
            Ok(None) => return Judged::Room(self.reserve(None)),
            Err(why) => return Judged::Refused(why),
        };
        let standing = match book.standing() {
            Ok(standing) => standing,
            Err(error) => {
                return Judged::Refused(format!(
                    "the book of machines made can't be read, so no make is held to the \
                     budget: {error}"
                ));
            }
        };
        if let Some(why) = standing.doubt.why() {
            return Judged::Refused(format!(
                "the book of machines made may hide a make, so no make is held to the \
                 budget: {why}"
            ));
        }
        let now = jiff::Timestamp::now();
        let standing: Vec<Option<u128>> = standing
            .made
            .iter()
            .filter(|made| made.until > now)
            .map(|made| nanos_an_hour(&made.costs))
            .collect();
        let this = nanos_an_hour(costs);
        let mut reserved = self.reserved();
        let made: Vec<Option<u128>> = standing
            .into_iter()
            .chain(reserved.1.values().copied())
            .collect();
        if let Some(most) = budget.machines_at_most
            && made.len() >= most
        {
            return Judged::Past(format!(
                "making it would pass this realm's budget of at most {most} machines made, \
                 as {} are made and not let go",
                made.len()
            ));
        }
        if let Some(most) = &budget.usd_an_hour_at_most {
            let Some(most_nanos) = nanos(most) else {
                return Judged::Refused(format!(
                    "the budget's usd_an_hour_at_most, {most}, isn't an amount of dollars"
                ));
            };
            let Some(spent) = made.iter().copied().sum::<Option<u128>>() else {
                return Judged::Refused(
                    "a machine made and not let go costs in a currency other than dollars, so \
                     makes can't be held to a budget in dollars"
                        .to_owned(),
                );
            };
            let Some(more) = this else {
                return Judged::Refused(format!(
                    "it costs {} {} an hour, not in dollars, so it can't be held to a budget \
                     in dollars",
                    costs.hourly, costs.currency
                ));
            };
            if spent + more > most_nanos {
                return Judged::Past(format!(
                    "making it would pass this realm's budget of at most {most} dollars an \
                     hour, as the {} machines made and not let go cost {} and it costs {}",
                    made.len(),
                    dollars(spent),
                    dollars(more)
                ));
            }
        }
        reserved.0 += 1;
        let number = reserved.0;
        reserved.1.insert(number, this);
        drop(reserved);
        Judged::Room(Reserved {
            makes: self.clone(),
            number,
        })
    }

    /// Reserves a make that no budget holds back.
    fn reserve(self: &Arc<Self>, costs: Option<u128>) -> Reserved {
        let mut reserved = self.reserved();
        reserved.0 += 1;
        let number = reserved.0;
        reserved.1.insert(number, costs);
        Reserved {
            makes: self.clone(),
            number,
        }
    }

    /// This realm's budget, none where none is written, said once each
    /// time that changes.
    ///
    /// # Errors
    ///
    /// When the file is there but can't be read or isn't a budget, so
    /// nothing is made past a budget the person meant to write.
    fn budget(&self) -> Result<Option<Budget>, String> {
        let Some(file) = &self.file else {
            self.say_unbounded(Some("no --makes-within names a budget".to_owned()));
            return Ok(None);
        };
        let text = match std::fs::read(file) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.say_unbounded(Some(format!("{} isn't there", file.display())));
                return Ok(None);
            }
            Err(error) => {
                return Err(format!(
                    "the budget in {} can't be read, so nothing is made: {error}",
                    file.display()
                ));
            }
        };
        let mut budgets: BTreeMap<String, Budget> =
            serde_json::from_slice(&text).map_err(|error| {
                format!(
                    "the budget in {} doesn't read, so nothing is made: {error}",
                    file.display()
                )
            })?;
        let budget = budgets.remove(&self.realm);
        self.say_unbounded(
            budget
                .is_none()
                .then(|| format!("{} names no budget for {}", file.display(), self.realm)),
        );
        Ok(budget)
    }

    /// Says once, each time it changes, why makes are unbounded, or that
    /// they are bounded again.
    fn say_unbounded(&self, why: Option<String>) {
        let mut said = self.said.lock().unwrap_or_else(PoisonError::into_inner);
        if *said != why {
            match &why {
                Some(why) => eprintln!("machines are made without bound, as {why}"),
                None => eprintln!("machines made are held to a budget again"),
            }
            said.clone_from(&why);
        }
    }

    fn reserved(&self) -> std::sync::MutexGuard<'_, (u64, BTreeMap<u64, Option<u128>>)> {
        // A panic while holding the lock leaves the makes under way as
        // they were before it, so carrying on with them is sound.
        self.reserved.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// An hour of `costs` in nano-dollars: nothing where it costs nothing,
/// whatever its currency, and none where it is in another currency or
/// isn't an amount, so it is never weighed as cheaper than it is.
fn nanos_an_hour(costs: &Costs) -> Option<u128> {
    if costs.free() {
        return Some(0);
    }
    (costs.currency == "USD")
        .then(|| nanos(&costs.hourly))
        .flatten()
}

/// A decimal amount, such as `0.12`, in billionths, without rounding;
/// none where it isn't one or is finer than a billionth.
fn nanos(amount: &str) -> Option<u128> {
    let (whole, part) = amount.split_once('.').unwrap_or((amount, ""));
    let digits = |text: &str| text.bytes().all(|byte| byte.is_ascii_digit());
    if (whole.is_empty() && part.is_empty()) || !digits(whole) || !digits(part) {
        return None;
    }
    let part = part.trim_end_matches('0');
    if part.len() > 9 {
        return None;
    }
    let whole: u128 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let part: u128 = format!("{part:0<9}").parse().ok()?;
    whole.checked_mul(1_000_000_000)?.checked_add(part)
}

/// Billionths of a dollar written out as dollars, without rounding.
fn dollars(billionths: u128) -> String {
    let part = format!("{:09}", billionths % 1_000_000_000);
    let part = part.trim_end_matches('0');
    if part.is_empty() {
        format!("{}", billionths / 1_000_000_000)
    } else {
        format!("{}.{part}", billionths / 1_000_000_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_read_in_billionths_without_rounding() {
        assert_eq!(nanos("0.12"), Some(120_000_000));
        assert_eq!(nanos("2"), Some(2_000_000_000));
        assert_eq!(nanos(".5"), Some(500_000_000));
        assert_eq!(nanos("1.0000000001"), None, "finer than a billionth");
        assert_eq!(nanos("1.5000000000"), Some(1_500_000_000));
        assert_eq!(nanos("one"), None);
        assert_eq!(nanos("."), None);
        assert_eq!(dollars(1_500_000_000), "1.5");
        assert_eq!(dollars(2_000_000_000), "2");
    }
}
