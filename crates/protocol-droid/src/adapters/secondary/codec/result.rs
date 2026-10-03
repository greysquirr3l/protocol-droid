//! The `2024-11-05` `CallToolResult` codec.
//!
//! # Why this one is written out and not generated
//!
//! The other five revisions differ from this one *cumulatively* — each adds to
//! what came before and nothing is taken away — so once the pattern here is
//! right, levels 1 to 4 are a table rather than a design. Writing this one by
//! hand first is deliberate: the variation is small enough to check by eye, and
//! a macro that hides a wrong field is worse than no macro at all.
//!
//! # What is different about `2024-11-05`
//!
//! - `content` is `Vec<CallToolResultContentChoice>`. From `2025-06-18` it is
//!   `Vec<ContentBlock>`. The members are otherwise in the same order.
//! - The union has three arms: text, image, and an embedded resource. Audio
//!   arrives at `2025-03-26` and resource links at `2025-06-18`.
//! - `annotations` is an untyped `serde_json::Value`, not an `Annotations`
//!   struct. It is present, though, which is why carrying annotations needs no
//!   feature at this revision.
//! - Content blocks have no `meta`. `CallToolResult` itself does, and has done
//!   since the first revision.
//!
//! # The member indices
//!
//! The union is untagged: serde takes the first member that round-trips, and
//! the JSON carries no discriminant. An image and an audio block are both a
//! base64 string and a media type and are each structurally valid as the
//! other, so an off-by-one produces no error — just a block that arrived as
//! the wrong kind of thing. `content_member_order.rs` in the schema crate is
//! what catches drift here.

use protocol_droid_schema::wire::v2024_11_05 as wire;

use crate::domain::ir::lower::{LowerError, TranslationReport, lower_tool_result};
use crate::domain::ir::{
    Annotations, Content, EmbeddedResource, ExtensionValue, Extensions, ImageContent, Role,
    TextContent, ToolResult,
};
use crate::domain::ir::Codec;
use crate::domain::{
    CapabilitySet, DegradePolicy, FeatureProfile, Negotiation, ProtocolVersion, TransportKind,
    resolve,
};

/// The key a block's or result's `_meta` travels under.
///
/// The IR has no `meta` field, so it rides in the bag and comes back out on the
/// way down. `MCP-1338` added `meta` to `CallToolResult`; carrying it in the
/// extensions is what lets one IR serve the revisions that have it and the ones
/// that do not.
const META_KEY: &str = "_meta";

/// The key untyped `annotations` travel under at this revision.
///
/// The field is a bare `serde_json::Value` here. Anything this codec can model
/// is lifted into [`Annotations`]; anything it cannot is kept whole under this
/// key, so a value shaped nothing like the spec still survives the crossing.
const RAW_ANNOTATIONS_KEY: &str = "annotations";

/// Lifts a value into typed annotations, keeping every unmodelled member.
///
/// Total, like every lift. A `priority` that is a string, an `audience` that
/// is a number, and a role this build has never heard of all land in
/// [`Annotations::extensions`] rather than failing — a bridge that dropped a
/// message over a surprising annotation would be worse than one that forwarded
/// it unparsed.
///
/// Members that *are* modelled are lifted even alongside unmodelled ones, so a
/// value keeps the parts that are understood as well as the parts that are not.
fn lift_annotations(value: &serde_json::Value) -> Annotations {
    let Some(object) = value.as_object() else {
        return Annotations {
            extensions: Extensions::empty().with(RAW_ANNOTATIONS_KEY, ExtensionValue::from(value.clone())),
            ..Annotations::empty()
        };
    };

    let mut annotations = Annotations::empty();

    if let Some(priority) = object.get("priority").and_then(serde_json::Value::as_f64) {
        annotations.priority = Some(priority);
    }
    if let Some(last_modified) = object.get("lastModified").and_then(serde_json::Value::as_str) {
        annotations.last_modified = Some(last_modified.to_owned());
    }
    if let Some(roles) = object.get("audience").and_then(serde_json::Value::as_array) {
        annotations.audience = roles.iter().filter_map(|role| role.as_str()).filter_map(|role| {
            if role == Role::User.as_str() {
                Some(Role::User)
            } else if role == Role::Assistant.as_str() {
                Some(Role::Assistant)
            } else {
                None
            }
        }).collect();
    }

    for (key, value) in object {
        if !matches!(key.as_str(), "priority" | "lastModified" | "audience") {
            annotations
                .extensions
                .insert(key.clone(), ExtensionValue::from(value.clone()));
        }
    }
    annotations
}

