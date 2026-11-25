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

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::offer::{Costs, Offer};

/// How long a program has to make a machine: a provider makes one in
/// seconds, and a minute or two when it is busy.
pub const MADE_WITHIN: Duration = Duration::from_secs(5 * 60);

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

/// What a program says it kept: the make's words, with the shape and
/// costs it now runs at.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptSaid {
    place: String,
    machine: String,
    until: jiff::Timestamp,
    shape: String,
    costs: Costs,
}

/// Asks `made`'s program to keep it with no end it is held to, as the
/// person's machine is kept: `PROGRAM keep PLACE --lasting 0`, told the
/// asker its make was told. The program may stop and start it on the
/// way, which ends what it held, and says where it is, its shape, its
/// costs and its new end, as one JSON line.
///
/// # Errors
///
/// When `made` wasn't made for the person, or its program can't be run,
/// ends with a failure or not within [`MADE_WITHIN`], or says something
/// other than that it kept this machine at this place, saying which.
pub async fn keep(made: &Made) -> Result<Made, String> {
    if made.asker.as_deref() != Some(PERSON) {
        return Err(format!(
            "{} wasn't made for the person, so it is kept only to its end",
            made.machine
        ));
    }
    let program = &made.provider;
    let mut command = tokio::process::Command::new(program);
    command
        .args(["keep", &made.place, "--lasting", "0"])
        .env(ASKER, PERSON)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let ran = tokio::time::timeout(MADE_WITHIN, command.output())
        .await
        .map_err(|_| {
            format!(
                "{program} keep {} said nothing within {}s",
                made.place,
                MADE_WITHIN.as_secs()
            )
        })?
        .map_err(|error| format!("{program} keep couldn't run: {error}"))?;
    if !ran.status.success() {
        let said = String::from_utf8_lossy(&ran.stderr);
        return Err(format!(
            "{program} keep {} ended {}: {}",
            made.place,
            ran.status,
            said.trim()
        ));
    }
    let text = String::from_utf8_lossy(&ran.stdout);
    let said: KeptSaid = serde_json::from_str(text.trim()).map_err(|error| {
        format!(
            "{program} keep {} said {}, which isn't what it kept: {error}",
            made.place,
            text.trim()
        )
    })?;
    if said.place != made.place || said.machine != made.machine {
        return Err(format!(
            "{program} keep {} said it kept {} at {}, not {} at {}",
            made.place, said.machine, said.place, made.machine, made.place
        ));
    }
    Ok(Made {
        shape: said.shape,
        until: said.until,
        costs: said.costs,
        ..made.clone()
    })
}

/// Where a node keeps each make and release, a JSON line each, newest
/// last. Every realm's node on a machine keeps the same book, so each
/// make names the realm whose node made it.
#[derive(Debug, Clone)]
pub struct Book {
    path: PathBuf,
    realm: Option<String>,
}

/// The machines a book says are made and not yet released, and how many
/// of its lines it couldn't read.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Standing {
    /// Each machine, oldest first.
    pub made: Vec<Made>,
    /// The lines passed over, each cut short by a crash among them.
    pub unread: usize,
    /// The makes that name no realm, kept before makes named one, and not
    /// released by a line naming none either, oldest first.
    pub unnamed: Vec<Made>,
}

