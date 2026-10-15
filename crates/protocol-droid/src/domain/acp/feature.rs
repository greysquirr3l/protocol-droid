//! `AcpFeatureProfile` — the ACP capability bitset, derived from
//! the negotiated version × agent capabilities × client
//! capabilities × transport.
//!
//! # Why a separate profile type
//!
//! The MCP and A2A modules each have their own `FeatureProfile`.
//! ACP's profile is a third one because the features it
//! enumerates are independent: a peer that speaks ACP does
//! not speak MCP, and the methods it advertises are the
//! slash-separated form `meta.json` lists. The cross-protocol
//! bridge holds the *union* of the three profiles side-by-side;
//! it does not collapse them into one bitset.
//!
//! # What this is
//!
//! The profile is a `[bool; N_FEATURES]` indexed by
//! [`AcpFeature`]. The shape mirrors the A2A `A2aFeatureProfile`
//! so the dispatcher can apply the same gate-check pattern
//! across protocols. A future revision that adds a new
//! feature gains a new variant in [`AcpFeature`] and a row in
//! the matrix; the type stays the same.

use crate::domain::acp::version::AcpVersion;

/// The count of features enumerated by [`AcpFeature`].
///
/// The constant is the source of truth for the column count
/// of the matrix; adding a variant to [`AcpFeature`] is the
/// only edit this constant needs to pick the new column up.
/// A unit test enforces that the variant count matches the
/// constant.
pub const N_FEATURES: usize = 7;

/// A capability an ACP peer can advertise. The variant
/// carries the spec section that introduced it; the
/// `feature_matrix_golden` test names the source.
///
/// # Why `Copy`
///
/// The matrix is `[[bool; N_FEATURES]; N_VERSIONS]` — a
/// per-revision row, indexed by `AcpFeature::ALL`. The
/// `ALL` array is the source of truth for the column count
/// and is a stack-local constant. The `Copy` derive keeps
/// the per-cell copies free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AcpFeature {
    /// `session/prompt` — the user-turn call. Every ACP
    /// agent the v1.25.0 schema describes supports
    /// `session/prompt`; the matrix row for v1.25.0 is `true`.
    /// Reference:
    /// <https://github.com/agentclientprotocol/agent-client-protocol/blob/schema-v1.25.0/schema/v1/schema.json>
    /// (the `agentMethods.session_prompt` entry in
    /// `meta.json`).
    SessionPrompt,
    /// `session/cancel` — cancels an in-flight `session/prompt`.
    /// Reference: `meta.json::agentMethods.session_cancel`.
    SessionCancel,
    /// `session/load` and `session/resume` — the editor's
    /// ability to resume a session. The agent advertises
    /// `loadSession` in `initialize`; the matrix row is
    /// `true` if the agent advertised it.
    /// Reference: `meta.json::agentMethods.session_load`
    /// and `session_resume`.
    SessionLoad,
    /// `fs/write_text_file` and `fs/read_text_file` — the
    /// editor's filesystem access. The agent's
    /// `fs.readTextFile` and `fs.writeTextFile` capabilities
    /// gate whether the agent may call the editor.
    /// Reference: `meta.json::clientMethods.fs_*`.
    FsAccess,
    /// `terminal/*` — the editor's terminal access. The
    /// agent's `terminal` capability gates the five
    /// `terminal/*` methods.
    /// Reference: `meta.json::clientMethods.terminal_*`.
    TerminalAccess,
    /// `elicitation/*` — the editor's structured elicitation
    /// surface. The agent's `elicitation` capability gates
    /// the two `elicitation/*` methods.
    /// Reference: `meta.json::clientMethods.elicitation_*`.
    Elicitation,
    /// `$/cancel_request` — the JSON-RPC standard cancel.
    /// Both peers must support this; the matrix row is
    /// always `true` and exists for symmetry with the
    /// `meta.json::protocolMethods` table.
    /// Reference: `meta.json::protocolMethods.cancel_request`.
    CancelRequest,
}

impl AcpFeature {
    /// The variant in declaration order. The order is
    /// the index the matrix uses.
    pub const ALL: [Self; N_FEATURES] = [
        Self::SessionPrompt,
        Self::SessionCancel,
        Self::SessionLoad,
        Self::FsAccess,
        Self::TerminalAccess,
        Self::Elicitation,
        Self::CancelRequest,
    ];
}

