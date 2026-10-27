//! A quest's harness.log, merged as the harness reads it.
//!
//! The harness writes one line per entry, each opening with the UTC time
//! to the second, oldest first, and keeps only the newest 2,000; readers
//! read its end. So two ends' logs merge as the union of their lines,
//! sorted by that time with each end's order kept within a second, each
//! line once, trimmed to the newest 2,000 as the harness would.

/// How many lines the harness keeps.
const KEPT: usize = 2_000;

/// The log both ends' lines make.
#[must_use]
pub fn merged(ours: &str, theirs: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in ours.lines().chain(theirs.lines()) {
        if !line.is_empty() && !lines.contains(&line) {
            lines.push(line);
        }
    }
    lines.sort_by_key(|line| stamp(line));
    let from = lines.len().saturating_sub(KEPT);
    let mut text = String::new();
    for line in &lines[from..] {
        text.push_str(line);
        text.push('\n');
    }
    text
}

/// The time a line opens with, `YYYY-MM-DDTHH:MM:SSZ`, which sorts as
/// text.
fn stamp(line: &str) -> &str {
    line.split_once(' ').map_or(line, |(stamp, _)| stamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_ends_lines_go_in_time_order_once_each() {
        let ours = "2026-10-27T00:00:01Z one\n2026-10-27T00:00:03Z three\n";
        let theirs = "2026-10-27T00:00:01Z one\n2026-10-27T00:00:02Z two\n";
        assert_eq!(
            merged(ours, theirs),
            "2026-10-27T00:00:01Z one\n2026-10-27T00:00:02Z two\n2026-10-27T00:00:03Z three\n",
            "lines are sorted by their time and a line both have is kept once"
        );
    }

    #[test]
    fn only_the_newest_lines_the_harness_keeps_are_kept() {
        let line = |second: usize| {
            format!(
                "2026-10-27T00:{:02}:{:02}Z line {second}\n",
                second / 60,
                second % 60
            )
        };
        let ours: String = (0..KEPT).map(line).collect();
        let theirs = line(KEPT);
        let log = merged(&ours, &theirs);
        assert_eq!(
            log.lines().count(),
            KEPT,
            "the log is trimmed to {KEPT} lines"
        );
        assert!(
            log.starts_with(&line(1)) && log.ends_with(&line(KEPT)),
            "the oldest line is the one let go"
        );
    }
}
