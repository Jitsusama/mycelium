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

/// The counts a Mac's Mach host keeps of each core's time, in its
/// `CPU_STATE_*` order: user, system, idle and nice.
pub type MachCore = [u32; 4];

/// How many counts the Mach host keeps of each core: `CPU_STATE_MAX`.
const MACH_STATES: usize = 4;

/// The first `processors` cores' counts in `counts`, the words the Mach
/// host lends, [`MACH_STATES`] to a core. A core the words run out
/// before is left out, and so are words past the last core.
#[must_use]
pub fn mach_cores(counts: &[u32], processors: usize) -> Vec<MachCore> {
    counts
        .chunks_exact(MACH_STATES)
        .take(processors)
        .filter_map(|core| MachCore::try_from(core).ok())
        .collect()
}

/// The counts of every core a Mac's Mach host says, summed. Each core's
/// count is its own 32 bits and wraps after about 490 days at 100 a
/// second; a sum gone backwards starts the stretch again, as on Linux.
#[must_use]
pub fn mac(cores: &[MachCore]) -> Ticks {
    cores.iter().fold(
        Ticks { busy: 0, total: 0 },
        |sum, &[user, system, idle, nice]| {
            let busy = u64::from(user) + u64::from(system) + u64::from(nice);
            Ticks {
                busy: sum.busy + busy,
                total: sum.total + busy + u64::from(idle),
            }
        },
    )
}

/// How finely use is said: in hundredths.
const HUNDREDTHS: f64 = 100.0;

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
            // To the hundredth, so a machine's record changes only when
            // its use does, not at every measure.
            self.said = Some((share * HUNDREDTHS).round() / HUNDREDTHS);
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

    #[test]
    fn a_macs_cores_count_busy_and_idle_time() {
        // User, system, idle and nice, for each of two cores.
        let cores = [[10, 5, 80, 5], [20, 0, 70, 10]];
        assert_eq!(
            mac(&cores),
            Ticks {
                busy: 10 + 5 + 5 + 20 + 10,
                total: 200,
            },
            "nice is busy, as on Linux"
        );
        assert_eq!(mac(&[]), Ticks { busy: 0, total: 0 }, "no cores said");
        assert_eq!(
            mac(&[[u32::MAX; 4]; 2]),
            Ticks {
                busy: 6 * u64::from(u32::MAX),
                total: 8 * u64::from(u32::MAX),
            },
            "each core's 32 bits sum past 32 bits"
        );
    }

    #[test]
    fn the_mach_hosts_words_are_cut_into_its_cores() {
        let words = [1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(
            mach_cores(&words, 2),
            [[1, 2, 3, 4], [5, 6, 7, 8]],
            "four counts to a core, in order"
        );
        assert_eq!(
            mach_cores(&words, 1),
            [[1, 2, 3, 4]],
            "words past the cores said are left"
        );
        assert_eq!(
            mach_cores(&words[..6], 2),
            [[1, 2, 3, 4]],
            "a core the words run out before is left out"
        );
        assert_eq!(mach_cores(&[], 4), Vec::<MachCore>::new(), "no words");
    }

    #[test]
    fn a_macs_core_wrapping_starts_the_stretch_again() {
        let start = Instant::now();
        let mut sampler = Sampler::default();
        let near = u32::MAX - 100;
        sampler.take(start, mac(&[[near, 0, near, 0]]));
        let said = sampler.take(start + WINDOW, mac(&[[near + 50, 0, near + 50, 0]]));
        assert_eq!(said, Some(0.5), "half busy before the wrap");
        assert_eq!(
            sampler.take(start + WINDOW * 2, mac(&[[10, 0, 90, 0]])),
            Some(0.5),
            "its 32 bits wrapped and the sum went back: keep what was said"
        );
        assert_eq!(
            sampler.take(start + WINDOW * 3, mac(&[[30, 0, 170, 0]])),
            Some(0.2),
            "measured again from the count after the wrap"
        );
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
    fn use_is_said_to_the_hundredth() {
        // Said finer, a machine's record would differ at every measure,
        // and every watcher would hear a change that moves nothing.
        let start = Instant::now();
        let mut sampler = Sampler::default();
        sampler.take(start, at(0, 0));
        assert_eq!(
            sampler.take(start + WINDOW, at(1, 3)),
            Some(0.33),
            "a third, to the hundredth"
        );
        assert_eq!(
            sampler.take(start + WINDOW * 2, at(1678, 3003)),
            Some(0.56),
            "1677 of 3000 rounds up"
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
