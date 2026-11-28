//! The line agents' starts wait in for the node's room, so room freed goes
//! to them in a fair order rather than to whichever asks first. Each start
//! short of room joins it and waits to be told room may have freed; then
//! only the one whose turn it is takes it. Turns go round the cells: the
//! cell that took the node's room least lately goes first, and within a
//! cell the oldest ask. A start that waits for nothing, or comes after
//! others wait, goes behind them too. One past its own cell's number waits
//! for its cell's room as before, holding no one's turn meanwhile, since
//! that room is its cell's alone.
//!
//! A start refused for room keeps its cell's place: for [`KEPT_FOR`] from
//! the room it was told of, an ask from its cell takes the node's room
//! ahead of every other, and the room is held from the rest until then.
//!
//! The line, its places and which cell took a turn when are held in memory
//! alone, never in the book's file, so a node started again begins with an
//! empty line. That costs only fairness, never a start: each still asks
//! the book as before.

use std::collections::BTreeMap;
use std::time::Duration;

use super::{Admitted, CELL_AN_HOUR, Kept, NODE_AN_HOUR, PACED_FOR_AN_AGENT, Starts};

/// The most starts waiting in the line and places kept for refused ones,
/// together: far more than the node's hour lets start, so only asks
/// without end fill it. A start past it is refused at once, and a refusal
/// past it keeps no place.
const LINE_MOST: usize = 4 * NODE_AN_HOUR;

/// How long a refused start's place stands from the room it was told of,
/// in milliseconds: long enough for an asker told to ask again then to be
/// a little late, short enough that one never asking again holds the room
/// from others only a little while.
const KEPT_FOR: i64 = 180_000;

/// A start waiting in the line.
#[derive(Debug, Clone)]
struct Waiter {
    ticket: u64,
    /// The cell it counts toward, none for agents of no known cell.
    cell: Option<String>,
    /// When it was first asked, in milliseconds since the epoch.
    asked_at: i64,
    /// When it stops waiting and is refused, so it holds no turn after.
    until: i64,
}

/// A place kept for a cell one of whose starts was refused for room.
#[derive(Debug, Clone)]
struct Place {
    cell: Option<String>,
    from: i64,
    to: i64,
}

impl Place {
    /// Whether it holds the node's room at `now`.
    fn holds(&self, now: i64) -> bool {
        self.from <= now && now <= self.to
    }
}

/// The node's line of starts waiting for its room, and the places kept.
#[derive(Debug, Clone, Default)]
pub(super) struct Line {
    waiting: Vec<Waiter>,
    places: Vec<Place>,
    /// The turn each cell last took the node's room at, counted by
    /// `turns`, so the cell that took it least lately is told apart even
    /// within one millisecond.
    took: BTreeMap<Option<String>, u64>,
    turns: u64,
    next: u64,
}

impl Line {
    /// Puts a start of `cell`, asked at `asked_at` and waiting till
    /// `until`, at the end of the line, answering its ticket; none while
    /// the line is full.
    fn join(&mut self, cell: Option<String>, asked_at: i64, until: i64) -> Option<u64> {
        if self.waiting.len() + self.places.len() >= LINE_MOST {
            return None;
        }
        let ticket = self.next;
        self.next += 1;
        self.waiting.push(Waiter {
            ticket,
            cell,
            asked_at,
            until,
        });
        Some(ticket)
    }

    /// Takes the start waiting as `ticket` out of the line.
    fn leave(&mut self, ticket: u64) {
        self.waiting.retain(|one| one.ticket != ticket);
    }

    /// Keeps a place for `cell`, one of whose starts was refused at `now`
    /// and told of room at `room`, unless the cell keeps its number of
    /// places already or the line is full.
    fn keep_place(&mut self, cell: Option<String>, room: i64, now: i64) {
        self.places.retain(|one| one.to >= now);
        let its = self.places.iter().filter(|one| one.cell == cell).count();
        if its >= CELL_AN_HOUR || self.waiting.len() + self.places.len() >= LINE_MOST {
            return;
        }
        self.places.push(Place {
            cell,
            from: room,
            to: room + KEPT_FOR,
        });
    }

