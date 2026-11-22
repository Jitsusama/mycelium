//! A machine made for a start that may cost money, or for any start where
//! the offer costs nothing: skiff's node asks a provider's program what it
//! offers, makes the one machine that would take the work, links it through
//! the program, starts the work there, and lets it go at its time, or once
//! it has handed over after its work, keeping each make and release in its
//! book. An offer that costs is never made for a start that didn't say it
//! may cost. Nothing of the asker's environment, or its node's, reaches
//! the work there, as nothing of a carried session's does. A node started
//! again links each machine its book says is still made. Each
//! node, and the made machine's end of the link, is a `mycelium` process of
//! its own, each machine in folders of its own.

use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};

/// How long anything here has to happen: each step takes a second or two.
const PATIENCE: Duration = Duration::from_secs(60);

/// How long a made machine has to hand over once its work ends: the far
/// end says so at its keep, a minute after the link opens.
const HANDS_OVER: Duration = Duration::from_secs(90);

/// How long the made machine lives, from when it is made.
const LIVES: jiff::SignedDuration = jiff::SignedDuration::from_secs(20);

/// A process that is stopped when the test is done with it.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        // One that ended already has nothing to stop.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `mycelium`, run as the machine whose folders are all under `home`.
fn mycelium(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mycelium"));
    command
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", home.join("r"))
        .env("XDG_CONFIG_HOME", home.join("c"))
        .env("XDG_STATE_HOME", home.join("s"))
        .env("XDG_DATA_HOME", home.join("d"))
        .env("XDG_CACHE_HOME", home.join("k"))
        // Under a systemd unit, a node would start sessions in scopes.
        .env_remove("INVOCATION_ID")
        .stdin(Stdio::null());
    command
}

/// Every fence, each shut but for no door, as a machine declares it.
const SHUT: &str = r#"{"none": {"builders": [], "forges": [], "secrets": []}, "fence": {"builders": [], "forges": [], "secrets": []}, "strict": {"builders": [], "forges": [], "secrets": []}}"#;

/// The folders of a machine at `home`, its runtime folder open to it
/// alone, declaring `declared`, or nothing it reaches where that is none,
/// and either way every fence [`SHUT`]. Where the node can't tell who
/// asks, as on macOS or where the tests run with no new privileges, it
/// takes the asker for a strictly fenced agent, which is offered only
/// ground as firm as here's, so each machine runs its fences as shut as
/// the others'.
fn machine(home: &Path, declared: Option<&str>) {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home.join("r"))
        .expect("its runtime folder is made");
    let mut said: serde_json::Value = serde_json::from_str(
        declared.unwrap_or(r#"{"reach": [], "outlives": [], "costs_money": false, "taints": []}"#),
    )
    .expect("what it declares is JSON");
    said["fence"] = serde_json::from_str(SHUT).expect("the fences are JSON");
    let config = home.join("c").join("mycelium");
    fs::create_dir_all(&config).expect("its configuration's folder is made");
    fs::write(config.join("declared.json"), said.to_string()).expect("what it declares is written");
}

/// Waits up to `within` until `holds` says `path` is as wanted, hearing
/// each change under `root`, and answers what it holds.
fn heard(root: &Path, path: &Path, within: Duration, holds: impl Fn(&str) -> bool) -> String {
    let (telling, told) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(telling).expect("a watcher");
    watcher
        .watch(root, RecursiveMode::Recursive)
        .expect("the folder is watched");
    let deadline = Instant::now() + within;
    loop {
        // A socket can't be read, so its being there is all it says. A
        // file counts only as what was read: one looked at after a failed
        // read may have come to be in between, its text not yet known.
        let read = match fs::metadata(path) {
            Ok(found) if !found.is_file() => Ok(String::new()),
            _ => fs::read_to_string(path),
        };
        if let Ok(text) = read.as_deref()
            && holds(text)
        {
            return text.to_owned();
        }
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "{} never came to be as wanted: {read:?}",
            path.display()
        );
        // A watch that fails or times out has the path looked at again.
        let _ = told.recv_timeout(left);
    }
}

