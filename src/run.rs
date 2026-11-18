//! Running a command on this machine for someone waiting on it, here or on
//! another machine of the realm: its output is handed on as it comes, a
//! bounded piece at a time, so a command writing faster than its reader
//! takes is slowed by its own pipes rather than growing the node, and its
//! exit comes last. A reader that lets go stops it, and everything it
//! started, with the terminate signal, and kills what outlives that by
//! [`GRACE`]. Each run is kept in a record of runs, a line as it starts
//! and a line as it ends, on each machine it crosses.

use std::io::{self, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rustix::process::{Pid, Signal};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;

use crate::wire::{By, Ran};

/// How long what a run started has to end once asked to stop, before it
/// is killed.
pub const GRACE: Duration = Duration::from_secs(10);

/// What a run is told names the session proved to ask for it.
const SESSION: &str = "PI_SESSION_ID";

/// How often a stopped run is looked at while it has yet to end.
const LOOKS_EVERY: Duration = Duration::from_millis(100);

/// Where a node keeps its record of runs, and the accounts whose files may
/// say how a fenced command runs (`declared::launcher`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Given {
    /// The record of runs, a JSON line each as a run starts and ends.
    pub journal: PathBuf,
    /// The accounts trusted to declare a fence's launcher: root alone, as
    /// the node's command sets it, so nothing an agent can write does.
    pub declarers: Vec<u32>,
    /// Where the work broker cuts its trees and records who holds each,
    /// which says which tree a run from another machine stands in.
    pub trees: crate::node::Trees,
}

impl Given {
    /// What a node keeping its state in `state` is given, knowing no work
    /// trees.
    #[must_use]
    pub fn at(state: &Path) -> Self {
        Self {
            journal: state.join("runs.jsonl"),
            declarers: vec![0],
            trees: crate::node::Trees {
                root: PathBuf::from("/nowhere/trees"),
                records: PathBuf::from("/nowhere/cut"),
                sessions: PathBuf::from("/nowhere/sessions"),
            },
        }
    }

    /// What it is given, knowing the work broker's `trees`.
    #[must_use]
    pub fn knowing(self, trees: crate::node::Trees) -> Self {
        Self { trees, ..self }
    }
}

impl Default for Given {
    fn default() -> Self {
        Self::at(Path::new("/nowhere"))
    }
}

/// Keeps `line` in the record of runs at `journal`, and tells the
/// machine's journal of it too.
pub fn journal(journal: &Path, line: &Value) {
    eprintln!("run: {line}");
    let kept = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal)
        // One write a line, so two runs kept at once never interleave.
        .and_then(|mut kept| kept.write_all(format!("{line}\n").as_bytes()));
    if let Err(error) = kept {
        eprintln!("the run isn't kept in {}: {error}", journal.display());
    }
}

/// Hands on what run `id` says, from `said`, and keeps how it ended in the
/// record of runs at `file`: as it said, or stopped once its reader let go.
#[must_use]
pub fn journaled(file: PathBuf, id: String, mut said: mpsc::Receiver<Ran>) -> mpsc::Receiver<Ran> {
    let (ran, heard) = mpsc::channel(BACKLOG);
    tokio::spawn(async move {
        let hung_up = json!({"Stopped": "its asker let go"});
        let ended = loop {
            let next = tokio::select! {
                next = said.recv() => next.unwrap_or_else(|| {
                    Ran::Failed("the run ended without saying how".to_owned())
                }),
                () = ran.closed() => break hung_up,
            };
            let ends = next.ends().then(|| json!(next));
            if ran.send(next).await.is_err() {
                break hung_up;
            }
            if let Some(ended) = ends {
                break ended;
            }
        };
        let at = jiff::Timestamp::now().to_string();
        journal(&file, &json!({"run": id, "at": at, "ended": ended}));
    });
    heard
}

/// The most of a command's output one [`Ran`] carries.
pub const PIECE: usize = 64 * 1024;

