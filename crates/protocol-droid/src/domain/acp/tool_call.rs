//! `domain::acp::tool_call` — the tool call shapes the
//! `session/update` notification's `tool_call` and
//! `tool_call_update` variants carry.
//!
//! Source of truth:
//! `crates/protocol-droid-schema/spec/acp/v1.25.0/schema.json`
//! under `$defs.ToolCall`, `$defs.ToolCallUpdate`,
//! `$defs.ToolKind`, `$defs.ToolCallStatus`,
//! `$defs.ToolCallContent`, `$defs.ToolCallLocation`,
//! `$defs.Content`, `$defs.Diff`, `$defs.Terminal`, and
//! the related id types.
//!
//! # The create / update split
//!
//! ACP models tool calls as a two-phase protocol: the
//! agent first sends a `ToolCall` (the full shape), and
//! then sends a stream of `ToolCallUpdate` notifications
//! to report progress. The update shape is "all fields
//! except `toolCallId` are optional" — the consumer
//! merges the update onto the existing call by id. The
//! typed form is two distinct structs (rather than a
//! single struct with an `Option<Option<T>>` per field)
//! because the create and update shapes differ in their
//! optionality: a `ToolCall`'s `title` is required; a
//! `ToolCallUpdate`'s `title` is patch-style
//! (`Some(value)` to set, `None` to leave alone,
//! explicit `null` to clear).
//!
//! # The content union
//!
//! A tool call's `content` is a `Vec<ToolCallContent>`.
//! `ToolCallContent` is a discriminated union of three
//! shapes: a content block (the standard text / image /
//! resource block), a file diff, and a reference to a
//! `terminal/create`d terminal the agent embedded.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::content::ContentBlock;

/// A unique identifier for a tool call within a
/// session. The schema's `ToolCallId` is `type: "string"`
/// — opaque to the protocol. The newtype gives the API
/// a self-documenting handle.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ToolCallId(pub String);

impl ToolCallId {
    /// A `ToolCallId` from any string the schema accepts.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl core::fmt::Display for ToolCallId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, formatter)
    }
}

/// A unique identifier for a terminal instance. The
/// schema's `TerminalId` is `type: "string"`; the
/// newtype is a typed handle.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TerminalId(pub String);

impl TerminalId {
    /// A `TerminalId` from any string the schema accepts.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl core::fmt::Display for TerminalId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, formatter)
    }
}

/// Categories of tools the agent can invoke. The
/// schema names ten values; the typed form is the
/// same ten-value space. `Copy` because the wire
/// form is a single string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Reading files or data.
    Read,
    /// Modifying files or content.
    Edit,
    /// Removing files or data.
    Delete,
    /// Moving or renaming files.
    Move,
    /// Searching for information.
    Search,
    /// Running commands or code.
    Execute,
    /// Internal reasoning or planning.
    Think,
    /// Retrieving external data.
    Fetch,
    /// Switching the current session mode.
    SwitchMode,
    /// Other tool types. The default.
    Other,
}

/// Execution status of a tool call. The schema names
/// four values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    /// The tool call hasn't started running yet
    /// because the input is streaming or the call
    /// is awaiting approval.
    Pending,
    /// The tool call is currently running.
    InProgress,
    /// The tool call completed successfully.
    Completed,
    /// The tool call failed with an error.
    Failed,
}

/// A complete tool call. The agent sends this in
/// the `tool_call` variant of `SessionUpdate` when
/// the tool call starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    /// Unique identifier for this tool call within
    /// the session. The agent picks it; the client
    /// uses it to correlate `ToolCallUpdate`s.
    pub tool_call_id: ToolCallId,
    /// Human-readable title describing what the
    /// tool is doing.
    pub title: String,
    /// Programmatic name of the tool being invoked.
    /// Omitting it or sending `null` both mean that
    /// no tool name is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The category of tool. Helps clients choose
    /// appropriate icons and UI treatment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ToolKind>,
    /// Current execution status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ToolCallStatus>,
    /// Content produced by the tool call. The
    /// schema's `x-deserialize-skip-invalid-items`
    /// means a malformed item is silently dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ToolCallContent>>,
    /// File locations affected by this tool call.
    /// Enables "follow-along" features in clients.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<ToolCallLocation>>,
    /// Raw input parameters sent to the tool. The
    /// agent's own shape; the typed form is the
    /// untyped `Value` so the schema's open
    /// `additionalProperties: true` survives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<serde_json::Value>,
    /// Raw output returned by the tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<serde_json::Value>,
}

