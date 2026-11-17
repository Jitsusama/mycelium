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

#[test]
fn a_reboot_is_done_once_the_boot_differs() {
    let when = json!({"probe": "rebooted", "boot_id": "then"});
    assert_eq!(rebooted(&when, Some("now")), Verdict::Done);
    assert_eq!(rebooted(&when, Some("then")), Verdict::NotDone);
    assert_eq!(
        rebooted(&when, None),
        Verdict::Unknown,
        "a machine naming no boot can't tell"
    );
    assert_eq!(
        rebooted(&json!({"probe": "rebooted"}), Some("now")),
        Verdict::Unknown,
        "nor a wait naming none"
    );
}

#[test]
fn a_switch_is_done_once_the_machine_runs_the_system_wanted() {
    let when = json!({"probe": "switch-done", "wanted": "/nix/store/aaaa-system"});
    let running = |path: &str| Some(PathBuf::from(path));
    assert_eq!(
        switched(&when, &running("/nix/store/aaaa-system")),
        Verdict::Done
    );
    assert_eq!(
        switched(&when, &running("/nix/store/bbbb-system")),
        Verdict::NotDone
    );
    assert_eq!(
        switched(&when, &None),
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
    assert!(signing_in(&json!({"tool": "gcloud"})).is_some());
    assert_eq!(signing_in(&json!({"tool": "rm"})), None);
    let said = |ok: bool| Said {
        ok,
        out: String::new(),
    };
    assert_eq!(signed_in(Some(&said(true))), Verdict::Done);
    assert_eq!(signed_in(Some(&said(false))), Verdict::NotDone);
    assert_eq!(signed_in(None), Verdict::Unknown);
}

#[tokio::test]
async fn a_probe_this_node_doesnt_know_keeps_the_wait() {
    assert_eq!(
        probe(&json!({"probe": "looked"})).await,
        Verdict::Unknown,
        "only done takes a wait away"
    );
    assert_eq!(
        act(&json!({"doneWhen": "posted"})),
        None,
        "an act is an object"
    );
}
