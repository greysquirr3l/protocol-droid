//! Property tests for lowering.
//!
//! # The two properties, and what each is for
//!
//! **Round trip.** For a value expressible at a revision, lowering must be the
//! identity and the report must be empty. If this fails, a bridge loses data
//! between two peers that agreed on a version — the exact failure the crate
//! exists to prevent, and the one that stays invisible until something is
//! subtly wrong on the far side.
//!
//! **The gate is exact.** Under a strict policy, lowering succeeds *iff*
//! `required_features ⊆ available`. Not "usually", not "except when the value
//! happens to be simple" — exactly. An off-by-one here is a `FeatureUnavailable`
//! for something expressible, or worse, a value emitted onto a revision that
//! cannot express it.
//!
//! # What is and is not covered
//!
//! These test the *decisions*: what is expressible at a revision, which rule
//! answers a missing feature, and what the report says. The serde layer that
//! turns wire bytes into a `ToolResult` and back is T09, snapshot-tested per
//! revision there. What is proven here is that whichever bytes a codec produces,
//! the gate and the degrade rules treat them correctly — so a codec bug cannot
//! be masked by a lenient lowering path.

// A failed property has to panic with the generated case, so the test
// expectations live on the file rather than on every call site.
#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "property tests assert on fallible calls; propagating would replace a shrunk counterexample with a generic error"
)]

use proptest::prelude::*;
use protocol_droid::domain::ir::lower::{
    DegradeRule, Loss, LowerError, check_expressible, lower_tool_result, rule_for,
};
use protocol_droid::domain::ir::{AudioContent, Content, FeatureSet, TextContent, ToolResult};
use protocol_droid::domain::{
    CapabilitySet, DegradePolicy, Feature, FeatureProfile, Negotiation, ProtocolVersion,
    TransportKind, resolve,
};

/// A revision, drawn from the five published ones.
fn any_session() -> impl Strategy<Value = ProtocolVersion> {
    prop_oneof![
        Just(ProtocolVersion::V2024_11_05),
        Just(ProtocolVersion::V2025_03_26),
        Just(ProtocolVersion::V2025_06_18),
        Just(ProtocolVersion::V2025_11_25),
        Just(ProtocolVersion::V2026_07_28),
    ]
}

/// A session at `version` with both sides advertising everything that version
/// defines — the most permissive session a peer can hold.
fn session(version: ProtocolVersion) -> FeatureProfile {
    let caps = CapabilitySet::all_for(version);
    resolve(Negotiation {
        version,
        client_caps: caps,
        server_caps: caps,
        transport: TransportKind::Stdio,
    })
}

fn features_of(profile: &FeatureProfile) -> FeatureSet {
    FeatureSet::union_of(profile.iter())
}

fn text() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 _\\-\\.]{0,24}"
}

fn json_key() -> impl Strategy<Value = String> {
    "[a-z]{1,8}"
}

/// A text result, sometimes with `structuredContent` — the field only
/// `2025-06-18` and later can express.
fn any_text_result() -> impl Strategy<Value = ToolResult> {
    (text(), prop::option::of(json_key()), prop::bool::ANY).prop_map(|(body, key, with_json)| {
        let plain = ToolResult::text(body);
        match key {
            Some(key) if with_json => plain
                .clone()
                .with_structured(format!("{{\"{key}\":1}}"))
                .unwrap_or(plain),
            _ => plain,
        }
    })
}

/// A result carrying an audio block, which `2024-11-05` cannot express.
fn audio_result() -> impl Strategy<Value = ToolResult> {
    text().prop_map(|body| {
        ToolResult::new(vec![
            Content::Text(TextContent::new(body)),
            Content::Audio(AudioContent::new("AAA=", "audio/wav")),
        ])
    })
}

