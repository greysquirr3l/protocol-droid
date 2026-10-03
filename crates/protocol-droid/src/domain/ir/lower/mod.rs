//! Lowering — making an IR value expressible at a target revision, or refusing.
//!
//! # The port trait lives here, not in an adapter
//!
//! [`Lowering`] is declared by the consumer, which is this module — the
//! translation path — and implemented wherever the wire format lives. That is
//! the project's rule, and it is worth restating why: a trait in an adapter
//! makes the adapter the owner of the vocabulary, so a second consumer has to
//! import from the first. A trait next to the thing that uses it means the
//! vocabulary belongs to the use case.
//!
//! # What lowering decides, and what it does not
//!
//! This module decides *whether* and *how*. It does not decide the wire bytes —
//! that is the adapter's job, and the reason the trait is here at all.
//!
//! The decision has exactly two outcomes, and which one you get depends on the
//! [`DegradePolicy`]:
//!
//! - The value's [`FeatureSet`] is a subset of what the target session has. It
//!   goes through unchanged, and the report is empty.
//! - It is not. Under a strict policy that is
//!   [`LowerError::FeatureUnavailable`], with nothing written. Under a policy
//!   that names a rule for the missing feature, the rule applies and the report
//!   says so.
//!
//! The third outcome is not a downgrade but a refusal of a different kind:
//! [`LowerError::FieldNotRepresentable`], where the value is fine as untyped
//! data but does not fit a struct the target revision requires. That one is a
//! property of the value rather than of the revision, so no policy can authorise
//! it; see the variant for why the alternatives are worse.
//!
//! What still holds is the property worth stating: lowering never returns a
//! value it cannot account for. A caller that gets `Ok` either has a lossless
//! translation or a report that names what was lost.

mod report;
mod rule;

pub use report::{TranslationReport, TranslationStep};
pub use rule::{DegradeRule, Loss};

use crate::domain::ir::features::FeatureSet;
use crate::domain::ir::tool::ToolResult;
use crate::domain::{DegradePolicy, Feature, FeatureProfile, ProtocolVersion};

/// Why a value could not be expressed at the target revision.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LowerError {
    /// The value needs a feature the negotiated revision cannot express, and
    /// no policy authorises a downgrade.
    #[error("FeatureUnavailable({feature:?}, {negotiated})")]
    FeatureUnavailable {
        /// The feature the value needs.
        feature: Feature,
        /// The revision it could not be expressed at.
        negotiated: ProtocolVersion,
    },

    /// The policy names a feature it can degrade, but names no rule for it.
    ///
    /// Distinct from [`LowerError::FeatureUnavailable`] because the fix is
    /// different: the caller asked for a downgrade and did not say how. Failing
    /// here rather than picking a rule is what keeps a policy from quietly
    /// choosing the destructive option.
    #[error("policy allows degrading {feature:?} but names no rule for it")]
    NoRuleForFeature {
        /// The feature with no rule.
        feature: Feature,
    },

    /// A field the target revision types as a struct does not fit it.
    ///
    /// This is the `2026-07-28` `meta` case, and it is deliberately *not* a
    /// [`LowerError::FeatureUnavailable`]. That variant says a revision cannot
    /// express a shape at all, so the fix is to negotiate something else or
    /// authorise a downgrade. This one says a *value* does not fit a shape: the
    /// same value lowers cleanly at `2025-06-18`, where `meta` is an untyped
    /// `serde_json::Value`, and fails at every revision that types it. Reporting
    /// it as a missing feature would put a false claim in the error — that the
    /// field is unavailable at the older revision — and would refuse messages
    /// that lower perfectly well.
    ///
    /// # Why no `DegradeRule` applies
    ///
    /// Every other variant here can be talked out of by a policy, and three
    /// options were considered for this one. All three are worse than refusing:
    ///
    /// - **Synthesise the missing member.** A fabricated `version` reads as
    ///   authoritative and was never sent by anyone.
    /// - **Drop the field, keep the rest of `meta`.** Silent data loss, and
    ///   `meta` is exactly where a sender puts something it meant to be
    ///   understood.
    /// - **Move the value into the struct's `extra` bag.** This one is a trap.
    ///   `ResultMetaObject` really does carry `#[serde(flatten)] extra`, so
    ///   putting `io.modelcontextprotocol/serverInfo` in there serialises
    ///   straight back out and *looks* lossless. It is not: the reader treats
    ///   that key as a known member and tries to parse it as an `Implementation`,
    ///   which is the parse that just failed here. So the document is unreadable
    ///   by its intended reader, and building it anyway launders the rejection
    ///   out of this layer and onto the peer — where the bridge looks healthy
    ///   and the error lands a long way from its cause.
    ///
    /// Refusing is the only option that leaves the failure where it can be
    /// diagnosed. Note this leaves nothing for a [`DegradePolicy`] to do, and
    /// that is the intent rather than an omission: the only rule available would
    /// discard the sender's data.
    ///
    /// # The blast radius is narrower than it looks
    ///
    /// The IR holds `meta` as a raw value, so a `meta` lifted from a
    /// `2026-07-28` peer has already been through that peer's deserialiser and
    /// will always re-parse. Only a value from a revision that treats `meta` as
    /// untyped, or one built by hand, can reach this.
    #[error("`{field}` does not fit {expected} required at {target}: {detail}")]
    FieldNotRepresentable {
        /// The JSON key that could not be represented.
        field: String,
        /// The type the target revision requires there.
        expected: &'static str,
        /// Why the value did not fit it.
        detail: String,
        /// The revision that types the field.
        target: ProtocolVersion,
    },
}