/// Every fence of a machine a provider offers, each shut but for no door,
/// and each naming how a command runs in it. Where the node takes the
/// asker for a strictly fenced agent, as on macOS, a machine is made for
/// it only where its offer says how its strict fence runs one.
const OFFERED: &str = r#"{"none": {"builders": [], "forges": [], "secrets": []}, "fence": {"builders": [], "forges": [], "secrets": [], "runs": ["/nix/store/0000-fence/bin/fence", "--"]}, "strict": {"builders": [], "forges": [], "secrets": [], "runs": ["/nix/store/0000-fence/bin/fence", "--"]}}"#;

/// What the machine `provider` offers costs an hour.
const COSTS: &str = "0.003";

/// What a machine that costs nothing costs an hour, as a provider may
/// write it.
const FREE: &str = "0.000";

/// A provider's program at `root`, offering one shape of machine that
/// reaches `lent-net` at `hourly` an hour, making it as `lent` at `lent`'s
/// folders until `until`, linking it there, saying in `linked` which node
/// asked each time, and saying what it released.
fn provider(root: &Path, lent: &Path, until: jiff::Timestamp, hourly: &str) -> PathBuf {
    let offer = format!(
        r#"{{"shape": "small", "facts": {{"declared": {{"reach": ["lent-net"], "outlives": [], "costs_money": true, "taints": [], "fence": {OFFERED}}}}}, "costs": {{"hourly": "{hourly}", "currency": "USD"}}, "lasts": 3600}}"#
    );
    let made = format!(r#"{{"place": "p1", "machine": "lent", "until": "{until}"}}"#);
    let at = |name: &str| lent.join(name).display().to_string();
    let script = format!(
        "#!/bin/sh\n\
         case \"$1\" in\n\
         offers) echo '{offer}' ;;\n\
         make) echo \"$@\" > {asked}; echo '{made}' ;;\n\
         link) echo $PPID >> {linked}; export HOME={home} XDG_RUNTIME_DIR={r} XDG_CONFIG_HOME={c} \
         XDG_STATE_HOME={s} XDG_DATA_HOME={d} XDG_CACHE_HOME={k} PATH={bin}:$PATH; \
         exec {mycelium} --realm \"$3\" link --machine lent ;;\n\
         release) echo \"$2\" > {released} ;;\n\
         esac\n",
        asked = root.join("asked").display(),
        linked = root.join("linked").display(),
        home = lent.display(),
        r = at("r"),
        c = at("c"),
        s = at("s"),
        d = at("d"),
        k = at("k"),
        mycelium = env!("CARGO_BIN_EXE_mycelium"),
        // The far end ends once its search path names another build.
        bin = Path::new(env!("CARGO_BIN_EXE_mycelium"))
            .parent()
            .expect("the build is in a folder")
            .display(),
        released = root.join("released").display(),
    );
    let text = root.join("provider.sh");
    fs::write(&text, script).expect("the provider is written");
    let program = root.join("provider");
    // Put in place by a child, so no test's fork holds it open.
    let put = Command::new("install")
        .args(["-m", "755"])
        .arg(&text)
        .arg(&program)
        .status()
        .expect("install runs");
    assert!(put.success(), "the provider is put in place");
    program
}

/// Two nodes, skiff's asking `program` and lent's, until dropped.
fn nodes(home: &Path, lent: &Path, program: &Path) -> (Running, Running) {
    nodes_with(home, lent, program, [("", ""); 2])
}

/// As [`nodes`], lent's node and skiff's each with one variable more in
/// their environments, `planted`, none where its name is empty.
fn nodes_with(
    home: &Path,
    lent: &Path,
    program: &Path,
    planted: [(&str, &str); 2],
) -> (Running, Running) {
    let with = |mut command: Command, (name, value): (&str, &str)| {
        if !name.is_empty() {
            command.env(name, value);
        }
        command
    };
    let lent_node = Running(
        with(mycelium(lent), planted[0])
            .args(["--realm", "work", "node", "--machine", "lent"])
            .spawn()
            .expect("lent's node starts"),
    );
    let node = skiff(with(mycelium(home), planted[1]), program);
    for at in [home, lent] {
        let socket = at.join("r").join("mycelium").join("work.sock");
        heard(at, &socket, PATIENCE, |_| true);
    }
    (lent_node, node)
}

/// Skiff's node, run by `command`, asking `program`, until dropped.
fn skiff(mut command: Command, program: &Path) -> Running {
    Running(
        command
            .args([
                "--realm",
                "work",
                "node",
                "--machine",
                "skiff",
                "--provider",
            ])
            .arg(program)
            .spawn()
            .expect("skiff's node starts"),
    )
}

/// Starts work that ends at once, as skiff's, that lent alone reaches,
/// saying it may cost where `may_cost`, answering where it started.
fn start(home: &Path, may_cost: bool) -> serde_json::Value {
    let (answered, answer) = mpsc::channel();
    let mut starting = mycelium(home);
    starting.args(["--realm", "work", "start", "--need", "reach=lent-net"]);
    if may_cost {
        starting.arg("--may-cost");
    }
    starting.args(["--", "true"]);
    std::thread::spawn(move || answered.send(starting.output()));
    let started = answer
        .recv_timeout(PATIENCE)
        .expect("the start answers in time")
        .expect("the start runs");
    serde_json::from_slice(&started.stdout).expect("the start says what it did")
}

/// Whether the work skiff's node answered of as `said` started on lent,
/// as `why` says it should: where lent is a Linux machine, it must have.
/// On a Mac lent is the Mac the test runs on, and the node takes every
/// asker there for a strictly fenced agent, whose held start a Mac's
/// holder would run outside any fence, so the start must be refused for
/// that, and the test has nothing further true to ask of lent.
fn started_on_lent(said: &serde_json::Value, why: &str) -> bool {
    if cfg!(target_os = "macos") {
        let refused = said["Err"].as_str().unwrap_or_default();
        assert!(
            refused.contains(
                "lent is a Mac, where a strictly fenced session started held would run outside \
                 any fence"
            ),
            "on a Mac, an agent's held start is refused there: {said}"
        );
        return false;
    }
    assert_eq!(said["Ok"]["Started"]["machine"], "lent", "{why}: {said}");
    true
}

/// The names of the machines skiff's node at `home` lists.
fn machines(home: &Path) -> Vec<String> {
    let listed = mycelium(home)
        .args(["--realm", "work", "machines"])
        .output()
        .expect("machines runs");
    let first = listed
        .stdout
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    let said: serde_json::Value = serde_json::from_slice(first).expect("the machines, as JSON");
    said["Ok"]["Machines"]
        .as_array()
        .expect("a list of machines")
        .iter()
        .filter_map(|one| one["name"].as_str().map(str::to_owned))
        .collect()
}

/// The book at skiff's `home`, once it holds a make and a release.
fn book(home: &Path, within: Duration) -> Vec<serde_json::Value> {
    let book = home.join("s").join("mycelium").join("made.jsonl");
    heard(home, &book, within, |text| {
        text.lines().count() == 2 && text.ends_with('\n')
    })
    .lines()
    .map(|line| serde_json::from_str(line).expect("a JSON line"))
    .collect()
}

#[test]
fn a_machine_made_is_let_go_once_it_hands_over_after_its_work() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let root = dir.path();
    let (home, lent) = (root.join("o"), root.join("l"));
    // It leaves, and is made to end, long after the test.
    let until = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1);
    machine(&home, None);
    machine(
        &lent,
        Some(&format!(
            r#"{{"reach": ["lent-net"], "outlives": [], "costs_money": true, "taints": [], "leaves_at": "{until}"}}"#
        )),
    );
    let program = provider(root, &lent, until, COSTS);
    let _nodes = nodes(&home, &lent, &program);

    let said = start(&home, true);
    if !started_on_lent(&said, "the work starts on the machine made for it") {
        return;
    }
    let lines = book(&home, HANDS_OVER);
    assert_eq!(
        lines[1]["released"]["why"], "its work handed over",
        "work that ended before a minute was out still ran there, so it is let go: {lines:?}"
    );
}