/// How many [`Ran`]s wait between a run and its reader at most, at each
/// step from the command to whoever asked for it.
pub const BACKLOG: usize = 4;

/// The words that run `command` here in `realm`: through `launcher`, a
/// fence's words or none for a person, then the person's login shell
/// with the realm's `kit` first on its path where there is one, then
/// the command. What an ask says comes only after both, and a
/// launcher's words end with `--` ([`crate::declared::launcher`] takes
/// no other), so none of it is read as the launcher's own options where
/// the launcher reads `--` as their end, as pi-fence does: no ask spares
/// itself a shared tree.
#[must_use]
pub fn words(
    launcher: Vec<String>,
    realm: &str,
    kit: Option<&Path>,
    command: Vec<String>,
) -> Vec<String> {
    let mut words = launcher;
    words.extend(crate::hold::start::login_run(realm, kit));
    words.extend(command);
    words
}

/// Where a run starts.
#[derive(Debug)]
pub enum Folder {
    /// The folder at a path, entered by it.
    At(PathBuf),
    /// A folder the node opened to judge where the run stands, entered as
    /// it is opened, wherever its path now leads.
    Opened(std::os::fd::OwnedFd),
}

impl From<PathBuf> for Folder {
    fn from(path: PathBuf) -> Self {
        Self::At(path)
    }
}

/// Runs `words`, the first the program, in `folder`, as `session` where
/// the node proved that session asks, signing as `by` asks, handing what
/// it says to `ran` as it says it and then how it ended; stops it once
/// `ran`'s reader lets go.
pub async fn here(
    words: Vec<String>,
    folder: impl Into<Folder>,
    session: Option<String>,
    by: By,
    ran: mpsc::Sender<Ran>,
) {
    let ended = match command(&words, folder.into(), session.as_deref(), by)
        .and_then(|mut run| run.spawn())
    {
        Ok(child) => follow(child, &ran).await,
        Err(error) => Some(Ran::Failed(format!("it didn't start: {error}"))),
    };
    if let Some(ended) = ended {
        // A reader that let go meanwhile hears nothing.
        let _ = ran.send(ended).await;
    }
}

/// `words` to start in a process group of its own, so a stop reaches all
/// it starts, with nothing on its standard input. Its `PI_SESSION_ID`,
/// which a fence's launcher reads to know whose work trees the run may
/// stand in, is `session` alone, never the node's own. An agent's run
/// signs over ssh as a held session does, through the agents' proxy
/// ([`crate::hold::holder::agents`]), named again after the login
/// shell's profile [`words`] puts it through, whether or not its session
/// was proved and wherever it was asked from; only a person's run keeps
/// the person's agent.
fn command(
    words: &[String],
    folder: Folder,
    session: Option<&str>,
    by: By,
) -> io::Result<tokio::process::Command> {
    let (program, arguments) = words
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "nothing to run"))?;
    let mut command = tokio::process::Command::new(program);
    match folder {
        Folder::At(path) => {
            command.current_dir(path);
        }
        Folder::Opened(opened) => {
            // The child enters the folder by what was opened, between its
            // fork and its exec, so a link swapped since the node judged
            // the folder leads it nowhere else. fchdir is one system call,
            // safe there: it takes no lock and allocates nothing.
            #[expect(
                unsafe_code,
                reason = "a child enters an opened folder only between fork and exec"
            )]
            unsafe {
                command.pre_exec(move || rustix::process::fchdir(&opened).map_err(io::Error::from));
            }
        }
    }
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    match session {
        Some(session) => command.env(SESSION, session),
        None => command.env_remove(SESSION),
    };
    if let By::Agent(_) = by {
        let proxy = crate::hold::holder::agents_proxy_here();
        for (variable, value) in crate::hold::holder::agents(proxy.as_deref(), true) {
            match value {
                Some(value) => command.env(variable, value),
                None => command.env_remove(variable),
            };
        }
    }
    Ok(command)
}

