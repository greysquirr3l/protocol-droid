//! The canonical, version-agnostic IR.
//!
//! # What this is for
//!
//! Application code and user handlers see these types. They do not see a
//! revision's wire types, and they never choose a version at compile time. T08
//! adds per-revision codecs that lift the wire into this IR and lower it back
//! out, so the same handler serves every revision and the bridge between them
//! is pure data.
//!
//! # The three properties everything here is built around
//!
//! **Superset.** Every revision's types are a subset of these. Fields a newer
//! revision added are `Option` and absent on older ones, so a `2024-11-05`
//! value and a `2026-07-28` value are both representable, and the difference is
//! visible rather than typed away.
//!
//! **Self-describing requirements.** Every value can report what it needs to be
//! expressed through `required_features()`. Lowering asks that question rather
//! than pattern-matching on a version, so adding a revision does not mean
//! touching every encoder — a new revision turns on the features it defines,
//! and the existing gates do the rest.
//!
//! **Lossless.** Unmodelled fields land in an [`Extensions`] bag instead of
//! being dropped, and an unparseable value lands as `Raw` rather than
//! disappearing. The bridge's `lift(v, lower(v, ir)) == ir` claim is only
//! meaningful because of this, and it is the property the crate exists for.
//!
//! # The layering
//!
//! ```text
//! mod features;    // FeatureSet — what a value needs
//! mod json;        // JsonObject — arbitrary JSON, validated once
//! mod extensions;  // Extensions  — the passthrough bag
//! mod content;     // Content     — content blocks
//! mod message;     // IrMessage   — the envelope
//! mod tool;        // ToolResult  — and the downgrade fallback
//! ```
//!
//! Each file holds one concern, and the dependency direction is downward:
//! `content`, `tool`, `codec` and `lower` use `features`, `json` and
//! `extensions`, and nothing uses `content`. `message` sits above the rest
//! and is the only place they are assembled. `codec` and `lower` are public
//! because the adapter layer implements their traits and a bridge reads the
//! reports those traits produce.

mod codec;
mod content;
mod extensions;
mod features;
mod json;
pub mod lower;
mod message;
mod tool;

mod envelope;

pub use codec::Codec;
pub use content::{
    Annotations, AudioContent, Content, EmbeddedResource, ImageContent, ResourceIcon,
    ResourceLinkContent, Role, TextContent, UnknownContent, UnknownRole,
};
pub use envelope::{EnvelopeError, lift as lift_envelope, lower as lower_envelope};
pub use extensions::{ExtensionValue, Extensions};
pub use features::FeatureSet;
pub use json::{JsonObject, JsonObjectError};
pub use lower::{DegradeRule, Loss, LowerError, Lowering, TranslationReport, TranslationStep};
pub use message::{
    IrError, IrErrorCode, IrMessage, IrNotification, IrParams, IrRequest, IrResult, MethodName,
    MethodNameError,
};
pub use tool::{Downgrade, ToolResult};