#[test]
fn a_start_that_may_cost_has_a_machine_made_for_it_until_its_time() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let root = dir.path();
    let (home, lent) = (root.join("o"), root.join("l"));
    machine(&home, None);
    machine(
        &lent,
        Some(r#"{"reach": ["lent-net"], "outlives": [], "costs_money": true, "taints": []}"#),
    );
    let until = jiff::Timestamp::now() + LIVES;
    let program = provider(root, &lent, until, COSTS);
    let _nodes = nodes(&home, &lent, &program);

    let said = start(&home, true);
    if !started_on_lent(&said, "the work starts on the machine made for it") {
        return;
    }
    assert_eq!(
        fs::read_to_string(root.join("asked")).expect("the provider was asked to make one"),
        "make small --lasting 3600\n",
        "made of the one shape that takes the work, to live as long as the offer does"
    );

    let lines = book(&home, PATIENCE);
    assert_eq!(
        (
            &lines[0]["made"]["machine"],
            &lines[0]["made"]["costs"]["hourly"],
            &lines[1]["released"]["why"],
        ),
        (
            &serde_json::json!("lent"),
            &serde_json::json!("0.003"),
            &serde_json::json!("its time was up"),
        ),
        "what was made and at what rate, and that it was let go at its time: {lines:?}"
    );
    assert_eq!(
        fs::read_to_string(root.join("released")).expect("it was released"),
        "p1\n",
        "released where it was made"
    );
}

