//! Live-refresh policy for the capture panel (T4-4).
//!
//! View rendering belongs to lane L5; this module is the pure state machine it
//! calls:
//!
//! - **throttle**: at most one refresh per [`DEFAULT_THROTTLE_MS`] (250 ms); the
//!   first change after an idle period refreshes at once;
//! - **batch merge**: every change inside the window is folded into the refresh
//!   that fires at the window's end, with the total item count;
//! - **pause anchor**: while paused, nothing refreshes and auto-scroll is off;
//!   resuming keeps the anchor, so the user's scroll position is not stolen.
//!
//! All time comes from the caller, which keeps the state machine deterministic
//! and lets tests drive it without sleeping.

/// Default throttle window (ms).
pub const DEFAULT_THROTTLE_MS: u64 = 250;

/// Throttle configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefreshPolicy {
    pub throttle_ms: u64,
}

impl Default for RefreshPolicy {
    fn default() -> Self {
        Self {
            throttle_ms: DEFAULT_THROTTLE_MS,
        }
    }
}

impl RefreshPolicy {
    pub fn new(throttle_ms: u64) -> Self {
        Self {
            throttle_ms: throttle_ms.max(1),
        }
    }
}

/// What the caller should do after feeding the state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshAction {
    /// Render now; `items` is the merged count since the last render.
    Refresh { items: usize },
    /// Nothing to render yet.
    None,
}

/// Throttled, pause-aware refresh state.
#[derive(Clone, Debug)]
pub struct RefreshState {
    policy: RefreshPolicy,
    pending: usize,
    last_refresh_ms: Option<u64>,
    paused: bool,
    anchored: bool,
}

impl RefreshState {
    pub fn new(policy: RefreshPolicy) -> Self {
        Self {
            policy,
            pending: 0,
            last_refresh_ms: None,
            paused: false,
            anchored: false,
        }
    }

    /// Feeds `items` new entries arriving at `now_ms`.
    ///
    /// The first batch refreshes immediately. Later batches inside the window
    /// only accumulate; [`tick`](Self::tick) fires them once the window closes.
    /// While paused everything accumulates and nothing fires.
    pub fn on_items(&mut self, items: usize, now_ms: u64) -> RefreshAction {
        if items == 0 {
            return RefreshAction::None;
        }
        self.pending += items;
        if self.paused {
            return RefreshAction::None;
        }
        match self.last_refresh_ms {
            None => self.fire(now_ms),
            Some(last) if now_ms.saturating_sub(last) >= self.policy.throttle_ms => {
                self.fire(now_ms)
            }
            Some(_) => RefreshAction::None,
        }
    }

    /// Called periodically (e.g. on a 100 ms view timer). Fires a deferred
    /// refresh when the throttle window has elapsed, never while paused.
    pub fn tick(&mut self, now_ms: u64) -> RefreshAction {
        if self.paused || self.pending == 0 {
            return RefreshAction::None;
        }
        match self.last_refresh_ms {
            Some(last) if now_ms.saturating_sub(last) >= self.policy.throttle_ms => {
                self.fire(now_ms)
            }
            None => self.fire(now_ms),
            Some(_) => RefreshAction::None,
        }
    }

    fn fire(&mut self, now_ms: u64) -> RefreshAction {
        let items = self.pending;
        self.pending = 0;
        self.last_refresh_ms = Some(now_ms);
        RefreshAction::Refresh { items }
    }

    /// Stops refreshing and stops auto-scroll (`anchored = true`).
    pub fn pause(&mut self) {
        self.paused = true;
        self.anchored = true;
    }

    /// Resumes refreshing. The anchor survives: auto-scroll stays off until the
    /// user explicitly returns to the bottom.
    pub fn resume(&mut self) {
        self.paused = false;
    }

    /// The user scrolled back to the bottom (or asked to follow again).
    pub fn scroll_to_bottom(&mut self) {
        self.anchored = false;
    }

