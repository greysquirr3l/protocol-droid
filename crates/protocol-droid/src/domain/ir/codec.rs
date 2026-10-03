//! The `Codec` port trait — the seam between the canonical IR and one revision.
//!
//! # Why this trait, and why here
//!
//! The canonical model is the point of the crate: application code speaks one
//! version-agnostic IR, and per-revision codecs lift wire messages into it and
//! lower them back out for whatever version was negotiated. This trait is that
//! seam, and it is deliberately narrow — one associated wire type, two
//! directions.
//!
//! It lives beside its vocabulary rather than in the adapter because the
//! *terms* belong to the translation path, not to the serialisation crate. An
//! adapter that owned the trait would make itself the owner of "what a codec
//! is", and the second consumer — a bridge, a prober, a conformance runner —
//! would have to import from the first to say what it wants.
//!
//! # The two directions are not symmetric, and the asymmetry is the design
//!
//! [`Codec::lift`] is **total**: every well-formed wire value produces an IR
//! value, because there is no such thing as a message the peer sent that the
//! bridge may refuse to represent. Fields this build does not name go to
//! [`Extensions`] and unrecognised content blocks go to
//! [`UnknownContent`](crate::domain::ir::UnknownContent), so a message survives
//! the crossing intact. A lift that could fail would mean some inbound message
//! was dropped silently, which is the one outcome a version-translating bridge
//! cannot have.
//!
//! [`Codec::lower`] is **partial**, and the failure is not a limitation to work
//! around but the whole point. A value may need a feature the target revision
//! cannot express. Then lowering either refuses with
//! [`LowerError::FeatureUnavailable`], or applies a rule the caller's
//! [`DegradePolicy`] named and records it in the [`TranslationReport`]. It never
//! emits a value it cannot account for — see [`lower::lower_tool_result`],
//! which is the decision half of this and is shared by every implementation.
//!
//! # What an implementor actually writes
//!
//! A codec is mostly mechanical translation between a generated wire struct
//! and the IR. Two things it must *not* decide:
//!
//! - Whether the value is expressible. That is the gate, and it is the same
//!   answer for every revision — asking it per-codec is how two revisions start
//!   disagreeing about what they support.
//! - How a value is degraded. [`DegradeRule`] owns that, so the report means
//!   the same thing whoever ran it.
//!
//! What an implementor *does* own is the shape: which fields its revision has,
//! how its content union is ordered, and how its own extra-field bag maps onto
//! [`Extensions`]. The generated types are the source of truth for all three,
//! and the union ordering in particular is not a detail — an `untagged` enum
//! matches the first member that round-trips, so a codec that assumes the wrong
//! order will silently accept an audio block as an image block.
//!
//! # Implementors
//!
//! See `adapters/secondary/codec` for the per-revision implementations. The
//! example below is the shape in miniature: a revision with one content kind
//! and no extra fields.
//!
//! ```
//! use protocol_droid::domain::ir::Codec;
//! use protocol_droid::domain::ir::lower::{LowerError, TranslationReport};
//! use protocol_droid::domain::ir::{Content, TextContent, ToolResult};
//! use protocol_droid::domain::{DegradePolicy, ProtocolVersion};
//!
//! /// A wire type, as the generated code would produce it.
//! #[derive(Debug, Clone, PartialEq)]
//! struct WireResult {
//!     content: Vec<WireText>,
//!     is_error: bool,
//! }
//!
//! #[derive(Debug, Clone, PartialEq)]
//! struct WireText {
//!     type_name: String,
//!     text: String,
//! }
//!
//! /// The revision that can only say one thing.
//! #[derive(Debug, Clone, Copy)]
//! struct TextOnlyRevision;
//!
//! impl Codec for TextOnlyRevision {
//!     type Wire = WireResult;
//!
//!     fn version(&self) -> ProtocolVersion {
//!         ProtocolVersion::V2024_11_05
//!     }
//!
//!     fn lift(&self, wire: &Self::Wire) -> ToolResult {
//!         let mut result = ToolResult::new(
//!             wire.content
//!                 .iter()
//!                 .map(|block| Content::Text(TextContent::new(block.text.clone())))
//!                 .collect(),
//!         );
//!         if wire.is_error {
//!             result = result.error();
//!         }
//!         result
//!     }
//!
//!     fn lower(
//!         &self,
//!         value: ToolResult,
//!         policy: DegradePolicy,
//!         report: &mut TranslationReport,
//!     ) -> Result<Self::Wire, LowerError> {
//!         // Every revision gates the same way, so ask once and delegate the
//!         // decisions to the shared path rather than re-deriving them here.
//!         let (lowered, _own) = protocol_droid::domain::ir::lower::lower_tool_result(
//!             value,
//!             &self.capabilities(),
//!             self.version(),
//!             policy,
//!         )?;
//!         let _ = report;
//!         Ok(WireResult {
//!             content: lowered
//!                 .content
//!                 .iter()
//!                 .map(|block| WireText {
//!                     type_name: "text".to_owned(),
//!                     text: match block {
//!                         Content::Text(text) => text.text.clone(),
//!                         // Unreachable: the gate refused anything else.
//!                         other => format!("{other:?}"),
//!                     },
//!                 })
//!                 .collect(),
//!             is_error: lowered.is_error,
//!         })
//!     }
//!
//!     fn capabilities(&self) -> protocol_droid::domain::FeatureProfile {
//!         let caps = protocol_droid::domain::CapabilitySet::all_for(self.version());
//!         protocol_droid::domain::resolve(protocol_droid::domain::Negotiation {
//!             version: self.version(),
//!             client_caps: caps,
//!             server_caps: caps,
//!             transport: protocol_droid::domain::TransportKind::Stdio,
//!         })
//!     }
//! }
//!
//! // It lifts and lowers, and a value it can express round-trips unchanged.
//! let codec = TextOnlyRevision;
//! let wire = WireResult {
//!     content: vec![WireText {
//!         type_name: "text".to_owned(),
//!         text: "hello".to_owned(),
//!     }],
//!     is_error: false,
//! };
//! let lifted = codec.lift(&wire);
//! assert_eq!(lifted.content.len(), 1);
//!
//! let mut report = TranslationReport::new(codec.version());
//! let lowered = codec
//!     .lower(lifted.clone(), DegradePolicy::strict(), &mut report)
//!     .expect("expressible");
//! assert_eq!(lowered, wire);
//! assert!(report.is_empty());
//! ```

