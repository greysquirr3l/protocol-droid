//! `Content` — the superset content block every revision can express.
//!
//! # The superset, and what "superset" costs
//!
//! Every revision's `Content` union is a subset of this enum. `Text` and
//! `Image` have been there since `2024-11-05`; `Audio` arrived in `2025-03-26`;
//! `ResourceLink` and `EmbeddedResource` in `2025-06-18`; `Unknown` is not a
//! content type at all, it is the escape hatch.
//!
//! The cost of a superset is that a variant can be present in a value the
//! target revision cannot express. That is not a modelling detail to paper
//! over — it is the whole input to T08's lowering. So every variant reports
//! what it needs through [`Content::required_features`], and lowering asks
//! rather than guessing. A `Content::Audio` is fine in the IR and a
//! `ProtocolError` on the wire to a `2024-11-05` peer, and the difference is a
//! method call, not a version check scattered through the encoder.
//!
//! # `Unknown` is not a shrug
//!
//! [`Content::Unknown`] carries a content block whose `type` this build does
//! not recognise. It exists because the alternative is dropping the block, and
//! a tool result that silently loses a block is worse than one that forwards
//! one the receiver does not understand — the receiver can ignore it, which
//! preserves the information that it was there.
//!
//! Its [`required_features`](Content::required_features) is empty on purpose.
//! An unknown block needs nothing *this build* knows how to express, because
//! the only way to lower it is to forward the original bytes unchanged, and
//! that is expressible at every revision. T08 pairs it with
//! [`Feature::Extensions`] as a *warning* rather than a hard requirement, so a
//! new content type flows through instead of failing the bridge.

use crate::domain::Feature;
use crate::domain::ir::extensions::Extensions;
use crate::domain::ir::features::FeatureSet;
use serde_json::Value as JsonValue;

use crate::domain::ir::json::JsonObject;

/// A content block.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    /// A text block. Every revision, every transport.
    Text(TextContent),

    /// A base64-encoded image. Every revision.
    Image(ImageContent),

    /// Base64-encoded audio. `2025-03-26`.
    Audio(AudioContent),

    /// A link to a resource the client can read. `2025-06-18`.
    ResourceLink(ResourceLinkContent),

    /// The resource's contents inline. Every revision.
    EmbeddedResource(EmbeddedResource),

    /// A content block this build does not recognise.
    ///
    /// Holds the original wire shape so it can be forwarded unchanged. See the
    /// module docs for why this is not a shrug.
    Unknown(UnknownContent),
}

impl Default for Content {
    /// An empty text block, which is the only content every revision can
    /// express and the only one with a meaningful zero value.
    fn default() -> Self {
        Self::Text(TextContent::default())
    }
}

impl TextContent {
    /// What this block needs, over and above being text at all.
    #[must_use]
    pub const fn required_features(&self) -> FeatureSet {
        FeatureSet::empty()
    }
}

impl ImageContent {
    /// What this block needs, over and above being an image at all.
    #[must_use]
    pub const fn required_features(&self) -> FeatureSet {
        FeatureSet::empty()
    }
}

impl AudioContent {
    /// What this block needs, over and above being audio.
    #[must_use]
    pub fn required_features(&self) -> FeatureSet {
        // "Added support for audio data", `2025-03-26`.
        FeatureSet::empty().with(Feature::ToolAudioContent)
    }
}

impl ResourceLinkContent {
    /// What this block needs, over and above being a link.
    #[must_use]
    pub fn required_features(&self) -> FeatureSet {
        let mut required = FeatureSet::empty().with(Feature::ToolResourceLinks);
        if !self.icons.is_empty() {
            required = required.with(Feature::ToolResourceLinkIcons);
        }
        required
    }
}

impl EmbeddedResource {
    /// What this block needs, over and above being an embedded resource.
    #[must_use]
    pub const fn required_features(&self) -> FeatureSet {
        FeatureSet::empty()
    }
}

impl Annotations {
    /// What these annotations need.
    ///
    /// What these annotations need.
    ///
    /// Annotations as a *field* need nothing: `2024-11-05` carries an
    /// untyped `annotations` on every content block, so a block with an
    /// audience and a priority lowers onto it and the value survives as JSON.
    /// That is why this is not `Feature::ToolAnnotations`, which gates the
    /// typed form added in `2025-03-26`.
    ///
    /// `lastModified` is different in kind, not degree. It exists only in the
    /// typed object, so an older revision cannot hold it at all — not as a
    /// field, not even untyped — and dropping it would lose information the
    /// sender put there. So it, and only it, requires `2025-06-18`.
    #[must_use]
    pub fn required_features(&self) -> FeatureSet {
        if self.last_modified.is_some() {
            FeatureSet::empty().with(Feature::ToolAnnotationsLastModified)
        } else {
            FeatureSet::empty()
        }
    }
}

