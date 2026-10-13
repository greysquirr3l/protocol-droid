//! Cross-protocol mapping rules: A2A v1.0 ↔ MCP (any negotiated revision).
//!
//! # What this module is
//!
//! Pure functions that translate between an A2A concept (skill, message) and
//! the corresponding MCP concept (tool, content block). The functions are
//! total where the mapping is 1:1 and partial where it is not — when an
//! A2A concept has no MCP shape, the translation records the loss in a
//! [`CrossProtocolReport`] rather than dropping the value silently.
//!
//! # What this module is *not*
//!
//! It is not the bridge. The bridge (in `application/bridge/`) is the loop
//! that drives these translations against a live peer on each side. This
//! module is the pure half: the *what* of the translation, kept separate
//! from the *when* so the rules can be unit-tested without a transport and
//! the transport can be exercised against a test fixture of rules.
//!
//! # Why the report accumulates rather than summarising
//!
//! A count of "3 rules fired" loses the *which*. Two translations that both
//! dropped a content block can be perfectly fine and a serious bug, and the
//! difference is whether it was a file part the caller agreed to lose or an
//! audio block nobody mentioned. [`CrossProtocolReport`] keeps the steps in
//! the order they were applied and lets the reviewer read what each one was.
//!
//! # Conventions
//!
//! A function whose name is `*_to_*` translates the first protocol's
//! concept into the second's. A function whose name is `*_from_*` is the
//! inverse. A pair of directions is exposed together so the consumer can
//! read both shapes without hunting.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::domain::a2a::agent_card::AgentSkill;
use crate::domain::a2a::message::{DataPart, FilePart, Message, Part, TextPart};
use crate::domain::ir::{
    AudioContent, Content, EmbeddedResource, ExtensionValue, Extensions, ImageContent,
    JsonObject, JsonObjectError, ResourceLinkContent, TextContent,
};
use crate::domain::registry::primitive::{Tool, ToolAnnotations};

/// The two protocols the mapping crosses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Protocol {
    /// A2A v1.0. The `major.minor` shape is a private concern of the A2A
    /// module; this enum is a tag, not a value object.
    A2a,
    /// MCP at the revision a peer negotiated.
    Mcp,
}

impl Protocol {
    /// The other side of the crossing.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::A2a => Self::Mcp,
            Self::Mcp => Self::A2a,
        }
    }
}

/// The two directions a translation can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// A2A → MCP.
    ToMcp,
    /// MCP → A2A.
    ToA2a,
}

/// The reason a step fired.
///
/// Each variant is a statement a reviewer can act on. The trivial case
/// (`PassThrough`) is recorded when the report needs to show *what* was
/// carried so the absence of any other step is meaningful; a translation
/// that did not need to record anything reports an empty
/// [`CrossProtocolReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepKind {
    /// A field round-tripped unchanged.
    PassThrough,
    /// A field was mapped onto a different field that carries the same
    /// information. The report names which side lost its native shape.
    Renamed {
        /// The A2A field the value came from, when the rule is A2A-side.
        from: Option<&'static str>,
        /// The MCP field the value went to, when the rule is MCP-side.
        to: Option<&'static str>,
    },
    /// A field has no peer-side shape and the value was dropped. The
    /// caller will see the step and decide whether the drop is acceptable.
    Dropped {
        /// The A2A field that was dropped, when the rule is A2A-side.
        from: Option<&'static str>,
    },
    /// A field was filled with a placeholder because the source had no
    /// shape to map. The placeholder is documented on the function that
    /// produced it.
    Synthesised {
        /// The MCP field that was filled, when the rule is MCP-side.
        to: Option<&'static str>,
    },
}

/// One step the translation took, and what it cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// Which direction the step went.
    pub direction: Direction,
    /// The A2A concept the step touched, when the step's rule is
    /// A2A-specific. `None` for a step whose rule is protocol-neutral.
    pub source: Option<Protocol>,
    /// What was done, named for the operator reading the report.
    pub note: StepKind,
}

/// What one translation changed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CrossProtocolReport {
    steps: Vec<Step>,
}

