//! The UTCP server side: publishing this server's tools as a manual.
//!
//! # What lives here
//!
//! [`serve::ManualPublisher`] keeps a manual in front of an HTTP route and
//! serves it at the path the specification names, `GET /utcp`. The manual is
//! rebuilt from the registry whenever the registry changes and swapped in
//! atomically, so a reader never sees a half-built document.
//!
//! # Why this is a separate router
//!
//! The MCP router is a session-bearing endpoint: it authenticates, it
//! negotiates, it tracks sessions. A manual is none of those things — it is
//! a static document about tools. Mounting it as its own router means a
//! consumer can serve it beside the MCP endpoint with one `merge`, and does
//! not have to make the MCP router aware of a protocol that has no envelope.

pub mod serve;
#[cfg(test)]
mod tests;

pub use serve::{MANUAL_PATH, ManualPublisher};
