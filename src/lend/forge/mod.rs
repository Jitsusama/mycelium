//! The forge: the commits a machine lending signs, and the branches it
//! pushes, for the machines it made, so their git work leaves them with
//! no key or token of the lender's ever crossing to them.
//!
//! It is a lend's upstream, as a model relay is: the lender carries each
//! request of a budget of kind `forge` here, having refused what the lend
//! file doesn't let that machine ask, counted it against the machine's
//! day and named the machine in [`BORROWER`], and a cut ends it as it
//! ends a model call. Here a request is signed with the one key the
//! forge is started with, whose private half only its agent holds, or
//! fetched from the bundle it carries and pushed to a branch the forge
//! is started allowing, never forced. Each is written as one line to
//! this machine's log, with the machine, the cell it says asked, and
//! the commit signed or each commit pushed.
//!
//! [`borrow`] is the borrowing machine's side: git's signing program, and
//! the push that bundles a branch and asks it.

pub mod borrow;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::net::UnixStream;

use crate::lend::serve::{MOST_BODY, MOST_HEAD};
use crate::node::Socket;

/// The header the lender names the borrowing machine in, for a forge
/// alone.
pub const BORROWER: &str = "mycelium-borrower";

/// The most a commit to sign may hold: far past any commit's header and
/// message.
const MOST_PAYLOAD: usize = 1024 * 1024;

/// How long the agent has to sign.
const SIGN_WITHIN: Duration = Duration::from_secs(30);

/// How long a bundle has to be fetched, and its commits listed.
const FETCH_WITHIN: Duration = Duration::from_secs(10 * 60);

/// How long a push has, past the half hour a remote's gate may hold it.
const PUSH_WITHIN: Duration = Duration::from_secs(45 * 60);

/// How often a push still going says so, well within the five minutes
/// the lender lets an answer go quiet.
const STILL_EVERY: Duration = Duration::from_secs(60);

/// The most commits a push's line names, past which it says how many.
const MOST_NAMED: usize = 200;

/// Where the forge fetches each bundle to, below the borrower's name.
const FETCHED: &str = "refs/mycelium-forge";

/// What this machine's forge is started with.
#[derive(Debug, Clone)]
pub struct Forge {
    /// The public half of the key it signs with.
    pub key: PathBuf,
    /// The agent holding its private half.
    pub agent: PathBuf,
    /// The repositories it pushes, by the name a push asks for, each
    /// pushed to its `origin`.
    pub repos: BTreeMap<String, PathBuf>,
    /// The starts of the branches a push may name.
    pub branches: Vec<String>,
    /// The program signing, `ssh-keygen`.
    pub ssh_keygen: PathBuf,
    /// The program fetching and pushing, `git`.
    pub git: PathBuf,
    /// Where each bundle waits while it is fetched.
    pub folder: PathBuf,
}

/// A commit a borrower asks signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signing {
    /// The cell asking, as the borrower says it.
    pub cell: Option<String>,
    /// The commit, as git asks it signed, in base64.
    pub payload: String,
}

/// A commit signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signed {
    /// The signature, as `ssh-keygen -Y sign` writes it.
    pub signature: String,
    /// The commit's id once the signature is in it.
    pub commit: String,
}

/// A branch a borrower asks pushed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pushing {
    /// The cell asking, as the borrower says it.
    pub cell: Option<String>,
    /// The repository, by the name the forge knows it by.
    pub repo: String,
    /// The branch, without `refs/heads/`.
    pub branch: String,
    /// A git bundle of one ref, the commit pushed, in base64.
    pub bundle: String,
}

/// A line of what a push answers, in the order they come: waiting lines
/// while it goes, then whether it was pushed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pushed {
    /// It is still going, this long since it was asked.
    Waiting {
        /// Whole seconds.
        seconds: u64,
    },
    /// It was pushed.
    Pushed {
        /// The commit the branch is at now.
        head: String,
        /// The commits it carried that the bundle's base didn't hold,
        /// newest first.
        commits: Vec<String>,
    },
    /// It wasn't.
    Refused {
        /// Why, in words the borrower shows.
        why: String,
    },
}

/// A request the lender carried here.
struct Asked {
    method: String,
    path: String,
    borrower: Option<String>,
    body: Vec<u8>,
}