proptest! {
    /// A value the session can express goes through untouched; one it cannot is
    /// refused, and the gate agrees with the value-level path about which.
    #[test]
    fn lowering_is_exact_under_a_strict_policy(
        version in any_session(),
        result in any_text_result(),
    ) {
        let profile = session(version);
        let available = features_of(&profile);
        let required = result.required_features();
        let fits = required.is_subset_of(available);

        let gated = check_expressible(&required, &profile, version);
        let lowered =
            lower_tool_result(result.clone(), &profile, version, DegradePolicy::strict());

        prop_assert_eq!(gated.is_ok(), fits, "gate disagreed at {}", version);
        prop_assert_eq!(lowered.is_ok(), fits, "lowering disagreed at {}", version);

        match lowered {
            Ok((value, report)) => {
                prop_assert_eq!(value, result, "{}", version);
                prop_assert!(report.is_empty(), "{} reported a change", version);
                prop_assert!(report.is_lossless(), "{}", version);
            }
            Err(LowerError::FeatureUnavailable { negotiated, .. }) => {
                prop_assert_eq!(negotiated, version);
            }
            Err(other) => prop_assert!(false, "unexpected under strict: {other}"),
        }
    }

    /// A value both sessions can express survives the crossing unchanged. This
    /// is the round trip the capability names, at the layer where it is
    /// decidable.
    #[test]
    fn a_value_that_fits_survives_lowering_unchanged(
        from in any_session(),
        to in any_session(),
        result in any_text_result(),
    ) {
        let required = result.required_features();
        prop_assume!(required.is_subset_of(features_of(&session(from))));
        prop_assume!(required.is_subset_of(features_of(&session(to))));

        let (lowered, report) =
            lower_tool_result(result.clone(), &session(to), to, DegradePolicy::strict())
                .expect("the value fits the target session");

        prop_assert_eq!(lowered, result);
        prop_assert!(report.is_empty());
    }

    /// The spec-named fallback: when structured output cannot be expressed, the
    /// exact JSON bytes survive in a text block and the report says it cost
    /// nothing.
    #[test]
    fn the_structured_content_fallback_is_lossless(
        version in any_session(),
        body in text(),
        key in json_key(),
    ) {
        let profile = session(version);
        let available = features_of(&profile);
        let source = ToolResult::text(body)
            .with_structured(format!("{{\"{key}\":1}}"))
            .expect("hand-built object");
        let json = source
            .structured_content
            .as_ref()
            .map(|json| json.as_str().to_owned())
            .expect("just set");

        if source.required_features().is_subset_of(available) {
            let (result, report) =
                lower_tool_result(source.clone(), &profile, version, DegradePolicy::strict())
                    .expect("fits");
            prop_assert_eq!(result, source);
            prop_assert!(report.is_empty());
            return Ok(());
        }

        let policy = DegradePolicy::strict().with(Feature::ToolStructuredOutput);
        let (result, report) = lower_tool_result(source, &profile, version, policy)
            .expect("the policy authorises the fallback");

        // The field is gone, so no encoder can wrongly emit it.
        prop_assert!(result.structured_content.is_none());
        // The exact bytes survive as a text block.
        let Some(Content::Text(appended)) = result.content.last() else {
            return Err(TestCaseError::fail("no text block appended"));
        };
        prop_assert_eq!(&appended.text, &json);
        prop_assert!(result.is_lossless_anywhere());
        // And the report names what happened, and what it cost.
        prop_assert_eq!(report.len(), 1);
        let Some((step, _)) = report.steps().split_first() else {
            return Err(TestCaseError::fail("expected one step"));
        };
        prop_assert_eq!(step.rule, DegradeRule::StructuredContentToText, "{}", version);
        prop_assert_eq!(report.worst_loss(), Loss::None);
    }

    /// Anything a policy authorises is applied; anything it does not is refused.
    /// There is no third outcome, and a permissive result always fits the target.
    #[test]
    fn no_rule_fires_without_a_policy_naming_its_feature(
        version in any_session(),
        result in audio_result(),
    ) {
        let profile = session(version);
        let available = features_of(&profile);

        let strict = lower_tool_result(result.clone(), &profile, version, DegradePolicy::strict());
        let permissive =
            lower_tool_result(result, &profile, version, DegradePolicy::permissive());

        match (strict, permissive) {
            (Ok((value, report)), Ok((other, _))) => {
                prop_assert_eq!(value, other);
                prop_assert!(report.is_empty());
            }
            (Err(LowerError::FeatureUnavailable { .. }), Ok((value, report))) => {
                prop_assert!(!report.is_empty());
                prop_assert!(!report.is_lossless_or_flagged());
                // What came back fits the target, or the degrade is broken.
                prop_assert!(
                    value.required_features().is_subset_of(available),
                    "permissive lowering left a feature the target lacks",
                );
            }
            (strict, permissive) => prop_assert!(
                false,
                "unexpected pairing: strict={strict:?} permissive={permissive:?}",
            ),
        }
    }

    /// Whatever `rule_for` selects really does address the missing feature, and
    /// is really selectable. A rule that does neither would be a silent no-op
    /// wearing a downgrade's name.
    #[test]
    fn every_selectable_rule_addresses_the_missing_feature(index in 0_usize..64) {
        let all = Feature::ALL;
        if all.is_empty() {
            return Err(TestCaseError::reject("no features"));
        }
        let Some(feature) = all.get(index % all.len()).copied() else {
            return Err(TestCaseError::reject("index out of range"));
        };
        let Some(rule) = rule_for(feature, DegradePolicy::permissive()) else {
            // No rule is legitimate: not every feature is degradable.
            return Ok(());
        };
        prop_assert!(
            rule.is_fallback_for(feature),
            "{rule:?} was selected for {feature} but does not address it",
        );
        prop_assert!(
            DegradeRule::ALL.contains(&rule),
            "{rule:?} was selected but is not in ALL",
        );
    }

    /// The report is an audit trail, so it must not understate what was lost.
    #[test]
    fn the_report_never_understates_what_was_lost(
        version in any_session(),
        result in audio_result(),
    ) {
        let profile = session(version);
        let Ok((_value, report)) =
            lower_tool_result(result, &profile, version, DegradePolicy::permissive())
        else {
            return Ok(());
        };

        for step in report.steps() {
            prop_assert_eq!(step.loss(), step.rule.loss(), "{:?}", step);
        }
        if report.is_lossless() {
            prop_assert!(report.steps().iter().all(|s| s.loss().is_lossless()));
        }
        // An empty report claims nothing was lost, and that claim must hold.
        if report.is_empty() {
            prop_assert!(report.is_lossless());
        } else {
            prop_assert_eq!(report.len(), report.steps().len(), "{}", version);
        }
    }
}

