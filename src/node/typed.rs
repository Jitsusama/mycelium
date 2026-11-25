//! Typed answers: the one way a pi session's process speaks as the person,
//! answering one cell's open ask once with the words the person typed on
//! that session's own command line (PROTOCOL.md's A Typed Answer). The
//! node mints a token to that pi alone, bound to the cell, the ask and the
//! words' digest, and takes it back at its first spend, taken or not, so a
//! token answers once at most and is never tried twice.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use jiff::{SignedDuration, Timestamp};
use sha2::{Digest, Sha256};

/// How long a token stands once minted, past which it is refused: long
/// enough for pi to send the steer it was minted for, and no longer.
const STANDS: SignedDuration = SignedDuration::from_secs(60);

/// How many random bytes a token holds.
const TOKEN_BYTES: usize = 16;

/// What a token is bound to: the cell answered, the ask answered and the
/// digest of the words that answer it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    /// The cell answered, by its address.
    pub address: String,
    /// The ask answered, by its message's id.
    pub want: String,
    /// The words' SHA-256, as 64 lowercase hex digits.
    pub digest: String,
}

/// A token minted and not yet spent.
struct Minted {
    token: String,
    /// The process it was minted to, by its id and its start in clock
    /// ticks since boot.
    asker: (i32, u64),
    bound: Bound,
    until: Timestamp,
}

/// The tokens standing, one a session at most, and the log each mint,
/// spend and refusal is kept in.
pub struct Typed {
    minted: Mutex<HashMap<String, Minted>>,
    log: PathBuf,
}

impl Typed {
    /// None standing yet, each mint, spend and refusal kept in `log`.
    pub fn new(log: PathBuf) -> Self {
        Self {
            minted: Mutex::new(HashMap::new()),
            log,
        }
    }

    /// A token for `session`, minted to the process `asker` and bound to
    /// `bound`, and when it is refused from, in RFC 3339: the session's
    /// token before is forgotten, so a session holds one at most.
    ///
    /// # Errors
    ///
    /// Where no random bytes can be read for it.
    pub fn mint(
        &self,
        session: &str,
        asker: (i32, u64),
        bound: Bound,
    ) -> Result<(String, String), String> {
        let until = Timestamp::now()
            .checked_add(STANDS)
            .map_err(|error| format!("a token's minute can't be told: {error}"))?;
        self.mint_until(session, asker, bound, until)
    }

    fn mint_until(
        &self,
        session: &str,
        asker: (i32, u64),
        bound: Bound,
        until: Timestamp,
    ) -> Result<(String, String), String> {
        let token = token()?;
        self.keep("minted", session, &bound, None);
        let mut minted = self.lock();
        // Those past their minute can never be spent, so none is kept.
        let now = Timestamp::now();
        minted.retain(|_, standing| standing.until > now);
        minted.insert(
            session.to_owned(),
            Minted {
                token: token.clone(),
                asker,
                bound,
                until,
            },
        );
        Ok((token, until.to_string()))
    }

    /// Spends `token` on the answer `bound` names, asked by the process
    /// `asker`, where it was minted to that process for that cell, ask
    /// and words, and stands yet; `refused` is why the steer it came with
    /// is refused whatever the token, such as a field no typed answer
    /// carries. The token is taken back whether or not it is taken.
    ///
    /// # Errors
    ///
    /// Where no token like it stands, or it isn't taken, saying why.
    pub fn spend(
        &self,
        token: &str,
        asker: Option<(i32, u64)>,
        bound: &Bound,
        refused: Option<String>,
    ) -> Result<(), String> {
        let now = Timestamp::now();
        let found = {
            let mut minted = self.lock();
            let session = minted
                .iter()
                .find(|(_, standing)| standing.token == token)
                .map(|(session, _)| session.clone());
            session.and_then(|session| minted.remove(&session).map(|one| (session, one)))
        };
        let Some((session, minted)) = found else {
            let why = "no token like it stands: each is spent by its first try and stands a \
                       minute";
            self.keep("refused", "", bound, Some(why));
            return Err(format!("the typed answer isn't taken: {why}"));
        };
        let why = refused.or_else(|| {
            let why = if asker != Some(minted.asker) {
                "its token was minted to another process"
            } else if !minted.bound.address.eq_ignore_ascii_case(&bound.address) {
                "its token was minted for another cell"
            } else if minted.bound.want != bound.want {
                "its token was minted for another ask"
            } else if minted.bound.digest != bound.digest {
                "its token was minted for other words"
            } else if now >= minted.until {
                "its token's minute is up"
            } else {
                return None;
            };
            Some(why.to_owned())
        });
        if let Some(why) = why {
            self.keep("refused", &session, bound, Some(&why));
            return Err(format!(
                "the typed answer isn't taken: {why}, and its token is spent"
            ));
        }
        self.keep("spent", &session, bound, None);
        Ok(())
    }

