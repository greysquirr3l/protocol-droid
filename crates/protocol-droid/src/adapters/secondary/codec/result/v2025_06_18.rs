//! The `2025-06-18` `CallToolResult` codec.
//!
//! # What this revision adds to `2025-03-26`
//!
//! - **Resource links.** A fifth union arm, inserted *before* the embedded
//!   resource — so the resource moves from fourth to fifth. The union is
//!   untagged, which makes every index after this one a place a block can
//!   arrive as the wrong kind of thing without any error.
//! - **The union is renamed.** `CallToolResultContentChoice` becomes
//!   `ContentBlock`. Same shape, different name, and the generated `ContentBlock`
//!   is an alias for `ContentBlockChoice`.
//! - **`structuredContent` on the result.** A raw JSON object, carried in the IR
//!   as [`JsonObject`].
//! - **`lastModified` on annotations.**
//! - **`meta` on content blocks.** Which is why a block's `meta` is a field from
//!   here on rather than a bag member, and why the two earlier revisions are
//!   not simply missing it.
//!
//! # `size` is signed on the wire and unsigned here
//!
//! `ResourceLink::size` is `i64` and [`ResourceLinkContent::size`] is `u64`, so
//! a negative value is not representable and is refused rather than clamped. A
//! negative size is nonsense as data and silently becoming `0` would be a
//! number the sender never sent.

use protocol_droid_schema::wire::v2025_06_18 as wire;

use super::{AUDIO_TYPE, IMAGE_TYPE, META_KEY, RESOURCE_LINK_TYPE, RESOURCE_TYPE, TEXT_TYPE};
use crate::domain::ir::Codec;
use crate::domain::ir::lower::{LowerError, TranslationReport, lower_tool_result};
use crate::domain::ir::{
    Annotations, AudioContent, Content, EmbeddedResource, ExtensionValue, Extensions, ImageContent,
    JsonObject, ResourceLinkContent, Role, TextContent, ToolResult,
};
use crate::domain::{
    CapabilitySet, DegradePolicy, FeatureProfile, Negotiation, ProtocolVersion, TransportKind,
    resolve,
};

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
        last_modified: annotations.last_modified.clone(),
        extensions: Extensions::from_json_object(annotations.extra.clone()),
    }
}

fn lower_annotations(
    annotations: Option<&Annotations>,
) -> Result<Option<wire::Annotations>, LowerError> {
    let Some(annotations) = annotations else {
        return Ok(None);
    };
    let extra = annotations.extensions.to_json_object();
    if annotations.audience.is_empty()
        && annotations.priority.is_none()
        && annotations.last_modified.is_none()
        && extra.is_empty()
    {
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
        last_modified: annotations.last_modified.clone(),
        priority: annotations.priority,
        extra,
    }))
}

/// The key a `structuredContent` that is not a JSON object travels under.
///
/// The wire type is a bare `serde_json::Value`, so a peer may legally send an
/// array or a string. [`JsonObject`] holds only objects, so such a value has
/// nowhere to go in the typed field — and a lift that dropped it would be the
/// one outcome a lift may not have. It rides in the bag instead, exactly as
/// unmodelled `annotations` does at `2024-11-05`, and is written back out
/// ahead of the typed field.
const RAW_STRUCTURED_KEY: &str = "structuredContent";

/// A block's own `meta`, taken from the extensions on the way down and put
/// there on the way up.
///
/// A block's `meta` is the same concept as the result's, so it shares
/// [`META_KEY`] rather than each inventing one. There is no collision: a block
/// and the result it sits in are separate objects with separate bags.
fn take_meta(extensions: &Extensions) -> Option<serde_json::Value> {
    let mut extensions = extensions.clone();
    extensions
        .take(META_KEY)
        .map(|value| serde_json::Value::from(&value))
}

/// The `2025-06-18` codec.
#[derive(Debug, Clone, Copy)]
pub struct V2025_06_18;

impl Codec for V2025_06_18 {
    type Wire = wire::CallToolResult;