impl Content {
    /// The features this block needs in order to be expressed.
    ///
    /// Empty for `Text`, `Image`, `EmbeddedResource` and `Unknown`, because all
    /// four are expressible at every revision. The rest name the revision that
    /// introduced them, and those are exactly the revisions where lowering this
    /// block to a `2024-11-05` peer is a downgrade.
    #[must_use]
    pub fn required_features(&self) -> FeatureSet {
        let mut required = match self {
            // Text, Image and EmbeddedResource are in `2024-11-05` — but a
            // block of any kind carrying annotations is not, because
            // annotations arrived in `2025-03-26`. The block's own kind and the
            // fields on it are separate questions, and answering only the first
            // one lets an annotated block lower onto a revision that would drop
            // the annotations.
            Self::Text(text) => text.required_features(),
            Self::Image(image) => image.required_features(),
            Self::Audio(audio) => audio.required_features(),
            Self::ResourceLink(link) => link.required_features(),
            Self::EmbeddedResource(resource) => resource.required_features(),
            // Carried verbatim, so it needs nothing — the bytes are the wire.
            Self::Unknown(_) => FeatureSet::empty(),
        };
        if let Some(annotations) = self.annotations() {
            required = required.union(annotations.required_features());
        }
        required
    }

    /// The block's annotations, if it has any.
    #[must_use]
    pub const fn annotations(&self) -> Option<&Annotations> {
        match self {
            Self::Text(block) => block.annotations.as_ref(),
            Self::Image(block) => block.annotations.as_ref(),
            Self::Audio(block) => block.annotations.as_ref(),
            Self::ResourceLink(block) => block.annotations.as_ref(),
            Self::EmbeddedResource(block) => block.annotations.as_ref(),
            Self::Unknown(_) => None,
        }
    }

    /// The wire `type` discriminator, for unknown blocks this is the string
    /// that was on the wire.
    #[must_use]
    pub fn type_name(&self) -> &str {
        match self {
            Self::Text(_) => "text",
            Self::Image(_) => "image",
            Self::Audio(_) => "audio",
            Self::ResourceLink(_) => "resource_link",
            Self::EmbeddedResource(_) => "resource",
            Self::Unknown(unknown) => &unknown.type_name,
        }
    }

    /// A short label for logs and downgrade messages.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Image(_) => "image",
            Self::Audio(_) => "audio",
            Self::ResourceLink(_) => "resourceLink",
            Self::EmbeddedResource(_) => "embeddedResource",
            Self::Unknown(_) => "unknown",
        }
    }
}

/// `{"type": "text", "text": "..."}`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextContent {
    /// The text.
    pub text: String,
    /// Optional annotations, `2025-03-26` (PR #185).
    pub annotations: Option<Annotations>,
    /// Unmodelled fields.
    pub extensions: Extensions,
}

impl TextContent {
    /// A text block with no annotations.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            annotations: None,
            extensions: Extensions::empty(),
        }
    }
}

/// `{"type": "image", "data": "...", "mimeType": "..."}`.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageContent {
    /// Base64 payload.
    pub data: String,
    /// The media type.
    pub mime_type: String,
    /// Optional annotations, `2025-03-26`.
    pub annotations: Option<Annotations>,
    /// Unmodelled fields.
    pub extensions: Extensions,
}

impl ImageContent {
    /// An image block.
    #[must_use]
    pub fn new(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self {
            data: data.into(),
            mime_type: mime_type.into(),
            annotations: None,
            extensions: Extensions::empty(),
        }
    }
}

/// The audio block, `2025-03-26` and later.
///
/// Structurally identical to [`ImageContent`]. It is a separate type rather
/// than a shared `BinaryContent` with a flag, because the feature it requires
/// differs and the IR's job is to make that difference impossible to miss.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioContent {
    /// Base64 payload.
    pub data: String,
    /// The media type.
    pub mime_type: String,
    /// Optional annotations, `2025-03-26`.
    pub annotations: Option<Annotations>,
    /// Unmodelled fields.
    pub extensions: Extensions,
}

