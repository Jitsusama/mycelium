use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;
use std::sync::Arc;

use serde_json::json;

use super::*;

const QUEST: &str = "QEST-20261124-SKIFF1";
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn start(command: &[&str]) -> Start {
    serde_json::from_value(json!({ "work": {}, "command": command })).expect("a start")
}

fn gate() -> Start {
    start(&["nix-offload", "answer", QUEST, COMMIT])
}

/// Gates answering with a stand-in program the workers `named` are named
/// in a file of `dir`.
fn gates(dir: &Path, named: &str) -> Arc<Gates> {
    let listed = dir.join("answers-for");
    fs::write(&listed, named).expect("the list");
    Arc::new(Gates::new(PathBuf::from("/bin/true"), listed).expect("gates"))
}

fn refused(gates: &Arc<Gates>, worker: &str, start: &Start) -> Refused {
    gates
        .admit(worker, start)
        .expect_err("the start must be refused")
}

#[test]
fn a_listed_worker_is_admitted_a_gate_run_by_its_store_path_with_its_two_arguments() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let gates = gates(dir.path(), "tern\nskiff\n");
    let answer = gates.admit("skiff", &gate()).expect("admitted");
    let command = answer.command();
    let run = command.as_std();
    assert_eq!(
        run.get_program(),
        "/bin/true",
        "only the program named runs"
    );
    let arguments: Vec<_> = run.get_args().collect();
    assert_eq!(
        arguments,
        ["answer", QUEST, COMMIT],
        "the act, the quest and the commit, and nothing else"
    );
    assert_eq!(
        run.get_envs().count(),
        0,
        "nothing of the worker's enters its environment"
    );
    assert_eq!(
        run.get_current_dir(),
        None,
        "it runs in no folder of the worker's"
    );
    assert_eq!(
        (answer.worker(), answer.quest(), answer.commit()),
        ("skiff", QUEST, COMMIT)
    );
}

#[test]
fn a_gate_with_a_third_argument_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let gates = gates(dir.path(), "skiff\n");
    let more = start(&["nix-offload", "answer", QUEST, COMMIT, "--flake"]);
    assert_eq!(refused(&gates, "skiff", &more), Refused::Arguments);
    let fewer = start(&["nix-offload", "answer", QUEST]);
    assert_eq!(refused(&gates, "skiff", &fewer), Refused::Arguments);
}

#[test]
fn a_commit_short_or_not_lowercase_hex_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let gates = gates(dir.path(), "skiff\n");
    for commit in [
        &COMMIT[..12],
        &format!("{COMMIT}0"),
        &COMMIT.to_uppercase(),
        "g123456789abcdef0123456789abcdef01234567",
        "HEAD",
    ] {
        let asked = start(&["nix-offload", "answer", QUEST, commit]);
        assert_eq!(
            refused(&gates, "skiff", &asked),
            Refused::Commit,
            "{commit}"
        );
    }
}

#[test]
fn a_quest_with_a_slash_or_dots_or_a_setting_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let gates = gates(dir.path(), "skiff\n");
    for quest in [
        "QEST-20261124-SKIFF1/..",
        "../QEST-20261124-SKIFF1",
        "..",
        "QEST-20261124-SKIFF1/x",
        "HOME=/tmp",
        "",
    ] {
        let asked = start(&["nix-offload", "answer", quest, COMMIT]);
        assert_eq!(refused(&gates, "skiff", &asked), Refused::Quest, "{quest}");
    }
}

#[test]
fn an_environment_set_before_the_program_or_another_program_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let gates = gates(dir.path(), "skiff\n");
    for command in [
        &["NIX_CONFIG=x", "nix-offload", "answer", QUEST, COMMIT][..],
        &[
            "env",
            "NIX_CONFIG=x",
            "nix-offload",
            "answer",
            QUEST,
            COMMIT,
        ],
        &["/tmp/nix-offload", "answer", QUEST, COMMIT],
        &["sh", "-c", "nix-offload answer"],
        &["nix-offload", "there", QUEST, COMMIT],
        &["nix-offload"],
        &[],
    ] {
        assert_eq!(
            refused(&gates, "skiff", &start(command)),
            Refused::Program,
            "{command:?}"
        );
    }
}

