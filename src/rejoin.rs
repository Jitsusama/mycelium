//! A session on another machine joined from a terminal here, and joined
//! again each time the way there drops: `mycelium attach --on MACHINE`,
//! the words a node serves for a session elsewhere that it reaches over
//! SSH. One dropped link closed 17 windows on 2026-11-16, each on a
//! session still running on the other machine.

use std::io;
use std::time::Duration;

use crate::record::Session;

/// The code SSH ends with when it couldn't connect or its connection
/// dropped, rather than one the command there ended with.
pub const DROPPED: i32 = 255;

/// The code a join ends with when the person stops it with Ctrl+C while
/// it waits to join again: 128 and SIGINT's number, as a shell says it.
pub const INTERRUPTED: u8 = 130;

/// The words that join, from a terminal here, the session that `joins`
/// joins on the machine `on`, given `there`, the words that run `joins`
/// there.
#[must_use]
pub fn served(on: &str, joins: &[String], there: Vec<String>) -> Vec<String> {
    let _ = (on, joins);
    there
}

/// How long a join waits before each try again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Waits {
    /// The first wait, and the wait again after a try that held.
    pub first: Duration,
    /// The longest, however many tries in a row dropped.
    pub most: Duration,
    /// How long a try must have run to count as joined, so the drop that
    /// ends it waits only `first` again.
    pub held: Duration,
}

impl Default for Waits {
    fn default() -> Self {
        Self {
            first: Duration::from_secs(1),
            most: Duration::from_secs(30),
            held: Duration::from_secs(10),
        }
    }
}

/// Whether a session joined still runs, as this machine's node knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Still {
    /// It runs, or its machine is out of reach, so it may.
    Runs,
    /// It has ended, said by its title or else its id.
    Ended(String),
    /// The node doesn't say: it lists no such session, or didn't answer.
    Unknown,
}

/// Which session a join joins, as the node lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// One a holder keeps, by its held id.
    Held(String),
    /// A conversation within a pi host, by the host's key and the
    /// conversation's id.
    Hosted {
        /// The host's key.
        host: String,
        /// The conversation's id.
        conversation: String,
    },
}

/// Whether the session `target` names on `on` still runs, as `sessions`,
/// the node's, say.
#[must_use]
pub fn still(sessions: &[Session], on: &str, target: &Target) -> Still {
    let _ = (sessions, on, target);
    Still::Unknown
}

/// A session joined over a way that drops.
pub trait Joining {
    /// Runs the words that join it once, answering the code they ended
    /// with.
    fn join(&mut self) -> impl Future<Output = io::Result<i32>>;

    /// Whether it still runs there, as this machine's node knows it.
    fn still(&mut self) -> impl Future<Output = Still>;

    /// Says `line` to the person.
    fn say(&mut self, line: &str);
}

