//! Machines made from offers. A node makes one only for a start that says
//! it may cost money and that no machine of the realm takes, asking the
//! provider's program `PROGRAM make SHAPE --lasting SECONDS`. The program
//! is told who asked for it in [`ASKER`], and refuses whoever it doesn't
//! serve by failing. It makes it, writes one JSON line saying where it is
//! (`place`, as `--via PROGRAM:PLACE` names it), the name its node will
//! say for itself (`machine`) and when it ends (`until`), and exits.
//!
//! Every machine made has an end: the provider ends it by `until` whatever
//! else happens, and the node releases it sooner once its work is done.
//! Each make and release is kept, a JSON line each, in the node's book
//! ([`Book`]), so what was made, for how long and at what rate can always
//! be read back and summed.
//!
//! Long because its tests, nearly half of it, walk each way the book is
//! kept and read back: grants, rooms, lines kept at once, restarts,
//! releases, a line cut short and a book that changes between reads.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::offer::{Costs, Offer};

/// How long a program has to make a machine: a provider makes one in
/// seconds, and a minute or two when it is busy.
const MADE_WITHIN: Duration = Duration::from_secs(5 * 60);

/// The variable a provider's program reads who asked for the machine in:
/// the address of a session the node proved asks, [`PERSON`] for the
/// person's own login, and [`UNKNOWN`] for anything else, which a
/// provider serving only some should refuse.
pub const ASKER: &str = "MYCELIUM_ASKER";

/// What [`ASKER`] says for the person's own login.
pub const PERSON: &str = "person";

/// What [`ASKER`] says where who asked can't be told.
pub const UNKNOWN: &str = "unknown";

/// A machine a provider made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Made {
    /// The program that made it.
    pub provider: String,
    /// Its shape, as offered.
    pub shape: String,
    /// Where it is, in the program's own words, as `--via` takes it.
    pub place: String,
    /// The name its node will say for itself once it links.
    pub machine: String,
    /// When it ends, whatever else happens.
    pub until: jiff::Timestamp,
    /// What it costs while it lives, as offered.
    pub costs: Costs,
    /// Who its make was told asked ([`ASKER`]), for whom alone, and the
    /// person, it is booked. A machine kept before makes said so has none,
    /// and is booked for nobody but the person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asker: Option<String>,
    /// How many sessions its offer promised it holds at most, for as long
    /// as its own configuration says none, so a provider can cap the
    /// machines it makes without changing what they run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions_at_most: Option<u32>,
}

/// What a program says it made.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Said {
    place: String,
    machine: String,
    until: jiff::Timestamp,
}

/// Asks `offer`'s program to make a machine of its shape, to live
/// `lasting`, for `asker`, as [`ASKER`] says it.
///
/// # Errors
///
/// When it can't be run, ends with a failure or not within
/// [`MADE_WITHIN`], or says something other than what it made, saying
/// which.
pub async fn make(offer: &Offer, lasting: Duration, asker: &str) -> Result<Made, String> {
    let program = &offer.provider;
    let mut command = tokio::process::Command::new(program);
    command
        .args(["make", &offer.shape, "--lasting"])
        .arg(lasting.as_secs().to_string())
        .env(ASKER, asker)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let ran = tokio::time::timeout(MADE_WITHIN, command.output())
        .await
        .map_err(|_| {
            format!(
                "{program} make {} said nothing within {}s",
                offer.shape,
                MADE_WITHIN.as_secs()
            )
        })?
        .map_err(|error| format!("{program} make couldn't run: {error}"))?;
    if !ran.status.success() {
        let said = String::from_utf8_lossy(&ran.stderr);
        return Err(format!(
            "{program} make {} ended {}: {}",
            offer.shape,
            ran.status,
            said.trim()
        ));
    }
    let text = String::from_utf8_lossy(&ran.stdout);
    let said: Said = serde_json::from_str(text.trim()).map_err(|error| {
        format!(
            "{program} make {} said {}, which isn't what it made: {error}",
            offer.shape,
            text.trim()
        )
    })?;
    Ok(Made {
        provider: program.clone(),
        shape: offer.shape.clone(),
        place: said.place,
        machine: said.machine,
        until: said.until,
        costs: offer.costs.clone(),
        asker: Some(asker.to_owned()),
        sessions_at_most: offer
            .facts
            .declared
            .as_ref()
            .and_then(|declared| declared.sessions_at_most),
    })
}

/// The file beside the book that holds the person's grants.
pub const GRANTS: &str = "grants.json";

/// The file beside the book that names the machines made for agents to
/// share ([`Book::agent_rooms`]).
pub const AGENT_ROOMS: &str = "agent-rooms";