/// Translates a value into one a target revision can express.
///
/// Implemented by the codec layer, consumed here.
pub trait Lowering {
    /// The value this lowers.
    type Value;

    /// Lowers `value` to `target`, recording anything changed in `report`.
    ///
    /// # Errors
    ///
    /// [`LowerError`] when the value needs something the target cannot express
    /// and no authorised rule applies.
    fn lower(
        &self,
        value: Self::Value,
        target: ProtocolVersion,
        policy: DegradePolicy,
        report: &mut TranslationReport,
    ) -> Result<Self::Value, LowerError>;
}

/// The gate every lowering path shares.
///
/// `required ⊆ available` is the whole question, and asking it in one place is
/// what stops each codec from answering it slightly differently. Returns the
/// first missing feature, in the order [`FeatureSet::iter`] yields, so the
/// error is deterministic for a given value — a bridge logging errors has to
/// be able to tell whether two peers hit the same wall.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::ir::lower::check_expressible;
/// use protocol_droid::domain::ir::{FeatureSet, ToolResult};
/// use protocol_droid::domain::{Feature, ProtocolVersion, resolve, Negotiation, CapabilitySet, TransportKind};
///
/// let caps = CapabilitySet::all_for(ProtocolVersion::V2025_03_26);
/// let session = resolve(Negotiation {
///     version: ProtocolVersion::V2025_03_26,
///     client_caps: caps,
///     server_caps: caps,
///     transport: TransportKind::Stdio,
/// });
///
/// // A plain text result needs nothing, so it fits anywhere.
/// let plain = ToolResult::text("hello");
/// assert!(check_expressible(&plain.required_features(), &session, ProtocolVersion::V2025_03_26).is_ok());
///
/// // Structured output does not fit at 2025-03-26.
/// let structured = ToolResult::text("hello").with_structured(r#"{"a":1}"#).unwrap();
/// let error = check_expressible(
///     &structured.required_features(), &session, ProtocolVersion::V2025_03_26,
/// ).unwrap_err();
/// assert!(matches!(error, _ if error.to_string().contains("ToolStructuredOutput")));
/// # let _ = Feature::Tools;
/// ```
pub fn check_expressible(
    required: &FeatureSet,
    available: &FeatureProfile,
    negotiated: ProtocolVersion,
) -> Result<(), LowerError> {
    let available = FeatureSet::union_of(available.iter());
    if required.is_subset_of(available) {
        return Ok(());
    }
    let missing = required
        .iter()
        .find(|feature| !available.contains(*feature));
    Err(LowerError::FeatureUnavailable {
        // `required` is non-empty whenever it is not a subset, so `iter` yields
        // at least one. Reported as an error rather than unwrapped, because the
        // invariant is worth stating rather than asserting.
        feature: missing.unwrap_or(Feature::Tools),
        negotiated,
    })
}

