//! A2A streaming event value objects — the
//! `TaskStatusUpdateEvent` and
//! `TaskArtifactUpdateEvent` types that flow
//! over a Server-Sent Events connection.
//!
//! # What these are
//!
//! A2A's two streaming methods
//! (`SendStreamingMessage` and
//! `SubscribeToTask`) emit a stream of events
//! over a single HTTP response. The events are
//! the same shape regardless of which method
//! the client used to open the stream:
//!
//! - [`TaskStatusUpdateEvent`] — the task's
//!   status changed. The consumer reads the
//!   new `status` field.
//! - [`TaskArtifactUpdateEvent`] — the task
//!   produced an artifact (or appended to an
//!   existing one). The consumer reads the
//!   `artifact` field.
//!
//! # Why the events are not a single enum
//!
//! The wire form is one of two distinct JSON
//! objects; the consumer's matching code is
//! field-presence discrimination. A typed
//! enum ([`StreamingEvent`]) is also shipped
//! for the consumer who wants exhaustive
//! matching. The enum is
//! `#[serde(untagged)]` so the wire form
//! matches the spec.
//!
//! # Why `final` is on the event, not derived
//! from `status.state`
//!
//! The proto's `TaskStatusUpdateEvent` has no
//! `final` field — the consumer derives
//! "this is the final update" from
//! `status.state` being a terminal state
//! (`Completed`, `Failed`, `Cancelled`,
//! `Rejected`). The JSON wire form (per
//! the A2A v1.0 spec) adds a `final: true`
//! boolean so a client does not have to
//! mirror the proto's terminal-state list.
//! The Rust type carries the boolean
//! explicitly; the consumer can read it
//! without re-implementing the
//! terminal-state check.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::task::{Artifact, TaskStatus};

/// A streaming event that reports a task's
/// status change.
///
/// Wire form:
/// ```json
/// {
///   "taskId": "task-1",
///   "contextId": "context-1",
///   "status": { "state": "working" },
///   "final": false,
///   "metadata": { ... }
/// }
/// ```
///
/// `taskId`, `contextId`, `status`, and
/// `final` are required; `metadata` is
/// optional. The `final` field is the
/// JSON-spec extension that lets a
/// client close the stream without
/// mirroring the proto's terminal-state
/// list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatusUpdateEvent {
    /// The task's id. Required.
    pub task_id: String,
    /// The task's context id. Required.
    pub context_id: String,
    /// The task's new status. Required.
    pub status: TaskStatus,
    /// Whether this is the last event for
    /// the stream. Required by the JSON
    /// spec; a value of `true` means the
    /// client can close the connection.
    #[serde(rename = "final")]
    pub is_final: bool,
    /// Per-event metadata.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl TaskStatusUpdateEvent {
    /// Starts a new event with `task_id`,
    /// `context_id`, and `status`. The
    /// `final` field defaults to `false`.
    /// Use
    /// [`with_final`](Self::with_final)
    /// to mark the event as the last
    /// one in the stream.
    #[must_use]
    pub fn new(
        task_id: impl Into<String>,
        context_id: impl Into<String>,
        status: TaskStatus,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            context_id: context_id.into(),
            status,
            is_final: false,
            metadata: BTreeMap::new(),
        }
    }

    /// Sets the `final` flag to `true`.
    #[must_use]
    pub const fn with_final(mut self) -> Self {
        self.is_final = true;
        self
    }
}

