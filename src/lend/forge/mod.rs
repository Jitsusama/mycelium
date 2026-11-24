//! The forge: the branches a machine lending pushes for the machines it
//! made, each commit signed, so their git work leaves them with no key
//! or token of the lender's ever crossing to them.
//!
//! It is a lend's upstream, as a model relay is: the lender carries each
//! request of a budget of kind `forge` here, having refused what the lend
//! file doesn't let that machine ask, counted it against the machine's
//! day and named the machine in [`BORROWER`], and a cut ends it as it
//! ends a model call. A push carries a bundle of unsigned commits; the
//! forge fetches it, signs each commit its origin doesn't hold yet with
//! the one key it is started with (whose private half only its agent
//! holds), oldest first, each pointing at its parents as signed, and
//! pushes the signed chain to a branch it is started allowing, never
//! forced. It signs nothing else: no signature leaves it but inside a
//! commit it pushes, so none can be carried to other work. Each push is
//! one line of this machine's log, with the machine, the cell it says
//! asked, and each commit as it came and as signed.
//!
//! [`borrow`] is the borrowing machine's side: the push that bundles a
//! branch and asks it, and taking the signed commits back.

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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
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

/// The most commits one push signs.
const MOST_SIGNED: usize = 500;

/// How long the agent has to sign.
const SIGN_WITHIN: Duration = Duration::from_secs(30);

/// How long a bundle has to be fetched, and its commits listed.
const FETCH_WITHIN: Duration = Duration::from_secs(10 * 60);

/// How long a push has, past the half hour a remote's gate may hold it.
const PUSH_WITHIN: Duration = Duration::from_secs(45 * 60);

/// How often a push still going says so, well within the five minutes
/// the lender lets an answer go quiet.
const STILL_EVERY: Duration = Duration::from_secs(60);

/// How long each ref a push leaves has to be let go, so a git that
/// hangs as it tidies never holds the forge's turn for good.
const TIDY_WITHIN: Duration = Duration::from_secs(30);

/// The most pushes the forge holds at once, the one pushing and those
/// waiting their turn, since each holds its connection and its bundle.
const MOST_PUSHING: usize = 8;

/// How long taking connections may fail without one taken before the
/// forge ends, so its service manager starts it afresh.
const ACCEPTS_FAIL_FOR: Duration = Duration::from_secs(60);

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
    /// pushed to its `origin`: a clone the forge keeps for itself, since
    /// what it holds that its origin doesn't is only what borrowers sent.
    pub repos: BTreeMap<String, PathBuf>,
    /// The starts of the branches a push may name, each ending in `/`.
    pub branches: Vec<String>,
    /// The program signing, `ssh-keygen`.
    pub ssh_keygen: PathBuf,
    /// The program fetching and pushing, `git`.
    pub git: PathBuf,
    /// Where each bundle waits while it is fetched.
    pub folder: PathBuf,
}