#[test]
fn a_machine_that_costs_nothing_is_made_for_any_start_and_let_go_after_its_work() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let root = dir.path();
    let (home, lent) = (root.join("o"), root.join("l"));
    let until = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1);
    machine(&home, None);
    machine(
        &lent,
        Some(&format!(
            r#"{{"reach": ["lent-net"], "outlives": [], "costs_money": true, "taints": [], "leaves_at": "{until}"}}"#
        )),
    );
    let program = provider(root, &lent, until, FREE);
    let _nodes = nodes(&home, &lent, &program);

    let said = start(&home, false);
    if !started_on_lent(
        &said,
        "a start that never said it may cost still has a free machine made for it",
    ) {
        return;
    }
    let lines = book(&home, HANDS_OVER);
    assert_eq!(
        (
            &lines[0]["made"]["costs"]["hourly"],
            &lines[1]["released"]["why"]
        ),
        (
            &serde_json::json!(FREE),
            &serde_json::json!("its work handed over")
        ),
        "made at no cost, and let go once its work was done: {lines:?}"
    );
    assert_eq!(
        machines(&home),
        ["skiff"],
        "a machine let go leaves the realm's machines at once, not at its time"
    );
}

#[test]
fn a_node_started_again_links_the_machine_it_made_before() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let root = dir.path();
    let (home, lent) = (root.join("o"), root.join("l"));
    // It never hands over, and is made to end long after the test.
    let until = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1);
    machine(&home, None);
    machine(
        &lent,
        Some(r#"{"reach": ["lent-net"], "outlives": [], "costs_money": true, "taints": []}"#),
    );
    let program = provider(root, &lent, until, COSTS);
    let (_lent_node, first) = nodes(&home, &lent, &program);
    let said = start(&home, true);
    if !started_on_lent(&said, "the work starts on the machine made for it") {
        return;
    }
    let linked = root.join("linked");
    heard(root, &linked, PATIENCE, |text| !text.is_empty());
    let book = fs::read_to_string(home.join("s").join("mycelium").join("made.jsonl"))
        .expect("the make is kept");
    let kept: serde_json::Value =
        serde_json::from_str(book.lines().next().unwrap_or_default()).expect("a line");
    assert_eq!(
        kept["realm"], "work",
        "the make names its node's realm: {kept}"
    );

    drop(first);
    let again = skiff(mycelium(&home), &program);
    let pid = again.0.id().to_string();
    heard(root, &linked, PATIENCE, |text| {
        text.lines().any(|line| line == pid)
    });
    ready(root, &home, "lent");
    assert!(
        !root.join("released").exists(),
        "a machine still made, and linked again, isn't released"
    );
}