impl AudioContent {
    /// An audio block.
    #[must_use]
    pub fn new(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self {
            data: data.into(),
            mime_type: mime_type.into(),
            annotations: None,
            extensions: Extensions::empty(),
        }
    }
}

/// A link to a resource, `2025-06-18` and later (PR #603).
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceLinkContent {
    /// The resource's URI.
    pub uri: String,
    /// The name to show.
    pub name: String,
    /// A human-readable description.
    pub description: Option<String>,
    /// The media type, if known.
    pub mime_type: Option<String>,
    /// The size in bytes, if known.
    pub size: Option<u64>,
    /// Icons for the resource. `2025-11-25`, SEP-973.
    ///
    /// <https://github.com/modelcontextprotocol/modelcontextprotocol/issues/973>
    pub icons: Vec<ResourceIcon>,
    /// A human-readable title for the resource. `2025-06-18`.
    ///
    /// Distinct from `name`: `name` is the identifier a client uses to ask for
    /// the resource, `title` is a label for a human reading the result. The
    /// specification defines both, and a link that carried only `title` across a
    /// crossing would arrive stripped of it.
    pub title: Option<String>,
    /// Optional annotations, `2025-03-26`.
    pub annotations: Option<Annotations>,
    /// Unmodelled fields.
    pub extensions: Extensions,
}

impl ResourceLinkContent {
    /// A resource link with only the required fields.
    #[must_use]
    pub fn new(uri: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            name: name.into(),
            description: None,
            mime_type: None,
            size: None,
            icons: Vec::new(),
            title: None,
            annotations: None,
            extensions: Extensions::empty(),
        }
    }
}

/// An icon for a resource link. `2025-11-25`, SEP-973.
///
/// <https://github.com/modelcontextprotocol/modelcontextprotocol/issues/973>
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceIcon {
    /// The image itself, as a data URI.
    pub src: String,
    /// The media type, if the URI does not imply one.
    pub mime_type: Option<String>,
    /// The sizes this icon is usable at.
    pub sizes: Vec<String>,
    /// How the icon is meant to be themed, when the scheme supports it.
    ///
    /// Untyped in every revision that has it, so it is a `JsonValue` rather
    /// than a modelled shape. It is still a named field, which means a codec
    /// that left it out would lose it — `extra` would not catch it, because
    /// the schema already claimed the name.
    pub theme: Option<JsonValue>,
    /// Unmodelled fields.
    pub extensions: Extensions,
}

impl ResourceIcon {
    /// An icon with only its source.
    #[must_use]
    pub fn new(src: impl Into<String>) -> Self {
        Self {
            src: src.into(),
            mime_type: None,
            sizes: Vec::new(),
            theme: None,
            extensions: Extensions::empty(),
        }
    }
}

/// A resource's contents, inline.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedResource {
    /// The text form, if the resource is text.
    pub text: Option<String>,
    /// The blob form, if the resource is binary.
    pub blob: Option<String>,
    /// The resource's URI.
    pub uri: String,
    /// The media type, if known.
    pub mime_type: Option<String>,
    /// Optional annotations, `2025-03-26`.
    pub annotations: Option<Annotations>,
    /// Unmodelled fields.
    pub extensions: Extensions,
}

impl EmbeddedResource {
    /// A text resource.
    #[must_use]
    pub fn text(uri: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            blob: None,
            uri: uri.into(),
            mime_type: None,
            annotations: None,
            extensions: Extensions::empty(),
        }
    }
}

/// Annotations, added in `2025-03-26` (PR #185).
///
/// Modeled rather than dropped, because they are advisory metadata a client
/// uses to decide how to present a result, and a tool that says it is
/// read-only means something to the caller.
///
/// Not `Eq`, and neither is anything that holds one: `priority` is an `f32`
/// because the specification defines it as a number in `0.0`–`1.0`, and
/// `f32` is not a valid `Eq` type because of `NaN`. The cost is that a content
/// block cannot be a `HashMap` key, which nothing needs — blocks live in a
/// `Vec` inside a result.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Annotations {
    /// The audience this block is meant for.
    pub audience: Vec<Role>,
    /// How important this block is, `0.0`–`1.0`.
    pub priority: Option<f32>,
    /// The last time the resource was modified, ISO 8601.
    pub last_modified: Option<String>,
    /// Unmodelled fields.
    pub extensions: Extensions,
}