/// The rule to use for a feature, under a policy.
///
/// `None` when the policy does not authorise degrading the feature at all.
/// Otherwise the first rule for that feature the policy allows, chosen in
/// [`DegradeRule::ALL`] order so the choice does not depend on enum ordering
/// accidents.
#[must_use]
pub fn rule_for(feature: Feature, policy: DegradePolicy) -> Option<DegradeRule> {
    if !policy.allows(feature) {
        return None;
    }
    DegradeRule::ALL
        .iter()
        .copied()
        .find(|rule| rule.is_fallback_for(feature))
}

/// Applies the authorised downgrades to a `ToolResult`.
///
/// The one place the degrade *decisions* and the IR's *content* meet, and
/// deliberately so: `ToolResult::downgrade_structured_output` owns how a text
/// block is built, and this owns when it is built. Splitting them the other way
/// round would put a content-shaping decision inside a policy loop.
///
/// Returns the value and a report naming everything that was given up. Under a
/// strict policy the report is always empty, because anything non-empty would
/// have been an error instead.
pub fn lower_tool_result(
    value: ToolResult,
    available: &FeatureProfile,
    target: ProtocolVersion,
    policy: DegradePolicy,
) -> Result<(ToolResult, TranslationReport), LowerError> {
    let required = value.required_features();
    let available = FeatureSet::union_of(available.iter());

    if required.is_subset_of(available) {
        return Ok((value, TranslationReport::new(target)));
    }

    let mut result = value;
    let mut report = TranslationReport::new(target);

    // Resolve every missing feature up front, so a policy that allows a
    // downgrade but names no rule fails before the value is touched. Applying
    // some rules and then erroring would leave a half-lowered value the caller
    // has to remember to discard.
    let mut pending: Vec<DegradeRule> = Vec::new();
    for feature in required.iter() {
        if available.contains(feature) {
            continue;
        }
        let rule = rule_for(feature, policy).ok_or_else(|| {
            if policy.allows(feature) {
                LowerError::NoRuleForFeature { feature }
            } else {
                LowerError::FeatureUnavailable {
                    feature,
                    negotiated: target,
                }
            }
        })?;
        pending.push(rule);
    }

    for rule in pending {
        match rule {
            // Handled in the IR, where the text block is built.
            DegradeRule::StructuredContentToText => {
                if result.downgrade_structured_output().is_some() {
                    report.record(rule);
                }
            }
            DegradeRule::DropAnnotations => {
                result = drop_annotations(&result);
                report.record(rule);
            }
            DegradeRule::DropAudio | DegradeRule::DropResourceLink => {
                result = drop_blocks(&result, rule);
                report.record(rule);
            }
            // Not reachable: `pending` is built from `rule_for`, which only
            // returns rules in `ALL`, and these three are opt-in reductions
            // rather than fallbacks. Delegated rather than ignored so that if
            // that ever changes the behaviour is the reduction and not a
            // silent no-op that would report a downgrade it did not perform.
            DegradeRule::AudioToTextPlaceholder
            | DegradeRule::DropImage
            | DegradeRule::DropResource => {
                if reduce_tool_result(&mut result, rule) {
                    report.record(rule);
                }
            }
        }
    }

    Ok((result, report))
}

