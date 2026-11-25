//! Capture chain: Java agent transport, WebSocket consumer, log file tail (T4-1~T4-5).
//!
//! Lane L7 owns this directory; see `docs/TASKS.md` P4.
//!
//! The platform has no raw socket and no run-console access, so capture has two
//! paths:
//!
//! - [`ws`] consumes the loopback WebSocket the Java agent exposes (T4-1/T4-3);
//! - [`file_tail`] incrementally follows an exported log file when no agent is
//!   attached (T4-5).
//!
//! [`refresh`] holds the pure throttling / pause-anchor policy the panel view
//! (lane L5) applies when new events arrive (T4-4).

pub mod file_tail;
pub mod refresh;
pub mod ws;
