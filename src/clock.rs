//! Monotonic clocks and cancellation primitives.
//!
//! Production uses [`MonoClock`] and [`SignalStop`]; tests inject their own
//! implementations so timeouts and deadlines stay deterministic and offline.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// Reads a monotonic clock. Tests advance a fake clock instead of sleeping.
pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;
}

/// Wall-independent monotonic time since the process started the clock.
pub struct MonoClock {
    start: Instant,
}

impl MonoClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for MonoClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for MonoClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
}

/// Cooperative cancellation checked between reads, writes and waits.
pub trait Cancellation: Send + Sync {
    fn cancelled(&self) -> bool;
}

impl Cancellation for AtomicBool {
    fn cancelled(&self) -> bool {
        self.load(Ordering::SeqCst)
    }
}

/// A stop source that can be waited on with a bounded timeout.
pub trait Stop: Send + Sync {
    fn is_set(&self) -> bool;
    fn wait(&self, timeout: Duration);
}

/// Longest signal-polling slice; SIGTERM is observed within this bound.
pub const WAIT_SLICE: Duration = Duration::from_millis(20);

static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_signal: libc::c_int) {
    SIGNALLED.store(true, Ordering::SeqCst);
}

/// SIGTERM/SIGINT handler that only stores a flag, then cooperatively aborts
/// reads, writes and sleeps through [`Stop::wait`].
pub struct SignalStop;

impl SignalStop {
    pub fn install() -> anyhow::Result<()> {
        SIGNALLED.store(false, Ordering::SeqCst);
        for signal in [libc::SIGTERM, libc::SIGINT] {
            // SAFETY: the handler only touches an atomic flag, which is
            // async-signal-safe, and installs a no-op trampoline for these
            // two termination signals without touching signal masks.
            let previous =
                unsafe { libc::signal(signal, handle_signal as *const () as libc::sighandler_t) };
            if previous == libc::SIG_ERR {
                anyhow::bail!(
                    "cannot install handler for signal {signal}: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
        Ok(())
    }

    /// Clears the process-wide flag. Only used by tests and startup.
    pub fn reset(&self) {
        SIGNALLED.store(false, Ordering::SeqCst);
    }
}

impl Cancellation for SignalStop {
    fn cancelled(&self) -> bool {
        SIGNALLED.load(Ordering::SeqCst)
    }
}

impl Stop for SignalStop {
    fn is_set(&self) -> bool {
        self.cancelled()
    }

    fn wait(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while !self.is_set() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            thread::sleep(remaining.min(WAIT_SLICE));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_stop_sets_flag_on_sigterm() {
        let stop = SignalStop;
        stop.reset();
        std::sync::atomic::AtomicBool::store(&SIGNALLED, false, Ordering::SeqCst);
        SignalStop::install().expect("handler install");
        // SAFETY: the test installed a handler above; the default action no
        // longer terminates the process.
        unsafe {
            libc::raise(libc::SIGTERM);
        }
        assert!(stop.is_set(), "SIGTERM should set the stop flag");
        stop.reset();
        assert!(!stop.is_set());
    }

    #[test]
    fn stop_wait_returns_within_slice_bound() {
        let stop = SignalStop;
        stop.reset();
        let started = Instant::now();
        stop.wait(Duration::from_millis(120));
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(110), "{elapsed:?}");
        assert!(elapsed < Duration::from_millis(400), "{elapsed:?}");
    }
}
