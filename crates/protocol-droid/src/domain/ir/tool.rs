//! `ToolResult` — the value T07's named scenario is about.
//!
//! # The scenario
//!
//! `structured-output-downlevel`: a `2025-06-18` server returns
//! `structuredContent`, and the downstream client negotiated `2025-03-26`,
//! which has no such field. The specification's recommended fallback is to
//! serialise the object and emit it as a text content block, and to record the
//! downgrade.
//!
//! The fallback lives here rather than in T08's encoder because it is a
//! *content* decision, not a wire one: the same downgraded result is valid on
//! any revision that has text content, and T08's job is only to check that.
//! Putting it in the encoder would mean two encoder paths — one that falls
//! back and one that does not — and the second would be the one that gets used
//! by mistake.
//!
//! # Why appending rather than replacing
//!
//! [`ToolResult::downgrade_structured_output`] appends the serialised JSON as
//! a text block and *clears* `structured_content`. Appending preserves the
//! result's own content, which is the part the tool meant to be read; clearing
//! prevents a lowering bug from leaving a field on a revision that has no field
//! for it. The returned [`Downgrade`] names the feature lost, so the caller can
//! put it in a `TranslationReport` without re-deriving it.

use crate::domain::Feature;
use crate::domain::ir::content::{Content, TextContent};
use crate::domain::ir::extensions::Extensions;
use crate::domain::ir::features::FeatureSet;
use crate::domain::ir::json::JsonObject;

/// The result of calling a tool.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolResult {
    /// The result's content blocks.
    pub content: Vec<Content>,
    /// Structured output, `2025-06-18` and later (PR #371).
    pub structured_content: Option<JsonObject>,
    /// Whether the call failed. Every revision.
    pub is_error: bool,
    /// Unmodelled fields.
    pub extensions: Extensions,
}

impl ToolResult {
    /// A result with the given content and no structured output.
    #[must_use]
    pub const fn new(content: Vec<Content>) -> Self {
        Self {
            content,
            structured_content: None,
            is_error: false,
            extensions: Extensions::empty(),
        }
    }

    /// A result that is only text.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::new(vec![Content::Text(TextContent::new(text))])
    }

    /// Attaches structured output, which is what makes this a
    /// `2025-06-18`-or-later value.
    ///
    /// # Errors
    ///
    /// [`JsonObjectError`](crate::domain::ir::JsonObjectError) when `json` is
    /// not a JSON object.
    pub fn with_structured(
        mut self,
        json: impl Into<String>,
    ) -> Result<Self, crate::domain::ir::JsonObjectError> {
        self.structured_content = Some(JsonObject::new(json)?);
        Ok(self)
    }

    /// Marks the result as an error.
    #[must_use]
    pub const fn error(mut self) -> Self {
        self.is_error = true;
        self
    }

    /// The features this result needs in order to be expressed.
    ///
    /// The union of what every content block needs, plus
    /// [`Feature::ToolStructuredOutput`] when there is structured output. Both
    /// halves matter: a result of three audio blocks needs
    /// `ToolAudioContent` exactly as much as a lone one does.
    #[must_use]
    pub fn required_features(&self) -> FeatureSet {
        let mut required = FeatureSet::empty();
        for block in &self.content {
            required = required.union(block.required_features());
        }
        if self.structured_content.is_some() {
            required = required.union(FeatureSet::empty().with(Feature::ToolStructuredOutput));
        }
        required
    }

    /// Whether this result can be expressed without losing anything.
    ///
    /// This is the question lowering asks, and it is *not* the same as
    /// "the session has the features" — a result with no structured output and
    /// no exotic content needs nothing and is lowerable everywhere.
    #[must_use]
    pub fn is_lossless_anywhere(&self) -> bool {
        self.required_features().is_empty()
    }

    /// Applies the `structured-output-downlevel` fallback.
    ///
    /// Appends the serialised `structuredContent` as a text content block and
    /// clears the field. Returns `None` when there is nothing to downgrade, so
    /// a caller can write `if let Some(downgrade) = ...` and only record a
    /// report when a real change happened — a result that was already
    /// lowerable must not produce a spurious entry in a `TranslationReport`.
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol_droid::domain::ir::{Content, ToolResult};
    ///
    /// let mut result = ToolResult::text("done").with_structured(r#"{"ok":true}"#)?;
    /// assert_eq!(result.required_features().len(), 1);
    ///
    /// let downgrade = result.downgrade_structured_output().unwrap();
    /// assert_eq!(downgrade.lost, protocol_droid::domain::Feature::ToolStructuredOutput);
    /// // The original text survived, and the JSON was appended.
    /// assert_eq!(result.content.len(), 2);
    /// assert!(result.structured_content.is_none());
    /// // And it is now expressible at every revision.
    /// assert!(result.is_lossless_anywhere());
    /// # Ok::<(), protocol_droid::domain::ir::JsonObjectError>(())
    /// ```
    #[must_use]
    pub fn downgrade_structured_output(&mut self) -> Option<Downgrade> {
        let structured = self.structured_content.take()?;
        let json = structured.into_string();
        self.content.push(Content::Text(TextContent::new(json)));
        Some(Downgrade {
            lost: Feature::ToolStructuredOutput,
            note: "structuredContent serialised into a text content block",
        })
    }
}