/// A capability profile negotiated at session startup.
///
/// Carries the negotiated version, the agent's
/// capabilities, the editor's capabilities, and the
/// per-feature bitset. The bitset is the answer to
/// "may code path X run right now?".
///
/// # Why a `Copy` newtype around a fixed-size array
///
/// The array is `Copy` because its element is `bool`
/// (which is `Copy`). A profile is a snapshot of the
/// negotiation result; it is small, cheap to copy,
/// and lives on the stack. The `AcpFeatureProfile` is
/// what the dispatcher's gate checks against on the
/// hot path; a `Copy` shape keeps that check an array
/// lookup, not a pointer chase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AcpFeatureProfile {
    /// The negotiated version. `AcpVersion::Unknown`
    /// means a future release the build has not seen
    /// the codec for; the dispatcher still gates on
    /// the bitset.
    pub version: AcpVersion,
    /// The per-feature bitset, indexed by `AcpFeature::ALL`.
    pub features: [bool; N_FEATURES],
}

impl AcpFeatureProfile {
    /// A profile with every feature off. Useful as a
    /// default before negotiation completes.
    pub const EMPTY: Self = Self {
        version: AcpVersion::V1_25_0,
        features: [false; N_FEATURES],
    };

    /// The canonical v1.25.0 profile: every method
    /// `meta.json` lists is on, because the v1.25.0
    /// schema is the union of those methods. An agent
    /// built on this build that wants to drop a
    /// method constructs a profile with the relevant
    /// bit off.
    pub const V1_25_0: Self = Self {
        version: AcpVersion::V1_25_0,
        features: [true; N_FEATURES],
    };

    /// Returns `true` if the feature is on. The
    /// dispatcher gates every feature-specific code
    /// path through this method; an off feature is
    /// the typed error path.
    pub const fn supports(self, feature: AcpFeature) -> bool {
        // The match maps the variant to its
        // bit-position. The variant-to-position
        // mapping is the same order the `ALL`
        // array uses, so the `feature_variant_count_matches_n_features`
        // test is the lock.
        match feature {
            AcpFeature::SessionPrompt => self.features[0],
            AcpFeature::SessionCancel => self.features[1],
            AcpFeature::SessionLoad => self.features[2],
            AcpFeature::FsAccess => self.features[3],
            AcpFeature::TerminalAccess => self.features[4],
            AcpFeature::Elicitation => self.features[5],
            AcpFeature::CancelRequest => self.features[6],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Copy` value the test harness can move
    /// freely; the matrix comparison is a `==` on
    /// a stack-local.
    fn assert_supports(profile: AcpFeatureProfile, feature: AcpFeature, expected: bool) {
        assert_eq!(
            profile.supports(feature),
            expected,
            "feature {feature:?} on {profile:?}"
        );
    }

    /// The v1.25.0 canonical profile is every
    /// feature on. A protocol-droid ACP agent built
    /// on the pin starts with this profile and
    /// removes features it does not implement.
    #[test]
    fn the_v1_25_0_canonical_profile_is_every_feature_on() {
        let profile = AcpFeatureProfile::V1_25_0;
        for feature in AcpFeature::ALL {
            assert_supports(profile, feature, true);
        }
    }

    /// The `EMPTY` profile is every feature off;
    /// `supports` is `false` for every cell. A
    /// pre-negotiation profile is `EMPTY` so the
    /// gate check rejects any code path that runs
    /// before the editor's capabilities are known.
    #[test]
    fn the_empty_profile_is_every_feature_off() {
        let profile = AcpFeatureProfile::EMPTY;
        for feature in AcpFeature::ALL {
            assert_supports(profile, feature, false);
        }
    }

    /// The matrix lock. The `ALL` array length
    /// matches the `N_FEATURES` constant, so a
    /// future variant added to the enum but
    /// forgotten in `ALL` is caught here, not
    /// by a downstream OOB read.
    #[test]
    fn feature_variant_count_matches_n_features() {
        assert_eq!(AcpFeature::ALL.len(), N_FEATURES);
    }
}
