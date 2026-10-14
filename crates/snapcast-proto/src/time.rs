//! Monotonic clock — the single source of truth for time synchronization.
//!
//! The client estimates the constant offset between its clock and the
//! server's, so the two ends need not share a clock domain; what matters is
//! that each end timestamps everything (time exchanges, audio chunks) with
//! one clock that never jumps. This module is that clock: `snapcast-client`
//! and `snapcast-server` both call [`now_usec`] rather than each carrying
//! their own copy of the platform FFI.
//!
//! Matches C++ `std::chrono::steady_clock`:
//! - macOS: `mach_continuous_time`, scaled by `mach_timebase_info`.
//! - Linux / other Unix: `CLOCK_MONOTONIC` via `clock_gettime`.
//! - Non-Unix: `std::time::Instant` (`QueryPerformanceCounter` on Windows,
//!   as C++), offset to the wall clock at first use so values look alike.

/// Current monotonic time in microseconds.
///
/// Equivalent to the C++ `chronos::steadytimeofday` — microseconds on a
/// monotonic timeline (since boot on Unix). Use this for every timestamp
/// sent to the peer.
pub fn now_usec() -> i64 {
    monotonic_usec()
}

/// Monotonic microsecond clock.
#[allow(unsafe_code)] // FFI: mach_continuous_time (macOS), clock_gettime (Linux)
fn monotonic_usec() -> i64 {
    #[cfg(target_os = "macos")]
    {
        // macOS: C++ steady_clock uses mach_continuous_time, not
        // CLOCK_MONOTONIC; unlike it, this keeps counting during sleep.
        unsafe extern "C" {
            fn mach_continuous_time() -> u64;
            fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
        }
        #[repr(C)]
        struct MachTimebaseInfo {
            numer: u32,
            denom: u32,
        }
        static TIMEBASE: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
        let (numer, denom) = *TIMEBASE.get_or_init(|| {
            let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
            unsafe {
                mach_timebase_info(&mut info);
            }
            (info.numer, info.denom)
        });
        let ticks = unsafe { mach_continuous_time() };
        let nanos = ticks as i128 * numer as i128 / denom as i128;
        (nanos / 1_000) as i64
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: clock_gettime with CLOCK_MONOTONIC and a valid timespec pointer is sound.
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
        }
        ts.tv_sec * 1_000_000 + ts.tv_nsec / 1_000
    }
    #[cfg(not(unix))]
    {
        // The wall clock jumps when NTP steps it, so only its reading at
        // first use is taken; the monotonic Instant counts from there.
        use std::time::{Instant, SystemTime, UNIX_EPOCH};
        static ANCHOR: std::sync::OnceLock<(Instant, i64)> = std::sync::OnceLock::new();
        let (start, start_usec) = *ANCHOR.get_or_init(|| {
            let wall = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros() as i64;
            (Instant::now(), wall.max(1))
        });
        start_usec + start.elapsed().as_micros() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_usec_is_positive_and_monotonic() {
        let a = now_usec();
        let b = now_usec();
        assert!(a > 0, "monotonic clock should be positive: {a}");
        assert!(b >= a, "monotonic clock must not go backwards: {a} -> {b}");
    }
}