    /// Notes that a start of `cell` took the node's room at `now`, taking
    /// up the earliest place kept for it, if any is still to lapse.
    fn took(&mut self, cell: Option<&str>, now: i64) {
        self.turns += 1;
        self.took.insert(cell.map(str::to_owned), self.turns);
        // Bounded as the line is: the cell that took its turn longest ago
        // is forgotten first, and so goes first again, as it would anyway.
        if self.took.len() > LINE_MOST
            && let Some(oldest) = self
                .took
                .iter()
                .min_by_key(|(_, turn)| **turn)
                .map(|(cell, _)| cell.clone())
        {
            self.took.remove(&oldest);
        }
        self.places.retain(|one| one.to >= now);
        let earliest = self
            .places
            .iter()
            .enumerate()
            .filter(|(_, one)| one.cell.as_deref() == cell)
            .min_by_key(|(_, one)| one.from)
            .map(|(at, _)| at);
        if let Some(at) = earliest {
            self.places.remove(at);
        }
    }

    /// How many starts the line lets take the node's room before `asked`,
    /// waiting as `ticket` if it does, as of when it was asked: every
    /// place another cell holds then, and each start waiting whose turn
    /// comes first. One whose cell is `full`, past its own number, holds no
    /// turn, nor one whose wait is over.
    pub(super) fn ahead(
        &self,
        asked: &Kept,
        ticket: Option<u64>,
        full: impl Fn(Option<&str>) -> bool,
    ) -> usize {
        let now = asked.at;
        let cell = asked.started_by.as_deref();
        let held = self
            .places
            .iter()
            .filter(|one| one.holds(now) && one.cell.as_deref() != cell)
            .count();
        // One waiting keeps the place it first asked in; one that waits
        // for nothing comes after all.
        let (asked_at, mine) = self
            .waiting
            .iter()
            .find(|one| Some(one.ticket) == ticket)
            .map_or((now, u64::MAX), |one| (one.asked_at, one.ticket));
        let mut turns: Vec<(Option<&str>, i64, u64)> = self
            .waiting
            .iter()
            .filter(|one| one.ticket != mine && one.until >= now && !full(one.cell.as_deref()))
            .map(|one| (one.cell.as_deref(), one.asked_at, one.ticket))
            .collect();
        turns.push((cell, asked_at, mine));
        turns.sort_by_key(|&(_, asked_at, ticket)| (asked_at, ticket));
        // Round by round: each cell's oldest first, a cell holding its
        // place before the rest, then the cell that took a turn least
        // lately, then the oldest ask.
        let mut rounds: BTreeMap<Option<&str>, usize> = BTreeMap::new();
        let keyed: Vec<_> = turns
            .into_iter()
            .map(|(its, asked_at, ticket)| {
                let round = rounds.entry(its).or_default();
                *round += 1;
                let placed = self
                    .places
                    .iter()
                    .any(|one| one.cell.as_deref() == its && one.holds(now));
                let took = self.took.get(&its.map(str::to_owned)).copied();
                (*round, !placed, took, asked_at, ticket)
            })
            .collect();
        let me = keyed
            .iter()
            .find(|key| key.4 == mine)
            .copied()
            .unwrap_or((0, false, None, asked_at, mine));
        held + keyed.iter().filter(|key| **key < me).count()
    }

    /// When the first place held from `cell` at `now` lapses, so a start
    /// waiting behind it looks again then.
    fn lapses(&self, cell: Option<&str>, now: i64) -> Option<i64> {
        self.places
            .iter()
            .filter(|one| one.holds(now) && one.cell.as_deref() != cell)
            .map(|one| one.to + 1)
            .min()
    }
}

/// A start's place in the line: it leaves the line however its wait ends,
/// so an asker gone, its wait dropped, never holds the turn.
struct Waiting<'a> {
    starts: &'a Starts,
    ticket: Option<u64>,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket.take() {
            self.starts.lock().line.leave(ticket);
            self.starts.freed.notify_waiters();
        }
    }
}

