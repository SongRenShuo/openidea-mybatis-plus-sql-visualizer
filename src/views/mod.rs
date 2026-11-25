//! View panels and their data assembly (T2-1~T2-2).
//!
//! Lane L5 owns this directory; see `docs/TASKS.md` P2.

pub mod icons;
pub mod log_panel;
pub mod workbench;

/// Serialises the tests of every panel: the row stores and the workbench model
/// are process-wide, so a test in `commands` (loading a log, opening the
/// workbench) would otherwise be observed by a parallel `views` test.
#[cfg(test)]
pub(crate) fn test_state_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
