//! A machine made for a start that may cost money: optimus's node asks a
//! provider's program what it offers, makes the one machine that would
//! take the work, links it through the program, starts the work there, and
//! lets it go at its time, keeping each make and release in its book. Each
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

/// The folders of a machine at `home`, its runtime folder open to it
/// alone, declaring `declared`.
fn machine(home: &Path, declared: Option<&str>) {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home.join("r"))
        .expect("its runtime folder is made");
    if let Some(declared) = declared {
        let config = home.join("c").join("mycelium");
        fs::create_dir_all(&config).expect("its configuration's folder is made");
        fs::write(config.join("declared.json"), declared).expect("what it declares is written");
    }
}

/// Waits until `holds` says `path` is as wanted, hearing each change under
/// `root`, and answers what it holds.
fn heard(root: &Path, path: &Path, holds: impl Fn(&str) -> bool) -> String {
    let (telling, told) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(telling).expect("a watcher");
    watcher
        .watch(root, RecursiveMode::Recursive)
        .expect("the folder is watched");
    let deadline = Instant::now() + PATIENCE;
    loop {
        let text = fs::read_to_string(path).unwrap_or_default();
        if path.exists() && holds(&text) {
            return text;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "{} never came to be as wanted: {text:?}",
            path.display()
        );
        // A watch that fails or times out has the path looked at again.
        let _ = told.recv_timeout(left);
    }
}

/// A provider's program at `root`, offering one shape of machine that
/// reaches `lent-net`, making it as `lent` at `lent`'s folders until
/// `until`, linking it there, and saying what it released.
fn provider(root: &Path, lent: &Path, until: jiff::Timestamp) -> PathBuf {
    let offer = r#"{"shape": "small", "facts": {"declared": {"reach": ["lent-net"], "outlives": [], "costs_money": true, "taints": []}}, "costs": {"hourly": "0.003", "currency": "USD"}, "lasts": 3600}"#;
    let made = format!(r#"{{"place": "p1", "machine": "lent", "until": "{until}"}}"#);
    let at = |name: &str| lent.join(name).display().to_string();
    let script = format!(
        "#!/bin/sh\n\
         case \"$1\" in\n\
         offers) echo '{offer}' ;;\n\
         make) echo \"$@\" > {asked}; echo '{made}' ;;\n\
         link) export HOME={home} XDG_RUNTIME_DIR={r} XDG_CONFIG_HOME={c} \
         XDG_STATE_HOME={s} XDG_DATA_HOME={d} XDG_CACHE_HOME={k}; \
         exec {mycelium} --realm \"$3\" link --machine lent ;;\n\
         release) echo \"$2\" > {released} ;;\n\
         esac\n",
        asked = root.join("asked").display(),
        home = lent.display(),
        r = at("r"),
        c = at("c"),
        s = at("s"),
        d = at("d"),
        k = at("k"),
        mycelium = env!("CARGO_BIN_EXE_mycelium"),
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
    let program = provider(root, &lent, until);

    let _lent_node = Running(
        mycelium(&lent)
            .args(["--realm", "work", "node", "--machine", "lent"])
            .spawn()
            .expect("lent's node starts"),
    );
    let _node = Running(
        mycelium(&home)
            .args([
                "--realm",
                "work",
                "node",
                "--machine",
                "optimus",
                "--provider",
            ])
            .arg(&program)
            .spawn()
            .expect("optimus's node starts"),
    );
    for at in [&home, &lent] {
        let socket = at.join("r").join("mycelium").join("work.sock");
        heard(at, &socket, |_| true);
    }

    let (answered, answer) = mpsc::channel();
    let mut starting = mycelium(&home);
    starting.args([
        "--realm",
        "work",
        "start",
        "--need",
        "reach=lent-net",
        "--may-cost",
        "--",
        "true",
    ]);
    std::thread::spawn(move || answered.send(starting.output()));
    let started = answer
        .recv_timeout(PATIENCE)
        .expect("the start answers in time")
        .expect("the start runs");
    let said: serde_json::Value =
        serde_json::from_slice(&started.stdout).expect("the start says what it did");
    assert_eq!(
        said["Ok"]["Started"]["machine"], "lent",
        "the work starts on the machine made for it: {said}"
    );
    assert_eq!(
        fs::read_to_string(root.join("asked")).expect("the provider was asked to make one"),
        "make small --lasting 3600\n",
        "made of the one shape that takes the work, to live as long as the offer does"
    );

    let book = home.join("s").join("mycelium").join("made.jsonl");
    let kept = heard(&home, &book, |text| text.contains("released"));
    let lines: Vec<serde_json::Value> = kept
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect();
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
        "what was made and at what rate, and that it was let go at its time: {kept}"
    );
    assert_eq!(
        fs::read_to_string(root.join("released")).expect("it was released"),
        "p1\n",
        "released where it was made"
    );
}
