//! The `2025-03-26` `CallToolResult` codec.
//!
//! # What this revision adds to `2024-11-05`
//!
//! - **Audio content.** "Added support for audio data" — the union's third arm
//!   is audio and the embedded resource moves to fourth. The order matters:
//!   the union is untagged, so every index below this one shifts too.
//! - **Typed `annotations`.** A real `Annotations` struct rather than a bare
//!   `serde_json::Value`, with an `extra` bag for what it does not name.
//!
//! # What it does not have
//!
//! No `lastModified` on annotations — that arrives at `2025-06-18`. No `meta`
//! on content blocks. No resource links. No `structuredContent` on the result.
//!
//! The one consequence worth stating: an `Annotations` carrying `lastModified`
//! cannot be written here, because the struct has no field to put it in and
//! `extra` is not a substitute — see
//! [`LowerError::FieldNotRepresentable`](crate::domain::ir::lower::LowerError).
//! The gate catches it first, on `ToolAnnotationsLastModified`.

use protocol_droid_schema::wire::v2025_03_26 as wire;

use super::{AUDIO_TYPE, IMAGE_TYPE, META_KEY, RESOURCE_TYPE, TEXT_TYPE};
use crate::domain::ir::Codec;
use crate::domain::ir::lower::{LowerError, TranslationReport, lower_tool_result};
use crate::domain::ir::{
    Annotations, AudioContent, Content, EmbeddedResource, ExtensionValue, Extensions, ImageContent,
    Role, TextContent, ToolResult,
};
use crate::domain::{
    CapabilitySet, DegradePolicy, FeatureProfile, Negotiation, ProtocolVersion, TransportKind,
    resolve,
};

/// Lifts this revision's typed `annotations`.
///
/// The `extra` bag goes into [`Annotations::extensions`] so a member this build
/// does not model is still carried. A total lift: there is no value a peer can
/// legitimately send here that this cannot represent.
fn lift_annotations(annotations: &wire::Annotations) -> Annotations {
    Annotations {
        audience: annotations
            .audience
            .iter()
            .flatten()
            .map(|role| match role {
                wire::Role::ASSISTANT => Role::Assistant,
                wire::Role::USER => Role::User,
            })
            .collect(),
        priority: annotations.priority,
        // `lastModified` does not exist at this revision, so there is no field
        // it could have arrived in. Anything with that spelling is in `extra`,
        // where the lift below preserves it.
        last_modified: None,
        extensions: Extensions::from_json_object(annotations.extra.clone()),
    }
}

/// Renders typed `annotations` back out.
///
/// `last_modified` is refused rather than dropped. The gate is what should have
/// caught it, on `ToolAnnotationsLastModified`; reaching here means the gate
/// admitted something it should not have, and writing the value into `extra`
/// would be the laundered version of the same failure the level-4 `meta` case
/// is about — a document the reader cannot parse, built by us instead of
/// reported by us.
fn lower_annotations(
    annotations: Option<&Annotations>,
) -> Result<Option<wire::Annotations>, LowerError> {
    let Some(annotations) = annotations else {
        return Ok(None);
    };
    if annotations.last_modified.is_some() {
        return Err(LowerError::FieldNotRepresentable {
            field: "annotations.lastModified".to_owned(),
            expected: "not a field of 2025-03-26 annotations",
            detail: "the value carries `lastModified`, which this revision's \
                     `Annotations` struct has no member for"
                .to_owned(),
            target: ProtocolVersion::V2025_03_26,
        });
    }

    let extra = annotations.extensions.to_json_object();
    if annotations.audience.is_empty() && annotations.priority.is_none() && extra.is_empty() {
        return Ok(None);
    }
    Ok(Some(wire::Annotations {
        audience: (!annotations.audience.is_empty()).then(|| {
            annotations
                .audience
                .iter()
                .map(|role| match role {
                    Role::Assistant => wire::Role::ASSISTANT,
                    Role::User => wire::Role::USER,
                })
                .collect()
        }),
        priority: annotations.priority,
        extra,
    }))
}

/// The `2025-03-26` codec.
#[derive(Debug, Clone, Copy)]
pub struct V2025_03_26;

impl Codec for V2025_03_26 {
    type Wire = wire::CallToolResult;

    fn version(&self) -> ProtocolVersion {
        ProtocolVersion::V2025_03_26
    }