/// A refusal, as its status and why.
#[derive(Debug, PartialEq, Eq)]
struct Refusal {
    status: u16,
    why: String,
}

fn refused(status: u16, why: impl Into<String>) -> Refusal {
    Refusal {
        status,
        why: why.into(),
    }
}

/// Serves the lender's requests on `socket` until it fails to accept.
///
/// # Errors
///
/// When the socket stops accepting.
pub async fn serve(socket: Socket, forge: Forge) -> io::Result<()> {
    let forge = Arc::new(forge);
    loop {
        let stream = socket.accept().await?;
        tokio::spawn(connection(stream, forge.clone()));
    }
}

/// Answers the one request a connection carries, as the lender asks
/// each, then hangs up.
async fn connection(stream: UnixStream, forge: Arc<Forge>) {
    let (reading, mut writing) = stream.into_split();
    let mut reading = BufReader::new(reading);
    let asked = match read(&mut reading).await {
        Ok(asked) => asked,
        Err(refusal) => {
            let _ = answer(&mut writing, refusal.status, &json!({"why": refusal.why})).await;
            return;
        }
    };
    let Some(borrower) = asked.borrower.filter(|borrower| named_plainly(borrower)) else {
        let why = "the lender named no machine asking, as it names one for a forge";
        let _ = answer(&mut writing, 400, &json!({"why": why})).await;
        return;
    };
    match (asked.method.as_str(), asked.path.as_str()) {
        ("POST", "/sign") => {
            let (status, said) = match sign(&forge, &borrower, &asked.body).await {
                Ok(signed) => (200, json!(signed)),
                Err(refusal) => (refusal.status, json!({"why": refusal.why})),
            };
            let _ = answer(&mut writing, status, &said).await;
        }
        ("POST", "/push") => push(&forge, &borrower, &asked.body, &mut writing).await,
        (method, path) => {
            let why = format!("the forge answers no {method} {}", path.escape_debug());
            let _ = answer(&mut writing, 404, &json!({"why": why})).await;
        }
    }
}

/// Whether `name` is plain enough to stand in a ref and a file's name:
/// letters, digits and `-`, as a machine's name is.
fn named_plainly(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// Reads the lender's request whole: its line, its headers, and its
/// body of the length they say.
async fn read<R: AsyncBufRead + Unpin>(reading: &mut R) -> Result<Asked, Refusal> {
    let mut spent = 0;
    let line = head_line(reading, &mut spent).await?;
    let mut words = line.split(' ');
    let (Some(method), Some(path), Some(version)) = (words.next(), words.next(), words.next())
    else {
        return Err(refused(400, "the request's line isn't HTTP"));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(refused(400, "the request's line isn't HTTP"));
    }
    let (method, path) = (method.to_owned(), path.to_owned());
    let mut length = 0;
    let mut borrower = None;
    loop {
        let said = head_line(reading, &mut spent).await?;
        if said.is_empty() {
            break;
        }
        let (name, value) = said
            .split_once(':')
            .ok_or_else(|| refused(400, "a header isn't one"))?;
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
        if name == "content-length" {
            length = value
                .parse()
                .map_err(|_| refused(400, "the length isn't one"))?;
        } else if name == BORROWER {
            borrower = Some(value.to_owned());
        } else if name == "transfer-encoding" {
            return Err(refused(501, "the forge takes a body of a length alone"));
        }
    }
    if length > MOST_BODY {
        return Err(refused(413, format!("a body is at most {MOST_BODY} bytes")));
    }
    let mut body = vec![0; length];
    reading
        .read_exact(&mut body)
        .await
        .map_err(|_| refused(400, "the body ended short"))?;
    Ok(Asked {
        method,
        path,
        borrower,
        body,
    })
}

/// The next line of the request's head, counting it against
/// [`MOST_HEAD`].
async fn head_line<R: AsyncBufRead + Unpin>(
    reading: &mut R,
    spent: &mut usize,
) -> Result<String, Refusal> {
    let mut said = Vec::new();
    let room = u64::try_from(MOST_HEAD.saturating_sub(*spent)).unwrap_or(u64::MAX);
    let read = (&mut *reading)
        .take(room)
        .read_until(b'\n', &mut said)
        .await
        .map_err(|_| refused(400, "the request couldn't be read"))?;
    *spent += read;
    let said = said
        .strip_suffix(b"\n")
        .ok_or_else(|| refused(400, "the request's head ended short or ran too long"))?;
    let said = said.strip_suffix(b"\r").unwrap_or(said);
    String::from_utf8(said.to_vec()).map_err(|_| refused(400, "the request's head isn't text"))
}

/// Answers `status` with `said`, whole.
async fn answer<W: AsyncWrite + Unpin>(
    writing: &mut W,
    status: u16,
    said: &serde_json::Value,
) -> io::Result<()> {
    let body = said.to_string();
    let head = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n",
        if status == 200 { "OK" } else { "Refused" },
        body.len()
    );
    writing.write_all(head.as_bytes()).await?;
    writing.write_all(body.as_bytes()).await?;
    writing.flush().await
}