impl Book {
    /// A book kept at `path`, its makes naming no realm.
    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        Self { path, realm: None }
    }

    /// The same book, kept by `realm`'s node.
    #[must_use]
    pub fn of_realm(self, realm: &str) -> Self {
        Self {
            realm: Some(realm.to_owned()),
            ..self
        }
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
        for line in String::from_utf8_lossy(&bytes).lines() {
            let (kept, whole) = kept(line);
            if !whole {
                standing.unread += 1;
            }
            let Some(kept) = kept else {
                continue;
            };
            let kept_by = if kept.realm.is_none() {
                &mut standing.unnamed
            } else if kept.realm == self.realm {
                &mut standing.made
            } else {
                continue;
            };
            if let Some(made) = kept.made {
                // A machine made again at its place, as a keep books it,
                // stands as it was booked last.
                kept_by.retain(|was| was.machine != made.machine || was.place != made.place);
                kept_by.push(made);
            } else if let Some(released) = kept.released {
                kept_by.retain(|made| {
                    made.machine != released.machine || made.place != released.place
                });
            }
        }
        Ok(standing)
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

/// What `line` keeps, and whether it reads whole; where it doesn't, what
/// the last line kept within it does, since a line cut short by a crash
/// has the next written straight after it.
fn kept(line: &str) -> (Option<Kept>, bool) {
    if let Ok(whole) = serde_json::from_str(line) {
        return (Some(whole), true);
    }
    let after = line
        .rfind(r#"{"at""#)
        .filter(|at| *at > 0)
        .and_then(|next| serde_json::from_str(&line[next..]).ok());
    (after, false)
}

/// Which machine a release let go.
#[derive(Deserialize)]
struct Released {
    machine: String,
    place: String,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::offer::Shape;

    /// A program at `dir`'s `provider` running `body`.
    fn provider(dir: &std::path::Path, body: &str) -> String {
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
                unnamed: vec![older],
            },
            "each machine made and not released, oldest first, ended or not, \
             a release taking only the machine of its name at its place, \
             another realm's makes and releases left to it, those that name \
             no realm kept apart, and the line cut short counted"
        );
    }

    #[test]
    fn a_machine_made_again_at_its_place_stands_as_booked_last() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let book = Book::at(dir.path().join("made.jsonl")).of_realm("work");
        let now = jiff::Timestamp::now();
        let made = made_at("wren", "p1", now + jiff::SignedDuration::from_hours(12));
        let kept = Made {
            until: "9999-01-01T00:00:00Z".parse().expect("a time"),
            shape: "standard".to_owned(),
            ..made.clone()
        };
        book.made(&made).expect("a make is kept");
        book.made(&kept).expect("a keep is kept");
        assert_eq!(
            book.standing().expect("the book reads").made,
            vec![kept.clone()],
            "a keep books the machine again, and only as it was kept"
        );
        book.released(&kept, "its work handed over")
            .expect("a release is kept");
        assert_eq!(
            book.standing().expect("the book reads").made,
            Vec::new(),
            "one release lets go a machine booked twice"
        );
    }

    /// A make for the person by `program`, at place `sbx-1` as `lent-1`.
    fn made_for_the_person(program: String) -> Made {
        Made {
            provider: program,
            asker: Some(PERSON.to_owned()),
            ..made_at("lent-1", "sbx-1", jiff::Timestamp::now())
        }
    }

    /// The line a program answers a keep with, saying `lent-1` stands at
    /// `place` with no end, as a standard room at 0.21 USD an hour.
    fn kept_at(place: &str) -> String {
        format!(
            "echo '{{\"place\": \"{place}\", \"machine\": \"lent-1\", \
             \"until\": \"9999-01-01T00:00:00Z\", \"shape\": \"standard\", \
             \"costs\": {{\"hourly\": \"0.21\", \"currency\": \"USD\"}}}}'"
        )
    }

    #[tokio::test]
    async fn a_program_keeps_the_persons_machine_saying_how_it_now_runs() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let told = dir.path().join("told");
        let program = provider(
            dir.path(),
            &format!(
                "echo \"$* ${ASKER}\" > {}\n{}",
                told.display(),
                kept_at("sbx-1")
            ),
        );
        let made = made_for_the_person(program);
        let kept = keep(&made).await.expect("it is kept");
        assert_eq!(
            fs::read_to_string(&told).expect("it was asked"),
            "keep sbx-1 --lasting 0 person\n",
            "the program is asked to keep the place with no end, as the person"
        );
        assert_eq!(
            kept,
            Made {
                shape: "standard".to_owned(),
                until: "9999-01-01T00:00:00Z".parse().expect("a time"),
                costs: Costs {
                    hourly: "0.21".to_owned(),
                    currency: "USD".to_owned(),
                },
                ..made
            },
            "it is booked at its new shape, costs and end, for the same asker"
        );
    }

    #[tokio::test]
    async fn only_the_persons_machine_is_kept_and_only_as_itself() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let asked = dir.path().join("asked");
        let program = provider(
            dir.path(),
            &format!("touch {}\n{}", asked.display(), kept_at("sbx-2")),
        );
        for asker in [None, Some("QK4T7Z"), Some(UNKNOWN)] {
            let made = Made {
                asker: asker.map(str::to_owned),
                ..made_for_the_person(program.clone())
            };
            let error = keep(&made).await.expect_err("it isn't kept");
            assert!(
                error.contains("wasn't made for the person"),
                "a machine made for {asker:?} is kept only to its end: {error}"
            );
        }
        assert!(!asked.exists(), "no program is asked to keep another's");
        let error = keep(&made_for_the_person(program))
            .await
            .expect_err("another place isn't taken as kept");
        assert!(
            error.contains("not lent-1 at sbx-1"),
            "a keep saying another place keeps the booking as it was: {error}"
        );
        let other = tempfile::tempdir().expect("a temporary folder");
        let failing = provider(other.path(), "echo 'stopped; no standard room' >&2\nexit 1");
        let error = keep(&made_for_the_person(failing))
            .await
            .expect_err("a failed keep keeps nothing");
        assert!(
            error.ends_with("stopped; no standard room"),
            "it says what the program said: {error}"
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