impl Starts {
    /// Lets `asked` go ahead as [`Self::admit`] does, but where the limits
    /// refuse it with room again within `waits` milliseconds, waits in the
    /// node's line for its turn and goes then, so a start a little early
    /// goes through by itself; one refused later is told when. It looks
    /// again only when told room may have freed or the line moved, or when
    /// a start leaves the hour or a place lapses, asking the book again as
    /// of `now`, the wall clock. The wait is kept by a clock that never
    /// steps back, so the wall clock set back meanwhile holds no start past
    /// `waits`.
    ///
    /// # Errors
    ///
    /// Why it is refused.
    pub async fn admitted_in_turn(
        &self,
        asked: Kept,
        waits: i64,
        now: impl Fn() -> i64,
    ) -> Result<Admitted, String> {
        let mut asked = super::bounded(asked);
        let until = asked.at + waits;
        let began = tokio::time::Instant::now();
        let mut waiting = Waiting {
            starts: self,
            ticket: None,
        };
        loop {
            // Told from before it looks, so nothing freed meanwhile is
            // missed.
            let freed = self.freed.notified();
            tokio::pin!(freed);
            freed.as_mut().enable();
            let (why, room) = match self.in_turn(&mut asked, waiting.ticket) {
                Ok(admitted) => {
                    waiting.ticket = None;
                    return Ok(admitted);
                }
                Err(refused) => refused,
            };
            let waited = i64::try_from(began.elapsed().as_millis()).unwrap_or(i64::MAX);
            let left = waits.saturating_sub(waited);
            let Some(room) = room.filter(|room| left > 0 && *room <= until) else {
                let ticket = waiting.ticket.take();
                return Err(self.refused(asked, ticket, why, room));
            };
            let wake = {
                let mut book = self.lock();
                if waiting.ticket.is_none() {
                    waiting.ticket = book.line.join(asked.started_by.clone(), asked.at, until);
                }
                waiting.ticket.map(|_| {
                    room.min(
                        book.line
                            .lapses(asked.started_by.as_deref(), asked.at)
                            .unwrap_or(room),
                    )
                })
            };
            let Some(wake) = wake else {
                let why = format!(
                    "{LINE_MOST} starts wait in this node's line for room or hold their place in \
                     it, and it holds no more; ask again in a minute. {PACED_FOR_AN_AGENT}"
                );
                return Err(self.refused(asked, None, why, None));
            };
            let sleep = u64::try_from((wake - asked.at).clamp(0, left)).unwrap_or(0) + 1;
            tokio::select! {
                () = &mut freed => {}
                () = tokio::time::sleep(Duration::from_millis(sleep)) => {}
            }
            asked.at = now();
        }
    }

    /// Lets `asked`, waiting in line as `ticket` if it does, go ahead
    /// where nothing refuses it, counting it as going and taking it out
    /// of the line, under the one lock, so two asked at once can't both
    /// pass; or answers why not, and when the limits would let it, if it
    /// is they that refuse it.
    pub(super) fn in_turn(
        &self,
        asked: &mut Kept,
        ticket: Option<u64>,
    ) -> Result<Admitted, (String, Option<i64>)> {
        let admitted = {
            let mut book = self.lock();
            let refused = Self::coming_back(&book, asked)
                .map(|why| (why, None))
                .or_else(|| {
                    Self::once_an_hour(&book, asked);
                    self.refusal(&book, asked, asked.at, ticket)
                });
            if let Some(refused) = refused {
                return Err(refused);
            }
            let id = book.next;
            book.next += 1;
            if asked.counts() {
                book.line.took(asked.started_by.as_deref(), asked.at);
            }
            if let Some(ticket) = ticket {
                book.line.leave(ticket);
            }
            book.going.push((id, asked.clone()));
            Admitted {
                id,
                asked: asked.clone(),
            }
        };
        // The line moved, so whoever is next looks again.
        self.freed.notify_waiters();
        Ok(admitted)
    }