#[test]
fn a_gate_naming_a_folder_or_marked_a_carry_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let gates = gates(dir.path(), "skiff\n");
    let mut foldered = gate();
    foldered.folder = Some(PathBuf::from("/tmp"));
    assert_eq!(refused(&gates, "skiff", &foldered), Refused::Folder);
    let mut carrying = gate();
    carrying.carries = true;
    assert_eq!(refused(&gates, "skiff", &carrying), Refused::Carry);
}

#[test]
fn an_unlisted_worker_is_refused_whatever_it_asks() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let gates = gates(dir.path(), "skiff\n# tern\n");
    assert_eq!(refused(&gates, "tern", &gate()), Refused::Unlisted);
    assert_eq!(refused(&gates, "skif", &gate()), Refused::Unlisted);
    assert_eq!(
        refused(&gates, "tern", &start(&["sh"])),
        Refused::Unlisted,
        "an unlisted worker learns nothing of the grammar"
    );
}

#[test]
fn emptying_or_removing_the_list_stops_new_gates_at_once() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let gates = gates(dir.path(), "skiff\n");
    drop(
        gates
            .admit("skiff", &gate())
            .expect("admitted while listed"),
    );
    fs::write(dir.path().join("answers-for"), "").expect("emptied");
    assert_eq!(refused(&gates, "skiff", &gate()), Refused::Unlisted);
    fs::remove_file(dir.path().join("answers-for")).expect("removed");
    assert_eq!(refused(&gates, "skiff", &gate()), Refused::Unlisted);
}

#[test]
fn one_gate_a_worker_and_two_on_the_machine_run_at_once_past_which_each_is_refused() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let gates = gates(dir.path(), "skiff\ntern\nkestrel\n");
    let first = gates.admit("skiff", &gate()).expect("the first");
    assert_eq!(refused(&gates, "skiff", &gate()), Refused::Busy);
    let second = gates.admit("tern", &gate()).expect("another worker's");
    assert_eq!(refused(&gates, "kestrel", &gate()), Refused::Full);
    assert_eq!(gates.running(), AT_ONCE);
    drop(first);
    assert_eq!(gates.running(), 1, "an answer ended gives its place back");
    drop(
        gates
            .admit("kestrel", &gate())
            .expect("a place is free again"),
    );
    drop(second);
    assert_eq!(gates.running(), 0);
}

#[test]
fn a_gate_program_named_without_its_full_path_is_refused_as_the_node_starts() {
    let refused = Gates::new(PathBuf::from("nix-offload"), PathBuf::from("/nowhere"));
    assert!(
        refused.is_err(),
        "a name alone would be found on a search path"
    );
}

fn key(name: &str) -> String {
    format!("scratch/handover/gates/{COMMIT}/{name}")
}

#[test]
fn only_a_gates_request_and_bundle_under_its_commit_are_files_a_worker_sends() {
    assert_eq!(
        sent(&key(REQUEST)).map(|file| file.largest),
        Ok(LARGEST_REQUEST)
    );
    assert_eq!(
        sent(&key(BUNDLE)).map(|file| file.largest),
        Ok(LARGEST_BUNDLE)
    );
    for wrong in [
        key(RESULT),
        key("log"),
        key("request.json/x"),
        format!("scratch/handover/gates/{}/gate.bundle", &COMMIT[..39]),
        format!(
            "scratch/handover/gates/{}/gate.bundle",
            COMMIT.to_uppercase()
        ),
        "scratch/handover/gates/../gate.bundle".to_owned(),
        "scratch/handover/other/gate.bundle".to_owned(),
        "record/README.md".to_owned(),
    ] {
        assert_eq!(sent(&wrong), Err(Refused::Name), "{wrong}");
    }
}

#[test]
fn a_gates_log_and_whatever_else_it_leaves_but_its_result_is_withheld_from_a_worker() {
    assert!(withheld(&key("log")));
    assert!(withheld(&key("other")));
    assert!(withheld("scratch/handover/gates/stray"));
    assert!(!withheld(&key(RESULT)));
    assert!(!withheld(&key(REQUEST)));
    assert!(!withheld(&key(BUNDLE)));
    assert!(
        !withheld("scratch/handover/carry.json"),
        "only the gates' files are"
    );
}

/// The quests' scratch folders, with `QUEST`'s there.
fn scratch() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary folder");
    fs::create_dir(dir.path().join(QUEST)).expect("the quest's scratch");
    dir
}

