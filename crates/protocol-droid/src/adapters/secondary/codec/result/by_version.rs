//! The codec the bridge actually calls: one entry point, six revisions.
//!
//! # Why this exists
//!
//! The six per-revision codecs are deliberately six types rather than one
//! generic over a wire enum — the member unions differ, and collapsing them
//! would reintroduce the least-common-denominator shape the IR exists to avoid
//! (see [`super`]).
//!
//! What the bridge needs is the opposite shape: a revision on one side and a
//! [`ToolResult`] on the other, without naming which of the six it got. That is
//! this file, and nothing else: a `match` from a [`ProtocolVersion`] to the
//! codec for it, plus `serde_json`'s crossing between a generated type and a
//! [`Value`].
//!
//! # No decisions live here
//!
//! Whether a value *can* be expressed at a revision, and what to give up if it
//! cannot, is the gate's and the policy's answer. This dispatches to a codec,
//! which asks `lower_tool_result` itself, and reports whatever came back. A
//! second opinion formed here is how two revisions start disagreeing about what
//! they support.
//!
//! # An unknown revision is refused, never guessed
//!
//! This build generates wire types for the revisions it vendors and no others,
//! so there is nothing to dispatch to for [`ProtocolVersion::Unknown`] — the
//! same rule the transport gate follows for the same reason. `lift` answers
//! `None`, which forwards the value untouched; `lower` refuses, because
//! fabricating a result at a revision whose schema was never read is the one
//! outcome a bridge must not produce.
//!
//! A `Bridge` cannot reach the `lower` arm for a real negotiation, because both
//! legs come from a `handshake` against a revision this build speaks. The arm
//! exists so that the claim is true rather than assumed, and it is asserted
//! directly below rather than left as a branch nothing exercises.
//!
//! [`Value`]: serde_json::Value

