//! The ties between the realm's cells as the daemon interface holds them:
//! a record for each pair of cells that talked in the last day, and one
//! for each cell that talked, made from the messages the node holds,
//! reading only who said each, when, how and where, never what nor its
//! id. The words never leave the node.
//!
//! A message ties its sender to each peer it names and to the sender of
//! the message it answers, where that is held, each once. One only to
//! named groups, answering nothing, ties no one: a broadcast is no
//! conversation, and tying its sender to every member would tie the
//! machine's group to everyone. It is still its sender's talk, so a
//! cell's own record counts every message it sent, and how many ties it
//! has.
//!
//! A tie's strength grows by one with each message and halves every
//! [`HALF_LIFE`], said as it stood at the tie's last message, when it is
//! heard, rather than as now, so a record changes only as its talk does,
//! or as a message leaves its hour or its day; a reader fades it to now.
//! A tie or a cell with no message in a day is gone.
//!
//! A tie's key joins its ends' names with [`SEPARATOR`], so a cell whose
//! name holds one would read as a tie: its messages are passed over, and
//! no one is tied to it. A group's name never holds one, so no peer it
//! names does either; only a sender's own word, or an address a session
//! says, can.
//!
//! The talk is folded from the messages first ([`talk`]), cheaply enough
//! to be done while the messages are held, each cell and group kept once
//! and known by its number, and made records after ([`records`]), as
//! `tests/ties.rs` measures.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{BuildHasher, BuildHasherDefault, Hasher};

use serde_json::{Map, Value};

use super::millis_at;
use crate::daemon;
use crate::group::Group;
use crate::record::{Intent, Message, Session};

/// What joins a tie's ends' names in its key, which no cell's name may
/// hold.
const SEPARATOR: char = '~';

/// The kind of a tie's records, and of a cell's talk.
pub const KIND: &str = "tie";

/// The fields of a tie, or a cell's talk, that change while it lives.
pub const LIVE: [&str; 6] = ["hour", "today", "intents", "groups", "strength", "ties"];

/// How long a tie's strength takes to halve, in milliseconds: two hours,
/// so a tie busy in the morning reads a sixteenth of it by evening.
pub const HALF_LIFE: i64 = 2 * HOUR;

/// An hour, in milliseconds.
const HOUR: i64 = 60 * 60 * 1000;

/// A day, in milliseconds: how long a tie lives past its last message.
const DAY: i64 = 24 * HOUR;

/// The intents in the order a tie says them, each its bit in a tie's
/// intents.
const INTENTS: [(Intent, &str); 5] = [
    (Intent::Fyi, "fyi"),
    (Intent::Finding, "finding"),
    (Intent::Hold, "hold"),
    (Intent::Pivot, "pivot"),
    (Intent::Question, "question"),
];

/// What the realm's cells said to each other in the last day.
#[derive(Debug, Default)]
pub struct Talk {
    /// Each pair of cells that talked.
    ties: Vec<Tie>,
    /// Each cell that talked or was talked to, by its number.
    cells: Vec<Cell>,
    /// Each group a tie's messages went to, by its number.
    groups: Vec<Group>,
}

/// Two cells that talked in the last day, and how.
#[derive(Debug)]
struct Tie {
    /// Its two ends' numbers, the one whose name sorts first first.
    ends: [u32; 2],
    /// Each end's messages on it in the last hour, in the order of
    /// `ends`.
    hour: [u32; 2],
    /// Each end's messages on it in the last day, in the order of `ends`.
    today: [u32; 2],
    /// When its last message was sent, in milliseconds since the epoch.
    at: i64,
    /// Its strength as it stood at `at`.
    strength: f64,
    /// The numbers of the groups its messages went to, each once.
    groups: Vec<u32>,
    /// The intents its messages had, a bit each as [`INTENTS`] orders
    /// them.
    intents: u8,
}

