//! How a ledger forgets a deletion once every machine sharing the quest
//! holds it deleted, and what it keeps of one forgotten.
//!
//! Of each file forgotten, only its key and the versions it was last
//! known at are kept ([`Ledger::forgotten`]), so a scratch whose files
//! come and go leaves no ledger of tombstones. That is what tells a stale
//! copy of a file deleted here, offered by a machine this ledger never
//! waited on, from a file written again since: the stale copy is refused
//! and told the deletion, and the file written again starts from its own
//! key's versions, never another file's, so it beats the deletion
//! wherever that is still kept and never beats a write made apart.

use std::collections::BTreeSet;

use super::ledger::{Entry, Ledger};
use super::settle::State;

/// The README's key: the one file a link carrying a quest's README alone
/// holds, so the one deletion such a link is waited on for.
const README: &str = "record/README.md";

impl Ledger {
    /// What this machine holds of file `key`: its entry's state, its
    /// deletion as forgotten where it has none, or a file never held.
    #[must_use]
    pub fn state_of(&self, key: &str) -> State {
        match (self.files.get(key), self.forgotten.get(key)) {
            (Some(entry), _) => entry.state(),
            (None, Some(version)) => State {
                hash: None,
                version: version.clone(),
            },
            (None, None) => State::default(),
        }
    }

    /// The deletion of file `key` this machine forgot, where it holds no
    /// entry of it and that deletion has seen every change of `theirs`:
    /// what the other end holds is as old as the deletion or older, a
    /// copy deleted here or the deletion itself, so nothing of it is
    /// taken.
    #[must_use]
    pub fn forgotten_over(&self, key: &str, theirs: &State) -> Option<State> {
        if self.files.contains_key(key) {
            return None;
        }
        let version = self.forgotten.get(key)?;
        version.holds(&theirs.version).then(|| State {
            hash: None,
            version: version.clone(),
        })
    }

    /// Enters `entry` for file `key`, which carries anything forgotten of
    /// the file from then on.
    pub fn enter(&mut self, key: String, entry: Entry) {
        self.forgotten.remove(&key);
        self.files.insert(key, entry);
    }

    /// Enters that `machine`, the other end, holds file `key` deleted, at
    /// `theirs`, where this machine holds it deleted too or never held
    /// it, as settling leaves both. Answers the state to tell it back
    /// where the two differ, or where this is news of that end, so each
    /// end learns the other holds it deleted: the end that told it hears
    /// it back, at most once, since the same word is news no longer. A
    /// file never held here is entered deleted as there, so the same word
    /// heard again answers nothing, and one whose deletion was forgotten
    /// here at that state or newer answers nothing either: two ends that
    /// hold no entry of it never tell each other of it for good.
    pub fn deleted_there(&mut self, machine: &str, key: &str, theirs: &State) -> Option<State> {
        let news = self
            .theirs
            .get(machine)
            .and_then(|held| held.get(key))
            .is_none_or(|known| known != theirs);
        if let Some(entry) = self.files.get_mut(key) {
            if entry.hash.is_some() {
                return None;
            }
            if entry.version == theirs.version {
                return news.then(|| entry.state());
            }
            entry.version = entry.version.joined(&theirs.version);
            return Some(entry.state());
        }
        if self.forgotten_over(key, theirs).is_some() {
            return None;
        }
        let version = self.forgotten.get(key).map_or_else(
            || theirs.version.clone(),
            |forgot| forgot.joined(&theirs.version),
        );
        let entry = Entry {
            hash: None,
            version,
            seen: None,
        };
        let state = entry.state();
        self.enter(key.to_owned(), entry);
        Some(state)
    }

    /// Names `machine` as linked through a link that carries the quest's
    /// README alone, or not, answering whether that changed. Such a
    /// machine never says it holds another file, so no deletion of one
    /// waits on it.
    pub fn carries_readme_alone(&mut self, machine: &str, alone: bool) -> bool {
        if alone {
            self.readme_alone.insert(machine.to_owned())
        } else {
            self.readme_alone.remove(machine)
        }
    }

    /// Whether this machine holds file `key` deleted and every other
    /// machine sharing the quest is known to hold it deleted at the same
    /// state or newer, so nothing is left to tell of it. A machine that
    /// held the file and hasn't said since that it holds it deleted, as
    /// one away when it was deleted, keeps it; one whose link carries the
    /// README alone keeps only the README's.
    #[must_use]
    pub fn deleted_everywhere(&self, key: &str) -> bool {
        let Some(ours) = self.files.get(key).filter(|entry| entry.hash.is_none()) else {
            return false;
        };
        self.theirs.iter().all(|(machine, held)| {
            (key != README && self.readme_alone.contains(machine))
                || held.get(key).is_some_and(|theirs| {
                    theirs.hash.is_none() && theirs.version.holds(&ours.version)
                })
        })
    }