    fn capabilities(&self) -> FeatureProfile {
        let caps = CapabilitySet::all_for(self.version());
        resolve(Negotiation {
            version: self.version(),
            client_caps: caps,
            server_caps: caps,
            transport: TransportKind::Stdio,
        })
    }

    fn lift(&self, wire: &Self::Wire) -> ToolResult {
        let mut extensions = Extensions::from_json_object(wire.extra.clone());
        if let Some(meta) = &wire.meta {
            extensions.insert(META_KEY, ExtensionValue::from(meta.clone()));
        }
        ToolResult {
            content: wire.content.iter().map(lift_block).collect(),
            // `structuredContent` does not exist at this revision. A value with
            // one cannot have been lifted from this revision, and a codec that
            // invented a place to put it would be inventing a field.
            structured_content: None,
            is_error: wire.is_error.unwrap_or(false),
            result_type: None,
            extensions,
        }
    }

    fn lower(
        &self,
        value: ToolResult,
        policy: DegradePolicy,
        report: &mut TranslationReport,
    ) -> Result<Self::Wire, LowerError> {
        let (value, lowered) =
            lower_tool_result(value, &self.capabilities(), self.version(), policy)?;
        report.absorb(&lowered);
        Ok(wire::CallToolResult {
            meta: value.extensions.get(META_KEY).map(serde_json::Value::from),
            content: value
                .content
                .iter()
                .map(lower_block)
                .collect::<Result<Vec<_>, _>>()?,
            is_error: value.is_error.then_some(true),
            extra: value.extensions.to_json_object(),
        })
    }
}

fn lift_block(block: &wire::CallToolResultContentChoice) -> Content {
    match block {
        wire::CallToolResultContentChoice::Member0(text) => Content::Text(TextContent {
            text: text.text.clone(),
            annotations: text.annotations.as_ref().map(lift_annotations),
            extensions: Extensions::from_json_object(text.extra.clone()),
        }),
        wire::CallToolResultContentChoice::Member1(image) => Content::Image(ImageContent {
            data: image.data.clone(),
            mime_type: image.mime_type.clone(),
            annotations: image.annotations.as_ref().map(lift_annotations),
            extensions: Extensions::from_json_object(image.extra.clone()),
        }),
        wire::CallToolResultContentChoice::Member2(audio) => Content::Audio(AudioContent {
            data: audio.data.clone(),
            mime_type: audio.mime_type.clone(),
            annotations: audio.annotations.as_ref().map(lift_annotations),
            extensions: Extensions::from_json_object(audio.extra.clone()),
        }),
        wire::CallToolResultContentChoice::Member3(resource) => {
            let wire::EmbeddedResource {
                annotations,
                resource,
                extra,
                ..
            } = resource;
            let (uri, mime_type, text, blob) = match resource {
                wire::EmbeddedResourceResourceChoice::Member0(text) => (
                    text.uri.clone(),
                    text.mime_type.clone(),
                    Some(text.text.clone()),
                    None,
                ),
                wire::EmbeddedResourceResourceChoice::Member1(blob) => (
                    blob.uri.clone(),
                    blob.mime_type.clone(),
                    None,
                    Some(blob.blob.clone()),
                ),
            };
            Content::EmbeddedResource(EmbeddedResource {
                uri,
                mime_type,
                text,
                blob,
                annotations: annotations.as_ref().map(lift_annotations),
                extensions: Extensions::from_json_object(extra.clone()),
            })
        }
    }
}

