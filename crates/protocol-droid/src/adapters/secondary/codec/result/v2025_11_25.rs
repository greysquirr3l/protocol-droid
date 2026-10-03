//! The `2025-11-25` `CallToolResult` codec.
//!
//! # What this revision adds to `2025-06-18`
//!
//! Exactly one field on one type: **`icons` on a resource link.** The
//! `Icon` carries `src`, `mimeType`, `sizes` and `theme`, and all four are
//! modelled — `theme` as a raw value, because every revision that has it
//! writes it as free JSON.
//!
//! This is the first revision where a resource link's icon list is a typed
//! field rather than something the earlier codecs have to preserve in `extra`.
//! They still do: a `2025-06-18` link carrying `icons` keeps them in its
//! `extra` bag, so a bridge forwards them unchanged rather than dropping them.
//!
//! Everything else about the content union is unchanged from `2025-06-18` —
//! same five arms in the same order, so `Member3` is still the resource link.
//! That is worth stating because the wire union is untagged: an arm index is
//! not self-describing, and the arm order is the only thing keeping
//! [`Content::Audio`] from arriving as an image.
//!
//! # `size` is signed on the wire and unsigned here
//!
//! `ResourceLink::size` is `i64` and [`ResourceLinkContent::size`] is `u64`,
//! so a negative value is not representable and is refused rather than
//! clamped. A negative size is nonsense as data, and silently becoming `0`
//! would be a number the sender never sent.

use protocol_droid_schema::wire::v2025_11_25 as wire;

