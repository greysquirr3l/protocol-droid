//! Turning a handler's own [`ToolResult`] into the document the wire answers with.
//!
//! # Why this is not `application::server::result::success`
//!
//! `success` builds its answer from a `Serialize`, and the only document shape
//! it can produce is one text block plus, when the answer is an object, a
//! `structuredContent` carrying the same JSON. That is the right shape for a
//! handler returning a value, and it is the *only* shape it can produce — so a
//! tool that wanted to answer with an image, an audio block or an embedded
//! resource had no way to say so, and no setting that changed it.
//!
//! The IR already models all of them (`Content::Image`, `Content::Audio`,
//! `Content::EmbeddedResource`, and the rest), and every per-revision codec
//! already knows how each revision spells them. The gap was the one hop
//! between a handler and that codec.
//!
//! # Why the codec, and not a serialiser here
//!
//! Hand-writing the JSON would be a second answer to a question the crate
//! already answers per revision: `2024-11-05` has no audio block, `2025-03-26`
//! introduced it, `2025-06-18` added resource links and `structuredContent`,
//! `2026-07-28` added `resultType`. Those are facts about vendored schemas, and
//! they are recorded in `codec/result/`. Answering them again here is how two
//! revisions start disagreeing about what they support.
//!
//! So this lowers through [`WireToolResults`] and lets it fail. A tool whose
//! answer the negotiated revision cannot express gets a typed error rather
//! than a result with a block quietly missing from it — [`DegradePolicy::strict`]
//! is deliberate. A server serving a client is not a prober: it should say
//! "this peer negotiated a revision I cannot answer in" rather than invent a
//! degraded answer, and the caller of a `DegradePolicy::permissive` downgrade
//! gets to make that choice explicitly where they can see it.
//!
//! # What the version is for
//!
//! It is the *negotiated* version, taken from the session, never a guess. That
//! is the one value that decides the answer, and it is the value the crate
//! already settled at handshake.

use serde_json::Value;

use crate::application::bridge::ToolResultCodec;
use crate::application::server::result::OutcomeError;
use crate::domain::ir::{JsonObject, ToolResult, TranslationReport};
use crate::domain::{DegradePolicy, ProtocolVersion};

use super::WireToolResults;

/// Lowers a handler's [`ToolResult`] into the `tools/call` result document.
///
/// `version` is the negotiated revision, from `ctx.peer().negotiated()`.
///
/// The report is discarded: a strict policy performs no downgrades, so there
/// is nothing in it to report. Building it and throwing it away is still the
/// right call — it is what makes a future permissive variant a one-line change
/// rather than a second signature.
///
/// # Errors
///
/// [`OutcomeError::NotLowerable`] when `result` needs a feature the negotiated
/// revision cannot express. This is a server-side answer, not a bad request,
/// so the caller turns it into an error *result* rather than a transport
/// failure; see `application::server::result::failure`.
///
/// [`OutcomeError::NotAnObject`] when the codec produced something that is not
/// a JSON object. A `CallToolResult` is always an object, so this means the
/// codec and the dispatcher disagree about the shape — a bug, propagated
/// rather than swallowed.
pub fn verbatim(version: ProtocolVersion, result: &ToolResult) -> Result<JsonObject, OutcomeError> {
    let mut report = TranslationReport::default();
    let value: Value = WireToolResults
        .lower(
            version,
            result.clone(),
            DegradePolicy::strict(),
            &mut report,
        )
        .map_err(OutcomeError::NotLowerable)?;
    JsonObject::from_value(value).map_err(OutcomeError::NotAnObject)
}

#[cfg(test)]
mod tests {
    use super::verbatim;
    use crate::domain::ProtocolVersion;
    use crate::domain::ir::{AudioContent, Content, ImageContent, ToolResult};

    /// A result with one text block, the smallest answer there is.
    fn text_result(text: &str) -> ToolResult {
        ToolResult::text(text)
    }