/// Every revision can express a plain text result, and lowering to any of them
/// is the identity. This is the floor the whole matrix rests on.
#[test]
fn text_is_expressible_at_every_revision() {
    for version in ProtocolVersion::KNOWN {
        let profile = session(version);
        let result = ToolResult::text("hello");
        let (lowered, report) =
            lower_tool_result(result.clone(), &profile, version, DegradePolicy::strict())
                .unwrap_or_else(|error| panic!("{version}: {error}"));
        assert_eq!(lowered, result, "{version}");
        assert!(report.is_empty(), "{version}");
    }
}

/// The cross-revision matrix, read against the changelog rather than the code.
#[test]
fn the_cross_revision_matrix_is_stable() {
    let rows: Vec<(ProtocolVersion, bool, bool, bool)> = ProtocolVersion::KNOWN
        .into_iter()
        .map(|version| {
            let available = features_of(&session(version));
            (
                version,
                available.contains(Feature::ToolStructuredOutput),
                available.contains(Feature::ToolAudioContent),
                available.contains(Feature::ToolResourceLinks),
            )
        })
        .collect();

    // structuredContent and resource links are 2025-06-18; audio is 2025-03-26.
    let expected = [
        (ProtocolVersion::V2024_11_05, false, false, false),
        (ProtocolVersion::V2025_03_26, false, true, false),
        (ProtocolVersion::V2025_06_18, true, true, true),
        (ProtocolVersion::V2025_11_25, true, true, true),
        (ProtocolVersion::V2026_07_28, true, true, true),
    ];
    assert_eq!(rows.len(), expected.len());
    for ((version, structured, audio, links), (want, want_struct, want_audio, want_links)) in
        rows.iter().zip(expected)
    {
        assert_eq!(*version, want);
        assert_eq!(*structured, want_struct, "{version} structuredContent");
        assert_eq!(*audio, want_audio, "{version} audio");
        assert_eq!(*links, want_links, "{version} resource links");
    }
}

/// A report's predicates never disagree with its steps.
#[test]
fn report_predicates_agree_with_their_steps() {
    for version in ProtocolVersion::KNOWN {
        let profile = session(version);
        for result in [
            ToolResult::text("x"),
            ToolResult::new(vec![Content::Audio(AudioContent::new("AAA=", "audio/wav"))]),
            ToolResult::text("x")
                .with_structured(r#"{"a":1}"#)
                .expect("hand-built"),
        ] {
            let Ok((_value, report)) =
                lower_tool_result(result, &profile, version, DegradePolicy::permissive())
            else {
                continue;
            };
            if report.is_empty() {
                assert_eq!(report.len(), 0, "{version}");
                assert!(report.is_lossless(), "{version}");
            } else {
                assert_eq!(report.len(), report.steps().len(), "{version}");
            }
        }
    }
}

/// The missing feature is reported deterministically, so a bridge logging
/// errors can tell whether two peers hit the same wall.
#[test]
fn the_missing_feature_is_deterministic() {
    for version in ProtocolVersion::KNOWN {
        let profile = session(version);
        let available = features_of(&profile);
        let missing = FeatureSet::union_of(
            Feature::ALL
                .iter()
                .copied()
                .filter(|f| !available.contains(*f)),
        );
        if missing.is_empty() {
            continue;
        }
        let first = check_expressible(&missing, &profile, version).unwrap_err();
        for _ in 0..8 {
            let again = check_expressible(&missing, &profile, version).unwrap_err();
            assert_eq!(first, again, "{version}");
        }
    }
}
