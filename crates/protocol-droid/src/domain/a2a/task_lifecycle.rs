//! A2A `GetTask` and `CancelTask` typed envelopes.
//!
//! # What these are
//!
//! [`GetTaskRequest`] and [`GetTaskResponse`] are
//! the typed shapes for the `GetTask` JSON-RPC
//! method. The method fetches a task's current
//! state by id; a client polls it to advance a
//! `Submitted` / `Working` task to a terminal
//! state.
//!
//! [`CancelTaskRequest`] and
//! [`CancelTaskResponse`] are the typed shapes
//! for the `CancelTask` JSON-RPC method. The
//! method requests that a task be cancelled;
//! the response is the task as it was observed
//! at the moment of cancellation.
//!
//! Both methods return a [`Task`] on success —
//! `CancelTask` returns the task in
//! [`Cancelled`](TaskState::Cancelled) state
//! (or the state it was in, if the task had
//! already completed), and `GetTask` returns
//! the task in whatever state it is currently
//! in.
//!
//! # Why one file for two methods
//!
//! `GetTask` and `CancelTask` are paired: a
//! client uses `GetTask` to poll a task to
//! completion, and `CancelTask` to abort a
//! long-running task. Their request shapes
//! overlap (both take an id), and the
//! file-split the project uses for the rest
//! of the A2A IR is "one method per file",
//! but pairing them keeps the relationship
//! visible at a glance.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::task::Task;

/// A typed `GetTask` request. Wire form:
/// ```json
/// {
///   "id": "task-123",
///   "historyLength": 10,
///   "metadata": { ... }
/// }
/// ```
///
/// `id` is required; `historyLength` and
/// `metadata` are optional. A request with
/// only the id serialises to `{"id": "..."}`
/// on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetTaskRequest {
    /// The task's id. Required.
    pub id: String,
    /// The number of history messages to
    /// include in the response. A request
    /// with no history length lets the
    /// agent choose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_length: Option<u32>,
    /// Per-request metadata. The spec leaves
    /// the shape open; the conventional
    /// keys are `"trace_id"`, `"session_id"`,
    /// and the custom names a particular
    /// deployment uses.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl GetTaskRequest {
    /// Starts a new request with `id`. Use
    /// [`with_history_length`](Self::with_history_length)
    /// to set the optional history length.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            history_length: None,
            metadata: BTreeMap::new(),
        }
    }

    /// Sets the history length.
    #[must_use]
    pub const fn with_history_length(mut self, length: u32) -> Self {
        self.history_length = Some(length);
        self
    }
}

/// A typed `GetTask` response. The wire form
/// is the [`Task`] object directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GetTaskResponse(pub Task);

impl From<Task> for GetTaskResponse {
    fn from(task: Task) -> Self {
        Self(task)
    }
}

impl std::ops::Deref for GetTaskResponse {
    type Target = Task;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A typed `CancelTask` request. Wire form:
/// ```json
/// { "id": "task-123", "metadata": { ... } }
/// ```
///
/// `id` is required; `metadata` is optional.
/// A request with only the id serialises to
/// `{"id": "..."}` on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelTaskRequest {
    /// The task's id. Required.
    pub id: String,
    /// Per-request metadata.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl CancelTaskRequest {
    /// Starts a new request with `id`.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            metadata: BTreeMap::new(),
        }
    }
}

/// A typed `CancelTask` response. The wire form
/// is the [`Task`] object directly.
///
/// The task is whatever state it was in at the
/// moment of cancellation — typically
/// [`Cancelled`](super::task::TaskState::Cancelled),
/// but possibly a terminal state if the task
/// had already completed. The handler is
/// responsible for setting the right state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CancelTaskResponse(pub Task);

impl From<Task> for CancelTaskResponse {
    fn from(task: Task) -> Self {
        Self(task)
    }
}

