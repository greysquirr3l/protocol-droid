//! A2A `Message` and `Part` value objects.
//!
//! # What these are
//!
//! A [`Message`] is the unit of conversation between
//! an A2A client and an A2A agent. It has a role
//! (the user or the agent), a list of [`Part`]s
//! (the content), and an optional metadata map.
//!
//! A [`Part`] is the unit of content within a
//! message. A2A defines three part kinds:
//!
//! - [`TextPart`](Part::Text) — a UTF-8 string.
//! - [`FilePart`](Part::File) — a file (named,
//!   MIME-typed, with either an inline base64
//!   payload or a URI).
//! - [`DataPart`](Part::Data) — a structured JSON
//!   value.
//!
//! The wire form of a `Part` is the proto's
//! `oneof content`: the JSON object has exactly
//! one of `text`, `file`, or `data` as a top-level
//! field. The optional `metadata` field rides
//! alongside whichever content field is present.
//!
//! # Why an untagged enum, not a tagged one
//!
//! A tagged enum (`{ "kind": "text", "text": "..." }`)
//! is unambiguous but adds a key the spec does
//! not name. An untagged enum
//! (`{ "text": "..." }` for text, `{ "file": {...} }`
//! for file, `{ "data": {...} }` for data) is
//! what the spec writes, and serde's untagged
//! deserialisation is the matching Rust shape.
//! The trade-off is that a malformed part with
//! both `text` and `file` deserialises as the
//! first variant (text) and silently drops the
//! `file` field; the alternative is
//! `#[serde(deny_unknown_fields)]` on each
//! variant, which would reject the malformed
//! case as a deserialisation error. The spec
//! allows the lenient shape (a part with at
//! most one of `text`/`file`/`data`), so the
//! lenient deserialisation is the right wire
//! match.
//!
//! # Why metadata is on each part variant, not on
//! the enum
//!
//! The proto's `Part` message is a wrapper around
//! `oneof content` plus a `metadata` field. The
//! JSON form flattens this — a `Part` is
//! `{ <content-field>, "metadata": {...}? }`,
//! and the metadata field rides alongside
//! whichever content field is present. The Rust
//! shape that round-trips with the wire form
//! is one struct per variant, each with its own
//! `metadata` field; the enum just dispatches
//! on which of `text`/`file`/`data` is present.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A message exchanged between a client and an
/// agent.
///
/// The wire form is `{ "role": "user" | "agent",
/// "parts": [<Part>, ...], "metadata"?: {...} }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    /// The role of the message's author. The
    /// spec fixes the two values; anything else
    /// is a wire error.
    pub role: MessageRole,
    /// The message's content. A message with no
    /// parts is technically valid per the
    /// `OpenAPI 3.x` shape, but it is not
    /// useful — a message with no content is a
    /// no-op on the wire.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
    /// Per-message metadata. The spec leaves the
    /// shape open; the conventional keys are
    /// `"trace_id"`, `"session_id"`, and the
    /// custom names a particular deployment
    /// uses. The map is `BTreeMap` so the wire
    /// form is deterministic (alphabetical by
    /// key), which keeps test fixtures readable.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl Message {
    /// Starts a new message with `role` and no
    /// parts. Parts are added with
    /// [`with_part`](Self::with_part) (or
    /// directly via `parts.push(...)`).
    #[must_use]
    pub const fn new(role: MessageRole) -> Self {
        Self {
            role,
            parts: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    /// Appends a part to the message. The
    /// message is consumed; chain
    /// `Message::new(role).with_part(p).with_part(q)`
    /// to build a multi-part message.
    #[must_use]
    pub fn with_part(mut self, part: impl Into<Part>) -> Self {
        self.parts.push(part.into());
        self
    }
}

/// The author of a [`Message`].
///
/// The two values are the spec's exhaustive
/// list. A message is either from the user
/// (the human or the calling system) or from
/// the agent (the A2A server). There is no
/// "system" role in v1.0.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    /// The message was sent by the user (the
    /// human or the calling system on the
    /// human's behalf).
    #[default]
    User,
    /// The message was sent by the agent (the
    /// A2A server, in response to a previous
    /// user message or proactively).
    Agent,
}