/// Signs the commit `body` asks for `borrower`, saying it in the log
/// however it ends.
async fn sign(forge: &Forge, borrower: &str, body: &[u8]) -> Result<Signed, Refusal> {
    let signing: Result<Signing, _> = serde_json::from_slice(body);
    let cell = signing
        .as_ref()
        .ok()
        .and_then(|signing| signing.cell.clone());
    let signed = signed(forge, signing).await;
    let mut line = json!({
        "time": format!("{:.3}", jiff::Timestamp::now()),
        "vm": borrower,
        "cell": cell,
        "service": "sign",
    });
    match &signed {
        Ok(signed) => {
            line["commit"] = json!(signed.commit);
            line["outcome"] = json!("signed");
        }
        Err(refusal) => line["outcome"] = json!(format!("refused: {}", refusal.why)),
    }
    eprintln!("mycelium-forge: {line}");
    signed
}

/// The signature `signing` asks for, and the commit it makes.
async fn signed(
    forge: &Forge,
    signing: Result<Signing, serde_json::Error>,
) -> Result<Signed, Refusal> {
    let signing = signing.map_err(|_| refused(400, "the request isn't a commit to sign"))?;
    let payload = STANDARD
        .decode(&signing.payload)
        .map_err(|_| refused(400, "the commit isn't base64"))?;
    if payload.len() > MOST_PAYLOAD {
        return Err(refused(
            413,
            format!("a commit to sign is at most {MOST_PAYLOAD} bytes"),
        ));
    }
    if !commit_like(&payload) {
        return Err(refused(
            422,
            "the forge signs commits alone, of a repository whose ids are SHA-1, not yet signed",
        ));
    }
    let signature = run(
        tokio::process::Command::new(&forge.ssh_keygen)
            .args(["-Y", "sign", "-n", "git", "-U", "-f"])
            .arg(&forge.key)
            .env_clear()
            .env("SSH_AUTH_SOCK", &forge.agent),
        Some(&payload),
        SIGN_WITHIN,
    )
    .await
    .map_err(|why| refused(502, format!("the commit couldn't be signed: {why}")))?;
    let signature = String::from_utf8(signature)
        .ok()
        .filter(|signature| signature.starts_with("-----BEGIN SSH SIGNATURE-----\n"))
        .ok_or_else(|| refused(502, "the signing key's agent said what isn't a signature"))?;
    let object = with_signature(&payload, &signature);
    // git hashes it in the forge's own folder, out of any repository.
    own_folder(&forge.folder)
        .map_err(|error| refused(500, format!("the forge's folder couldn't be made: {error}")))?;
    let commit = run(
        tokio::process::Command::new(&forge.git)
            .args(["hash-object", "-t", "commit", "--stdin"])
            .current_dir(&forge.folder)
            .env_clear()
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("HOME", &forge.folder),
        Some(&object),
        SIGN_WITHIN,
    )
    .await
    .map_err(|why| refused(502, format!("the commit signed isn't one: {why}")))?;
    let commit = String::from_utf8_lossy(&commit).trim().to_owned();
    Ok(Signed { signature, commit })
}

