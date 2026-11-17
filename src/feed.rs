//! Feeding: a receptor saying one change to the one cell whose quest
//! claims it, rather than a count to everyone on its network.
//!
//! A receptor keeps the changes it read beside its counts, in a file of
//! their own, `<state>/receptors/<source>.items.json`, written whole and
//! renamed into place, its owner's alone, and never served or carried
//! across a link: what reaches anyone of an item is the words this node
//! makes of it, and only for the cell that claims it. An item holds three
//! fields and nothing else: the repository, as `owner/name`, the change's
//! number, and why it is there, a word from a short list. Never a title,
//! a body, an author, a branch or a comment.
//!
//! A quest claims a change by its README's `github-pr` alias,
//! `owner/name#N`, matched whole and never by a prefix. The cells fed are
//! the sessions that quest's README lists as active, running on this
//! machine and not ended: a session's own word that it works on a quest
//! is not enough, so no session can draw another quest's changes to
//! itself. Each cell is fed an item once, for as long as the item stands.
//! An item goes from the file once its change closes or its reason no
//! longer holds, and never stays past [`KEPT_FOR_DAYS`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rustix::fd::AsFd;
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::receptor::Source;

/// The kind of a feeding's records in the daemon interface.
pub const KIND: &str = "fed";

/// How long a feeding is served after it was said, in seconds, so the
/// map draws the line while it is fresh and lets it fade.
pub const SHOWN_FOR: i64 = 600;

/// The longest an item is kept, in days, whatever its receptor says.
pub const KEPT_FOR_DAYS: i64 = 30;

/// The most items a file may hold: more than any person is asked at once.
const MOST_ITEMS: usize = 200;

/// The most an items file may hold, in bytes: [`MOST_ITEMS`] at well
/// under 200 bytes each.
const LARGEST_FILE: u64 = 64 * 1024;

/// The longest name GitHub gives an owner or a repository.
const LONGEST_NAME: usize = 100;

/// Mode of an items file: its owner's alone.
const FILE_MODE: rustix::fs::RawMode = 0o600;

/// Why a change is among a receptor's items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// Its person's review is asked.
    ReviewAsked,
}

impl Reason {
    /// The kind of signal it makes, in a word, as a feeding's record says.
    #[must_use]
    pub fn kind(self) -> &'static str {
        match self {
            Self::ReviewAsked => "review",
        }
    }
}

/// One change a receptor read, in three fields and when it was first
/// read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    /// Its repository, as `owner/name`.
    pub repo: String,
    /// Its number in that repository.
    pub number: u64,
    /// Why it is here.
    pub reason: Reason,
    /// When it was first read, so it is let go after
    /// [`KEPT_FOR_DAYS`].
    pub since: Timestamp,
}

impl Item {
    /// The change it is, as a quest's alias names it: `owner/name#N`.
    #[must_use]
    pub fn change(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }

    /// What its cell is told, made of its fields alone.
    #[must_use]
    pub fn words(&self, source: Source) -> String {
        match self.reason {
            Reason::ReviewAsked => format!(
                "{}: your person's review is asked on {}",
                source.name(),
                self.change()
            ),
        }
    }
}

/// An items file as written: whose they are, and the items.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Written {
    source: Source,
    realm: String,
    items: Vec<Item>,
}

/// How an items file's name ends, after its source's name.
pub const ITEMS: &str = ".items.json";

/// The items file of `source`'s receptor in the receptors' `folder`.
#[must_use]
pub fn file(folder: &Path, source: Source) -> PathBuf {
    folder.join(format!("{}{ITEMS}", source.name()))
}

fn refused(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, why.into())
}

/// Whether `name` is one GitHub could give an owner or a repository:
/// letters, digits, `-`, `_` and `.`, and not only dots.
fn is_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= LONGEST_NAME
        && name.chars().any(|each| each != '.')
        && name
            .chars()
            .all(|each| each.is_ascii_alphanumeric() || matches!(each, '-' | '_' | '.'))
}