/// A unit of content within a [`Message`].
///
/// The wire form is the proto's `oneof content`:
/// the JSON object has exactly one of
/// `text`, `file`, or `data` as a top-level
/// field. The optional `metadata` field rides
/// alongside whichever content field is
/// present.
///
/// # Examples
///
/// ```
/// use protocol_droid::domain::a2a::message::{Message, MessageRole, Part, TextPart};
///
/// let message = Message::new(MessageRole::User)
///     .with_part(Part::Text(TextPart {
///         text: "Hello, agent.".to_owned(),
///         metadata: std::collections::BTreeMap::new(),
///     }));
/// let json = serde_json::to_string(&message).unwrap();
/// assert!(json.contains(r#""role":"user""#));
/// assert!(json.contains(r#""text":"Hello, agent.""#));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Part {
    /// A UTF-8 string. Wire form:
    /// `{ "text": "..." }`.
    Text(TextPart),
    /// A file (named, MIME-typed, with either an
    /// inline base64 payload or a URI). Wire
    /// form: `{ "file": {...} }`.
    File(FilePart),
    /// A structured JSON value. Wire form:
    /// `{ "data": {...} }`.
    Data(DataPart),
}

impl From<TextPart> for Part {
    fn from(part: TextPart) -> Self {
        Self::Text(part)
    }
}

impl From<FilePart> for Part {
    fn from(part: FilePart) -> Self {
        Self::File(part)
    }
}

impl From<DataPart> for Part {
    fn from(part: DataPart) -> Self {
        Self::Data(part)
    }
}

/// A text part. Wire form:
/// `{ "text": "...", "metadata"?: {...} }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextPart {
    /// The text. The spec says it is a UTF-8
    /// string; the Rust type is `String` so the
    /// value is validated by the JSON parser.
    pub text: String,
    /// Per-part metadata. Same shape and
    /// rationale as [`Message::metadata`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// A file part. Wire form:
/// `{ "file": {...}, "metadata"?: {...} }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePart {
    /// The file's content. Either `bytes` (an
    /// inline base64 string) or `uri` is set;
    /// the spec does not allow both. The Rust
    /// type does not enforce the rule at the
    /// type level — a builder that wants the
    /// guarantee enforces it before
    /// construction.
    pub file: FileContent,
    /// Per-part metadata. Same shape and
    /// rationale as [`Message::metadata`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// The content of a [`FilePart`]. Wire form:
/// `{ "name"?: "...", "mimeType"?: "...",
/// "bytes"?: "base64...", "uri"?: "..." }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContent {
    /// An optional file name. The spec does not
    /// require it; a part that is a
    /// `text/plain` snippet with no name is
    /// valid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// An optional MIME type. The spec
    /// recommends setting it; an agent that
    /// has to guess the type from a URI or a
    /// base64 prefix is doing more work than
    /// the wire form requires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// The file's bytes, base64-encoded. Set
    /// when the file is small enough to fit in
    /// the message; for larger files, set
    /// `uri` instead and let the recipient
    /// fetch it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<String>,
    /// A URI the recipient can fetch the file
    /// from. The URI is the SSRF surface the
    /// A2A SSRF denylist filters on the server
    /// side; a client that sends a URI is
    /// trusting the server to either fetch it
    /// or refuse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