    /// Keeps that a mint for `session`, bound to `bound`, was refused, and
    /// `why`.
    pub fn refused(&self, session: &str, bound: &Bound, why: &str) {
        self.keep("refused", session, bound, Some(why));
    }

    /// Keeps what was `did` in the log, as one line written at once, so
    /// two kept together never interleave: never the token or the words.
    fn keep(&self, did: &str, session: &str, bound: &Bound, why: Option<&str>) {
        let mut line = serde_json::json!({
            "at": Timestamp::now().to_string(),
            "did": did,
            "session": session,
            "address": bound.address,
            "want": bound.want,
        });
        if let (Some(why), Some(fields)) = (why, line.as_object_mut()) {
            fields.insert("why".to_owned(), why.into());
        }
        let written = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .and_then(|mut log| log.write_all(format!("{line}\n").as_bytes()));
        if let Err(error) = written {
            eprintln!(
                "mycelium: a typed answer isn't kept in {}: {error}",
                self.log.display()
            );
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Minted>> {
        // A panic while holding it leaves the tokens as they were before
        // the change that panicked, so carrying on with them is sound.
        self.minted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The SHA-256 of `words`, as UTF-8, in 64 lowercase hex digits.
pub fn digest(words: &str) -> String {
    hex(&Sha256::digest(words.as_bytes()))
}

/// Whether `said` is a digest as a mint names one: 64 lowercase hex
/// digits.
pub fn is_digest(said: &str) -> bool {
    said.len() == 64
        && said
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A new token: [`TOKEN_BYTES`] read from the kernel's random source.
fn token() -> Result<String, String> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut bytes))
        .map_err(|error| format!("no random bytes could be read for a token: {error}"))?;
    Ok(hex(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut hex, byte| {
            // Writing to a string never fails.
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASKER: (i32, u64) = (4242, 77);

    /// A try at a token: by which process, bound to what, and what its
    /// refusal says.
    type Tried = (Option<(i32, u64)>, Bound, &'static str);

    fn bound(words: &str) -> Bound {
        Bound {
            address: "TERN02".to_owned(),
            want: "m-ask".to_owned(),
            digest: digest(words),
        }
    }

    fn typed() -> (tempfile::TempDir, Typed) {
        let dir = tempfile::tempdir().expect("a folder");
        let typed = Typed::new(dir.path().join("typed-answers.jsonl"));
        (dir, typed)
    }

    fn logged(dir: &tempfile::TempDir) -> String {
        std::fs::read_to_string(dir.path().join("typed-answers.jsonl")).expect("its log")
    }

    #[test]
    fn a_digest_is_sha256_in_lowercase_hex() {
        assert_eq!(
            digest("yes"),
            "8a798890fe93817163b10b5f7bd2ca4d25d84c52739a645a889c173eee7d9d3d"
        );
        assert!(is_digest(&digest("anything")));
        assert!(
            !is_digest(&digest("anything").to_uppercase()),
            "lowercase only"
        );
        assert!(!is_digest("ab"), "64 digits");
    }

    #[test]
    fn a_token_is_spent_once_by_the_process_it_was_minted_to() {
        let (dir, typed) = typed();
        let (token, until) = typed.mint("s-own", ASKER, bound("yes")).expect("it mints");
        assert_eq!(token.len(), 32, "128 bits in hex");
        assert!(until.parse::<Timestamp>().is_ok(), "{until} is RFC 3339");
        typed
            .spend(&token, Some(ASKER), &bound("yes"), None)
            .expect("it is taken once");
        let again = typed.spend(&token, Some(ASKER), &bound("yes"), None);
        assert!(
            again.is_err_and(|why| why.contains("no token like it")),
            "and never again"
        );
        let log = logged(&dir);
        assert!(
            !log.contains(&token),
            "the log never holds the token: {log}"
        );
        assert!(!log.contains("\"yes\""), "nor the words: {log}");
        let did: Vec<String> = log
            .lines()
            .map(|line| {
                let line: serde_json::Value = serde_json::from_str(line).expect("a line");
                line["did"].as_str().unwrap_or_default().to_owned()
            })
            .collect();
        assert_eq!(did, ["minted", "spent", "refused"]);
    }

    #[test]
    fn a_token_tried_wrong_is_spent_all_the_same() {
        let (_dir, typed) = typed();
        let cases: [Tried; 5] = [
            (Some((4243, 77)), bound("yes"), "another process"),
            (Some((4242, 78)), bound("yes"), "another process"),
            (None, bound("yes"), "another process"),
            (
                Some(ASKER),
                Bound {
                    address: "WREN01".to_owned(),
                    ..bound("yes")
                },
                "another cell",
            ),
            (Some(ASKER), bound("no"), "other words"),
        ];
        for (asker, tried, says) in cases {
            let (token, _) = typed.mint("s-own", ASKER, bound("yes")).expect("it mints");
            let spent = typed.spend(&token, asker, &tried, None);
            assert!(
                spent.is_err_and(|why| why.contains(says)),
                "{asker:?} {tried:?}"
            );
            assert!(
                typed
                    .spend(&token, Some(ASKER), &bound("yes"), None)
                    .is_err(),
                "a token tried wrong is spent"
            );
        }
    }

    #[test]
    fn a_token_for_another_ask_or_past_its_minute_or_refused_besides_isnt_taken() {
        let (_dir, typed) = typed();
        let (token, _) = typed.mint("s-own", ASKER, bound("yes")).expect("it mints");
        let other = Bound {
            want: "m-other".to_owned(),
            ..bound("yes")
        };
        assert!(
            typed
                .spend(&token, Some(ASKER), &other, None)
                .is_err_and(|why| why.contains("another ask"))
        );
        let gone = Timestamp::now()
            .checked_sub(SignedDuration::from_secs(1))
            .expect("a second ago");
        let (token, _) = typed
            .mint_until("s-own", ASKER, bound("yes"), gone)
            .expect("it mints");
        assert!(
            typed
                .spend(&token, Some(ASKER), &bound("yes"), None)
                .is_err_and(|why| why.contains("minute is up"))
        );
        let (token, _) = typed.mint("s-own", ASKER, bound("yes")).expect("it mints");
        let refused = Some("a typed answer never answers a hold".to_owned());
        assert!(
            typed
                .spend(&token, Some(ASKER), &bound("yes"), refused)
                .is_err_and(|why| why.contains("never answers a hold"))
        );
        assert!(
            typed
                .spend(&token, Some(ASKER), &bound("yes"), None)
                .is_err(),
            "a token refused besides is spent"
        );
    }

    #[test]
    fn minting_again_forgets_the_sessions_token_before() {
        let (_dir, typed) = typed();
        let (first, _) = typed.mint("s-own", ASKER, bound("yes")).expect("it mints");
        let (second, _) = typed.mint("s-own", ASKER, bound("yes")).expect("it mints");
        assert_ne!(first, second);
        assert!(
            typed
                .spend(&first, Some(ASKER), &bound("yes"), None)
                .is_err(),
            "the first is forgotten"
        );
        typed
            .spend(&second, Some(ASKER), &bound("yes"), None)
            .expect("the second stands");
    }
}