/// Hands on what `child` says until it ends, and answers how it did, or
/// nothing once `ran`'s reader has let go and it is stopped.
async fn follow(mut child: tokio::process::Child, ran: &mpsc::Sender<Ran>) -> Option<Ran> {
    let group = Group(
        child
            .id()
            .and_then(|group| i32::try_from(group).ok())
            .and_then(Pid::from_raw),
    );
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    let (mut out_piece, mut err_piece) = (vec![0; PIECE], vec![0; PIECE]);
    while out.is_some() || err.is_some() {
        let said = tokio::select! {
            read = read_some(&mut out, &mut out_piece) => {
                let Some(read) = read else { out = None; continue };
                Ran::Out(STANDARD.encode(read))
            },
            read = read_some(&mut err, &mut err_piece) => {
                let Some(read) = read else { err = None; continue };
                Ran::Err(STANDARD.encode(read))
            },
            () = ran.closed() => return stop(group, child).await,
        };
        if ran.send(said).await.is_err() {
            return stop(group, child).await;
        }
    }
    tokio::select! {
        status = child.wait() => {
            group.ended();
            Some(match status {
                Ok(status) => Ran::Exited(code(status)),
                Err(error) => Ran::Failed(format!("its end couldn't be told: {error}")),
            })
        }
        () = ran.closed() => stop(group, child).await,
    }
}

/// The process group a run leads, stopped once this is dropped, unless
/// the run ended on its own: so a run whose task is cut short leaves
/// nothing running.
struct Group(Option<Pid>);

impl Group {
    /// The run ended on its own, so what it left behind is left be.
    fn ended(mut self) {
        self.0 = None;
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        let Some(group) = self.0 else { return };
        // A group that has already ended has nothing to stop.
        let _ = rustix::process::kill_process_group(group, Signal::TERM);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            let killed_at = tokio::time::Instant::now() + GRACE;
            // Looked at only while it lasts, so a group id used again
            // long after is never killed.
            while rustix::process::test_kill_process_group(group).is_ok() {
                if tokio::time::Instant::now() >= killed_at {
                    let _ = rustix::process::kill_process_group(group, Signal::KILL);
                    return;
                }
                tokio::time::sleep(LOOKS_EVERY).await;
            }
        });
    }
}

/// What a pipe says next, or nothing once it has closed or there is none.
async fn read_some<'a>(
    pipe: &mut Option<impl AsyncRead + Unpin>,
    piece: &'a mut [u8],
) -> Option<&'a [u8]> {
    let Some(pipe) = pipe else {
        return std::future::pending().await;
    };
    match pipe.read(piece).await {
        Ok(0) | Err(_) => None,
        Ok(read) => Some(&piece[..read]),
    }
}

/// Stops `group`, which `child` leads, and waits for `child` to end so it
/// leaves no zombie.
async fn stop(group: Group, mut child: tokio::process::Child) -> Option<Ran> {
    drop(group);
    let _ = child.wait().await;
    None
}

/// The exit code a shell would say for `status`: its own, or 128 and the
/// signal that ended it.
fn code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(255)
}

