//! Links whose way in turned this machine away, as SSH does when the other
//! machine doesn't take this one's key. Such a link isn't dialled again for
//! hours: dialling it every minute only fails again, and every failure is a
//! refused login that the other machine, and whoever watches it, sees.
//!
//! A refusal is remembered in the node's state by the words that dial the
//! link, so a node started again doesn't dial it at once either, while a
//! link whose words change, as when its configuration does, is dialled at
//! once. Removing the record and starting the node again dials it sooner,
//! as once the other machine takes the key.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

/// How long a refused link waits before it is dialled again.
pub const REFUSED_FOR: Duration = Duration::from_secs(6 * 60 * 60);

/// What SSH says when the other machine turns this one away, rather than
/// being out of reach: a key it doesn't take, a host key that doesn't
/// match what this machine knows, or too many keys tried.
const TURNED_AWAY: [&str; 3] = [
    "Permission denied (",
    "Host key verification failed",
    "Too many authentication failures",
];

/// How a node keeps the refusals of its links.
#[derive(Debug, Clone)]
pub struct Refusals {
    /// The folder they are remembered in, or none to keep them only while
    /// the node runs.
    pub folder: Option<PathBuf>,
    /// How long a refused link waits before it is dialled again.
    pub lasting: Duration,
}

impl Default for Refusals {
    fn default() -> Self {
        Self {
            folder: None,
            lasting: REFUSED_FOR,
        }
    }
}

impl Refusals {
    /// Refusals remembered in `folder` for as long as `REFUSED_FOR`.
    #[must_use]
    pub fn kept_in(folder: PathBuf) -> Self {
        Self {
            folder: Some(folder),
            lasting: REFUSED_FOR,
        }
    }

    /// How long the link dialled by `words` has yet to wait, if it was
    /// turned away within `lasting`.
    #[must_use]
    pub fn waiting(&self, words: &[String]) -> Option<Duration> {
        let written = fs::metadata(self.record(words)?).ok()?.modified().ok()?;
        // A record written in the future, by a clock set back, waits in full.
        let since = written.elapsed().unwrap_or_default();
        self.lasting
            .checked_sub(since)
            .filter(|left| !left.is_zero())
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
        assert_eq!(refusals.waiting(&equilibrius), None, "nothing refused yet");

        refusals
            .remember(&equilibrius, "Permission denied (publickey).")
            .expect("the refusal is remembered");
        let left = refusals.waiting(&equilibrius).expect("the link waits");
        assert!(
            left > REFUSED_FOR.saturating_sub(Duration::from_secs(60)) && left <= REFUSED_FOR,
            "it waits about {REFUSED_FOR:?}, not {left:?}"
        );
        let again = Refusals::kept_in(dir.path().join("refused")).waiting(&equilibrius);
        assert!(
            again.is_some_and(|again| again <= left),
            "a node started again reads the same refusal, not {again:?}"
        );
        assert_eq!(
            refusals.waiting(&words("equilibrius.local")),
            None,
            "a link whose words changed is dialled at once"
        );

        refusals
            .forget(&equilibrius)
            .expect("the refusal is forgotten");
        assert_eq!(refusals.waiting(&equilibrius), None, "and dialled again");
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
        assert_eq!(refusals.waiting(&equilibrius), None, "so none is waited on");
    }
}
