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
    /// The makes that name no realm, kept before makes named one.
    pub unnamed: usize,
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
            if kept.realm.is_none() && kept.made.is_some() {
                standing.unnamed += 1;
            }
            if kept.realm.is_none() || kept.realm != self.realm {
                continue;
            }
            if let Some(made) = kept.made {
                standing.made.push(made);
            } else if let Some(released) = kept.released {
                standing.made.retain(|made| {
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
                unnamed: 1,
            },
            "each machine made and not released, oldest first, ended or not, \
             a release taking only the machine of its name at its place, \
             another realm's makes and releases left to it, none that names \
             no realm but counted, and the line cut short counted"
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