/// The bytes a piece of output carries.
///
/// # Errors
///
/// When it isn't base64.
pub fn bytes(piece: &str) -> Result<Vec<u8>, base64::DecodeError> {
    STANDARD.decode(piece)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How long a test waits on a run before failing.
    const PATIENCE: Duration = Duration::from_secs(10);

    #[test]
    fn an_ask_never_says_the_fences_words() {
        let launcher: Vec<String> = ["/nix/store/pi-fence/bin/pi-fence", "--kind", "strict", "--"]
            .map(str::to_owned)
            .to_vec();
        let asked: Vec<String> = [
            "--shared-tree",
            "--folder",
            "/home/kestrel/trees/another-quests",
            "--",
            "sh",
            "-c",
            "true",
        ]
        .map(str::to_owned)
        .to_vec();
        let words = words(launcher.clone(), "work", None, asked.clone());
        let options = words
            .iter()
            .position(|word| word == "--")
            .map(|at| &words[..=at]);
        assert_eq!(
            options,
            Some(&launcher[..]),
            "the fence's options are its own: {words:?}"
        );
        assert!(
            words.ends_with(&asked),
            "the ask runs as it was said, after the fence and the shell: {words:?}"
        );
        assert_eq!(
            words[launcher.len()..words.len() - asked.len()],
            crate::hold::start::login_run("work", None),
            "the login shell stands between them"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn an_agents_run_is_told_the_agents_proxy_and_a_persons_keeps_its_own() {
        let told = |session, by| {
            let run = command(&sh("true"), std::env::temp_dir().into(), session, by)
                .expect("the run is made");
            run.as_std()
                .get_envs()
                .map(|(name, value)| {
                    let shown = |said: &std::ffi::OsStr| said.to_string_lossy().into_owned();
                    (shown(name), value.map(shown))
                })
                .filter(|(name, _)| name.contains("AGENT") || name.contains("AUTH_SOCK"))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        // Worked out here, apart from the lookup under test: the proxy's
        // socket in the node's own runtime folder, none where it has none.
        let proxy = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .filter(|runtime| !runtime.is_empty())
            .map(|runtime| format!("{runtime}/pi-agent.sock"));
        let agents = std::collections::BTreeMap::from([
            (
                "MYCELIUM_AGENT".to_owned(),
                Some(proxy.clone().unwrap_or_default()),
            ),
            ("SSH_AUTH_SOCK".to_owned(), proxy),
            ("SSH_TPM_AUTH_SOCK".to_owned(), None),
        ]);
        let agent = By::Agent(crate::declared::Kind::Fence);
        assert_eq!(
            told(Some("s-harbour"), agent),
            agents,
            "a session's run is told the proxy, again for after the profile, and no other"
        );
        assert_eq!(
            told(None, agent),
            agents,
            "so is an agent's run with no session proved, or one asked from elsewhere"
        );
        assert_eq!(
            told(None, By::Person),
            std::collections::BTreeMap::new(),
            "a person's run is left as the person's session has it"
        );
    }

    fn sh(script: &str) -> Vec<String> {
        vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()]
    }

    /// Everything a run says, its output and errors joined, and its end.
    async fn all_of(words: Vec<String>) -> (Vec<u8>, Vec<u8>, Ran) {
        let (ran, mut said) = mpsc::channel(BACKLOG);
        let folder = std::env::temp_dir();
        tokio::spawn(here(words, folder, None, By::Person, ran));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        loop {
            let next = tokio::time::timeout(PATIENCE, said.recv())
                .await
                .expect("the run says more")
                .expect("the run ends with its end");
            match next {
                Ran::Out(piece) => out.extend(bytes(&piece).expect("base64")),
                Ran::Err(piece) => err.extend(bytes(&piece).expect("base64")),
                ended => return (out, err, ended),
            }
        }
    }

    #[tokio::test]
    async fn a_run_hands_on_its_output_and_errors_apart_then_its_own_exit_code() {
        let (out, err, ended) = all_of(sh("echo said; echo wrong >&2; exit 3")).await;
        assert_eq!(out, b"said\n");
        assert_eq!(err, b"wrong\n");
        assert_eq!(ended, Ran::Exited(3), "its exit code is its own");
    }

    #[tokio::test]
    async fn a_run_reads_nothing_and_a_signal_that_ends_it_says_so_as_a_shell_would() {
        let (out, _, ended) = all_of(sh("cat; echo read")).await;
        assert_eq!(out, b"read\n", "its input is empty, so it reads nothing");
        assert_eq!(ended, Ran::Exited(0));
        let (_, _, killed) = all_of(sh("kill -KILL $$")).await;
        assert_eq!(killed, Ran::Exited(128 + 9));
    }

    /// What a run's own environment says `PI_SESSION_ID` is, as `session`
    /// asks it, "none" where it's unset.
    async fn its_session(session: Option<&str>) -> Vec<u8> {
        let (ran, mut said) = mpsc::channel(BACKLOG);
        tokio::spawn(here(
            sh("printf %s \"${PI_SESSION_ID-none}\""),
            std::env::temp_dir(),
            session.map(str::to_owned),
            By::Person,
            ran,
        ));
        let mut out = Vec::new();
        loop {
            match tokio::time::timeout(PATIENCE, said.recv())
                .await
                .expect("the run says more")
                .expect("the run ends with its end")
            {
                Ran::Out(piece) => out.extend(bytes(&piece).expect("base64")),
                Ran::Exited(0) => return out,
                other => panic!("it says its session and ends: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_run_is_told_the_session_proved_to_ask_and_no_other() {
        assert_eq!(
            its_session(Some("s-proved")).await,
            b"s-proved",
            "the session proved to ask is the run's own"
        );
        assert_eq!(
            its_session(None).await,
            b"none",
            "where none was proved, the run names none"
        );
    }

    #[test]
    fn a_run_never_inherits_the_nodes_own_session() {
        let words = sh("true");
        let envs = |session| {
            let run = command(
                &words,
                Folder::At(std::env::temp_dir()),
                session,
                By::Person,
            )
            .expect("words to run");
            run.as_std()
                .get_envs()
                .filter(|(key, _)| *key == "PI_SESSION_ID")
                .map(|(_, value)| value.map(ToOwned::to_owned))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            envs(None),
            [None],
            "whatever session the node was started in, a run unproved is told none"
        );
        assert_eq!(
            envs(Some("s-proved")),
            [Some("s-proved".into())],
            "and one proved is told only its own"
        );
    }

    #[tokio::test]
    async fn a_program_that_isnt_there_says_it_didnt_start() {
        let (_, _, ended) = all_of(vec!["/nowhere/at/all".to_owned()]).await;
        assert!(
            matches!(&ended, Ran::Failed(why) if why.starts_with("it didn't start")),
            "{ended:?}"
        );
    }

    #[tokio::test]
    async fn a_run_in_an_opened_folder_starts_there_wherever_its_path_now_leads() {
        use rustix::fs::{Mode, OFlags};
        use std::fs;
        let dir = tempfile::tempdir().expect("a temporary folder");
        let judged = dir.path().join("judged");
        fs::create_dir(&judged).expect("made");
        let opened = rustix::fs::open(
            &judged,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .expect("opened");
        // Its path now leads elsewhere, as a swapped link would leave it.
        let moved = dir.path().join("moved");
        fs::rename(&judged, &moved).expect("moved");
        fs::create_dir(&judged).expect("made again");
        let (ran, mut said) = mpsc::channel(BACKLOG);
        tokio::spawn(here(
            sh("pwd -P"),
            Folder::Opened(opened),
            None,
            By::Person,
            ran,
        ));
        let mut out = Vec::new();
        loop {
            match tokio::time::timeout(PATIENCE, said.recv())
                .await
                .expect("the run says more")
                .expect("the run ends with its end")
            {
                Ran::Out(piece) => out.extend(bytes(&piece).expect("base64")),
                Ran::Exited(0) => break,
                other => panic!("it ran amiss: {other:?}"),
            }
        }
        assert_eq!(
            String::from_utf8_lossy(&out),
            format!("{}\n", moved.canonicalize().expect("it resolves").display()),
            "it started in the folder opened, not the one its path now names"
        );
    }

    #[tokio::test]
    async fn a_flood_of_output_comes_whole_in_bounded_pieces() {
        let (ran, mut said) = mpsc::channel(BACKLOG);
        tokio::spawn(here(
            sh("head -c 8388608 /dev/zero"),
            std::env::temp_dir(),
            None,
            By::Person,
            ran,
        ));
        let mut got = 0;
        loop {
            match tokio::time::timeout(PATIENCE, said.recv())
                .await
                .expect("the run says more")
                .expect("the run ends with its end")
            {
                Ran::Out(piece) => {
                    let piece = bytes(&piece).expect("base64");
                    assert!(piece.len() <= PIECE, "no piece is larger than {PIECE}");
                    got += piece.len();
                }
                Ran::Exited(code) => {
                    assert_eq!(code, 0);
                    break;
                }
                other => panic!("nothing else is said: {other:?}"),
            }
        }
        assert_eq!(got, 8 * 1024 * 1024, "all of it comes");
    }

    #[tokio::test]
    async fn a_reader_that_lets_go_stops_the_run_and_all_it_started() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let mark = folder.path().join("pid");
        let (ran, mut said) = mpsc::channel(BACKLOG);
        // A child of the shell, in its group, says its pid, then sleeps.
        let script = format!(
            "sleep 600 & echo $! > {}; echo started; wait",
            mark.display()
        );
        let run = tokio::spawn(here(
            sh(&script),
            folder.path().to_owned(),
            None,
            By::Person,
            ran,
        ));
        let first = tokio::time::timeout(PATIENCE, said.recv())
            .await
            .expect("the run says something");
        assert!(matches!(first, Some(Ran::Out(_))), "{first:?}");
        let pid: i32 = std::fs::read_to_string(&mark)
            .expect("its child said its pid")
            .trim()
            .parse()
            .expect("a pid");
        drop(said);
        tokio::time::timeout(PATIENCE, run)
            .await
            .expect("the run stops once its reader lets go")
            .expect("the run ends cleanly");
        let pid = Pid::from_raw(pid).expect("a real pid");
        tokio::time::timeout(PATIENCE, async {
            while rustix::process::test_kill_process(pid).is_ok() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("its child is stopped too");
    }

    /// Starts `script` in `folder`, whose child writes its pid to `mark`,
    /// and waits for it to say it started: its reader, its task and the
    /// child's pid.
    async fn started(
        script: &str,
        folder: &Path,
    ) -> (mpsc::Receiver<Ran>, tokio::task::JoinHandle<()>, Pid) {
        let mark = folder.join("pid");
        let script = script.replace("MARK", &mark.display().to_string());
        let (ran, mut said) = mpsc::channel(BACKLOG);
        let run = tokio::spawn(here(sh(&script), folder.to_owned(), None, By::Person, ran));
        let first = tokio::time::timeout(PATIENCE, said.recv())
            .await
            .expect("the run says something");
        assert!(matches!(first, Some(Ran::Out(_))), "{first:?}");
        let pid: i32 = std::fs::read_to_string(&mark)
            .expect("its child said its pid")
            .trim()
            .parse()
            .expect("a pid");
        (said, run, Pid::from_raw(pid).expect("a real pid"))
    }

    /// Waits up to `within` for `pid` to end.
    async fn ends(pid: Pid, within: Duration) {
        tokio::time::timeout(within, async {
            while rustix::process::test_kill_process(pid).is_ok() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("it ends in time");
    }

    #[tokio::test]
    async fn what_a_stop_doesnt_end_is_killed_once_its_grace_is_up() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        // Its child ignores the terminate signal, as the shell does.
        let (said, run, pid) = started(
            "trap '' TERM; sleep 600 & echo $! > MARK; echo started; wait",
            folder.path(),
        )
        .await;
        let asked = tokio::time::Instant::now();
        drop(said);
        ends(pid, GRACE + PATIENCE).await;
        assert!(
            asked.elapsed() >= GRACE.saturating_sub(Duration::from_secs(1)),
            "the terminate signal alone didn't end it"
        );
        tokio::time::timeout(PATIENCE, run)
            .await
            .expect("the run stops")
            .expect("the run ends cleanly");
    }

    #[tokio::test]
    async fn a_run_cut_short_stops_all_it_started() {
        let folder = tempfile::tempdir().expect("a temporary folder");
        let (_said, run, pid) = started(
            "sleep 600 & echo $! > MARK; echo started; wait",
            folder.path(),
        )
        .await;
        run.abort();
        ends(pid, PATIENCE).await;
    }
}
