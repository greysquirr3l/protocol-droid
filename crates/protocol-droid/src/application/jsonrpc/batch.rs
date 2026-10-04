//! Whether this session accepts a batch frame at all.
//!
//! # Why this is a port and not a flag
//!
//! Batching is not a transport setting and it is not a decode setting. It is a
//! property of the negotiated pair, and it moved once already: `2025-06-18`
//! removed it, and `2026-07-28` is stateless precisely because a stateless
//! request cannot be correlated without one. So the question is asked against
//! the session's [`FeatureProfile`] rather than answered once at construction.
//!
//! Keeping it a trait rather than a bool is what lets A2A and ACP each bring
//! their own answer. A bool would put the default — "batching is fine" — in the
//! JSON-RPC core, and the default is the wrong one: a protocol that never
//! defined batching would silently inherit support for it.

use crate::domain::feature::{Feature, FeatureProfile};

/// Decides whether a batch frame is acceptable in this session.
///
/// The trait belongs to the consumer, so it lives in the module that consumes
/// it rather than in a shared ports module or in an adapter.
pub trait BatchPolicy {
    /// Whether a frame holding several messages may be processed.
    fn allows_batch(&self, profile: &FeatureProfile) -> bool;
}

/// Batching only when the negotiated pair supports it.
///
/// This is the MCP policy. [`Feature::JsonRpcBatching`] is absent from a
/// `2025-06-18`-or-later profile, so a modern peer that sends a batch is
/// refused rather than quietly processed — the difference is observable to the
/// sender, which is the point.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProfileBatching;

impl BatchPolicy for ProfileBatching {
    fn allows_batch(&self, profile: &FeatureProfile) -> bool {
        profile.allows(Feature::JsonRpcBatching)
    }
}

/// Always accept a batch, whatever the profile says.
///
/// For a protocol whose specification defines batching in every version, and
/// for tests that need a frame to reach the router without a capability
/// negotiation first. It is named rather than being the default so that
/// choosing it is a decision in the source rather than an omission.
#[derive(Debug, Clone, Copy, Default)]
pub struct AllowBatching;

impl BatchPolicy for AllowBatching {
    fn allows_batch(&self, _profile: &FeatureProfile) -> bool {
        true
    }
}

/// Refuse every batch.
#[derive(Debug, Clone, Copy, Default)]
pub struct DenyBatching;

impl BatchPolicy for DenyBatching {
    fn allows_batch(&self, _profile: &FeatureProfile) -> bool {
        false
    }
}
