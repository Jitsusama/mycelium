use serde_json::json;

use super::*;

/// When the tests stand: 17 November 2026, 06:00 UTC.
const NOW: i64 = 1_795_413_600_000;

/// `millis` since the epoch, in RFC 3339.
fn said_at(millis: i64) -> String {
    jiff::Timestamp::from_millisecond(millis)
        .expect("the tests stand in this age")
        .to_string()
}

/// A boot, as `/proc` names it.
const BOOT: &str = "3f2c1a6e-8b9d-4e7f-a1b2-c3d4e5f60718";

/// Another boot.
const OTHER_BOOT: &str = "9a8b7c6d-5e4f-4a3b-9c2d-1e0f2a3b4c5d";

#[test]
fn a_reboot_is_done_once_the_boot_differs() {
    let when = |boot: &str| json!({"probe": "rebooted", "boot_id": boot});
    assert_eq!(rebooted(&when(OTHER_BOOT), Some(BOOT)), Verdict::Done);
    assert_eq!(rebooted(&when(BOOT), Some(BOOT)), Verdict::NotDone);
    for (how, written) in [
        ("with its line's end", format!("{BOOT}\n")),
        ("in capitals", BOOT.to_uppercase()),
        ("without dashes", BOOT.replace('-', "")),
    ] {
        assert_eq!(
            rebooted(&when(&written), Some(BOOT)),
            Verdict::NotDone,
            "this boot written {how} is this boot"
        );
    }
    assert_eq!(
        rebooted(&when("then"), Some(BOOT)),
        Verdict::Unknown,
        "what isn't a boot can't tell"
    );
    assert_eq!(
        rebooted(&when(BOOT), None),
        Verdict::Unknown,
        "nor can a machine naming no boot"
    );
    assert_eq!(
        rebooted(&json!({"probe": "rebooted"}), Some(BOOT)),
        Verdict::Unknown,
        "nor a wait naming none"
    );
}

#[test]
fn a_switch_is_done_once_the_machine_runs_the_system_wanted() {
    let when = json!({"probe": "switch-done", "wanted": "/nix/store/aaaa-system"});
    assert_eq!(
        switched(&when, Some(Path::new("/nix/store/aaaa-system"))),
        Verdict::Done
    );
    assert_eq!(
        switched(&when, Some(Path::new("/nix/store/bbbb-system"))),
        Verdict::NotDone
    );
    assert_eq!(
        switched(&when, None),
        Verdict::Unknown,
        "a machine with no current system can't tell"
    );
}

#[test]
fn a_post_is_done_by_a_fresh_verdict_alone() {
    let verdict = |done: bool, ago: i64| json!({"probe": "message-posted", "verdict": {"done": done, "at": said_at(NOW - ago)}});
    assert_eq!(posted(&verdict(true, 60_000), NOW), Verdict::Done);
    assert_eq!(posted(&verdict(false, 60_000), NOW), Verdict::NotDone);
    assert_eq!(
        posted(&verdict(true, VERDICT_KEPT), NOW),
        Verdict::Done,
        "believed for its fifteen minutes"
    );
    assert_eq!(
        posted(&verdict(true, VERDICT_KEPT + 1), NOW),
        Verdict::Unknown,
        "and no longer, as nobody refreshed it"
    );
    assert_eq!(
        posted(&verdict(true, -60_000), NOW),
        Verdict::Unknown,
        "one said from the future isn't believed"
    );
    assert_eq!(
        posted(&json!({"probe": "message-posted"}), NOW),
        Verdict::Unknown,
        "with no verdict, nothing can tell"
    );
}

#[test]
fn a_pull_request_is_asked_of_github_only_by_a_repository_and_number() {
    let asked =
        pr_view(&json!({"probe": "pr-merged", "repo": "Jitsusama/mycelium", "number": 286}));
    assert_eq!(
        asked.as_deref().map(|words| &words[..5]),
        Some(&["pr", "view", "286", "--repo", "Jitsusama/mycelium"].map(str::to_owned)[..]),
    );
    for (why, when) in [
        ("no number", json!({"repo": "a/b"})),
        ("a number in words", json!({"repo": "a/b", "number": "1"})),
        ("no owner", json!({"repo": "/b", "number": 1})),
        (
            "an option for a repository",
            json!({"repo": "--jq/x", "number": 1}),
        ),
        ("a path", json!({"repo": "a/b/c", "number": 1})),
        (
            "another host",
            json!({"host": "world", "repo": "a/b", "number": 1}),
        ),
    ] {
        assert_eq!(pr_view(&when), None, "{why} asks nothing");
    }
}