/// The person's word that a session may have machines made and placed for
/// it as though the person asked, until `until`: its makes are told to
/// the provider as [`PERSON`] and booked for the person, so the person's
/// own starts may use them. It changes only who a machine is for; the
/// session's own fence still holds its work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    /// The session granted, by its mesh address.
    pub address: String,
    /// The person's words granting it, as said.
    pub said: String,
    /// When they said them.
    pub at: jiff::Timestamp,
    /// When the grant ends.
    pub until: jiff::Timestamp,
}

/// Where a node keeps each make and release, a JSON line each, newest
/// last. Every realm's node on a machine keeps the same book, so each
/// make names the realm whose node made it.
#[derive(Debug, Clone)]
pub struct Book {
    path: PathBuf,
    realm: Option<String>,
    /// Why the rooms file last couldn't be read, if it couldn't, so the
    /// journal says so once each time that changes, not on every read.
    rooms_unread: Arc<Mutex<Option<String>>>,
    /// The ends last read, with the file as it was then, so the book is
    /// read again only once it changes, and the journal says once each
    /// time that changes why the book can't say every end.
    ends_read: Arc<Mutex<Option<EndsRead>>>,
}

/// The ends last read, with the stamp the book had as they were read;
/// none where its stamp couldn't be taken, or the book couldn't be read,
/// so they're read again.
type EndsRead = (Option<Stamp>, Ends);

/// A file as its metadata tells it apart from itself once changed: its
/// inode, its length, when it was last written, and when its inode last
/// changed, as a change of who may read it does, to the nanosecond.
type Stamp = (u64, u64, std::time::SystemTime, i64, i64);

/// The stamp `file` has now.
fn stamp(file: &std::fs::Metadata) -> Option<Stamp> {
    Some((
        file.ino(),
        file.len(),
        file.modified().ok()?,
        file.ctime(),
        file.ctime_nsec(),
    ))
}

/// When each machine this realm's node made ends, as the book stands by
/// it, and whose make may be missing from them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ends {
    /// Each machine's end, its newest make's.
    pub until: BTreeMap<String, jiff::Timestamp>,
    /// The machines whose make may be missing from them, and why.
    pub doubt: Doubt,
}

/// Whose make a book may have lost, as what is left of the lines it
/// couldn't read says, each maybe a make cut short by a crash.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Doubt {
    /// Nobody's: it read whole, or every line it couldn't read was a
    /// release or another realm's, which hide no make of its own.
    #[default]
    None,
    /// Only these machines': what is left of a line still names the
    /// machine it made, and no line kept after it made or released that
    /// machine again.
    Of {
        /// The machines in doubt.
        machines: BTreeSet<String>,
        /// Why, as the journal says it.
        why: String,
    },
    /// Any machine's: the book can't be read, or a line was cut short
    /// before it said what it kept, the reason saying which.
    Any(String),
}

impl Doubt {
    /// Whether `machine`'s make may be missing.
    #[must_use]
    pub fn of(&self, machine: &str) -> bool {
        match self {
            Self::None => false,
            Self::Of { machines, .. } => machines.contains(machine),
            Self::Any(_) => true,
        }
    }

    /// Why a make may be missing, where one may.
    #[must_use]
    pub fn why(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Of { why, .. } | Self::Any(why) => Some(why),
        }
    }

    /// Adds that the line numbered `line` was cut short, leaving `torn`.
    fn torn(&mut self, line: usize, torn: Torn) {
        if matches!(self, Self::Any(_)) {
            return;
        }
        match torn {
            Torn::Nothing => {}
            Torn::Any => {
                *self = Self::Any(format!(
                    "line {line} can't be read, nor which machine it may have made"
                ));
            }
            Torn::Machine(machine) => {
                if let Self::Of { machines, .. } = self {
                    machines.insert(machine);
                } else {
                    *self = Self::Of {
                        machines: BTreeSet::from([machine]),
                        why: String::new(),
                    };
                }
                self.say_of();
            }
        }
    }

    /// Lets go the doubt of `machine`, made or released whole since.
    fn cleared(&mut self, machine: &str) {
        if let Self::Of { machines, .. } = self
            && machines.remove(machine)
        {
            if machines.is_empty() {
                *self = Self::None;
            } else {
                self.say_of();
            }
        }
    }

    /// Says why again, once the machines in doubt change.
    fn say_of(&mut self) {
        if let Self::Of { machines, why } = self {
            let names: Vec<&str> = machines.iter().map(String::as_str).collect();
            *why = format!("a line cut short may hide a make of {}", names.join(", "));
        }
    }
}

/// What a line cut short may have hidden, read from what is left of it.
#[derive(Debug, PartialEq, Eq)]
enum Torn {
    /// No make of this realm's: it was a release, or another realm's.
    Nothing,
    /// A make of this machine, whose name is still whole.
    Machine(String),
    /// A make of any machine, since nothing left says which.
    Any,
}