/// A streaming event that reports a task
/// artifact being produced or appended.
///
/// Wire form:
/// ```json
/// {
///   "taskId": "task-1",
///   "contextId": "context-1",
///   "artifact": { "name": "answer", "parts": [...] },
///   "append": false,
///   "lastChunk": false,
///   "metadata": { ... }
/// }
/// ```
///
/// `taskId`, `contextId`, and `artifact`
/// are required; `append`, `lastChunk`,
/// and `metadata` are optional. The
/// `append` flag is `true` when the
/// artifact should be appended to a
/// previously sent artifact with the
/// same id (chunked transfer); the
/// `lastChunk` flag is `true` when this
/// is the final chunk of an append.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskArtifactUpdateEvent {
    /// The task's id. Required.
    pub task_id: String,
    /// The task's context id. Required.
    pub context_id: String,
    /// The artifact that was produced or
    /// updated. Required.
    pub artifact: Artifact,
    /// Whether the artifact should be
    /// appended to a previously sent
    /// artifact with the same id. The
    /// default (`false`) means this
    /// artifact replaces any previous
    /// one with the same id.
    #[serde(default, skip_serializing_if = "is_false")]
    pub append: bool,
    /// Whether this is the final chunk
    /// of the artifact. Only meaningful
    /// when `append` is `true`; ignored
    /// otherwise.
    #[serde(default, skip_serializing_if = "is_false")]
    pub last_chunk: bool,
    /// Per-event metadata.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

// `skip_serializing_if` requires
// `fn(&T) -> bool`; the clippy lint is a
// false positive for serde-internal helpers.
#[expect(clippy::trivially_copy_pass_by_ref, reason = "required by skip_serializing_if")]
const fn is_false(b: &bool) -> bool {
    !*b
}

impl TaskArtifactUpdateEvent {
    /// Starts a new event with `task_id`,
    /// `context_id`, and `artifact`. The
    /// `append` and `lastChunk` flags
    /// default to `false`. Use
    /// [`with_append`](Self::with_append)
    /// to mark the artifact as an
    /// append, and
    /// [`with_last_chunk`](Self::with_last_chunk)
    /// to mark this as the final chunk.
    #[must_use]
    pub fn new(
        task_id: impl Into<String>,
        context_id: impl Into<String>,
        artifact: Artifact,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            context_id: context_id.into(),
            artifact,
            append: false,
            last_chunk: false,
            metadata: BTreeMap::new(),
        }
    }

    /// Sets the `append` flag to `true`.
    #[must_use]
    pub const fn with_append(mut self) -> Self {
        self.append = true;
        self
    }

    /// Sets the `lastChunk` flag to `true`.
    #[must_use]
    pub const fn with_last_chunk(mut self) -> Self {
        self.last_chunk = true;
        self
    }
}

/// An untagged enum of the two streaming
/// event types.
///
/// The wire form is the inner object directly
/// — a `TaskStatusUpdateEvent` has `taskId`,
/// `contextId`, `status`, and `final`; a
/// `TaskArtifactUpdateEvent` has `taskId`,
/// `contextId`, and `artifact`. The two shapes
/// are discriminated by whether `status` or
/// `artifact` is the body field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StreamingEvent {
    /// A status update.
    Status(TaskStatusUpdateEvent),
    /// An artifact update.
    Artifact(TaskArtifactUpdateEvent),
}

impl From<TaskStatusUpdateEvent> for StreamingEvent {
    fn from(event: TaskStatusUpdateEvent) -> Self {
        Self::Status(event)
    }
}

impl From<TaskArtifactUpdateEvent> for StreamingEvent {
    fn from(event: TaskArtifactUpdateEvent) -> Self {
        Self::Artifact(event)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        StreamingEvent, TaskArtifactUpdateEvent, TaskStatusUpdateEvent,
    };
    use crate::domain::a2a::message::{Part, TextPart};
    use crate::domain::a2a::task::{Artifact, TaskState, TaskStatus};

    type TestError = Box<dyn std::error::Error>;

    fn sample_status() -> TaskStatus {
        TaskStatus {
            state: TaskState::Working,
            message: None,
            timestamp: None,
        }
    }

    fn sample_artifact() -> Artifact {
        Artifact::new(Part::Text(TextPart {
            text: "answer".to_owned(),
            metadata: std::collections::BTreeMap::new(),
        }))
    }

    // --- TaskStatusUpdateEvent wire form ---------------------------

