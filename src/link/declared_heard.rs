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
//! says, and so does an end saying its node answered declaring nothing,
//! as one whose declaration was taken away does: only a silence is kept
//! through.

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
    /// Whether what it declared lapsed, or was taken away, since it last
    /// declared anything, so the log says when it declares itself again.
    dropped: bool,
}

impl DeclaredHeard {
    /// What `machine` is taken to declare, heard saying `said` at `now`,
    /// with whether its node answered declaring nothing: what it says,
    /// where it says anything; nothing, where its node answered so; and
    /// otherwise, through a silence, what it last said, while that is no
    /// older than [`KEPT_FOR`].
    pub(super) fn heard(
        &mut self,
        machine: &str,
        (said, answered_nothing): (Option<Declared>, bool),
        now: Instant,
    ) -> Option<Declared> {
        if let Some(declared) = said {
            if self.standing_in || self.dropped {
                eprintln!("{machine} declares itself again");
            }
            self.standing_in = false;
            self.dropped = false;
            self.last = Some((declared.clone(), now));
            return Some(declared);
        }
        let (last, at) = self.last.as_ref()?;
        let since = now.saturating_duration_since(*at);
        if answered_nothing {
            eprintln!(
                "{machine}'s node says it declares nothing of itself, so what it declared {}s \
                 ago is no longer taken for it",
                since.as_secs()
            );
            return self.drop_last();
        }
        if since > KEPT_FOR {
            eprintln!(
                "{machine} has declared nothing of itself for {}s, so what it declared before \
                 is no longer taken for it",
                since.as_secs()
            );
            return self.drop_last();
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

    /// Whether what the machine last declared stands in for nothing now.
    pub(super) fn standing_in(&self) -> bool {
        self.standing_in
    }

    /// Forgets what the machine declared, which is taken for it no longer.
    fn drop_last(&mut self) -> Option<Declared> {
        self.last = None;
        self.standing_in = false;
        self.dropped = true;
        None
    }
}

/// Whether a machine taken to declare `declared` is withheld the quests
/// above those shared with it: a worker is, and so is one declaring
/// nothing, and one whose last declaration is `standing_in` for nothing,
/// since what it declares now is unknown, though what it stands on for a
/// carry is kept.
pub(super) fn withheld_above(declared: Option<&Declared>, standing_in: bool) -> bool {
    standing_in || declared.is_none_or(Declared::is_worker)
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

    /// Nothing declared, through a silence.
    const SILENT: (Option<Declared>, bool) = (None, false);

    #[test]
    fn a_machine_declaring_nothing_for_a_while_keeps_what_it_declared() {
        let mut declaring = DeclaredHeard::default();
        let began = Instant::now();
        declaring.heard("tern", (Some(fenced()), false), began);

        let restarting = declaring.heard("tern", SILENT, began + Duration::from_secs(30));

        assert_eq!(restarting, Some(fenced()));
    }

    #[test]
    fn what_a_machine_declared_stands_up_to_its_bound_and_not_past_it() {
        let mut declaring = DeclaredHeard::default();
        let began = Instant::now();
        declaring.heard("tern", (Some(fenced()), false), began);

        let at_the_bound = declaring.heard("tern", SILENT, began + KEPT_FOR);
        let past_it = declaring.heard("tern", SILENT, began + KEPT_FOR + Duration::from_secs(1));

        assert_eq!((at_the_bound, past_it), (Some(fenced()), None));
    }

    #[test]
    fn a_silence_counts_from_the_last_declaration_not_the_last_silence() {
        let mut declaring = DeclaredHeard::default();
        let began = Instant::now();
        declaring.heard("tern", (Some(fenced()), false), began);
        declaring.heard("tern", SILENT, began + Duration::from_secs(5 * 60));
        declaring.heard("tern", SILENT, began + Duration::from_secs(9 * 60));

        let gone = declaring.heard("tern", SILENT, began + KEPT_FOR + Duration::from_secs(1));
        let after = declaring.heard("tern", SILENT, began + KEPT_FOR + Duration::from_secs(2));

        assert_eq!((gone, after), (None, None));
    }

    #[test]
    fn a_machine_that_declares_again_is_taken_at_its_word() {
        let mut declaring = DeclaredHeard::default();
        let began = Instant::now();
        declaring.heard("tern", (Some(fenced()), false), began);
        declaring.heard("tern", SILENT, began + Duration::from_secs(30));

        let unfenced = declaring.heard(
            "tern",
            (Some(Declared::default()), false),
            began + Duration::from_secs(60),
        );
        let then_silent = declaring.heard("tern", SILENT, began + Duration::from_secs(90));

        assert_eq!(
            (unfenced, then_silent),
            (Some(Declared::default()), Some(Declared::default())),
            "what it declares now replaces what it declared, kept through a silence after"
        );
    }

    #[test]
    fn a_node_answering_that_its_machine_declares_nothing_is_taken_at_its_word() {
        let mut declaring = DeclaredHeard::default();
        let began = Instant::now();
        declaring.heard("tern", (Some(fenced()), false), began);

        let withdrawn = declaring.heard("tern", (None, true), began + Duration::from_secs(30));
        let then_silent = declaring.heard("tern", SILENT, began + Duration::from_secs(40));

        assert_eq!((withdrawn, then_silent), (None, None));
    }

    #[test]
    fn a_machine_never_heard_declaring_declares_nothing() {
        let mut declaring = DeclaredHeard::default();

        assert_eq!(declaring.heard("tern", SILENT, Instant::now()), None);
    }

    #[test]
    fn quests_above_stay_withheld_while_a_declaration_stands_in() {
        let mut declaring = DeclaredHeard::default();
        let began = Instant::now();
        let declared = declaring.heard("tern", (Some(fenced()), false), began);
        assert!(
            !withheld_above(declared.as_ref(), declaring.standing_in()),
            "a peer declaring itself is shared quests above"
        );

        let kept = declaring.heard("tern", SILENT, began + Duration::from_secs(30));

        assert!(
            kept.is_some() && withheld_above(kept.as_ref(), declaring.standing_in()),
            "kept for a carry, it is withheld them while it says nothing"
        );
    }
}