/// Whether `payload` is a commit as git asks one signed: a tree of a
/// SHA-1 id first, a committer, and no signature yet.
fn commit_like(payload: &[u8]) -> bool {
    let header = payload
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .map_or(payload, |end| &payload[..=end]);
    let Ok(header) = std::str::from_utf8(header) else {
        return false;
    };
    let mut lines = header.lines();
    let tree = lines.next().and_then(|line| line.strip_prefix("tree "));
    tree.is_some_and(|id| id.len() == 40 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        && header.lines().any(|line| line.starts_with("committer "))
        && !header.lines().any(|line| line.starts_with("gpgsig"))
}

/// `payload` with `signature` in it as git puts one: a `gpgsig` header
/// after the last, each line past its first begun with a space.
#[must_use]
pub fn with_signature(payload: &[u8], signature: &str) -> Vec<u8> {
    let at = payload
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .map_or(payload.len(), |end| end + 1);
    let mut header = String::from("gpgsig");
    for line in signature.split_inclusive('\n') {
        header.push(' ');
        header.push_str(line);
    }
    let mut object = Vec::with_capacity(payload.len() + header.len());
    object.extend_from_slice(&payload[..at]);
    object.extend_from_slice(header.as_bytes());
    object.extend_from_slice(&payload[at..]);
    object
}

/// Runs `command`, handing it `input`, within `within`: what it wrote,
/// where it ended well; else the last line it said, or why it couldn't
/// run.
async fn run(
    command: &mut tokio::process::Command,
    input: Option<&[u8]>,
    within: Duration,
) -> Result<Vec<u8>, String> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let mut stdin = child.stdin.take();
    let feeding = async {
        if let (Some(stdin), Some(input)) = (stdin.as_mut(), input) {
            stdin.write_all(input).await?;
        }
        drop(stdin.take());
        Ok::<(), io::Error>(())
    };
    let ran = tokio::time::timeout(within, async {
        let (fed, output) = tokio::join!(feeding, child.wait_with_output());
        fed.map_err(|error| error.to_string())?;
        output.map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| format!("it took more than {} seconds", within.as_secs()))??;
    if ran.status.success() {
        return Ok(ran.stdout);
    }
    let said = String::from_utf8_lossy(&ran.stderr);
    let last = said
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("it said nothing");
    Err(format!(
        "{} ({})",
        last.chars().take(300).collect::<String>(),
        ran.status
    ))
}

/// Whether `branch` may be pushed as the forge is started: within one of
/// `allowed`, and a branch git would name, with nothing that climbs or
/// is read as more than a name.
fn branch_allowed(branch: &str, allowed: &[String]) -> bool {
    let plain = |byte: u8| byte.is_ascii_alphanumeric() || b"/_-.".contains(&byte);
    allowed
        .iter()
        .any(|start| !start.is_empty() && branch.len() > start.len() && branch.starts_with(start))
        && branch.bytes().all(plain)
        && !branch.contains("..")
        && !branch.contains("//")
        && !branch.ends_with('/')
        && !Path::new(branch)
            .extension()
            .is_some_and(|ending| ending.eq_ignore_ascii_case("lock"))
        && !branch.split('/').any(|part| part.starts_with('.'))
}

/// A bundle's one ref and the commits it needs the fetching repository
/// to hold, as its header says them; else why it isn't one.
fn bundled(bundle: &[u8]) -> Result<(String, Vec<String>), &'static str> {
    let end = bundle
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .ok_or("isn't a git bundle")?;
    let header = std::str::from_utf8(&bundle[..end]).map_err(|_| "isn't a git bundle")?;
    let mut lines = header.lines();
    if !matches!(lines.next(), Some("# v2 git bundle" | "# v3 git bundle")) {
        return Err("isn't a git bundle of version 2 or 3");
    }
    let id = |word: &str| word.len() == 40 && word.bytes().all(|byte| byte.is_ascii_hexdigit());
    let mut needs = Vec::new();
    let mut heads = Vec::new();
    for line in lines {
        if line.starts_with('@') {
            if line != "@object-format=sha1" {
                return Err("has a capability the forge doesn't take");
            }
        } else if let Some(needed) = line.strip_prefix('-') {
            let needed = needed.split(' ').next().unwrap_or_default();
            if !id(needed) {
                return Err("needs what isn't a commit");
            }
            needs.push(needed.to_owned());
        } else {
            let head = line.split(' ').next().unwrap_or_default();
            if !id(head) {
                return Err("holds a ref that isn't a commit");
            }
            heads.push(head.to_owned());
        }
    }
    heads.dedup();
    match heads.as_slice() {
        [head] => Ok((head.clone(), needs)),
        [] => Err("holds no commit to push"),
        _ => Err("holds more than one commit to push"),
    }
}

