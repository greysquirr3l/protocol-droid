//! `domain::acp::notification` — the `session/update`
//! notification the agent streams to the client while
//! processing a `session/prompt` turn.
//!
//! Source of truth:
//! `crates/protocol-droid-schema/spec/acp/v1.25.0/schema.json`
//! under `$defs.SessionNotification`, `$defs.SessionUpdate`,
//! and the twelve variant types the union names.
//!
//! # What this module ships
//!
//! The top-level `SessionNotification` and the
//! `SessionUpdate` discriminated union over the
//! twelve variants the v1.25.0 schema names:
//!
//!   - `user_message_chunk`, `agent_message_chunk`,
//!     `agent_thought_chunk` — `ContentChunk` shapes
//!     for streaming text
//!   - `tool_call`, `tool_call_update` —
//!     [`ToolCall`][super::tool_call::ToolCall] and
//!     [`ToolCallUpdate`][super::tool_call::ToolCallUpdate]
//!   - `plan` — the agent's execution plan
//!   - `available_commands_update` — slash commands
//!   - `current_mode_update` — mode change
//!   - `config_option_update` — config change
//!   - `session_info_update` — title / timestamp
//!   - `usage_update` — context window and cost
//!   - `notice` — a one-off user notice
//!
//! # Why a 13-variant enum
//!
//! The schema's `SessionUpdate` is a 13-arm `oneOf`
//! where the discriminator is the `sessionUpdate` field
//! (a string constant per variant). The typed form is
//! a `Copy`-shaped enum that names the variant on the
//! type, not on the wire; the `#[serde(tag = "sessionUpdate")]`
//! attribute maps the variant to the wire form.
//!
//! # Why a `ContentChunk` wrapper
//!
//! The schema's `ContentChunk` is `{ content, messageId, _meta }`.
//! The `messageId` is a `MessageId` — a string that
//! groups consecutive chunks of the same message. A
//! change in `messageId` means a new message has
//! started. The typed form mirrors that shape; the
//! consumer's update handler threads the `messageId`
//! to whatever surface the consumer renders on.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::content::ContentBlock;
use super::session::{SessionConfigOption, SessionId, SessionModeId};
use super::tool_call::{ToolCall, ToolCallUpdate};

/// Notification containing a session update from the
/// agent. The agent streams these during
/// `session/prompt` processing.
///
/// `PartialEq` only — see [`SessionUpdate`] for the
/// reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotification {
    /// The id of the session this update pertains to.
    pub session_id: SessionId,
    /// The actual update content.
    pub update: SessionUpdate,
}

/// A streamed item of content. The three message-
/// chunk variants of `SessionUpdate` all use this
/// shape; the `messageId` groups consecutive
/// chunks of the same message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentChunk {
    /// A single item of content.
    pub content: ContentBlock,
    /// A unique identifier for the message this
    /// chunk belongs to. All chunks of the same
    /// message share the same `messageId`; a change
    /// in `messageId` indicates a new message has
    /// started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    /// Per-implementation metadata.
    #[serde(default, rename = "_meta", skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: BTreeMap<String, serde_json::Value>,
}

/// A unique identifier for a message within a
/// session. The schema's `MessageId` is `type: "string"`
/// — opaque to the protocol. The newtype makes the
/// API self-documenting.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageId(pub String);

impl MessageId {
    /// A `MessageId` from any string the schema accepts.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl core::fmt::Display for MessageId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, formatter)
    }
}

/// The agent's execution plan for the turn. A
/// `Vec<PlanEntry>` of tasks the agent intends to
/// run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// The ordered list of plan entries. The agent
    /// can re-send the plan to add or remove
    /// entries; the consumer treats the array as
    /// the agent's current intent.
    pub entries: Vec<PlanEntry>,
}

/// A single entry in the agent's [`Plan`]. Each
/// entry is a task the agent intends to run, with
/// a priority and a status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntry {
    /// The content of the task. The schema does
    /// not constrain the format; the consumer
    /// renders it as-is.
    pub content: String,
    /// The relative importance of this task. Used
    /// to indicate which tasks are most critical
    /// to the overall goal.
    pub priority: PlanEntryPriority,
    /// The current execution status.
    pub status: PlanEntryStatus,
}

