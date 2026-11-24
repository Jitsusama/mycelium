//! A borrower's push, on the lending side: its bundle checked, fetched
//! into the repository's clone in the forge's turn, each commit its
//! origin doesn't hold signed, the signed chain pushed, and what it left
//! tidied however it ended.

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
use serde_json::json;
use tokio::io::{AsyncWrite, AsyncWriteExt};

use super::{
    FETCHED, Forge, MOST_PAYLOAD, Pushed, Pushing, Refusal, SIGN_WITHIN, answer, branch_allowed,
    bundled, refused, run, unsigned,
};

/// The most commits one push signs.
const MOST_SIGNED: usize = 500;

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
        // A loop of its own rather than `try_update`, which is newer than
        // the crate's oldest Rust, or `fetch_update`, which is deprecated.
        let mut held = PUSHING.load(Ordering::Acquire);
        while held < MOST_PUSHING {
            match PUSHING.compare_exchange_weak(held, held + 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return Some(Self),
                Err(now) => held = now,
            }
        }
        None
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
pub(super) async fn push<W: AsyncWrite + Unpin>(
    forge: &Forge,
    borrower: &str,
    body: &[u8],
    writing: &mut W,
) {
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
pub(super) fn ran_within(command: &mut std::process::Command, within: Duration) {
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
        let object = super::signed(forge, &payload).await?;
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