impl std::ops::Deref for CancelTaskResponse {
    type Target = Task;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CancelTaskRequest, CancelTaskResponse, GetTaskRequest, GetTaskResponse,
    };
    use crate::domain::a2a::task::{Task, TaskState, TaskStatus};

    type TestError = Box<dyn std::error::Error>;

    // --- GetTaskRequest -----------------------------------------------

    #[test]
    fn get_task_request_serialises_with_id_only() -> Result<(), TestError> {
        let request = GetTaskRequest::new("task-1");
        let value = serde_json::to_value(&request)?;
        let obj = value
            .as_object()
            .ok_or("a GetTaskRequest must serialise to a JSON object")?;
        assert_eq!(
            obj.get("id").and_then(serde_json::Value::as_str),
            Some("task-1")
        );
        // The optional fields are absent on
        // the wire when they are `None` /
        // empty.
        assert!(!obj.contains_key("historyLength"));
        assert!(!obj.contains_key("metadata"));
        Ok(())
    }

    #[test]
    fn get_task_request_round_trips_with_history_length() -> Result<(), TestError> {
        let request = GetTaskRequest::new("task-1").with_history_length(5);
        let json = serde_json::to_string(&request)?;
        let parsed: GetTaskRequest = serde_json::from_str(&json)?;
        assert_eq!(parsed, request);
        Ok(())
    }

    // --- GetTaskResponse ----------------------------------------------

    #[test]
    fn get_task_response_serialises_as_task_object() -> Result<(), TestError> {
        let task = Task::new("task-1").with_status(TaskStatus {
            state: TaskState::Working,
            message: None,
            timestamp: None,
        });
        let response = GetTaskResponse(task);
        let value = serde_json::to_value(&response)?;
        let obj = value
            .as_object()
            .ok_or("a GetTaskResponse must serialise to a JSON object")?;
        // `#[serde(transparent)]` flattens the
        // wrapper — the wire form is the
        // inner Task, not a wrapping object.
        assert_eq!(
            obj.get("id").and_then(serde_json::Value::as_str),
            Some("task-1")
        );
        assert!(obj.contains_key("status"));
        Ok(())
    }

    #[test]
    fn get_task_response_from_task_via_into_works() {
        let task = Task::new("task-1");
        let response: GetTaskResponse = task.clone().into();
        assert_eq!(response.0, task);
    }

    // --- CancelTaskRequest --------------------------------------------

    #[test]
    fn cancel_task_request_serialises_with_id_only() -> Result<(), TestError> {
        let request = CancelTaskRequest::new("task-1");
        let value = serde_json::to_value(&request)?;
        let obj = value
            .as_object()
            .ok_or("a CancelTaskRequest must serialise to a JSON object")?;
        assert_eq!(
            obj.get("id").and_then(serde_json::Value::as_str),
            Some("task-1")
        );
        assert!(!obj.contains_key("metadata"));
        Ok(())
    }

    #[test]
    fn cancel_task_request_round_trips() -> Result<(), TestError> {
        let request = CancelTaskRequest::new("task-1");
        let json = serde_json::to_string(&request)?;
        let parsed: CancelTaskRequest = serde_json::from_str(&json)?;
        assert_eq!(parsed, request);
        Ok(())
    }

    // --- CancelTaskResponse -------------------------------------------

    #[test]
    fn cancel_task_response_serialises_cancelled_state() -> Result<(), TestError> {
        // The typical wire form: a task in
        // `Cancelled` state. The handler
        // is responsible for setting the
        // right state; the response type
        // just wraps the Task.
        let task = Task::new("task-1").with_status(TaskStatus {
            state: TaskState::Cancelled,
            message: None,
            timestamp: None,
        });
        let response = CancelTaskResponse(task);
        let json = serde_json::to_string(&response)?;
        let value: serde_json::Value = serde_json::from_str(&json)?;
        let obj = value
            .as_object()
            .ok_or("a CancelTaskResponse must serialise to a JSON object")?;
        let status = obj
            .get("status")
            .and_then(serde_json::Value::as_object)
            .ok_or("`status` must be a JSON object")?;
        assert_eq!(
            status.get("state").and_then(serde_json::Value::as_str),
            Some("cancelled")
        );
        Ok(())
    }
}
