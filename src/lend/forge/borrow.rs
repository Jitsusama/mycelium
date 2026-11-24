//! The forge's side on a borrowing machine: the push that bundles a
//! branch of unsigned commits and asks it, and taking back the commits
//! the forge signed. A push asks this machine's node for a lend of a
//! `forge` budget, as `lend serve` asks one of a model's, so the node
//! carries it over the link it already holds to the lender, and nothing
//! here holds a key or a token.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::{Pushed, Pushing};
use crate::lend::serve::MOST_BODY;
use crate::lend::{Lend, Lent};
use crate::wire::Request;

/// The budget a forge's lends are counted against unless one is named.
pub const BUDGET: &str = "forge";

/// How long the node may say nothing before the lend is given up: past
/// the minute between a push's waiting lines, and within the lender's
/// own five.
const QUIET_WITHIN: Duration = Duration::from_secs(5 * 60);

/// Whom a lend is asked of, and as whom.
#[derive(Debug, Clone)]
pub struct Asking<'a> {
    /// This machine's node.
    pub node: &'a Path,
    /// The budget it is counted against.
    pub budget: &'a str,
    /// The cell asking, as this machine says it.
    pub cell: Option<String>,
}

/// What a lend answered: its status, and its body as it came, each piece
/// given to `each`.
async fn lent(
    asking: &Asking<'_>,
    path: &str,
    body: &serde_json::Value,
    mut each: impl FnMut(&[u8]),
) -> Result<u16, String> {
    let body = body.to_string();
    let encoded = STANDARD.encode(body.as_bytes());
    if encoded.len() > MOST_BODY {
        return Err(format!(
            "what would be asked is {} bytes once encoded, past the {MOST_BODY} a lend carries",
            encoded.len()
        ));
    }
    let lend = Lend {
        budget: Some(asking.budget.to_owned()),
        method: "POST".to_owned(),
        path: path.to_owned(),
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        body: encoded,
    };
    let mut stream = UnixStream::connect(asking.node)
        .await
        .map_err(|error| format!("this machine's node didn't answer: {error}"))?;
    let mut line =
        serde_json::to_string(&Request::Lend(lend)).map_err(|error| error.to_string())?;
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .await
        .map_err(|error| format!("this machine's node didn't answer: {error}"))?;
    let mut lines = BufReader::new(stream).lines();
    let mut status = None;
    loop {
        let said = tokio::time::timeout(QUIET_WITHIN, lines.next_line())
            .await
            .map_err(|_| "this machine's node said nothing for five minutes".to_owned())?
            .map_err(|error| format!("this machine's node stopped answering: {error}"))?
            .ok_or_else(|| "this machine's node stopped answering".to_owned())?;
        let heard: Lent = serde_json::from_str(&said).map_err(|error| {
            format!("this machine's node said what isn't a lend's answer: {error}")
        })?;
        match (heard, status) {
            (Lent::Head { status: code, .. }, None) => status = Some(code),
            (Lent::Body(piece), Some(_)) => {
                let piece = STANDARD
                    .decode(piece)
                    .map_err(|_| "the lender sent a piece that isn't base64".to_owned())?;
                each(&piece);
            }
            (Lent::Done, Some(status)) => return Ok(status),
            (Lent::Refused { status, why }, None) => return Err(format!("{why} ({status})")),
            _ => return Err("the lender's answer came out of order".to_owned()),
        }
    }
}

/// Why a refusal of `status` said it was refused, from its body.
fn why_refused(status: u16, body: &[u8]) -> String {
    let said: Option<String> = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|body| body.get("why")?.as_str().map(str::to_owned));
    said.map_or_else(
        || format!("the forge refused it ({status})"),
        |why| format!("{why} ({status})"),
    )
}

