//! One end of a link's talk about shared quests: which it shares, what it
//! holds of each, and the files the other end lacks.
//!
//! Each end says which quests it shares, and the other starts sharing any
//! it doesn't: its own copy of that quest, if it has one, is moved aside
//! first, so the quest it then takes is the sharing machine's and nothing
//! it held is lost. Each end then says what it holds of each file, by
//! content and version, again whenever a scan finds a change, and asks for
//! each file the other holds newer. A file written on both ends apart is
//! settled by one end alone, the machine whose name sorts first, so the
//! two never both merge and chase each other's merges; the other takes
//! what it settled, which is newer than either.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use super::ledger::{Entry, Ledgers, is_quest, shared_of};
use super::settle::{Merge, Settled, State, beside, settle};
use super::tree::{Place, Quest, Shared, content};
use super::{journey, log};
use crate::group::Group;
use crate::record::Intent;
use crate::wire::Draft;

/// Where this machine keeps quests, and mycelium's own folder.
#[derive(Debug, Clone)]
pub struct Places {
    /// The quest store: a folder a quest.
    pub quests: PathBuf,
    /// The account's home folder.
    pub home: PathBuf,
    /// Mycelium's own folder, where the ledgers are kept and a quest's
    /// copy is moved aside to.
    pub state: PathBuf,
}

impl Places {
    /// Where `id` is kept on this machine. Only its record is carried so
    /// far: a scratch folder can hold hundreds of thousands of files, too
    /// many to look over every second, and secrets whose modes must cross
    /// with them, so it waits on a watch and on modes.
    #[must_use]
    pub fn quest(&self, id: &str) -> Quest {
        Quest::new(self.quests.join(id), None, self.home.clone())
    }

    /// This machine's ledgers.
    #[must_use]
    pub fn ledgers(&self) -> Ledgers {
        Ledgers::in_state(&self.state)
    }

    /// The group the sessions on quest `id` hear, named by the short ids of
    /// the quests above it, outermost first, as the mesh names it: a group
    /// is heard by its own and its parents' names, never a part of one.
    #[must_use]
    pub fn group(&self, id: &str) -> Option<Group> {
        let mut shorts = Vec::new();
        let mut quest = Some(id.to_owned());
        while let Some(at) = quest {
            // A parent named twice is a loop no quest store should hold.
            if shorts.len() > MOST_NESTED {
                return None;
            }
            shorts.push(at.rsplit('-').next()?.to_lowercase());
            let readme = fs::read_to_string(self.quests.join(&at).join("README.md")).ok()?;
            quest = parent(&readme);
        }
        shorts.reverse();
        Group::new(format!("quest.{}", shorts.join("."))).ok()
    }
}

/// How many quests deep a quest may sit under others.
const MOST_NESTED: usize = 16;

/// The quest a README's front matter names as its parent, if any.
fn parent(readme: &str) -> Option<String> {
    let front = readme.strip_prefix("---\n")?.split("\n---").next()?;
    let parent = front
        .lines()
        .find_map(|line| line.strip_prefix("parent:"))?
        .trim()
        .trim_matches(['"', '\'']);
    (!parent.is_empty() && parent != "null" && parent != "~").then(|| parent.to_owned())
}

/// What one end says to the other about shared quests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Say {
    /// Every quest this end shares.
    Shares {
        /// Their ids.
        quests: Vec<String>,
    },
    /// What this end holds of some of a quest's files.
    Holds {
        /// The quest's id.
        quest: String,
        /// Each file's state, by its key.
        files: BTreeMap<String, State>,
    },
    /// Asks for a file.
    Want {
        /// The quest's id.
        quest: String,
        /// The file's key.
        file: String,
    },
    /// A file, as it crosses.
    File {
        /// The quest's id.
        quest: String,
        /// The file's key.
        file: String,
        /// Its state on the end that sent it.
        state: State,
        /// What it holds, in base64, or `None` once deleted.
        body: Option<String>,
        /// Who may read, write and run it, as its mode's permission bits;
        /// an end older than this names none, and the file keeps its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<u32>,
    },
}