fn lower_block(block: &Content) -> Result<wire::CallToolResultContentChoice, LowerError> {
    let lowered = match block {
        Content::Text(text) => wire::CallToolResultContentChoice::Member0(wire::TextContent {
            annotations: lower_annotations(text.annotations.as_ref())?,
            text: text.text.clone(),
            r#type: TEXT_TYPE.to_owned(),
            extra: text.extensions.to_json_object(),
        }),
        Content::Image(image) => wire::CallToolResultContentChoice::Member1(wire::ImageContent {
            annotations: lower_annotations(image.annotations.as_ref())?,
            data: image.data.clone(),
            mime_type: image.mime_type.clone(),
            r#type: IMAGE_TYPE.to_owned(),
            extra: image.extensions.to_json_object(),
        }),
        Content::Audio(audio) => wire::CallToolResultContentChoice::Member2(wire::AudioContent {
            annotations: lower_annotations(audio.annotations.as_ref())?,
            data: audio.data.clone(),
            mime_type: audio.mime_type.clone(),
            r#type: AUDIO_TYPE.to_owned(),
            extra: audio.extensions.to_json_object(),
        }),
        Content::EmbeddedResource(resource) => {
            let contents = if let Some(text) = &resource.text {
                wire::EmbeddedResourceResourceChoice::Member0(wire::TextResourceContents {
                    mime_type: resource.mime_type.clone(),
                    text: text.clone(),
                    uri: resource.uri.clone(),
                    extra: serde_json::Map::new(),
                })
            } else if let Some(blob) = &resource.blob {
                wire::EmbeddedResourceResourceChoice::Member1(wire::BlobResourceContents {
                    blob: blob.clone(),
                    mime_type: resource.mime_type.clone(),
                    uri: resource.uri.clone(),
                    extra: serde_json::Map::new(),
                })
            } else {
                return Err(LowerError::FieldNotRepresentable {
                    field: "resource".to_owned(),
                    expected: "a text or blob body",
                    detail: "the resource has neither `text` nor `blob`".to_owned(),
                    target: ProtocolVersion::V2025_03_26,
                });
            };
            wire::CallToolResultContentChoice::Member3(wire::EmbeddedResource {
                annotations: lower_annotations(resource.annotations.as_ref())?,
                resource: contents,
                r#type: RESOURCE_TYPE.to_owned(),
                extra: resource.extensions.to_json_object(),
            })
        }
        Content::Unknown(unknown) => {
            return Err(LowerError::FieldNotRepresentable {
                field: unknown.type_name.clone(),
                expected: "a 2025-03-26 content block",
                detail: "this build does not model the block, and this revision has no \
                         member that could carry it"
                    .to_owned(),
                target: ProtocolVersion::V2025_03_26,
            });
        }
        Content::ResourceLink(_) => {
            return Err(LowerError::FieldNotRepresentable {
                field: "content".to_owned(),
                expected: "a 2025-03-26 content block",
                detail: "resource links arrive at 2025-06-18".to_owned(),
                target: ProtocolVersion::V2025_03_26,
            });
        }
    };
    Ok(lowered)
}

#[cfg(test)]
mod tests {

    /// What a test fails with.
    ///
    /// One type for every error a test body can hit, so `?` carries a codec
    /// error, a JSON error or a bare message alike and the harness prints it
    /// rather than the test having to choose between three error types.
    type TestError = Box<dyn std::error::Error>;

    use super::V2025_03_26;
    use std::assert_matches;

    use crate::domain::DegradePolicy;
    use crate::domain::ir::Codec;
    use crate::domain::ir::lower::{LowerError, TranslationReport};
    use crate::domain::ir::{Content, Role, ToolResult};
    use crate::domain::{Feature, ProtocolVersion};

    fn round_trip(json: &str) -> Result<serde_json::Value, TestError> {
        let codec = V2025_03_26;
        let wire: super::wire::CallToolResult = serde_json::from_str(json)?;
        let mut report = TranslationReport::new(codec.version());
        let lowered = codec.lower(codec.lift(&wire), DegradePolicy::strict(), &mut report)?;
        Ok(serde_json::to_value(&lowered)?)
    }

    /// The union is untagged, and audio and image are the same shape: a `type`,
    /// a `data` and a `mimeType`. Nothing in the JSON says which is which, so
    /// the member index is the only thing telling them apart and an off-by-one
    /// would swap them with no error at all.
    ///
    /// This is the test that fails if the arm order is wrong, which is why the
    /// assertion is on the IR variant rather than on re-serialisation: a block
    /// that arrived as an image re-serialises to a valid image.
    /// KNOWN BUG -- codegen does not enforce the `type` const.
    ///
    /// `ImageContent::r#type` is a plain `String` with `#[serde(rename =
    /// "type")]`, so an `ImageContent` accepts `{"type":"audio", ...}` without
    /// complaint. The union is untagged and tries members in order, so every
    /// `{type, data, mimeType}` block deserialises as the *image* arm and an
    /// audio block silently becomes an image -- the codec then writes
    /// `"type":"image"` and the data is corrupted with no error anywhere.
    ///
    /// The fix is in the generator: emit the specification's `const` as a
    /// single-valued type rather than a string. Until then this fails, and
    /// failing is the correct state -- it is the test that should be loud.
    #[test]
    fn audio_and_image_land_in_different_arms() -> Result<(), TestError> {
        let codec = V2025_03_26;
        let audio: super::wire::CallToolResult = serde_json::from_str(
            r#"{"content":[{"type":"audio","data":"AAA","mimeType":"audio/wav"}]}"#,
        )?;
        let image: super::wire::CallToolResult = serde_json::from_str(
            r#"{"content":[{"type":"image","data":"AAA","mimeType":"image/png"}]}"#,
        )?;

        assert!(matches!(
            codec.lift(&audio).content.first().ok_or("no audio block")?,
            Content::Audio(_)
        ));
        assert!(matches!(
            codec.lift(&image).content.first().ok_or("no image block")?,
            Content::Image(_)
        ));
        Ok(())
    }

