//! A quest's JOURNEY.md, merged by entry rather than kept whole.
//!
//! Every session on a quest adds to its journey, so two machines adding at
//! once is the usual case, not a conflict. The file is a heading, then one
//! entry per item, newest first: a line opening `- **YYYY-MM-DD**:`, and
//! whatever lines follow it until the next. Two ends are merged against
//! the last version both had: an entry either added is kept, one either
//! took out stays out, and the entries are ordered newest first again,
//! the other end's new ones above this end's on the same day.

/// The journey both ends' changes make, from `ours` and `theirs` and the
/// last version both had, `base`; with no `base`, the union of the two.
#[must_use]
pub fn merged(base: Option<&str>, ours: &str, theirs: &str) -> String {
    let (heading, our_entries) = split(ours);
    let (_, their_entries) = split(theirs);
    let base_entries = base.map(|base| split(base).1).unwrap_or_default();

    let added: Vec<&str> = their_entries
        .iter()
        .filter(|entry| !base_entries.contains(entry) && !our_entries.contains(entry))
        .copied()
        .collect();
    let taken_out = |entry: &&str| base_entries.contains(entry) && !their_entries.contains(entry);
    let mut entries: Vec<&str> = added;
    entries.extend(our_entries.iter().filter(|entry| !taken_out(entry)));
    // Newest first; a stable sort keeps each day's entries in the order
    // they were placed, the other end's new ones above this end's.
    entries.sort_by(|one, other| day(other).cmp(day(one)));

    let mut text = heading.to_owned();
    for entry in entries {
        text.push_str(entry);
    }
    text
}

/// The heading, everything before the first entry, and each entry with
/// the lines that follow it.
fn split(text: &str) -> (&str, Vec<&str>) {
    let starts: Vec<usize> = line_starts(text)
        .filter(|&start| is_entry(&text[start..]))
        .collect();
    let Some(&first) = starts.first() else {
        return (text, Vec::new());
    };
    let ends = starts.iter().skip(1).copied().chain([text.len()]);
    let entries = starts
        .iter()
        .zip(ends)
        .map(|(&start, end)| &text[start..end])
        .collect();
    (&text[..first], entries)
}

fn line_starts(text: &str) -> impl Iterator<Item = usize> + '_ {
    std::iter::once(0).chain(text.match_indices('\n').map(|(at, _)| at + 1))
}

fn is_entry(line: &str) -> bool {
    line.starts_with("- **") && line.get(14..17) == Some("**:")
}

/// The day an entry is dated, which sorts as text.
fn day(entry: &str) -> &str {
    entry.get(4..14).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADING: &str = "# 🌄 Journey\n\n";

    fn journey(entries: &[&str]) -> String {
        let mut text = HEADING.to_owned();
        for entry in entries {
            text.push_str(entry);
            text.push('\n');
        }
        text
    }

    #[test]
    fn what_each_end_added_is_kept_newest_first() {
        let base = journey(&["- **2026-10-25**: began"]);
        let ours = journey(&["- **2026-10-26**: ours", "- **2026-10-25**: began"]);
        let theirs = journey(&[
            "- **2026-10-27**: theirs, later",
            "- **2026-10-26**: theirs",
            "- **2026-10-25**: began",
        ]);
        assert_eq!(
            merged(Some(&base), &ours, &theirs),
            journey(&[
                "- **2026-10-27**: theirs, later",
                "- **2026-10-26**: theirs",
                "- **2026-10-26**: ours",
                "- **2026-10-25**: began",
            ]),
            "both ends' entries are kept, newest first, theirs above ours on a day"
        );
    }

    #[test]
    fn what_an_end_took_out_stays_out() {
        let base = journey(&["- **2026-10-26**: wrong", "- **2026-10-25**: began"]);
        let ours = base.clone();
        let theirs = journey(&["- **2026-10-25**: began"]);
        assert_eq!(
            merged(Some(&base), &ours, &theirs),
            theirs,
            "an entry the other end took out is not brought back"
        );
    }

    #[test]
    fn with_nothing_shared_it_is_the_union() {
        let ours = journey(&["- **2026-10-26**: ours", "- **2026-10-25**: both"]);
        let theirs = journey(&["- **2026-10-25**: both"]);
        assert_eq!(
            merged(None, &ours, &theirs),
            ours,
            "an entry both have is kept once"
        );
    }

    #[test]
    fn an_entry_keeps_the_lines_that_follow_it() {
        let ours = journey(&["- **2026-10-26**: one\n  more of it"]);
        let theirs = journey(&["- **2026-10-27**: two"]);
        assert_eq!(
            merged(None, &ours, &theirs),
            journey(&[
                "- **2026-10-27**: two",
                "- **2026-10-26**: one\n  more of it"
            ]),
            "a line that opens no entry belongs to the entry above it"
        );
    }
}