/// This end's part in the talk.
pub struct Exchange {
    places: Places,
    machine: String,
    other: String,
    /// The quests last said to be shared.
    told: BTreeSet<String>,
    /// What the quests' sessions should hear, not yet said.
    findings: Vec<Draft>,
}

impl Exchange {
    /// `machine`'s end of the talk with `other`.
    #[must_use]
    pub fn new(places: Places, machine: String, other: String) -> Self {
        Self {
            places,
            machine,
            other,
            told: BTreeSet::new(),
            findings: Vec::new(),
        }
    }

    /// What the shared quests' sessions should hear since last asked, as
    /// findings this machine's node sends: from `mycelium@<machine>`, since
    /// it is no session, with any dash in the machine's name made `_`, as a
    /// mesh id parts its sender from the rest by dashes.
    pub fn findings(&mut self) -> Vec<Draft> {
        std::mem::take(&mut self.findings)
    }

    /// Tells the sessions on quest `id` that its file `key` was written on
    /// both machines apart, and where the other's now is.
    fn kept_beside(&mut self, id: &str, key: &str, beside: &str) {
        let text = format!(
            "{key} of {id} was written on {other} and {here} apart. {here} kept its own, \
             and {other}'s is beside it on {here} as {beside}: read both and keep what each says.",
            other = self.other,
            here = self.machine,
        );
        match self.places.group(id) {
            Some(group) => self.findings.push(Draft {
                from: format!("mycelium@{}", self.machine.replace('-', "_")),
                groups: vec![group],
                intent: Some(Intent::Finding),
                text,
                re: None,
            }),
            None => eprintln!("{text} (its sessions' group couldn't be read, so none was told)"),
        }
    }

    /// Whether this end settles files written apart.
    fn settles(&self) -> bool {
        self.machine < self.other
    }

    /// Looks at every shared quest for changes, saying which quests are
    /// shared if that changed, everything held of a quest newly shared,
    /// and what changed of the rest.
    pub fn look(&mut self) -> io::Result<Vec<Say>> {
        let ledgers = self.places.ledgers();
        let quests: BTreeSet<String> = ledgers.quests()?.into_iter().collect();
        let mut says = Vec::new();
        if quests != self.told {
            says.push(Say::Shares {
                quests: quests.iter().cloned().collect(),
            });
        }
        for id in &quests {
            let mut held = ledgers.hold(id)?;
            let changed = held.ledger.scan(&self.places.quest(id), &self.machine)?;
            if !changed.is_empty() {
                held.keep()?;
            }
            let states = held.ledger.states();
            let files: BTreeMap<String, State> = if self.told.contains(id) {
                states
                    .into_iter()
                    .filter(|(key, _)| changed.contains(key))
                    .collect()
            } else {
                states
            };
            if !files.is_empty() || !self.told.contains(id) {
                says.push(Say::Holds {
                    quest: id.clone(),
                    files,
                });
            }
        }
        self.told = quests;
        Ok(says)
    }

    /// Takes what the other end said, returning what this end says back.
    pub fn hear(&mut self, say: Say) -> io::Result<Vec<Say>> {
        match say {
            Say::Shares { quests } => {
                for id in quests {
                    self.share(&id)?;
                }
                Ok(Vec::new())
            }
            Say::Holds { quest, files } => {
                self.share(&quest)?;
                self.held(&quest, files)
            }
            Say::Want { quest, file } => self.want(&quest, &file),
            Say::File {
                quest,
                file,
                state,
                body,
                mode,
            } => self.file(&quest, &file, &state, body.as_deref(), mode),
        }
    }

