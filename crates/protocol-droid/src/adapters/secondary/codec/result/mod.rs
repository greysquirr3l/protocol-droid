//! The per-revision `CallToolResult` codecs, one module per revision.
//!
//! # Why one file per revision rather than a macro
//!
//! The revisions differ cumulatively, so the whole spread is one axis:
//!
//! | level | revision | adds |
//! | --- | --- | --- |
//! | 0 | 2024-11-05 | untyped `annotations`; no `meta` on blocks |
//! | 1 | 2025-03-26 | audio; typed `annotations` without `lastModified` |
//! | 2 | 2025-06-18 | resource links, `structuredContent`, `lastModified`, block `meta` |
//! | 3 | 2025-11-25 | `icons` on resource links |
//! | 4 | 2026-07-28, draft | `resultType`, and a *typed* `meta` |
//!
//! A macro over that table was tried and is the wrong tool. The content unions
//! are `#[serde(untagged)]` — the member index is the only thing telling audio
//! from image, and an off-by-one produces no error at all, just a block that
//! arrived as the wrong kind of thing. Generating the arms from a level trades
//! a visible, checkable repetition for an invisible index, and the repetition
//! is what a reader checks against the schema. Each file here is short enough
//! to read against the generated types it wraps.
//!
//! What is shared lives here rather than in each file: a constant duplicated
//! five times is a constant that will be wrong once.
//!
//! # The union is where the risk is
//!
//! serde takes the first member that round-trips and the JSON carries no
//! discriminant, so an image and an audio block — both a base64 string and a
//! media type, each structurally valid as the other — swap silently.
//! `crates/protocol-droid-schema/tests/content_member_order.rs` is the test
//! that catches it, by asserting which arm a real fixture re-serialises into.
//!
//! # Lowering makes no decisions of its own
//!
//! Every codec asks [`lower_tool_result`](crate::domain::ir::lower::lower_tool_result) and reports what came back. Whether
//! a value is expressible is the gate's answer and a downgrade is a
//! `DegradeRule`'s. A codec that formed a second opinion is how two revisions
//! start disagreeing about what they support.

pub mod by_version;
pub mod draft;
pub mod v2024_11_05;
pub mod v2025_03_26;
pub mod v2025_06_18;
pub mod v2025_11_25;
pub mod v2026_07_28;
pub mod verbatim;

pub use by_version::WireToolResults;
pub use verbatim::verbatim;

/// The key `_meta` travels under in the IR's extension bag.
///
/// `CallToolResult` has had `_meta` since the first revision, so whether a
/// value can carry it is never a feature question — only a question of shape.
/// The IR has no `meta` field, so it rides in the bag and comes back out as a
/// field on the way down.
///
/// The wire key is `_meta` while the generated field is `meta`, because
/// `_meta` is not an identifier. A fixture that writes `meta` lands in the
/// catch-all `extra` instead, which is precisely the confusion the underscore
/// exists to prevent.
pub(super) const META_KEY: &str = "_meta";

/// The `type` discriminants, per each revision's schema.
pub(super) const TEXT_TYPE: &str = "text";
pub(super) const IMAGE_TYPE: &str = "image";
pub(super) const AUDIO_TYPE: &str = "audio";
pub(super) const RESOURCE_LINK_TYPE: &str = "resource_link";
pub(super) const RESOURCE_TYPE: &str = "resource";

#[cfg(test)]
mod tests {
    use super::draft::Draft;
    use super::v2026_07_28::V2026_07_28;
    use crate::domain::ProtocolVersion;
    use crate::domain::ir::Codec;

    /// `draft` and `2026-07-28` share a version string, so both codecs report
    /// the same revision.
    ///
    /// Upstream's floating `draft` schema declares
    /// `LATEST_PROTOCOL_VERSION = "2026-07-28"` — the same as the final
    /// revision. The two differ in schema shape and nothing else that a peer
    /// can see. A session that has negotiated a version has no way to tell
    /// which shape it is talking to, so a codec that reported a sixth version
    /// would be reporting a distinction the wire does not carry.
    #[test]
    fn the_draft_shape_is_not_a_version_of_its_own() {
        assert_eq!(Draft.version(), ProtocolVersion::V2026_07_28);
        assert_eq!(V2026_07_28.version(), ProtocolVersion::V2026_07_28);
        // And it is not a sixth entry in the catalogue.
        assert_eq!(ProtocolVersion::KNOWN.len(), 5);
    }
}