impl CrossProtocolReport {
    /// A report with nothing in it yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Records a step.
    pub fn record(&mut self, direction: Direction, source: Option<Protocol>, note: StepKind) {
        self.steps.push(Step {
            direction,
            source,
            note,
        });
    }

    /// Whether anything was recorded. An empty report is a lossless
    /// translation; a non-empty report names the changes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// The number of recorded steps.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.steps.len()
    }

    /// The steps, in the order they were applied.
    #[must_use]
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Merges `other` into this one. Used by the bridge to combine the
    /// report from a forward translation with the report from the reply.
    pub fn merge(&mut self, other: Self) {
        self.steps.extend(other.steps);
    }
}

impl core::fmt::Display for CrossProtocolReport {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.steps.is_empty() {
            return formatter.write_str("cross-protocol translation: no changes");
        }
        for (index, step) in self.steps.iter().enumerate() {
            if index > 0 {
                formatter.write_str("; ")?;
            }
            core::fmt::Display::fmt(step, formatter)?;
        }
        Ok(())
    }
}

impl core::fmt::Display for Step {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let direction = match self.direction {
            Direction::ToMcp => "a2a→mcp",
            Direction::ToA2a => "mcp→a2a",
        };
        let source = self
            .source
            .map_or_else(|| "neutral".to_owned(), |p| format!("{p:?}").to_lowercase());
        write!(formatter, "{direction}@{source}: {}", self.note)
    }
}

impl core::fmt::Display for StepKind {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PassThrough => formatter.write_str("pass-through"),
            Self::Renamed { from, to } => {
                let from = from.unwrap_or("?");
                let to = to.unwrap_or("?");
                write!(formatter, "renamed `{from}` → `{to}`")
            }
            Self::Dropped { from } => {
                let from = from.unwrap_or("?");
                write!(formatter, "dropped `{from}`")
            }
            Self::Synthesised { to } => {
                let to = to.unwrap_or("?");
                write!(formatter, "synthesised `{to}`")
            }
        }
    }
}