    /// Starts sharing `id` if this end doesn't yet, moving its own copy
    /// aside first.
    fn share(&self, id: &str) -> io::Result<()> {
        let ledgers = self.places.ledgers();
        if !is_quest(id) || ledgers.shares(id) {
            return Ok(());
        }
        let quest = self.places.quest(id);
        let aside = self
            .places
            .state
            .join("before-share")
            .join(
                jiff::Timestamp::now()
                    .strftime("%Y%m%dT%H%M%SZ")
                    .to_string(),
            )
            .join(id);
        let scratch = quest.scratch.iter().map(|folder| (folder, "scratch"));
        for (folder, name) in std::iter::once((&quest.record, "record")).chain(scratch) {
            if folder.exists() {
                fs::create_dir_all(&aside)?;
                fs::rename(folder, aside.join(name))?;
                eprintln!(
                    "moved this machine's copy of {id}'s {name} aside to {}",
                    aside.display()
                );
            }
        }
        ledgers.hold(id)?.keep()
    }

    /// Asks for each file the other end holds that this end should take.
    fn held(&self, id: &str, files: BTreeMap<String, State>) -> io::Result<Vec<Say>> {
        let quest = self.places.quest(id);
        let mut held = self.places.ledgers().hold(id)?;
        if !held.ledger.scan(&quest, &self.machine)?.is_empty() {
            held.keep()?;
        }
        let mut wants = Vec::new();
        for (key, theirs) in files {
            let Some(shared) = shared_of(&key).filter(|shared| quest.carries(shared)) else {
                continue;
            };
            let ours = held
                .ledger
                .files
                .get(&key)
                .map(Entry::state)
                .unwrap_or_default();
            match settle(&shared, &ours, &theirs) {
                Settled::Keep => {}
                Settled::Merge(_) | Settled::Beside if !self.settles() => {}
                _ if theirs.hash.is_none() => {
                    // A deletion needs nothing sent: it is taken here.
                    quest.write(&shared, None, None)?;
                    held.ledger.files.insert(
                        key,
                        Entry {
                            hash: None,
                            version: ours.version.joined(&theirs.version),
                            seen: None,
                        },
                    );
                    held.keep()?;
                }
                _ => wants.push(Say::Want {
                    quest: id.to_owned(),
                    file: key,
                }),
            }
        }
        Ok(wants)
    }

    /// Sends a file the other end asked for, as this end holds it now.
    fn want(&self, id: &str, key: &str) -> io::Result<Vec<Say>> {
        let quest = self.places.quest(id);
        let Some(shared) = shared_of(key).filter(|shared| quest.carries(shared)) else {
            return Ok(Vec::new());
        };
        if !self.places.ledgers().shares(id) {
            return Ok(Vec::new());
        }
        let mut held = self.places.ledgers().hold(id)?;
        if !held.ledger.scan(&quest, &self.machine)?.is_empty() {
            held.keep()?;
        }
        let Some(entry) = held.ledger.files.get(key) else {
            return Ok(Vec::new());
        };
        let body = quest.read(&shared)?.map(|bytes| STANDARD.encode(bytes));
        Ok(vec![Say::File {
            quest: id.to_owned(),
            file: key.to_owned(),
            state: entry.state(),
            body,
            mode: quest.mode_of(&shared)?,
        }])
    }

