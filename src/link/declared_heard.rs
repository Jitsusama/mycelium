//! What the other end last declared of its machine, kept through a short
//! silence about it.
//!
//! A link another machine dialled runs apart from that machine's node and
//! asks the node what it measures the machine to be; a node restarting,
//! or too busy to answer, isn't asked, so the end says its machine with
//! nothing declared. Taken as it is said, that machine would read for a
//! while as declaring no fence, and a carry ordered then would be refused
//! for ground the machine still stands on. So what it last declared is
//! kept, for [`KEPT_FOR`] at most, while it declares nothing, and dropped
//! after, since a machine that truly stops declaring itself must come to
//! read so. A declaration said again takes its place at once, whatever it
//! says.

use std::time::Duration;

use tokio::time::Instant;

use crate::declared::Declared;

/// How long what the other end last declared stands while it declares
/// nothing: a node's restart, or a few slow answers, and not much more.
pub(super) const KEPT_FOR: Duration = Duration::from_secs(10 * 60);

/// What the other end last declared, and when, as one link heard it.
#[derive(Debug, Default)]
pub(super) struct DeclaredHeard {
    last: Option<(Declared, Instant)>,
    /// Whether what was last declared stands in for nothing now, so the
    /// log says so once each time it starts and once as it ends.
    standing_in: bool,
}

impl DeclaredHeard {
    /// What `machine` is taken to declare, heard saying `said` at `now`:
    /// what it says, where it says anything, and otherwise what it last
    /// said, while that is no older than [`KEPT_FOR`].
    pub(super) fn heard(
        &mut self,
        machine: &str,
        said: Option<Declared>,
        now: Instant,
    ) -> Option<Declared> {
        if let Some(declared) = said {
            if self.standing_in {
                eprintln!("{machine} declares itself again");
                self.standing_in = false;
            }
            self.last = Some((declared.clone(), now));
            return Some(declared);
        }
        let (last, at) = self.last.as_ref()?;
        let since = now.saturating_duration_since(*at);
        if since > KEPT_FOR {
            eprintln!(
                "{machine} has declared nothing of itself for {}s, so what it declared before \
                 is no longer taken for it",
                since.as_secs()
            );
            self.last = None;
            self.standing_in = false;
            return None;
        }
        if !self.standing_in {
            eprintln!(
                "{machine} declares nothing of itself, as its node does while it restarts or \
                 can't answer, so what it declared {}s ago stands for up to {}s",
                since.as_secs(),
                KEPT_FOR.as_secs()
            );
            self.standing_in = true;
        }
        Some(last.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// A machine declaring a fence, though none of its kinds: enough to
    /// tell from one declaring nothing.
    fn fenced() -> Declared {
        Declared {
            fence: Some(std::collections::BTreeMap::new()),
            ..Declared::default()
        }
    }

    #[test]
    fn a_machine_declaring_nothing_for_a_while_keeps_what_it_declared() {
        let mut declaring = DeclaredHeard::default();
        let began = Instant::now();
        declaring.heard("tern", Some(fenced()), began);

        let restarting = declaring.heard("tern", None, began + Duration::from_secs(30));

        assert_eq!(restarting, Some(fenced()));
    }

    #[test]
    fn what_a_machine_declared_lapses_once_it_declares_nothing_for_long() {
        let mut declaring = DeclaredHeard::default();
        let began = Instant::now();
        declaring.heard("tern", Some(fenced()), began);

        let gone = declaring.heard("tern", None, began + KEPT_FOR + Duration::from_secs(1));
        let after = declaring.heard("tern", None, began + KEPT_FOR + Duration::from_secs(2));

        assert_eq!((gone, after), (None, None));
    }

    #[test]
    fn a_machine_that_declares_again_is_taken_at_its_word() {
        let mut declaring = DeclaredHeard::default();
        let began = Instant::now();
        declaring.heard("tern", Some(fenced()), began);
        declaring.heard("tern", None, began + Duration::from_secs(30));

        let unfenced = declaring.heard(
            "tern",
            Some(Declared::default()),
            began + Duration::from_secs(60),
        );

        assert_eq!(unfenced, Some(Declared::default()));
    }

    #[test]
    fn a_machine_never_heard_declaring_declares_nothing() {
        let mut declaring = DeclaredHeard::default();

        assert_eq!(declaring.heard("tern", None, Instant::now()), None);
    }
}
