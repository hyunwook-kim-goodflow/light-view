//! Panics the app expects and recovers from. A decoder, a save or a folder scan runs
//! inside [`catch`]: a panic there becomes an error and the app carries on, so the
//! crash recorder (lv-app `crash.rs`) must not count it as a crash. [`inside_catch`]
//! tells it whether the panicking thread is in such a scope.

use std::cell::Cell;
use std::panic::{catch_unwind, UnwindSafe};

thread_local! {
    /// How many [`catch`] scopes this thread is in.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// `std::panic::catch_unwind`, marking the thread as handling its own panics meanwhile.
pub fn catch<R>(f: impl FnOnce() -> R + UnwindSafe) -> std::thread::Result<R> {
    DEPTH.with(|d| d.set(d.get() + 1));
    let result = catch_unwind(f);
    DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    result
}

/// True while this thread runs inside [`catch`] (a panic now will be caught).
pub fn inside_catch() -> bool {
    DEPTH.try_with(Cell::get).is_ok_and(|d| d > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_is_tracked_through_panics_and_nesting() {
        assert!(!inside_catch());
        let seen = catch(|| {
            let inner = catch(|| {
                assert!(inside_catch());
                panic!("expected")
            });
            assert!(inner.is_err());
            inside_catch()
        });
        assert_eq!(seen.ok(), Some(true));
        assert!(!inside_catch());
        // Another thread has its own scope.
        let other = catch(|| std::thread::spawn(inside_catch).join().unwrap());
        assert_eq!(other.ok(), Some(false));
    }
}
