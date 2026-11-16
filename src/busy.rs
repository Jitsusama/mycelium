//! How busy a machine's processors were lately: the share of the time its
//! cores spent running work, from 0 to 1.
//!
//! This is processor use, not processor pressure. Pressure says how long
//! work waited for a core and is zero until every core is busy, so it can
//! tell a machine with none to spare but not which of two with some has
//! more; use says that. Place ranks by use and calls a machine over by
//! pressure, as the USE method keeps both.
//!
//! The system counts each core's time since it booted, so use is the
//! change between two counts. A [`Sampler`] keeps the last count and says
//! use over at least [`WINDOW`] since it, so measuring twice in a breath
//! neither blocks nor says the noise of a few milliseconds.

use std::time::{Duration, Instant};

/// The shortest stretch use is said over.
pub const WINDOW: Duration = Duration::from_secs(5);

/// What the system counted of all its cores' time: busy and in all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticks {
    /// Time spent running work, the kernel's own included.
    pub busy: u64,
    /// All time counted, idle and waiting on the disk included.
    pub total: u64,
}

/// The counts on `/proc/stat`'s first line, which sums every core's:
/// `user nice system idle iowait irq softirq steal`, then guest time,
/// which user and nice already hold. Time waiting on the disk is idle,
/// since a core waiting on the disk would run other work.
#[must_use]
pub fn linux(stat: &str) -> Option<Ticks> {
    let line = stat.lines().find_map(|line| line.strip_prefix("cpu "))?;
    let counts: Vec<u64> = line
        .split_whitespace()
        .take(COUNTED)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let &[user, nice, system, idle, iowait, irq, softirq, steal] = counts.as_slice() else {
        return None;
    };
    let busy = [user, nice, system, irq, softirq, steal]
        .into_iter()
        .try_fold(0u64, u64::checked_add)?;
    Some(Ticks {
        busy,
        total: busy.checked_add(idle)?.checked_add(iowait)?,
    })
}

/// How many of the first line's counts say where time went; those after
/// are guest time, counted again.
const COUNTED: usize = 8;

/// The last count, and the use said over the stretch before it.
#[derive(Debug, Default)]
pub struct Sampler {
    since: Option<(Instant, Ticks)>,
    said: Option<f64>,
}

impl Sampler {
    /// Takes the count `ticks` read at `now`, answering the use over the
    /// last stretch of at least [`WINDOW`], or none before one has passed.
    /// A count that went backwards, as when a core goes offline and its
    /// time leaves the sum, starts the stretch again.
    pub fn take(&mut self, now: Instant, ticks: Ticks) -> Option<f64> {
        let Some((then, before)) = self.since else {
            self.since = Some((now, ticks));
            return self.said;
        };
        if now.duration_since(then) < WINDOW {
            return self.said;
        }
        self.since = Some((now, ticks));
        let busy = ticks.busy.checked_sub(before.busy);
        let total = ticks.total.checked_sub(before.total);
        if let (Some(busy), Some(total)) = (busy, total)
            && total > 0
            && busy <= total
        {
            #[expect(
                clippy::cast_precision_loss,
                reason = "a few seconds of ticks is far under 2^52"
            )]
            let share = busy as f64 / total as f64;
            self.said = Some(share);
        }
        self.said
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "cpu  4705 356 584 3699 23 23 0 7 11 13\n\
                        cpu0 1393 179 270 1838 11 12 0 3 5 6\n\
                        intr 114930548 113199788 3 0 5 263 0 4 [...]\n";

    #[test]
    fn the_first_line_counts_busy_and_idle_time() {
        assert_eq!(
            linux(STAT),
            Some(Ticks {
                // User, nice, system, irq, softirq (none) and steal.
                busy: 4705 + 356 + 584 + 23 + 7,
                // Those, and idle and waiting on the disk.
                total: 5675 + 3699 + 23,
            }),
            "waiting on the disk is idle, and guest time is already in user"
        );
    }

    #[test]
    fn a_line_cut_short_or_not_numbers_counts_nothing() {
        assert_eq!(linux("cpu  4705 356\n"), None, "too few counts");
        assert_eq!(linux("cpu  a b c d e f g h\n"), None, "not numbers");
        assert_eq!(linux("intr 1 2 3\n"), None, "no processor line");
    }

    fn at(busy: u64, total: u64) -> Ticks {
        Ticks { busy, total }
    }

    #[test]
    fn use_is_said_over_a_stretch_of_at_least_the_window() {
        let start = Instant::now();
        let mut sampler = Sampler::default();
        assert_eq!(
            sampler.take(start, at(100, 1000)),
            None,
            "nothing to compare"
        );
        assert_eq!(
            sampler.take(start + Duration::from_secs(1), at(150, 1100)),
            None,
            "a second isn't a stretch"
        );
        assert_eq!(
            sampler.take(start + WINDOW, at(400, 2000)),
            Some(0.3),
            "300 of 1000 ticks busy since the first count"
        );
        assert_eq!(
            sampler.take(start + WINDOW + Duration::from_secs(1), at(700, 2500)),
            Some(0.3),
            "within the next window it says the last stretch's"
        );
        assert_eq!(
            sampler.take(start + WINDOW * 2, at(1400, 3000)),
            Some(1.0),
            "all 1000 ticks since the last stretch's count busy"
        );
    }

    #[test]
    fn a_count_gone_backwards_starts_the_stretch_again() {
        let start = Instant::now();
        let mut sampler = Sampler::default();
        sampler.take(start, at(100, 1000));
        let said = sampler.take(start + WINDOW, at(600, 2000));
        assert_eq!(said, Some(0.5), "half busy");
        assert_eq!(
            sampler.take(start + WINDOW * 2, at(500, 1900)),
            Some(0.5),
            "a core went offline and took its time with it: keep what was said"
        );
        assert_eq!(
            sampler.take(start + WINDOW * 3, at(1400, 2900)),
            Some(0.9),
            "measured again from the count after the drop"
        );
    }
}
