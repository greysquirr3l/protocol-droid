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
//! - [`method::AcpMethod`] — the 25 methods the v1.25.0
//!   `meta.json` lists (13 agent, 11 client, 1 protocol).
//! - [`feature::AcpFeature`] and [`feature::AcpFeatureProfile`]
//!   — the per-feature bitset the dispatcher gates against.
//!
//! What is not yet here:
//!
//! - The `initialize` request/response types and the
//!   `capabilities` shape. The session lifecycle types.
//!   The slash-separated method params, the streaming
//!   update model. These land in subsequent
//!   `domain::acp` modules as the application-layer
//!   dispatch and the per-revision codec build them out.
//!
//! This module is feature-gated behind `acp-experimental` at
//! the crate level. The ACP module is post-MVP and
//! experimental; the gate keeps the protocol-neutral core
//! lean for consumers who never touch ACP.

#![cfg(feature = "acp-experimental")]

pub mod feature;
pub mod method;
pub mod version;
