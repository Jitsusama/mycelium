//! The steers a parked program's holder watches for: the person's words
//! and asks, left in a session's steer inboxes, that the session would
//! answer at once were it running.
//!
//! A parked pi used to wake for a talk only once some running pi looked
//! at its inbox, once a minute, and wrote its wake file (14 November). A
//! parking program says in `<id>.steers` which folders its steers land
//! in and which steers wake it ([`Steers`]); its holder watches those
//! folders while it is parked and wakes it as one lands. The rule is the
//! mesh's, said as data, so the holder never keeps a copy of its own.
//!
//! Each inbox is watched alone, not recursively, and only while the
//! program is parked, so a hundred parked sessions cost a few hundred
//! watches at most. An inbox not there yet, or removed and made again,
//! is watched once the holder's next look finds it there.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use tokio::sync::mpsc;

/// What a parking program says of its steers: the folders they land in,
/// and which of them wake it.
#[derive(Debug, Deserialize)]
pub struct Steers {
    inboxes: Vec<PathBuf>,
    wakes: Wakes,
}

/// Which steers wake a parked program: one whose act it names, a resume
/// with words when `resume_with_words`, and words with no act when `talk`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wakes {
    #[serde(default)]
    acts: Vec<String>,
    #[serde(default)]
    talk: bool,
    #[serde(default)]
    resume_with_words: bool,
}

impl Steers {
    /// What the program wrote in `file` as it parked, if it wrote one this
    /// holder can read. Without one it wakes as it always has: at a key,
    /// or as its wake file appears.
    #[must_use]
    pub fn read(file: &Path) -> Option<Self> {
        serde_json::from_slice(&fs::read(file).ok()?).ok()
    }

    /// Whether a steer that wakes the program waits in any of its inboxes.
    #[must_use]
    pub fn waiting(&self) -> bool {
        self.inboxes.iter().any(|inbox| self.waits_in(inbox))
    }

    /// Whether a steer that wakes the program waits in `inbox`.
    fn waits_in(&self, inbox: &Path) -> bool {
        let Ok(entries) = fs::read_dir(inbox) else {
            return false;
        };
        entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| is_steer(path))
            .any(|path| fs::read(&path).is_ok_and(|body| self.wakes.wake(&body)))
    }
}

/// Whether `path` is a steer: a JSON file, not one still being written
/// under a name that starts with a dot.
fn is_steer(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "json")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| !name.starts_with('.'))
}

impl Wakes {
    /// Whether the steer `body` wakes the program. One that isn't a JSON
    /// object, as the mesh sets aside, doesn't.
    fn wake(&self, body: &[u8]) -> bool {
        let Ok(serde_json::Value::Object(steer)) = serde_json::from_slice(body) else {
            return false;
        };
        let text = steer
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map_or("", str::trim);
        match steer.get("act").and_then(serde_json::Value::as_str) {
            None => self.talk && !text.is_empty(),
            Some("resume") if !text.is_empty() => self.resume_with_words,
            Some(act) => self.acts.iter().any(|one| one == act),
        }
    }
}

/// What a watch of a parked program's inboxes heard.
pub type Heard = notify::Result<notify::Event>;

/// A watch of a parked program's inboxes, which lasts as long as it is
/// kept.
pub struct Watching<'a> {
    steers: &'a Steers,
    watcher: RecommendedWatcher,
    /// The inboxes watched now.
    armed: HashSet<PathBuf>,
}

impl<'a> Watching<'a> {
    /// A watch of `steers`' inboxes, and what it hears, watching none yet
    /// ([`Self::arm`]); none when the system can't make one, which leaves
    /// the program to wake as it always has.
    #[must_use]
    pub fn of(steers: &'a Steers) -> Option<(Self, mpsc::UnboundedReceiver<Heard>)> {
        let (tell, heard) = mpsc::unbounded_channel();
        let watcher = crate::watching::watcher(move |event: Heard| {
            // The holder has stopped listening once the program woke.
            let _ = tell.send(event);
        })
        .ok()?;
        let watching = Self {
            steers,
            watcher,
            armed: HashSet::new(),
        };
        Some((watching, heard))
    }

