//! Opt-in phase timing for `--timing` and lv-bench: code wraps a step in [`phase`], and
//! a caller that wants the times runs the work inside [`collect`] on the same thread.
//! Outside `collect` a phase costs one thread-local check.

use std::cell::RefCell;
use std::time::Instant;

/// A phase name and its time in milliseconds.
pub type Phase = (&'static str, f64);

thread_local! {
    static PHASES: RefCell<Option<Vec<Phase>>> = const { RefCell::new(None) };
}

/// Runs `f`, recording its time as `name` when called inside [`collect`].
pub fn phase<T>(name: &'static str, f: impl FnOnce() -> T) -> T {
    if !PHASES.with(|p| p.borrow().is_some()) {
        return f();
    }
    let start = Instant::now();
    let out = f();
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    PHASES.with(|p| {
        if let Some(v) = p.borrow_mut().as_mut() {
            v.push((name, ms));
        }
    });
    out
}

/// Runs `f` and returns the phases it recorded on this thread, in order. A phase that
/// runs more than once is listed each time. Nested `collect` calls keep their own list.
pub fn collect<T>(f: impl FnOnce() -> T) -> (T, Vec<Phase>) {
    let outer = PHASES.with(|p| p.borrow_mut().replace(Vec::new()));
    let out = f();
    let phases = PHASES.with(|p| std::mem::replace(&mut *p.borrow_mut(), outer));
    (out, phases.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_are_recorded_only_inside_collect() {
        assert_eq!(phase("outside", || 1), 1);
        let (out, phases) = collect(|| {
            phase("a", || ());
            let (_, inner) = collect(|| phase("inner", || ()));
            assert_eq!(inner.len(), 1);
            phase("b", || 7)
        });
        assert_eq!(out, 7);
        let names: Vec<_> = phases.iter().map(|p| p.0).collect();
        assert_eq!(names, ["a", "b"]);
        assert!(phases.iter().all(|p| p.1 >= 0.0));
        // Nothing is left switched on.
        assert!(collect(|| ()).1.is_empty());
        assert!(PHASES.with(|p| p.borrow().is_none()));
    }
}