/// Strips annotations from every content block.
fn drop_annotations(result: &ToolResult) -> ToolResult {
    use crate::domain::ir::content::Content;
    let mut result = result.clone();
    for block in &mut result.content {
        match block {
            Content::Text(block) => block.annotations = None,
            Content::Image(block) => block.annotations = None,
            Content::Audio(block) => block.annotations = None,
            Content::ResourceLink(block) => block.annotations = None,
            Content::EmbeddedResource(block) => block.annotations = None,
            Content::Unknown(_) => {}
        }
    }
    result
}

/// Removes the content blocks a rule cannot express.
fn drop_blocks(result: &ToolResult, rule: DegradeRule) -> ToolResult {
    use crate::domain::ir::content::Content;
    let mut result = result.clone();
    result.content.retain(|block| match block {
        Content::Audio(_) => !matches!(rule, DegradeRule::DropAudio),
        Content::ResourceLink(_) => !matches!(rule, DegradeRule::DropResourceLink),
        Content::Text(_)
        | Content::Image(_)
        | Content::EmbeddedResource(_)
        | Content::Unknown(_) => true,
    });
    result
}

/// Applies an opt-in reduction that no [`DegradePolicy`] can select, in place.
///
/// Three rules are reachable only this way, and the split is the point:
///
/// - [`DegradeRule::AudioToTextPlaceholder`] — the specification does not
///   sanction it, and there is no textual stand-in for audio.
/// - [`DegradeRule::DropImage`] and [`DegradeRule::DropResource`] — images and
///   embedded resources are expressible at every revision, so dropping one is
///   never *required*; a caller who wants a smaller message asks for it.
///
/// Returns whether anything changed, so a caller can record a step only when a
/// reduction actually happened and a loop over rules needs no pre-check.
/// Returns `false` unchanged for a fallback, because reaching one here would be
/// a caller mistake and doing nothing is the honest response.
pub fn reduce_tool_result(result: &mut ToolResult, rule: DegradeRule) -> bool {
    use crate::domain::ir::content::{Content, TextContent};
    let mut replaced = 0_usize;
    match rule {
        DegradeRule::AudioToTextPlaceholder => {
            for block in &mut result.content {
                if matches!(block, Content::Audio(_)) {
                    *block = Content::Text(TextContent::new("[audio omitted]"));
                    replaced += 1;
                }
            }
        }
        DegradeRule::DropImage | DegradeRule::DropResource => {
            let before = result.content.len();
            result.content.retain(|block| match block {
                Content::Image(_) => !matches!(rule, DegradeRule::DropImage),
                Content::EmbeddedResource(_) => !matches!(rule, DegradeRule::DropResource),
                _ => true,
            });
            replaced = before - result.content.len();
        }
        // A fallback, not a reduction: doing nothing is better than applying it
        // without the policy check that is the whole point of the fallback path.
        _ => return false,
    }
    replaced > 0
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        clippy::unwrap_used,
        reason = "tests assert on fallible calls and panic with the failing precondition; propagating errors would replace a clear failure message with noise"
    )]

    use super::{
        DegradeRule, LowerError, Lowering, TranslationReport, check_expressible, lower_tool_result,
        reduce_tool_result, rule_for,
    };
    use crate::domain::ir::lower::Loss;
    use crate::domain::ir::{AudioContent, Content, ImageContent, TextContent, ToolResult};
    use crate::domain::{
        CapabilitySet, DegradePolicy, Feature, FeatureProfile, Negotiation, ProtocolVersion,
        TransportKind, resolve,
    };

    /// The two failures are different in kind, and a codec that reports a value
    /// problem as a capability problem refuses messages that lower cleanly.
    ///
    /// `FieldNotRepresentable` is not `FeatureUnavailable` and must not be
    /// spelled as one. The same `meta` lowers at `2025-06-18` — where the field
    /// is an untyped value — and fails at `2026-07-28`, where the revision types
    /// it as a struct with required members. Reading that as "the field needs a
    /// feature `2025-06-18` lacks" would put a false claim in the error and
    /// refuse the older revision, which can carry the value untouched.
    #[test]
    fn a_value_that_does_not_fit_is_not_reported_as_a_missing_feature() {
        let not_representable = LowerError::FieldNotRepresentable {
            field: "io.modelcontextprotocol/serverInfo".to_owned(),
            expected: "Implementation",
            detail: "missing field `version`".to_owned(),
            target: ProtocolVersion::V2026_07_28,
        };
        let unavailable = LowerError::FeatureUnavailable {
            feature: Feature::ToolResultType,
            negotiated: ProtocolVersion::V2025_06_18,
        };

        assert_ne!(not_representable, unavailable);
        assert!(not_representable.to_string().contains("serverInfo"));
        assert!(!not_representable.to_string().contains("ToolResultType"));
    }

    /// The error names the target revision, because the same value is fine at
    /// another one and the diagnostic is useless without it.
    #[test]
    fn the_unrepresentable_field_error_names_where_it_failed() {
        let error = LowerError::FieldNotRepresentable {
            field: "io.modelcontextprotocol/serverInfo".to_owned(),
            expected: "Implementation",
            detail: "missing field `version`".to_owned(),
            target: ProtocolVersion::V2026_07_28,
        };

        let rendered = error.to_string();
        assert!(rendered.contains("2026-07-28"), "{rendered}");
        assert!(rendered.contains("missing field `version`"), "{rendered}");
    }

    /// A session at `version` with both sides advertising everything that
    /// version defines.
    fn session(version: ProtocolVersion) -> FeatureProfile {
        let caps = CapabilitySet::all_for(version);
        resolve(Negotiation {
            version,
            client_caps: caps,
            server_caps: caps,
            transport: TransportKind::Stdio,
        })
    }

    fn downgrade(feature: Feature) -> DegradePolicy {
        DegradePolicy::strict().with(feature)
    }

    /// A value that fits goes through untouched, and the report says nothing
    /// happened.
    #[test]
    fn a_value_that_fits_is_unchanged_and_unreported() {
        let value = ToolResult::text("hello");
        let (result, report) = lower_tool_result(
            value.clone(),
            &session(ProtocolVersion::V2025_03_26),
            ProtocolVersion::V2025_03_26,
            DegradePolicy::strict(),
        )
        .unwrap();
        assert_eq!(result, value);
        assert!(report.is_empty());
        assert!(report.is_lossless());
    }

    /// The named scenario, at the lowering layer.
    #[test]
    fn structured_output_downlevel_scenario() {
        let source = ToolResult::new(vec![Content::Text(TextContent::new("ok"))])
            .with_structured(r#"{"temperature":0.7}"#)
            .unwrap();
        let target = ProtocolVersion::V2025_03_26;

        // Strict: refused, with the feature named.
        let error = lower_tool_result(
            source.clone(),
            &session(target),
            target,
            DegradePolicy::strict(),
        )
        .unwrap_err();
        assert!(matches!(error, LowerError::FeatureUnavailable { .. }));
        assert!(
            error.to_string().contains("ToolStructuredOutput"),
            "{error}"
        );

        // Opted in: the text fallback, recorded, and lossless.
        let (result, report) = lower_tool_result(
            source,
            &session(target),
            target,
            downgrade(Feature::ToolStructuredOutput),
        )
        .unwrap();
        assert!(result.structured_content.is_none());
        assert_eq!(result.content.len(), 2);
        assert_eq!(report.len(), 1);
        let Some((step, _)) = report.steps().split_first() else {
            panic!("expected one step");
        };
        assert_eq!(step.rule, DegradeRule::StructuredContentToText);
        assert!(report.is_lossless(), "the fallback loses nothing");
    }

    /// The `elicitation-on-old-session` shape, at the gate.
    #[test]
    fn a_missing_feature_is_reported_with_its_revision() {
        let required = crate::domain::ir::FeatureSet::empty().with(Feature::Elicitation);
        let error = check_expressible(
            &required,
            &session(ProtocolVersion::V2025_03_26),
            ProtocolVersion::V2025_03_26,
        )
        .unwrap_err();
        assert_eq!(
            error,
            LowerError::FeatureUnavailable {
                feature: Feature::Elicitation,
                negotiated: ProtocolVersion::V2025_03_26,
            },
        );
    }

    #[test]
    fn the_error_message_names_the_feature_and_the_revision() {
        let required = crate::domain::ir::FeatureSet::empty().with(Feature::Elicitation);
        let error = check_expressible(
            &required,
            &session(ProtocolVersion::V2025_03_26),
            ProtocolVersion::V2025_03_26,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "FeatureUnavailable(Elicitation, 2025-03-26)"
        );
    }

    /// The gate and the value-level lowering must agree, or one of them is
    /// lying about what is expressible.
    #[test]
    fn the_gate_and_the_value_agree() {
        for version in ProtocolVersion::KNOWN {
            let available = session(version);
            for value in [
                ToolResult::text("x"),
                ToolResult::new(vec![Content::Audio(AudioContent::new("A", "audio/wav"))]),
                ToolResult::new(vec![Content::Image(ImageContent::new("A", "image/png"))]),
            ] {
                let gate = check_expressible(&value.required_features(), &available, version);
                let lower =
                    lower_tool_result(value.clone(), &available, version, DegradePolicy::strict());
                assert_eq!(
                    gate.is_ok(),
                    lower.is_ok(),
                    "{version} / {:?}: gate said {:?}, lower said {:?}",
                    value.content.first().map(Content::kind),
                    gate.err(),
                    lower.err(),
                );
            }
        }
    }

    /// A destructive rule is only reachable when a caller names it, and the
    /// report says so loudly.
    #[test]
    fn a_destructive_downgrade_is_recorded_as_destructive() {
        let value = ToolResult::new(vec![Content::Audio(AudioContent::new("A", "audio/wav"))]);
        let target = ProtocolVersion::V2024_11_05;

        assert!(
            lower_tool_result(
                value.clone(),
                &session(target),
                target,
                DegradePolicy::strict()
            )
            .is_err()
        );

        let policy = downgrade(Feature::ToolAudioContent);
        let (result, report) = lower_tool_result(value, &session(target), target, policy).unwrap();
        assert!(result.content.is_empty(), "the block was dropped");
        assert_eq!(report.worst_loss(), Loss::Destructive);
        assert!(!report.is_lossless_or_flagged());
    }

    #[test]
    fn a_destructive_placeholder_is_opt_in_only() {
        let mut value = ToolResult::new(vec![Content::Audio(AudioContent::new("A", "audio/wav"))]);
        assert!(reduce_tool_result(
            &mut value,
            DegradeRule::AudioToTextPlaceholder
        ));
        // A placeholder says something was there; an empty array does not.
        assert_eq!(value.content.len(), 1);
        let Some(block) = value.content.first() else {
            panic!("expected one block");
        };
        assert_eq!(block.kind(), "text");
    }

    /// A result that already holds a text block must not be reported as
    /// changed — the placeholder replaced an audio block, not the presence of
    /// text.
    #[test]
    fn a_reduction_that_changes_nothing_says_so() {
        let mut value = ToolResult::text("x");
        assert!(!reduce_tool_result(
            &mut value,
            DegradeRule::AudioToTextPlaceholder
        ));
        assert!(!reduce_tool_result(&mut value, DegradeRule::DropImage));
        assert_eq!(value.content.len(), 1);
    }

    #[test]
    fn asking_for_a_fallback_as_a_reduction_does_nothing() {
        let mut value = ToolResult::text("x").with_structured(r#"{"a":1}"#).unwrap();
        assert!(!reduce_tool_result(
            &mut value,
            DegradeRule::StructuredContentToText
        ));
        assert!(value.structured_content.is_some(), "not applied");
    }

    #[test]
    fn rule_lookup_requires_the_policy_to_name_the_feature() {
        assert!(rule_for(Feature::ToolStructuredOutput, DegradePolicy::strict()).is_none());
        assert_eq!(
            rule_for(
                Feature::ToolStructuredOutput,
                downgrade(Feature::ToolStructuredOutput)
            ),
            Some(DegradeRule::StructuredContentToText),
        );
    }

    /// A permissive policy cannot reach a rule that does not exist for a
    /// feature, so it must never produce `NoRuleForFeature` for a feature that
    /// does have one.
    #[test]
    fn a_permissive_policy_never_hits_the_no_rule_error() {
        let value = ToolResult::new(vec![Content::Audio(AudioContent::new("A", "audio/wav"))]);
        let result = lower_tool_result(
            value,
            &session(ProtocolVersion::V2024_11_05),
            ProtocolVersion::V2024_11_05,
            DegradePolicy::permissive(),
        );
        assert!(result.is_ok());
    }

    /// A worked implementor of the port trait, because a port trait nobody has
    /// implemented is a guess about what implementors need rather than a
    /// contract. This one delegates to [`lower_tool_result`] and shows the whole
    /// shape: an associated type, an in-place report, and domain errors out.
    struct SessionLowering {
        available: FeatureProfile,
    }

    impl Lowering for SessionLowering {
        type Value = ToolResult;

        fn lower(
            &self,
            value: ToolResult,
            target: ProtocolVersion,
            policy: DegradePolicy,
            report: &mut TranslationReport,
        ) -> Result<ToolResult, LowerError> {
            let (lowered, own) = lower_tool_result(value, &self.available, target, policy)?;
            report.absorb(&own);
            Ok(lowered)
        }
    }

    #[test]
    fn a_port_implementor_reports_into_the_callers_report() {
        let target = ProtocolVersion::V2025_03_26;
        let codec = SessionLowering {
            available: session(target),
        };
        let value = ToolResult::text("ok")
            .with_structured(r#"{"a":1}"#)
            .unwrap();

        let mut report = TranslationReport::new(target);
        let lowered = codec
            .lower(
                value,
                target,
                downgrade(Feature::ToolStructuredOutput),
                &mut report,
            )
            .unwrap();
        assert!(lowered.structured_content.is_none());
        // The implementor wrote into the caller's report rather than its own.
        assert_eq!(report.len(), 1);
        assert!(report.is_lossless());
    }

    #[test]
    fn a_port_implementor_errors_rather_than_returning_a_lossy_value() {
        let target = ProtocolVersion::V2025_03_26;
        let codec = SessionLowering {
            available: session(target),
        };
        let value = ToolResult::text("ok")
            .with_structured(r#"{"a":1}"#)
            .unwrap();
        let mut report = TranslationReport::new(target);
        let result = codec.lower(value, target, DegradePolicy::strict(), &mut report);
        assert!(result.is_err());
        // Nothing was written, so nothing has to be undone.
        assert!(report.is_empty());
    }

    /// Multiple missing features each need a rule, and every applied rule is
    /// recorded — a caller that asked for two downgrades gets two steps.
    #[test]
    fn every_applied_rule_is_recorded() {
        let value = ToolResult::new(vec![
            Content::Audio(AudioContent::new("A", "audio/wav")),
            Content::Image(ImageContent::new("B", "image/png")),
        ])
        .with_structured(r#"{"a":1}"#)
        .unwrap();
        let target = ProtocolVersion::V2024_11_05;
        let policy = DegradePolicy::permissive();
        let (result, report) = lower_tool_result(value, &session(target), target, policy).unwrap();
        assert!(report.len() >= 2, "{report:?}");
        assert!(!report.is_lossless_or_flagged());
        assert!(result.structured_content.is_none());
        assert!(!result.content.is_empty(), "the text fallback survived");
    }
}