    /// Watches each inbox that is there and not watched yet, answering
    /// whether a steer that wakes the program already waits in one of
    /// them: one that landed before its watch began is heard by nothing.
    pub fn arm(&mut self) -> bool {
        let mut waiting = false;
        for inbox in &self.steers.inboxes {
            if self.armed.contains(inbox) {
                continue;
            }
            if self
                .watcher
                .watch(inbox, RecursiveMode::NonRecursive)
                .is_ok()
            {
                self.armed.insert(inbox.clone());
                waiting = waiting || self.steers.waits_in(inbox);
            }
        }
        waiting
    }

    /// Takes in what the watch heard, answering whether a steer that wakes
    /// the program now waits in the inbox it was heard in. An inbox removed
    /// is watched no more, until [`Self::arm`] finds it made again. A watch
    /// that failed, or heard no path, looks in every inbox.
    pub fn heard(&mut self, heard: &Heard) -> bool {
        let Ok(event) = heard else {
            return self.steers.waiting();
        };
        if event.paths.is_empty() {
            return self.steers.waiting();
        }
        if matches!(event.kind, EventKind::Remove(_)) {
            for path in &event.paths {
                if self.armed.remove(path) {
                    let _ = self.watcher.unwatch(path);
                }
            }
        }
        event
            .paths
            .iter()
            .filter_map(|path| self.inbox_of(path))
            .any(|inbox| self.steers.waits_in(inbox))
    }

