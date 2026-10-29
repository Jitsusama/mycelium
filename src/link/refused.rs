//! Links whose way in turned this machine away, as SSH does when the other
//! machine doesn't take this one's key. Such a link isn't dialled again
//! until someone says it may be: dialling it again, however seldom, only
//! fails again, and every failure is a refused login that the other
//! machine, and whoever watches it, sees. Nothing on this machine can tell
//! when the other starts taking its key, so no wait of ours would be right.
//!
//! A refusal is remembered in the node's state by the words that dial the
//! link, so a node started again doesn't dial it either, while a link whose
//! words change, as when its configuration does, is dialled at once.
//! Removing the record, as once the other machine takes the key, withdraws
//! it: the node looks for it every `LOOK_EVERY`, on its own disk alone, and
//! dials once it is gone.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

/// How often a node looks whether a refusal it keeps was withdrawn.
pub const LOOK_EVERY: Duration = Duration::from_secs(60);

/// What a way in says when the other machine turns this one away, rather
/// than being out of reach. SSH's: a key it doesn't take, a host key that
/// doesn't match what this machine knows, or too many keys tried. A
/// provider's program says it in mycelium's own words.
const TURNED_AWAY: [&str; 4] = [
    "Permission denied (",
    "Host key verification failed",
    "Too many authentication failures",
    "turned this machine away",
];

/// How a node keeps the refusals of its links.
#[derive(Debug, Clone)]
pub struct Refusals {
    /// The folder they are remembered in, or none to keep them only while
    /// the node runs, standing until it ends.
    pub folder: Option<PathBuf>,
    /// How often a refusal kept there is looked for.
    pub look_every: Duration,
}

impl Default for Refusals {
    fn default() -> Self {
        Self {
            folder: None,
            look_every: LOOK_EVERY,
        }
    }
}

impl Refusals {
    /// Refusals remembered in `folder`, looked for every `LOOK_EVERY`.
    #[must_use]
    pub fn kept_in(folder: PathBuf) -> Self {
        Self {
            folder: Some(folder),
            look_every: LOOK_EVERY,
        }
    }

    /// Whether a refusal of the link dialled by `words` is remembered, and
    /// not yet withdrawn.
    #[must_use]
    pub fn stands(&self, words: &[String]) -> bool {
        self.record(words).is_some_and(|record| record.exists())
    }

    /// Waits until the refusal of the link dialled by `words` is withdrawn,
    /// which a refusal kept nowhere never is while the node runs.
    pub async fn withdrawn(&self, words: &[String]) {
        if self.folder.is_none() {
            std::future::pending::<()>().await;
        }
        while self.stands(words) {
            tokio::time::sleep(self.look_every).await;
        }
    }

    /// Remembers that the link dialled by `words` was turned away, and why.
    ///
    /// # Errors
    ///
    /// When the record can't be written.
    pub fn remember(&self, words: &[String], why: &str) -> io::Result<()> {
        let Some(record) = self.record(words) else {
            return Ok(());
        };
        if let Some(folder) = record.parent() {
            fs::create_dir_all(folder)?;
        }
        fs::write(record, format!("{}\n{why}\n", words.join(" ")))
    }