impl Tie {
    /// Takes a message with `intent` to the groups numbered `groups`,
    /// sent at `sent` by its end `end`, as `now` stands.
    fn hear(
        &mut self,
        end: usize,
        sent: i64,
        (intent, groups): (Option<Intent>, &[u32]),
        now: i64,
    ) {
        if now - sent < HOUR {
            self.hour[end] += 1;
        }
        self.today[end] += 1;
        // Messages come oldest first, as the node holds them, but one
        // older than the last fades from it instead.
        if sent >= self.at {
            self.strength = self.strength * faded(sent - self.at) + 1.0;
            self.at = sent;
        } else {
            self.strength += faded(self.at - sent);
        }
        if let Some(intent) = intent
            && let Some(bit) = INTENTS.iter().position(|(one, _)| *one == intent)
        {
            self.intents |= 1 << bit;
        }
        for group in groups {
            if !self.groups.contains(group) {
                self.groups.push(*group);
            }
        }
    }
}

/// A cell's own talk in the last day: every message it sent, to whatever
/// group, and how many cells it is tied to.
#[derive(Debug)]
struct Cell {
    /// Its name.
    name: String,
    /// Its messages in the last hour.
    hour: u32,
    /// Its messages in the last day.
    today: u32,
    /// How many ties it has.
    ties: u32,
    /// When it last sent a message or was talked to, in milliseconds
    /// since the epoch.
    at: i64,
}

/// What strength is left of one after `since` milliseconds: none past
/// the 24 days an `i32` counts, a fade of thousands of halvings.
fn faded(since: i64) -> f64 {
    let half_life = f64::from(i32::try_from(HALF_LIFE).unwrap_or(i32::MAX));
    i32::try_from(since).map_or(0.0, |since| (-f64::from(since) / half_life).exp2())
}

/// The cell each of `sessions`' addresses names, uppercase, by the
/// address: a session going by several is one cell, named by its own.
pub fn aliases<'a>(sessions: impl IntoIterator<Item = &'a Session>) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    for session in sessions {
        let address = session.address.to_ascii_uppercase();
        let others = session.presence["addresses"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str);
        for alias in others {
            aliases.insert(alias.to_ascii_uppercase(), address.clone());
        }
        aliases.insert(address.clone(), address);
    }
    aliases
}

/// What `messages` say of the day before `now`, in milliseconds since
/// the epoch, each cell named as `aliases` says it is, and otherwise as
/// said, uppercase. A message older than a day still names who it
/// answers.
///
/// `messages` come in the order they were sent, as the node holds them,
/// each time in one form so its words sort as it was sent, so only the
/// last day's are read through, found by halving, and of the older ones
/// only those an answer names, since each message a watched realm
/// sends folds them again.
pub fn talk<'a, S: BuildHasher>(
    messages: impl IntoIterator<Item = &'a Message>,
    aliases: &HashMap<String, String, S>,
    now: i64,
) -> Talk {
    let messages: Vec<&Message> = messages.into_iter().collect();
    let first = messages.partition_point(|message| now - millis_at(&message.at) >= DAY);
    let (before, today) = messages.split_at(first);
    let senders = senders(before, today);
    let mut names = Names::new(aliases);
    let mut numbered: HashMap<&Group, u32, Quick> = HashMap::default();
    let mut groups: Vec<Group> = Vec::new();
    let mut ties: HashMap<(u32, u32), Tie, Quick> = HashMap::default();
    let mut others: Vec<u32> = Vec::new();
    let mut went: Vec<u32> = Vec::new();
    for message in today.iter().copied() {
        let sent = millis_at(&message.at).min(now);
        if now - sent >= DAY {
            continue;
        }
        let Some(from) = names.of(&message.from) else {
            continue;
        };
        names.said(from, sent, now);
        others.clear();
        let peers = message
            .groups
            .iter()
            .filter_map(|group| group.as_str().strip_prefix("peer."));
        let answered = message
            .re
            .as_deref()
            .and_then(|re| senders.get(re).copied());
        for other in peers.chain(answered) {
            let Some(other) = names.of(other) else {
                continue;
            };
            if other != from && !others.contains(&other) {
                others.push(other);
            }
        }
        if others.is_empty() {
            continue;
        }
        went.clear();
        for group in &message.groups {
            let number = *numbered.entry(group).or_insert_with(|| {
                groups.push(group.clone());
                u32::try_from(groups.len() - 1).unwrap_or(u32::MAX)
            });
            went.push(number);
        }
        for &other in &others {
            names.heard(other, sent);
            let pair = (from.min(other), from.max(other));
            let tie = ties.entry(pair).or_insert_with(|| names.tie(pair, sent));
            let end = usize::from(from != tie.ends[0]);
            tie.hear(end, sent, (message.intent, &went), now);
        }
    }
    for tie in ties.values() {
        for end in tie.ends {
            names.cells[end as usize].ties += 1;
        }
    }
    Talk {
        ties: ties.into_values().collect(),
        cells: names.cells,
        groups,
    }
}

