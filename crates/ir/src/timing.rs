//! Zero-dependency phase timing for `--debug` / `RIPPLE_DEBUG`.
//!
//! Off by default and writing only to stderr, so it never touches query output —
//! the determinism invariant is unaffected. Turn it on with the `--debug` CLI flag
//! (which sets the env for us) or `RIPPLE_DEBUG=1`, and each instrumented phase
//! prints its wall time and a count, so a slow index says *which* phase is slow
//! before anyone guesses at an algorithm.

use std::sync::OnceLock;
use std::time::Instant;

/// Read once: `RIPPLE_DEBUG` set to anything but empty/`0` turns timing on.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("RIPPLE_DEBUG").is_ok_and(|v| !v.is_empty() && v != "0"))
}

/// Force timing on for this process (the `--debug` flag calls this before any
/// phase runs, so the `OnceLock` in `enabled` latches to `true`).
///
/// # Safety
/// Sets a process env var; call before threads that read the environment start.
pub fn force_on() {
    if std::env::var("RIPPLE_DEBUG").is_err() {
        // SAFETY: called once at command start, before rayon/LSP threads spawn.
        unsafe { std::env::set_var("RIPPLE_DEBUG", "1") };
    }
}

/// Run `f`, and when timing is on print `name`, its wall time, and `count(&result)`
/// (a phase-specific tally — files parsed, edges linked) to stderr. Returns `f`'s
/// value untouched, so wrapping a phase never changes behaviour.
pub fn phase<T>(name: &str, count: impl FnOnce(&T) -> usize, f: impl FnOnce() -> T) -> T {
    if !enabled() {
        return f();
    }
    let start = Instant::now();
    let out = f();
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    eprintln!("[ripple] {name:<20} {ms:>9.1}ms  ({} items)", count(&out));
    out
}

/// Like `phase`, but with no count to report — for a phase whose size is not a
/// single number.
pub fn step<T>(name: &str, f: impl FnOnce() -> T) -> T {
    phase(name, |_| 0, f)
}

/// A manually-stopped span, for a phase whose body cannot be wrapped in one
/// closure (borrows straddle several statements). `stop` prints and consumes it.
#[must_use]
pub struct Span {
    name: String,
    start: Instant,
}

/// Begin a named span. Cheap even when timing is off (one `Instant::now`).
pub fn start(name: &str) -> Span {
    Span {
        name: name.to_owned(),
        start: Instant::now(),
    }
}

impl Span {
    /// End the span, printing `name`, wall time, and `count` when timing is on.
    pub fn stop(self, count: usize) {
        if enabled() {
            let ms = self.start.elapsed().as_secs_f64() * 1000.0;
            eprintln!("[ripple] {:<20} {ms:>9.1}ms  ({count} items)", self.name);
        }
    }
}