/// A push, asked once at a time across the forge by its count.
static PUSHES: AtomicU64 = AtomicU64::new(0);

/// Pushes what `body` asks for `borrower`, answering as it goes: its
/// refusal whole where it can be refused at once, else a head, a waiting
/// line each minute, and how it ended.
async fn push<W: AsyncWrite + Unpin>(forge: &Forge, borrower: &str, body: &[u8], writing: &mut W) {
    let began = Instant::now();
    let pushing: Result<Pushing, _> = serde_json::from_slice(body);
    let mut line = json!({
        "time": format!("{:.3}", jiff::Timestamp::now()),
        "vm": borrower,
        "cell": pushing.as_ref().ok().and_then(|pushing| pushing.cell.clone()),
        "service": "push",
    });
    let checked = pushing
        .map_err(|_| refused(400, "the request isn't a branch to push"))
        .and_then(|pushing| checked(forge, pushing));
    let (repo, pushing, bundle, head, needs) = match checked {
        Ok(checked) => checked,
        Err(refusal) => {
            line["outcome"] = json!(format!("refused: {}", refusal.why));
            eprintln!("mycelium-forge: {line}");
            let _ = answer(writing, refusal.status, &json!({"why": refusal.why})).await;
            return;
        }
    };
    line["repo"] = json!(pushing.repo);
    line["branch"] = json!(pushing.branch);
    line["head"] = json!(head);
    let headed = writing
        .write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\n\
              transfer-encoding: chunked\r\nconnection: close\r\n\r\n",
        )
        .await;
    if headed.is_err() {
        return;
    }
    let going = pushed(
        forge,
        &repo,
        borrower,
        &pushing.branch,
        &bundle,
        &head,
        &needs,
    );
    tokio::pin!(going);
    let ended = loop {
        tokio::select! {
            ended = &mut going => break ended,
            () = tokio::time::sleep(STILL_EVERY) => {
                let seconds = began.elapsed().as_secs();
                if said(writing, &Pushed::Waiting { seconds }).await.is_err() {
                    // The lender let go, so the push is dropped, and git
                    // with it.
                    line["outcome"] = json!("let go: its borrower stopped reading");
                    eprintln!("mycelium-forge: {line}");
                    return;
                }
            }
        }
    };
    match &ended {
        Pushed::Pushed { commits, .. } => {
            line["commits"] = json!(commits);
            line["outcome"] = json!("pushed");
        }
        Pushed::Refused { why } => line["outcome"] = json!(format!("refused: {why}")),
        Pushed::Waiting { .. } => {}
    }
    eprintln!("mycelium-forge: {line}");
    if said(writing, &ended).await.is_ok() {
        let _ = writing.write_all(b"0\r\n\r\n").await;
        let _ = writing.flush().await;
    }
}

/// What a push checked at once: its repository's folder, the push, its
/// bundle, the commit it pushes and those it needs.
type Checked = (PathBuf, Pushing, Vec<u8>, String, Vec<String>);

/// Whether `pushing` may be pushed, as far as can be told before git is
/// asked.
fn checked(forge: &Forge, pushing: Pushing) -> Result<Checked, Refusal> {
    let repo = forge
        .repos
        .get(&pushing.repo)
        .ok_or_else(|| {
            refused(
                404,
                format!(
                    "the forge pushes no repository named {}",
                    pushing.repo.escape_debug()
                ),
            )
        })?
        .clone();
    if !branch_allowed(&pushing.branch, &forge.branches) {
        return Err(refused(
            403,
            format!(
                "the forge pushes only branches beginning {}, so not {}",
                forge.branches.join(" or "),
                pushing.branch.escape_debug()
            ),
        ));
    }
    let bundle = STANDARD
        .decode(&pushing.bundle)
        .map_err(|_| refused(400, "the bundle isn't base64"))?;
    let (head, needs) =
        bundled(&bundle).map_err(|why| refused(400, format!("the bundle {why}")))?;
    Ok((repo, pushing, bundle, head, needs))
}