/// The most of the rooms file read, far past any list of machines, so a
/// file grown without end costs a placement no more than this.
const ROOMS_AT_MOST: u64 = 64 * 1024;

/// The machines a book says are made and not yet released, and how many
/// of its lines it couldn't read.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Standing {
    /// Each machine, oldest first.
    pub made: Vec<Made>,
    /// The lines passed over, each cut short by a crash among them.
    pub unread: usize,
    /// Whose make those lines may have hidden.
    pub doubt: Doubt,
    /// The makes that name no realm, kept before makes named one, and not
    /// released by a line naming none either, oldest first.
    pub unnamed: Vec<Made>,
}

impl Book {
    /// A book kept at `path`, its makes naming no realm.
    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        Self {
            path,
            realm: None,
            rooms_unread: Arc::default(),
            ends_read: Arc::default(),
        }
    }

    /// The same book, kept by `realm`'s node.
    #[must_use]
    pub fn of_realm(self, realm: &str) -> Self {
        Self {
            realm: Some(realm.to_owned()),
            ..self
        }
    }

    /// The grant `address` holds at `now` to have machines made and
    /// placed for it as the person's, as the person said it, from
    /// `grants.json` beside the book ([`Grant`]). A grant past its end, a
    /// file that isn't there or can't be read, and a line that isn't a
    /// grant all grant nothing, so a mistake never makes an agent the
    /// person.
    #[must_use]
    pub fn granted(&self, address: &str, now: jiff::Timestamp) -> Option<Grant> {
        let text = std::fs::read_to_string(self.path.with_file_name(GRANTS)).ok()?;
        let grants: Vec<Grant> = serde_json::from_str(&text).ok()?;
        grants
            .into_iter()
            .find(|grant| grant.address.eq_ignore_ascii_case(address) && now < grant.until)
    }

    /// The machines made for agents to share, each already up and paid
    /// for by being so, which agents' work may go to though it names
    /// none of them: `agent-rooms` beside the book, a machine's name a
    /// line, what follows a `#` and blank lines passed over. A machine it
    /// doesn't name stays as booked, so the person's own are theirs. A
    /// file that isn't there names none, and so does one that can't be
    /// read or isn't a plain file, said in the node's journal once each
    /// time that changes, since agents' work then goes back to the
    /// machines it would leave. Only its first [`ROOMS_AT_MOST`] bytes
    /// are read.
    #[must_use]
    pub fn agent_rooms(&self) -> BTreeSet<String> {
        let path = self.path.with_file_name(AGENT_ROOMS);
        let read = rooms_said(&path);
        let unread = read
            .as_ref()
            .err()
            .filter(|error| error.kind() != io::ErrorKind::NotFound)
            .map(ToString::to_string);
        let mut last = self
            .rooms_unread
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *last != unread {
            match (&unread, &read) {
                (Some(why), _) => {
                    eprintln!(
                        "{} names no rooms, as it can't be read: {why}",
                        path.display()
                    );
                }
                (None, Ok(_)) => eprintln!("{} can be read again", path.display()),
                (None, Err(_)) => eprintln!("{} is gone, so it names no rooms", path.display()),
            }
            last.clone_from(&unread);
        }
        drop(last);
        read.unwrap_or_default()
            .lines()
            .filter_map(|line| line.split('#').next())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// Keeps that `made` was made.
    ///
    /// # Errors
    ///
    /// When the book can't be written.
    pub fn made(&self, made: &Made) -> io::Result<()> {
        self.keep(json!({ "at": jiff::Timestamp::now(), "made": made }))
    }

    /// Keeps that `made` was released, or let end, and why.
    ///
    /// # Errors
    ///
    /// When the book can't be written.
    pub fn released(&self, made: &Made, why: &str) -> io::Result<()> {
        self.keep(json!({
            "at": jiff::Timestamp::now(),
            "released": { "machine": made.machine, "place": made.place, "why": why },
        }))
    }

    /// The machines this realm's node made and hasn't yet released, ended
    /// or not: those a node started again still has to link or let go.
    /// A line that names no realm, as those kept before lines named one,
    /// is no realm's, so it is never linked again into the wrong one. A
    /// line it can't read is passed over and
    /// counted; one cut short by a crash, with the next kept straight
    /// after it, still gives up that next one.
    ///
    /// # Errors
    ///
    /// When the book is there but can't be read.
    pub fn standing(&self) -> io::Result<Standing> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Standing::default());
            }
            Err(error) => return Err(error),
        };
        let mut standing = Standing::default();
        for (at, line) in String::from_utf8_lossy(&bytes).lines().enumerate() {
            let (kept, left) = kept(line);
            if let Some(left) = left {
                standing.unread += 1;
                standing
                    .doubt
                    .torn(at + 1, torn(left, self.realm.as_deref()));
            }
            let Some(kept) = kept else {
                continue;
            };
            if kept.realm == self.realm {
                let named = kept.made.as_ref().map(|made| &made.machine);
                let released = kept.released.as_ref().map(|released| &released.machine);
                if let Some(machine) = named.or(released) {
                    standing.doubt.cleared(machine);
                }
            }
            let kept_by = if kept.realm.is_none() {
                &mut standing.unnamed
            } else if kept.realm == self.realm {
                &mut standing.made
            } else {
                continue;
            };
            if let Some(made) = kept.made {
                kept_by.push(made);
            } else if let Some(released) = kept.released {
                kept_by.retain(|made| {
                    made.machine != released.machine || made.place != released.place
                });
            }
        }
        Ok(standing)
    }

    /// When each machine this realm's node made and hasn't released
    /// ends, by its newest make as [`Book::standing`] reads them, and
    /// whose make the book may have lost ([`Doubt`]). Read again only
    /// once the book changes, so a reader asking often costs no reading
    /// of the whole book each time, and the journal says why it is
    /// unsure once each time that changes.
    #[must_use]
    pub fn ends(&self) -> Ends {
        // Taken before the book is read, so a line kept meanwhile is read
        // at the next ask rather than missed for good.
        let stamp = std::fs::metadata(&self.path).ok().as_ref().and_then(stamp);
        let mut last = self
            .ends_read
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let (Some(now), Some((Some(then), ends))) = (stamp, last.as_ref())
            && now == *then
        {
            return ends.clone();
        }
        let read = self.standing();
        // One that couldn't be read is read again at the next ask, though
        // its stamp hasn't moved.
        let stamp = stamp.filter(|_| read.is_ok());
        let ends = match read {
            Ok(standing) => Ends {
                until: standing
                    .made
                    .into_iter()
                    .map(|made| (made.machine, made.until))
                    .collect(),
                doubt: standing.doubt,
            },
            Err(error) => Ends {
                until: BTreeMap::new(),
                doubt: Doubt::Any(format!("it can't be read: {error}")),
            },
        };
        let before = last.as_ref().and_then(|(_, ends)| ends.doubt.why());
        if before != ends.doubt.why() {
            match ends.doubt.why() {
                Some(why) => eprintln!(
                    "{} can't say when every machine made ends, as {why}, so no paid machine \
                     in doubt is carried to till its end is known",
                    self.path.display()
                ),
                None => eprintln!(
                    "{} says when every machine made ends again",
                    self.path.display()
                ),
            }
        }
        *last = Some((stamp, ends.clone()));
        ends
    }

    /// Keeps `line`, naming this book's realm where it has one.
    fn keep(&self, mut line: serde_json::Value) -> io::Result<()> {
        if let Some(realm) = &self.realm {
            line["realm"] = json!(realm);
        }
        if let Some(folder) = self.path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let mut book = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        // One write a line, so nobody reads half of one, and two kept at
        // once don't interleave.
        book.write_all(format!("{line}\n").as_bytes())
    }
}

