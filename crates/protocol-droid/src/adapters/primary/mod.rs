//! Driving adapters: the server transports.
//!
//! Populated by T13 (stdio), T14 (Streamable HTTP) and T15 (legacy HTTP+SSE).
//!
//! These are the transports the project has to run on: stdio, legacy HTTP+SSE as
//! defined for 2024-11-05, stateful Streamable HTTP, and the stateless
//! lifecycle from 2026-07-28. Each is feature-gated so a
//! `--no-default-features` build stays a lean core.

#[cfg(feature = "stdio")]
pub mod stdio;