/// The change `said` names, as `owner/name#N`, as a repository and a
/// number, held to `owners`: refused when the repository isn't one of
/// theirs, isn't a name GitHub could give, or the number isn't a
/// positive whole one.
///
/// # Errors
///
/// When any of those holds.
pub fn change_of(said: &str, owners: &[String]) -> io::Result<(String, u64)> {
    let (repo, number) = said
        .split_once('#')
        .ok_or_else(|| refused(format!("{said:?} names no change, as owner/name#N")))?;
    let number: u64 = number
        .parse()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| refused(format!("{said:?} has no positive number")))?;
    let (owner, name) = repo
        .split_once('/')
        .filter(|(owner, name)| is_name(owner) && is_name(name))
        .ok_or_else(|| refused(format!("{said:?} names no repository, as owner/name")))?;
    if !owners.iter().any(|each| each == owner) {
        return Err(refused(format!(
            "{owner:?} isn't among the owners this receptor reads"
        )));
    }
    Ok((format!("{owner}/{name}"), number))
}

/// Whether `item` is still to be kept at `now`: first read less than
/// [`KEPT_FOR_DAYS`] of whole days before it, and not after it, since a
/// time ahead of the clock would otherwise keep it for good.
fn fresh(item: &Item, now: Timestamp) -> bool {
    let age = now.as_second() - item.since.as_second();
    (0..KEPT_FOR_DAYS * SECONDS_A_DAY).contains(&age)
}

/// The seconds in a whole day, as a timestamp counts them.
const SECONDS_A_DAY: i64 = 86_400;

