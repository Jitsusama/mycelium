//! The system's allocator, counting what is held and the most held, and
//! the processor's time for this thread, which the bars measure by. Only
//! one allocator may count a program's memory, so each test using this
//! is a program of its own.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// The system's allocator, counting.
struct Counting;

/// How many bytes are held now.
pub static HELD: AtomicUsize = AtomicUsize::new(0);

/// The most bytes held since it was last reset.
pub static PEAK: AtomicUsize = AtomicUsize::new(0);

/// That `bytes` more are held.
fn took(bytes: usize) {
    let held = HELD.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(held, Ordering::Relaxed);
}

/// That `bytes` fewer are held.
fn gave(bytes: usize) {
    HELD.fetch_sub(bytes, Ordering::Relaxed);
}

// SAFETY: each call is the system allocator's own, only counted.
#[expect(unsafe_code, reason = "an allocator is unsafe to implement")]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller keeps `alloc`'s contract, which this passes on.
        let made = unsafe { System.alloc(layout) };
        if !made.is_null() {
            took(layout.size());
        }
        made
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller keeps `dealloc`'s contract, which this passes on.
        unsafe { System.dealloc(ptr, layout) };
        gave(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller keeps `realloc`'s contract, which this passes on.
        let made = unsafe { System.realloc(ptr, layout, new_size) };
        if !made.is_null() {
            took(new_size);
            gave(layout.size());
        }
        made
    }
}

/// Every allocation this test makes, counted.
#[global_allocator]
static COUNTING: Counting = Counting;

/// How long the processor has run this thread, which waiting for it
/// doesn't count towards.
pub fn thread_time() -> Duration {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::ThreadCPUTime);
    Duration::new(
        u64::try_from(now.tv_sec).unwrap_or(0),
        u32::try_from(now.tv_nsec).unwrap_or(0),
    )
}