use crate::domain::ir::ToolResult;
use crate::domain::ir::lower::{LowerError, TranslationReport};
use crate::domain::{DegradePolicy, FeatureProfile, ProtocolVersion};

/// Translates between one revision's wire type and the canonical IR.
///
/// See the [module docs](self) for the contract. The short version: [`lift`](Self::lift)
/// is total, [`lower`](Self::lower) is gated.
pub trait Codec {
    /// The generated wire type for this revision.
    ///
    /// Named per implementation rather than fixed, because the revisions do not
    /// agree on their shapes: `CallToolResult.content` is
    /// `Vec<CallToolResultContentChoice>` at `2024-11-05` and `Vec<ContentBlock>`
    /// from `2025-06-18`, and only `structuredContent` distinguishes the
    /// revisions that can carry it. A single wire type would have to be the
    /// least common denominator, which is a type that cannot express anything
    /// — the mistake the IR exists to avoid.
    type Wire;

    /// The revision this codec speaks.
    fn version(&self) -> ProtocolVersion;

    /// What this revision can express, as a negotiated profile.
    ///
    /// Used by the gate, so it must be the *real* profile for a peer speaking
    /// this revision — the intersection of what both sides advertise, not what
    /// the revision permits in the abstract. A codec that answers with the
    /// revision's maximum would let a value through that the actual peer cannot
    /// read.
    fn capabilities(&self) -> FeatureProfile;

    /// Wire to IR.
    ///
    /// Infallible by design: a message the peer sent always has an IR form. Use
    /// [`UnknownContent`](crate::domain::ir::UnknownContent) for a block this
    /// build does not recognise, and [`Extensions`] for fields it does not
    /// name, so nothing is lost in the crossing.
    fn lift(&self, wire: &Self::Wire) -> ToolResult;