/// What `folder` holds for `source` of `realm`, opened never through a
/// link and without waiting on a writer, or none where it's another
/// realm's, malformed, too large, or holds an item no receptor could
/// have written.
fn read_at(opened: impl AsFd, source: Source, realm: &str) -> Option<Vec<Item>> {
    let name = format!("{}{ITEMS}", source.name());
    let stat = rustix::fs::statat(&opened, &name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return None;
    }
    let file = fs::File::from(
        rustix::fs::openat(
            &opened,
            &name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .ok()?,
    );
    if !file.metadata().ok()?.file_type().is_file() {
        return None;
    }
    let mut text = Vec::new();
    file.take(LARGEST_FILE + 1).read_to_end(&mut text).ok()?;
    if u64::try_from(text.len()).ok()? > LARGEST_FILE {
        return None;
    }
    let written: Written = serde_json::from_slice(&text).ok()?;
    let sound = written.source == source
        && written.realm == realm
        && written.items.len() <= MOST_ITEMS
        && written.items.iter().all(|item| {
            item.number > 0
                && item
                    .repo
                    .split_once('/')
                    .is_some_and(|(owner, name)| is_name(owner) && is_name(name))
        });
    sound.then_some(written.items)
}

/// The items of `source`'s receptor of `realm` in the receptors'
/// `folder` still kept at `now`: none where the file or its folder is
/// missing, since a receptor that never named an item has none, and
/// `None` where either is there but unsound, since what it holds can't
/// be known, and its feeding should stand as it was rather than start
/// over.
#[must_use]
pub fn read(folder: &Path, source: Source, realm: &str, now: Timestamp) -> Option<Vec<Item>> {
    let opened = match rustix::fs::open(
        folder,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(opened) => opened,
        Err(rustix::io::Errno::NOENT) => return Some(Vec::new()),
        Err(_) => return None,
    };
    let name = format!("{}{ITEMS}", source.name());
    if matches!(
        rustix::fs::statat(&opened, &name, AtFlags::SYMLINK_NOFOLLOW),
        Err(rustix::io::Errno::NOENT)
    ) {
        return Some(Vec::new());
    }
    let items = read_at(&opened, source, realm)?;
    Some(items.into_iter().filter(|item| fresh(item, now)).collect())
}

/// `items` written whole for `source`'s receptor of `realm` in the
/// receptors' `folder` as it reads them at `now`, over what it said
/// before: an item kept keeps when it was first read, one no longer said
/// is gone, and one first read more than [`KEPT_FOR_DAYS`] ago isn't
/// kept.
///
/// # Errors
///
/// When there are more than [`MOST_ITEMS`], or the file can't be
/// written.
pub fn write(
    folder: &Path,
    source: Source,
    realm: &str,
    items: &[(String, u64, Reason)],
    now: Timestamp,
) -> io::Result<PathBuf> {
    if items.len() > MOST_ITEMS {
        return Err(refused(format!(
            "a receptor keeps at most {MOST_ITEMS} items, not {}",
            items.len()
        )));
    }
    let opened = crate::receptor::owned(folder)?;
    let was: BTreeMap<(String, u64, Reason), Timestamp> = read_at(&opened, source, realm)
        .unwrap_or_default()
        .into_iter()
        .map(|item| ((item.repo, item.number, item.reason), item.since))
        .collect();
    let mut seen = BTreeSet::new();
    let kept: Vec<Item> = items
        .iter()
        .filter(|each| seen.insert((*each).clone()))
        .map(|(repo, number, reason)| Item {
            // Never later than now, so a time ahead of the clock can't
            // keep an item past its days.
            since: was
                .get(&(repo.clone(), *number, *reason))
                .copied()
                .map_or(now, |since| since.min(now)),
            repo: repo.clone(),
            number: *number,
            reason: *reason,
        })
        .filter(|item| fresh(item, now))
        .collect();
    let body = serde_json::to_vec(&Written {
        source,
        realm: realm.to_owned(),
        items: kept,
    })
    .map_err(io::Error::other)?;
    let name = format!("{}{ITEMS}", source.name());
    let staged = format!(".{name}.{}", std::process::id());
    match rustix::fs::unlinkat(&opened, &staged, AtFlags::empty()) {
        Err(rustix::io::Errno::NOENT) | Ok(()) => {}
        Err(error) => return Err(error.into()),
    }
    let mut out = fs::File::from(rustix::fs::openat(
        &opened,
        &staged,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(FILE_MODE),
    )?);
    out.write_all(&body)?;
    out.sync_all()?;
    drop(out);
    rustix::fs::renameat(&opened, &staged, &opened, &name)?;
    Ok(file(folder, source))
}

/// A cell that may be fed: its address, its session, and what its quest
/// says of itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claimant {
    /// Its address.
    pub address: String,
    /// Its session's id.
    pub session: String,
    /// Whether it is running, not ended or out of reach.
    pub live: bool,
    /// The realm its quest's README says it is of.
    pub realm: Option<String>,
    /// The changes its quest claims, as `owner/name#N`.
    pub claims: Vec<String>,
    /// The sessions its quest lists as active, by id.
    pub sessions: Vec<String>,
}

impl Claimant {
    /// Whether it is to be fed `item` in `realm`.
    fn claims(&self, item: &Item, realm: &str) -> bool {
        let change = item.change();
        self.live
            && self.realm.as_deref() == Some(realm)
            && self.sessions.contains(&self.session)
            && self.claims.contains(&change)
    }
}

/// One cell fed one item: who, what, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fed {
    /// The cell fed, by address.
    pub cell: String,
    /// The item it was fed.
    pub item: Item,
    /// When.
    pub at: Timestamp,
}

/// What one pass fed, and whether what is served of it moved.
#[derive(Debug, Default)]
pub struct Feeding {
    /// The feedings said this pass.
    pub fed: Vec<Fed>,
    /// Whether a feeding was added to what is served, or left it.
    pub moved: bool,
}

/// What a node has fed, so each cell is fed an item once while it stands.
#[derive(Debug, Default)]
pub struct Feeder {
    /// Each cell and change fed, with why, while the item stands.
    said: BTreeSet<(String, String, Reason)>,
    /// What was fed lately, newest last, to be served a while.
    lately: Vec<Fed>,
}