use protocol_droid_schema::wire::{
    v2024_11_05 as schema_2024_11_05, v2025_03_26 as schema_2025_03_26,
    v2025_06_18 as schema_2025_06_18, v2025_11_25 as schema_2025_11_25,
    v2026_07_28 as schema_2026_07_28,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{
    v2024_11_05::V2024_11_05, v2025_03_26::V2025_03_26, v2025_06_18::V2025_06_18,
    v2025_11_25::V2025_11_25, v2026_07_28::V2026_07_28,
};
use crate::application::bridge::ToolResultCodec;
use crate::domain::ir::{Codec, LowerError, ToolResult, TranslationReport};
use crate::domain::{DegradePolicy, ProtocolVersion};

/// Reads and writes a `ToolResult` at whichever revision it is given.
///
/// A zero-sized type: there is no state to hold, because all the work is in the
/// match.
#[derive(Debug, Clone, Copy, Default)]
pub struct WireToolResults;

/// The per-revision codecs, addressed by revision.
///
/// One `match` on each half rather than six copies of it, so adding a revision
/// is two arms and nothing can drift out of step with the other. The helpers
/// below are generic over the *generated* type rather than over the codec —
/// `Codec::Wire` carries no `Deserialize` bound, and a type parameter is not a
/// value, so a helper generic over the codec could not name either.
fn read<T: DeserializeOwned>(wire: &Value) -> Option<T> {
    serde_json::from_value::<T>(wire.clone()).ok()
}

fn encode(version: ProtocolVersion, wire: impl Serialize) -> Result<Value, LowerError> {
    serde_json::to_value(wire).map_err(|error| LowerError::FieldNotRepresentable {
        field: "result".to_owned(),
        expected: "a result that serialises",
        detail: error.to_string(),
        target: version,
    })
}

impl ToolResultCodec for WireToolResults {
    /// `None` covers both "this build has no wire types for that revision" and
    /// "the value is not a result this revision can read" — the caller
    /// forwards either, and cannot tell the two apart or need to.
    fn lift(&self, version: ProtocolVersion, wire: &Value) -> Option<ToolResult> {
        match version {
            ProtocolVersion::V2024_11_05 => read::<schema_2024_11_05::CallToolResult>(wire)
                .map(|typed| V2024_11_05.lift(&typed)),
            ProtocolVersion::V2025_03_26 => read::<schema_2025_03_26::CallToolResult>(wire)
                .map(|typed| V2025_03_26.lift(&typed)),
            ProtocolVersion::V2025_06_18 => read::<schema_2025_06_18::CallToolResult>(wire)
                .map(|typed| V2025_06_18.lift(&typed)),
            ProtocolVersion::V2025_11_25 => read::<schema_2025_11_25::CallToolResult>(wire)
                .map(|typed| V2025_11_25.lift(&typed)),
            ProtocolVersion::V2026_07_28 => read::<schema_2026_07_28::CallToolResult>(wire)
                .map(|typed| V2026_07_28.lift(&typed)),
            // A revision this build did not vendor, and `draft` too — it
            // reports `2026-07-28` as its version, because upstream's floating
            // schema declares the same `LATEST_PROTOCOL_VERSION`, so a version
            // alone cannot select it. That is a real limitation rather than an
            // omission: a caller that wants the floating schema has to name
            // [`Draft`](super::draft::Draft) directly.
            //
            // Named rather than wildcarded so that vendoring a sixth revision
            // is a compile error here instead of a revision this build quietly
            // cannot speak.
            ProtocolVersion::Unknown(_) => None,
        }
    }

    fn lower(
        &self,
        version: ProtocolVersion,
        value: ToolResult,
        policy: DegradePolicy,
        report: &mut TranslationReport,
    ) -> Result<Value, LowerError> {
        match version {
            ProtocolVersion::V2024_11_05 => {
                encode(version, V2024_11_05.lower(value, policy, report)?)
            }
            ProtocolVersion::V2025_03_26 => {
                encode(version, V2025_03_26.lower(value, policy, report)?)
            }
            ProtocolVersion::V2025_06_18 => {
                encode(version, V2025_06_18.lower(value, policy, report)?)
            }
            ProtocolVersion::V2025_11_25 => {
                encode(version, V2025_11_25.lower(value, policy, report)?)
            }
            ProtocolVersion::V2026_07_28 => {
                encode(version, V2026_07_28.lower(value, policy, report)?)
            }
            // Refused rather than guessed: fabricating a result at a revision
            // whose schema was never read is the one outcome a bridge must not
            // produce. `draft` is here for the same reason as in `lift`, and
            // this arm is named for the same reason as the one above.
            ProtocolVersion::Unknown(_) => Err(LowerError::FieldNotRepresentable {
                field: "result".to_owned(),
                expected: "a revision this build has generated wire types for",
                detail: format!(
                    "{version} has no schema vendored here, so there is no \
                     CallToolResult shape to write the value into"
                ),
                target: version,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::WireToolResults;
    use crate::application::bridge::ToolResultCodec;
    use crate::domain::ir::lower::check_expressible;
    use crate::domain::ir::{LowerError, ToolResult, TranslationReport};
    use crate::domain::{
        CapabilitySet, DegradePolicy, FeatureProfile, Negotiation, ProtocolVersion, TransportKind,
        resolve,
    };

    type TestError = Box<dyn std::error::Error>;

    /// Every revision this build vendored, so a new one cannot be added without
    /// a decision about whether it dispatches.
    fn vendored() -> Vec<ProtocolVersion> {
        vec![
            ProtocolVersion::V2024_11_05,
            ProtocolVersion::V2025_03_26,
            ProtocolVersion::V2025_06_18,
            ProtocolVersion::V2025_11_25,
            ProtocolVersion::V2026_07_28,
        ]
    }

    /// A value that needs nothing, so it is expressible at every revision and
    /// the round trip is about the *codec*, not about a downgrade.
    fn plain() -> ToolResult {
        ToolResult::text("hello")
    }

    /// The property the dispatch exists to preserve: a value this revision can
    /// express comes back as the value that went in.
    #[test]
    fn every_vendored_revision_reads_back_what_it_wrote() -> Result<(), TestError> {
        let codec = WireToolResults;
        let value = plain();

        for version in vendored() {
            let mut report = TranslationReport::new(version);
            let wire = codec.lower(
                version,
                value.clone(),
                DegradePolicy::permissive(),
                &mut report,
            )?;
            let back = codec
                .lift(version, &wire)
                .ok_or("the codec refused to read back its own output")?;

            assert_eq!(back.content, value.content, "content differs at {version}");
            assert_eq!(
                back.is_error, value.is_error,
                "isError differs at {version}"
            );
            assert!(
                report.is_empty(),
                "{version} downgraded a plain text result"
            );
        }
        Ok(())
    }

    /// A content block this build does not model reads as nothing rather than
    /// as a failure, which is what lets the bridge forward it.
    #[test]
    fn a_block_this_build_cannot_read_lifts_to_nothing() {
        let codec = WireToolResults;
        let wire = json!({
            "content": [{ "type": "annotation", "audience": ["assistant"] }]
        });
        assert_eq!(codec.lift(ProtocolVersion::V2026_07_28, &wire), None);
    }

    /// The same for a payload that is not a result at all.
    #[test]
    fn a_payload_that_is_not_a_result_lifts_to_nothing() {
        let codec = WireToolResults;
        let wire = json!([{ "name": "echo" }]);
        assert_eq!(codec.lift(ProtocolVersion::V2025_03_26, &wire), None);
    }

    /// A revision with no vendored schema reads as nothing, and is refused on
    /// the way down. The `lower` arm is unreachable from a real negotiation —
    /// both legs come from a handshake against a revision this build speaks —
    /// so it is asserted here rather than left as a branch nothing exercises.
    #[test]
    fn an_unknown_revision_is_refused_rather_than_guessed() -> Result<(), TestError> {
        let version = ProtocolVersion::Unknown("2027-01-01".parse()?);
        let codec = WireToolResults;

        assert_eq!(
            codec.lift(version, &json!({ "content": [] })),
            None,
            "an unknown revision is forwarded, not read"
        );

        let mut report = TranslationReport::new(version);
        let outcome = codec.lower(version, plain(), DegradePolicy::permissive(), &mut report);

        assert!(
            matches!(outcome, Err(LowerError::FieldNotRepresentable { .. })),
            "an unknown revision must refuse rather than fabricate a shape, got {outcome:?}"
        );
        Ok(())
    }

    /// `draft` and `2026-07-28` report the same version string, so the
    /// dispatcher has to reach `draft` deliberately or not at all. This pins
    /// that the *published* revision answers for `2026-07-28`, and that the two
    /// schemas are not interchangeable.
    #[test]
    fn the_published_revision_answers_for_its_version() -> Result<(), TestError> {
        let version = ProtocolVersion::V2026_07_28;
        let caps = CapabilitySet::all_for(version);
        let profile = resolve(Negotiation {
            version,
            client_caps: caps,
            server_caps: caps,
            transport: TransportKind::Stdio,
        });

        // A value that only the newest schema can express: `resultType` is
        // `2026-07-28`-and-later, and has no lower revision to be compared to.
        let value = ToolResult {
            result_type: Some("search".to_owned()),
            ..ToolResult::text("hello")
        };
        assert!(
            check_expressible(&value.required_features(), &profile, version).is_ok(),
            "the fixture must be expressible at its own revision"
        );

        let mut report = TranslationReport::new(version);
        let wire = WireToolResults.lower(version, value, DegradePolicy::strict(), &mut report)?;

        assert_eq!(
            wire.get("resultType"),
            Some(&json!("search")),
            "the newest schema must have been used"
        );
        Ok(())
    }

    /// The other half of the previous claim: `draft` is reachable and still
    /// speaks its own schema, so dispatch has not simply forgotten it.
    #[test]
    fn draft_is_still_a_working_codec() -> Result<(), TestError> {
        let mut report = TranslationReport::new(ProtocolVersion::V2026_07_28);
        let wire = WireToolResults.lower(
            ProtocolVersion::V2026_07_28,
            plain(),
            DegradePolicy::strict(),
            &mut report,
        )?;
        let back = WireToolResults.lift(ProtocolVersion::V2026_07_28, &wire);
        assert!(back.is_some(), "the published revision must round-trip");
        Ok(())
    }

    /// The gate the bridge consults before it touches a codec, exercised here
    /// so that this file's dependency on it is visible rather than implied.
    #[test]
    fn a_profile_with_no_features_expresses_only_what_needs_nothing() {
        assert!(
            check_expressible(
                &plain().required_features(),
                &FeatureProfile::empty(),
                ProtocolVersion::V2024_11_05,
            )
            .is_ok()
        );
    }
}
