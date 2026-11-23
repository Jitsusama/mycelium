//! `mycelium sprout` as its own process: asked from a home folder, or
//! told to begin in one, it refuses before asking the node anything, so
//! no session is started where it would have none of its repository's
//! skills. Its home is a checkout here, as a home kept in git is, since
//! that is when a home could pass for a repository. Asked of a node
//! standing in, it says what its seed left out only where that node is
//! from before `"Sprout"`, and only once the session started.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

/// A home kept in git, with a folder of notes in it that isn't a
/// repository of its own.
fn home_in_git() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("a home");
    std::fs::create_dir(home.path().join(".git")).expect("home in git");
    std::fs::create_dir(home.path().join("notes")).expect("a folder in home");
    home
}

/// `mycelium sprout` run from `cwd` with `args`, its home `home` and its
/// node's socket where no node listens.
fn sprout(home: &Path, cwd: &Path, args: &[&str]) -> (bool, String) {
    sprout_asking(home, &home.join("no-node.sock"), cwd, args)
}

/// `mycelium sprout` run from `cwd` with `args`, its home `home` and its
/// node's socket `socket`.
fn sprout_asking(home: &Path, socket: &Path, cwd: &Path, args: &[&str]) -> (bool, String) {
    let ran = Command::new(env!("CARGO_BIN_EXE_mycelium"))
        .args(["--realm", "work", "--socket"])
        .arg(socket)
        .arg("sprout")
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", home.join("r"))
        .env("XDG_STATE_HOME", home.join("s"))
        .output()
        .expect("mycelium runs");
    (
        ran.status.success(),
        String::from_utf8_lossy(&ran.stderr).into_owned(),
    )
}

#[test]
fn a_sprout_from_home_or_a_folder_in_it_asks_which_repository() {
    let home = home_in_git();
    for cwd in [home.path().to_owned(), home.path().join("notes")] {
        let (ran, said) = sprout(home.path(), &cwd, &["make", "tea"]);
        assert!(!ran, "it refused from {}: {said}", cwd.display());
        assert!(
            said.contains("--in <folder>"),
            "it says what to name, not that the node is away: {said}"
        );
    }
}

#[test]
fn a_home_folder_named_is_refused_however_it_is_spelled() {
    let home = home_in_git();
    let notes = home.path().join("notes");
    let whole = home.path().to_str().expect("a home said in words");
    for named in [whole, "..", "../notes/.."] {
        let (ran, said) = sprout(home.path(), &notes, &["--in", named, "make", "tea"]);
        assert!(!ran, "it refused {named}: {said}");
        assert!(
            said.contains("home folder"),
            "it says why for {named}: {said}"
        );
    }
}

/// A sprout `--on` another machine is that machine's node's to judge, so
/// this machine's name, which only a sprout here needs, can't stop it:
/// its node's record of the name unreadable, it goes on to ask the node.
#[test]
fn a_sprout_on_another_machine_needs_no_name_for_this_one() {
    let home = home_in_git();
    let rows = home.path().join("src/rows");
    std::fs::create_dir_all(rows.join(".git")).expect("a checkout");
    let record = home.path().join("s/mycelium/machine");
    std::fs::create_dir_all(&record).expect("a record that can't be read");
    let folder = rows.to_str().expect("a folder said in words");

    let (ran, said) = sprout(
        home.path(),
        &rows,
        &["--on", "tern", "--in", folder, "make", "tea"],
    );

    assert!(!ran, "no node listens: {said}");
    assert!(
        !said.contains("Is a directory"),
        "it asked the node rather than reading this machine's name: {said}"
    );
}

/// How a node standing in answers one ask, given the line it was asked.
type Answer = Box<dyn FnOnce(&str) -> serde_json::Value + Send>;

/// An answer that is `value`, whatever was asked.
fn always(value: serde_json::Value) -> Answer {
    Box::new(move |_| value)
}

/// A node at `socket` answering its asks in turn with `answers`, one ask
/// a connection as the client makes them, keeping each ask it was given.
fn node_answering(socket: &Path, answers: Vec<Answer>) -> Arc<Mutex<Vec<String>>> {
    let listener = UnixListener::bind(socket).expect("the node's socket");
    let asked = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&asked);
    std::thread::spawn(move || {
        for answer in answers {
            let Ok((client, _)) = listener.accept() else {
                return;
            };
            let mut ask = String::new();
            let _ = BufReader::new(&client).read_line(&mut ask);
            let answered = answer(&ask);
            kept.lock().expect("the asks kept").push(ask);
            let _ = writeln!(&client, "{answered}");
        }
    });
    asked
}

/// A checkout in a home whose node records this machine as tern, so a
/// sprout from it is judged here.
fn checkout_on_tern() -> (tempfile::TempDir, std::path::PathBuf) {
    let home = home_in_git();
    let rows = home.path().join("src/rows");
    std::fs::create_dir_all(rows.join(".git")).expect("a checkout");
    let state = home.path().join("s/mycelium");
    std::fs::create_dir_all(&state).expect("the node's state");
    std::fs::write(state.join("machine"), "tern").expect("this machine's name");
    (home, rows)
}