#[test]
fn what_github_says_of_a_pull_request_is_read_as_a_verdict() {
    let said = |ok: bool, out: &str| Said {
        ok,
        out: out.to_owned(),
    };
    assert_eq!(merged(Some(&said(true, "MERGED\n"))), Verdict::Done);
    assert_eq!(merged(Some(&said(true, "OPEN\n"))), Verdict::NotDone);
    assert_eq!(
        merged(Some(&said(false, ""))),
        Verdict::Unknown,
        "a failed ask can't tell"
    );
    assert_eq!(merged(None), Verdict::Unknown, "nor one that never ran");
}

#[test]
fn a_sign_in_is_asked_only_of_the_tools_this_node_knows() {
    assert_eq!(
        signing_in(&json!({"tool": "gh"})),
        Some(("gh", vec!["auth".to_owned(), "status".to_owned()]))
    );
    assert_eq!(
        signing_in(&json!({"tool": "gcloud"})),
        Some((
            "gcloud",
            ["auth", "print-access-token", "-q"]
                .map(str::to_owned)
                .to_vec()
        )),
        "asked quietly, so it never waits on the person"
    );
    assert_eq!(signing_in(&json!({"tool": "rm"})), None);
    let said = |ok: bool| Said {
        ok,
        out: String::new(),
    };
    assert_eq!(signed_in(Some(&said(true))), Verdict::Done);
    assert_eq!(signed_in(Some(&said(false))), Verdict::NotDone);
    assert_eq!(signed_in(None), Verdict::Unknown);
}

#[test]
fn each_probe_is_told_at_once_or_asked_of_its_program_as_its_name_says() {
    let system = Path::new("/nix/store/aaaa-system");
    let machine = Machine {
        boot: Some(BOOT),
        system: Some(system),
        now: NOW,
    };
    let told = |when: Value| match probe(&when, &machine) {
        Probe::Told {
            verdict,
            may_change,
        } => Some((verdict, may_change)),
        Probe::Asked(_) => None,
    };
    let asked = |when: Value| match probe(&when, &machine) {
        Probe::Asked(asking) => Some((
            asking.program,
            asking.words.first().cloned(),
            asking.reads_output,
            asking.lasting,
        )),
        Probe::Told { .. } => None,
    };
    assert_eq!(
        told(json!({"probe": "rebooted", "boot_id": OTHER_BOOT})),
        Some((Verdict::Done, false)),
        "a reboot is this boot's for as long as the node runs"
    );
    assert_eq!(
        told(json!({"probe": "switch-done", "wanted": system})),
        Some((Verdict::Done, true)),
        "a switch may be made while the node runs"
    );
    assert_eq!(
        told(json!({"probe": "message-posted", "verdict": {"done": true, "at": said_at(NOW)}})),
        Some((Verdict::Done, true)),
        "a verdict comes of age"
    );
    assert_eq!(
        asked(json!({"probe": "pr-merged", "repo": "a/b", "number": 1})),
        Some(("gh", Some("pr".to_owned()), true, true)),
        "a merge is asked of GitHub, read, and stays done"
    );
    assert_eq!(
        asked(json!({"probe": "signed-in", "tool": "gcloud"})),
        Some(("gcloud", Some("auth".to_owned()), false, false)),
        "a sign-in's output is never kept, and it may lapse"
    );
    assert_eq!(
        told(json!({"probe": "pr-merged", "repo": "--x/y", "number": 1})),
        Some((Verdict::Unknown, false)),
        "one that would ask nothing can't tell"
    );
    assert_eq!(
        told(json!({"probe": "looked"})),
        Some((Verdict::Unknown, false)),
        "a probe this node doesn't know keeps the wait, as only done takes it away"
    );
    assert_eq!(
        act(&json!({"doneWhen": "posted"})),
        None,
        "an act is an object"
    );
}