impl Feeder {
    /// The cells of `realm` among `claimants` that `items` feed now, at
    /// `now`, each once: an item fed before is fed again only once it
    /// went and came back. Each is taken as said.
    pub fn feed(
        &mut self,
        items: &[Item],
        claimants: &[Claimant],
        realm: &str,
        now: Timestamp,
    ) -> Vec<Fed> {
        self.feed_saying(items, claimants, realm, now, |_| true).fed
    }

    /// As [`Feeder::feed`], each feeding kept only once `say` says it:
    /// one it couldn't say isn't marked fed or served, so the next pass
    /// tries it again.
    pub fn feed_saying(
        &mut self,
        items: &[Item],
        claimants: &[Claimant],
        realm: &str,
        now: Timestamp,
        mut say: impl FnMut(&Fed) -> bool,
    ) -> Feeding {
        let standing: BTreeSet<(String, Reason)> = items
            .iter()
            .map(|item| (item.change(), item.reason))
            .collect();
        self.said
            .retain(|(_, change, reason)| standing.contains(&(change.clone(), *reason)));
        let mut fed = Vec::new();
        for item in items {
            for claimant in claimants.iter().filter(|each| each.claims(item, realm)) {
                let said = (claimant.address.clone(), item.change(), item.reason);
                if self.said.contains(&said) {
                    continue;
                }
                let feeding = Fed {
                    cell: claimant.address.clone(),
                    item: item.clone(),
                    at: now,
                };
                if say(&feeding) {
                    self.said.insert(said);
                    fed.push(feeding);
                }
            }
        }
        let moved = self.fade(now) || !fed.is_empty();
        self.lately.extend(fed.iter().cloned());
        Feeding { fed, moved }
    }

    /// Lets go of what was fed [`SHOWN_FOR`] or more before `now`, saying
    /// whether any went. A pass that can't read a source's items fades
    /// its feedings all the same, so no line outlasts its ten minutes.
    pub fn fade(&mut self, now: Timestamp) -> bool {
        let shown = self.lately.len();
        self.lately
            .retain(|each| now.as_second() - each.at.as_second() < SHOWN_FOR);
        self.lately.len() != shown
    }

