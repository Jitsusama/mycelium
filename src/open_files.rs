//! Raising the node's own limit on open files to all the system lets it
//! have. A node holds a descriptor for each link, each session's hold,
//! each watch and each client, so with seventy sessions and their asks it
//! runs past a thousand. The soft limit it starts with is the service
//! manager's, which is 1024 under systemd unless the unit raises it and
//! 256 under launchd, while the hard limit above it is far higher. A
//! process may raise its own soft limit as far as its hard one without
//! any privilege, so the node does, as Go's runtime does for every Go
//! program, rather than leaving its reach to whatever its unit says.

use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

/// The most a soft limit on open files may be on macOS, whose kernel
/// refuses a soft limit above it, an unlimited one included, whatever
/// the hard limit says.
const MACOS_CEILING: u64 = 10_240;

/// What the soft limit on open files was raised from and to, where it
/// was raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Raised {
    /// The soft limit before, or none where it was unlimited.
    pub from: Option<u64>,
    /// The soft limit after, or none where it is unlimited.
    pub to: Option<u64>,
}

/// Raises this process's soft limit on open files to its hard limit, or
/// to the most this system takes where that is lower, answering what it
/// was raised from and to, or none where it was already there. A refusal
/// is the system's error, and leaves the limit as it was.
///
/// # Errors
///
/// Where the system refuses the new limit.
pub fn raise() -> std::io::Result<Option<Raised>> {
    let ceiling = cfg!(target_os = "macos").then_some(MACOS_CEILING);
    let now = getrlimit(Resource::Nofile);
    let Some(new) = raised(&now, ceiling) else {
        return Ok(None);
    };
    let to = new.current;
    setrlimit(Resource::Nofile, new)?;
    Ok(Some(Raised {
        from: now.current,
        to,
    }))
}

/// The limit `now` raised as far as it may go, its hard limit kept: its
/// soft limit up to its hard one, or to `ceiling` where that is lower;
/// none where the soft limit is that already, or above, which is never
/// lowered.
fn raised(now: &Rlimit, ceiling: Option<u64>) -> Option<Rlimit> {
    let most = match (now.maximum, ceiling) {
        (Some(maximum), Some(ceiling)) => Some(maximum.min(ceiling)),
        (None, ceiling) => ceiling,
        (maximum, None) => maximum,
    };
    let higher = match (now.current, most) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(current), Some(most)) => most > current,
    };
    higher.then_some(Rlimit {
        current: most,
        maximum: now.maximum,
    })
}

#[cfg(test)]
mod tests {
    use super::{Raised, Rlimit, raise, raised};

    fn limit(current: Option<u64>, maximum: Option<u64>) -> Rlimit {
        Rlimit { current, maximum }
    }

    #[test]
    fn a_soft_limit_goes_up_to_the_hard_one() {
        assert_eq!(
            raised(&limit(Some(1024), Some(524_288)), None),
            Some(limit(Some(524_288), Some(524_288))),
            "systemd's soft 1024 rises to its hard limit"
        );
        assert_eq!(
            raised(&limit(Some(1024), None), None),
            Some(limit(None, None)),
            "an unlimited hard limit leaves the soft one unlimited too"
        );
    }

    #[test]
    fn a_ceiling_holds_it_below_an_unlimited_or_higher_hard_limit() {
        assert_eq!(
            raised(&limit(Some(256), None), Some(10_240)),
            Some(limit(Some(10_240), None)),
            "launchd's soft 256 under an unlimited hard limit stops at the ceiling"
        );
        assert_eq!(
            raised(&limit(Some(256), Some(4096)), Some(10_240)),
            Some(limit(Some(4096), Some(4096))),
            "a hard limit below the ceiling is the most"
        );
    }

    #[test]
    fn a_limit_already_as_high_is_left_and_none_is_lowered() {
        assert_eq!(
            raised(&limit(Some(524_288), Some(524_288)), None),
            None,
            "already at its hard limit"
        );
        assert_eq!(
            raised(&limit(Some(65_536), Some(524_288)), Some(10_240)),
            None,
            "a soft limit above the ceiling isn't lowered to it"
        );
        assert_eq!(
            raised(&limit(None, None), Some(10_240)),
            None,
            "an unlimited soft limit isn't lowered to the ceiling"
        );
    }

    #[test]
    fn raising_twice_raises_once() {
        let first: Option<Raised> = raise().expect("a process may raise its own soft limit");
        assert_eq!(
            raise().expect("and ask again"),
            None,
            "the second finds it raised already, after {first:?}"
        );
    }
}