    /// The watched inbox `path` is, or lies in.
    fn inbox_of<'p>(&self, path: &'p Path) -> Option<&'p Path> {
        if self.armed.contains(path) {
            return Some(path);
        }
        path.parent().filter(|inbox| self.armed.contains(*inbox))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// How long a test waits for what the watch owes it.
    const PATIENCE: Duration = Duration::from_secs(20);

    /// The mesh's rule, as a parking pi writes it.
    const RULE: &str =
        r#"{"acts":["close","carry","move","fill"],"talk":true,"resumeWithWords":true}"#;

    fn steers(inboxes: &[&Path]) -> Steers {
        let inboxes: Vec<String> = inboxes
            .iter()
            .map(|inbox| serde_json::to_string(inbox).expect("a path in JSON"))
            .collect();
        serde_json::from_str(&format!(
            r#"{{"inboxes":[{}],"wakes":{RULE}}}"#,
            inboxes.join(",")
        ))
        .expect("what a parking pi writes reads")
    }

    /// Lands `body` in `inbox` as the mesh writes a steer: under another
    /// name first, then renamed whole.
    fn lands(inbox: &Path, name: &str, body: &str) {
        let writing = inbox.join(format!(".{name}"));
        fs::write(&writing, body).expect("the steer is written");
        fs::rename(&writing, inbox.join(name)).expect("the steer lands");
    }

    #[test]
    fn a_steer_wakes_by_the_rule_the_program_gave() {
        let rule: Wakes = serde_json::from_str(RULE).expect("the rule reads");
        for (steer, wakes) in [
            (r#"{"text":"put the void's card on top"}"#, true),
            (r#"{"act":"resume","text":"carry on"}"#, true),
            (r#"{"act":"resume"}"#, false),
            (r#"{"act":"pause"}"#, false),
            (r#"{"act":"stop"}"#, false),
            (r#"{"act":"close"}"#, true),
            (r#"{"act":"carry","to":"penelope"}"#, true),
            (r#"{"act":"fill","text":"Ready for this."}"#, true),
            (r#"{"text":"   "}"#, false),
            ("not a steer", false),
        ] {
            assert_eq!(rule.wake(steer.as_bytes()), wakes, "{steer}");
        }
    }

    #[test]
    fn a_file_missing_or_unreadable_says_no_steers() {
        let dir = tempfile::tempdir().expect("a folder");
        let file = dir.path().join("trial1.steers");
        assert!(Steers::read(&file).is_none(), "none written");
        fs::write(&file, "{\"inboxes\":").expect("half a file");
        assert!(Steers::read(&file).is_none(), "half written");
    }

    #[test]
    fn only_a_steer_that_wakes_waits_and_never_one_being_written() {
        let dir = tempfile::tempdir().expect("a folder");
        let steers = steers(&[dir.path()]);
        lands(dir.path(), "1-a.json", r#"{"act":"pause"}"#);
        fs::write(dir.path().join(".2-b.json"), r#"{"text":"half"}"#).expect("being written");
        assert!(!steers.waiting(), "a pause, and one being written");

        lands(dir.path(), "3-c.json", r#"{"text":"are you there?"}"#);
        assert!(steers.waiting(), "a talk");
    }

    #[test]
    fn arming_sees_a_steer_that_landed_before_the_watch_began() {
        let dir = tempfile::tempdir().expect("a folder");
        lands(dir.path(), "1-a.json", r#"{"act":"close"}"#);
        let steers = steers(&[dir.path()]);
        let (mut watching, _heard) = Watching::of(&steers).expect("a watch");
        assert!(watching.arm(), "the close already there wakes it");
        assert!(
            !watching.arm(),
            "an inbox already watched isn't looked at again"
        );
    }

    #[tokio::test]
    async fn a_steer_landing_in_an_inbox_made_later_is_heard_once_it_is_watched() {
        let root = tempfile::tempdir().expect("a folder");
        let inbox = root.path().join("ZJ62K2");
        let steers = steers(&[&inbox]);
        let (mut watching, mut heard) = Watching::of(&steers).expect("a watch");
        assert!(!watching.arm(), "nothing to watch yet");

        fs::create_dir(&inbox).expect("the inbox is made");
        assert!(!watching.arm(), "watched now, and empty");
        lands(&inbox, "1-a.json", r#"{"text":"are you there?"}"#);
        let woke = tokio::time::timeout(PATIENCE, async {
            while let Some(event) = heard.recv().await {
                if watching.heard(&event) {
                    return true;
                }
            }
            false
        })
        .await;
        assert!(matches!(woke, Ok(true)), "the talk is heard");
    }

    #[test]
    fn what_is_heard_in_one_inbox_is_judged_by_that_inbox_alone() {
        let root = tempfile::tempdir().expect("a folder");
        let (one, other) = (root.path().join("ZJ62K2"), root.path().join("QUEST1"));
        fs::create_dir(&one).expect("one inbox");
        fs::create_dir(&other).expect("another");
        let steers = steers(&[&one, &other]);
        let (mut watching, _heard) = Watching::of(&steers).expect("a watch");
        assert!(!watching.arm(), "both watched, and empty");
        lands(&one, "1-a.json", r#"{"act":"pause"}"#);
        lands(&other, "2-b.json", r#"{"text":"are you there?"}"#);
        let landed = |inbox: &Path, name: &str| -> Heard {
            Ok(
                notify::Event::new(EventKind::Create(notify::event::CreateKind::File))
                    .add_path(inbox.join(name)),
            )
        };

        assert!(
            !watching.heard(&landed(&one, "1-a.json")),
            "a pause heard in one inbox wakes nothing"
        );
        assert!(
            watching.heard(&landed(&other, "2-b.json")),
            "the talk wakes it as it is heard in its own"
        );
    }

    #[tokio::test]
    async fn an_inbox_removed_and_made_again_is_watched_again() {
        let root = tempfile::tempdir().expect("a folder");
        let inbox = root.path().join("ZJ62K2");
        fs::create_dir(&inbox).expect("the inbox is made");
        let steers = steers(&[&inbox]);
        let (mut watching, mut heard) = Watching::of(&steers).expect("a watch");
        assert!(!watching.arm(), "watched, and empty");

        fs::remove_dir(&inbox).expect("the mesh sweeps it");
        let forgot = tokio::time::timeout(PATIENCE, async {
            while let Some(event) = heard.recv().await {
                watching.heard(&event);
                if !watching.armed.contains(&inbox) {
                    return;
                }
            }
        })
        .await;
        assert!(forgot.is_ok(), "its removal is heard");
        fs::create_dir(&inbox).expect("a sender makes it again");
        lands(&inbox, "1-a.json", r#"{"act":"carry","to":"penelope"}"#);
        assert!(watching.arm(), "watched again, its carry is seen");
    }
}