/// Fetches `bundle` into `repo` and pushes `head` to `branch` on its
/// origin, never forced: how it ended.
async fn pushed(
    forge: &Forge,
    repo: &Path,
    borrower: &str,
    branch: &str,
    bundle: &[u8],
    head: &str,
    needs: &[String],
) -> Pushed {
    let count = PUSHES.fetch_add(1, Ordering::Relaxed);
    let file = forge
        .folder
        .join(format!("{borrower}-{}-{count}.bundle", std::process::id()));
    let fetched = format!("{FETCHED}/{borrower}/{}-{count}", std::process::id());
    let ended = carried(forge, repo, &file, &fetched, branch, bundle, head, needs).await;
    let _ = fs::remove_file(&file);
    let _ = run(
        git(forge, repo).args(["update-ref", "-d", &fetched]),
        None,
        FETCH_WITHIN,
    )
    .await;
    match ended {
        Ok(commits) => Pushed::Pushed {
            head: head.to_owned(),
            commits,
        },
        Err(why) => Pushed::Refused { why },
    }
}

/// `git` in `repo`.
fn git(forge: &Forge, repo: &Path) -> tokio::process::Command {
    let mut git = tokio::process::Command::new(&forge.git);
    git.arg("-C").arg(repo);
    git
}

/// The steps of [`pushed`], up to its tidying: the commits pushed.
#[expect(
    clippy::too_many_arguments,
    reason = "each is one push's own, and bundling them would only rename them"
)]
async fn carried(
    forge: &Forge,
    repo: &Path,
    file: &Path,
    fetched: &str,
    branch: &str,
    bundle: &[u8],
    head: &str,
    needs: &[String],
) -> Result<Vec<String>, String> {
    kept(&forge.folder, file, bundle)
        .map_err(|error| format!("the bundle couldn't be kept: {error}"))?;
    run(
        git(forge, repo)
            .args(["fetch", "--quiet", "--no-tags", "--no-write-fetch-head"])
            .arg(file)
            .arg(format!("{head}:{fetched}")),
        None,
        FETCH_WITHIN,
    )
    .await
    .map_err(|why| format!("the bundle couldn't be fetched: {why}"))?;
    let mut listing = git(forge, repo);
    listing
        .args(["rev-list", &format!("--max-count={}", MOST_NAMED + 1), head])
        .args(needs.iter().map(|needed| format!("^{needed}")));
    let listed = run(&mut listing, None, FETCH_WITHIN)
        .await
        .map_err(|why| format!("the bundle's commits couldn't be listed: {why}"))?;
    let mut commits: Vec<String> = String::from_utf8_lossy(&listed)
        .lines()
        .map(str::to_owned)
        .collect();
    if commits.len() > MOST_NAMED {
        commits.truncate(MOST_NAMED);
        commits.push(format!("and more past these {MOST_NAMED}"));
    }
    run(
        git(forge, repo)
            // The repository's own hooks run, so its gates hold.
            .args(["push", "--quiet", "origin"])
            .arg(format!("{head}:refs/heads/{branch}")),
        None,
        PUSH_WITHIN,
    )
    .await
    .map_err(|why| format!("the push was refused: {why}"))?;
    Ok(commits)
}

/// Keeps `bundle` at `file` in `folder`, both for this account alone.
fn kept(folder: &Path, file: &Path, bundle: &[u8]) -> io::Result<()> {
    own_folder(folder)?;
    let mut opened = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(file)?;
    io::Write::write_all(&mut opened, bundle)
}

/// Makes `folder`, for this account alone, if it isn't there.
fn own_folder(folder: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(folder)
}

/// Writes `pushed` as one line of a chunked answer.
async fn said<W: AsyncWrite + Unpin>(writing: &mut W, pushed: &Pushed) -> io::Result<()> {
    let mut line = serde_json::to_string(pushed).map_err(io::Error::other)?;
    line.push('\n');
    let mut piece = String::new();
    let _ = write!(piece, "{:x}\r\n{line}\r\n", line.len());
    writing.write_all(piece.as_bytes()).await?;
    writing.flush().await
}
