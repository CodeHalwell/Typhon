//! Process signal defaults.
//!
//! The single-function wrapper around `libc` this repository's dependency
//! rule asks for: nothing else in the tree touches the C library directly.

/// Restore the default disposition for `SIGPIPE`.
///
/// The Rust runtime ignores `SIGPIPE` so that a write to a closed pipe
/// surfaces as an `EPIPE` error — but `println!` turns that error into a
/// panic, so `tyc explain --list | head -3` exited 101 with a panic message
/// instead of stopping quietly. Every Unix CLI wants the default here:
/// the kernel terminates the process at the first write to a closed pipe,
/// exactly as `ls | head` does.
pub fn restore_sigpipe_default() {
    #[cfg(unix)]
    // SAFETY: `signal` with `SIG_DFL` for `SIGPIPE` is async-signal-safe and
    // is the documented way to opt out of Rust's `SIGPIPE` masking. Called
    // once, before any thread is spawned.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}