/// The sender of each message `today` answers, by its id: today's own,
/// then the older ones, newest first, until each answered is found, as
/// most answer what was said lately.
fn senders<'m>(before: &[&'m Message], today: &[&'m Message]) -> HashMap<&'m str, &'m str, Quick> {
    let mut senders: HashMap<&str, &str, Quick> = today
        .iter()
        .map(|message| (message.id.as_str(), message.from.as_str()))
        .collect();
    let mut unfound: HashSet<&str, Quick> = today
        .iter()
        .filter_map(|message| message.re.as_deref())
        .filter(|re| !senders.contains_key(re))
        .collect();
    for message in before.iter().rev() {
        if unfound.is_empty() {
            break;
        }
        if unfound.remove(message.id.as_str()) {
            senders.insert(&message.id, &message.from);
        }
    }
    senders
}

/// How the fold's own maps hash, by its names, ids and numbers: a
/// multiply and a rotate a word, as rustc's own maps do, rather than the
/// standard library's `SipHash`, whose guard against a crafted key the
/// fold has little need of: names and ids come from peers over links,
/// but a crafted collision costs only the fold's own time, once the
/// node's lock is let go.
type Quick = BuildHasherDefault<Quickly>;

/// A hash as [`Quick`] makes it.
#[derive(Default)]
struct Quickly(u64);

impl Quickly {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl Hasher for Quickly {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let (words, rest) = bytes.as_chunks::<8>();
        for word in words {
            self.add(u64::from_le_bytes(*word));
        }
        let mut last = [0; 8];
        last[..rest.len()].copy_from_slice(rest);
        self.add(u64::from_le_bytes(last));
    }

    fn write_u8(&mut self, byte: u8) {
        self.add(u64::from(byte));
    }

    fn write_u32(&mut self, number: u32) {
        self.add(u64::from(number));
    }

    fn write_u64(&mut self, number: u64) {
        self.add(number);
    }
}

/// The cells a fold meets, each held once and known by its number.
struct Names<'m, 'a, S> {
    aliases: &'a HashMap<String, String, S>,
    numbers: HashMap<String, u32, Quick>,
    /// Each name as a message says it, and the cell it names, so a
    /// name said again is looked up once.
    said: HashMap<&'m str, Option<u32>, Quick>,
    cells: Vec<Cell>,
    /// A name as said, uppercase, kept to look up without making one
    /// for each message.
    upper: String,
}

impl<'m, 'a, S: BuildHasher> Names<'m, 'a, S> {
    fn new(aliases: &'a HashMap<String, String, S>) -> Self {
        Self {
            aliases,
            numbers: HashMap::default(),
            said: HashMap::default(),
            cells: Vec::new(),
            upper: String::new(),
        }
    }

    /// The number of the cell `said` names, or none where its name holds
    /// [`SEPARATOR`].
    fn of(&mut self, said: &'m str) -> Option<u32> {
        if let Some(&number) = self.said.get(said) {
            return number;
        }
        let number = self.named(said);
        self.said.insert(said, number);
        number
    }