/// A structured data part. Wire form:
/// `{ "data": {...}, "metadata"?: {...} }`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataPart {
    /// The data. The shape is whatever the
    /// sending side and the receiving side
    /// agree on; the spec does not fix it. A
    /// consumer who wants a typed shape
    /// deserialises this into their own type.
    pub data: serde_json::Value,
    /// Per-part metadata. Same shape and
    /// rationale as [`Message::metadata`].
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        DataPart, FileContent, FilePart, Message, MessageRole, Part, TextPart,
    };

    type TestError = Box<dyn std::error::Error>;

    fn metadata_with(key: &str, value: serde_json::Value) -> BTreeMap<String, serde_json::Value> {
        let mut map = BTreeMap::new();
        map.insert(key.to_owned(), value);
        map
    }

    // --- Message wire form -------------------------------------------

    #[test]
    fn message_serialises_with_role_and_parts() -> Result<(), TestError> {
        let message = Message::new(MessageRole::User).with_part(TextPart {
            text: "Hello.".to_owned(),
            metadata: BTreeMap::new(),
        });
        let value = serde_json::to_value(&message)?;
        let obj = value
            .as_object()
            .ok_or("Message must serialise to a JSON object")?;
        assert_eq!(
            obj.get("role").and_then(serde_json::Value::as_str),
            Some("user")
        );
        let parts = obj
            .get("parts")
            .and_then(serde_json::Value::as_array)
            .ok_or("`parts` must be a JSON array")?;
        assert_eq!(parts.len(), 1);
        Ok(())
    }

    #[test]
    fn message_with_no_metadata_omits_the_field() -> Result<(), TestError> {
        // The `metadata` field is `Option`-shaped
        // in the wire form: present only when
        // the message has metadata. A message
        // with no metadata must not serialise
        // `"metadata": {}` on the wire — that
        // would be a parser-level signal that
        // differs from "no metadata" on a
        // strict peer.
        let message = Message::new(MessageRole::Agent);
        let value = serde_json::to_value(&message)?;
        let obj = value
            .as_object()
            .ok_or("Message must serialise to a JSON object")?;
        assert!(!obj.contains_key("metadata"));
        assert!(!obj.contains_key("parts"));
        Ok(())
    }

    #[test]
    fn message_round_trips_with_role_and_metadata() -> Result<(), TestError> {
        let message = Message {
            role: MessageRole::Agent,
            parts: vec![],
            metadata: metadata_with("trace_id", serde_json::json!("abc-123")),
        };
        let json = serde_json::to_string(&message)?;
        let parsed: Message = serde_json::from_str(&json)?;
        assert_eq!(parsed, message);
        Ok(())
    }

    // --- Part wire form ----------------------------------------------

    #[test]
    fn text_part_serialises_with_text_field() -> Result<(), TestError> {
        let part = Part::Text(TextPart {
            text: "Hello.".to_owned(),
            metadata: BTreeMap::new(),
        });
        let value = serde_json::to_value(&part)?;
        let obj = value
            .as_object()
            .ok_or("a Part must serialise to a JSON object")?;
        assert_eq!(
            obj.get("text").and_then(serde_json::Value::as_str),
            Some("Hello.")
        );
        // A text part must NOT have `file` or
        // `data` on the wire.
        assert!(!obj.contains_key("file"));
        assert!(!obj.contains_key("data"));
        Ok(())
    }

    #[test]
    fn file_part_serialises_with_file_field() -> Result<(), TestError> {
        let part = Part::File(FilePart {
            file: FileContent {
                name: Some("hello.txt".to_owned()),
                mime_type: Some("text/plain".to_owned()),
                bytes: Some("SGVsbG8=".to_owned()),
                uri: None,
            },
            metadata: BTreeMap::new(),
        });
        let value = serde_json::to_value(&part)?;
        let obj = value
            .as_object()
            .ok_or("a Part must serialise to a JSON object")?;
        let file = obj
            .get("file")
            .and_then(serde_json::Value::as_object)
            .ok_or("`file` must be a JSON object")?;
        assert_eq!(
            file.get("name").and_then(serde_json::Value::as_str),
            Some("hello.txt")
        );
        assert_eq!(
            file.get("mimeType").and_then(serde_json::Value::as_str),
            Some("text/plain")
        );
        assert_eq!(
            file.get("bytes").and_then(serde_json::Value::as_str),
            Some("SGVsbG8=")
        );
        // The optional `uri` must not appear
        // when it is `None` — same shape as
        // `metadata` above.
        assert!(!file.contains_key("uri"));
        Ok(())
    }

    #[test]
    fn data_part_serialises_with_data_field() -> Result<(), TestError> {
        let part = Part::Data(DataPart {
            data: serde_json::json!({ "answer": 42 }),
            metadata: BTreeMap::new(),
        });
        let value = serde_json::to_value(&part)?;
        let obj = value
            .as_object()
            .ok_or("a Part must serialise to a JSON object")?;
        let data = obj
            .get("data")
            .and_then(serde_json::Value::as_object)
            .ok_or("`data` must be a JSON object")?;
        assert_eq!(
            data.get("answer").and_then(serde_json::Value::as_i64),
            Some(42)
        );
        Ok(())
    }

    #[test]
    fn parts_are_unambiguous_on_deserialise() -> Result<(), TestError> {
        // The wire form is field-presence
        // discrimination. A `{"text": "..."}`
        // object decodes to `Part::Text`, not
        // `Part::File` or `Part::Data`; same
        // for the others.
        let text_json = r#"{"text":"hi"}"#;
        let file_json = r#"{"file":{"name":"x"}}"#;
        let data_json = r#"{"data":{"a":1}}"#;
        let text: Part = serde_json::from_str(text_json)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let file: Part = serde_json::from_str(file_json)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let data: Part = serde_json::from_str(data_json)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        assert!(matches!(text, Part::Text(_)));
        assert!(matches!(file, Part::File(_)));
        assert!(matches!(data, Part::Data(_)));
        Ok(())
    }

    #[test]
    fn text_part_with_metadata_round_trips() -> Result<(), TestError> {
        let part = Part::Text(TextPart {
            text: "Hello.".to_owned(),
            metadata: metadata_with("lang", serde_json::json!("en")),
        });
        let json = serde_json::to_string(&part)?;
        let parsed: Part = serde_json::from_str(&json)?;
        assert_eq!(parsed, part);
        // The metadata is a sibling of `text`
        // on the wire, not nested inside an
        // inner object.
        assert!(json.contains(r#""metadata":{"lang":"en"}"#));
        Ok(())
    }
}