    #[test]
    fn a_result_round_trips_through_json() -> Result<(), TestError> {
        for json in [
            r#"{"content":[{"type":"text","text":"hello"}]}"#,
            r#"{"content":[{"type":"resource","resource":{"uri":"file:///a","text":"body"}}]}"#,
            r#"{"content":[{"type":"text","text":"x","annotations":{"audience":["user"],"priority":0.5}}]}"#,
            r#"{"content":[],"isError":true}"#,
        ] {
            assert_eq!(
                round_trip(json)?,
                serde_json::from_str::<serde_json::Value>(json)?,
                "did not survive the crossing"
            );
        }
        Ok(())
    }

    /// Typed `annotations` means an unmodelled member lands in the struct's
    /// `extra` bag rather than anywhere else, and has to come back out.
    #[test]
    fn an_unmodelled_annotation_member_survives_the_structs_extra_bag() -> Result<(), TestError> {
        let json = r#"{"content":[{"type":"text","text":"x","annotations":{"audience":["user"],"unheardOf":true}}]}"#;
        assert_eq!(
            round_trip(json)?,
            serde_json::from_str::<serde_json::Value>(json)?
        );
        Ok(())
    }

    /// Annotations are typed here, so `lastModified` has no field to land in.
    /// The gate refuses the value on `ToolAnnotationsLastModified` before the
    /// codec sees it; the codec refusing it too is belt and braces, and what
    /// matters is that it refuses rather than laundering the value into `extra`
    /// where the reader would choke on it.
    /// The gate refuses `lastModified` before the codec is reached.
    ///
    /// Asserted on the error's _value_ rather than on its `Display`: the string
    /// form carries the feature as `{feature:?}`, which is the Rust variant
    /// name, so a substring test against the wire slug is a test of the
    /// formatting rather than of the behaviour. Matching the variant is what
    /// says the refusal was for the right reason.
    #[test]
    fn last_modified_is_refused_by_the_gate() -> Result<(), TestError> {
        let codec = V2025_03_26;
        let ir = ToolResult::new(vec![Content::Text(crate::domain::ir::TextContent {
            text: "x".to_owned(),
            annotations: Some(crate::domain::ir::Annotations {
                last_modified: Some("2025-06-18T00:00:00Z".to_owned()),
                audience: vec![Role::User],
                ..crate::domain::ir::Annotations::empty()
            }),
            extensions: crate::domain::ir::Extensions::empty(),
        })]);

        let mut report = TranslationReport::new(codec.version());
        let Err(error) = codec.lower(ir, DegradePolicy::strict(), &mut report) else {
            return Err("this must be refused".into());
        };
        assert_matches!(
            error,
            LowerError::FeatureUnavailable {
                feature: Feature::ToolAnnotationsLastModified,
                negotiated: ProtocolVersion::V2025_03_26,
            }
        );
        Ok(())
    }

    /// `structuredContent` does not exist at this revision, so there is no
    /// field to write it into and no way to carry it.
    #[test]
    fn structured_content_is_refused_by_the_gate() -> Result<(), TestError> {
        let codec = V2025_03_26;
        let ir = ToolResult::new(vec![Content::Text(crate::domain::ir::TextContent::new(
            "x",
        ))])
        .with_structured(r#"{"a":1}"#)?;

        let mut report = TranslationReport::new(codec.version());
        let Err(error) = codec.lower(ir, DegradePolicy::strict(), &mut report) else {
            return Err("this must be refused".into());
        };
        assert!(!error.to_string().is_empty());
        Ok(())
    }
}
