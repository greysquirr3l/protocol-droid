//! `domain::acp` — the protocol-neutral ACP value objects.
//!
//! The ACP v1.25.0 schema is the wire format this module
//! speaks, but the value objects here are *protocol-neutral
//! IR* — they are the consumer's view of an ACP peer, not
//! the wire types the per-revision codec generates. The wire
//! types come from
//! `crates/protocol-droid-schema` (the vendored schema is
//! digested in
//! `crates/protocol-droid-schema/spec/MANIFEST.toml`).
//!
//! This module currently ships:
//!
//! - [`version::AcpVersion`] — the pinned ACP release,
//!   ordered by `(major, minor, patch)`. Unknown future
//!   releases are `Unknown { major, minor, patch }`.
//! - [`protocol_version::AcpProtocolVersion`] — the
//!   wire-level `u16` `initialize` exchanges.
//! - [`implementation::Implementation`] — the
//!   `clientInfo` / `agentInfo` block.
//! - [`initialize::InitializeRequest`] and
//!   [`initialize::InitializeResponse`] — the
//!   `initialize` round-trip, plus the
//!   `ClientCapabilities` / `AgentCapabilities`
//!   shapes the request/response carry.
//! - [`method::AcpMethod`] — the 25 methods the v1.25.0
//!   `meta.json` lists (13 agent, 11 client, 1 protocol).
//! - [`feature::AcpFeature`] and [`feature::AcpFeatureProfile`]
//!   — the per-feature bitset the dispatcher gates against.
//!
//! What is not yet here:
//!
//! - The session lifecycle types (`session/new`,
//!   `session/load`, `session/prompt`, `session/cancel`,
//!   `session/list`, `session/delete`, `session/resume`,
//!   `session/close`, `logout`).
//! - The slash-separated method params and the streaming
//!   update model.
//! - The `SessionCapabilities`, `ClientSessionCapabilities`,
//!   `ElicitationCapabilities`, and the rest of the
//!   capability sub-types the schema names.
//! - The application-layer dispatch and the per-revision
//!   wire codec.
//! - The stdio transport adapter.
//!
//! This module is feature-gated behind `acp-experimental` at
//! the crate level. The ACP module is post-MVP and
//! experimental; the gate keeps the protocol-neutral core
//! lean for consumers who never touch ACP.

#![cfg(feature = "acp-experimental")]

pub mod content;
pub mod feature;
pub mod implementation;
pub mod initialize;
pub mod method;
pub mod notification;
pub mod protocol_version;
pub mod session;
pub mod tool_call;
pub mod version;