impl Forge {
    /// Why the forge can't start as it is given, if it can't: each branch
    /// start ends in `/`, so `agents/` never allows `agents-of-another`,
    /// and its folder and each repository are named whole, since git
    /// runs in each repository.
    #[must_use]
    pub fn unstartable(&self) -> Option<String> {
        if self.branches.is_empty() {
            return Some("the forge pushes no branch without a start".to_owned());
        }
        if let Some(start) = self.branches.iter().find(|start| {
            !start.ends_with('/')
                || !branch_allowed(&format!("{start}x"), std::slice::from_ref(*start))
        }) {
            return Some(format!(
                "a branch start ends in / and is a branch's name, so not {}",
                start.escape_debug()
            ));
        }
        let whole = std::iter::once(&self.folder).chain(self.repos.values());
        whole
            .into_iter()
            .find(|path| !path.is_absolute())
            .map(|path| {
                format!(
                    "{} isn't named from the root, as the forge's paths are",
                    path.display()
                )
            })
    }
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
        /// The commit the branch is at now, as signed.
        head: String,
        /// The commits it carried that its origin didn't hold, as signed,
        /// newest first.
        commits: Vec<String>,
        /// Each of those commits as the borrower made it, to the one the
        /// forge signed.
        #[serde(default)]
        signed: BTreeMap<String, String>,
        /// A bundle of the signed commits past the borrower's head, in
        /// base64, for it to take them back; none where nothing was
        /// signed.
        #[serde(default)]
        bundle: Option<String>,
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

/// Serves the lender's requests on `socket`, for as long as it runs: a
/// connection that fails as it is taken, as one does when the account
/// is out of files, is passed over, so one never ends the forge.
///
/// # Errors
///
/// When no connection could be taken for [`ACCEPTS_FAIL_FOR`], since a
/// socket that fails that long is broken, and only a fresh start mends
/// it.
pub async fn serve(socket: Socket, forge: Forge) -> io::Result<()> {
    let forge = Arc::new(forge);
    let mut failing_since = None;
    loop {
        match socket.accept().await {
            Ok(stream) => {
                failing_since = None;
                drop(tokio::spawn(connection(stream, forge.clone())));
            }
            Err(error) => {
                // Said once a run of failures begins, not each time.
                if failing_since.is_none() {
                    eprintln!("mycelium-forge: a connection couldn't be taken: {error}");
                }
                let since = *failing_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= ACCEPTS_FAIL_FOR {
                    return Err(io::Error::other(format!(
                        "no connection could be taken for {} seconds: {error}",
                        ACCEPTS_FAIL_FOR.as_secs()
                    )));
                }
                // Not at once, so a want of files doesn't spin.
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
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
            // Named twice, neither can be taken as the one the lender said.
            if borrower.replace(value.to_owned()).is_some() {
                return Err(refused(400, "the machine asking is named twice"));
            }
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

/// `payload`, a commit as git asks one signed, with the forge's signature
/// in it.
async fn signed(forge: &Forge, payload: &[u8]) -> Result<Vec<u8>, String> {
    let signature = run(
        tokio::process::Command::new(&forge.ssh_keygen)
            .args(["-Y", "sign", "-n", "git", "-U", "-f"])
            .arg(&forge.key)
            .env_clear()
            .env("SSH_AUTH_SOCK", &forge.agent),
        Some(payload),
        SIGN_WITHIN,
    )
    .await
    .map_err(|why| format!("a commit couldn't be signed: {why}"))?;
    let signature = String::from_utf8(signature)
        .ok()
        .filter(|signature| signature.starts_with("-----BEGIN SSH SIGNATURE-----\n"))
        .ok_or_else(|| "the signing key's agent said what isn't a signature".to_owned())?;
    Ok(with_signature(payload, &signature))
}

/// The commit `was`, as git keeps it, ready to sign: any signature it had
/// taken out, and each parent the forge signed in this push, as `signed`
/// maps them, named as signed. None where it isn't a commit the forge
/// signs ([`commit_like`]).
fn unsigned(was: &[u8], signed: &BTreeMap<String, String>) -> Option<Vec<u8>> {
    let at = was
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .map_or(was.len(), |end| end + 1);
    let header = std::str::from_utf8(&was[..at]).ok()?;
    let mut payload = Vec::with_capacity(was.len());
    let mut dropping = false;
    for line in header.split_inclusive('\n') {
        // A signature's lines past its first each begin with a space.
        if dropping && line.starts_with(' ') {
            continue;
        }
        dropping = line.starts_with("gpgsig ") || line.starts_with("gpgsig-sha256 ");
        if dropping {
            continue;
        }
        match line.strip_prefix("parent ").map(str::trim_end) {
            Some(parent) => {
                let parent = signed.get(parent).map_or(parent, String::as_str);
                payload.extend_from_slice(format!("parent {parent}\n").as_bytes());
            }
            None => payload.extend_from_slice(line.as_bytes()),
        }
    }
    payload.extend_from_slice(&was[at..]);
    (payload.len() <= MOST_PAYLOAD && commit_like(&payload)).then_some(payload)
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
        unlocked(last).chars().take(300).collect::<String>(),
        ran.status
    ))
}

/// `said` with whatever a URL in it carries before its host's `@` taken
/// out, since git names a remote's URL whole, a token in it included,
/// and what git said goes back to the borrower.
fn unlocked(said: &str) -> String {
    let mut kept = String::with_capacity(said.len());
    let mut rest = said;
    while let Some(at) = rest.find("://") {
        let (before, after) = rest.split_at(at + 3);
        kept.push_str(before);
        let host_ends = after
            .find(|letter: char| letter == '/' || letter.is_whitespace() || letter == '\'')
            .unwrap_or(after.len());
        match after[..host_ends].rfind('@') {
            Some(user) => {
                kept.push_str("***@");
                rest = &after[user + 1..];
            }
            None => rest = after,
        }
    }
    kept.push_str(rest);
    kept
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
            // As git names objects, so each is found as git lists it.
            needs.push(needed.to_ascii_lowercase());
        } else {
            let head = line.split(' ').next().unwrap_or_default();
            if !id(head) {
                return Err("holds a ref that isn't a commit");
            }
            heads.push(head.to_ascii_lowercase());
        }
    }
    heads.dedup();
    match heads.as_slice() {
        [head] => Ok((head.clone(), needs)),
        [] => Err("holds no commit to push"),
        _ => Err("holds more than one commit to push"),
    }
}

/// Each push's number, so the bundles and refs of two never share a
/// name.
static PUSHES: AtomicU64 = AtomicU64::new(0);

/// The forge's turn to push, which one push holds at a time, from its
/// fetch until it is tidied: two in a clone at once would fetch, prune
/// and name refs under each other.
static TURN: LazyLock<Arc<tokio::sync::Mutex<()>>> = LazyLock::new(Arc::default);

/// How many pushes the forge holds now, pushing or waiting their turn.
static PUSHING: AtomicUsize = AtomicUsize::new(0);

/// One push the forge holds, counted in [`PUSHING`] until it is done.
struct Held;

impl Held {
    /// A push held, unless the forge holds [`MOST_PUSHING`] already.
    fn taken() -> Option<Self> {
        PUSHING
            .try_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                (held < MOST_PUSHING).then_some(held + 1)
            })
            .ok()
            .map(|_| Self)
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        PUSHING.fetch_sub(1, Ordering::AcqRel);
    }
}

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
    // The turn is kept to the end of the push, so it counts until done.
    let turn = Held::taken();
    let checked = pushing
        .map_err(|_| refused(400, "the request isn't a branch to push"))
        .and_then(|pushing| checked(forge, pushing))
        .and_then(|checked| {
            if turn.is_none() {
                return Err(refused(
                    503,
                    format!("the forge holds {MOST_PUSHING} pushes already; ask again soon"),
                ));
            }
            Ok(checked)
        });
    let (repo, pushing, bundle, head) = match checked {
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
    let going = pushed(forge, &repo, borrower, &pushing.branch, &bundle, &head);
    tokio::pin!(going);
    let ended = loop {
        tokio::select! {
            ended = &mut going => break ended,
            () = tokio::time::sleep(STILL_EVERY) => {
                let seconds = began.elapsed().as_secs();
                if said(writing, &Pushed::Waiting { seconds }).await.is_err() {
                    // The lender let go, so the push is dropped, and git
                    // with it.
                    line["outcome"] = json!(
                        "let go: its borrower stopped reading, so a push git had begun \
                         may still have landed"
                    );
                    eprintln!("mycelium-forge: {line}");
                    return;
                }
            }
        }
    };
    match &ended {
        Pushed::Pushed {
            head,
            commits,
            signed,
            ..
        } => {
            line["pushed"] = json!(head);
            line["commits"] = json!(commits);
            line["signed"] = json!(signed);
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
/// bundle and the commit it carries.
type Checked = (PathBuf, Pushing, Vec<u8>, String);

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
    let (head, _) = bundled(&bundle).map_err(|why| refused(400, format!("the bundle {why}")))?;
    Ok((repo, pushing, bundle, head))
}

/// What a push leaves to let go however it ends, the borrower letting go
/// partway included: its bundle, and the refs it fetched and signed to.
/// It holds the forge's turn until they are let go.
struct Tidy {
    git: PathBuf,
    repo: PathBuf,
    file: PathBuf,
    fetched: String,
    signed: String,
    /// The forge's turn, until tidying takes it.
    turn: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl Tidy {
    /// The tidying, to run where blocking holds up no other push, which
    /// gives the turn back once it is done.
    fn tidying(&mut self) -> impl FnOnce() + Send + 'static {
        let (git, repo, file) = (self.git.clone(), self.repo.clone(), self.file.clone());
        let names = [self.fetched.clone(), self.signed.clone()];
        let turn = self.turn.take();
        move || {
            let _ = fs::remove_file(&file);
            for name in &names {
                // A ref never made is no matter.
                let mut deleting = std::process::Command::new(&git);
                deleting
                    .arg("-C")
                    .arg(&repo)
                    .args(["update-ref", "-d", name])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                ran_within(&mut deleting, TIDY_WITHIN);
            }
            drop(turn);
        }
    }

    /// Tidies, waiting until it is done, as a push that ended does
    /// before it answers.
    async fn done(mut self) {
        let _ = tokio::task::spawn_blocking(self.tidying()).await;
    }
}

impl Drop for Tidy {
    fn drop(&mut self) {
        // Tidied already, as a push that ended is.
        if self.turn.is_none() {
            return;
        }
        // A push let go partway is tidied after, off the async workers.
        let tidying = self.tidying();
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn_blocking(tidying)),
            Err(_) => tidying(),
        }
    }
}