/// Priority levels for plan entries. The schema
/// names three values; the typed form is the same
/// three-value space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlanEntryPriority {
    /// High priority task — critical to the
    /// overall goal.
    High,
    /// Medium priority task — important but not
    /// critical.
    Medium,
    /// Low priority task — nice to have but not
    /// essential.
    Low,
}

/// Status of a plan entry in the execution flow.
/// The schema names four values; the typed form is
/// the same four-value space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanEntryStatus {
    /// The task has not started.
    Pending,
    /// The task is currently running.
    InProgress,
    /// The task completed successfully.
    Completed,
    /// The task failed.
    Failed,
}

/// `available_commands_update` payload. The set of
/// slash-commands the agent makes available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableCommandsUpdate {
    /// The list of available commands.
    pub available_commands: Vec<AvailableCommand>,
}

/// A slash-command the agent makes available. The
/// consumer surfaces the command in a UI; when the
/// user invokes the command, the consumer sends
/// the command's input as a `session/prompt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableCommand {
    /// Command name (e.g., `create_plan`,
    /// `research_codebase`).
    pub name: String,
    /// Human-readable description of what the
    /// command does.
    pub description: String,
    /// Input for the command if required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<AvailableCommandInput>,
}

/// The input specification for an
/// [`AvailableCommand`].
///
/// The schema names one variant: unstructured
/// text. A future revision may add a
/// structured-input variant; the typed form's
/// `Other(Value)` passthrough preserves unknown
/// variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AvailableCommandInput {
    /// All text typed after the command name is
    /// the input. The most common form.
    Unstructured(UnstructuredCommandInput),
    /// A future or unknown input shape. The wire
    /// payload is preserved verbatim for the
    /// bridge to forward.
    #[serde(untagged)]
    Other(serde_json::Value),
}

/// An unstructured input. The user types text
/// after the command name; the entire string is
/// the input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnstructuredCommandInput {
    /// A hint to display when the input hasn't
    /// been provided yet (e.g. "Search for…" on a
    /// `/search` command).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// `current_mode_update` payload. The current mode
/// of the session has changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentModeUpdate {
    /// The id of the current mode. The id is one
    /// of the modes the agent advertised in the
    /// `SessionModeState::available_modes` from
    /// `session/new` or `session/load`.
    pub current_mode_id: SessionModeId,
}

/// `config_option_update` payload. The set of
/// session configuration options has been updated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigOptionUpdate {
    /// The full set of configuration options and
    /// their current values.
    pub config_options: Vec<SessionConfigOption>,
}

/// `session_info_update` payload. Session metadata
/// has been updated (title, timestamps). All
/// fields are optional to support partial updates.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoUpdate {
    /// Updated human-readable title. Absent means
    /// the title is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Updated ISO 8601 timestamp of last
    /// activity. Absent means the timestamp is
    /// unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// `usage_update` payload. Context window and
/// cumulative cost update for a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageUpdate {
    /// Tokens currently in context.
    pub used: u64,
    /// Total context window size in tokens.
    pub size: u64,
    /// Cumulative session cost. Absent when the
    /// agent does not track cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Cost>,
}

/// Cost information for a session. The schema's
/// `Cost` is `{ amount, currency }`; the typed
/// form is the same shape.
///
/// `PartialEq` only — `f64` does not implement
/// `Eq` because of `NaN`, so the type opts out of
/// the stronger equality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cost {
    /// Total cumulative cost for the session.
    /// The schema's `format: "double"`; the typed
    /// form is `f64`.
    pub amount: f64,
    /// ISO 4217 currency code (e.g., "USD",
    /// "EUR").
    pub currency: String,
}

/// `notice` payload. Information for the user
/// that is not part of session history. The
/// schema says agents MUST only send this when
/// the client advertised
/// `ClientSessionCapabilities::notices`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    /// Severity hint for the notice.
    pub level: NoticeSeverity,
    /// Human-readable text.
    pub text: String,
}