    /// The number of the cell `said` names, made where it is new.
    fn named(&mut self, said: &str) -> Option<u32> {
        self.upper.clear();
        self.upper.push_str(said);
        self.upper.make_ascii_uppercase();
        let name = self
            .aliases
            .get(&self.upper)
            .map_or(self.upper.as_str(), String::as_str);
        if name.contains(SEPARATOR) {
            return None;
        }
        if let Some(&number) = self.numbers.get(name) {
            return Some(number);
        }
        let number = u32::try_from(self.cells.len()).unwrap_or(u32::MAX);
        self.cells.push(Cell {
            name: name.to_owned(),
            hour: 0,
            today: 0,
            ties: 0,
            at: 0,
        });
        self.numbers.insert(name.to_owned(), number);
        Some(number)
    }

    /// That the cell numbered `cell` sent a message at `sent`, as `now`
    /// stands.
    fn said(&mut self, cell: u32, sent: i64, now: i64) {
        let cell = &mut self.cells[cell as usize];
        if now - sent < HOUR {
            cell.hour += 1;
        }
        cell.today += 1;
        cell.at = cell.at.max(sent);
    }

    /// That the cell numbered `cell` was talked to at `sent`.
    fn heard(&mut self, cell: u32, sent: i64) {
        let cell = &mut self.cells[cell as usize];
        cell.at = cell.at.max(sent);
    }

    /// A new tie between `pair`'s cells, first heard at `sent`, the one
    /// whose name sorts first its first end.
    fn tie(&self, (one, other): (u32, u32), sent: i64) -> Tie {
        let name = |number: u32| &self.cells[number as usize].name;
        let ends = if name(one) <= name(other) {
            [one, other]
        } else {
            [other, one]
        };
        Tie {
            ends,
            hour: [0, 0],
            today: [0, 0],
            at: sent,
            strength: 0.0,
            groups: Vec::new(),
            intents: 0,
        }
    }
}

/// The records of `talk`, by key, each heard at its last message: a tie
/// keyed by its ends joined, `tie:<A>~<B>`, and a cell's talk by its
/// name, `tie:<A>`. The key alone names them, as no field need again.
/// Each is held as its bytes as soon as it is made, so the kind is never
/// held whole parsed.
pub fn records(talk: Talk) -> BTreeMap<String, daemon::Record> {
    let Talk {
        ties,
        cells,
        groups,
    } = talk;
    let made = ties.into_iter().map(|mut tie| {
        let [first, second] = tie.ends.map(|end| cells[end as usize].name.as_str());
        let id = format!("{first}{SEPARATOR}{second}");
        let strength = (tie.strength * 1000.0).round() / 1000.0;
        let intents: Vec<Value> = INTENTS
            .iter()
            .enumerate()
            .filter(|(bit, _)| tie.intents & (1 << bit) != 0)
            .map(|(_, (_, name))| Value::from(*name))
            .collect();
        tie.groups.sort_by_key(|group| &groups[*group as usize]);
        let went: Vec<Value> = tie
            .groups
            .iter()
            .map(|group| Value::from(groups[*group as usize].as_str()))
            .collect();
        let fields = Map::from_iter([
            ("hour".to_owned(), Value::from(tie.hour.to_vec())),
            ("today".to_owned(), Value::from(tie.today.to_vec())),
            ("intents".to_owned(), Value::from(intents)),
            ("groups".to_owned(), Value::from(went)),
            ("strength".to_owned(), Value::from(strength)),
        ]);
        record(&id, tie.at, fields)
    });
    let mut records: BTreeMap<String, daemon::Record> = made.collect();
    records.extend(cells.into_iter().map(|cell| {
        let fields = Map::from_iter([
            ("hour".to_owned(), Value::from(cell.hour)),
            ("today".to_owned(), Value::from(cell.today)),
            ("ties".to_owned(), Value::from(cell.ties)),
        ]);
        record(&cell.name, cell.at, fields)
    }));
    records
}

/// The record keyed `id`, heard at `heard`, holding `fields`, as its
/// bytes.
fn record(id: &str, heard: i64, fields: Map<String, Value>) -> (String, daemon::Record) {
    let record = daemon::record(KIND, id, heard, Value::Object(fields));
    (daemon::key(KIND, id), daemon::Record::from(record))
}

#[cfg(test)]
mod tests;
