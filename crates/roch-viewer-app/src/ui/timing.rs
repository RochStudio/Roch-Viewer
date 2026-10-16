//! Where the time goes, when asked.
//!
//! With `ROCHVIEWER_TIMING` set, milestones (startup, the first frame, the
//! first full read, a tab switch, a pop-out opening) are printed to stderr
//! with the milliseconds since the process started. Unset, every call is one
//! cached check.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

static START: OnceLock<Instant> = OnceLock::new();
static ENABLED: OnceLock<bool> = OnceLock::new();

/// Note the process start. Called first thing in `main`.
pub fn start() {
    START.get_or_init(Instant::now);
}

pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var_os("ROCHVIEWER_TIMING").is_some())
}

fn since_start() -> f64 {
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

/// A milestone.
pub fn mark(label: &str) {
    if enabled() {
        eprintln!("[{:9.1} ms] {label}", since_start());
    }
}

/// A milestone with how long the thing it ends took.
pub fn took(label: &str, since: Instant) {
    if enabled() {
        eprintln!("[{:9.1} ms] {label}: {:.1} ms", since_start(), as_ms(since.elapsed()));
    }
}

/// Time a block.
pub fn span<R>(label: &str, f: impl FnOnce() -> R) -> R {
    if !enabled() {
        return f();
    }
    let started = Instant::now();
    let result = f();
    took(label, started);
    result
}

fn as_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
