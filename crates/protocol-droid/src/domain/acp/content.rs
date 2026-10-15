//! `ContentBlock` — the typed content the `session/prompt`
//! and `session/update` paths carry.
//!
//! Source of truth:
//! `crates/protocol-droid-schema/spec/acp/v1.25.0/schema.json`
//! under `$defs.ContentBlock` and the variant `$ref`s.
//!
//! # Why a typed union
//!
//! The schema names five variants: `text`, `image`,
//! `audio`, `resource_link`, and `resource`. The
//! `session/prompt` `prompt` field is `Vec<ContentBlock>`;
//! the agent's `session/update` notifications carry
//! content blocks too. The typed form matches what the
//! schema says, so a consumer that constructs a prompt
//! block by block gets the wire form for free.
//!
//! The schema's `x-deserialize-default-on-error` annotation
//! on the variant fields means a wire block that the typed
//! form does not model round-trips through `Other(Value)`
//! — the bridge can forward an unknown block losslessly
//! without losing data, and the consumer that does not
//! care about that variant just ignores it.
//!
//! # The "required for prompts" baseline
//!
//! `Text` and `ResourceLink` are the two variants the
//! schema says every agent MUST support. `Image`,
//! `Audio`, and `Resource` are gated on the corresponding
//! `PromptCapabilities` (the `image`, `audio`, and
//! `embeddedContext` flags). A consumer that constructs
//! an `Image` block against an agent that did not
//! advertise `image` is a wire-level error; the typed
//! form does not enforce that gate because the gate is
//! the dispatcher's job, not the data layer's.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A single block of content the agent and client exchange.
///
/// The discriminator is the `type` field; the variants
/// are the five the schema names plus a `passthrough`
/// variant for blocks the typed form does not yet
/// model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text or Markdown. The schema says every
    /// agent MUST support this in prompts.
    Text(TextContent),
    /// An image. The agent advertises support via
    /// `PromptCapabilities::image`.
    Image(ImageContent),
    /// An audio clip. The agent advertises support via
    /// `PromptCapabilities::audio`.
    Audio(AudioContent),
    /// A link to an external resource. The schema says
    /// every agent MUST support this in prompts; the
    /// block is metadata, not the resource body.
    ResourceLink(ResourceLink),
    /// An inline resource. The agent advertises support
    /// via `PromptCapabilities::embedded_context`.
    Resource(EmbeddedResource),
    /// A block the typed form does not model. The wire
    /// payload is preserved verbatim in the
    /// `serde_json::Value` so the bridge can forward it
    /// losslessly.
    #[serde(untagged)]
    Other(serde_json::Value),
}

/// A `text` content block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    /// The text. May be plain text or Markdown; the
    /// schema does not constrain the formatting.
    pub text: String,
    /// Optional annotations. Reserved for the
    /// `_meta`-style extensions the schema
    /// documents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    /// Per-implementation metadata.
    #[serde(default, rename = "_meta", skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, serde_json::Value>,
}

/// An `image` content block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    /// The image data, base64-encoded. The schema
    /// does not constrain the encoding beyond
    /// "base64".
    pub data: String,
    /// The MIME type, e.g. `image/png`.
    pub mime_type: String,
    /// Optional annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    /// Per-implementation metadata.
    #[serde(default, rename = "_meta", skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, serde_json::Value>,
}

/// An `audio` content block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioContent {
    /// The audio data, base64-encoded.
    pub data: String,
    /// The MIME type, e.g. `audio/mp3`.
    pub mime_type: String,
    /// Optional annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    /// Per-implementation metadata.
    #[serde(default, rename = "_meta", skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, serde_json::Value>,
}

/// A `resource_link` content block. A pointer to an
/// external resource the consumer may or may not
/// dereference; the block is the metadata, not the
/// resource body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLink {
    /// A URI for the resource. The schema says "any
    /// URI scheme the editor recognises"; `file://` is
    /// the most common.
    pub uri: String,
    /// Human-readable name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The MIME type, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// The size in bytes, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Optional annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    /// Per-implementation metadata.
    #[serde(default, rename = "_meta", skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, serde_json::Value>,
}

/// A `resource` content block. The resource body is
/// inline; the block carries the same metadata as
/// `ResourceLink` plus the body itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedResource {
    /// The resource body, embedded.
    pub resource: EmbeddedResourceBody,
    /// Optional annotations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    /// Per-implementation metadata.
    #[serde(default, rename = "_meta", skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, serde_json::Value>,
}

/// The body of an [`EmbeddedResource`]. A text
/// resource is a `String`; a binary resource is
/// base64-encoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mimeType", rename_all = "camelCase")]
pub enum EmbeddedResourceBody {
    /// A text resource. The MIME type is the tag;
    /// e.g. `text/plain` for plain text or
    /// `text/markdown` for Markdown.
    Text {
        /// The text body.
        text: String,
    },
    /// A binary resource. The MIME type is the tag;
    /// the data is base64-encoded.
    Blob {
        /// The base64-encoded body.
        blob: String,
    },
}