/// Whether anything but `left` is in the gate's folder.
fn left_in_gate(scratch: &Path) -> Vec<String> {
    let folder = scratch.join(QUEST).join("handover/gates").join(COMMIT);
    let mut names: Vec<String> = fs::read_dir(folder)
        .map(|read| {
            read.map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn a_request_sent_is_taken_whole_as_a_new_file_of_its_own() {
    let dir = scratch();
    let placed = take(dir.path(), QUEST, &key(REQUEST), b"{}").expect("taken");
    assert_eq!(
        placed,
        dir.path()
            .join(QUEST)
            .join("handover/gates")
            .join(COMMIT)
            .join(REQUEST)
    );
    assert_eq!(fs::read(&placed).expect("read"), b"{}");
    let stat = fs::symlink_metadata(&placed).expect("its stat");
    assert!(stat.file_type().is_file());
    assert_eq!(stat.nlink(), 1, "no other name holds it");
    assert_eq!(stat.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        left_in_gate(dir.path()),
        [REQUEST],
        "nothing half written is left"
    );
}

#[test]
fn a_file_already_there_is_never_written_over() {
    let dir = scratch();
    take(dir.path(), QUEST, &key(BUNDLE), b"first").expect("the first");
    assert_eq!(
        take(dir.path(), QUEST, &key(BUNDLE), b"second"),
        Err(Refused::Exists)
    );
    let placed = dir
        .path()
        .join(QUEST)
        .join("handover/gates")
        .join(COMMIT)
        .join(BUNDLE);
    assert_eq!(fs::read(placed).expect("read"), b"first");
    assert_eq!(left_in_gate(dir.path()), [BUNDLE]);
}

#[test]
fn a_link_where_the_file_goes_is_refused_and_left_as_it_was() {
    let dir = scratch();
    let elsewhere = dir.path().join("elsewhere");
    let folder = dir.path().join(QUEST).join("handover/gates").join(COMMIT);
    fs::create_dir_all(&folder).expect("the gate's folder");
    symlink(&elsewhere, folder.join(REQUEST)).expect("a planted link");
    assert_eq!(
        take(dir.path(), QUEST, &key(REQUEST), b"{}"),
        Err(Refused::Exists)
    );
    assert!(!elsewhere.exists(), "nothing is written through it");
}

#[test]
fn a_link_on_the_way_to_the_gates_folder_is_refused() {
    let dir = scratch();
    let elsewhere = dir.path().join("elsewhere");
    fs::create_dir(&elsewhere).expect("elsewhere");
    fs::create_dir(dir.path().join(QUEST).join("handover")).expect("the handover");
    symlink(&elsewhere, dir.path().join(QUEST).join("handover/gates")).expect("a planted link");
    assert_eq!(
        take(dir.path(), QUEST, &key(REQUEST), b"{}"),
        Err(Refused::Link)
    );
    assert_eq!(
        fs::read_dir(&elsewhere).expect("read").count(),
        0,
        "nothing is made through it"
    );
}

#[test]
fn a_file_larger_than_a_gate_takes_is_refused_before_anything_is_written() {
    let dir = scratch();
    let large = vec![b' '; usize::try_from(LARGEST_REQUEST).expect("small") + 1];
    assert_eq!(
        take(dir.path(), QUEST, &key(REQUEST), &large),
        Err(Refused::Large)
    );
    assert!(
        !dir.path().join(QUEST).join("handover").exists(),
        "not even a folder is made"
    );
}

#[test]
fn a_wrong_name_or_folder_is_refused() {
    let dir = scratch();
    assert_eq!(
        take(dir.path(), QUEST, &key("log"), b"x"),
        Err(Refused::Name)
    );
    let short = format!("scratch/handover/gates/{}/{REQUEST}", &COMMIT[..12]);
    assert_eq!(take(dir.path(), QUEST, &short, b"x"), Err(Refused::Name));
    assert!(!dir.path().join(QUEST).join("handover").exists());
}

#[test]
fn a_quest_with_no_scratch_here_has_one_made_but_no_quests_id_is_refused() {
    let dir = scratch();
    let at = take(dir.path(), "QEST-20261124-TERN22", &key(REQUEST), b"{}")
        .expect("a quest with no scratch here yet takes its gate");
    assert!(
        at.starts_with(dir.path().join("QEST-20261124-TERN22")),
        "{}",
        at.display()
    );
    assert_eq!(
        take(dir.path(), "..", &key(REQUEST), b"{}"),
        Err(Refused::NotGiven)
    );
}
