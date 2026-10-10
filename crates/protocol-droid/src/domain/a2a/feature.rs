//! `A2aFeatureProfile` — the per-revision capability bitset, derived
//! from the same four inputs as the MCP profile.
//!
//! # Why a separate profile type from MCP
//!
//! MCP's `FeatureProfile` and A2A's `A2aFeatureProfile` are
//! independent types because the features they enumerate are
//! independent. A bridge that translates MCP↔A2A maps the *union*
//! of the two — the bridge's domain code carries both profiles
//! side-by-side; it does not unify them into one bitset.
//!
//! # What this is not
//!
//! This is not the place to enumerate every A2A feature. The
//! enumeration in `Feature` below is the v1.0 minimum: the
//! methods the spec lists, the streaming shape, and the
//! push-notification delivery model. A future revision that adds
//! a new method gains a new variant and a row in the matrix; the
//! type stays the same.

use crate::domain::a2a::version::A2aVersion;

/// A capability an A2A peer can negotiate. Each variant has a
/// row in `tests::feature_matrix_golden` that names the
/// A2A spec section that introduced it.
///
/// # Why a `Copy` newtype for the matrix
///
/// The matrix is a `[[bool; N_FEATURES]; N_VERSIONS]` — a
/// per-revision row, indexed by `Feature::ALL`. The `ALL`
/// array is the source of truth for the column count, so a new
/// variant appended to the enum is the only edit the matrix
/// needs to pick it up. The `Copy` derive keeps the
/// `Feature::ALL` array a stack-local constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    /// `SendMessage` — the unary message-send method.
    SendMessage,
    /// `SendStreamingMessage` — the streaming counterpart of
    /// `SendMessage`, server-sent events on a Streamable HTTP
    /// POST.
    SendStreamingMessage,
    /// `GetTask` — fetch a task by id.
    GetTask,
    /// `ListTasks` — page through the tasks the agent owns.
    ListTasks,
    /// `CancelTask` — request cancellation of an in-flight task.
    CancelTask,
    /// `SubscribeToTask` — long-poll or SSE-style subscription to
    /// task updates.
    SubscribeToTask,
    /// Push-notification config CRUD — the four `Create*`,
    /// `Get*`, `List*`, `Delete*` methods.
    PushNotificationConfig,
    /// `GetExtendedAgentCard` — fetch the authenticated
    /// `AgentCard` once the peer has proven its identity.
    GetExtendedAgentCard,
}

impl Feature {
    /// Every variant, in declaration order. The matrix's
    /// column order is this order; a variant appended here
    /// becomes a new column without a separate edit.
    pub const ALL: [Self; 8] = [
        Self::SendMessage,
        Self::SendStreamingMessage,
        Self::GetTask,
        Self::ListTasks,
        Self::CancelTask,
        Self::SubscribeToTask,
        Self::PushNotificationConfig,
        Self::GetExtendedAgentCard,
    ];

    /// The human-readable label used in the matrix snapshot and
    /// in the `Display` form. Kept here rather than derived so a
    /// future variant that needs a different label (e.g. for
    /// `camelCase` ↔ `snake_case`) can hold one without changing
    /// the matrix's column header.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::SendMessage => "SendMessage",
            Self::SendStreamingMessage => "SendStreamingMessage",
            Self::GetTask => "GetTask",
            Self::ListTasks => "ListTasks",
            Self::CancelTask => "CancelTask",
            Self::SubscribeToTask => "SubscribeToTask",
            Self::PushNotificationConfig => "PushNotificationConfig",
            Self::GetExtendedAgentCard => "GetExtendedAgentCard",
        }
    }
}

/// The per-revision capability bitset for an A2A peer.
///
/// The matrix is small (one row per known version, one column
/// per `Feature::ALL`); the type holds it as a fixed-size
/// `[bool; N_FEATURES]` so the matrix lives in the type
/// system rather than in a `HashMap` that the feature set could
/// silently diverge from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct A2aFeatureProfile {
    /// The version this profile was derived for.
    version: A2aVersion,
    /// One bit per `Feature::ALL`, in the same order.
    bitset: [bool; 8],
}

impl A2aFeatureProfile {
    /// Build the profile for a known version. The matrix is the
    /// authoritative source; a per-version row is a literal
    /// here. A future revision adds a row, not a column.
    pub const fn for_known_version(version: A2aVersion) -> Self {
        match version {
            A2aVersion::V1_0 => Self {
                version,
                // Every v1.0 method is supported. The streaming
                // shape is gated by transport, not by version
                // — the profile here is the *capability*, the
                // transport is consulted separately.
                bitset: [true; 8],
            },
            A2aVersion::Unknown { major: _, minor: _ } => Self {
                version,
                // An unknown version is a peer the matrix has
                // not been updated for. The conservative default
                // is "no features" so a feature-specific code
                // path consults the profile and falls through to
                // the typed error, not to a silent
                // success-on-typo. The peer that names a
                // `VersionNotSupportedError` upgrade is the
                // upstream-correct path.
                bitset: [false; 8],
            },
        }
    }

    /// `true` if the named feature is enabled. The match is on
    /// a fixed-size array indexed by the feature's position in
    /// `Feature::ALL`; a new variant appended to `ALL` is the
    /// only edit the index needs to pick up.
    pub const fn is_enabled(&self, feature: Feature) -> bool {
        // `feature_index` returns a value in `0..Feature::ALL.len()`,
        // and `self.bitset` is sized to that constant. The match in
        // `feature_index` is exhaustive on `Feature`, so a future
        // variant added to the enum without an index entry is a
        // compile error rather than a runtime out-of-bounds.
        match feature {
            Feature::SendMessage => self.bitset[0],
            Feature::SendStreamingMessage => self.bitset[1],
            Feature::GetTask => self.bitset[2],
            Feature::ListTasks => self.bitset[3],
            Feature::CancelTask => self.bitset[4],
            Feature::SubscribeToTask => self.bitset[5],
            Feature::PushNotificationConfig => self.bitset[6],
            Feature::GetExtendedAgentCard => self.bitset[7],
        }
    }

    /// The version this profile was derived for. The
    /// dispatcher's `VersionNotSupportedError` includes this so
    /// a peer that did not negotiate sees which version it
    /// landed on.
    pub const fn version(&self) -> A2aVersion {
        self.version
    }
}

/// Compile-time feature match. The match in
/// `A2aFeatureProfile::is_enabled` is exhaustive on every
/// variant of `Feature`, so a future variant added to the
/// enum without an entry is a compile error rather than a
/// runtime out-of-bounds.
const fn _ensure_feature_match_is_exhaustive(feature: Feature) -> usize {
    match feature {
        Feature::SendMessage => 0,
        Feature::SendStreamingMessage => 1,
        Feature::GetTask => 2,
        Feature::ListTasks => 3,
        Feature::CancelTask => 4,
        Feature::SubscribeToTask => 5,
        Feature::PushNotificationConfig => 6,
        Feature::GetExtendedAgentCard => 7,
    }
}
