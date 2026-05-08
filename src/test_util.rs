//! Test-only helpers for serialising tests that mutate process-wide state
//! (primarily environment variables read by `issue_log::report` and friends).
//!
//! Cargo runs unit tests in parallel by default, so any test that touches
//! `std::env::set_var` is racy by nature. Acquire the lock returned by
//! [`env_lock`] for the duration of the test and the env mutations stay
//! coherent across all test modules in the binary.

#[cfg(test)]
pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}