    /// The daemon interface's records of what was fed by `source`'s
    /// receptor of `machine` within [`SHOWN_FOR`] of `now`, by key, the
    /// latest for each cell: who fed whom, when, and the kind in a word,
    /// never the item itself.
    #[must_use]
    pub fn records(
        &self,
        source: Source,
        machine: &str,
        now: Timestamp,
    ) -> BTreeMap<String, Value> {
        let receptor = crate::stimuli::cell(source, machine);
        self.lately
            .iter()
            .filter(|each| now.as_second() - each.at.as_second() < SHOWN_FOR)
            .map(|each| {
                let id = format!("{receptor}~{}", each.cell);
                let fields = json!({
                    "receptor": receptor,
                    "cell": each.cell,
                    "at": each.at.to_string(),
                    "kind": each.item.reason.kind(),
                });
                (
                    crate::daemon::key(KIND, &id),
                    crate::daemon::record(KIND, &id, each.at.as_millisecond(), fields),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn at(text: &str) -> Timestamp {
        text.parse().expect("a time")
    }

    fn owners() -> Vec<String> {
        vec!["jsama".to_owned(), "osprey-works".to_owned()]
    }

    fn item(repo: &str, number: u64) -> Item {
        Item {
            repo: repo.to_owned(),
            number,
            reason: Reason::ReviewAsked,
            since: at("2026-11-18T06:00:00Z"),
        }
    }

    fn cell(address: &str, claims: &[&str]) -> Claimant {
        Claimant {
            address: address.to_owned(),
            session: format!("session-{address}"),
            live: true,
            realm: Some("work".to_owned()),
            claims: claims.iter().map(|each| (*each).to_owned()).collect(),
            sessions: vec![format!("session-{address}")],
        }
    }

    fn fed_cells(fed: &[Fed]) -> Vec<&str> {
        fed.iter().map(|each| each.cell.as_str()).collect()
    }

    const NOW: &str = "2026-11-18T06:00:00Z";

    #[test]
    fn a_feeding_fades_at_ten_minutes_though_its_source_wasnt_read() {
        let mut feeder = Feeder::default();
        let cells = [cell("KSTRL1", &["jsama/tern#12"])];
        feeder.feed(&[item("jsama/tern", 12)], &cells, "work", at(NOW));
        let shown = |feeder: &Feeder, when| feeder.records(Source::Github, "skiff", at(when));
        assert!(
            !feeder.fade(at("2026-11-18T06:09:59Z")),
            "a feeding went early"
        );
        assert_eq!(shown(&feeder, "2026-11-18T06:09:59Z").len(), 1);
        assert!(
            feeder.fade(at("2026-11-18T06:10:00Z")),
            "a feeding outlasted its ten minutes unsaid"
        );
        assert!(feeder.lately.is_empty(), "a faded feeding was kept");
    }

    #[test]
    fn a_claim_in_another_case_claims_nothing() {
        let mut feeder = Feeder::default();
        let cells = [
            cell("KSTRL1", &["Jsama/tern#12"]),
            cell("KSTRL2", &["jsama/Tern#12"]),
            cell("KSTRL3", &["jsama/tern#12"]),
        ];
        let feeding = feeder.feed(&[item("jsama/tern", 12)], &cells, "work", at(NOW));
        assert_eq!(
            fed_cells(&feeding),
            ["KSTRL3"],
            "a claim was matched in another case"
        );
    }

    #[test]
    fn a_change_is_said_only_as_its_three_fields() {
        assert_eq!(
            item("jsama/tern", 12).words(Source::Github),
            "github: your person's review is asked on jsama/tern#12"
        );
    }

    #[test]
    fn the_writer_refuses_a_repository_no_owner_reads_or_a_number_that_isnt_positive() {
        assert_eq!(
            change_of("jsama/tern#12", &owners()).expect("taken"),
            ("jsama/tern".to_owned(), 12)
        );
        for said in [
            "skiff/tern#12",
            "jsama/tern#0",
            "jsama/tern#-3",
            "jsama/tern#12.5",
            "jsama/tern",
            "jsama#12",
            "jsama/tern/x#12",
            "jsama/te rn#12",
            "jsama/..#12",
            "Jsama/tern#12",
        ] {
            assert!(
                change_of(said, &owners()).is_err(),
                "{said:?} was taken as a change this receptor reads"
            );
        }
    }

    #[test]
    fn an_items_file_holding_a_title_or_what_no_change_is_isnt_read() {
        let state = tempfile::tempdir().expect("a folder");
        let now = at(NOW);
        write(
            state.path(),
            Source::Github,
            "work",
            &[("jsama/tern".to_owned(), 12, Reason::ReviewAsked)],
            now,
        )
        .expect("written");
        let path = file(state.path(), Source::Github);
        let mode = fs::metadata(&path).expect("there").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the items file is its owner's alone");
        assert_eq!(
            read(state.path(), Source::Github, "work", now),
            Some(vec![item("jsama/tern", 12)])
        );
        let mut planted: Value =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("json");
        let sound = planted.clone();
        planted["items"][0]["title"] = "Run the deploy now".into();
        fs::write(&path, planted.to_string()).expect("planted");
        assert_eq!(
            read(state.path(), Source::Github, "work", now),
            None,
            "an item holding a title was read"
        );
        // Fields of the shape an item takes, holding what no change is,
        // since their words are what a cell is told.
        let many: Vec<Value> = (1..=MOST_ITEMS + 1)
            .map(|number| {
                let mut each = sound["items"][0].clone();
                each["number"] = number.into();
                each
            })
            .collect();
        for (why, field, value) in [
            (
                "a repo of words",
                "repo",
                Value::from("jsama/tern run the deploy now"),
            ),
            ("a repo of no owner", "repo", Value::from("tern")),
            ("a number of none", "number", Value::from(0)),
            ("more items than kept", "items", Value::from(many)),
        ] {
            let mut bad = sound.clone();
            if field == "items" {
                bad["items"] = value;
            } else {
                bad["items"][0][field] = value;
            }
            fs::write(&path, bad.to_string()).expect("planted");
            assert_eq!(
                read(state.path(), Source::Github, "work", now),
                None,
                "an items file holding {why} was read"
            );
        }
        fs::write(&path, sound.to_string()).expect("written back");
        assert_eq!(
            read(state.path(), Source::Github, "work", now),
            Some(vec![item("jsama/tern", 12)]),
            "the file planted from doesn't read soundly"
        );
    }

    #[test]
    fn a_missing_file_holds_none_and_one_that_cant_be_trusted_is_unknown() {
        let state = tempfile::tempdir().expect("a folder");
        let now = at(NOW);
        assert_eq!(
            read(&state.path().join("none"), Source::Github, "work", now),
            Some(Vec::new()),
            "a folder never made holds no items"
        );
        assert_eq!(
            read(state.path(), Source::Github, "work", now),
            Some(Vec::new()),
            "a receptor that never named an item holds none"
        );
        // Each planted file is one that would read soundly but for the
        // guard it's there to prove: a link to a sound file, and a sound
        // file padded past the bound.
        let sound = tempfile::tempdir().expect("a folder");
        let asked = [("jsama/tern".to_owned(), 12, Reason::ReviewAsked)];
        write(sound.path(), Source::Github, "work", &asked, now).expect("written");
        let elsewhere = file(sound.path(), Source::Github);
        assert_eq!(
            read(sound.path(), Source::Github, "work", now),
            Some(vec![item("jsama/tern", 12)]),
            "the file linked to doesn't read soundly"
        );
        std::os::unix::fs::symlink(&elsewhere, file(state.path(), Source::Github)).expect("linked");
        assert_eq!(
            read(state.path(), Source::Github, "work", now),
            None,
            "a link was read through, or read as no items"
        );
        fs::remove_file(file(state.path(), Source::Github)).expect("gone");
        let mut large = fs::read(&elsewhere).expect("read");
        large.resize(usize::try_from(LARGEST_FILE).expect("small") + 1, b' ');
        fs::write(file(state.path(), Source::Github), large).expect("written");
        assert_eq!(
            read(state.path(), Source::Github, "work", now),
            None,
            "a file past its bound was read, or read as no items"
        );
    }

    #[test]
    fn an_item_first_read_after_now_or_at_thirty_days_isnt_kept() {
        let now = at(NOW);
        let since = |seconds: i64| Item {
            since: Timestamp::from_second(now.as_second() - seconds).expect("a time"),
            ..item("jsama/tern", 12)
        };
        assert!(!fresh(&since(-1), now), "an item from the future was kept");
        assert!(fresh(&since(0), now));
        let month = KEPT_FOR_DAYS * SECONDS_A_DAY;
        assert!(fresh(&since(month - 1), now));
        assert!(!fresh(&since(month), now), "an item was kept thirty days");

        let state = tempfile::tempdir().expect("a folder");
        let asked = [("jsama/tern".to_owned(), 12, Reason::ReviewAsked)];
        let ahead = at("2026-12-01T00:00:00Z");
        write(state.path(), Source::Github, "work", &asked, ahead).expect("written");
        write(state.path(), Source::Github, "work", &asked, now).expect("written");
        assert_eq!(
            read(state.path(), Source::Github, "work", now),
            Some(vec![item("jsama/tern", 12)]),
            "a time ahead of the clock was carried on"
        );
    }

    #[test]
    fn a_feeding_that_couldnt_be_said_is_neither_kept_nor_lost() {
        let mut feeder = Feeder::default();
        let cells = [cell("KSTRL1", &["jsama/tern#12"])];
        let items = [item("jsama/tern", 12)];
        let failed = feeder.feed_saying(&items, &cells, "work", at(NOW), |_| false);
        assert!(
            failed.fed.is_empty() && !failed.moved,
            "an unsaid feeding was kept"
        );
        assert!(
            feeder
                .records(Source::Github, "kestrel", at(NOW))
                .is_empty(),
            "an unsaid feeding was drawn"
        );
        let said = feeder.feed_saying(&items, &cells, "work", at(NOW), |_| true);
        assert_eq!(fed_cells(&said.fed), ["KSTRL1"], "it was never tried again");
        assert!(said.moved, "the map wasn't told");
    }

    #[test]
    fn the_map_is_told_when_one_feeding_ends_as_another_starts() {
        let mut feeder = Feeder::default();
        let cells = [
            cell("KSTRL1", &["jsama/tern#12"]),
            cell("KSTRL2", &["jsama/tern#13"]),
        ];
        feeder.feed(&[item("jsama/tern", 12)], &cells, "work", at(NOW));
        let later = at("2026-11-18T06:10:00Z");
        let feeding = feeder.feed_saying(&[], &cells, "work", later, |_| true);
        assert!(feeding.moved, "a feeding left the map unsaid");
        let both = [item("jsama/tern", 12), item("jsama/tern", 13)];
        let still = feeder.feed_saying(&both, &cells, "work", later, |_| true);
        assert!(still.moved, "a new feeding left the map unsaid");
        let none = feeder.feed_saying(&both, &cells, "work", later, |_| true);
        assert!(!none.moved, "the map was told of nothing");
    }

    #[test]
    fn an_item_of_another_realm_or_source_is_never_read() {
        let state = tempfile::tempdir().expect("a folder");
        let now = at(NOW);
        write(
            state.path(),
            Source::Github,
            "work",
            &[("jsama/tern".to_owned(), 12, Reason::ReviewAsked)],
            now,
        )
        .expect("written");
        assert_eq!(
            read(state.path(), Source::Github, "personal", now),
            None,
            "a work item was read for the personal realm"
        );
        fs::rename(
            file(state.path(), Source::Github),
            file(state.path(), Source::Slack),
        )
        .expect("moved");
        assert_eq!(
            read(state.path(), Source::Slack, "work", now),
            None,
            "GitHub's items were read as Slack's"
        );
    }

    #[test]
    fn an_item_goes_once_it_is_no_longer_said_and_never_stays_past_thirty_days() {
        let state = tempfile::tempdir().expect("a folder");
        let first = at(NOW);
        let both = [
            ("jsama/tern".to_owned(), 12, Reason::ReviewAsked),
            ("jsama/tern".to_owned(), 13, Reason::ReviewAsked),
        ];
        write(state.path(), Source::Github, "work", &both, first).expect("written");
        let later = at("2026-11-20T06:00:00Z");
        write(state.path(), Source::Github, "work", &both[1..], later).expect("written");
        assert_eq!(
            read(state.path(), Source::Github, "work", later),
            Some(vec![item("jsama/tern", 13)]),
            "a closed change stayed, or one kept lost when it was first read"
        );
        let month = at("2026-12-18T06:00:01Z");
        assert_eq!(
            read(state.path(), Source::Github, "work", month),
            Some(Vec::new()),
            "an item was read past thirty days"
        );
        write(state.path(), Source::Github, "work", &both[1..], month).expect("written");
        assert_eq!(
            read(state.path(), Source::Github, "work", month),
            Some(Vec::new()),
            "an item was written past thirty days"
        );
    }

    #[test]
    fn an_item_no_quest_claims_feeds_nobody() {
        let mut feeder = Feeder::default();
        let fed = feeder.feed(
            &[item("jsama/tern", 12)],
            &[cell("KSTRL1", &["jsama/tern#13"]), cell("OSPRY1", &[])],
            "work",
            at(NOW),
        );
        assert!(fed.is_empty(), "a change nobody claims fed {fed:?}");
    }

    #[test]
    fn a_claim_matches_its_change_whole_never_by_a_prefix() {
        let mut feeder = Feeder::default();
        let fed = feeder.feed(
            &[item("jsama/tern", 12)],
            &[
                cell("KSTRL1", &["jsama/tern#1"]),
                cell("KSTRL2", &["jsama/tern#123"]),
                cell("KSTRL3", &["jsama/ter#12"]),
                cell("KSTRL4", &["jsama/tern#12"]),
            ],
            "work",
            at(NOW),
        );
        assert_eq!(fed_cells(&fed), ["KSTRL4"]);
    }

    #[test]
    fn two_live_cells_claiming_one_change_are_both_fed_and_a_dormant_one_is_not() {
        let mut feeder = Feeder::default();
        let mut ended = cell("OSPRY1", &["jsama/tern#12"]);
        ended.live = false;
        let fed = feeder.feed(
            &[item("jsama/tern", 12)],
            &[
                cell("KSTRL1", &["jsama/tern#12"]),
                cell("KSTRL2", &["jsama/tern#12"]),
                ended,
            ],
            "work",
            at(NOW),
        );
        assert_eq!(fed_cells(&fed), ["KSTRL1", "KSTRL2"]);
    }

    #[test]
    fn a_session_its_quest_doesnt_list_hears_nothing() {
        let mut feeder = Feeder::default();
        let mut stranger = cell("KSTRL1", &["jsama/tern#12"]);
        stranger.sessions = vec!["session-OSPRY1".to_owned()];
        let fed = feeder.feed(&[item("jsama/tern", 12)], &[stranger], "work", at(NOW));
        assert!(
            fed.is_empty(),
            "a session saying it works on a quest that doesn't list it was fed"
        );
    }

    #[test]
    fn a_personal_cell_is_never_fed_a_work_item() {
        let mut feeder = Feeder::default();
        let mut personal = cell("KSTRL1", &["jsama/tern#12"]);
        personal.realm = Some("personal".to_owned());
        let mut unsaid = cell("KSTRL2", &["jsama/tern#12"]);
        unsaid.realm = None;
        let fed = feeder.feed(
            &[item("jsama/tern", 12)],
            &[personal, unsaid],
            "work",
            at(NOW),
        );
        assert!(fed.is_empty(), "a cell of no work quest was fed {fed:?}");
    }

    #[test]
    fn a_cell_is_fed_an_item_once_while_it_stands() {
        let mut feeder = Feeder::default();
        let cells = [cell("KSTRL1", &["jsama/tern#12"])];
        let items = [item("jsama/tern", 12)];
        assert_eq!(feeder.feed(&items, &cells, "work", at(NOW)).len(), 1);
        assert!(
            feeder
                .feed(&items, &cells, "work", at("2026-11-18T06:30:00Z"))
                .is_empty(),
            "a cell was fed the same item twice"
        );
        feeder.feed(&[], &cells, "work", at("2026-11-18T07:00:00Z"));
        assert_eq!(
            feeder
                .feed(&items, &cells, "work", at("2026-11-18T08:00:00Z"))
                .len(),
            1,
            "a review asked again after it went fed nobody"
        );
    }

    #[test]
    fn a_feeding_is_served_ten_minutes_by_who_fed_whom_never_by_what() {
        let mut feeder = Feeder::default();
        feeder.feed(
            &[item("jsama/tern", 12)],
            &[cell("KSTRL1", &["jsama/tern#12"])],
            "work",
            at(NOW),
        );
        let records = feeder.records(Source::Github, "kestrel", at("2026-11-18T06:09:59Z"));
        let record = records
            .get(&crate::daemon::key(KIND, "github@kestrel~KSTRL1"))
            .expect("served");
        let text = record.to_string();
        assert!(text.contains("\"kind\":\"review\""), "{text}");
        assert!(
            !text.contains("tern"),
            "a feeding's record named its change: {text}"
        );
        assert!(
            feeder
                .records(Source::Github, "kestrel", at("2026-11-18T06:10:00Z"))
                .is_empty(),
            "a feeding was served past its ten minutes"
        );
    }
}
