//! View panels and their data assembly (T2-1~T2-2).
//!
//! Lane L5 owns this directory; see `docs/TASKS.md` P2.

pub mod icons;
pub mod log_panel;
pub mod workbench;

/// Serialises the tests of every panel: the row stores and the workbench model
/// are process-wide, so a test in `commands` (loading a log, opening the
/// workbench) would otherwise be observed by a parallel `views` test.
///
/// The returned guard also records that this thread holds the lock, which
/// [`assert_state_lock_held`] checks from the one place that cannot be made
/// safe by convention alone: [`log_panel::push_state`]. Every push consumes the
/// workbench's one-shot `replace_input` flag (`mem::take`), so a test that
/// pushes without the lock silently eats another test's flag — three separate
/// CI failures came from exactly that, and none of them reproduced locally.
#[cfg(test)]
pub(crate) fn test_state_lock() -> StateLockGuard {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let inner = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    HOLDING.with(|depth| depth.set(depth.get() + 1));
    StateLockGuard { _inner: inner }
}

#[cfg(test)]
thread_local! {
    /// How many [`StateLockGuard`]s this thread holds. A count, not a flag,
    /// because `test_guard()` hands the same lock out and a test may wrap a
    /// helper that takes it again.
    static HOLDING: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The test lock's guard. Wraps the `MutexGuard` so dropping it also clears the
/// thread-local depth. The inner guard is only there to hold the lock; nothing
/// reads it, hence the underscore.
#[cfg(test)]
pub(crate) struct StateLockGuard {
    _inner: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl Drop for StateLockGuard {
    fn drop(&mut self) {
        HOLDING.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// Panics when the caller does not hold [`test_state_lock`]. Test-only, and
/// deliberately a panic rather than a warning: the failure it guards against
/// (a push stealing another test's one-shot flag) is otherwise a rare,
/// order-dependent red that only shows up on CI.
#[cfg(test)]
pub(crate) fn assert_state_lock_held(what: &str) {
    HOLDING.with(|depth| {
        assert!(
            depth.get() > 0,
            "{what} touches the process-wide panel store without \
             views::test_state_lock(); take the lock or it will race another test"
        );
    });
}