/// A line of the book, as [`Book::standing`] reads it.
#[derive(Deserialize)]
struct Kept {
    realm: Option<String>,
    made: Option<Made>,
    released: Option<Released>,
}

/// What `line` keeps, and where it doesn't read whole, what is left of
/// the line cut short; then what it keeps is what the last line kept
/// within it does, since a line cut short by a crash has the next
/// written straight after it.
fn kept(line: &str) -> (Option<Kept>, Option<&str>) {
    if let Ok(whole) = serde_json::from_str(line) {
        return (Some(whole), None);
    }
    let next = line.rfind(r#"{"at""#).filter(|at| *at > 0);
    match next.and_then(|next| Some((next, serde_json::from_str(&line[next..]).ok()?))) {
        Some((next, after)) => (Some(after), Some(&line[..next])),
        None => (None, Some(line)),
    }
}

/// What `left`, all that is left of a line cut short, may have hidden
/// from a book kept by `realm`. A line is kept with its keys in order,
/// so a release says so before its machine, and a make says its
/// machine before its place and its realm last; nothing a make holds
/// before its machine has a key of that name, and a name said within
/// another's text has its quotes escaped, so the first whole name after
/// the make's start is its own. What is left before either says which
/// may be any machine's make.
fn torn(left: &str, realm: Option<&str>) -> Torn {
    if left.contains(r#""released":"#)
        || said(left, "realm").is_some_and(|said| Some(said.as_str()) != realm)
    {
        return Torn::Nothing;
    }
    left.find(r#""made":{"#)
        .and_then(|made| said(&left[made..], "machine"))
        .map_or(Torn::Any, Torn::Machine)
}

/// The text `left` first says whole for `key`, if it says one.
fn said(left: &str, key: &str) -> Option<String> {
    let key = format!("\"{key}\":");
    let at = left.find(&key)? + key.len();
    serde_json::Deserializer::from_str(&left[at..])
        .into_iter::<String>()
        .next()?
        .ok()
}

/// Which machine a release let go.
#[derive(Deserialize)]
struct Released {
    machine: String,
    place: String,
}

/// What the rooms file at `path` says, read only where it is a plain
/// file, so a pipe or a device left there never holds a placement, and
/// only so far as [`ROOMS_AT_MOST`].
fn rooms_said(path: &Path) -> io::Result<String> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "it isn't a plain file",
        ));
    }
    let mut said = String::new();
    std::fs::File::open(path)?
        .take(ROOMS_AT_MOST)
        .read_to_string(&mut said)?;
    Ok(said)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::offer::Shape;

    /// A program at `dir`'s `provider` running `body`.
    fn provider(dir: &Path, body: &str) -> String {
        let program = dir.join("provider");
        crate::stand_in::program(&program, &format!("#!/bin/sh\n{body}\n"));
        program.display().to_string()
    }

    fn offer_of(provider: String) -> Offer {
        Offer {
            provider,
            shape: "scratch".to_owned(),
            facts: Shape::default(),
            costs: Costs {
                hourly: "0.12".to_owned(),
                currency: "USD".to_owned(),
            },
            lasts: Some(7200),
        }
    }

    #[tokio::test]
    async fn a_program_makes_a_machine_saying_where_it_is_and_when_it_ends() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let asked = dir.path().join("asked");
        let program = provider(
            dir.path(),
            &format!(
                "echo \"$@\" > {}\necho '{{\"place\": \"sbx-1\", \"machine\": \"lent-1\", \"until\": \"2026-10-31T20:00:00Z\"}}'",
                asked.display()
            ),
        );
        let made = make(
            &offer_of(program.clone()),
            Duration::from_secs(3600),
            "person",
        )
        .await
        .expect("it is made");
        assert_eq!(
            fs::read_to_string(&asked).expect("it was asked"),
            "make scratch --lasting 3600\n",
            "the program is asked for its shape and how long it lives"
        );
        assert_eq!(
            made,
            Made {
                provider: program,
                shape: "scratch".to_owned(),
                place: "sbx-1".to_owned(),
                machine: "lent-1".to_owned(),
                until: "2026-10-31T20:00:00Z".parse().expect("a time"),
                costs: offer_of(String::new()).costs,
                asker: Some("person".to_owned()),
                sessions_at_most: None,
            },
            "what was made, where, until when, at what rate and for whom"
        );

        let book = Book::at(dir.path().join("state").join("made.jsonl"));
        book.made(&made).expect("its making is kept");
        book.released(&made, "its work was done")
            .expect("its release is kept");
        let kept: Vec<serde_json::Value> = fs::read_to_string(dir.path().join("state/made.jsonl"))
            .expect("the book is there")
            .lines()
            .map(|line| serde_json::from_str(line).expect("a JSON line"))
            .collect();
        assert_eq!(
            (
                &kept[0]["made"]["costs"]["hourly"],
                &kept[1]["released"]["why"]
            ),
            (&json!("0.12"), &json!("its work was done")),
            "each make and release is kept, a line each, in order"
        );
    }

    #[test]
    fn the_rooms_made_for_agents_are_named_a_line_each_beside_the_book() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let book = Book::at(dir.path().join("made.jsonl"));
        assert!(book.agent_rooms().is_empty(), "no file names none");
        fs::write(
            dir.path().join(AGENT_ROOMS),
            "# rooms for agents\nwren # the big one\n\n  tern  \n",
        )
        .expect("written");
        assert_eq!(
            book.agent_rooms(),
            BTreeSet::from(["tern".to_owned(), "wren".to_owned()]),
            "each name a line, trimmed, past blanks and comments"
        );
        fs::remove_file(dir.path().join(AGENT_ROOMS)).expect("removed");
        fs::create_dir(dir.path().join(AGENT_ROOMS)).expect("a folder in its place");
        assert!(
            book.agent_rooms().is_empty(),
            "what isn't a file names none"
        );
        assert!(
            book.rooms_unread
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some(),
            "and why is kept, so it is said once"
        );
    }

    #[test]
    fn a_grant_holds_only_for_its_session_and_until_its_end() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let book = Book::at(dir.path().join("made.jsonl"));
        let at = |said: &str| said.parse::<jiff::Timestamp>().expect("a time");
        let now = at("2026-11-25T23:50:00Z");
        assert_eq!(book.granted("WREN01", now), None, "no file grants nothing");

        fs::write(dir.path().join(GRANTS), "not json").expect("written");
        assert_eq!(
            book.granted("WREN01", now),
            None,
            "nor does one that isn't grants"
        );

        fs::write(
            dir.path().join(GRANTS),
            r#"[{"address":"WREN01","said":"you may make it as me",
                "at":"2026-11-25T23:05:00Z","until":"2026-11-26T12:00:00Z"}]"#,
        )
        .expect("written");
        assert_eq!(
            book.granted("wren01", now).map(|grant| grant.said),
            Some("you may make it as me".to_owned()),
            "the session granted, however its address is cased"
        );
        assert_eq!(book.granted("TERN02", now), None, "and no other");
        assert_eq!(
            book.granted("WREN01", at("2026-11-26T12:00:00Z")),
            None,
            "and not once it ends"
        );
    }

    #[test]
    fn lines_kept_at_once_are_each_kept_whole() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let book = Book::at(dir.path().join("made.jsonl"));
        let made = Made {
            provider: "lender".to_owned(),
            shape: "small".to_owned(),
            place: "p1".to_owned(),
            machine: "lent".to_owned(),
            until: jiff::Timestamp::UNIX_EPOCH,
            costs: offer_of(String::new()).costs,
            asker: None,
            sessions_at_most: None,
        };
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..50 {
                        book.released(&made, "its time was up").expect("kept");
                    }
                });
            }
        });
        let kept = fs::read_to_string(dir.path().join("made.jsonl")).expect("the book");
        let whole = kept
            .lines()
            .filter(|line| serde_json::from_str::<serde_json::Value>(line).is_ok())
            .count();
        assert_eq!(whole, 400, "every line is whole, none run into another");
    }

    fn made_at(machine: &str, place: &str, until: jiff::Timestamp) -> Made {
        Made {
            provider: "provider".to_owned(),
            shape: "scratch".to_owned(),
            place: place.to_owned(),
            machine: machine.to_owned(),
            until,
            costs: offer_of(String::new()).costs,
            asker: None,
            sessions_at_most: None,
        }
    }

    #[test]
    fn the_book_keeps_who_a_machine_was_made_for_through_a_restart() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let path = dir.path().join("made.jsonl");
        let later = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1);
        let mut made = made_at("osprey", "p1", later);
        made.asker = Some("P9R1KI".to_owned());
        Book::at(path.clone())
            .of_realm("work")
            .made(&made)
            .expect("a make is kept");
        assert_eq!(
            Book::at(path)
                .of_realm("work")
                .standing()
                .expect("the book reads")
                .made,
            vec![made],
            "a node reading its book again books the machine for the same asker"
        );
    }

    #[test]
    fn a_release_naming_no_realm_lets_go_a_make_naming_none() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let path = dir.path().join("made.jsonl");
        let later = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1);
        let (gone, kept) = (made_at("skiff", "p0", later), made_at("tern", "p1", later));
        let old = Book::at(path.clone());
        for made in [&gone, &kept] {
            old.made(made).expect("a make naming no realm is kept");
        }
        old.released(&gone, "its work handed over")
            .expect("a release naming no realm is kept");
        let work = Book::at(path).of_realm("work");
        work.released(&kept, "its time was up")
            .expect("a release is kept");
        assert_eq!(
            work.standing().expect("the book reads").unnamed,
            vec![kept],
            "a release naming no realm lets go the make naming none of its \
             machine at its place, and a realm's release never does"
        );
    }

    #[test]
    fn the_ends_are_read_again_once_the_book_changes() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let book = Book::at(dir.path().join("made.jsonl")).of_realm("work");
        assert_eq!(book.ends(), Ends::default(), "a book never kept ends none");
        let now = jiff::Timestamp::now();
        let (soon, later) = (
            now + jiff::SignedDuration::from_hours(1),
            now + jiff::SignedDuration::from_hours(2),
        );
        let (tern, wren) = (made_at("tern", "p1", soon), made_at("wren", "p2", soon));
        book.made(&tern).expect("a make is kept");
        assert_eq!(
            book.ends().until,
            BTreeMap::from([("tern".to_owned(), soon)]),
            "a make kept is read"
        );
        book.made(&wren).expect("a make is kept");
        book.made(&made_at("tern", "p3", later))
            .expect("a make is kept");
        book.released(&wren, "its time was up")
            .expect("a release is kept");
        assert_eq!(
            book.ends(),
            Ends {
                until: BTreeMap::from([("tern".to_owned(), later)]),
                doubt: Doubt::None,
            },
            "read again once the book changes: the newest make of a name \
             ends it, and one released ends nothing"
        );
    }

    #[test]
    fn a_book_it_cant_read_whole_is_unsure_of_its_ends() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let path = dir.path().join("made.jsonl");
        let book = Book::at(path.clone()).of_realm("work");
        let soon = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1);
        book.made(&made_at("tern", "p1", soon))
            .expect("a make is kept");
        OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("the book opens")
            .write_all(b"{\"at\": \"cut sh\n")
            .expect("a line cut short is written");
        book.made(&made_at("wren", "p2", soon))
            .expect("a make is kept");
        let torn = book.ends();
        assert_eq!(
            torn.until,
            BTreeMap::from([("tern".to_owned(), soon), ("wren".to_owned(), soon)]),
            "the ends it can read still stand"
        );
        assert_eq!(
            torn.doubt,
            Doubt::Any("line 2 can't be read, nor which machine it may have made".to_owned()),
            "a line cut short before it names its machine may be any machine's \
             make, though a whole make follows it"
        );
        assert!(torn.doubt.of("osprey"), "so every machine is in doubt");
        let gone = tempfile::tempdir().expect("a temporary folder");
        fs::create_dir(gone.path().join("made.jsonl")).expect("the book is a folder");
        let folder = Book::at(gone.path().join("made.jsonl")).ends();
        assert!(folder.until.is_empty(), "a book it can't read ends none");
        assert!(
            matches!(&folder.doubt, Doubt::Any(why) if why.starts_with("it can't be read")),
            "and doubts every machine, saying why: {:?}",
            folder.doubt
        );
    }

    /// Appends `torn` to the book at `path`, as a crash leaves a line.
    fn cut_short(path: &Path, torn: &str) {
        OpenOptions::new()
            .append(true)
            .open(path)
            .expect("the book opens")
            .write_all(format!("{torn}\n").as_bytes())
            .expect("a line cut short is written");
    }

    #[test]
    fn a_make_cut_short_doubts_only_the_machine_it_still_names() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let path = dir.path().join("made.jsonl");
        let book = Book::at(path.clone()).of_realm("work");
        let soon = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1);
        let (tern, wren) = (made_at("tern", "p1", soon), made_at("wren", "p2", soon));
        book.made(&tern).expect("a make is kept");
        let whole = serde_json::to_string(&json!({ "at": soon, "made": wren })).expect("a line");
        let named = whole
            .find(r#","place""#)
            .expect("its place follows its name");
        cut_short(&path, &whole[..named + 4]);
        let doubt = book.ends().doubt;
        assert!(
            doubt.of("wren"),
            "the machine it names is in doubt: {doubt:?}"
        );
        assert!(
            !doubt.of("tern") && !doubt.of("osprey"),
            "no other machine is: {doubt:?}"
        );
        assert_eq!(
            doubt.why(),
            Some("a line cut short may hide a make of wren"),
            "saying which"
        );
        let cut_in_name = whole.find(r#""wren""#).expect("its name");
        cut_short(&path, &whole[..cut_in_name + 3]);
        assert!(
            matches!(book.ends().doubt, Doubt::Any(ref why) if why.starts_with("line 3 ")),
            "a make cut short within its name may be any machine's"
        );
        let again = Book::at(dir.path().join("again.jsonl")).of_realm("work");
        cut_short(&again.path, &whole[..named + 4]);
        again.made(&wren).expect("a make is kept");
        assert_eq!(
            again.ends().doubt,
            Doubt::None,
            "a whole make of that machine kept after lets the doubt go"
        );
        let released = Book::at(dir.path().join("released.jsonl")).of_realm("work");
        cut_short(&released.path, &whole[..named + 4]);
        released
            .released(&wren, "its time was up")
            .expect("a release is kept");
        assert_eq!(
            released.ends().doubt,
            Doubt::None,
            "and so does a whole release of it"
        );
    }

    #[test]
    fn a_release_cut_short_doubts_nothing() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let path = dir.path().join("made.jsonl");
        let book = Book::at(path.clone()).of_realm("work");
        let soon = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1);
        book.made(&made_at("tern", "p1", soon))
            .expect("a make is kept");
        for realm in ["work", "home"] {
            cut_short(
                &path,
                &format!(r#"{{"at":"{soon}","realm":"{realm}","released":{{"machine":"te"#),
            );
        }
        let ends = book.ends();
        assert_eq!(ends.doubt, Doubt::None, "a release hides no make");
        assert_eq!(
            ends.until,
            BTreeMap::from([("tern".to_owned(), soon)]),
            "and ends nothing it might have named"
        );
    }

    #[test]
    fn a_book_it_couldnt_read_is_read_again_once_it_can() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("a temporary folder");
        let path = dir.path().join("made.jsonl");
        let book = Book::at(path.clone()).of_realm("work");
        let soon = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1);
        book.made(&made_at("tern", "p1", soon))
            .expect("a make is kept");
        let mode = |mode| fs::set_permissions(&path, fs::Permissions::from_mode(mode));
        mode(0o000).expect("the book is shut");
        if fs::read(&path).is_ok() {
            // Whoever runs this reads past permissions, as root does.
            return;
        }
        assert!(
            matches!(book.ends().doubt, Doubt::Any(_)),
            "a book it can't read doubts every machine"
        );
        mode(0o644).expect("the book opens again");
        assert_eq!(
            book.ends(),
            Ends {
                until: BTreeMap::from([("tern".to_owned(), soon)]),
                doubt: Doubt::None,
            },
            "and is read again once it can be, though only its mode changed"
        );
    }

    #[test]
    fn the_book_stands_by_each_machine_its_realm_made_and_has_not_released() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let path = dir.path().join("made.jsonl");
        let book = Book::at(path.clone()).of_realm("work");
        let now = jiff::Timestamp::now();
        let later = now + jiff::SignedDuration::from_hours(1);
        assert_eq!(
            book.standing().expect("a book not yet kept reads"),
            Standing::default(),
            "a book never kept holds nothing"
        );
        let older = made_at("skiff", "p0", later);
        Book::at(path.clone())
            .made(&older)
            .expect("a make naming no realm is kept");
        let (kept, gone, its_place, its_name, ended, again) = (
            made_at("tern", "p1", later),
            made_at("osprey", "p2", later),
            made_at("osprey", "p3", later),
            made_at("kestrel", "p2", later),
            made_at("kestrel", "p4", now - jiff::SignedDuration::from_secs(1)),
            made_at("osprey", "p5", later),
        );
        for made in [&kept, &gone, &its_place, &its_name, &ended] {
            book.made(made).expect("a make is kept");
        }
        let home = Book::at(path.clone()).of_realm("home");
        home.made(&made_at("tern", "p6", later))
            .expect("another realm's make is kept");
        home.released(&kept, "its time was up")
            .expect("another realm's release is kept");
        book.released(&gone, "its time was up")
            .expect("a release is kept");
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("the book opens");
        // Cut short by a crash, so the next is kept on the same line.
        file.write_all(b"{\"at\": \"cut \xff sh")
            .expect("a line cut short is written");
        book.made(&again).expect("a make is kept");
        file.write_all(b"not a line of the book\n")
            .expect("a line it can't read is written");
        assert_eq!(
            book.standing().expect("the book reads"),
            Standing {
                made: vec![kept, its_place, its_name, ended, again],
                unread: 2,
                doubt: Doubt::Any(
                    "line 10 can't be read, nor which machine it may have made".to_owned()
                ),
                unnamed: vec![older],
            },
            "each machine made and not released, oldest first, ended or not, \
             a release taking only the machine of its name at its place, \
             another realm's makes and releases left to it, those that name \
             no realm kept apart, and the line cut short counted"
        );
    }

    #[tokio::test]
    async fn a_program_that_makes_nothing_says_why() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let failing = provider(dir.path(), "echo 'quota exceeded' >&2\nexit 3");
        let error = make(&offer_of(failing), Duration::from_secs(60), "person")
            .await
            .expect_err("nothing is made");
        assert!(
            error.ends_with("quota exceeded"),
            "it says what the program said: {error}"
        );

        let other = tempfile::tempdir().expect("a temporary folder");
        let vague = provider(other.path(), "echo '{\"place\": \"sbx-1\"}'");
        let error = make(&offer_of(vague), Duration::from_secs(60), "person")
            .await
            .expect_err("nothing is taken as made");
        assert!(
            error.contains("isn't what it made"),
            "a machine with no name or end isn't taken as made: {error}"
        );
    }

    #[tokio::test]
    async fn a_program_is_told_who_asked_for_the_machine() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let told = dir.path().join("told");
        let program = provider(
            dir.path(),
            &format!(
                "echo \"${ASKER}\" > {}\necho '{{\"place\": \"sbx-1\", \"machine\": \"lent-1\", \"until\": \"2026-10-31T20:00:00Z\"}}'",
                told.display()
            ),
        );
        for asker in ["person", "QK4T7Z"] {
            make(&offer_of(program.clone()), Duration::from_secs(60), asker)
                .await
                .expect("it is made");
            assert_eq!(
                fs::read_to_string(&told).expect("it was told"),
                format!("{asker}\n"),
                "the program is told who asked, so it can refuse anyone it doesn't serve"
            );
        }
    }
}