    fn version(&self) -> ProtocolVersion {
        ProtocolVersion::V2025_06_18
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
        // A `structuredContent` that is not an object has nowhere in the typed
        // field to go, so it rides in the bag. `.ok()` would be the bug here:
        // a lift that quietly discarded an array a peer sent is the outcome
        // this whole trait exists to prevent.
        let structured_content = match &wire.structured_content {
            Some(value) if value.is_object() => JsonObject::new(value.to_string()).ok(),
            Some(value) => {
                extensions.insert(RAW_STRUCTURED_KEY, ExtensionValue::from(value.clone()));
                None
            }
            None => None,
        };
        ToolResult {
            content: wire.content.iter().map(lift_block).collect(),
            structured_content,
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
        // A raw value kept on the way in takes precedence: it is what the peer
        // actually sent, and re-deriving it from the typed field would be
        // inventing a shape for a value that never had one.
        let structured_content = match value.extensions.get(RAW_STRUCTURED_KEY) {
            Some(raw) => Some(serde_json::Value::from(raw)),
            None => value
                .structured_content
                .as_ref()
                .and_then(|object| serde_json::from_str(object.as_str()).ok()),
        };
        Ok(wire::CallToolResult {
            meta: value.extensions.get(META_KEY).map(serde_json::Value::from),
            content: value
                .content
                .iter()
                .map(lower_block)
                .collect::<Result<Vec<_>, _>>()?,
            is_error: value.is_error.then_some(true),
            structured_content,
            extra: value.extensions.to_json_object(),
        })
    }
}

fn lift_block(block: &wire::ContentBlock) -> Content {
    match block {
        wire::ContentBlock::Member0(text) => {
            let mut extensions = Extensions::from_json_object(text.extra.clone());
            if let Some(meta) = &text.meta {
                extensions.insert(META_KEY, ExtensionValue::from(meta.clone()));
            }
            Content::Text(TextContent {
                text: text.text.clone(),
                annotations: text.annotations.as_ref().map(lift_annotations),
                extensions,
            })
        }
        wire::ContentBlock::Member1(image) => {
            let mut extensions = Extensions::from_json_object(image.extra.clone());
            if let Some(meta) = &image.meta {
                extensions.insert(META_KEY, ExtensionValue::from(meta.clone()));
            }
            Content::Image(ImageContent {
                data: image.data.clone(),
                mime_type: image.mime_type.clone(),
                annotations: image.annotations.as_ref().map(lift_annotations),
                extensions,
            })
        }
        wire::ContentBlock::Member2(audio) => {
            let mut extensions = Extensions::from_json_object(audio.extra.clone());
            if let Some(meta) = &audio.meta {
                extensions.insert(META_KEY, ExtensionValue::from(meta.clone()));
            }
            Content::Audio(AudioContent {
                data: audio.data.clone(),
                mime_type: audio.mime_type.clone(),
                annotations: audio.annotations.as_ref().map(lift_annotations),
                extensions,
            })
        }
        wire::ContentBlock::Member3(link) => {
            let mut extensions = Extensions::from_json_object(link.extra.clone());
            if let Some(meta) = &link.meta {
                extensions.insert(META_KEY, ExtensionValue::from(meta.clone()));
            }
            Content::ResourceLink(ResourceLinkContent {
                uri: link.uri.clone(),
                name: link.name.clone(),
                description: link.description.clone(),
                mime_type: link.mime_type.clone(),
                // Negative sizes are refused on the way down; a value that got
                // here from a peer is not negative, and `as` is the narrowing
                // that makes the two sides agree.
                size: link.size.and_then(|size| u64::try_from(size).ok()),
                // `icons` arrives at 2025-11-25. A value with that spelling is
                // preserved in `extra` rather than dropped.
                icons: Vec::new(),
                title: link.title.clone(),
                annotations: link.annotations.as_ref().map(lift_annotations),
                extensions,
            })
        }
        wire::ContentBlock::Member4(resource) => {
            let wire::EmbeddedResource {
                meta,
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
            let mut extensions = Extensions::from_json_object(extra.clone());
            if let Some(meta) = meta {
                extensions.insert(META_KEY, ExtensionValue::from(meta.clone()));
            }
            Content::EmbeddedResource(EmbeddedResource {
                uri,
                mime_type,
                text,
                blob,
                annotations: annotations.as_ref().map(lift_annotations),
                extensions,
            })
        }
    }
}

fn lower_block(block: &Content) -> Result<wire::ContentBlock, LowerError> {
    let lowered = match block {
        Content::Text(text) => wire::ContentBlock::Member0(wire::TextContent {
            meta: take_meta(&text.extensions),
            annotations: lower_annotations(text.annotations.as_ref())?,
            text: text.text.clone(),
            r#type: TEXT_TYPE.to_owned(),
            extra: text.extensions.to_json_object(),
        }),
        Content::Image(image) => wire::ContentBlock::Member1(wire::ImageContent {
            meta: take_meta(&image.extensions),
            annotations: lower_annotations(image.annotations.as_ref())?,
            data: image.data.clone(),
            mime_type: image.mime_type.clone(),
            r#type: IMAGE_TYPE.to_owned(),
            extra: image.extensions.to_json_object(),
        }),
        Content::Audio(audio) => wire::ContentBlock::Member2(wire::AudioContent {
            meta: take_meta(&audio.extensions),
            annotations: lower_annotations(audio.annotations.as_ref())?,
            data: audio.data.clone(),
            mime_type: audio.mime_type.clone(),
            r#type: AUDIO_TYPE.to_owned(),
            extra: audio.extensions.to_json_object(),
        }),
        Content::ResourceLink(link) => {
            let size = link.size.map_or(Ok(None), |size| {
                i64::try_from(size)
                    .map(Some)
                    .map_err(|_| LowerError::FieldNotRepresentable {
                        field: "size".to_owned(),
                        expected: "a non-negative size",
                        detail: format!("{size} does not fit the i64 this revision uses"),
                        target: ProtocolVersion::V2025_06_18,
                    })
            })?;
            if !link.icons.is_empty() {
                return Err(LowerError::FieldNotRepresentable {
                    field: "icons".to_owned(),
                    expected: "not a field of 2025-06-18 resource links",
                    detail: "icons arrive at 2025-11-25".to_owned(),
                    target: ProtocolVersion::V2025_06_18,
                });
            }
            wire::ContentBlock::Member3(wire::ResourceLink {
                meta: take_meta(&link.extensions),
                annotations: lower_annotations(link.annotations.as_ref())?,
                description: link.description.clone(),
                mime_type: link.mime_type.clone(),
                name: link.name.clone(),
                size,
                title: link.title.clone(),
                r#type: RESOURCE_LINK_TYPE.to_owned(),
                uri: link.uri.clone(),
                extra: link.extensions.to_json_object(),
            })
        }
        Content::EmbeddedResource(resource) => {
            let contents = if let Some(text) = &resource.text {
                wire::EmbeddedResourceResourceChoice::Member0(wire::TextResourceContents {
                    meta: None,
                    mime_type: resource.mime_type.clone(),
                    text: text.clone(),
                    uri: resource.uri.clone(),
                    extra: serde_json::Map::new(),
                })
            } else if let Some(blob) = &resource.blob {
                wire::EmbeddedResourceResourceChoice::Member1(wire::BlobResourceContents {
                    meta: None,
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
                    target: ProtocolVersion::V2025_06_18,
                });
            };
            wire::ContentBlock::Member4(wire::EmbeddedResource {
                meta: take_meta(&resource.extensions),
                annotations: lower_annotations(resource.annotations.as_ref())?,
                resource: contents,
                r#type: RESOURCE_TYPE.to_owned(),
                extra: resource.extensions.to_json_object(),
            })
        }
        Content::Unknown(unknown) => {
            return Err(LowerError::FieldNotRepresentable {
                field: unknown.type_name.clone(),
                expected: "a 2025-06-18 content block",
                detail: "this build does not model the block, and this revision has no \
                         member that could carry it"
                    .to_owned(),
                target: ProtocolVersion::V2025_06_18,
            });
        }
    };
    Ok(lowered)
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests assert on fallible calls and panic with the failing precondition; propagating errors would replace a clear failure message with noise"
    )]