/// What a node answers a start that started, in `folder`.
fn started_in(folder: &Path) -> Answer {
    always(serde_json::json!({"Ok": {"Started": {
        "machine": "tern",
        "held": "5e55",
        "folder": folder,
        "attach": ["mycelium", "--realm", "work", "attach", "5e55"],
    }}}))
}

/// What a node from before `"Sprout"` answers the line it was asked, made
/// as such a node makes it: serde's own refusal of a request it has no
/// variant for, wrapped as the node wraps any line it can't read
/// (`node::serve`), so a rename of the ask on the wire, or new wording
/// from serde, reaches the fallback here as it would there.
fn as_a_node_before_sprout() -> Answer {
    // The requests of a node from before `"Sprout"`, as far as matters:
    // serde refuses what isn't among them by name.
    #[derive(Debug, serde::Deserialize)]
    enum Old {
        Start,
        Sessions,
    }
    Box::new(|ask: &str| {
        let error = serde_json::from_str::<Old>(ask.trim_end()).expect_err("unknown to it");
        serde_json::json!({"Err": format!("not a request: {error}")})
    })
}

/// The lines of `said` saying what a seed left out.
fn left_out_lines(said: &str) -> Vec<&str> {
    said.lines()
        .filter(|line| line.contains("its seed leaves out"))
        .collect()
}

#[test]
fn a_node_from_before_sprout_is_asked_to_start_and_what_the_seed_left_out_is_said() {
    let (home, rows) = checkout_on_tern();
    let socket = home.path().join("old.sock");
    let asked = node_answering(
        &socket,
        vec![as_a_node_before_sprout(), started_in(&rows)],
    );

    let (ran, said) = sprout_asking(
        home.path(),
        &socket,
        &rows,
        &["--from", "QEST-20261116-42P0R4", "make", "tea"],
    );

    assert!(ran, "it started: {said}");
    let lines = left_out_lines(&said);
    assert_eq!(lines.len(), 1, "said once: {said}");
    let line = lines[0];
    assert!(
        line.starts_with("mycelium sprout: sprout sprout-"),
        "as the command, naming the sprout: {line}"
    );
    assert!(
        line.contains(" on tern, held as 5e55: its seed leaves out the cell it came from"),
        "naming its machine, its session and why: {line}"
    );
    let asked = asked.lock().expect("the asks kept");
    assert_eq!(asked.len(), 2, "a sprout, then the start it judged");
    assert!(asked[0].starts_with(r#"{"Sprout":"#), "{}", asked[0]);
    assert!(asked[1].starts_with(r#"{"Start":"#), "{}", asked[1]);
    let id = line
        .strip_prefix("mycelium sprout: sprout ")
        .and_then(|rest| rest.split_once(' '))
        .map(|(id, _)| id)
        .expect("the sprout's id");
    assert!(
        asked[1].contains(id),
        "the id named is the one its seed was sent with: {id} in {}",
        asked[1]
    );
}

#[test]
fn a_start_an_old_node_refuses_says_nothing_of_what_was_left_out() {
    let (home, rows) = checkout_on_tern();
    let socket = home.path().join("old.sock");
    let refused = always(serde_json::json!({"Err": "no room on tern"}));
    let _asked = node_answering(&socket, vec![as_a_node_before_sprout(), refused]);

    let (ran, said) = sprout_asking(
        home.path(),
        &socket,
        &rows,
        &["--from", "QEST-20261116-42P0R4", "make", "tea"],
    );

    assert!(!ran, "it was refused: {said}");
    assert!(said.contains("no room on tern"), "saying why: {said}");
    assert_eq!(left_out_lines(&said), Vec::<&str>::new(), "{said}");
}

/// A node of now makes the seed and its own id, so what the seed left
/// out is the node's to log (`sprout::LeftOut::tell`, tested there), and
/// the command, whose id was never sent, says none of it.
#[test]
fn the_command_says_nothing_left_out_where_a_node_of_now_answers() {
    let (home, rows) = checkout_on_tern();
    let socket = home.path().join("now.sock");
    let asked = node_answering(&socket, vec![started_in(&rows)]);

    let (ran, said) = sprout_asking(
        home.path(),
        &socket,
        &rows,
        &["--from", "QEST-20261116-42P0R4", "make", "tea"],
    );

    assert!(ran, "it started: {said}");
    assert_eq!(left_out_lines(&said), Vec::<&str>::new(), "{said}");
    assert_eq!(asked.lock().expect("the asks kept").len(), 1, "one ask");
}

#[test]
fn a_cell_it_came_from_that_isnt_one_is_refused() {
    let home = home_in_git();
    let (ran, said) = sprout(
        home.path(),
        home.path(),
        &["--from", "X; rm -rf ~", "make", "tea"],
    );
    assert!(!ran, "it refused: {said}");
    assert!(said.contains("isn't a cell"), "it says why: {said}");
}