    /// Keeps `asked` as refused for `why`, taking it out of the line as
    /// `ticket` if it waits there, and keeps its cell's place where it is
    /// a start that counts refused for want of room till `room`; and
    /// answers `why`.
    pub(super) fn refused(
        &self,
        mut asked: Kept,
        ticket: Option<u64>,
        why: String,
        room: Option<i64>,
    ) -> String {
        {
            let mut book = self.lock();
            if let Some(ticket) = ticket {
                book.line.leave(ticket);
            }
            if let Some(room) = room.filter(|_| asked.counts()) {
                book.line
                    .keep_place(asked.started_by.clone(), room, asked.at);
            }
            asked.refused = Some(why.clone());
            self.keep_line(&mut book, asked);
        }
        self.freed.notify_waiters();
        why
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::HOUR;
    use super::*;
    use crate::declared::Kind;
    use crate::wire::By;

    const NOW: i64 = 1_800_000_000_000;

    /// The longest a start waits for room in these, as a node's does.
    const WAITS: i64 = 60_000;

    fn asked(by: By, cell: Option<&str>, at: i64) -> Kept {
        Kept {
            at,
            by,
            started_by: cell.map(str::to_owned),
            folder: None,
            program: Some("pi".to_owned()),
            resumes: None,
            reopens: None,
            carried: None,
            machine: None,
            held: None,
            failed: None,
            refused: None,
            once: None,
            joined: 0,
            again: false,
            declared: None,
            relayed_from: None,
        }
    }

    fn agent(cell: &str, at: i64) -> Kept {
        asked(By::Agent(Kind::Fence), Some(cell), at)
    }

    fn book() -> (tempfile::TempDir, Arc<Starts>) {
        let dir = tempfile::tempdir().expect("a folder");
        let starts = Starts::at(dir.path().join("starts.jsonl"));
        (dir, Arc::new(starts))
    }

    /// Fills the node's hour with starts still going, of cells other than
    /// the ones asking here: the first asked at `first`, the rest at
    /// `rest`, so room comes again an hour after each.
    fn filled(starts: &Starts, first: i64, rest: i64) -> Vec<Admitted> {
        (0..NODE_AN_HOUR)
            .map(|one| {
                let cell = format!("CELL{:02}", one / CELL_AN_HOUR);
                let at = if one == 0 { first } else { rest };
                starts.admit(agent(&cell, at)).expect("within the hour")
            })
            .collect()
    }

    /// How many starts wait in the node's line.
    fn waiting(starts: &Starts) -> usize {
        starts.lock().line.waiting.len()
    }

    /// Lets every task woken run, so what it does is done.
    async fn settled() {
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
    }

    type Asking = tokio::task::JoinHandle<Result<Admitted, String>>;

    /// Asks `kept` as a node does, waiting for room, its wall clock
    /// running with the paused clock from `NOW`; answered once it waits
    /// in the line.
    async fn waits(starts: &Arc<Starts>, kept: Kept) -> Asking {
        let before = waiting(starts);
        let began = tokio::time::Instant::now();
        let clock = move || NOW + i64::try_from(began.elapsed().as_millis()).expect("short");
        let asking = Arc::clone(starts);
        let task = tokio::spawn(async move { asking.admitted_in_turn(kept, WAITS, clock).await });
        settled().await;
        assert_eq!(waiting(starts), before + 1, "it waits in the line");
        task
    }

    /// The cell of the one start of `asking` let go, once they settle.
    async fn let_go(asking: &mut Vec<Asking>) -> String {
        settled().await;
        let went: Vec<usize> = (0..asking.len())
            .filter(|at| asking[*at].is_finished())
            .collect();
        assert_eq!(went.len(), 1, "one start takes the room freed");
        let admitted = asking
            .remove(went[0])
            .await
            .expect("it ran")
            .expect("let go");
        admitted.asked.started_by.expect("a cell's")
    }

    // 80ECO2 saw one cell's free minute move three times while other
    // cells' starts took it: each waiter woke and raced, so whoever asked
    // most took most.
    #[tokio::test(start_paused = true)]
    async fn two_cells_waiting_take_room_in_turn_though_one_asked_three_times_first() {
        let (_dir, starts) = book();
        let mut going = filled(&starts, NOW + 30_000 - HOUR, NOW + 30_000 - HOUR);
        let mut asking = Vec::new();
        for cell in ["OSPREY", "OSPREY", "OSPREY", "SKIFFS", "SKIFFS", "SKIFFS"] {
            asking.push(waits(&starts, agent(cell, NOW)).await);
        }
        let mut turns = Vec::new();
        for _ in 0..4 {
            let freed = going.pop().expect("one going");
            starts.keep_unbegun(freed, "it didn't begin", 0);
            turns.push(let_go(&mut asking).await);
        }
        assert_eq!(turns, ["OSPREY", "SKIFFS", "OSPREY", "SKIFFS"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_start_arriving_while_one_waits_is_held_behind_it() {
        let (_dir, starts) = book();
        let mut going = filled(&starts, NOW + 30_000 - HOUR, NOW + 30_000 - HOUR);
        let mut asking = vec![waits(&starts, agent("OSPREY", NOW)).await];
        asking.push(waits(&starts, agent("SKIFFS", NOW)).await);
        let freed = going.pop().expect("one going");
        starts.keep_unbegun(freed, "it didn't begin", 0);
        // Before the one waiting has looked again, the room is its.
        let refused = starts
            .admit(agent("TERNSS", NOW))
            .expect_err("held behind those waiting");
        assert!(
            refused.contains("63 sessions through this node in the last hour and 2 more wait"),
            "{refused}"
        );
        assert_eq!(let_go(&mut asking).await, "OSPREY");
        assert!(!asking[0].is_finished(), "the one that came later waits on");
    }

    #[tokio::test(start_paused = true)]
    async fn a_start_whose_asker_is_gone_gives_up_its_turn_at_once() {
        let (_dir, starts) = book();
        let mut going = filled(&starts, NOW + 30_000 - HOUR, NOW + 30_000 - HOUR);
        let gone = waits(&starts, agent("OSPREY", NOW)).await;
        let mut asking = vec![waits(&starts, agent("SKIFFS", NOW)).await];
        gone.abort();
        settled().await;
        assert_eq!(waiting(&starts), 1, "the gone asker's start left the line");
        let freed = going.pop().expect("one going");
        starts.keep_unbegun(freed, "it didn't begin", 0);
        assert_eq!(let_go(&mut asking).await, "SKIFFS");
    }

    #[test]
    fn a_refused_cell_asking_again_soon_takes_its_place_ahead_of_another() {
        let (_dir, starts) = book();
        let _going = filled(&starts, NOW + 1_000 - HOUR, NOW);
        let refused = starts
            .admit(agent("OSPREY", NOW))
            .expect_err("the node's hour is full");
        assert!(refused.contains("room again in 1 minute"), "{refused}");
        let held = starts
            .admit(agent("SKIFFS", NOW + 2_000))
            .expect_err("the room freed is OSPREY's");
        assert!(
            held.contains("1 more wait ahead in its line for room or hold their place there"),
            "{held}"
        );
        let _back = starts
            .admit(agent("OSPREY", NOW + 2_000))
            .expect("its place");
    }

    #[test]
    fn a_refused_cells_place_lapses_unclaimed() {
        let (_dir, starts) = book();
        let _going = filled(&starts, NOW + 1_000 - HOUR, NOW);
        starts
            .admit(agent("OSPREY", NOW))
            .expect_err("the node's hour is full");
        let _skiffs = starts
            .admit(agent("SKIFFS", NOW + 1_000 + KEPT_FOR + 1))
            .expect("OSPREY never asked again, so its place lapsed");
    }

    #[tokio::test(start_paused = true)]
    async fn the_persons_start_is_never_held() {
        let (_dir, starts) = book();
        let _going = filled(&starts, NOW + 30_000 - HOUR, NOW + 30_000 - HOUR);
        let _osprey = waits(&starts, agent("OSPREY", NOW)).await;
        let person = || asked(By::Person, None, NOW);
        let _admitted = starts.admit(person()).expect("the person's own start");
        let _waited = starts
            .admitted_in_turn(person(), WAITS, || NOW)
            .await
            .expect("and it waits for no one");
        assert_eq!(waiting(&starts), 1, "and never joined the line");
    }
}