/// A change lowering made to make a value expressible.
///
/// Describes what happened; the value itself is changed in place, so there is
/// nothing to hand back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Downgrade {
    /// The feature that was given up.
    pub lost: Feature,
    /// A human-readable description, for the `TranslationReport`.
    pub note: &'static str,
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::panic,
        clippy::unwrap_used,
        reason = "tests assert on fallible calls and unwrap with the failing precondition in the message"
    )]

    use super::ToolResult;
    use crate::domain::Feature;
    use crate::domain::ir::content::{AudioContent, Content, ImageContent, TextContent};

    #[test]
    fn a_plain_text_result_needs_nothing() {
        let result = ToolResult::text("hello");
        assert!(result.required_features().is_empty());
        assert!(result.is_lossless_anywhere());
        assert!(!result.is_error);
    }

    #[test]
    fn structured_output_needs_exactly_one_feature() {
        let result = ToolResult::text("x").with_structured(r#"{"a":1}"#).unwrap();
        let required = result.required_features();
        assert!(required.contains(Feature::ToolStructuredOutput));
        assert_eq!(required.len(), 1);
        assert!(!result.is_lossless_anywhere());
    }

    /// The union, not the max: a result with audio *and* structured output
    /// needs both, and a caller that checked only the first would emit a field
    /// the target revision has no place for.
    #[test]
    fn requirements_union_across_blocks_and_fields() {
        let result = ToolResult::new(vec![
            Content::Text(TextContent::new("x")),
            Content::Image(ImageContent::new("A", "image/png")),
            Content::Audio(AudioContent::new("A", "audio/wav")),
        ])
        .with_structured(r#"{"a":1}"#)
        .unwrap();
        let required = result.required_features();
        assert_eq!(required.len(), 2);
        assert!(required.contains(Feature::ToolAudioContent));
        assert!(required.contains(Feature::ToolStructuredOutput));
    }

    #[test]
    fn duplicates_of_the_same_block_need_only_one_feature() {
        let result = ToolResult::new(vec![
            Content::Audio(AudioContent::new("A", "audio/wav")),
            Content::Audio(AudioContent::new("B", "audio/wav")),
            Content::Audio(AudioContent::new("C", "audio/wav")),
        ]);
        assert_eq!(result.required_features().len(), 1);
    }

    /// The named scenario, end to end.
    #[test]
    fn structured_output_downlevel_scenario() {
        let source = ToolResult::new(vec![Content::Text(TextContent::new("ok"))])
            .with_structured(r#"{"temperature":0.7,"unit":"c"}"#)
            .unwrap();

        // Before: needs the structured-output feature, so a 2025-03-26 peer
        // cannot be given it as-is.
        assert!(
            source
                .required_features()
                .contains(Feature::ToolStructuredOutput)
        );

        let mut result = source;
        let downgrade = result.downgrade_structured_output().unwrap();
        assert_eq!(downgrade.lost, Feature::ToolStructuredOutput);
        assert!(!downgrade.note.is_empty());

        // The field is gone, so there is nothing left for an encoder to
        // wrongly emit.
        assert!(result.structured_content.is_none());
        // The tool's own content survived, and the JSON was appended.
        assert_eq!(result.content.len(), 2);
        let Some(Content::Text(added)) = result.content.get(1) else {
            panic!("expected an appended text block");
        };
        assert_eq!(added.text, r#"{"temperature":0.7,"unit":"c"}"#);
        // And it is now expressible at every revision, which is the whole
        // point of the fallback.
        assert!(result.is_lossless_anywhere());
    }

    /// A result with nothing to downgrade reports nothing, so a
    /// `TranslationReport` does not fill with entries for values that were
    /// already fine.
    #[test]
    fn a_result_with_nothing_to_downgrade_reports_nothing() {
        assert!(
            ToolResult::text("x")
                .downgrade_structured_output()
                .is_none()
        );
    }

    /// The downgrade is idempotent in the way that matters: applying it twice
    /// cannot append the JSON twice, because the second call has nothing left
    /// to take.
    #[test]
    fn downgrading_twice_appends_once() {
        let mut once = ToolResult::text("x").with_structured(r#"{"a":1}"#).unwrap();
        assert!(once.downgrade_structured_output().is_some());
        assert!(once.downgrade_structured_output().is_none());
        assert_eq!(once.content.len(), 2);
    }

    #[test]
    fn the_error_flag_survives_a_downgrade() {
        let mut failed = ToolResult::text("boom")
            .error()
            .with_structured(r#"{"e":1}"#)
            .unwrap();
        assert!(failed.downgrade_structured_output().is_some());
        assert!(failed.is_error);
    }

    /// The appended text is the exact bytes that were received, which is what
    /// makes the fallback lossless rather than lossy-but-readable.
    #[test]
    fn the_appended_text_is_byte_identical() {
        let json = r#"{ "unit" : "c" , "n" : 1.0 }"#;
        let mut downgraded = ToolResult::text("x").with_structured(json).unwrap();
        assert!(downgraded.downgrade_structured_output().is_some());
        let Some(Content::Text(added)) = downgraded.content.get(1) else {
            panic!("expected an appended text block");
        };
        assert_eq!(added.text, json);
    }

    #[test]
    fn a_non_object_is_refused_at_construction() {
        assert!(ToolResult::text("x").with_structured("[]").is_err());
        assert!(ToolResult::text("x").with_structured("{oops").is_err());
    }
}
