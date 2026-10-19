//! Errors the server API returns before anything reaches the wire.

use crate::domain::ProtocolVersion;
use crate::domain::feature::Feature;

/// Why a feature-specific call was refused.
///
/// Every variant here is a refusal the peer never sees. The distinction from a
/// JSON-RPC error matters: an error the peer sees is a protocol interaction,
/// and this crate is careful about when it sends one. A handler that asks for
/// a capability the session does not have is not a protocol error — the request
/// was well formed and the server simply cannot serve it the way the handler
/// imagined. Sending a JSON-RPC error there teaches the peer something false
/// about the protocol, so the failure stays on this side of the wire and the
/// handler is told directly.
///
/// That is the whole reason this type exists rather than reusing the
/// lower-time `LowerError`: one happens during a translation the peer asked
/// for, the other happens before anything is written at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GatedError {
    /// The negotiated revision and the peer's declared capabilities do not
    /// include the feature the call needs.
    ///
    /// The negotiated version is carried rather than left to the caller to
    /// remember, because "elicitation is unavailable" is an answer nobody can
    /// act on without knowing what *was* negotiated.
    #[error("{feature} was not negotiated (session is {negotiated})")]
    FeatureUnavailable {
        /// The feature the call needed.
        feature: Feature,
        /// The revision this session actually settled on.
        negotiated: ProtocolVersion,
    },

    /// A progress report was attempted on a request that was never given a
    /// progress token.
    ///
    /// Separate from [`FeatureUnavailable`](Self::FeatureUnavailable) because
    /// the fix is different: progress is available on every revision, and the
    /// client is the one that declined to open a channel.
    #[error("this request has no progress token; the client did not open one")]
    NoProgressToken,

    /// A `requestState` was to be sealed and the server holds no key.
    ///
    /// A server built in-process with no key, serving through no transport
    /// that would have supplied one. Sealing under a placeholder would hand
    /// out a state any client could forge, which is worse than refusing.
    #[error("this server has no request-state key to seal with")]
    NoStateKey,

    /// The peer's answer to a server-initiated request did not arrive as JSON,
    /// or the exchange did not complete.
    ///
    /// The gate is not implicated — the call was permitted, and the failure is
    /// the peer's or the transport's. Kept here so a handler has one error
    /// type to match rather than a gate error and a transport error.
    #[error("the exchange did not complete: {reason}")]
    MalformedAnswer {
        /// What the transport reported.
        ///
        /// A `String` because the transport names its own failures and the
        /// application layer has no business enumerating them. The rule is that
        /// application code never names a _third-party_ error type; a
        /// transport's own description is data the adapter chose to hand over.
        reason: String,
    },
}

impl GatedError {
    /// The feature this refusal is about, when there is one.
    ///
    /// A progress-token refusal is about no feature, which is why this returns
    /// an `Option` rather than inventing one — a caller logging the answer
    /// wants to omit the field, not print a placeholder that reads as a claim.
    #[must_use]
    pub const fn feature(&self) -> Option<Feature> {
        match self {
            Self::FeatureUnavailable { feature, .. } => Some(*feature),
            Self::NoProgressToken | Self::NoStateKey | Self::MalformedAnswer { .. } => None,
        }
    }

    /// Whether nothing was written to the transport before this refusal.
    ///
    /// Always true, and that is the point: the gate runs before the exchange,
    /// so a refused call cannot have left a partial frame behind. The
    /// [`MalformedAnswer`](Self::MalformedAnswer) case is the exception in
    /// spirit — the request _was_ written, because the gate had passed and the
    /// failure came afterwards — so it reports false.
    #[must_use]
    pub const fn wrote_nothing(&self) -> bool {
        match self {
            Self::FeatureUnavailable { .. } | Self::NoProgressToken | Self::NoStateKey => true,
            Self::MalformedAnswer { .. } => false,
        }
    }
}
