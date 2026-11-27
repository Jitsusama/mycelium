//! What a page drawn for a person prints into a pipe, as its own process
//! against a node: a time says its day, date and zone, as a copy kept
//! is read later or on a machine in another zone, where a time of day
//! alone says nothing.

use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};
use process::{made, mycelium};

mod process;

/// The zone the node's times are drawn in: a POSIX rule, which needs no
/// zone database, as the build's sandbox has none to look `UTC` up in,
/// and half an hour off UTC, so a stamp drawn in UTC regardless fails.
const ZONE: &str = "<+0530>-5:30";

/// How long the node has to start and measure its machine.
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
/// stamped as a pipe draws it; none until it has been measured.
fn measured(home: &Path) -> Option<String> {
    let json = printed(home, &["machines", "-o", "json"]);
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
    let (telling, told) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(telling).expect("a watcher");
    watcher
        .watch(home, RecursiveMode::Recursive)
        .expect("the home is watched");
    let deadline = Instant::now() + PATIENCE;
    let (before, page, after) = loop {
        let before = measured(home);
        let page = printed(home, &["machines", "kestrel", "-o", "text"]);
        let after = measured(home);
        if before.is_some() && line(&page, "measured").is_some() {
            break (before, page, after);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "kestrel was never measured:\n{page}");
        // A watch that fails or times out has the node asked again.
        let _ = told.recv_timeout(left);
    };
    drop(node);

    let said = line(&page, "measured").map(str::to_owned);
    assert!(
        said == before || said == after,
        "into a pipe, the time kestrel was measured wasn't stamped as {before:?} or \
         {after:?}:\n{page}"
    );
}