/// Asks the forge to push `bundle`, a git bundle of one ref, to `branch`
/// of `repo`, giving each line it answers to `each` as it comes.
/// Answers how it ended: pushed or refused.
///
/// # Errors
///
/// When it can't be asked, or the answer ends before saying how it ended.
pub async fn push(
    asking: &Asking<'_>,
    repo: &str,
    branch: &str,
    bundle: &[u8],
    mut each: impl FnMut(&str),
) -> Result<Pushed, String> {
    let pushing = Pushing {
        cell: asking.cell.clone(),
        repo: repo.to_owned(),
        branch: branch.to_owned(),
        bundle: STANDARD.encode(bundle),
    };
    let (mut pending, mut body, mut ended) = (Vec::new(), Vec::new(), None);
    let status = lent(asking, "/push", &json!(pushing), |piece| {
        body.extend_from_slice(piece);
        pending.extend_from_slice(piece);
        while let Some(at) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=at).collect();
            let line = String::from_utf8_lossy(&line[..at]).into_owned();
            if let Ok(pushed) = serde_json::from_str::<Pushed>(&line)
                && !matches!(pushed, Pushed::Waiting { .. })
            {
                ended = Some(pushed);
            }
            each(&line);
        }
    })
    .await?;
    if status != 200 {
        return Err(why_refused(status, &body));
    }
    ended.ok_or_else(|| "the forge's answer ended before saying whether it pushed".to_owned())
}

/// A bundle of what `head` holds past `base`, in `repo`, as git makes it.
///
/// # Errors
///
/// When git can't make it, saying what git said.
pub fn bundle(git: &Path, repo: &Path, base: &str, head: &str) -> io::Result<Vec<u8>> {
    let made = std::process::Command::new(git)
        .arg("-C")
        .arg(repo)
        .args(["bundle", "create", "--quiet", "-"])
        .arg(format!("{base}..{head}"))
        .output()?;
    if made.status.success() {
        Ok(made.stdout)
    } else {
        Err(io::Error::other(format!(
            "git couldn't bundle {base}..{head}: {}",
            String::from_utf8_lossy(&made.stderr).trim()
        )))
    }
}

/// The commit `rev` names in `repo`.
///
/// # Errors
///
/// When git can't say, saying what git said.
pub fn resolved(git: &Path, repo: &Path, rev: &str) -> io::Result<String> {
    said(
        git,
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
}

/// What git says to `words` in `repo`, trimmed; else what it said wrong.
fn said(git: &Path, repo: &Path, words: &[&str]) -> io::Result<String> {
    let ran = std::process::Command::new(git)
        .arg("-C")
        .arg(repo)
        .args(words)
        .output()?;
    if ran.status.success() {
        Ok(String::from_utf8_lossy(&ran.stdout).trim().to_owned())
    } else {
        Err(io::Error::other(format!(
            "git {} said: {}",
            words.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&ran.stderr).trim()
        )))
    }
}

/// Takes the commits the forge signed in `pushed` back into `repo`, and
/// moves `local`, which named `was` when it was pushed, to the signed
/// head, unless it has moved since: the ref it moved, if any. The signed
/// commits hold the trees the unsigned ones did, so nothing checked out
/// changes.
///
/// # Errors
///
/// When the bundle isn't one, or git can't take it.
pub fn arrive(
    git: &Path,
    repo: &Path,
    local: &str,
    was: &str,
    pushed: &Pushed,
) -> io::Result<Option<String>> {
    let Pushed::Pushed {
        head,
        bundle: Some(bundle),
        ..
    } = pushed
    else {
        return Ok(None);
    };
    let bundle = STANDARD
        .decode(bundle)
        .map_err(|_| io::Error::other("the forge's bundle isn't base64"))?;
    let file = PathBuf::from(said(git, repo, &["rev-parse", "--absolute-git-dir"])?)
        .join(format!("mycelium-forge-{}.bundle", std::process::id()));
    std::fs::write(&file, bundle)?;
    let took = said(
        git,
        repo,
        &["bundle", "unbundle", file.to_str().unwrap_or_default()],
    );
    let _ = std::fs::remove_file(&file);
    took?;
    // A commit named by its id moves nothing; HEAD detached moves itself.
    let named = said(git, repo, &["rev-parse", "--symbolic-full-name", local])?;
    if named.is_empty() {
        return Ok(None);
    }
    said(git, repo, &["update-ref", &named, head, was])?;
    Ok(Some(named))
}