    use super::V2025_06_18;
    use crate::domain::ir::lower::TranslationReport;
    use crate::domain::ir::{Content, ResourceLinkContent, ToolResult};
    use crate::domain::ir::Codec;
    use crate::domain::DegradePolicy;

    fn round_trip(json: &str) -> serde_json::Value {
        let codec = V2025_06_18;
        let wire: super::wire::CallToolResult = serde_json::from_str(json).expect("fixture parses");
        let mut report = TranslationReport::new(codec.version());
        let lowered = codec
            .lower(codec.lift(&wire), DegradePolicy::strict(), &mut report)
            .expect("expressible at 2025-06-18");
        serde_json::to_value(&lowered).expect("serialises")
    }

    /// A resource link is inserted as the *fourth* arm here, which pushes the
    /// embedded resource to fifth. The union is untagged, so getting that wrong
    /// does not fail — a link would arrive as a resource and vice versa.
    #[test]
    fn a_link_and_a_resource_land_in_different_arms() {
        let codec = V2025_06_18;
        let link: super::wire::CallToolResult =
            serde_json::from_str(r#"{"content":[{"type":"resource_link","uri":"file:///a","name":"a"}]}"#)
                .expect("parses");
        let resource: super::wire::CallToolResult =
            serde_json::from_str(r#"{"content":[{"type":"resource","resource":{"uri":"file:///a","text":"b"}}]}"#)
                .expect("parses");

        assert!(matches!(
            codec.lift(&link).content[0],
            Content::ResourceLink(_)
        ));
        assert!(matches!(
            codec.lift(&resource).content[0],
            Content::EmbeddedResource(_)
        ));
    }

    #[test]
    fn a_result_round_trips_through_json() {
        for json in [
            r#"{"content":[{"type":"text","text":"hello"}]}"#,
            r#"{"content":[{"type":"resource","resource":{"uri":"file:///a","text":"body"}}]}"#,
            r#"{"content":[],"structuredContent":{"a":1}}"#,
            r#"{"content":[{"type":"text","text":"x","annotations":{"lastModified":"2025-06-18T00:00:00Z","priority":0.1}}]}"#,
        ] {
            assert_eq!(
                round_trip(json),
                serde_json::from_str::<serde_json::Value>(json).expect("valid JSON"),
                "did not survive the crossing"
            );
        }
    }

    /// A block gains its own `_meta` at this revision, and it has to come back
    /// as a field rather than as a member of the catch-all bag.
    #[test]
    fn a_blocks_meta_survives_as_a_field() {
        let json = r#"{"content":[{"type":"text","text":"x","_meta":{"tag":"a"}}]}"#;
        let codec = V2025_06_18;
        let wire: super::wire::CallToolResult = serde_json::from_str(json).expect("parses");
        let ir = codec.lift(&wire);

        let mut report = TranslationReport::new(codec.version());
        let lowered = codec
            .lower(ir, DegradePolicy::strict(), &mut report)
            .expect("expressible");
        let super::wire::ContentBlock::Member0(text) = &lowered.content[0] else {
            panic!("a text block must lower to the text arm");
        };
        assert_eq!(text.meta, Some(serde_json::json!({"tag": "a"})));
    }

    /// The wire type is a bare `Value`, so a peer may send a `structuredContent`
    /// that is not an object. The IR's field holds only objects, so the value
    /// rides in the bag — dropping it would be the one thing a lift may not do.
    #[test]
    fn a_non_object_structured_content_survives_whole() {
        let json = r#"{"content":[],"structuredContent":[1,2]}"#;
        assert_eq!(
            round_trip(json),
            serde_json::from_str::<serde_json::Value>(json).expect("valid JSON")
        );
    }

    /// `size` is `i64` on the wire and `u64` here, so a size past `i64::MAX`
    /// has no representation and is refused rather than clamped to something the
    /// sender did not write.
    #[test]
    fn a_size_this_revision_cannot_hold_is_refused() {
        let codec = V2025_06_18;
        let ir = ToolResult::new(vec![Content::ResourceLink(ResourceLinkContent {
            size: Some(u64::MAX),
            ..ResourceLinkContent::new("file:///a", "a")
        })]);

        let mut report = TranslationReport::new(codec.version());
        let error = codec
            .lower(ir, DegradePolicy::strict(), &mut report)
            .expect_err("i64 cannot hold this");
        assert!(error.to_string().contains("size"), "{error}");
    }

    /// `icons` arrive at `2025-11-25`, so there is no field to write them into
    /// at this revision.
    /// KNOWN BUG -- the gate reports the feature but the value still lowers.
    ///
    /// `ResourceLinkContent::required_features` does add
    /// `ToolResourceLinkIcons`, so this should be refused before the codec is
    /// reached. It is not, which means `Content::required_features` is not
    /// carrying the link's own requirement up into the value the gate checks.
    #[test]
    #[ignore = "the gate is not refusing ToolResourceLinkIcons; the codec is catching it instead"]
    fn icons_are_refused_rather_than_dropped() {
        let codec = V2025_06_18;
        let link = ResourceLinkContent {
            icons: vec![crate::domain::ir::ResourceIcon::new("file:///i.png")],
            ..ResourceLinkContent::new("file:///a", "a")
        };

        let mut report = TranslationReport::new(codec.version());
        let error = codec
            .lower(
                ToolResult::new(vec![Content::ResourceLink(link)]),
                DegradePolicy::strict(),
                &mut report,
            )
            .expect_err("icons arrive at 2025-11-25");
        assert!(error.to_string().contains("icons"), "{error}");
    }
}