/// Waits until skiff's node at `home` says `name` is ready, hearing each
/// change under `root`.
fn ready(root: &Path, home: &Path, name: &str) {
    let (telling, told) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(telling).expect("a watcher");
    watcher
        .watch(root, RecursiveMode::Recursive)
        .expect("the folder is watched");
    let deadline = Instant::now() + PATIENCE;
    loop {
        let listed = mycelium(home)
            .args(["--realm", "work", "machines"])
            .output()
            .expect("machines runs");
        let first = listed
            .stdout
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default();
        let said: serde_json::Value = serde_json::from_slice(first).unwrap_or_default();
        let state = said["Ok"]["Machines"]
            .as_array()
            .and_then(|all| all.iter().find(|one| one["name"] == name))
            .map(|one| one["state"].clone());
        if state == Some(serde_json::json!("ready")) {
            return;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "{name} never came to be ready: {said}");
        // A watch that fails or times out has the machines asked again.
        let _ = told.recv_timeout(left);
    }
}

/// The secret planted where a carry starts, which never reaches where it
/// lands.
const SECRET: &str = "planted-7c1e4b";

#[test]
fn nothing_of_the_askers_environment_reaches_the_work_on_another_machine() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let root = dir.path();
    let (home, lent) = (root.join("o"), root.join("l"));
    let until = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1);
    machine(&home, None);
    machine(
        &lent,
        Some(&format!(
            r#"{{"reach": ["lent-net"], "outlives": [], "costs_money": true, "taints": [], "leaves_at": "{until}"}}"#
        )),
    );
    let program = provider(root, &lent, until, FREE);
    // Lent's own node holds a mark, which its work should have, and
    // skiff's holds the secret, as the session's machine might.
    let _nodes = nodes_with(
        &home,
        &lent,
        &program,
        [("LENT_OWN", "lent-mark"), ("SKIFF_SECRET", SECRET)],
    );
    let out = root.join("environment");
    // A full path, since a sandbox's search path may hold no shell.
    let shell = std::env::var("BASH").unwrap_or_else(|_| "/bin/sh".to_owned());
    let (answered, answer) = mpsc::channel();
    let mut starting = mycelium(&home);
    // The asker holds it as a carry run from inside a session does, since
    // `mycelium carry` is that session's child.
    starting
        .env("SESSION_SECRET", SECRET)
        .args(["--realm", "work", "start", "--need", "reach=lent-net", "--"])
        .args([&shell, "-c", r#"env > "$0.new" && mv "$0.new" "$0""#])
        .arg(&out);
    std::thread::spawn(move || answered.send(starting.output()));
    let started = answer
        .recv_timeout(PATIENCE)
        .expect("the start answers in time")
        .expect("the start runs");
    let said: serde_json::Value =
        serde_json::from_slice(&started.stdout).expect("the start says what it did");
    if !started_on_lent(&said, "the work starts on the other machine") {
        return;
    }
    // The work's file comes only as the far end hands over, at its keep a
    // minute in, so it's waited for as a hand-over is, longer than that
    // minute.
    let environment = heard(root, &out, HANDS_OVER, |_| true);
    assert!(
        environment.lines().any(|line| line == "LENT_OWN=lent-mark"),
        "the work has its own machine's environment: {environment}"
    );
    let arrived: Vec<&str> = environment
        .lines()
        .filter(|line| line.contains(SECRET) || line.contains("_SECRET="))
        .collect();
    assert_eq!(
        arrived,
        Vec::<&str>::new(),
        "nothing planted in the asker's or skiff's environment arrives"
    );
}

#[test]
fn a_machine_that_costs_is_never_made_for_a_start_that_did_not_say_it_may() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let root = dir.path();
    let (home, lent) = (root.join("o"), root.join("l"));
    machine(&home, None);
    machine(
        &lent,
        Some(r#"{"reach": ["lent-net"], "outlives": [], "costs_money": true, "taints": []}"#),
    );
    let program = provider(root, &lent, jiff::Timestamp::now() + LIVES, COSTS);
    let _nodes = nodes(&home, &lent, &program);

    let said = start(&home, false);
    let why = said["Err"].as_str().unwrap_or_default();
    assert!(
        why.starts_with("nowhere") && why.contains("costs"),
        "refused, saying the only offer costs: {said}"
    );
    assert!(
        !root.join("asked").exists(),
        "the provider was never asked to make one"
    );
}