/// A2A `AgentSkill` → MCP `Tool`.
///
/// # Mapping
///
/// - `id` → `Tool.name`
/// - `name` → `Tool.title` (an empty name is recorded as a dropped step)
/// - `description` → `Tool.description` (an empty description is recorded)
/// - `tags` → `extensions["tags"]` (MCP has no first-class tag field)
/// - `inputSchema` is **synthesised** as `{"type":"object"}` because A2A
///   skills carry no schema and MCP requires one at every revision. The
///   synthesis is a recorded step.
///
/// # Errors
///
/// [`Tool::new`] parses the synthesised schema and refuses a value that is
/// not a JSON Schema object. The empty-object schema is always parseable, so
/// the only way this function errors is if [`JsonObject::new`] itself fails,
/// which it does not for `{"type":"object"}`; the signature keeps the
/// fallibility explicit so a future synthesised schema is honest about the
/// cost of being parseable.
pub fn a2a_skill_to_mcp_tool(
    skill: &AgentSkill,
    report: &mut CrossProtocolReport,
) -> Result<Tool, JsonObjectError> {
    JsonObject::new(r#"{"type":"object"}"#)?;
    report.record(
        Direction::ToMcp,
        Some(Protocol::A2a),
        StepKind::Synthesised { to: Some("inputSchema") },
    );

    let mut extensions = Extensions::empty();
    if !skill.tags.is_empty() {
        let tags_value = Value::Array(
            skill
                .tags
                .iter()
                .map(|tag| Value::String(tag.to_string()))
                .collect(),
        );
        extensions.insert("tags".to_owned(), ExtensionValue::from(tags_value));
        report.record(
            Direction::ToMcp,
            Some(Protocol::A2a),
            StepKind::Renamed {
                from: Some("tags"),
                to: Some("extensions[\"tags\"]"),
            },
        );
    }

    report.record(
        Direction::ToMcp,
        Some(Protocol::A2a),
        StepKind::Renamed {
            from: Some("id"),
            to: Some("name"),
        },
    );

    let title = if skill.name.as_ref().is_empty() {
        report.record(
            Direction::ToMcp,
            Some(Protocol::A2a),
            StepKind::Dropped { from: Some("name") },
        );
        None
    } else {
        report.record(
            Direction::ToMcp,
            Some(Protocol::A2a),
            StepKind::Renamed {
                from: Some("name"),
                to: Some("title"),
            },
        );
        Some(skill.name.to_string())
    };

    let description = if skill.description.as_ref().is_empty() {
        report.record(
            Direction::ToMcp,
            Some(Protocol::A2a),
            StepKind::Dropped { from: Some("description") },
        );
        None
    } else {
        report.record(
            Direction::ToMcp,
            Some(Protocol::A2a),
            StepKind::PassThrough,
        );
        Some(skill.description.to_string())
    };

    let mut tool = Tool::new(skill.id.as_ref(), r#"{"type":"object"}"#)?;
    tool.title = title;
    tool.description = description;
    tool.annotations = Some(ToolAnnotations::default());
    tool.extensions = extensions;
    Ok(tool)
}

/// A2A `Message` → a list of MCP `Content` blocks.
///
/// The mapping is one-to-one for the common case (`TextPart` → `Text`); the
/// `FilePart` and `DataPart` cases each get a best-effort mapping with a
/// recorded step. A `FilePart` whose MIME type is an image becomes an
/// `ImageContent` block, an audio MIME type becomes `AudioContent`, and a
/// URI becomes a `ResourceLink`. A `FilePart` whose MIME type is neither
/// and whose content is a URI is treated as a `ResourceLink`; a `FilePart`
/// with no URI and a non-image/audio MIME type is dropped with a recorded
/// step. A `DataPart` becomes an `EmbeddedResource` with a JSON MIME type.
pub fn a2a_message_to_mcp_content(
    message: &Message,
    report: &mut CrossProtocolReport,
) -> Vec<Content> {
    let mut out = Vec::with_capacity(message.parts.len());
    for part in &message.parts {
        match part {
            Part::Text(text) => {
                out.push(Content::Text(TextContent::new(text.text.clone())));
                report.record(
                    Direction::ToMcp,
                    Some(Protocol::A2a),
                    StepKind::PassThrough,
                );
            }
            Part::File(file) => {
                if let Some(block) = a2a_file_to_mcp_content(file, report) {
                    out.push(block);
                }
            }
            Part::Data(data) => {
                let value = data.data.clone();
                let block = EmbeddedResource {
                    text: Some(value.to_string()),
                    blob: None,
                    uri: format!("data:application/json,{value}"),
                    mime_type: Some("application/json".to_owned()),
                    annotations: None,
                    extensions: Extensions::empty(),
                };
                out.push(Content::EmbeddedResource(block));
                report.record(
                    Direction::ToMcp,
                    Some(Protocol::A2a),
                    StepKind::Renamed {
                        from: Some("DataPart"),
                        to: Some("EmbeddedResource"),
                    },
                );
            }
        }
    }
    out
}

fn a2a_file_to_mcp_content(
    file: &FilePart,
    report: &mut CrossProtocolReport,
) -> Option<Content> {
    if let Some(uri) = &file.file.uri {
        let mut link = ResourceLinkContent::new(uri.clone(), uri.clone());
        link.mime_type.clone_from(&file.file.mime_type);
        report.record(
            Direction::ToMcp,
            Some(Protocol::A2a),
            StepKind::Renamed {
                from: Some("FilePart.uri"),
                to: Some("ResourceLink"),
            },
        );
        Some(Content::ResourceLink(link))
    } else if let Some(bytes) = &file.file.bytes {
        let mime = file.file.mime_type.as_deref().unwrap_or("application/octet-stream");
        if mime.starts_with("image/") {
            report.record(
                Direction::ToMcp,
                Some(Protocol::A2a),
                StepKind::Renamed {
                    from: Some("FilePart.bytes(image)"),
                    to: Some("ImageContent"),
                },
            );
            Some(Content::Image(ImageContent::new(bytes.clone(), mime.to_owned())))
        } else if mime.starts_with("audio/") {
            report.record(
                Direction::ToMcp,
                Some(Protocol::A2a),
                StepKind::Renamed {
                    from: Some("FilePart.bytes(audio)"),
                    to: Some("AudioContent"),
                },
            );
            Some(Content::Audio(AudioContent::new(bytes.clone(), mime.to_owned())))
        } else {
            report.record(
                Direction::ToMcp,
                Some(Protocol::A2a),
                StepKind::Dropped {
                    from: Some("FilePart.bytes"),
                },
            );
            None
        }
    } else {
        report.record(
            Direction::ToMcp,
            Some(Protocol::A2a),
            StepKind::Dropped { from: Some("FilePart (empty)") },
        );
        None
    }
}

/// A2A `Message` → a single text payload, the join of every `TextPart`.
///
/// A2A messages are multimodal — the `Message` carries any number of
/// `Part` variants — but an MCP `tools/call` arguments payload is a single
/// JSON object. The conservative translation is to join every `TextPart`
/// into one block and drop the rest, recording the loss in `report`. A
/// caller that wants the full translation uses
/// [`a2a_message_to_mcp_content`] and a tool whose `inputSchema` accepts
/// an array of content blocks.
///
/// Returns `None` when the message has no text parts; the bridge treats
/// `None` as a refusal and surfaces a `-32602 Invalid params` to the peer.
pub fn a2a_message_to_mcp_text(
    message: &Message,
    report: &mut CrossProtocolReport,
) -> Option<TextContent> {
    let mut joined = String::new();
    let mut saw_non_text = false;
    for part in &message.parts {
        match part {
            Part::Text(text) => {
                if !joined.is_empty() {
                    joined.push('\n');
                }
                joined.push_str(&text.text);
            }
            Part::File(_) | Part::Data(_) => saw_non_text = true,
        }
    }
    if saw_non_text {
        report.record(
            Direction::ToMcp,
            Some(Protocol::A2a),
            StepKind::Dropped { from: Some("non-text parts") },
        );
    }
    if joined.is_empty() {
        report.record(
            Direction::ToMcp,
            Some(Protocol::A2a),
            StepKind::Dropped { from: Some("text parts") },
        );
        return None;
    }
    Some(TextContent::new(joined))
}

/// MCP `Content` → A2A `Part`.
///
/// The inverse of [`a2a_message_to_mcp_content`]. The mapping is
/// best-effort: every content block has a candidate A2A shape, but the
/// round-trip is lossy when the original A2A `Part` carried information
/// MCP has no slot for (the per-part `name` / `metadata` in a `FilePart`,
/// for example). The steps recorded in `report` are the loss summary.
pub fn mcp_content_to_a2a_part(
    content: &Content,
    report: &mut CrossProtocolReport,
) -> Option<Part> {
    match content {
        Content::Text(text) => {
            report.record(
                Direction::ToA2a,
                Some(Protocol::Mcp),
                StepKind::PassThrough,
            );
            Some(Part::Text(TextPart {
                text: text.text.clone(),
                metadata: BTreeMap::new(),
            }))
        }
        Content::Image(image) => {
            report.record(
                Direction::ToA2a,
                Some(Protocol::Mcp),
                StepKind::Renamed {
                    from: Some("ImageContent"),
                    to: Some("FilePart(bytes, image/*)"),
                },
            );
            Some(Part::File(FilePart {
                file: a2a_bytes_file(image.mime_type.clone(), image.data.clone()),
                metadata: BTreeMap::new(),
            }))
        }
        Content::Audio(audio) => {
            report.record(
                Direction::ToA2a,
                Some(Protocol::Mcp),
                StepKind::Renamed {
                    from: Some("AudioContent"),
                    to: Some("FilePart(bytes, audio/*)"),
                },
            );
            Some(Part::File(FilePart {
                file: a2a_bytes_file(audio.mime_type.clone(), audio.data.clone()),
                metadata: BTreeMap::new(),
            }))
        }
        Content::ResourceLink(link) => {
            report.record(
                Direction::ToA2a,
                Some(Protocol::Mcp),
                StepKind::Renamed {
                    from: Some("ResourceLink"),
                    to: Some("FilePart(uri)"),
                },
            );
            Some(Part::File(FilePart {
                file: a2a_uri_file(link.uri.clone(), link.mime_type.clone()),
                metadata: BTreeMap::new(),
            }))
        }
        Content::EmbeddedResource(resource) => {
            let Ok(value) = serde_json::from_str::<Value>(&resource.uri) else {
                report.record(
                    Direction::ToA2a,
                    Some(Protocol::Mcp),
                    StepKind::Dropped {
                        from: Some("EmbeddedResource.uri"),
                    },
                );
                return None;
            };
            let Value::Object(map) = value else {
                report.record(
                    Direction::ToA2a,
                    Some(Protocol::Mcp),
                    StepKind::Dropped {
                        from: Some("EmbeddedResource.uri (not an object)"),
                    },
                );
                return None;
            };
            report.record(
                Direction::ToA2a,
                Some(Protocol::Mcp),
                StepKind::Renamed {
                    from: Some("EmbeddedResource"),
                    to: Some("DataPart"),
                },
            );
            Some(Part::Data(DataPart {
                data: Value::Object(map),
                metadata: BTreeMap::new(),
            }))
        }
        Content::Unknown(_) => {
            report.record(
                Direction::ToA2a,
                Some(Protocol::Mcp),
                StepKind::Dropped { from: Some("Unknown") },
            );
            None
        }
    }
}

const fn a2a_bytes_file(mime: String, bytes: String) -> crate::domain::a2a::message::FileContent {
    crate::domain::a2a::message::FileContent {
        name: None,
        mime_type: Some(mime),
        bytes: Some(bytes),
        uri: None,
    }
}

const fn a2a_uri_file(uri: String, mime: Option<String>) -> crate::domain::a2a::message::FileContent {
    crate::domain::a2a::message::FileContent {
        name: None,
        mime_type: mime,
        bytes: None,
        uri: Some(uri),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::domain::a2a::message::{FileContent, FilePart, MessageRole};

    /// All the error types the tests below can return. A single enum keeps
    /// `From` impls narrow and the test signatures short.
    enum Failure {
        Skill(crate::domain::ir::JsonObjectError),
        Mismatch(String),
    }

    impl core::fmt::Display for Failure {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Skill(err) => core::fmt::Display::fmt(err, formatter),
                Self::Mismatch(message) => formatter.write_str(message),
            }
        }
    }

    impl core::fmt::Debug for Failure {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            core::fmt::Debug::fmt(&self.mismatch_or_skill(), formatter)
        }
    }

    impl Failure {
        fn mismatch_or_skill(&self) -> String {
            match self {
                Self::Skill(err) => format!("{err}"),
                Self::Mismatch(message) => message.clone(),
            }
        }
    }

    impl From<crate::domain::ir::JsonObjectError> for Failure {
        fn from(err: crate::domain::ir::JsonObjectError) -> Self {
            Self::Skill(err)
        }
    }

    fn skill(id: &str, name: &str, description: &str, tags: &[&str]) -> AgentSkill {
        AgentSkill {
            id: Arc::from(id),
            name: Arc::from(name),
            description: Arc::from(description),
            tags: tags.iter().map(|t| Arc::from(*t)).collect(),
        }
    }

    #[test]
    fn skill_to_tool_maps_required_fields_and_records_steps() -> Result<(), Failure> {
        let s = skill("lookup", "Lookup", "Look up a record", &["search", "read"]);
        let mut report = CrossProtocolReport::new();
        let tool = a2a_skill_to_mcp_tool(&s, &mut report)?;

        if tool.name.as_str() != "lookup" {
            return Err(Failure::Mismatch(format!(
                "expected tool.name 'lookup', got {:?}",
                tool.name
            )));
        }
        if tool.title.as_deref() != Some("Lookup") {
            return Err(Failure::Mismatch(format!(
                "expected tool.title 'Lookup', got {:?}",
                tool.title
            )));
        }
        if tool.description.as_deref() != Some("Look up a record") {
            return Err(Failure::Mismatch(format!(
                "expected tool.description, got {:?}",
                tool.description
            )));
        }
        if report.is_empty() {
            return Err(Failure::Mismatch(
                "expected the steps to be recorded, got an empty report".to_owned(),
            ));
        }
        if tool.extensions.get("tags").is_none() {
            return Err(Failure::Mismatch(
                "expected tags to be recorded in extensions".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn skill_to_tool_with_empty_name_drops_title() -> Result<(), Failure> {
        let s = skill("lookup", "", "Look up a record", &[]);
        let mut report = CrossProtocolReport::new();
        let tool = a2a_skill_to_mcp_tool(&s, &mut report)?;
        if tool.title.is_some() {
            return Err(Failure::Mismatch("expected title to be None".to_owned()));
        }
        if !report.steps().iter().any(|s| {
            matches!(&s.note, StepKind::Dropped { from: Some("name") })
        }) {
            return Err(Failure::Mismatch("expected a Dropped(name) step".to_owned()));
        }
        Ok(())
    }

    #[test]
    fn skill_to_tool_with_empty_description_drops_description() -> Result<(), Failure> {
        let s = skill("lookup", "Lookup", "", &[]);
        let mut report = CrossProtocolReport::new();
        let tool = a2a_skill_to_mcp_tool(&s, &mut report)?;
        if tool.description.is_some() {
            return Err(Failure::Mismatch("expected description to be None".to_owned()));
        }
        if !report.steps().iter().any(|s| {
            matches!(&s.note, StepKind::Dropped { from: Some("description") })
        }) {
            return Err(Failure::Mismatch(
                "expected a Dropped(description) step".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn message_to_content_text_round_trips() -> Result<(), Failure> {
        let m = Message {
            role: MessageRole::User,
            parts: vec![Part::Text(TextPart {
                text: "hello".to_owned(),
                metadata: BTreeMap::new(),
            })],
            metadata: BTreeMap::new(),
        };
        let mut report = CrossProtocolReport::new();
        let blocks = a2a_message_to_mcp_content(&m, &mut report);
        if blocks.len() != 1 {
            return Err(Failure::Mismatch(format!(
                "expected 1 block, got {}",
                blocks.len()
            )));
        }
        let Some(first) = blocks.first() else {
            return Err(Failure::Mismatch(
                "expected at least one content block".to_owned(),
            ));
        };
        let Content::Text(t) = first else {
            return Err(Failure::Mismatch(format!(
                "expected Text, got {first:?}"
            )));
        };
        if t.text != "hello" {
            return Err(Failure::Mismatch(format!(
                "expected 'hello', got {:?}",
                t.text
            )));
        }
        if report.is_empty() {
            return Err(Failure::Mismatch(
                "expected a pass-through step".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn message_to_content_image_file_becomes_image_content() -> Result<(), Failure> {
        let file = FilePart {
            file: FileContent {
                name: None,
                mime_type: Some("image/png".to_owned()),
                bytes: Some("AAAA".to_owned()),
                uri: None,
            },
            metadata: BTreeMap::new(),
        };
        let m = Message {
            role: MessageRole::User,
            parts: vec![Part::File(file)],
            metadata: BTreeMap::new(),
        };
        let mut report = CrossProtocolReport::new();
        let blocks = a2a_message_to_mcp_content(&m, &mut report);
        if !blocks.iter().any(|b| matches!(b, Content::Image(_))) {
            return Err(Failure::Mismatch(
                "expected at least one ImageContent block".to_owned(),
            ));
        }
        if !report.steps().iter().any(|s| {
            matches!(
                &s.note,
                StepKind::Renamed {
                    from: Some(_),
                    to: Some("ImageContent")
                }
            )
        }) {
            return Err(Failure::Mismatch(
                "expected a Renamed → ImageContent step".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn message_to_content_uri_file_becomes_resource_link() -> Result<(), Failure> {
        let file = FilePart {
            file: FileContent {
                name: None,
                mime_type: Some("application/pdf".to_owned()),
                bytes: None,
                uri: Some("https://example.com/x".to_owned()),
            },
            metadata: BTreeMap::new(),
        };
        let m = Message {
            role: MessageRole::User,
            parts: vec![Part::File(file)],
            metadata: BTreeMap::new(),
        };
        let mut report = CrossProtocolReport::new();
        let blocks = a2a_message_to_mcp_content(&m, &mut report);
        if !blocks.iter().any(|b| matches!(b, Content::ResourceLink(_))) {
            return Err(Failure::Mismatch(
                "expected at least one ResourceLink block".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn message_to_content_unknown_file_type_is_dropped() -> Result<(), Failure> {
        let file = FilePart {
            file: FileContent {
                name: None,
                mime_type: Some("application/x-binary".to_owned()),
                bytes: Some("AAAA".to_owned()),
                uri: None,
            },
            metadata: BTreeMap::new(),
        };
        let m = Message {
            role: MessageRole::User,
            parts: vec![Part::File(file)],
            metadata: BTreeMap::new(),
        };
        let mut report = CrossProtocolReport::new();
        let blocks = a2a_message_to_mcp_content(&m, &mut report);
        if !blocks.is_empty() {
            return Err(Failure::Mismatch(format!(
                "expected no blocks, got {}",
                blocks.len()
            )));
        }
        if !report.steps().iter().any(|s| {
            matches!(&s.note, StepKind::Dropped { from: Some("FilePart.bytes") })
        }) {
            return Err(Failure::Mismatch(
                "expected a Dropped(FilePart.bytes) step".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn message_to_content_data_becomes_embedded_resource() -> Result<(), Failure> {
        let mut map = serde_json::Map::new();
        map.insert("k".to_owned(), Value::String("v".to_owned()));
        let m = Message {
            role: MessageRole::User,
            parts: vec![Part::Data(DataPart {
                data: Value::Object(map),
                metadata: BTreeMap::new(),
            })],
            metadata: BTreeMap::new(),
        };
        let mut report = CrossProtocolReport::new();
        let blocks = a2a_message_to_mcp_content(&m, &mut report);
        if !blocks
            .iter()
            .any(|b| matches!(b, Content::EmbeddedResource(_)))
        {
            return Err(Failure::Mismatch(
                "expected an EmbeddedResource block".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn message_to_text_joins_text_parts_and_drops_others() -> Result<(), Failure> {
        let m = Message {
            role: MessageRole::User,
            parts: vec![
                Part::Text(TextPart {
                    text: "first".to_owned(),
                    metadata: BTreeMap::new(),
                }),
                Part::Text(TextPart {
                    text: "second".to_owned(),
                    metadata: BTreeMap::new(),
                }),
                Part::File(FilePart {
                    file: FileContent {
                        name: None,
                        mime_type: None,
                        bytes: None,
                        uri: Some("https://example.com/x".to_owned()),
                    },
                    metadata: BTreeMap::new(),
                }),
            ],
            metadata: BTreeMap::new(),
        };
        let mut report = CrossProtocolReport::new();
        let joined = a2a_message_to_mcp_text(&m, &mut report);
        let Some(joined) = joined else {
            return Err(Failure::Mismatch("expected Some(text)".to_owned()));
        };
        if joined.text != "first\nsecond" {
            return Err(Failure::Mismatch(format!(
                "expected 'first\\nsecond', got {:?}",
                joined.text
            )));
        }
        if !report.steps().iter().any(|s| {
            matches!(&s.note, StepKind::Dropped { from: Some("non-text parts") })
        }) {
            return Err(Failure::Mismatch(
                "expected a Dropped(non-text parts) step".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn message_to_text_with_only_non_text_returns_none() -> Result<(), Failure> {
        let m = Message {
            role: MessageRole::User,
            parts: vec![Part::File(FilePart {
                file: FileContent {
                    name: None,
                    mime_type: None,
                    bytes: None,
                    uri: Some("https://example.com/x".to_owned()),
                },
                metadata: BTreeMap::new(),
            })],
            metadata: BTreeMap::new(),
        };
        let mut report = CrossProtocolReport::new();
        let joined = a2a_message_to_mcp_text(&m, &mut report);
        if joined.is_some() {
            return Err(Failure::Mismatch("expected None".to_owned()));
        }
        Ok(())
    }

    #[test]
    fn content_to_part_text_round_trips() -> Result<(), Failure> {
        let block = Content::Text(TextContent::new("hi"));
        let mut report = CrossProtocolReport::new();
        let part = mcp_content_to_a2a_part(&block, &mut report);
        let Some(Part::Text(text)) = part else {
            return Err(Failure::Mismatch(format!(
                "expected TextPart, got {part:?}"
            )));
        };
        if text.text != "hi" {
            return Err(Failure::Mismatch(format!(
                "expected 'hi', got {:?}",
                text.text
            )));
        }
        Ok(())
    }

    #[test]
    fn content_to_part_image_becomes_file_part() -> Result<(), Failure> {
        let block = Content::Image(ImageContent::new("AAAA".to_owned(), "image/png".to_owned()));
        let mut report = CrossProtocolReport::new();
        let part = mcp_content_to_a2a_part(&block, &mut report);
        let Some(Part::File(f)) = part else {
            return Err(Failure::Mismatch(format!(
                "expected FilePart, got {part:?}"
            )));
        };
        if f.file.mime_type.as_deref() != Some("image/png") {
            return Err(Failure::Mismatch(format!(
                "expected image/png, got {:?}",
                f.file.mime_type
            )));
        }
        if f.file.bytes.is_none() {
            return Err(Failure::Mismatch("expected bytes".to_owned()));
        }
        if f.file.uri.is_some() {
            return Err(Failure::Mismatch("expected no uri".to_owned()));
        }
        Ok(())
    }

    #[test]
    fn content_to_part_resource_link_becomes_uri_file() -> Result<(), Failure> {
        let mut link = ResourceLinkContent::new("https://example.com/x".to_owned(), "x".to_owned());
        link.mime_type = Some("application/pdf".to_owned());
        let block = Content::ResourceLink(link);
        let mut report = CrossProtocolReport::new();
        let part = mcp_content_to_a2a_part(&block, &mut report);
        let Some(Part::File(f)) = part else {
            return Err(Failure::Mismatch(format!(
                "expected FilePart, got {part:?}"
            )));
        };
        if f.file.uri.as_deref() != Some("https://example.com/x") {
            return Err(Failure::Mismatch(format!(
                "expected uri, got {:?}",
                f.file.uri
            )));
        }
        if f.file.bytes.is_some() {
            return Err(Failure::Mismatch("expected no bytes".to_owned()));
        }
        Ok(())
    }

    #[test]
    fn report_merge_combines_steps() -> Result<(), Failure> {
        let mut a = CrossProtocolReport::new();
        a.record(
            Direction::ToMcp,
            Some(Protocol::A2a),
            StepKind::PassThrough,
        );
        let mut b = CrossProtocolReport::new();
        b.record(
            Direction::ToA2a,
            Some(Protocol::Mcp),
            StepKind::Dropped { from: Some("x") },
        );
        a.merge(b);
        if a.len() != 2 {
            return Err(Failure::Mismatch(format!(
                "expected 2 steps after merge, got {}",
                a.len()
            )));
        }
        Ok(())
    }

    #[test]
    fn report_display_shows_steps() -> Result<(), Failure> {
        let mut report = CrossProtocolReport::new();
        report.record(
            Direction::ToMcp,
            Some(Protocol::A2a),
            StepKind::Dropped { from: Some("name") },
        );
        let s = report.to_string();
        if !s.contains("a2a→mcp") {
            return Err(Failure::Mismatch(
                "expected 'a2a→mcp' in the report".to_owned(),
            ));
        }
        if !s.contains("dropped") {
            return Err(Failure::Mismatch(
                "expected 'dropped' in the report".to_owned(),
            ));
        }
        Ok(())
    }

    #[test]
    fn protocol_other_swaps() -> Result<(), Failure> {
        if Protocol::A2a.other() != Protocol::Mcp {
            return Err(Failure::Mismatch("A2a.other() should be Mcp".to_owned()));
        }
        if Protocol::Mcp.other() != Protocol::A2a {
            return Err(Failure::Mismatch("Mcp.other() should be A2a".to_owned()));
        }
        Ok(())
    }
}