/// An update to an existing tool call. All fields
/// except `toolCallId` are optional — only changed
/// fields need to be included. The consumer merges
/// the update onto the existing call by id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallUpdate {
    /// The id of the tool call being updated.
    pub tool_call_id: ToolCallId,
    /// Update the tool kind. `Some(None)` means
    /// "clear"; `None` means "leave alone".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Option<ToolKind>>,
    /// Update the execution status. `Some(None)`
    /// means "clear".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Option<ToolCallStatus>>,
    /// Update the human-readable title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<Option<String>>,
    /// Update the programmatic name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<Option<String>>,
    /// Replace the content collection. The
    /// `Option<Vec<ToolCallContent>>` is the
    /// wire-level "replace" semantics: `None`
    /// leaves the old content alone; `Some(vec)`
    /// replaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ToolCallContent>>,
    /// Replace the locations collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<ToolCallLocation>>,
    /// Update the raw input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<serde_json::Value>,
    /// Update the raw output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<serde_json::Value>,
}

/// Content produced by a tool call. A discriminated
/// union of three shapes: a content block, a file
/// diff, and a reference to an embedded terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolCallContent {
    /// A standard content block (text, image, or
    /// embedded resource).
    Content(ContentBlockWrapper),
    /// A file modification shown as a diff.
    Diff(Diff),
    /// A reference to a `terminal/create`d terminal
    /// the agent embedded in the content stream.
    Terminal(TerminalRef),
}

/// A wrapper around a `ContentBlock` for the
/// `content` variant of `ToolCallContent`.
///
/// The schema's `Content` type is a single-key
/// object `{ "content": <ContentBlock> }`; the
/// wrapper keeps the typed form aligned with that
/// shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentBlockWrapper {
    /// The actual content block.
    pub content: ContentBlock,
}

/// A diff representing a file modification. The
/// schema's `Diff` carries the absolute path, the
/// old text (omitted for new files), and the new
/// text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diff {
    /// The absolute file path being modified.
    pub path: String,
    /// The original content. `None` for new files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    /// The new content after modification.
    pub new_text: String,
}

/// A reference to a terminal instance the agent
/// created via `terminal/create` and is embedding
/// in the tool call's content stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRef {
    /// The terminal instance's id. The client uses
    /// this id in `terminal/release` and
    /// `terminal/wait_for_exit`.
    pub terminal_id: TerminalId,
}

