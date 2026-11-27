//! What a page drawn for a person prints into a pipe, as its own process
//! against a node: a time says its day, date and zone, as a copy kept
//! is read later or on a machine in another zone, where a time of day
//! alone says nothing.

use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::Duration;

use process::{made, mycelium};

mod process;

/// The zone the node's times are drawn in: a POSIX rule, which needs no
/// zone database, as the build's sandbox has none to look `UTC` up in,
/// and half an hour off UTC, so a stamp drawn in UTC regardless fails.
const ZONE: &str = "<+0530>-5:30";

/// How long the node has to start.
const PATIENCE: Duration = Duration::from_secs(30);

/// A node that is stopped once the test is done with it, however it ends.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// What `mycelium` prints with `args` against the node at `home`, its
/// times in [`ZONE`], so a stamp can be said exactly.
fn printed(home: &Path, args: &[&str]) -> String {
    let ran = mycelium(home, args)
        .env("TZ", ZONE)
        .output()
        .expect("mycelium runs");
    assert!(ran.status.success(), "{args:?} failed: {ran:?}");
    String::from_utf8(ran.stdout).expect("UTF-8")
}

/// When kestrel was last measured, as the node at `home` says in JSON,
/// stamped as a pipe draws it; the JSON itself if it doesn't say, so a
/// failure shows what it said instead.
fn measured(home: &Path) -> Result<String, String> {
    let json = printed(home, &["machines", "-o", "json"]);
    stamp(&json).ok_or(json)
}

/// When kestrel was measured, from the first line of the node's `json`.
fn stamp(json: &str) -> Option<String> {
    let said: serde_json::Value = serde_json::from_str(json.lines().next()?).ok()?;
    let at = said["Ok"]["Machines"]
        .as_array()?
        .iter()
        .find(|one| one["name"] == "kestrel")?["measured_at"]
        .as_str()?
        .parse::<jiff::Timestamp>()
        .ok()?;
    Some(
        at.to_zoned(jiff::tz::TimeZone::posix(ZONE).expect("a POSIX zone"))
            .strftime("%a %-d %b %Y %H:%M %Z (%:z)")
            .to_string(),
    )
}

/// The value of the line `label` begins on `page`, if it has one.
fn line<'a>(page: &'a str, label: &str) -> Option<&'a str> {
    page.lines()
        .find_map(|line| line.trim_start().strip_prefix(label))
        .map(str::trim)
}

#[test]
fn into_a_pipe_a_machine_says_when_it_was_measured_with_its_day_and_zone() {
    let home = tempfile::tempdir().expect("a home");
    let home = home.path();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home.join("r"))
        .expect("its runtime folder is made");
    let node = Running(
        mycelium(home, &["node", "--machine", "kestrel"])
            .stderr(Stdio::null())
            .spawn()
            .expect("the node starts"),
    );
    made(
        &home.join("r"),
        &home.join("r").join("mycelium").join("work.sock"),
        PATIENCE,
    );
    // The node measures its own machine each time it is asked, so the
    // first page already says when, and nothing is waited for. Each ask
    // measures again, so the page's minute is the one read before it or
    // the one read after, whichever side of a minute it fell.
    let before = measured(home);
    let page = printed(home, &["machines", "kestrel", "-o", "text"]);
    let after = measured(home);
    drop(node);

    let (Ok(before), Ok(after)) = (before, after) else {
        panic!("the JSON didn't say when kestrel was measured: {before:?} {after:?}");
    };
    let said = line(&page, "measured").map(str::to_owned);
    assert!(said.is_some(), "the page didn't say when:\n{page}");
    let said = said.as_deref();
    assert!(
        said == Some(before.as_str()) || said == Some(after.as_str()),
        "into a pipe, the time kestrel was measured wasn't stamped as {before:?} or \
         {after:?}:\n{page}"
    );
}
