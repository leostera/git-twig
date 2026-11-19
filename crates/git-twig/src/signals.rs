//! Native creation defers SIGHUP, SIGINT and SIGTERM until Git registers the worktree
//! or falls back. Until then nothing refers to staging, so an interrupt removes it first.
//! SIGKILL cannot be deferred; its staging is retained like any other crash.
use std::sync::atomic::{AtomicI32, Ordering};

#[cfg(unix)]
const SIGNALS: [libc::c_int; 3] = [libc::SIGHUP, libc::SIGINT, libc::SIGTERM];
#[cfg(unix)]
static DEFERRED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static RECEIVED: AtomicI32 = AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn record(signal: libc::c_int) {
    // Async-signal-safe: a single lock-free atomic store.
    RECEIVED.store(signal, Ordering::SeqCst);
}

/// Record termination signals instead of dying. Ignored or handled signals are left alone.
pub(super) fn defer() {
    #[cfg(unix)]
    {
        for signal in SIGNALS {
            // SAFETY: a zeroed sigaction is valid; sigaction only reads and writes it,
            // and the installed handler is async-signal-safe.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(signal, std::ptr::null(), &mut action) != 0
                    || action.sa_sigaction != libc::SIG_DFL
                {
                    continue;
                }
                // Without SA_RESTART, blocking calls that do not retry EINTR fail early.
                action.sa_sigaction = record as extern "C" fn(libc::c_int) as libc::sighandler_t;
                action.sa_flags = 0;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(signal, &action, std::ptr::null_mut()) == 0 {
                    DEFERRED.fetch_or(1 << signal, Ordering::SeqCst);
                }
            }
        }
    }
}

/// The signal that arrived while deferred, if any.
pub(super) fn received() -> Option<i32> {
    match RECEIVED.load(Ordering::SeqCst) {
        0 => None,
        signal => Some(signal),
    }
}

/// Stop deferring: later signals terminate as before. Returns a signal that already arrived.
pub(super) fn resume() -> Option<i32> {
    #[cfg(unix)]
    {
        let deferred = DEFERRED.swap(0, Ordering::SeqCst);
        for signal in SIGNALS {
            if deferred & (1 << signal) != 0 {
                // SAFETY: reinstates the default disposition that defer() replaced.
                unsafe {
                    libc::signal(signal, libc::SIG_DFL);
                }
            }
        }
    }
    received()
}

/// After resume(), terminate by the signal the way its default disposition would have.
pub(super) fn raise(signal: i32) -> ! {
    #[cfg(unix)]
    {
        // SAFETY: raise only delivers the signal to this thread.
        unsafe {
            libc::raise(signal);
        }
    }
    std::process::exit(128 + signal)
}