    #[test]
    fn status_event_serialises_with_required_fields() -> Result<(), TestError> {
        let event = TaskStatusUpdateEvent::new("t-1", "c-1", sample_status());
        let value = serde_json::to_value(&event)?;
        let obj = value
            .as_object()
            .ok_or("a TaskStatusUpdateEvent must serialise to a JSON object")?;
        assert_eq!(
            obj.get("taskId").and_then(serde_json::Value::as_str),
            Some("t-1")
        );
        assert_eq!(
            obj.get("contextId").and_then(serde_json::Value::as_str),
            Some("c-1")
        );
        assert!(obj.contains_key("status"));
        // The `final` field is required
        // by the JSON spec — a value of
        // `false` is present on the wire.
        assert!(
            obj.contains_key("final"),
            "the `final` field must be present even when false"
        );
        assert_eq!(
            obj.get("final").and_then(serde_json::Value::as_bool),
            Some(false)
        );
        Ok(())
    }

    #[test]
    fn status_event_with_final_true_round_trips() -> Result<(), TestError> {
        let event = TaskStatusUpdateEvent::new(
            "t-1",
            "c-1",
            TaskStatus {
                state: TaskState::Completed,
                message: None,
                timestamp: None,
            },
        )
        .with_final();
        let json = serde_json::to_string(&event)?;
        let parsed: TaskStatusUpdateEvent = serde_json::from_str(&json)?;
        assert_eq!(parsed, event);
        assert!(json.contains(r#""final":true"#));
        Ok(())
    }

    // --- TaskArtifactUpdateEvent wire form -------------------------

    #[test]
    fn artifact_event_serialises_with_artifact_field() -> Result<(), TestError> {
        let event = TaskArtifactUpdateEvent::new("t-1", "c-1", sample_artifact());
        let value = serde_json::to_value(&event)?;
        let obj = value
            .as_object()
            .ok_or("a TaskArtifactUpdateEvent must serialise to a JSON object")?;
        assert_eq!(
            obj.get("taskId").and_then(serde_json::Value::as_str),
            Some("t-1")
        );
        assert_eq!(
            obj.get("contextId").and_then(serde_json::Value::as_str),
            Some("c-1")
        );
        assert!(obj.contains_key("artifact"));
        // The optional `append` and
        // `lastChunk` flags are absent
        // when they are `false`.
        assert!(!obj.contains_key("append"));
        assert!(!obj.contains_key("lastChunk"));
        Ok(())
    }

    #[test]
    fn artifact_event_with_append_and_last_chunk_round_trips() -> Result<(), TestError> {
        let event = TaskArtifactUpdateEvent::new("t-1", "c-1", sample_artifact())
            .with_append()
            .with_last_chunk();
        let json = serde_json::to_string(&event)?;
        let parsed: TaskArtifactUpdateEvent = serde_json::from_str(&json)?;
        assert_eq!(parsed, event);
        assert!(json.contains(r#""append":true"#));
        assert!(json.contains(r#""lastChunk":true"#));
        Ok(())
    }

    // --- StreamingEvent untagged enum ------------------------------

    #[test]
    fn streaming_event_untagged_discrimination_works_both_ways(
    ) -> Result<(), TestError> {
        // A status event JSON object
        // decodes to `StreamingEvent::Status`,
        // not `::Artifact`. An artifact
        // event JSON object decodes to
        // `::Artifact`.
        let status_json =
            r#"{"taskId":"t-1","contextId":"c-1","status":{"state":"working"},"final":false}"#;
        let artifact_json = r#"{"taskId":"t-1","contextId":"c-1","artifact":{"parts":[{"text":"x"}]}}"#;
        let from_status: StreamingEvent = serde_json::from_str(status_json)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let from_artifact: StreamingEvent = serde_json::from_str(artifact_json)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        assert!(matches!(from_status, StreamingEvent::Status(_)));
        assert!(matches!(from_artifact, StreamingEvent::Artifact(_)));
        Ok(())
    }
}