    /// IR to wire.
    ///
    /// # Errors
    ///
    /// [`LowerError::FeatureUnavailable`] when the value needs something this
    /// revision cannot express and the policy authorises no rule for it. Any
    /// rule the policy *does* authorise is applied and recorded in `report`.
    fn lower(
        &self,
        value: ToolResult,
        policy: DegradePolicy,
        report: &mut TranslationReport,
    ) -> Result<Self::Wire, LowerError>;
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::expect_used,
        reason = "tests assert on fallible calls; propagating would replace a clear failure with noise"
    )]

    use super::Codec;
    use crate::domain::ir::lower::{TranslationReport, lower_tool_result};
    use crate::domain::ir::{Content, TextContent, ToolResult};
    use crate::domain::{
        CapabilitySet, DegradePolicy, Feature, Negotiation, ProtocolVersion, TransportKind, resolve,
    };

    /// A minimal revision: text only, no extra fields, nothing structured.
    #[derive(Debug, Clone, Copy)]
    struct TextOnly;

    impl Codec for TextOnly {
        type Wire = Vec<String>;

        fn version(&self) -> ProtocolVersion {
            ProtocolVersion::V2024_11_05
        }

        fn capabilities(&self) -> crate::domain::FeatureProfile {
            let caps = CapabilitySet::all_for(self.version());
            resolve(Negotiation {
                version: self.version(),
                client_caps: caps,
                server_caps: caps,
                transport: TransportKind::Stdio,
            })
        }

        fn lift(&self, wire: &Self::Wire) -> ToolResult {
            ToolResult::new(
                wire.iter()
                    .map(|text| Content::Text(TextContent::new(text.clone())))
                    .collect(),
            )
        }

        fn lower(
            &self,
            value: ToolResult,
            _policy: DegradePolicy,
            _report: &mut TranslationReport,
        ) -> Result<Self::Wire, super::LowerError> {
            Ok(value
                .content
                .iter()
                .map(|block| match block {
                    Content::Text(text) => text.text.clone(),
                    other => format!("{other:?}"),
                })
                .collect())
        }
    }

    /// A codec that can express what its wire type can, round-trips
    /// unchanged. The contract every implementation has to keep.
    #[test]
    fn a_value_the_revision_expresses_round_trips() {
        let codec = TextOnly;
        let ir = ToolResult::text("hello");
        let mut report = TranslationReport::new(codec.version());
        let wire = codec
            .lower(ir.clone(), DegradePolicy::strict(), &mut report)
            .expect("expressible");
        assert_eq!(wire, vec!["hello".to_owned()]);
        assert_eq!(codec.lift(&wire), ir);
        assert!(report.is_empty());
    }

    /// A lift is total: whatever is on the wire produces an IR value. The
    /// contract that keeps a bridge from dropping a message it did not expect.
    #[test]
    fn lifting_never_fails() {
        let codec = TextOnly;
        for wire in [
            vec![],
            vec!["a".to_owned()],
            vec!["a".to_owned(), "b".to_owned()],
        ] {
            let ir = codec.lift(&wire);
            assert_eq!(ir.content.len(), wire.len());
        }
    }

    /// The gate is the same answer whoever asks, so a codec cannot disagree
    /// with `lower_tool_result` about expressibility.
    #[test]
    fn a_codec_and_the_gate_agree() {
        let codec = TextOnly;
        let profile = codec.capabilities();
        for value in [
            ToolResult::text("x"),
            ToolResult::text("x")
                .with_structured(r#"{"a":1}"#)
                .expect("hand-built"),
        ] {
            let fits = value
                .required_features()
                .is_subset_of(crate::domain::ir::FeatureSet::union_of(profile.iter()));
            let gated = lower_tool_result(
                value.clone(),
                &profile,
                codec.version(),
                DegradePolicy::strict(),
            );
            assert_eq!(fits, gated.is_ok(), "{value:?}");
        }
    }

    /// `capabilities` has to describe the revision, not an idealised peer.
    #[test]
    fn capabilities_describe_this_revision() {
        let codec = TextOnly;
        let profile = codec.capabilities();
        // `FeatureProfile` is a bare bitset — the version lives on the codec,
        // not on the profile, so a codec cannot report a version it does not
        // speak.
        // 2024-11-05 has no structuredContent and no audio.
        assert!(!profile.allows(Feature::ToolStructuredOutput));
        assert!(!profile.allows(Feature::ToolAudioContent));
        // But it does have tools.
        assert!(profile.allows(Feature::Tools));
    }
}
