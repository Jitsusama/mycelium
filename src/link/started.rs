//! Who started each of this machine's held sessions, as its node's book of
//! starts says, which an end of a link says beside their presences so the
//! other machine's records name the starter too.
//!
//! An end asks the node over its socket, as it asks what the node measures
//! this machine to be, since an end SSH started runs apart from the node
//! and its book. A held id the node names a starter for is kept, as a
//! book's line for a start never changes; one it names none for is asked
//! about again, but no oftener than [`ASKED_AGAIN_AFTER`].

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::Instant;

use crate::wire::{Reply, Request, Response};

/// How long a held id the node named no starter for waits before it is
/// asked about again. A start's line is written to the book as its holder
/// answers, which can come after its session's presence, so one asked too
/// soon is asked again; the person's sessions, which the book never names,
/// are asked about only this often.
const ASKED_AGAIN_AFTER: Duration = Duration::from_secs(30);

/// How long an end waits for its node to answer. The node answers from
/// the book it holds, so one slower than this is stuck, and the end says
/// no starter rather than holding its presences back.
const ANSWERED_WITHIN: Duration = Duration::from_secs(5);

/// The starters an end knows of its machine's held sessions.
#[derive(Debug, Default)]
pub(super) struct Starters {
    /// The starter of each held id the node named one for.
    named: BTreeMap<String, String>,
    /// When each held id the node named none for was last asked about.
    unnamed: BTreeMap<String, Instant>,
}

impl Starters {
    /// Learns the starters of the sessions held as `living` that the node
    /// on `node` hasn't yet named one for, asking it about any not asked
    /// about lately, and forgets every id no longer living.
    pub(super) async fn learn(&mut self, node: &Path, living: &[&str]) {
        self.named.retain(|held, _| living.contains(&held.as_str()));
        self.unnamed
            .retain(|held, _| living.contains(&held.as_str()));
        let now = Instant::now();
        let due: Vec<String> = living
            .iter()
            .filter(|held| !self.named.contains_key(**held))
            .filter(|held| {
                self.unnamed
                    .get(**held)
                    .is_none_or(|asked| now.duration_since(*asked) >= ASKED_AGAIN_AFTER)
            })
            .map(|held| (*held).to_owned())
            .collect();
        if due.is_empty() {
            return;
        }
        let named = asked(node, due.clone()).await.unwrap_or_default();
        for held in due {
            match named.get(&held) {
                Some(by) => {
                    self.unnamed.remove(&held);
                    self.named.insert(held, by.clone());
                }
                None => {
                    self.unnamed.insert(held, now);
                }
            }
        }
    }

    /// The starter of the session held as `held`, as the node last named.
    pub(super) fn of(&self, held: &str) -> Option<&str> {
        self.named.get(held).map(String::as_str)
    }
}

/// The starter of each of `held` that the node on `node` names one for, or
/// none where no node answers there in time, as while it restarts, or one
/// older than [`Request::StartedBy`] refuses the ask.
async fn asked(node: &Path, held: Vec<String>) -> Option<BTreeMap<String, String>> {
    let asking = async {
        let (reading, mut writing) = tokio::net::UnixStream::connect(node)
            .await
            .ok()?
            .into_split();
        let mut ask = serde_json::to_vec(&Request::StartedBy(held)).ok()?;
        ask.push(b'\n');
        writing.write_all(&ask).await.ok()?;
        let line = BufReader::new(reading).lines().next_line().await.ok()??;
        match serde_json::from_str::<Reply>(&line).ok()? {
            Ok(Response::StartedBy(named)) => Some(named),
            _ => None,
        }
    };
    tokio::time::timeout(ANSWERED_WITHIN, asking).await.ok()?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A node on `socket` that answers each ask with `named`, counting
    /// the asks it takes.
    fn answering(
        socket: &Path,
        named: BTreeMap<String, String>,
    ) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        let listener = tokio::net::UnixListener::bind(socket).expect("the socket binds");
        let asks = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&asks);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let (reading, mut writing) = stream.into_split();
                let mut lines = BufReader::new(reading).lines();
                let Ok(Some(line)) = lines.next_line().await else {
                    continue;
                };
                let Ok(Request::StartedBy(held)) = serde_json::from_str(&line) else {
                    continue;
                };
                let answer: Reply = Ok(Response::StartedBy(
                    held.into_iter()
                        .filter_map(|one| Some((one.clone(), named.get(&one)?.clone())))
                        .collect(),
                ));
                let mut said = serde_json::to_vec(&answer).expect("an answer");
                said.push(b'\n');
                let _ = writing.write_all(&said).await;
            }
        });
        asks
    }

    #[tokio::test]
    async fn a_starter_named_is_kept_and_an_unnamed_one_is_asked_again_only_once_due() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let socket = dir.path().join("work.sock");
        let named = BTreeMap::from([("h-1".to_owned(), "OAR001".to_owned())]);
        let asks = answering(&socket, named);
        let mut starters = Starters::default();
        starters.learn(&socket, &["h-1", "h-2"]).await;
        assert_eq!(starters.of("h-1"), Some("OAR001"), "the book's starter");
        assert_eq!(starters.of("h-2"), None, "the book names none");
        starters.learn(&socket, &["h-1", "h-2"]).await;
        assert_eq!(
            asks.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one named is kept, and one unnamed waits to be asked again"
        );
        // The clock runs again once moved, as the node's answer is real.
        tokio::time::pause();
        tokio::time::advance(ASKED_AGAIN_AFTER).await;
        tokio::time::resume();
        starters.learn(&socket, &["h-1", "h-2"]).await;
        assert_eq!(
            asks.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "the unnamed one is asked about again once due"
        );
        starters.learn(&socket, &["h-2"]).await;
        assert_eq!(
            starters.of("h-1"),
            None,
            "an id no longer living is forgotten"
        );
    }

    #[tokio::test]
    async fn a_node_not_answering_names_no_starter() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let mut starters = Starters::default();
        starters
            .learn(&dir.path().join("nobody.sock"), &["h-1"])
            .await;
        assert_eq!(starters.of("h-1"), None);
    }
}