/// Severity hint for a [`Notice`]. The schema
/// names three concrete values plus an `other`
/// escape hatch for future severities.
///
/// The wire form is always a string. The
/// `known / unknown` split is encoded by hand:
/// a custom `Deserialize` maps the three named
/// values to their variants and preserves any
/// other string as [`NoticeSeverity::Other`].
/// A future revision that adds a fourth named
/// value gets a new variant here and a
/// `severity_string_to_variant` match arm.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NoticeSeverity {
    /// Informational notice.
    Info,
    /// Warning notice.
    Warning,
    /// Error notice.
    Error,
    /// A custom or future severity. The wire
    /// form is a string; the typed form
    /// preserves it for the bridge to forward.
    Other(String),
}

impl Serialize for NoticeSeverity {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value = match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Other(name) => name,
        };
        serializer.serialize_str(value)
    }
}

impl<'de> Deserialize<'de> for NoticeSeverity {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Ok(match raw.as_str() {
            "info" => Self::Info,
            "warning" => Self::Warning,
            "error" => Self::Error,
            _ => Self::Other(raw),
        })
    }
}

/// Different types of updates the agent can send
/// during session processing. A 13-arm
/// discriminated union keyed on the wire
/// `sessionUpdate` field.
///
/// `PartialEq` only — the `UsageUpdate` variant
/// carries an `f64` (the cost amount), and `f64`
/// does not implement `Eq` because of `NaN`. The
/// `usage_update` payload is the only variant that
/// pays the price, but dropping `Eq` from the
/// whole enum is the simpler choice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum SessionUpdate {
    /// A chunk of the user's message being
    /// streamed.
    UserMessageChunk(ContentChunk),
    /// A chunk of the agent's response being
    /// streamed.
    AgentMessageChunk(ContentChunk),
    /// A chunk of the agent's internal
    /// reasoning being streamed.
    AgentThoughtChunk(ContentChunk),
    /// Notification that a new tool call has
    /// been initiated.
    ToolCall(ToolCall),
    /// Update on the status or results of a
    /// tool call.
    ToolCallUpdate(ToolCallUpdate),
    /// The agent's execution plan.
    Plan(Plan),
    /// Available commands are ready or have
    /// changed.
    AvailableCommandsUpdate(AvailableCommandsUpdate),
    /// The current mode of the session has
    /// changed.
    CurrentModeUpdate(CurrentModeUpdate),
    /// Session configuration options have
    /// been updated.
    ConfigOptionUpdate(ConfigOptionUpdate),
    /// Session metadata has been updated
    /// (title, timestamps).
    SessionInfoUpdate(SessionInfoUpdate),
    /// Context window and cost update.
    UsageUpdate(UsageUpdate),
    /// Information for the user that is not
    /// part of session history.
    Notice(Notice),
    /// A session update the typed form does
    /// not yet model. The wire payload is
    /// preserved verbatim for the bridge to
    /// forward.
    #[serde(untagged)]
    Other(serde_json::Value),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::acp::content::TextContent;
    use crate::domain::acp::tool_call::{ToolCallId, ToolCallStatus};
    use serde_json::json;

    /// The `ContentChunk` wire form is `{ content,
    /// messageId? }`. The `messageId` is omitted
    /// from the wire when absent.
    #[test]
    fn a_content_chunk_round_trips() -> Result<(), serde_json::Error> {
        let chunk = ContentChunk {
            content: ContentBlock::Text(TextContent {
                text: "hello".to_owned(),
                annotations: None,
                meta: BTreeMap::new(),
            }),
            message_id: Some(MessageId::new("msg-1")),
            meta: BTreeMap::new(),
        };
        let value = serde_json::to_value(&chunk)?;
        assert_eq!(value.get("messageId"), Some(&json!("msg-1")));
        let back: ContentChunk = serde_json::from_value(value)?;
        assert_eq!(back, chunk);
        Ok(())
    }

    /// The `PlanEntryPriority` enum deserialises
    /// every wire value the schema names.
    #[test]
    fn plan_entry_priority_string_round_trips() -> Result<(), serde_json::Error> {
        for (variant, wire) in [
            (PlanEntryPriority::High, "high"),
            (PlanEntryPriority::Medium, "medium"),
            (PlanEntryPriority::Low, "low"),
        ] {
            let value = serde_json::to_value(variant)?;
            assert_eq!(value, json!(wire));
            let back: PlanEntryPriority = serde_json::from_value(value)?;
            assert_eq!(back, variant);
        }
        Ok(())
    }

    /// The `PlanEntryStatus` enum deserialises
    /// every wire value the schema names.
    #[test]
    fn plan_entry_status_string_round_trips() -> Result<(), serde_json::Error> {
        for (variant, wire) in [
            (PlanEntryStatus::Pending, "pending"),
            (PlanEntryStatus::InProgress, "in_progress"),
            (PlanEntryStatus::Completed, "completed"),
            (PlanEntryStatus::Failed, "failed"),
        ] {
            let value = serde_json::to_value(variant)?;
            assert_eq!(value, json!(wire));
            let back: PlanEntryStatus = serde_json::from_value(value)?;
            assert_eq!(back, variant);
        }
        Ok(())
    }

    /// A `Plan` with one entry round-trips. The
    /// wire form is `{"entries": [...]}`. A plan
    /// is `Default` because every field is
    /// optional.
    #[test]
    fn a_plan_round_trips() -> Result<(), serde_json::Error> {
        let plan = Plan {
            entries: vec![PlanEntry {
                content: "Read the README".to_owned(),
                priority: PlanEntryPriority::High,
                status: PlanEntryStatus::InProgress,
            }],
        };
        let value = serde_json::to_value(&plan)?;
        let entries = value
            .get("entries")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| serde_json::Error::io(std::io::Error::other("missing `entries`")))?;
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries
                .first()
                .and_then(|e| e.get("content"))
                .and_then(serde_json::Value::as_str),
            Some("Read the README"),
        );
        let back: Plan = serde_json::from_value(value)?;
        assert_eq!(back, plan);
        Ok(())
    }

    /// The `NoticeSeverity::Info/Warning/Error`
    /// variants serialise as the schema's
    /// lowercase strings. The `Other(String)`
    /// passthrough preserves an unknown wire
    /// value for the bridge to forward.
    #[test]
    fn notice_severity_round_trips() -> Result<(), serde_json::Error> {
        for (variant, wire) in [
            (NoticeSeverity::Info, "info"),
            (NoticeSeverity::Warning, "warning"),
            (NoticeSeverity::Error, "error"),
        ] {
            let value = serde_json::to_value(&variant)?;
            assert_eq!(value, json!(wire));
            let back: NoticeSeverity = serde_json::from_value(value)?;
            assert_eq!(back, variant);
        }
        let other_value = json!("future_severity");
        let back: NoticeSeverity = serde_json::from_value(other_value)?;
        assert_eq!(back, NoticeSeverity::Other("future_severity".to_owned()));
        Ok(())
    }

    /// The `SessionUpdate::AgentMessageChunk`
    /// variant carries a `ContentChunk` payload
    /// and the wire `sessionUpdate` discriminator
    /// is `agent_message_chunk`.
    #[test]
    fn an_agent_message_chunk_session_update_round_trips() -> Result<(), serde_json::Error> {
        let update = SessionUpdate::AgentMessageChunk(ContentChunk {
            content: ContentBlock::Text(TextContent {
                text: "hi".to_owned(),
                annotations: None,
                meta: BTreeMap::new(),
            }),
            message_id: Some(MessageId::new("msg-1")),
            meta: BTreeMap::new(),
        });
        let value = serde_json::to_value(&update)?;
        assert_eq!(value.get("sessionUpdate"), Some(&json!("agent_message_chunk")));
        let back: SessionUpdate = serde_json::from_value(value)?;
        assert_eq!(back, update);
        Ok(())
    }

    /// The `SessionUpdate::ToolCall` variant
    /// carries a `ToolCall` payload and the wire
    /// `sessionUpdate` discriminator is
    /// `tool_call`.
    #[test]
    fn a_tool_call_session_update_round_trips() -> Result<(), serde_json::Error> {
        let update = SessionUpdate::ToolCall(ToolCall {
            tool_call_id: ToolCallId::new("call-1"),
            title: "Reading".to_owned(),
            name: Some("read_file".to_owned()),
            kind: None,
            status: Some(ToolCallStatus::InProgress),
            content: None,
            locations: None,
            raw_input: None,
            raw_output: None,
        });
        let value = serde_json::to_value(&update)?;
        assert_eq!(value.get("sessionUpdate"), Some(&json!("tool_call")));
        assert_eq!(value.get("title"), Some(&json!("Reading")));
        let back: SessionUpdate = serde_json::from_value(value)?;
        assert_eq!(back, update);
        Ok(())
    }

    /// The `SessionUpdate::UsageUpdate` variant
    /// carries a `UsageUpdate` payload and the
    /// wire `sessionUpdate` discriminator is
    /// `usage_update`.
    #[test]
    fn a_usage_update_session_update_round_trips() -> Result<(), serde_json::Error> {
        let update = SessionUpdate::UsageUpdate(UsageUpdate {
            used: 1024,
            size: 200_000,
            cost: Some(Cost {
                amount: 0.42,
                currency: "USD".to_owned(),
            }),
        });
        let value = serde_json::to_value(&update)?;
        assert_eq!(value.get("sessionUpdate"), Some(&json!("usage_update")));
        let back: SessionUpdate = serde_json::from_value(value)?;
        assert_eq!(back, update);
        Ok(())
    }

    /// The `SessionUpdate::Notice` variant
    /// carries a `Notice` payload. The wire
    /// `sessionUpdate` discriminator is `notice`.
    #[test]
    fn a_notice_session_update_round_trips() -> Result<(), serde_json::Error> {
        let update = SessionUpdate::Notice(Notice {
            level: NoticeSeverity::Warning,
            text: "Rate limit approaching".to_owned(),
        });
        let value = serde_json::to_value(&update)?;
        assert_eq!(value.get("sessionUpdate"), Some(&json!("notice")));
        assert_eq!(
            value.get("level"),
            Some(&json!("warning"))
        );
        let back: SessionUpdate = serde_json::from_value(value)?;
        assert_eq!(back, update);
        Ok(())
    }

    /// An unknown `sessionUpdate` value
    /// deserialises into the `Other` passthrough
    /// variant. The wire payload is preserved
    /// verbatim for the bridge to forward.
    #[test]
    fn an_unknown_session_update_lands_in_the_other_variant() -> Result<(), serde_json::Error>
    {
        let raw = json!({
            "sessionUpdate": "future_update",
            "payload": "anything"
        });
        let update: SessionUpdate = serde_json::from_value(raw)?;
        match update {
            SessionUpdate::Other(value) => {
                assert_eq!(value.get("sessionUpdate"), Some(&json!("future_update")));
            }
            other => {
                return Err(serde_json::Error::io(std::io::Error::other(format!(
                    "expected Other, got {other:?}"
                ))));
            }
        }
        Ok(())
    }

    /// The `SessionNotification` envelope carries
    /// a `sessionId` and an `update`. The wire
    /// form is `{"sessionId": "...", "update": ...}`.
    #[test]
    fn a_session_notification_round_trips() -> Result<(), serde_json::Error> {
        let notification = SessionNotification {
            session_id: SessionId::new("session-1"),
            update: SessionUpdate::Plan(Plan { entries: vec![] }),
        };
        let value = serde_json::to_value(&notification)?;
        assert_eq!(value.get("sessionId"), Some(&json!("session-1")));
        let back: SessionNotification = serde_json::from_value(value)?;
        assert_eq!(back, notification);
        Ok(())
    }
}