/// Joins over `joining` until a try ends otherwise than as a dropped way
/// does, answering the code it ended with.
pub async fn rejoin(
    on: &str,
    joining: &mut impl Joining,
    waits: Waits,
    interrupted: impl Future<Output = ()>,
) -> io::Result<u8> {
    let _ = (on, waits, interrupted);
    Ok(u8::try_from(joining.join().await?).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use tokio::time::Instant;

    use super::*;

    /// A join whose tries end as scripted, each with its code after
    /// running as long as it says, and whose node says the session ended
    /// at the ask numbered `ends_at`, if any.
    #[derive(Default)]
    struct Scripted {
        tries: VecDeque<(i32, Duration)>,
        tried: Vec<Instant>,
        said: Vec<String>,
        asked: usize,
        ends_at: Option<usize>,
    }

    impl Scripted {
        fn ending(tries: &[(i32, u64)]) -> Self {
            Self {
                tries: tries
                    .iter()
                    .map(|&(code, lasts)| (code, Duration::from_secs(lasts)))
                    .collect(),
                ..Self::default()
            }
        }

        /// The seconds between one try and the next.
        fn gaps(&self) -> Vec<u64> {
            self.tried
                .windows(2)
                .map(|two| (two[1] - two[0]).as_secs())
                .collect()
        }
    }

    impl Joining for Scripted {
        async fn join(&mut self) -> io::Result<i32> {
            self.tried.push(Instant::now());
            let (code, lasts) = self.tries.pop_front().expect("a try scripted");
            tokio::time::sleep(lasts).await;
            Ok(code)
        }

        async fn still(&mut self) -> Still {
            self.asked += 1;
            if Some(self.asked) == self.ends_at {
                Still::Ended("Harbour work".to_owned())
            } else {
                Still::Runs
            }
        }

        fn say(&mut self, line: &str) {
            self.said.push(line.to_owned());
        }
    }

    fn never() -> std::future::Pending<()> {
        std::future::pending()
    }

    #[tokio::test(start_paused = true)]
    async fn waits_double_from_a_second_to_at_most_thirty() {
        let mut joining = Scripted::ending(&[
            (DROPPED, 0),
            (DROPPED, 0),
            (DROPPED, 0),
            (DROPPED, 0),
            (DROPPED, 0),
            (DROPPED, 0),
            (DROPPED, 0),
            (0, 0),
        ]);

        let ended = rejoin("skiff", &mut joining, Waits::default(), never()).await;

        assert_eq!(ended.expect("it joins"), 0, "the last try ended well");
        assert_eq!(
            joining.gaps(),
            [1, 2, 4, 8, 16, 30, 30],
            "each wait doubles, up to 30 seconds"
        );
        assert_eq!(
            joining.said, ["Reconnecting to skiff."; 7],
            "saying so once for each try again"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_try_that_held_waits_only_a_second_again() {
        let mut joining = Scripted::ending(&[
            (DROPPED, 0),
            (DROPPED, 0),
            (DROPPED, 15),
            (DROPPED, 0),
            (0, 0),
        ]);

        rejoin("skiff", &mut joining, Waits::default(), never())
            .await
            .expect("it joins");

        assert_eq!(
            joining.gaps(),
            [1, 2, 15 + 1, 2],
            "a join that held 15 seconds waits a second once it drops, then doubles again"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_code_other_than_a_drop_is_passed_on() {
        let mut joining = Scripted::ending(&[(DROPPED, 0), (3, 0)]);

        let ended = rejoin("skiff", &mut joining, Waits::default(), never()).await;

        assert_eq!(ended.expect("it joins"), 3, "as the join there ended");
        assert_eq!(joining.tried.len(), 2, "tried once more after the drop");
    }

    #[tokio::test(start_paused = true)]
    async fn a_session_ended_there_ends_the_join_well() {
        let mut joining = Scripted {
            ends_at: Some(1),
            ..Scripted::ending(&[(DROPPED, 0)])
        };

        let ended = rejoin("skiff", &mut joining, Waits::default(), never()).await;

        assert_eq!(ended.expect("it ends"), 0, "an ended session ends it well");
        assert_eq!(joining.said, ["Harbour work has ended."], "saying which");
        assert_eq!(joining.tried.len(), 1, "never trying again");
    }

    #[tokio::test(start_paused = true)]
    async fn ctrl_c_while_waiting_ends_the_join() {
        let mut joining = Scripted::ending(&[(DROPPED, 0), (0, 0)]);
        let pressed = tokio::time::sleep(Duration::from_millis(500));

        let ended = rejoin("skiff", &mut joining, Waits::default(), pressed).await;

        assert_eq!(
            ended.expect("it ends"),
            INTERRUPTED,
            "as a shell says Ctrl+C"
        );
        assert_eq!(joining.tried.len(), 1, "never trying again");
    }

    fn words(words: &[&str]) -> Vec<String> {
        words.iter().map(|&word| word.to_owned()).collect()
    }

    #[test]
    fn a_join_elsewhere_is_served_as_one_here_carrying_the_words_there() {
        let joins = words(&["mycelium", "--realm", "work", "attach", "5e55"]);
        let there = words(&["ssh", "-t", "--", "skiff", "mycelium", "attach", "5e55"]);
        assert_eq!(
            served("skiff", &joins, there.clone()),
            words(&[
                "mycelium", "--realm", "work", "attach", "--on", "skiff", "5e55", "--", "ssh",
                "-t", "--", "skiff", "mycelium", "attach", "5e55"
            ]),
            "the realm kept, the machine named, and the words there carried whole"
        );
        let hosted = words(&[
            "mycelium",
            "--realm",
            "work",
            "attach",
            "--host",
            "h",
            "--session",
            "c",
        ]);
        assert_eq!(
            served("skiff", &hosted, there.clone())[..9],
            words(&[
                "mycelium",
                "--realm",
                "work",
                "attach",
                "--on",
                "skiff",
                "--host",
                "h",
                "--session"
            ]),
            "a hosted conversation is named as it is there"
        );
        assert_eq!(
            served("skiff", &words(&["mycelium", "link"]), there.clone()),
            there,
            "words that join nothing are run there as they are"
        );
    }

    fn session(said: &serde_json::Value) -> Session {
        let mut session = serde_json::json!({
            "id": "0e50",
            "address": "HARBOR",
            "machine": "skiff",
            "groups": [],
            "state": "idle",
            "held": "5e55",
            "at": "2026-11-16T10:00:00Z",
        });
        for (key, value) in said.as_object().expect("fields") {
            session[key] = value.clone();
        }
        serde_json::from_value(session).expect("a session")
    }

    #[test]
    fn a_session_has_ended_only_when_the_node_lists_it_ended_and_unparked() {
        let held = Target::Held("5e55".to_owned());
        let ended = serde_json::json!({"state": "ended", "title": "Harbour work"});
        assert_eq!(
            still(&[session(&ended)], "skiff", &held),
            Still::Ended("Harbour work".to_owned()),
            "an ended session is named by its title"
        );
        assert_eq!(
            still(
                &[session(&serde_json::json!({"state": "ended"}))],
                "skiff",
                &held
            ),
            Still::Ended("5e55".to_owned()),
            "or else by its id"
        );
        let parked = serde_json::json!({"state": "ended", "presence": {"state": "parked"}});
        assert_eq!(
            still(&[session(&parked)], "skiff", &held),
            Still::Runs,
            "a parked session starts again as it is joined"
        );
        let unreachable = serde_json::json!({"state": "unreachable"});
        assert_eq!(
            still(&[session(&unreachable)], "skiff", &held),
            Still::Runs,
            "a session out of reach may run"
        );
        assert_eq!(
            still(&[session(&ended)], "tern", &held),
            Still::Unknown,
            "a session of another machine isn't this one"
        );
        assert_eq!(still(&[], "skiff", &held), Still::Unknown, "nor is none");
        let hosted = serde_json::json!({
            "state": "ended", "held": null, "host": "h", "conversation": "c"
        });
        assert_eq!(
            still(
                &[session(&hosted)],
                "skiff",
                &Target::Hosted {
                    host: "h".to_owned(),
                    conversation: "c".to_owned()
                }
            ),
            Still::Ended("c".to_owned()),
            "a hosted conversation is found by its host and id"
        );
    }
}