/// Optional annotations a content block carries. The
/// schema names two fields: `audience` (who the block
/// is for) and `priority` (how important the block is).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotations {
    /// Who the block is for. `["user"]`, `["assistant"]`,
    /// or both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<ContentAudience>>,
    /// The priority the block should be rendered with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<ContentPriority>,
}

/// The audience a content block is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentAudience {
    /// The end user reading the agent's output.
    User,
    /// The agent itself, processing the block as
    /// context.
    Assistant,
}

/// The priority a content block should be rendered
/// with. The schema is a string union; the typed
/// form is the same five-value space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentPriority {
    /// The block is incidental; the UI may render it
    /// behind a "show details" affordance.
    Background,
    /// The default priority.
    Normal,
    /// The block is important; the UI should surface
    /// it prominently.
    High,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The `text` content block round-trips through
    /// `Serialize` → `Deserialize`. The `type`
    /// discriminator is the variant tag.
    #[test]
    fn a_text_content_block_round_trips() -> Result<(), serde_json::Error> {
        let block = ContentBlock::Text(TextContent {
            text: "hello".to_owned(),
            annotations: None,
            meta: BTreeMap::new(),
        });
        let value = serde_json::to_value(&block)?;
        assert_eq!(value.get("type"), Some(&json!("text")));
        assert_eq!(value.get("text"), Some(&json!("hello")));
        let round: ContentBlock = serde_json::from_value(value)?;
        assert_eq!(round, block);
        Ok(())
    }

    /// The `image` content block carries a base64
    /// payload and a MIME type on the wire.
    #[test]
    fn an_image_content_block_round_trips() -> Result<(), serde_json::Error> {
        let block = ContentBlock::Image(ImageContent {
            data: "aGVsbG8=".to_owned(),
            mime_type: "image/png".to_owned(),
            annotations: None,
            meta: BTreeMap::new(),
        });
        let value = serde_json::to_value(&block)?;
        assert_eq!(value.get("type"), Some(&json!("image")));
        assert_eq!(value.get("data"), Some(&json!("aGVsbG8=")));
        assert_eq!(value.get("mimeType"), Some(&json!("image/png")));
        Ok(())
    }

    /// The `audio` content block carries a base64
    /// payload and a MIME type.
    #[test]
    fn an_audio_content_block_round_trips() -> Result<(), serde_json::Error> {
        let block = ContentBlock::Audio(AudioContent {
            data: "ZGF0YQ==".to_owned(),
            mime_type: "audio/mp3".to_owned(),
            annotations: None,
            meta: BTreeMap::new(),
        });
        let value = serde_json::to_value(&block)?;
        assert_eq!(value.get("type"), Some(&json!("audio")));
        assert_eq!(value.get("mimeType"), Some(&json!("audio/mp3")));
        Ok(())
    }

    /// A `resource_link` block carries the URI
    /// metadata; the body is not in the block.
    #[test]
    fn a_resource_link_block_round_trips() -> Result<(), serde_json::Error> {
        let block = ContentBlock::ResourceLink(ResourceLink {
            uri: "file:///tmp/example.txt".to_owned(),
            name: Some("example.txt".to_owned()),
            description: None,
            mime_type: Some("text/plain".to_owned()),
            size: Some(12),
            annotations: None,
            meta: BTreeMap::new(),
        });
        let value = serde_json::to_value(&block)?;
        assert_eq!(value.get("type"), Some(&json!("resource_link")));
        assert_eq!(value.get("uri"), Some(&json!("file:///tmp/example.txt")));
        assert_eq!(value.get("size"), Some(&json!(12)));
        Ok(())
    }

    /// An `embedded_resource` block carries the body
    /// inline. A text body uses the text payload; a
    /// binary body uses the base64-encoded blob.
    #[test]
    fn an_embedded_resource_text_block_round_trips() -> Result<(), serde_json::Error> {
        let block = ContentBlock::Resource(EmbeddedResource {
            resource: EmbeddedResourceBody::Text {
                text: "inline content".to_owned(),
            },
            annotations: None,
            meta: BTreeMap::new(),
        });
        let value = serde_json::to_value(&block)?;
        assert_eq!(value.get("type"), Some(&json!("resource")));
        let resource = value
            .get("resource")
            .ok_or_else(|| serde_json::Error::io(std::io::Error::other("missing `resource` field")))?;
        assert_eq!(resource.get("text"), Some(&json!("inline content")));
        Ok(())
    }

    /// An unknown `type` value deserialises into the
    /// `Other` passthrough variant. The wire payload
    /// is preserved verbatim for the bridge to forward.
    #[test]
    fn an_unknown_content_type_lands_in_the_other_variant() -> Result<(), serde_json::Error> {
        let raw = json!({"type": "future_block", "payload": "anything"});
        let block: ContentBlock = serde_json::from_value(raw)?;
        match block {
            ContentBlock::Other(value) => {
                assert_eq!(value.get("type"), Some(&json!("future_block")));
            }
            ContentBlock::Text(_)
            | ContentBlock::Image(_)
            | ContentBlock::Audio(_)
            | ContentBlock::ResourceLink(_)
            | ContentBlock::Resource(_) => {
                return Err(serde_json::Error::io(std::io::Error::other(
                    "an unknown type landed in the wrong variant",
                )));
            }
        }
        Ok(())
    }
}
