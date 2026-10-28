//! Telling a store's reader the moment one of this machine's sessions ends.
//!
//! A session's file stays after its process ends, so nothing in the folder
//! changes when it does. On Linux each living session's process is held by
//! a pidfd, which becomes readable as the process exits; one thread waits on
//! them all at once, so the end is told at once and nothing is judged on a
//! timer. Elsewhere every session is judged again every few seconds, until a
//! Mac has its own way to be told.

use std::io;

use tokio::sync::mpsc;

use super::Change;

pub(super) use watch::Ends;

#[cfg(target_os = "linux")]
mod watch {
    use std::collections::HashMap;
    use std::os::fd::OwnedFd;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::thread::JoinHandle;

    use rustix::event::epoll::{self, CreateFlags, EventData, EventFlags};
    use rustix::event::{EventfdFlags, eventfd};
    use rustix::process::{Pid, PidfdFlags, pidfd_open};

    use super::{Change, io, mpsc};

    /// The key the waiting thread is woken with to stop; no process id is
    /// so large.
    const STOP: u64 = u64::MAX;

    /// How many ends the waiting thread takes at a time.
    const AT_ONCE: usize = 64;

    /// The processes watched for one reader of a store's changes, each
    /// watch ended when this is dropped.
    pub(in crate::store) struct Ends {
        told: mpsc::UnboundedSender<Change>,
        epoll: Arc<OwnedFd>,
        stop: OwnedFd,
        /// Each process held, by its id, so none is watched twice and each
        /// is let go once it has ended.
        watched: Arc<Mutex<HashMap<i32, OwnedFd>>>,
        waiting: Option<JoinHandle<()>>,
    }

    impl Ends {
        /// Tells each end to `told`.
        pub(in crate::store) fn new(told: mpsc::UnboundedSender<Change>) -> io::Result<Self> {
            let epoll = Arc::new(epoll::create(CreateFlags::CLOEXEC)?);
            let stop = eventfd(0, EventfdFlags::CLOEXEC)?;
            epoll::add(&epoll, &stop, EventData::new_u64(STOP), EventFlags::IN)?;
            let watched: Arc<Mutex<HashMap<i32, OwnedFd>>> = Arc::default();
            let waiting = {
                let (epoll, watched, told) =
                    (Arc::clone(&epoll), Arc::clone(&watched), told.clone());
                std::thread::Builder::new()
                    .name("mycelium-ends".to_owned())
                    .spawn(move || wait(&epoll, &watched, &told))?
            };
            Ok(Self {
                told,
                epoll,
                stop,
                watched,
                waiting: Some(waiting),
            })
        }

        /// Watches process `pid`, which `lives` says is still the
        /// session's, and tells once it has ended. Should it end before the
        /// watch begins, or its id now name another process, that is told
        /// at once.
        pub(in crate::store) fn watch(&self, pid: i32, lives: impl FnOnce() -> bool) {
            let mut watched = self.watched.lock().unwrap_or_else(PoisonError::into_inner);
            if watched.contains_key(&pid) {
                return;
            }
            // The process is held before it is judged, so it can't end and
            // its id be taken by another between the two.
            let held = Pid::from_raw(pid)
                .and_then(|id| pidfd_open(id, PidfdFlags::empty()).ok())
                .filter(|_| lives());
            let added = held.and_then(|held| {
                let key = EventData::new_u64(u64::try_from(pid).ok()?);
                epoll::add(
                    &*self.epoll,
                    &held,
                    key,
                    EventFlags::IN | EventFlags::ONESHOT,
                )
                .ok()?;
                Some(held)
            });
            match added {
                Some(held) => {
                    watched.insert(pid, held);
                }
                // Nobody receives once the store's reader has gone.
                None => {
                    let _ = self.told.send(Change::Presence);
                }
            }
        }
    }

    /// Waits on every held process, letting go of each as it ends and
    /// telling it, until woken to stop.
    fn wait(
        epoll: &OwnedFd,
        watched: &Mutex<HashMap<i32, OwnedFd>>,
        told: &mpsc::UnboundedSender<Change>,
    ) {
        let mut ends = Vec::with_capacity(AT_ONCE);
        loop {
            ends.clear();
            match epoll::wait(epoll, rustix::buffer::spare_capacity(&mut ends), None) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                // An epoll that can't be waited on tells nothing more; the
                // store's reader still hears every change to the folder.
                Err(_) => return,
            }
            for end in &ends {
                let key = end.data.u64();
                if key == STOP {
                    return;
                }
                if let Ok(pid) = i32::try_from(key) {
                    // Let go, the pidfd leaves the epoll with it.
                    watched
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&pid);
                }
                if told.send(Change::Presence).is_err() {
                    return;
                }
            }
        }
    }

    impl Drop for Ends {
        fn drop(&mut self) {
            // Should the wake fail, the thread ends with the reader's
            // channel at the next process to end.
            let _ = rustix::io::write(&self.stop, &1u64.to_ne_bytes());
            if let Some(waiting) = self.waiting.take() {
                let _ = waiting.join();
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod watch {
    use std::time::Duration;

    use tokio::runtime::Handle;
    use tokio::task::AbortHandle;

    use super::{Change, io, mpsc};

    /// How often the sessions are judged again where no process can be
    /// watched.
    const JUDGED_EVERY: Duration = Duration::from_secs(5);

    /// Where no process can be watched, the task judging every session
    /// again on a timer, stopped when this is dropped.
    pub(in crate::store) struct Ends {
        judging: AbortHandle,
    }

    impl Ends {
        /// Tells `told` every few seconds to judge the sessions again, from
        /// a task on the runtime this is made in.
        pub(in crate::store) fn new(told: mpsc::UnboundedSender<Change>) -> io::Result<Self> {
            let runtime = Handle::try_current().map_err(io::Error::other)?;
            let judging = runtime.spawn(async move {
                let mut every = tokio::time::interval(JUDGED_EVERY);
                loop {
                    every.tick().await;
                    if told.send(Change::Presence).is_err() {
                        break;
                    }
                }
            });
            Ok(Self {
                judging: judging.abort_handle(),
            })
        }

        /// Nothing to watch: the timer judges every session.
        #[expect(
            clippy::unused_self,
            reason = "the same call watches a process where one can be watched"
        )]
        pub(in crate::store) fn watch(&self, _pid: i32, _lives: impl FnOnce() -> bool) {}
    }

    impl Drop for Ends {
        fn drop(&mut self) {
            self.judging.abort();
        }
    }
}