    /// Forgets any refusal of the link dialled by `words`, as once it has
    /// been let in, or failed only to reach the other machine.
    ///
    /// # Errors
    ///
    /// When a record there can't be removed.
    pub fn forget(&self, words: &[String]) -> io::Result<()> {
        match self.record(words).map(fs::remove_file) {
            Some(Err(error)) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Where the refusal of the link dialled by `words` is remembered, if
    /// anywhere.
    #[must_use]
    pub fn record(&self, words: &[String]) -> Option<PathBuf> {
        let key = blake3::hash(words.join("\0").as_bytes()).to_hex();
        Some(self.folder.as_ref()?.join(&key.as_str()[..16]))
    }
}

/// Passes on each line `said` holds, as SSH's own error output, answering
/// the first that says the other machine turned this one away, once all
/// of it has been said.
pub async fn turned_away(said: impl AsyncRead + Unpin) -> Option<String> {
    let mut lines = BufReader::new(said).lines();
    let mut refused = None;
    while let Ok(Some(line)) = lines.next_line().await {
        eprintln!("{line}");
        if refused.is_none() && TURNED_AWAY.iter().any(|away| line.contains(away)) {
            refused = Some(line);
        }
    }
    refused
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(destination: &str) -> Vec<String> {
        ["ssh", "-t", "--", destination].map(str::to_owned).to_vec()
    }

    #[tokio::test]
    async fn a_refused_key_is_told_from_a_machine_out_of_reach() {
        let refused = turned_away(
            &b"Warning: added a host key\n\
               joel@equilibrius.local: Permission denied (publickey,password).\n"[..],
        )
        .await;
        assert_eq!(
            refused.as_deref(),
            Some("joel@equilibrius.local: Permission denied (publickey,password)."),
            "a key the other machine doesn't take turns this one away"
        );
        for out_of_reach in [
            "ssh: connect to host equilibrius.local port 22: Connection refused\n",
            "ssh: Could not resolve hostname equilibrius.local: Name or service not known\n",
            "ssh: connect to host equilibrius.local port 22: Operation timed out\n",
        ] {
            assert_eq!(
                turned_away(out_of_reach.as_bytes()).await,
                None,
                "a machine out of reach hasn't turned this one away: {out_of_reach}"
            );
        }
    }

    #[test]
    fn a_refusal_is_remembered_for_the_words_that_dialled_it_until_forgotten() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let refusals = Refusals::kept_in(dir.path().join("refused"));
        let equilibrius = words("equilibrius");
        assert!(!refusals.stands(&equilibrius), "nothing refused yet");

        refusals
            .remember(&equilibrius, "Permission denied (publickey).")
            .expect("the refusal is remembered");
        assert!(refusals.stands(&equilibrius), "the link waits");
        assert!(
            Refusals::kept_in(dir.path().join("refused")).stands(&equilibrius),
            "a node started again reads the same refusal"
        );
        assert!(
            !refusals.stands(&words("equilibrius.local")),
            "a link whose words changed is dialled at once"
        );

        refusals
            .forget(&equilibrius)
            .expect("the refusal is forgotten");
        assert!(!refusals.stands(&equilibrius), "and dialled again");
        refusals
            .forget(&equilibrius)
            .expect("forgetting what isn't remembered is no error");
    }

    #[test]
    fn a_refusal_kept_nowhere_is_never_waited_out_from_a_record() {
        let refusals = Refusals::default();
        let equilibrius = words("equilibrius");
        refusals
            .remember(&equilibrius, "Permission denied (publickey).")
            .expect("nothing is written");
        assert_eq!(refusals.record(&equilibrius), None, "no record is kept");
        assert!(!refusals.stands(&equilibrius), "so none is read back");
    }

    #[tokio::test]
    async fn a_refusal_stands_until_its_record_is_removed() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let refusals = Refusals {
            folder: Some(dir.path().join("refused")),
            look_every: Duration::from_millis(10),
        };
        let equilibrius = words("equilibrius");
        refusals
            .remember(&equilibrius, "Permission denied (publickey).")
            .expect("the refusal is remembered");
        let waiting = {
            let (refusals, equilibrius) = (refusals.clone(), equilibrius.clone());
            tokio::spawn(async move { refusals.withdrawn(&equilibrius).await })
        };
        // That nothing withdraws it can only be shown over a span, here ten
        // looks' worth, rather than waited on as an event.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!waiting.is_finished(), "no time withdraws a refusal");

        let record = refusals.record(&equilibrius).expect("a record is kept");
        fs::remove_file(record).expect("the record is removed");
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("removing the record withdraws the refusal")
            .expect("the wait ends");
    }
}