/// A file location being accessed or modified by
/// a tool. Enables "follow-along" features that
/// track which files the agent is working with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallLocation {
    /// The absolute file path being accessed or
    /// modified.
    pub path: String,
    /// Optional line number within the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Per-implementation metadata.
    #[serde(default, rename = "_meta", skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The `ToolKind` enum deserialises every wire
    /// value the schema names. The wire form is a
    /// string; the typed form is a `Copy` enum.
    #[test]
    fn tool_kind_string_round_trips() -> Result<(), serde_json::Error> {
        for (variant, wire) in [
            (ToolKind::Read, "read"),
            (ToolKind::Edit, "edit"),
            (ToolKind::Delete, "delete"),
            (ToolKind::Move, "move"),
            (ToolKind::Search, "search"),
            (ToolKind::Execute, "execute"),
            (ToolKind::Think, "think"),
            (ToolKind::Fetch, "fetch"),
            (ToolKind::SwitchMode, "switch_mode"),
            (ToolKind::Other, "other"),
        ] {
            let value = serde_json::to_value(variant)?;
            assert_eq!(value, json!(wire));
            let back: ToolKind = serde_json::from_value(value)?;
            assert_eq!(back, variant);
        }
        Ok(())
    }

    /// The `ToolCallStatus` enum deserialises every
    /// wire value the schema names.
    #[test]
    fn tool_call_status_string_round_trips() -> Result<(), serde_json::Error> {
        for (variant, wire) in [
            (ToolCallStatus::Pending, "pending"),
            (ToolCallStatus::InProgress, "in_progress"),
            (ToolCallStatus::Completed, "completed"),
            (ToolCallStatus::Failed, "failed"),
        ] {
            let value = serde_json::to_value(variant)?;
            assert_eq!(value, json!(wire));
            let back: ToolCallStatus = serde_json::from_value(value)?;
            assert_eq!(back, variant);
        }
        Ok(())
    }

    /// A `ToolCall` with a `read` kind round-trips.
    /// The wire form carries the `toolCallId`, the
    /// `title`, and the optional fields.
    #[test]
    fn a_tool_call_round_trips() -> Result<(), serde_json::Error> {
        let call = ToolCall {
            tool_call_id: ToolCallId::new("call-1"),
            title: "Reading config".to_owned(),
            name: Some("read_file".to_owned()),
            kind: Some(ToolKind::Read),
            status: Some(ToolCallStatus::InProgress),
            content: None,
            locations: Some(vec![ToolCallLocation {
                path: "/etc/hosts".to_owned(),
                line: Some(1),
                meta: BTreeMap::new(),
            }]),
            raw_input: None,
            raw_output: None,
        };
        let value = serde_json::to_value(&call)?;
        assert_eq!(value.get("toolCallId"), Some(&json!("call-1")));
        assert_eq!(value.get("title"), Some(&json!("Reading config")));
        assert_eq!(value.get("kind"), Some(&json!("read")));
        let back: ToolCall = serde_json::from_value(value)?;
        assert_eq!(back, call);
        Ok(())
    }

    /// A `ToolCallUpdate` with a `Some(None)` field
    /// round-trips as an explicit `null` on the
    /// wire. The `Option<Option<T>>` shape is the
    /// typed form's encoding of patch-style
    /// semantics.
    #[test]
    fn a_tool_call_update_round_trips() -> Result<(), serde_json::Error> {
        let update = ToolCallUpdate {
            tool_call_id: ToolCallId::new("call-1"),
            kind: Some(Some(ToolKind::Execute)),
            status: Some(Some(ToolCallStatus::Completed)),
            title: None,
            name: None,
            content: None,
            locations: None,
            raw_input: None,
            raw_output: None,
        };
        let value = serde_json::to_value(&update)?;
        assert_eq!(value.get("toolCallId"), Some(&json!("call-1")));
        assert_eq!(value.get("kind"), Some(&json!("execute")));
        assert_eq!(value.get("status"), Some(&json!("completed")));
        Ok(())
    }

    /// The `ToolCallContent::Diff` variant carries
    /// the `path`, the optional `oldText`, and the
    /// `newText`. The `type` discriminator is `diff`.
    #[test]
    fn a_tool_call_content_diff_round_trips() -> Result<(), serde_json::Error> {
        let content = ToolCallContent::Diff(Diff {
            path: "/tmp/example.txt".to_owned(),
            old_text: Some("old".to_owned()),
            new_text: "new".to_owned(),
        });
        let value = serde_json::to_value(&content)?;
        assert_eq!(value.get("type"), Some(&json!("diff")));
        assert_eq!(value.get("path"), Some(&json!("/tmp/example.txt")));
        assert_eq!(value.get("oldText"), Some(&json!("old")));
        assert_eq!(value.get("newText"), Some(&json!("new")));
        Ok(())
    }

    /// The `ToolCallContent::Terminal` variant
    /// references a `TerminalId`. The wire form is
    /// `{ "type": "terminal", "terminalId": "..." }`.
    #[test]
    fn a_tool_call_content_terminal_round_trips() -> Result<(), serde_json::Error> {
        let content = ToolCallContent::Terminal(TerminalRef {
            terminal_id: TerminalId::new("term-1"),
        });
        let value = serde_json::to_value(&content)?;
        assert_eq!(value.get("type"), Some(&json!("terminal")));
        assert_eq!(value.get("terminalId"), Some(&json!("term-1")));
        Ok(())
    }
}
