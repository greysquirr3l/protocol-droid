//! The `2024-11-05` client, driven at the same altitude as the Streamable one.
//!
//! Gated by `legacy-sse`, which implies `http` — the client speaks HTTP either
//! way, and the two differ in *when* the server tells the client where to send,
//! not in what it speaks. See [`client`] for the client and
//! [`crate::adapters::secondary::transport::detect`] for the rule that decides
//! whether to use it.

mod client;
mod error;

pub use client::{ClientFrame, LegacySseClient, LegacySseConfig};
pub use error::LegacySseError;