    /// The user scrolled away from the bottom while live: keep their position.
    pub fn anchor(&mut self) {
        self.anchored = true;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn is_anchored(&self) -> bool {
        self.anchored
    }

    /// Auto-scroll only while live and not anchored.
    pub fn should_autoscroll(&self) -> bool {
        !self.paused && !self.anchored
    }

    pub fn pending(&self) -> usize {
        self.pending
    }

    pub fn last_refresh_ms(&self) -> Option<u64> {
        self.last_refresh_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_batch_refreshes_immediately() {
        let mut state = RefreshState::new(RefreshPolicy::default());
        assert_eq!(
            state.on_items(3, 1_000),
            RefreshAction::Refresh { items: 3 }
        );
        assert_eq!(state.pending(), 0);
        assert_eq!(state.last_refresh_ms(), Some(1_000));
    }

    #[test]
    fn batches_inside_the_window_merge_into_one_refresh() {
        let mut state = RefreshState::new(RefreshPolicy::default());
        state.on_items(1, 0);
        assert_eq!(state.on_items(2, 100), RefreshAction::None);
        assert_eq!(state.on_items(4, 249), RefreshAction::None);
        assert_eq!(state.pending(), 6);
        assert_eq!(state.tick(250), RefreshAction::Refresh { items: 6 });
        assert_eq!(state.pending(), 0);
    }

    #[test]
    fn a_batch_after_an_idle_window_refreshes_at_once() {
        let mut state = RefreshState::new(RefreshPolicy::default());
        assert_eq!(state.on_items(1, 0), RefreshAction::Refresh { items: 1 });
        assert_eq!(state.on_items(2, 250), RefreshAction::Refresh { items: 2 });
        assert_eq!(state.tick(260), RefreshAction::None);
    }

    #[test]
    fn zero_items_never_schedule_a_refresh() {
        let mut state = RefreshState::new(RefreshPolicy::default());
        assert_eq!(state.on_items(0, 10), RefreshAction::None);
        assert_eq!(state.tick(10_000), RefreshAction::None);
        assert_eq!(state.pending(), 0);
    }

    #[test]
    fn pause_defers_refreshes_and_disables_autoscroll() {
        let mut state = RefreshState::new(RefreshPolicy::default());
        state.on_items(1, 0);
        state.pause();
        assert_eq!(state.on_items(5, 500), RefreshAction::None);
        assert_eq!(state.tick(900), RefreshAction::None);
        assert_eq!(state.pending(), 5);
        assert!(state.is_paused());
        assert!(!state.should_autoscroll());
    }

    #[test]
    fn resume_keeps_the_anchor_until_the_user_returns_to_the_bottom() {
        let mut state = RefreshState::new(RefreshPolicy::default());
        state.pause();
        state.resume();
        assert!(!state.is_paused());
        assert!(state.is_anchored());
        assert!(!state.should_autoscroll());
        assert_eq!(
            state.on_items(2, 1_000),
            RefreshAction::Refresh { items: 2 }
        );
        state.scroll_to_bottom();
        assert!(state.should_autoscroll());
    }

    #[test]
    fn scrolling_away_while_live_stops_autoscroll() {
        let mut state = RefreshState::new(RefreshPolicy::default());
        state.anchor();
        assert!(!state.should_autoscroll());
        assert_eq!(state.on_items(1, 0), RefreshAction::Refresh { items: 1 });
        assert!(!state.is_paused());
    }

    #[test]
    fn pausing_and_resuming_does_not_lose_the_merged_count() {
        let mut state = RefreshState::new(RefreshPolicy::default());
        state.pause();
        state.on_items(2, 10);
        state.on_items(3, 20);
        state.resume();
        state.scroll_to_bottom();
        assert_eq!(state.pending(), 5);
        assert_eq!(state.on_items(1, 30), RefreshAction::Refresh { items: 6 });
        assert_eq!(state.pending(), 0);
    }
}
