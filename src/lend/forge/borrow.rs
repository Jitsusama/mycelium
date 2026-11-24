//! The forge's side on a borrowing machine: git's signing program, and
//! the push that bundles a branch and asks it. Each asks this machine's
//! node for a lend of a `forge` budget, as `lend serve` asks one of a
//! model's, so the node carries it over the link it already holds to the
//! lender, and nothing here holds a key or a token.

use std::io;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::{Pushed, Pushing, Signed, Signing};
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

/// Asks the forge to sign `payload`, a commit as git asks it signed.
///
/// # Errors
///
/// When the node, the lender or the forge refuses it, saying why.
pub async fn sign(asking: &Asking<'_>, payload: &[u8]) -> Result<Signed, String> {
    let signing = Signing {
        cell: asking.cell.clone(),
        payload: STANDARD.encode(payload),
    };
    let mut body = Vec::new();
    let status = lent(asking, "/sign", &json!(signing), |piece| {
        body.extend_from_slice(piece);
    })
    .await?;
    if status != 200 {
        return Err(why_refused(status, &body));
    }
    serde_json::from_slice(&body)
        .map_err(|error| format!("the forge's answer isn't a signature: {error}"))
}

/// What git asks of its signing program, read from its words: to sign
/// the file named last, or anything else, which goes to `ssh-keygen`.
#[derive(Debug, PartialEq, Eq)]
pub enum Asked<'a> {
    /// Sign this file, writing its signature beside it, as `FILE.sig`.
    Sign(&'a str),
    /// Verify, find a principal or anything else `ssh-keygen` does.
    Other,
}

/// What `words`, git's to its signing program, ask.
#[must_use]
pub fn asked(words: &[String]) -> Asked<'_> {
    let signs = words
        .windows(2)
        .any(|pair| pair[0] == "-Y" && pair[1] == "sign");
    match words.last() {
        Some(file) if signs && !file.starts_with('-') => Asked::Sign(file),
        _ => Asked::Other,
    }
}

/// Signs the file `file` names through the forge, writing the signature
/// to `FILE.sig` as `ssh-keygen -Y sign` does, so git takes it as its own.
///
/// # Errors
///
/// When the file can't be read or the signature written, or the forge
/// refuses it.
pub async fn sign_file(asking: &Asking<'_>, file: &Path) -> Result<Signed, String> {
    let payload = std::fs::read(file)
        .map_err(|error| format!("{} couldn't be read: {error}", file.display()))?;
    let signed = sign(asking, &payload).await?;
    let mut written = file.as_os_str().to_owned();
    written.push(".sig");
    std::fs::write(&written, &signed.signature).map_err(|error| {
        format!(
            "the signature couldn't be written to {}: {error}",
            written.display()
        )
    })?;
    Ok(signed)
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