/// Fetches `bundle` into `repo`, signs each commit to `head` its origin
/// doesn't hold, and pushes the signed chain to `branch` on its origin,
/// never forced: how it ended.
async fn pushed(
    forge: &Forge,
    repo: &Path,
    borrower: &str,
    branch: &str,
    bundle: &[u8],
    head: &str,
) -> Pushed {
    let turn = Arc::clone(&TURN).lock_owned().await;
    let count = PUSHES.fetch_add(1, Ordering::Relaxed);
    let named = format!("{borrower}/{}-{count}", std::process::id());
    let tidy = Tidy {
        git: forge.git.clone(),
        repo: repo.to_owned(),
        file: forge
            .folder
            .join(format!("{borrower}-{}-{count}.bundle", std::process::id())),
        fetched: format!("{FETCHED}/{named}"),
        signed: format!("{FETCHED}/{named}-signed"),
        turn: Some(turn),
    };
    let ended = carried(forge, &tidy, branch, bundle, head)
        .await
        .unwrap_or_else(|why| Pushed::Refused { why });
    tidy.done().await;
    ended
}

/// Runs `command` to its end, or ends it once it has run `within`,
/// blocking until either.
fn ran_within(command: &mut std::process::Command, within: Duration) {
    let Ok(mut child) = command.spawn() else {
        return;
    };
    let ends = Instant::now() + within;
    while matches!(child.try_wait(), Ok(None)) {
        if Instant::now() >= ends {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `git` in `repo`, never asking anyone anything.
fn git(forge: &Forge, repo: &Path) -> tokio::process::Command {
    let mut git = tokio::process::Command::new(&forge.git);
    git.arg("-C")
        .arg(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("SSH_ASKPASS_REQUIRE", "never");
    git
}

/// The lines `listed` holds, each a word.
fn listed(listed: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(listed)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The steps of [`pushed`], up to its tidying, which `tidy` does.
async fn carried(
    forge: &Forge,
    tidy: &Tidy,
    branch: &str,
    bundle: &[u8],
    head: &str,
) -> Result<Pushed, String> {
    let repo = tidy.repo.as_path();
    kept(&forge.folder, &tidy.file, bundle)
        .map_err(|error| format!("the bundle couldn't be kept: {error}"))?;
    // What its origin holds now is what the forge needn't sign.
    run(
        git(forge, repo).args([
            "fetch",
            "--quiet",
            "--no-tags",
            "--prune",
            "origin",
            "+refs/heads/*:refs/remotes/origin/*",
        ]),
        None,
        FETCH_WITHIN,
    )
    .await
    .map_err(|why| format!("the origin couldn't be fetched: {why}"))?;
    run(
        git(forge, repo)
            .args([
                "-c",
                "fetch.fsckObjects=true",
                "fetch",
                "--quiet",
                "--no-tags",
            ])
            .arg("--no-write-fetch-head")
            .arg(&tidy.file)
            .arg(format!("{head}:{}", tidy.fetched)),
        None,
        FETCH_WITHIN,
    )
    .await
    .map_err(|why| format!("the bundle couldn't be fetched: {why}"))?;
    // Only a commit is pushed, so nothing is signed for a head that would
    // be refused at the push, such as a tag's.
    let kind = run(
        git(forge, repo).args(["cat-file", "-t", head]),
        None,
        FETCH_WITHIN,
    )
    .await
    .map_err(|why| format!("the bundle's head couldn't be read: {why}"))?;
    if kind.trim_ascii() != b"commit" {
        return Err("the bundle's head isn't a commit".to_owned());
    }
    let mut listing = git(forge, repo);
    listing
        .args(["rev-list", "--reverse", "--topo-order"])
        .arg(format!("--max-count={}", MOST_SIGNED + 1))
        .args([head, "--not", "--remotes=origin"]);
    let unsigned_ones = listed(
        &run(&mut listing, None, FETCH_WITHIN)
            .await
            .map_err(|why| format!("the bundle's commits couldn't be listed: {why}"))?,
    );
    if unsigned_ones.len() > MOST_SIGNED {
        return Err(format!(
            "the forge signs at most {MOST_SIGNED} commits a push"
        ));
    }
    let signed = signed_all(forge, repo, &unsigned_ones).await?;
    let now = signed.get(head).map_or(head, String::as_str).to_owned();
    // The borrower takes back what was signed, past what it holds.
    let back = if now == head {
        None
    } else {
        run(
            git(forge, repo).args(["update-ref", &tidy.signed, &now]),
            None,
            FETCH_WITHIN,
        )
        .await
        .map_err(|why| format!("the signed commits couldn't be named: {why}"))?;
        let made = run(
            git(forge, repo)
                .args(["bundle", "create", "--quiet", "-", &tidy.signed])
                .arg(format!("^{head}")),
            None,
            FETCH_WITHIN,
        )
        .await
        .map_err(|why| format!("the signed commits couldn't be bundled: {why}"))?;
        Some(STANDARD.encode(made))
    };
    run(
        git(forge, repo)
            // The repository's own hooks run, so its gates hold.
            .args(["push", "--quiet", "origin"])
            .arg(format!("{now}:refs/heads/{branch}")),
        None,
        PUSH_WITHIN,
    )
    .await
    .map_err(|why| format!("the push was refused: {why}"))?;
    Ok(Pushed::Pushed {
        commits: unsigned_ones
            .iter()
            .rev()
            .filter_map(|was| signed.get(was).cloned())
            .collect(),
        head: now,
        signed,
        bundle: back,
    })
}

/// Signs each of `unsigned_ones` in `repo`, oldest first, each pointing at
/// its parents as signed: each as it came, to the commit signed.
async fn signed_all(
    forge: &Forge,
    repo: &Path,
    unsigned_ones: &[String],
) -> Result<BTreeMap<String, String>, String> {
    let mut signed = BTreeMap::new();
    for was in unsigned_ones {
        let read = run(
            git(forge, repo).args(["cat-file", "commit", was]),
            None,
            FETCH_WITHIN,
        )
        .await
        .map_err(|why| format!("{was} couldn't be read: {why}"))?;
        let payload = unsigned(&read, &signed).ok_or_else(|| {
            format!(
                "{was} isn't a commit the forge signs: one of SHA-1 ids, at most \
                 {MOST_PAYLOAD} bytes, saying who committed it"
            )
        })?;
        let object = self::signed(forge, &payload).await?;
        let now = run(
            git(forge, repo).args(["hash-object", "-t", "commit", "-w", "--stdin"]),
            Some(&object),
            SIGN_WITHIN,
        )
        .await
        .map_err(|why| format!("{was} signed isn't a commit git takes: {why}"))?;
        signed.insert(was.clone(), String::from_utf8_lossy(&now).trim().to_owned());
    }
    Ok(signed)
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
