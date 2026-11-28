//! How a newer link with a machine takes over from the link holding its
//! claim here: a newer link from the same machine means the older one's
//! other end is gone though it hasn't heard so.
//!
//! The newer link asks for the claim ([`Store::supersede`]), and the older
//! one lets it go once it sees an ask newer than the one that stood when
//! it took it. Asks are told apart by the store's own marks, never by
//! when the claim was taken: an ask made in the very millisecond the
//! claim was taken was once never seen, so both links went on, the older
//! keeping the claim and the newer refused.

use std::io;
use std::time::Duration;

use super::{LOOK_EVERY, refusal};
use crate::store::{Claim, Store};

/// How long a newer link waits for the link holding its machine's claim
/// to let it go: that one looks each [`LOOK_EVERY`], and lets go what it
/// carried in under a second.
const TAKEN_OVER_WITHIN: Duration = Duration::from_secs(10);

/// This link's hold on `machine`, with the newest ask for it as it stood
/// just before the link took it, for [`superseded`]. Where another link
/// holds it, that one is asked to let it go: a node restarted on 25
/// November had its new links to a cloud machine refused there while the
/// old one's end went on for minutes. Refused when it isn't let go in
/// time.
pub(super) async fn claimed(store: &impl Store, machine: &str) -> io::Result<(Claim, Option<i64>)> {
    if let Some(taken) = taken(store, machine)? {
        return Ok(taken);
    }
    store.supersede(machine)?;
    let waited = tokio::time::timeout(TAKEN_OVER_WITHIN, async {
        loop {
            tokio::time::sleep(LOOK_EVERY / 4).await;
            if let Some(taken) = taken(store, machine)? {
                return Ok::<_, io::Error>(taken);
            }
        }
    });
    waited.await.unwrap_or_else(|_| {
        Err(refusal(format!(
            "a link to {machine} already carries it here, and didn't let it go \
             within {TAKEN_OVER_WITHIN:?}"
        )))
    })
}

/// `machine`'s claim, where no link holds it, with the newest ask for it
/// read just before, so only an ask made since counts against it; this
/// link's own ask, made before, never does.
fn taken(store: &impl Store, machine: &str) -> io::Result<Option<(Claim, Option<i64>)>> {
    let asked = store.superseded(machine);
    Ok(store.claim(machine)?.map(|claim| (claim, asked)))
}

/// Waits until a newer link with `machine` asks for the claim this link
/// took when `asked` was the newest ask, looking each [`LOOK_EVERY`].
pub(super) async fn superseded(store: &impl Store, machine: &str, asked: Option<i64>) {
    let mut looking = tokio::time::interval(LOOK_EVERY);
    loop {
        looking.tick().await;
        if store.superseded(machine) > asked {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::memory::Memory;

    /// How long a test waits on what it needs before failing.
    const PATIENCE: Duration = Duration::from_secs(30);

    #[tokio::test(start_paused = true)]
    async fn an_ask_in_the_millisecond_the_claim_was_taken_still_takes_it_over() {
        let skiff = Memory::default();
        // An older ask, as a takeover before this claim left it.
        skiff.supersede("kestrel").expect("a memory store asks");
        let (_claim, asked) = claimed(&skiff, "kestrel").await.expect("none holds it");
        skiff.supersede("kestrel").expect("a memory store asks");
        let seen = tokio::time::timeout(PATIENCE, superseded(&skiff, "kestrel", asked)).await;
        assert!(seen.is_ok(), "the newer ask went unseen for {PATIENCE:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_ask_made_before_the_claim_was_taken_never_takes_it_over() {
        let skiff = Memory::default();
        skiff.supersede("kestrel").expect("a memory store asks");
        let (_claim, asked) = claimed(&skiff, "kestrel").await.expect("none holds it");
        let seen = tokio::time::timeout(3 * LOOK_EVERY, superseded(&skiff, "kestrel", asked)).await;
        assert!(seen.is_err(), "an ask older than the claim let it go");
    }

    #[tokio::test(start_paused = true)]
    async fn a_link_that_takes_over_isnt_let_go_by_its_own_ask() {
        let skiff = Memory::default();
        let older = skiff
            .claim("kestrel")
            .expect("a memory store claims")
            .expect("none holds it");
        let newer = tokio::spawn({
            let skiff = skiff.clone();
            async move { claimed(&skiff, "kestrel").await }
        });
        let asked = tokio::time::timeout(PATIENCE, superseded(&skiff, "kestrel", None)).await;
        assert!(
            asked.is_ok(),
            "the newer link didn't ask within {PATIENCE:?}"
        );
        drop(older);
        let (_claim, asked) = tokio::time::timeout(PATIENCE, newer)
            .await
            .expect("the newer link takes the claim in time")
            .expect("its task runs to its end")
            .expect("the older let it go");
        let seen = tokio::time::timeout(3 * LOOK_EVERY, superseded(&skiff, "kestrel", asked)).await;
        assert!(seen.is_err(), "its own ask let the newer link's claim go");
    }
}