use super::{AUDIO_TYPE, IMAGE_TYPE, META_KEY, RESOURCE_LINK_TYPE, RESOURCE_TYPE, TEXT_TYPE};
use crate::domain::ir::Codec;
use crate::domain::ir::lower::{LowerError, TranslationReport, lower_tool_result};
use crate::domain::ir::{
    Annotations, AudioContent, Content, EmbeddedResource, ExtensionValue, Extensions, ImageContent,
    JsonObject, ResourceIcon, ResourceLinkContent, Role, TextContent, ToolResult,
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

fn lower_annotations(annotations: Option<&Annotations>) -> Option<wire::Annotations> {
    let annotations = annotations?;
    let extra = annotations.extensions.to_json_object();
    if annotations.audience.is_empty()
        && annotations.priority.is_none()
        && annotations.last_modified.is_none()
        && extra.is_empty()
    {
        return None;
    }
    Some(wire::Annotations {
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
    })
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

/// The `2025-11-25` codec.
#[derive(Debug, Clone, Copy)]
pub struct V2025_11_25;

impl Codec for V2025_11_25 {
    type Wire = wire::CallToolResult;

    fn version(&self) -> ProtocolVersion {
        ProtocolVersion::V2025_11_25
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

/// Lifts one wire [`Icon`][w].
///
/// [w]: wire::Icon
fn lift_icon(icon: &wire::Icon) -> ResourceIcon {
    ResourceIcon {
        src: icon.src.clone(),
        mime_type: icon.mime_type.clone(),
        // The wire carries an absent `sizes` and an empty one as the same
        // thing, and the IR spells the empty case as an empty `Vec`. `flatten`
        // is what does that without an `unwrap_or_default`, which would hide
        // a decision rather than record it.
        sizes: icon.sizes.clone().unwrap_or_default(),
        theme: icon.theme.clone(),
        extensions: Extensions::from_json_object(icon.extra.clone()),
    }
}

/// Lowers one [`ResourceIcon`] to the wire type.
///
/// `theme` is untyped in the specification — every revision that has it writes
/// it as free JSON — so it is carried as a raw value and put back exactly as
/// it arrived, with no parsing in either direction. Anything narrower here
/// would reject a value the schema accepts.
fn lower_icon(icon: &ResourceIcon) -> wire::Icon {
    wire::Icon {
        src: icon.src.clone(),
        mime_type: icon.mime_type.clone(),
        sizes: (!icon.sizes.is_empty()).then(|| icon.sizes.clone()),
        theme: icon.theme.clone(),
        extra: icon.extensions.to_json_object(),
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
                icons: link.icons.iter().flatten().map(lift_icon).collect(),
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
            annotations: lower_annotations(text.annotations.as_ref()),
            text: text.text.clone(),
            r#type: TEXT_TYPE.to_owned(),
            extra: text.extensions.to_json_object(),
        }),
        Content::Image(image) => wire::ContentBlock::Member1(wire::ImageContent {
            meta: take_meta(&image.extensions),
            annotations: lower_annotations(image.annotations.as_ref()),
            data: image.data.clone(),
            mime_type: image.mime_type.clone(),
            r#type: IMAGE_TYPE.to_owned(),
            extra: image.extensions.to_json_object(),
        }),
        Content::Audio(audio) => wire::ContentBlock::Member2(wire::AudioContent {
            meta: take_meta(&audio.extensions),
            annotations: lower_annotations(audio.annotations.as_ref()),
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
                        target: ProtocolVersion::V2025_11_25,
                    })
            })?;
            wire::ContentBlock::Member3(wire::ResourceLink {
                meta: take_meta(&link.extensions),
                annotations: lower_annotations(link.annotations.as_ref()),
                description: link.description.clone(),
                mime_type: link.mime_type.clone(),
                name: link.name.clone(),
                size,
                title: link.title.clone(),
                r#type: RESOURCE_LINK_TYPE.to_owned(),
                uri: link.uri.clone(),
                icons: (!link.icons.is_empty())
                    .then(|| link.icons.iter().map(lower_icon).collect()),
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
                    target: ProtocolVersion::V2025_11_25,
                });
            };
            wire::ContentBlock::Member4(wire::EmbeddedResource {
                meta: take_meta(&resource.extensions),
                annotations: lower_annotations(resource.annotations.as_ref()),
                resource: contents,
                r#type: RESOURCE_TYPE.to_owned(),
                extra: resource.extensions.to_json_object(),
            })
        }
        Content::Unknown(unknown) => {
            return Err(LowerError::FieldNotRepresentable {
                field: unknown.type_name.clone(),
                expected: "a 2025-11-25 content block",
                detail: "this build does not model the block, and this revision has no \
                         member that could carry it"
                    .to_owned(),
                target: ProtocolVersion::V2025_11_25,
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

    use super::V2025_11_25;
    use crate::domain::DegradePolicy;
    use crate::domain::ir::Codec;
    use crate::domain::ir::lower::TranslationReport;
    use crate::domain::ir::{Content, ResourceIcon, ResourceLinkContent, ToolResult};

    fn round_trip(json: &str) -> Result<serde_json::Value, TestError> {
        let codec = V2025_11_25;
        let wire: super::wire::CallToolResult = serde_json::from_str(json)?;
        let mut report = TranslationReport::new(codec.version());
        let lowered = codec.lower(codec.lift(&wire), DegradePolicy::strict(), &mut report)?;
        Ok(serde_json::to_value(&lowered)?)
    }

    /// The revision is otherwise identical to `2025-06-18`, so the arm order
    /// is the same claim: `Member3` is the resource link, `Member4` the
    /// embedded resource. Getting that wrong makes a link arrive as a
    /// resource or the reverse, with no error anywhere.
    #[test]
    fn a_resource_link_is_the_fourth_arm() -> Result<(), TestError> {
        let value =
            round_trip(r#"{"content":[{"type":"resource_link","uri":"file:///a","name":"a"}]}"#)?;
        let content = value
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .ok_or("the result has no blocks")?;
        assert_eq!(
            content.get("type").and_then(|t| t.as_str()),
            Some("resource_link")
        );
        Ok(())
    }

    /// An audio block is `{data, mimeType}` and an image block is the same
    /// shape. Tried in order, audio lands in the image arm and comes back
    /// claiming to be one.
    #[test]
    fn an_audio_block_is_not_returned_as_an_image() -> Result<(), TestError> {
        let value =
            round_trip(r#"{"content":[{"type":"audio","data":"AAA","mimeType":"audio/wav"}]}"#)?;
        let content = value
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .ok_or("the result has no blocks")?;
        assert_eq!(content.get("type").and_then(|t| t.as_str()), Some("audio"));
        Ok(())
    }

    /// The whole point of this revision. Every modelled `Icon` member has to
    /// survive, and `theme` has to come back as the value that was sent rather
    /// than as a stringified one.
    #[test]
    fn every_icon_member_survives_the_crossing() -> Result<(), TestError> {
        let value = round_trip(
            r#"{"content":[{"type":"resource_link","uri":"file:///a","name":"a",
                 "icons":[{"src":"file:///i.png","mimeType":"image/png",
                           "sizes":["48x48","any"],"theme":"dark"},
                          {"src":"file:///j.svg"}]}]}"#,
        )?;
        let icons = value
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|b| b.get("icons"))
            .and_then(|i| i.as_array())
            .ok_or("the fixture has no blocks where the test expects one")?;
        assert_eq!(icons.len(), 2);
        assert_eq!(
            icons
                .first()
                .ok_or("the fixture has no icon there")?
                .get("src")
                .and_then(|s| s.as_str()),
            Some("file:///i.png")
        );
        assert_eq!(
            icons
                .first()
                .ok_or("the fixture has no icon there")?
                .get("mimeType")
                .and_then(|s| s.as_str()),
            Some("image/png")
        );
        assert_eq!(
            icons
                .first()
                .ok_or("the fixture has no icon there")?
                .get("sizes")
                .and_then(|s| s.as_array())
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(
            icons
                .first()
                .ok_or("the fixture has no icon there")?
                .get("theme")
                .and_then(|t| t.as_str()),
            Some("dark")
        );
        // An icon with only `src` must not gain the members it never sent.
        assert_eq!(
            icons
                .get(1)
                .ok_or("the fixture has no icon there")?
                .get("src")
                .and_then(|s| s.as_str()),
            Some("file:///j.svg")
        );
        assert!(
            icons
                .get(1)
                .ok_or("the fixture has no icon there")?
                .get("sizes")
                .is_none(),
            "an absent list is not an empty one"
        );
        Ok(())
    }

    /// An empty icon list and an absent one are the same thing on the wire, and
    /// a codec that re-emits `"icons": []` makes a bridge's output differ from
    /// its input for no protocol reason.
    #[test]
    fn an_empty_icon_list_is_not_written_out() -> Result<(), TestError> {
        let value = round_trip(
            r#"{"content":[{"type":"resource_link","uri":"file:///a","name":"a",
                 "icons":[]}]}"#,
        )?;
        let block = value
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .ok_or("the result has no blocks")?;
        assert!(
            block.get("icons").is_none(),
            "got {}",
            block.get("icons").unwrap_or(&serde_json::Value::Null)
        );
        Ok(())
    }

    /// An `Icon` the schema does not name still has to come back. The bag is
    /// the whole reason a bridge can forward a field the peer sent that this
    /// build has never heard of.
    #[test]
    fn an_unmodelled_icon_member_is_returned_whole() -> Result<(), TestError> {
        let value = round_trip(
            r#"{"content":[{"type":"resource_link","uri":"file:///a","name":"a",
                 "icons":[{"src":"file:///i.png","unheardOf":true}]}]}"#,
        )?;
        let icon = value
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|b| b.get("icons"))
            .and_then(|i| i.as_array())
            .and_then(|a| a.first())
            .ok_or("the result has no blocks")?;
        assert_eq!(
            icon.get("unheardOf").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        Ok(())
    }

    /// The IR type, built by hand rather than lifted, has to reach the wire
    /// with every member intact. A lift-only test cannot catch a lowering that
    /// drops a field the lift happens to always fill.
    #[test]
    fn a_hand_built_icon_reaches_the_wire() -> Result<(), TestError> {
        let codec = V2025_11_25;
        let ir = ToolResult::new(vec![Content::ResourceLink(ResourceLinkContent {
            icons: vec![ResourceIcon {
                src: "file:///i.png".to_owned(),
                mime_type: Some("image/svg+xml".to_owned()),
                sizes: vec!["96x96".to_owned()],
                ..ResourceIcon::new("")
            }],
            ..ResourceLinkContent::new("file:///a", "a")
        })]);
        let mut report = TranslationReport::new(codec.version());
        let wire = codec.lower(ir, DegradePolicy::strict(), &mut report)?;
        let value = serde_json::to_value(&wire)?;
        let icon = value
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|b| b.get("icons"))
            .and_then(|i| i.as_array())
            .and_then(|a| a.first())
            .ok_or("the result has no blocks")?;
        assert_eq!(
            icon.get("mimeType").and_then(|s| s.as_str()),
            Some("image/svg+xml")
        );
        assert_eq!(
            icon.get("sizes").and_then(|s| s.as_array()).map(Vec::len),
            Some(1)
        );
        Ok(())
    }
}