impl Annotations {
    /// Annotations with nothing set.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            audience: Vec::new(),
            priority: None,
            last_modified: None,
            extensions: Extensions::empty(),
        }
    }
}

/// Who a content block is meant for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// Meant for the user.
    User,
    /// Meant for the model.
    Assistant,
}

impl Role {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

impl core::str::FromStr for Role {
    type Err = UnknownRole;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            other => Err(UnknownRole {
                role: other.to_owned(),
            }),
        }
    }
}

/// A role this build does not recognise.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown content role {role:?}")]
pub struct UnknownRole {
    /// The role, verbatim.
    pub role: String,
}

/// A content block whose `type` this build does not know.
#[derive(Debug, Clone, PartialEq)]
pub struct UnknownContent {
    /// The `type` string that was on the wire.
    pub type_name: String,
    /// The block exactly as received, so it can be forwarded unchanged.
    pub raw: JsonObject,
    /// Unmodelled fields, kept alongside the raw copy so a codec that can model
    /// them does not have to re-derive them from text.
    pub extensions: Extensions,
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "tests assert on fallible calls and unwrap with the failing precondition in the message"
    )]

    use super::{
        Annotations, AudioContent, Content, EmbeddedResource, ImageContent, ResourceIcon,
        ResourceLinkContent, Role, TextContent, UnknownContent,
    };
    use crate::domain::Feature;
    use crate::domain::ir::extensions::{ExtensionValue, Extensions};

    use crate::domain::ir::json::JsonObject;

    /// One case per variant, because `required_features` is per-variant and
    /// the task asks for exactly this test.
    #[test]
    fn required_features_per_variant() {
        let cases: [(Content, &[Feature]); 6] = [
            (Content::Text(TextContent::new("x")), &[]),
            (Content::Image(ImageContent::new("AAAA", "image/png")), &[]),
            (
                Content::Audio(AudioContent::new("AAAA", "audio/wav")),
                &[Feature::ToolAudioContent],
            ),
            (
                Content::ResourceLink(ResourceLinkContent::new("file:///a", "a")),
                &[Feature::ToolResourceLinks],
            ),
            (
                Content::EmbeddedResource(EmbeddedResource::text("file:///a", "x")),
                &[],
            ),
            (
                Content::Unknown(UnknownContent {
                    type_name: "video".into(),
                    raw: JsonObject::new(r#"{"type":"video"}"#).unwrap(),
                    extensions: Extensions::empty(),
                }),
                &[],
            ),
        ];
        for (content, expected_features) in cases {
            let required = content.required_features();
            let expected_count = u32::try_from(expected_features.len()).unwrap_or(u32::MAX);
            assert_eq!(
                required.len(),
                expected_count,
                "{}: {required:?}",
                content.kind(),
            );
            for feature in expected_features {
                assert!(
                    required.contains(*feature),
                    "{} should need {feature}",
                    content.kind(),
                );
            }
        }
    }

    /// The requirement has to be one the revision that introduced it actually
    /// turns on, or lowering would gate on a feature the session never has.
    #[test]
    fn required_features_are_turned_on_by_the_revision_that_defines_them() {
        for (content, version) in [
            (
                Content::Audio(AudioContent::new("AAAA", "audio/wav")),
                crate::domain::ProtocolVersion::V2025_03_26,
            ),
            (
                Content::ResourceLink(ResourceLinkContent::new("file:///a", "a")),
                crate::domain::ProtocolVersion::V2025_06_18,
            ),
        ] {
            for feature in content.required_features().iter() {
                let caps = crate::domain::CapabilitySet::all_for(version);
                let profile = crate::domain::resolve(crate::domain::Negotiation {
                    version,
                    client_caps: caps,
                    server_caps: caps,
                    transport: crate::domain::TransportKind::Stdio,
                });
                assert!(
                    profile.allows(feature),
                    "{version} defines {feature} but a full profile omits it",
                );
            }
        }
    }

    #[test]
    fn type_names_match_the_wire() {
        assert_eq!(Content::Text(TextContent::new("x")).type_name(), "text");
        assert_eq!(
            Content::Image(ImageContent::new("A", "image/png")).type_name(),
            "image"
        );
        assert_eq!(
            Content::Audio(AudioContent::new("A", "audio/wav")).type_name(),
            "audio"
        );
        assert_eq!(
            Content::ResourceLink(ResourceLinkContent::new("u", "n")).type_name(),
            "resource_link"
        );
        assert_eq!(
            Content::EmbeddedResource(EmbeddedResource::text("u", "t")).type_name(),
            "resource"
        );
        assert_eq!(
            Content::Unknown(UnknownContent {
                type_name: "video".into(),
                raw: JsonObject::new("{}").unwrap(),
                extensions: Extensions::empty(),
            })
            .type_name(),
            "video",
        );
    }

    #[test]
    fn text_defaults_to_an_empty_string() {
        assert_eq!(Content::default(), Content::Text(TextContent::new("")));
    }

    /// Annotations with only `audience` and `priority` do not raise a
    /// requirement.
    ///
    /// This test previously asserted the same equality with a different
    /// reason: that a `2024-11-05` peer "simply ignores" annotations. It does
    /// not ignore them — `2024-11-05`'s `TextContent` has an
    /// `annotations: Option<Value>`, untyped but present. So the value can be
    /// carried, and requiring a feature for it would refuse something the
    /// revision can express. The conclusion was right and the reasoning wrong,
    /// which is worth replacing rather than leaving to mislead the next reader.
    #[test]
    fn plain_annotations_do_not_raise_the_requirement() {
        let plain = Content::Text(TextContent::new("x"));
        let annotated = Content::Text(TextContent {
            annotations: Some(Annotations {
                audience: vec![Role::User, Role::Assistant],
                priority: Some(0.5),
                ..Annotations::empty()
            }),
            ..TextContent::new("x")
        });
        assert_eq!(plain.required_features(), annotated.required_features());
    }

    /// `lastModified` is the one annotation field an older revision cannot
    /// hold at all — not even untyped — so it is the one that gates.
    #[test]
    fn last_modified_raises_the_requirement() {
        let mut annotations = Annotations::empty();
        annotations.audience = vec![Role::User];
        assert!(annotations.required_features().is_empty());

        annotations.last_modified = Some("2025-06-18T00:00:00Z".to_owned());
        assert!(
            annotations
                .required_features()
                .contains(Feature::ToolAnnotationsLastModified)
        );
    }

    /// Icons are `2025-11-25`, so a link carrying them needs a newer revision
    /// than a link without. Before this the IR had nowhere to put them.
    #[test]
    fn icons_raise_a_link_s_requirement() {
        let bare = ResourceLinkContent::new("file:///a", "a");
        assert!(
            !bare
                .required_features()
                .contains(Feature::ToolResourceLinkIcons)
        );

        let with_icons = ResourceLinkContent {
            icons: vec![ResourceIcon::new("data:image/png;base64,AAA=")],
            ..ResourceLinkContent::new("file:///a", "a")
        };
        assert!(
            with_icons
                .required_features()
                .contains(Feature::ToolResourceLinkIcons)
        );
        assert!(
            with_icons
                .required_features()
                .contains(Feature::ToolResourceLinks)
        );
    }

    #[test]
    fn extensions_ride_along_without_affecting_requirements() {
        let content = Content::Text(TextContent {
            extensions: Extensions::empty().with("vendor.x", ExtensionValue::Bool(true)),
            ..TextContent::new("x")
        });
        assert!(content.required_features().is_empty());
    }

    #[test]
    fn roles_round_trip_through_their_wire_spelling() {
        for role in [Role::User, Role::Assistant] {
            assert_eq!(role.as_str().parse::<Role>().unwrap(), role);
        }
        assert!("teapot".parse::<Role>().is_err());
    }

    #[test]
    fn binary_content_builders_do_not_set_annotations() {
        let image = ImageContent::new("AAAA", "image/png");
        assert!(image.annotations.is_none());
        assert_eq!(image.mime_type, "image/png");
        let audio = AudioContent::new("BBBB", "audio/wav");
        assert!(audio.annotations.is_none());
        assert_eq!(audio.data, "BBBB");
    }

    #[test]
    fn resource_link_optional_fields_default_to_none() {
        let link = ResourceLinkContent::new("file:///x", "x");
        assert_eq!(link.uri, "file:///x");
        assert_eq!(link.name, "x");
        assert!(link.description.is_none());
        assert!(link.mime_type.is_none());
        assert!(link.size.is_none());
    }

    #[test]
    fn an_embedded_text_resource_has_no_blob() {
        let resource = EmbeddedResource::text("file:///x", "body");
        assert_eq!(resource.text.as_deref(), Some("body"));
        assert!(resource.blob.is_none());
        assert_eq!(resource.uri, "file:///x");
    }
}