/// Renders typed annotations back into the untyped value this revision wants.
///
/// A modelled field is written in the spec's spelling; everything in
/// [`Annotations::extensions`] is written alongside it, and anything left under
/// the raw key is written last so a value that never modelled in the first
/// place comes back out byte-for-byte.
fn lower_annotations(annotations: Option<&Annotations>) -> Option<serde_json::Value> {
    let Some(annotations) = annotations else {
        return None;
    };

    let mut object = annotations.extensions.to_json_object();

    // A value that never parsed the first time must not be re-rendered from a
    // partial model now, or the unmodelled part would be lost to a shape the
    // codec guessed at.
    if let Some(raw) = object.remove(RAW_ANNOTATIONS_KEY) {
        return Some(raw);
    }

    if !annotations.audience.is_empty() {
        object.insert(
            "audience".to_owned(),
            serde_json::Value::Array(
                annotations
                    .audience
                    .iter()
                    .map(|role| serde_json::Value::String((*role).as_str().to_owned()))
                    .collect(),
            ),
        );
    }
    if let Some(priority) = annotations.priority {
        object.insert(
            "priority".to_owned(),
            serde_json::Number::from_f64(priority)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
        );
    }
    if let Some(last_modified) = &annotations.last_modified {
        object.insert(
            "lastModified".to_owned(),
            serde_json::Value::String(last_modified.clone()),
        );
    }
    if object.is_empty() {
        return None;
    }
    Some(serde_json::Value::Object(object))
}

/// The `type` discriminant every block carries, per the schema.
const TEXT_TYPE: &str = "text";
const IMAGE_TYPE: &str = "image";
const RESOURCE_TYPE: &str = "resource";

/// The `2024-11-05` codec.
#[derive(Debug, Clone, Copy)]
pub struct V2024_11_05;

impl Codec for V2024_11_05 {
    type Wire = wire::CallToolResult;

    fn version(&self) -> ProtocolVersion {
        ProtocolVersion::V2024_11_05
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

        let extra = value.extensions.to_json_object();
        let meta = value
            .extensions
            .get(META_KEY)
            .map(serde_json::Value::from);

        let content = value
            .content
            .iter()
            .map(lower_block)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(wire::CallToolResult {
            meta,
            content,
            is_error: value.is_error.then_some(true),
            extra,
        })
    }
}