    /// Takes a file the other end sent, by how it settles against this
    /// end's as it stands now, saying what this end then holds if that is
    /// something new.
    fn file(
        &mut self,
        id: &str,
        key: &str,
        theirs: &State,
        body: Option<&str>,
        mode: Option<u32>,
    ) -> io::Result<Vec<Say>> {
        let quest = self.places.quest(id);
        let Some(shared) = shared_of(key).filter(|shared| quest.carries(shared)) else {
            return Ok(Vec::new());
        };
        if !self.places.ledgers().shares(id) {
            return Ok(Vec::new());
        }
        let body = body
            .map(|body| STANDARD.decode(body))
            .transpose()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if body.as_deref().map(|body| content(body).hash) != theirs.hash {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{key} of {id} doesn't hold what its hash says"),
            ));
        }
        let mut held = self.places.ledgers().hold(id)?;
        let scanned = !held.ledger.scan(&quest, &self.machine)?.is_empty();
        let ours = held
            .ledger
            .files
            .get(key)
            .map(Entry::state)
            .unwrap_or_default();
        let joined = ours.version.joined(&theirs.version);
        let mut says = Vec::new();
        let entry = match settle(&shared, &ours, theirs) {
            Settled::Keep => None,
            Settled::Merge(_) | Settled::Beside if !self.settles() => None,
            Settled::Take => {
                let written = quest.write(&shared, body.as_deref(), mode)?;
                if is_journey(&shared) {
                    held.ledger.journey = body
                        .as_deref()
                        .map(|body| String::from_utf8_lossy(body).into_owned());
                }
                Some(Entry {
                    hash: theirs.hash.clone(),
                    version: joined,
                    seen: written,
                })
            }
            Settled::Merge(merge) => {
                let ours_text = quest.read(&shared)?.unwrap_or_default();
                let ours_text = String::from_utf8_lossy(&ours_text);
                let theirs_text = String::from_utf8_lossy(body.as_deref().unwrap_or_default());
                let merged = match merge {
                    Merge::Journey => {
                        journey::merged(held.ledger.journey.as_deref(), &ours_text, &theirs_text)
                    }
                    Merge::Log => log::merged(&ours_text, &theirs_text),
                };
                // Both ends' entries are kept, and so is this end's mode.
                let written = quest.write(&shared, Some(merged.as_bytes()), None)?;
                if merge == Merge::Journey {
                    held.ledger.journey = Some(merged.clone());
                }
                Some(Entry {
                    hash: Some(content(merged.as_bytes()).hash),
                    version: joined.changed_on(&self.machine),
                    seen: written,
                })
            }
            Settled::Beside => {
                let theirs_at = Shared {
                    place: shared.place,
                    path: beside(&shared.path, &self.other),
                };
                quest.write(&theirs_at, body.as_deref(), mode)?;
                self.kept_beside(id, key, &theirs_at.path);
                let ours_entry = held.ledger.files.get(key).cloned().unwrap_or_default();
                Some(Entry {
                    version: joined.changed_on(&self.machine),
                    ..ours_entry
                })
            }
        };
        if let Some(entry) = entry {
            let state = entry.state();
            let changed = state != *theirs;
            held.ledger.files.insert(key.to_owned(), entry);
            held.keep()?;
            if changed {
                says.push(Say::Holds {
                    quest: id.to_owned(),
                    files: BTreeMap::from([(key.to_owned(), state)]),
                });
            }
        } else if scanned {
            held.keep()?;
        }
        Ok(says)
    }
}