    /// The lowered document as a `serde_json::Value`, which is what a peer
    /// would read off the wire.
    ///
    /// `None` means the answer did not lower at all, which is the signal the
    /// refusal tests assert on.
    fn document(version: ProtocolVersion, result: &ToolResult) -> Option<serde_json::Value> {
        let lowered = verbatim(version, result).ok()?;
        serde_json::from_str(lowered.as_str()).ok()
    }

    /// The first content block of a lowered document.
    fn first_block(value: &serde_json::Value) -> Option<&serde_json::Value> {
        value
            .get("content")
            .and_then(serde_json::Value::as_array)
            .and_then(|blocks| blocks.first())
    }

    /// The `isError` flag, read as a bool.
    ///
    /// `None` when the member is absent or is not a bool.
    fn is_error(value: &serde_json::Value) -> Option<bool> {
        value.get("isError").and_then(serde_json::Value::as_bool)
    }

    /// Whether the document does not claim a failure.
    ///
    /// Absent counts. The generated wire types skip `isError` when it is
    /// false, because `false` is the schema's default and every revision that
    /// has the field defaults it — so a successful result legitimately has no
    /// `isError` member at all, and a test that required one would be
    /// asserting the serialiser's spelling rather than the specification's
    /// meaning.
    fn is_success(value: &serde_json::Value) -> bool {
        matches!(is_error(value), None | Some(false))
    }

    #[test]
    fn a_text_answer_lands_in_the_content_array() -> Result<(), String> {
        let result = text_result("hello");
        let value =
            document(ProtocolVersion::V2025_06_18, &result).ok_or("the document did not lower")?;
        let block = first_block(&value).ok_or("no content block")?;

        match block.get("text").and_then(serde_json::Value::as_str) {
            Some("hello") => {}
            other => return Err(format!("the text block was {other:?}")),
        }
        if is_success(&value) {
            Ok(())
        } else {
            Err(format!("a successful result claimed an error: {value}"))
        }
    }

    /// The case `success` could not serve: a block that is not text, which
    /// only reaches the wire because `verbatim` goes through the codec.
    #[test]
    fn an_image_answer_keeps_its_type_and_data() -> Result<(), String> {
        let image = ImageContent::new("aGk=", "image/png");
        let result = ToolResult::new(vec![Content::Image(image)]);
        let value =
            document(ProtocolVersion::V2025_06_18, &result).ok_or("the document did not lower")?;
        let block = first_block(&value).ok_or("no content block")?;

        match block.get("type").and_then(serde_json::Value::as_str) {
            Some("image") => {}
            other => return Err(format!("the block type was {other:?}")),
        }
        match block.get("data").and_then(serde_json::Value::as_str) {
            Some("aGk=") => Ok(()),
            other => Err(format!("the data was {other:?}")),
        }
    }

    /// The reason `verbatim` exists rather than a serialiser: the codec knows
    /// `2024-11-05` has no audio block and `2025-03-26` does. Both revisions
    /// are named here precisely because the difference between them is the
    /// whole justification.
    #[test]
    fn audio_is_refused_by_the_revision_that_has_no_audio_block() -> Result<(), String> {
        let audio = AudioContent::new("aGk=", "audio/wav");
        let result = ToolResult::new(vec![Content::Audio(audio)]);

        if document(ProtocolVersion::V2025_03_26, &result).is_none() {
            return Err("2025-03-26 refused an audio block it introduced".to_owned());
        }
        if document(ProtocolVersion::V2024_11_05, &result).is_some() {
            return Err("2024-11-05 answered with an audio block it does not have".to_owned());
        }
        Ok(())
    }

    /// The flag has to survive, or a tool cannot report a failure that is a
    /// result rather than a refusal.
    #[test]
    fn an_error_flag_survives_the_lowering() -> Result<(), String> {
        let result = text_result("nope").error();
        let value =
            document(ProtocolVersion::V2025_06_18, &result).ok_or("the document did not lower")?;

        match is_error(&value) {
            Some(true) => Ok(()),
            other => Err(format!("isError was {other:?}")),
        }
    }
}
