//! Capture chain: Java agent transport, WebSocket consumer, log file tail (T4-1~T4-5).
//!
//! Lane L7 owns this directory; see `docs/TASKS.md` P4.
//!
//! The platform has no raw socket and no run-console access, so capture has two
//! paths; this module starts with the agent one:
//!
//! - [`ws`] consumes the loopback WebSocket the Java agent exposes (T4-1/T4-3).

pub mod ws;