fn is_journey(shared: &Shared) -> bool {
    shared.place == Place::Record && shared.path == "JOURNEY.md"
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUEST: &str = "QEST-20261013-4UHC5Q";

    struct Machine {
        _dir: tempfile::TempDir,
        places: Places,
        name: &'static str,
    }

    impl Machine {
        fn new(name: &'static str) -> Self {
            let dir = tempfile::tempdir().expect("a temporary folder");
            let places = Places {
                quests: dir.path().join("quests"),
                home: dir.path().join("home"),
                state: dir.path().join("state"),
            };
            Self {
                _dir: dir,
                places,
                name,
            }
        }

        fn write(&self, path: &str, text: &str) {
            let path = self.places.quests.join(QUEST).join(path);
            fs::create_dir_all(path.parent().expect("a file has a folder")).expect("made");
            fs::write(path, text).expect("written");
        }

        fn read(&self, path: &str) -> Option<String> {
            fs::read_to_string(self.places.quests.join(QUEST).join(path)).ok()
        }

        fn exchange(&self, other: &Machine) -> Exchange {
            Exchange::new(
                self.places.clone(),
                self.name.to_owned(),
                other.name.to_owned(),
            )
        }
    }

    /// Passes what each end says to the other until neither has more to
    /// say, after each has looked for changes once.
    fn talk(one: &mut Exchange, two: &mut Exchange) {
        let mut to_two = one.look().expect("one looks");
        let mut to_one = two.look().expect("two looks");
        for _ in 0..20 {
            if to_one.is_empty() && to_two.is_empty() {
                return;
            }
            let mut next_two = Vec::new();
            for say in to_two.drain(..) {
                next_two.extend(two.hear(say).expect("two hears"));
            }
            let mut next_one = Vec::new();
            for say in to_one.drain(..) {
                next_one.extend(one.hear(say).expect("one hears"));
            }
            to_one = next_two;
            to_two = next_one;
        }
        panic!("the two ends never stopped talking");
    }

    #[test]
    fn a_shared_quest_reaches_the_other_end_whose_own_copy_moves_aside() {
        let optimus = Machine::new("optimus");
        let equilibrius = Machine::new("equilibrius");
        optimus.write("README.md", "# Shared\n");
        optimus.write("plans/PLAN-20261026-IZH11J.md", "plan\n");
        equilibrius.write("README.md", "# Stale\n");
        optimus
            .places
            .ledgers()
            .hold(QUEST)
            .expect("held")
            .keep()
            .expect("shared");

        let (mut near, mut far) = (
            optimus.exchange(&equilibrius),
            equilibrius.exchange(&optimus),
        );
        talk(&mut near, &mut far);

        assert_eq!(
            equilibrius.read("README.md").as_deref(),
            Some("# Shared\n"),
            "the README crossed"
        );
        assert_eq!(
            equilibrius.read("plans/PLAN-20261026-IZH11J.md").as_deref(),
            Some("plan\n"),
            "the plan crossed"
        );
        let aside = fs::read_dir(equilibrius.places.state.join("before-share"))
            .expect("something moved aside")
            .next()
            .expect("one move")
            .expect("read")
            .path();
        assert_eq!(
            fs::read_to_string(aside.join(QUEST).join("record").join("README.md")).expect("kept"),
            "# Stale\n",
            "the far end's own copy was kept aside"
        );

        equilibrius.write("README.md", "# Changed there\n");
        talk(&mut near, &mut far);
        assert_eq!(
            optimus.read("README.md").as_deref(),
            Some("# Changed there\n"),
            "a change comes back"
        );
    }

    #[test]
    fn a_journey_written_on_both_ends_keeps_both_entries() {
        let optimus = Machine::new("optimus");
        let equilibrius = Machine::new("equilibrius");
        optimus.write("JOURNEY.md", "# Journey\n\n- **2026-10-26**: began\n");
        optimus
            .places
            .ledgers()
            .hold(QUEST)
            .expect("held")
            .keep()
            .expect("shared");
        let (mut near, mut far) = (
            optimus.exchange(&equilibrius),
            equilibrius.exchange(&optimus),
        );
        talk(&mut near, &mut far);

        optimus.write(
            "JOURNEY.md",
            "# Journey\n\n- **2026-10-27**: here\n- **2026-10-26**: began\n",
        );
        equilibrius.write(
            "JOURNEY.md",
            "# Journey\n\n- **2026-10-27**: there\n- **2026-10-26**: began\n",
        );
        talk(&mut near, &mut far);

        let journey = optimus.read("JOURNEY.md").expect("a journey");
        assert!(
            journey.contains("here") && journey.contains("there") && journey.contains("began"),
            "both entries and the old one are kept: {journey}"
        );
        assert_eq!(
            equilibrius.read("JOURNEY.md"),
            Some(journey),
            "both ends hold the same"
        );
    }

    #[test]
    fn a_document_written_on_both_ends_keeps_the_others_beside_and_says_so() {
        let optimus = Machine::new("optimus");
        let equilibrius = Machine::new("equilibrius");
        let plan = "plans/PLAN-20261026-IZH11J.md";
        optimus.write(plan, "one\n");
        optimus.write(
            "README.md",
            "---\nid: QEST-20261013-4UHC5Q\nparent: QEST-20261007-WCJQSG\n---\n",
        );
        for machine in [&optimus, &equilibrius] {
            let above = machine.places.quests.join("QEST-20261007-WCJQSG");
            fs::create_dir_all(&above).expect("made");
            fs::write(above.join("README.md"), "---\nparent: null\n---\n").expect("written");
        }
        optimus
            .places
            .ledgers()
            .hold(QUEST)
            .expect("held")
            .keep()
            .expect("shared");
        let (mut near, mut far) = (
            optimus.exchange(&equilibrius),
            equilibrius.exchange(&optimus),
        );
        talk(&mut near, &mut far);

        optimus.write(plan, "here\n");
        equilibrius.write(plan, "there\n");
        talk(&mut near, &mut far);

        // equilibrius sorts first, so it settles: its own stands, and
        // optimus's is beside it, on its disk alone.
        assert_eq!(
            equilibrius.read(plan).as_deref(),
            Some("there\n"),
            "the settler's stands"
        );
        assert_eq!(
            optimus.read(plan).as_deref(),
            Some("there\n"),
            "and both ends hold it"
        );
        assert_eq!(
            equilibrius.read(&beside(plan, "optimus")).as_deref(),
            Some("here\n"),
            "the other's is kept beside it"
        );
        assert_eq!(
            optimus.read(&beside(plan, "optimus")),
            None,
            "and never crosses back"
        );
        let told = far.findings();
        assert_eq!(told.len(), 1, "the settler tells of it once: {told:?}");
        assert_eq!(
            (
                told[0].from.as_str(),
                told[0].groups[0].as_str(),
                told[0].intent
            ),
            (
                "mycelium@equilibrius",
                "quest.wcjqsg.4uhc5q",
                Some(Intent::Finding)
            ),
            "as a finding to the quest's whole group"
        );
        assert!(
            told[0].text.contains(&beside(plan, "optimus")),
            "naming where the other's is: {}",
            told[0].text
        );
        assert!(near.findings().is_empty(), "the other end tells nothing");
    }

    #[test]
    fn a_deletion_crosses() {
        let optimus = Machine::new("optimus");
        let equilibrius = Machine::new("equilibrius");
        optimus.write("attachments/map.md", "map\n");
        optimus
            .places
            .ledgers()
            .hold(QUEST)
            .expect("held")
            .keep()
            .expect("shared");
        let (mut near, mut far) = (
            optimus.exchange(&equilibrius),
            equilibrius.exchange(&optimus),
        );
        talk(&mut near, &mut far);
        assert!(
            equilibrius.read("attachments/map.md").is_some(),
            "the attachment crossed"
        );

        fs::remove_file(optimus.places.quests.join(QUEST).join("attachments/map.md"))
            .expect("removed");
        talk(&mut near, &mut far);
        assert_eq!(
            equilibrius.read("attachments/map.md"),
            None,
            "and its removal did"
        );
    }

    #[test]
    fn a_file_crosses_with_who_may_read_it() {
        use std::os::unix::fs::PermissionsExt;
        let mode_of = |machine: &Machine, path: &str| {
            fs::metadata(machine.places.quests.join(QUEST).join(path))
                .expect("there")
                .permissions()
                .mode()
                & 0o777
        };
        let optimus = Machine::new("optimus");
        let equilibrius = Machine::new("equilibrius");
        optimus.write("attachments/hub.env", "TOKEN=1\n");
        fs::set_permissions(
            optimus
                .places
                .quests
                .join(QUEST)
                .join("attachments/hub.env"),
            fs::Permissions::from_mode(0o600),
        )
        .expect("made the owner's alone");
        optimus
            .places
            .ledgers()
            .hold(QUEST)
            .expect("held")
            .keep()
            .expect("shared");
        let (mut near, mut far) = (
            optimus.exchange(&equilibrius),
            equilibrius.exchange(&optimus),
        );
        talk(&mut near, &mut far);
        assert_eq!(
            mode_of(&equilibrius, "attachments/hub.env"),
            0o600,
            "it lands as only its owner may read it"
        );

        optimus.write("attachments/hub.env", "TOKEN=2\n");
        talk(&mut near, &mut far);
        assert_eq!(
            equilibrius.read("attachments/hub.env").as_deref(),
            Some("TOKEN=2\n"),
            "a change crosses"
        );
        assert_eq!(
            mode_of(&equilibrius, "attachments/hub.env"),
            0o600,
            "still as only its owner may read it"
        );
    }
}
