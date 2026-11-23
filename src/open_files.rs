//! Raising the node's own limit on open files to all the system lets it
//! have. A node holds a descriptor for each link, each session's hold,
//! each watch and each client, so with seventy sessions and their asks it
//! runs past a thousand. The soft limit it starts with is the service
//! manager's, which is 1024 under systemd unless the unit raises it and
//! 256 under launchd, while the hard limit above it is far higher. A
//! process may raise its own soft limit as far as its hard one without
//! any privilege, so the node does, as Go's runtime does for every Go
//! program, rather than leaving its reach to whatever its unit says.
//!
//! What the node starts is given back the limit the node was started
//! with ([`as_started`]), as Go's runtime does for what it starts too: a
//! program that waits on its descriptors with `select` can't take one
//! numbered past 1024, and a session's tools are no business of the
//! node's limit.

use std::sync::OnceLock;

use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

/// The most a soft limit on open files may be on macOS where the kernel
/// won't say its own most: `OPEN_MAX`, which every Mac takes.
#[cfg(target_os = "macos")]
const MACOS_OPEN_MAX: u64 = 10_240;

/// This process's limit on open files as it started, kept by the first
/// [`raise`].
static STARTED_WITH: OnceLock<Rlimit> = OnceLock::new();

/// What the soft limit on open files was raised from and to, where it
/// was raised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Raised {
    /// The soft limit before, or none where it was unlimited.
    pub from: Option<u64>,
    /// The soft limit after, as the kernel says it set it, or none where
    /// it is unlimited.
    pub to: Option<u64>,
}

/// Raises this process's soft limit on open files to its hard limit, or
/// to the most this system takes where that is lower, answering what it
/// was raised from and to, or none where it was already there. A refusal
/// is the system's error, naming the limit asked for, and leaves the
/// limit as it was.
///
/// # Errors
///
/// Where the system refuses the new limit.
pub fn raise() -> std::io::Result<Option<Raised>> {
    let now = getrlimit(Resource::Nofile);
    let started = STARTED_WITH.get_or_init(|| now);
    let Some(new) = raised(&now, ceiling()) else {
        return Ok(None);
    };
    let asked = new.current;
    setrlimit(Resource::Nofile, new).map_err(|error| {
        std::io::Error::new(
            std::io::Error::from(error).kind(),
            format!("raising it to {} failed: {error}", said(asked)),
        )
    })?;
    Ok(Some(Raised {
        from: started.current,
        to: getrlimit(Resource::Nofile).current,
    }))
}

/// Has `command` start with the limit on open files this process was
/// started with, where [`raise`] raised it since: set between its fork
/// and its exec, so this process's own stays as raised. A child whose
/// limit can't be lowered starts with this process's.
pub fn as_started(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;

    let Some(started) = STARTED_WITH.get().copied() else {
        return;
    };
    #[expect(
        unsafe_code,
        reason = "a child is given back its limit only between fork and exec"
    )]
    // SAFETY: setrlimit is one system call, safe between fork and exec: it
    // takes no lock and allocates nothing, and the limit is copied in
    // before the fork.
    unsafe {
        command.pre_exec(move || {
            let _ = setrlimit(Resource::Nofile, started);
            Ok(())
        });
    }
}

/// The limit as said: its number, or `unlimited` where it has none.
fn said(limit: Option<u64>) -> String {
    limit.map_or_else(|| "unlimited".to_owned(), |limit| limit.to_string())
}

/// The most this system takes as a soft limit on open files whatever the
/// hard limit says, where it has one: on macOS the kernel refuses a soft
/// limit above `kern.maxfilesperproc`, an unlimited one included, as Go's
/// runtime reads it too.
#[cfg(target_os = "macos")]
fn ceiling() -> Option<u64> {
    let mut most: libc::c_int = 0;
    let mut size = size_of::<libc::c_int>();
    #[expect(
        unsafe_code,
        reason = "macOS says its most open files per process only through sysctl"
    )]
    // SAFETY: the name is a NUL-ended string, and the value is read into
    // an int of the size given, only once sysctlbyname says it succeeded.
    let read = unsafe {
        libc::sysctlbyname(
            c"kern.maxfilesperproc".as_ptr(),
            (&raw mut most).cast(),
            &raw mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    let read = (read == 0).then_some(most);
    Some(
        read.and_then(|most| u64::try_from(most).ok())
            .filter(|most| *most > 0)
            .unwrap_or(MACOS_OPEN_MAX),
    )
}

#[cfg(not(target_os = "macos"))]
const fn ceiling() -> Option<u64> {
    None
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
    use super::{
        Resource, Rlimit, STARTED_WITH, as_started, ceiling, getrlimit, raise, raised, said,
    };

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
            raised(&limit(Some(256), None), Some(184_320)),
            Some(limit(Some(184_320), None)),
            "launchd's soft 256 under an unlimited hard limit stops at the ceiling"
        );
        assert_eq!(
            raised(&limit(Some(256), Some(4096)), Some(184_320)),
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

    /// The only test that raises this test process's own limit, so what
    /// it started with is known here.
    #[test]
    fn the_process_takes_all_it_may_and_what_it_starts_gets_what_it_started_with() {
        let before = getrlimit(Resource::Nofile);
        raise().expect("a process may raise its own soft limit");
        let expected = raised(&before, ceiling()).unwrap_or(before);
        assert_eq!(
            getrlimit(Resource::Nofile),
            expected,
            "the soft limit is all the hard limit and the system allow"
        );
        assert_eq!(
            raise().expect("and ask again"),
            None,
            "a second raise finds it raised already"
        );

        let started = STARTED_WITH.get().expect("kept by the first raise");
        let mut child = std::process::Command::new("sh");
        child.args(["-c", "ulimit -n"]);
        as_started(&mut child);
        let output = child.output().expect("sh runs");
        let child_said = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        assert_eq!(
            child_said,
            said(started.current),
            "a child starts with the limit this process started with"
        );
    }
}