    /// Forgets each of `keys` deleted everywhere, as
    /// [`Self::deleted_everywhere`] says, answering whether it forgot any.
    pub(super) fn forget_deleted(&mut self, keys: &[String]) -> bool {
        let mut forgot = false;
        for key in keys {
            if self.deleted_everywhere(key) {
                self.forget(key);
                forgot = true;
            }
        }
        forgot
    }

    /// Forgets file `key`, keeping its versions here and theirs, joined,
    /// in [`Self::forgotten`] under its key.
    fn forget(&mut self, key: &str) {
        let mut versions = Vec::new();
        if let Some(entry) = self.files.remove(key) {
            versions.push(entry.version);
        }
        for held in self.theirs.values_mut() {
            if let Some(theirs) = held.remove(key) {
                versions.push(theirs.version);
            }
        }
        if versions.is_empty() {
            return;
        }
        let forgot = self.forgotten.entry(key.to_owned()).or_default();
        for version in versions {
            *forgot = forgot.joined(&version);
        }
    }

    /// Lets go of `machines`, which the realm has let go of for good, as
    /// sharing the quest or reading it, then forgets each file deleted
    /// that every machine left holds deleted. Answers whether any was
    /// named.
    pub fn let_go(&mut self, machines: &BTreeSet<String>) -> bool {
        let named = self.theirs.len() + self.read.len();
        self.theirs.retain(|machine, _| !machines.contains(machine));
        self.read.retain(|machine, _| !machines.contains(machine));
        self.readme_alone
            .retain(|machine| !machines.contains(machine));
        if self.theirs.len() + self.read.len() == named {
            return false;
        }
        let deleted: Vec<String> = self
            .files
            .iter()
            .filter(|(_, entry)| entry.hash.is_none())
            .map(|(key, _)| key.clone())
            .collect();
        self.forget_deleted(&deleted);
        true
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    use super::super::ledger::Ledgers;
    use super::super::tree::Quest;
    use super::super::version::{Standing, Version};
    use super::*;

    const KEY: &str = "record/plans/PLAN-20261115-AAAAAA.md";

    fn quest(dir: &Path) -> Quest {
        Quest::new(dir.join("record"), dir.join("home"))
    }

    fn write(quest: &Quest, path: &str, text: &str) {
        let path = quest.record.join(path);
        fs::create_dir_all(path.parent().expect("a file has a folder")).expect("its folder made");
        fs::write(path, text).expect("the file written");
    }

    /// A deletion as a machine holds it.
    fn deleted(version: &Version) -> State {
        State {
            hash: None,
            version: version.clone(),
        }
    }

    fn said(key: &str, state: State) -> BTreeMap<String, State> {
        BTreeMap::from([(key.to_owned(), state)])
    }

    /// Kestrel holds the plan deleted, told to skiff and tern, which
    /// both held it.
    fn deleted_at_kestrel(dir: &Path) -> (Ledger, Version) {
        let quest = quest(dir);
        let path = KEY.strip_prefix("record/").expect("a record's key");
        write(&quest, path, "# Ours\n");
        let mut ledger = Ledger::default();
        ledger.scan(&quest, "kestrel").expect("scanned");
        let held = ledger.states();
        ledger.heard("skiff", held.clone());
        ledger.heard("tern", held);
        fs::remove_file(quest.record.join(path)).expect("removed");
        ledger.scan(&quest, "kestrel").expect("scanned");
        ledger.bound_theirs();
        let version = ledger.files[KEY].version.clone();
        (ledger, version)
    }

    #[test]
    fn a_deletion_is_forgotten_only_once_every_machine_holds_it_deleted() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut ledger, version) = deleted_at_kestrel(dir.path());
        assert!(
            ledger.heard("skiff", said(KEY, deleted(&version))),
            "skiff holding it deleted is heard"
        );
        assert!(
            ledger.files.contains_key(KEY),
            "kept while tern, which held it, hasn't said it holds it deleted"
        );
        ledger.heard(
            "tern",
            said(KEY, deleted(&Version::default().changed_on("kestrel"))),
        );
        assert!(
            ledger.files.contains_key(KEY),
            "and kept while tern holds it deleted at an older state"
        );
        assert!(
            ledger.heard("tern", said(KEY, deleted(&version.changed_on("tern")))),
            "tern holding it deleted at a newer state is heard"
        );
        assert!(
            !ledger.files.contains_key(KEY),
            "and once every machine holds it deleted, it is forgotten"
        );
        assert!(
            ledger.theirs.values().all(|held| !held.contains_key(KEY)),
            "by every machine's too"
        );
        assert_eq!(
            ledger.forgotten.get(KEY),
            Some(&version.changed_on("tern")),
            "keeping only the versions it was last known at, under its key"
        );
    }

    #[test]
    fn a_file_written_again_after_its_deletion_was_forgotten_is_newer() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut ledger, version) = deleted_at_kestrel(dir.path());
        let gone = said(KEY, deleted(&version));
        ledger.heard("skiff", gone.clone());
        ledger.heard("tern", gone);
        assert!(!ledger.files.contains_key(KEY), "forgotten");
        let quest = quest(dir.path());
        write(&quest, "plans/PLAN-20261115-AAAAAA.md", "# Again\n");
        ledger.scan(&quest, "kestrel").expect("scanned");
        assert_eq!(
            ledger.files[KEY].version.against(&version),
            Standing::Newer,
            "so the write outlasts the deletion wherever it is still kept"
        );
        assert!(
            !ledger.forgotten.contains_key(KEY),
            "and its entry carries what was forgotten of it from then on"
        );
    }

    /// A new file starts from its own key's forgotten versions alone: one
    /// joined across every file forgotten would make it newer than a
    /// write of the same path made apart on another machine, and take
    /// that write's place rather than stand beside it.
    #[test]
    fn a_new_file_is_never_newer_than_a_write_made_apart_elsewhere() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut ledger, version) = deleted_at_kestrel(dir.path());
        let tern_deleted = version.changed_on("tern").changed_on("tern");
        ledger.heard("skiff", said(KEY, deleted(&tern_deleted)));
        ledger.heard("tern", said(KEY, deleted(&tern_deleted)));
        assert!(!ledger.files.contains_key(KEY), "the plan is forgotten");

        let quest = quest(dir.path());
        write(&quest, "plans/PLAN-20261115-BBBBBB.md", "kestrel's plan\n");
        ledger.scan(&quest, "kestrel").expect("scanned");
        let terns = Version::default().changed_on("tern");
        assert_eq!(
            ledger.files["record/plans/PLAN-20261115-BBBBBB.md"]
                .version
                .against(&terns),
            Standing::Apart,
            "kestrel's new plan and tern's, written apart, are kept both"
        );
    }

    /// Hears from `machine` that it holds the plan deleted at `state`, as
    /// an exchange does: settled first, then entered as what it holds.
    fn hear(ledger: &mut Ledger, machine: &str, state: &State) -> Option<State> {
        let answer = ledger.deleted_there(machine, KEY, state);
        ledger.heard(machine, said(KEY, state.clone()));
        answer
    }

    #[test]
    fn a_deletion_held_at_another_state_is_said_back_until_both_hold_one() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut ledger, version) = deleted_at_kestrel(dir.path());
        let theirs = version.changed_on("skiff");
        assert_eq!(
            hear(&mut ledger, "skiff", &deleted(&theirs)),
            Some(deleted(&theirs)),
            "a newer deletion there is joined here, and said back"
        );
        assert_eq!(
            hear(&mut ledger, "skiff", &deleted(&theirs)),
            None,
            "and once both hold one, nothing more is said"
        );
    }

    /// An end that took a deletion and said it back at the state the two
    /// share was never told back in turn, so it waited on the other for
    /// good, though the other held it deleted and forgot it.
    #[test]
    fn a_deletion_heard_at_the_state_held_here_is_said_back_once() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut ledger, version) = deleted_at_kestrel(dir.path());
        assert_eq!(
            hear(&mut ledger, "tern", &deleted(&version)),
            Some(deleted(&version)),
            "tern holding it deleted as kestrel does is news, said back"
        );
        assert_eq!(
            hear(&mut ledger, "tern", &deleted(&version)),
            None,
            "and the same word again is news no longer"
        );
    }

    /// Two ends holding no entry of a file once told each other its
    /// deletion back and forth for good, each answering the other's word.
    #[test]
    fn two_ends_holding_no_entry_of_a_deletion_stop_telling_each_other() {
        let mut kestrel = Ledger::default();
        let mut skiff = Ledger::default();
        let gone = deleted(&Version::default().changed_on("tern"));
        let mut word = hear(&mut kestrel, "skiff", &gone);
        let mut told = 0;
        while let Some(state) = word {
            told += 1;
            assert!(told < 4, "the two ends never stopped telling each other");
            word = if told % 2 == 1 {
                hear(&mut skiff, "kestrel", &state)
            } else {
                hear(&mut kestrel, "skiff", &state)
            };
        }
        assert_eq!(
            kestrel.state_of(KEY),
            gone,
            "a deletion never held here is entered, so it is known heard"
        );

        let mut forgot = Ledger::default();
        forgot
            .forgotten
            .insert(KEY.to_owned(), gone.version.clone());
        assert_eq!(
            forgot.deleted_there("skiff", KEY, &gone),
            None,
            "one forgotten here at that state answers nothing"
        );
        assert!(!forgot.files.contains_key(KEY), "and enters nothing");
        let newer = deleted(&gone.version.changed_on("skiff"));
        assert_eq!(
            forgot.deleted_there("skiff", KEY, &newer),
            Some(newer),
            "while a newer deletion is entered and said back"
        );
    }

    #[test]
    fn a_copy_as_old_as_a_deletion_forgotten_is_refused() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut ledger, version) = deleted_at_kestrel(dir.path());
        let gone = said(KEY, deleted(&version));
        ledger.heard("skiff", gone.clone());
        ledger.heard("tern", gone);
        let stale = State {
            hash: Some("old".to_owned()),
            version: Version::default().changed_on("kestrel"),
        };
        assert_eq!(
            ledger.forgotten_over(KEY, &stale),
            Some(deleted(&version)),
            "a copy older than the deletion forgotten is answered with it"
        );
        assert_eq!(
            ledger.state_of(KEY),
            deleted(&version),
            "and the file is held deleted as forgotten"
        );
        let again = State {
            hash: Some("again".to_owned()),
            version: version.changed_on("wren"),
        };
        assert_eq!(
            ledger.forgotten_over(KEY, &again),
            None,
            "while one written again since is taken"
        );
    }

    #[test]
    fn a_machine_holding_the_readme_alone_is_waited_on_for_it_alone() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut ledger, version) = deleted_at_kestrel(dir.path());
        assert!(
            ledger.carries_readme_alone("tern", true),
            "tern's link carries the README alone"
        );
        ledger.heard("skiff", said(KEY, deleted(&version)));
        assert!(
            !ledger.files.contains_key(KEY),
            "so the plan is forgotten once skiff alone holds it deleted"
        );
        assert!(
            ledger.carries_readme_alone("tern", false),
            "and once its link carries the whole quest"
        );
        assert!(
            !ledger.readme_alone.contains("tern"),
            "it is waited on again"
        );
    }

    #[test]
    fn a_ledger_that_cant_be_read_leaves_the_rest_to_be_let_go() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let ledgers = Ledgers::in_state(dir.path());
        let (torn, kept) = ("QEST-20261013-AAAAAA", "QEST-20261013-BBBBBB");
        let mut held = ledgers.hold(kept).expect("held");
        held.ledger.asked_by("tern");
        held.keep().expect("kept");
        drop(held);
        let shared = dir.path().join("shared");
        fs::write(shared.join(format!("{torn}.json")), "{ torn").expect("written");

        let let_go = ledgers
            .let_go(&BTreeSet::from(["tern".to_owned()]))
            .expect("the quests are listed");
        assert_eq!(let_go.quests, [kept], "the ledger after it lets tern go");
        assert_eq!(
            let_go
                .failed
                .iter()
                .map(|(quest, _)| quest.as_str())
                .collect::<Vec<_>>(),
            [torn],
            "and the one that can't be read is named"
        );
        assert!(
            !ledgers
                .read(kept)
                .expect("read")
                .theirs
                .contains_key("tern"),
            "tern is let go for good"
        );
    }

    #[test]
    fn a_machine_let_go_for_good_is_waited_on_no_longer() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let (mut ledger, version) = deleted_at_kestrel(dir.path());
        ledger.heard("skiff", said(KEY, deleted(&version)));
        ledger.read_by("wren", BTreeMap::new());
        assert!(
            !ledger.let_go(&BTreeSet::from(["osprey".to_owned()])),
            "a machine never named lets nothing go"
        );
        assert!(
            ledger.let_go(&BTreeSet::from(["tern".to_owned(), "wren".to_owned()])),
            "a machine named is let go"
        );
        assert_eq!(
            ledger.theirs.keys().collect::<Vec<_>>(),
            ["skiff"],
            "skiff stays"
        );
        assert!(ledger.read.is_empty(), "and a worker let go reads no more");
        assert!(
            !ledger.files.contains_key(KEY),
            "what only tern lacked is forgotten"
        );
    }
}