/// Lifts one union member.
///
/// Total, so the `else` arm is the interesting one: a block this build does
/// not recognise keeps its JSON whole rather than being dropped, which is what
/// lets a bridge forward a message from a peer newer than it is.
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
        wire::CallToolResultContentChoice::Member2(resource) => {
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

/// Lowers one block.
///
/// Every arm is a member of this revision's union or a refusal. There is no
/// fallback arm, because a fallback is how a codec starts inventing shapes: a
/// block with no member here is reported, not forced into the nearest arm that
/// happens to exist.
///
/// The refusals are not all the same kind. Audio and resource links are already
/// gone by the gate — they require features `2024-11-05` does not have — so
/// reaching one would mean the gate let something through, and saying so is
/// more useful than a silent skip. An [`Content::Unknown`] block is different:
/// it raises no feature requirement, because the IR cannot know which union
/// member would hold it, so this is the one case that arrives here honestly and
/// has to be refused on its own account.
fn lower_block(
    block: &Content,
) -> Result<wire::CallToolResultContentChoice, LowerError> {
    let lowered = match block {
        Content::Text(text) => wire::CallToolResultContentChoice::Member0(wire::TextContent {
            annotations: lower_annotations(text.annotations.as_ref()),
            text: text.text.clone(),
            r#type: TEXT_TYPE.to_owned(),
            extra: text.extensions.to_json_object(),
        }),
        Content::Image(image) => wire::CallToolResultContentChoice::Member1(wire::ImageContent {
            annotations: lower_annotations(image.annotations.as_ref()),
            data: image.data.clone(),
            mime_type: image.mime_type.clone(),
            r#type: IMAGE_TYPE.to_owned(),
            extra: image.extensions.to_json_object(),
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
                // A resource with neither a body nor a blob has nothing to put
                // in the union. Refusing here beats writing a member that
                // re-serialises to something the peer cannot read.
                return Err(LowerError::FieldNotRepresentable {
                    field: "resource".to_owned(),
                    expected: "a text or blob body",
                    detail: "the resource has neither `text` nor `blob`".to_owned(),
                    target: ProtocolVersion::V2024_11_05,
                });
            };
            wire::CallToolResultContentChoice::Member2(wire::EmbeddedResource {
                annotations: lower_annotations(resource.annotations.as_ref()),
                resource: contents,
                r#type: RESOURCE_TYPE.to_owned(),
                extra: resource.extensions.to_json_object(),
            })
        }
        Content::Unknown(unknown) => {
            return Err(LowerError::FieldNotRepresentable {
                field: unknown.type_name.clone(),
                expected: "a 2024-11-05 content block",
                detail: "this build does not model the block, and this revision has no \
                         member that could carry it"
                    .to_owned(),
                target: ProtocolVersion::V2024_11_05,
            });
        }
        Content::Audio(_) | Content::ResourceLink(_) => {
            return Err(LowerError::FieldNotRepresentable {
                field: "content".to_owned(),
                expected: "a 2024-11-05 content block",
                detail: "the gate admitted a block this revision cannot express".to_owned(),
                target: ProtocolVersion::V2024_11_05,
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

    use super::{V2024_11_05, lift_annotations, lower_annotations, wire};
    use crate::domain::ir::lower::{TranslationReport, lower_tool_result};
    use crate::domain::ir::{
        Annotations, Content, EmbeddedResource, ExtensionValue, ImageContent, JsonObject, Role,
        TextContent, ToolResult, UnknownContent,
    };
    use crate::domain::ir::Codec;
    use crate::domain::DegradePolicy;

    /// Lifts, lowers, and returns the message as the peer would receive it.
    ///
    /// Compared as a parsed [`serde_json::Value`] rather than as text, because
    /// member order is insignificant under RFC 8259 and `serde_json::Map` sorts
    /// its keys. A textual comparison would fail on `{"text":..,"type":..}`
    /// against `{"type":..,"text":..}` — the same message — and passing it would
    /// mean asserting an ordering the wire format does not define.
    fn round_trip(json: &str) -> serde_json::Value {
        let codec = V2024_11_05;
        let wire: wire::CallToolResult = serde_json::from_str(json).expect("fixture parses");
        let mut report = TranslationReport::new(codec.version());
        let lowered = codec
            .lower(codec.lift(&wire), DegradePolicy::strict(), &mut report)
            .expect("expressible at 2024-11-05");
        serde_json::to_value(&lowered).expect("serialises")
    }

    /// The parsed form of a fixture, for the expected side.
    fn parsed(json: &str) -> serde_json::Value {
        serde_json::from_str(json).expect("fixture is valid JSON")
    }

    /// The contract: a message the peer sent comes back out the same.
    ///
    /// Compared as JSON rather than as structs, because the struct comparison
    /// would pass on a value that had been through a lossy detour and arrived
    /// with the same fields filled in. JSON is what the peer actually sees.
    #[test]
    fn a_result_round_trips_through_json() {
        for json in [
            r#"{"content":[{"type":"text","text":"hello"}]}"#,
            r#"{"content":[{"type":"text","text":"hi","annotations":{"audience":["user"],"priority":0.5}}]}"#,
            r#"{"content":[{"type":"image","data":"AAA","mimeType":"image/png"}]}"#,
            r#"{"content":[{"type":"resource","resource":{"uri":"file:///a","text":"body"}}]}"#,
            r#"{"content":[{"type":"resource","resource":{"uri":"file:///a","blob":"AAA"}}]}"#,
            r#"{"content":[],"isError":true}"#,
        ] {
            assert_eq!(round_trip(json), parsed(json), "did not survive the crossing");
        }
    }

    /// `meta` has no field in the IR, so it rides in the extensions. The test
    /// is that it comes back out as a *field* rather than as a bag member —
    /// the peer sent `meta`, and `meta` is what it must receive.
    #[test]
    fn meta_survives_as_a_field_rather_than_a_bag_member() {
        // The wire key is `_meta`; the generated field is `meta`, because
        // `_meta` is not an identifier. Writing `meta` in a fixture puts the
        // value in `extra` instead, which is the same mistake the key's
        // existence is meant to prevent.
        let json = r#"{"_meta":{"progress":0.5},"content":[]}"#;
        let codec = V2024_11_05;
        let wire: wire::CallToolResult = serde_json::from_str(json).expect("parses");
        let ir = codec.lift(&wire);
        assert!(ir.extensions.contains(crate::adapters::secondary::codec::result::META_KEY));

        let mut report = TranslationReport::new(codec.version());
        let lowered = codec
            .lower(ir, DegradePolicy::strict(), &mut report)
            .expect("expressible");
        assert_eq!(lowered.meta, Some(serde_json::json!({"progress": 0.5})));
        assert_eq!(serde_json::to_value(&lowered).unwrap(), parsed(json));
    }

    /// Annotations a newer peer sends are unmodelled here, and must come back
    /// whole rather than being re-rendered from the parts this build knows.
    ///
    /// A codec that kept what it recognised and re-serialised would still
    /// produce a plausible-looking object — just missing the member the
    /// sender actually sent, which is the failure this pins.
    #[test]
    fn unmodelled_annotations_are_returned_whole() {
        let json = r#"{"content":[{"type":"text","text":"x","annotations":{"priority":0.5,"unheardOf":true}}]}"#;
        assert_eq!(round_trip(json), parsed(json));
    }

    /// A modelled annotation is lifted into the IR rather than left as text, so
    /// the rest of the system can reason about it.
    #[test]
    fn modelled_annotations_are_typed_in_the_ir() {
        let value = serde_json::json!({"audience": ["user", "assistant"], "priority": 0.25});
        let annotations = lift_annotations(&value);
        assert_eq!(annotations.audience, vec![Role::User, Role::Assistant]);
        assert_eq!(annotations.priority, Some(0.25));
        assert_eq!(lower_annotations(Some(&annotations)), Some(value));
    }

    /// A member this build does not recognise is kept, not dropped, and not
    /// merged into the modelled fields — otherwise a value with both a known
    /// and an unknown member would come back with the unknown one missing.
    #[test]
    fn an_unrecognised_member_is_kept_alongside_the_modelled_ones() {
        let value = serde_json::json!({"priority": 0.5, "unheardOf": true});
        let annotations = lift_annotations(&value);
        assert_eq!(annotations.priority, Some(0.5));
        assert_eq!(
            annotations.extensions.get("unheardOf"),
            Some(&ExtensionValue::Bool(true))
        );
        assert_eq!(lower_annotations(Some(&annotations)), Some(value));
    }

    /// An annotations value that is not an object at all cannot be modelled, so
    /// it is carried whole. Losing it would be the one outcome a lift may not
    /// have.
    #[test]
    fn annotations_that_are_not_an_object_survive_whole() {
        let value = serde_json::json!("just a string");
        let annotations = lift_annotations(&value);
        assert!(annotations.audience.is_empty());
        assert_eq!(lower_annotations(Some(&annotations)), Some(value));
    }

    /// `priority` is `f64` on the wire and in the IR, so a value the peer sent
    /// is the value the peer gets back.
    #[test]
    fn priority_is_not_narrowed_on_the_way_through() {
        let json = r#"{"content":[{"type":"text","text":"x","annotations":{"priority":0.1}}]}"#;
        assert_eq!(round_trip(json), parsed(json));
    }

    /// A block this build does not model cannot be written at a revision with
    /// no member for it, and saying so beats inventing a shape or dropping the
    /// message. The gate admits it — an unknown block raises no requirement,
    /// because the IR cannot know which member would hold it — so this is the
    /// codec refusing on its own account.
    #[test]
    fn an_unmodelled_block_is_refused_rather_than_dropped() {
        let codec = V2024_11_05;
        let mut ir = ToolResult::new(vec![Content::Text(TextContent::new("x"))]);
        ir.content.push(Content::Unknown(UnknownContent {
            type_name: "video".to_owned(),
            raw: JsonObject::new(r#"{"type":"video"}"#.to_owned()).expect("valid JSON object"),
            extensions: crate::domain::ir::Extensions::empty(),
        }));

        let mut report = TranslationReport::new(codec.version());
        let error = codec
            .lower(ir, DegradePolicy::strict(), &mut report)
            .expect_err("no union member for it");
        assert!(
            error.to_string().contains("video"),
            "the error must name the block: {error}"
        );
    }

    /// Audio arrives at `2025-03-26`, so the gate refuses it here before the
    /// codec is ever reached. This is the gate working, and the codec must not
    /// be a second place that decides.
    #[test]
    fn audio_is_refused_by_the_gate_before_the_codec_sees_it() {
        let codec = V2024_11_05;
        let ir = ToolResult::new(vec![Content::Audio(
            crate::domain::ir::AudioContent::new("AAA", "audio/wav"),
        )]);

        let mut report = TranslationReport::new(codec.version());
        let error = codec
            .lower(ir, DegradePolicy::strict(), &mut report)
            .expect_err("audio needs 2025-03-26");

        let gate = lower_tool_result(
            ToolResult::new(vec![Content::Audio(
                crate::domain::ir::AudioContent::new("AAA", "audio/wav"),
            )]),
            &codec.capabilities(),
            codec.version(),
            DegradePolicy::strict(),
        );
        assert!(gate.is_err(), "the gate is what refuses, not the codec");
        assert!(!error.to_string().is_empty());
    }

    /// The IR's own round trip, independent of the wire, so a failure here
    /// points at the IR rather than at the codec.
    #[test]
    fn a_typed_result_survives_lift_and_lower() {
        let codec = V2024_11_05;
        let ir = ToolResult::new(vec![
            Content::Text(TextContent::new("hello")),
            Content::Image(ImageContent::new("AAA", "image/png")),
            Content::EmbeddedResource(EmbeddedResource::text("file:///a", "body")),
        ]);

        let mut report = TranslationReport::new(codec.version());
        let wire = codec
            .lower(ir.clone(), DegradePolicy::strict(), &mut report)
            .expect("expressible");
        assert!(report.is_empty());
        assert_eq!(codec.lift(&wire), ir);
    }

    /// `Annotations::empty()` on the wire is `None`, not `Some({})`. Writing an
    /// empty object where the peer sent nothing would be a change the peer
    /// could see.
    #[test]
    fn absent_annotations_stay_absent() {
        assert_eq!(lower_annotations(None), None);
        assert_eq!(lower_annotations(Some(&Annotations::empty())), None);
    }
}
