//! A2A `ListTasks` typed envelopes.
//!
//! # What these are
//!
//! [`ListTasksRequest`] and
//! [`ListTasksResponse`] are the typed shapes
//! for the `ListTasks` JSON-RPC method. The
//! method fetches a paginated list of tasks,
//! optionally filtered by context id and
//! status.
//!
//! # Why the response carries `page_size` and
//! `total_size`
//!
//! The spec's wire form includes the page
//! size used and the total number of tasks
//! available (before pagination). A client
//! that wants to know "how many pages?" can
//! read the field directly instead of polling
//! until the `nextPageToken` is empty.
//!
//! # Why `page_size` and `total_size` are
//! `u32` (not `i32`)
//!
//! The spec's `int32` for these fields
//! accepts negatives on the wire, but a
//! negative page size or total size is
//! nonsensical. The Rust type that
//! refuses them is `u32`. A client that
//! sends a negative value gets a
//! deserialisation error, which the
//! serving adapter surfaces as
//! `A2aError::InvalidParams`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::task::{Task, TaskState};

/// A typed `ListTasks` request. Wire form:
/// ```json
/// {
///   "contextId": "ctx-1",
///   "status": "working",
///   "pageSize": 50,
///   "pageToken": "...",
///   "historyLength": 10,
///   "statusTimestampAfter": "2025-01-01T00:00:00Z",
///   "metadata": { ... }
/// }
/// ```
///
/// All fields are optional. A request with
/// no filters and no pagination serialises
/// to `{}` on the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTasksRequest {
    /// Filter tasks by context id. A
    /// request with a context id is a
    /// request for the tasks within that
    /// context; a request with no context
    /// id is a request across all contexts
    /// (subject to the agent's policy).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    /// Filter tasks by status. A request
    /// with a status is a request for the
    /// tasks in that state; a request with
    /// no status is a request across all
    /// states.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskState>,
    /// The maximum number of tasks to
    /// return. A request with no page size
    /// lets the agent choose (the spec's
    /// default is 50, capped at 100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u32>,
    /// A page token received from a
    /// previous `ListTasksResponse`. The
    /// agent uses it to continue from the
    /// last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
    /// The maximum number of history
    /// messages to include in each task's
    /// history. A request with no history
    /// length lets the agent choose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_length: Option<u32>,
    /// Filter tasks whose status was
    /// updated after the provided
    /// timestamp. The format is ISO 8601
    /// (e.g. `"2025-01-01T00:00:00Z"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_timestamp_after: Option<String>,
    /// Per-request metadata.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl ListTasksRequest {
    /// Starts a new empty request. Use
    /// [`with_context_id`](Self::with_context_id),
    /// [`with_status`](Self::with_status),
    /// [`with_page_size`](Self::with_page_size),
    /// [`with_page_token`](Self::with_page_token),
    /// [`with_history_length`](Self::with_history_length),
    /// and
    /// [`with_status_timestamp_after`](Self::with_status_timestamp_after)
    /// to set the optional fields.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            context_id: None,
            status: None,
            page_size: None,
            page_token: None,
            history_length: None,
            status_timestamp_after: None,
            metadata: BTreeMap::new(),
        }
    }

    /// Sets the context id filter.
    #[must_use]
    pub fn with_context_id(mut self, context_id: impl Into<String>) -> Self {
        self.context_id = Some(context_id.into());
        self
    }

    /// Sets the status filter.
    #[must_use]
    pub const fn with_status(mut self, status: TaskState) -> Self {
        self.status = Some(status);
        self
    }

    /// Sets the page size.
    #[must_use]
    pub const fn with_page_size(mut self, page_size: u32) -> Self {
        self.page_size = Some(page_size);
        self
    }

    /// Sets the page token.
    #[must_use]
    pub fn with_page_token(mut self, page_token: impl Into<String>) -> Self {
        self.page_token = Some(page_token.into());
        self
    }

    /// Sets the history length.
    #[must_use]
    pub const fn with_history_length(mut self, length: u32) -> Self {
        self.history_length = Some(length);
        self
    }

    /// Sets the status timestamp filter.
    #[must_use]
    pub fn with_status_timestamp_after(
        mut self,
        timestamp: impl Into<String>,
    ) -> Self {
        self.status_timestamp_after = Some(timestamp.into());
        self
    }
}

/// A typed `ListTasks` response. Wire form:
/// ```json
/// {
///   "tasks": [Task, ...],
///   "nextPageToken": "...",
///   "pageSize": 50,
///   "totalSize": 123
/// }
/// ```
///
/// `tasks` is required (but may be empty).
/// `nextPageToken` is required: the spec
/// uses an empty string to mean "no more
/// pages", which is the spec's idiomatic
/// way of expressing the absence. The Rust
/// type mirrors that with a `String` (not
/// an `Option<String>`).
///
/// `pageSize` and `totalSize` are also
/// required. The spec names them as
/// required fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTasksResponse {
    /// The tasks. An empty list is a valid
    /// response (no tasks matched the
    /// filters).
    pub tasks: Vec<Task>,
    /// A token to retrieve the next page of
    /// results, or an empty string if there
    /// are no more results. The spec
    /// requires this field; an empty string
    /// is the absence signal.
    pub next_page_token: String,
    /// The page size used for this
    /// response. The spec requires this
    /// field; a value of `0` is a malformed
    /// response (the page size is at least
    /// `1` per the spec).
    pub page_size: u32,
    /// The total number of tasks available
    /// before pagination. The spec requires
    /// this field.
    pub total_size: u32,
}

impl ListTasksResponse {
    /// Starts a new response with `tasks`.
    /// Use [`with_next_page_token`](Self::with_next_page_token),
    /// [`with_page_size`](Self::with_page_size),
    /// and [`with_total_size`](Self::with_total_size)
    /// to set the required metadata fields.
    #[must_use]
    pub fn new(tasks: impl IntoIterator<Item = Task>) -> Self {
        Self {
            tasks: tasks.into_iter().collect(),
            next_page_token: String::new(),
            page_size: 0,
            total_size: 0,
        }
    }

    /// Sets the next page token. An empty
    /// string is the spec's "no more pages"
    /// signal.
    #[must_use]
    pub fn with_next_page_token(
        mut self,
        next_page_token: impl Into<String>,
    ) -> Self {
        self.next_page_token = next_page_token.into();
        self
    }

    /// Sets the page size.
    #[must_use]
    pub const fn with_page_size(mut self, page_size: u32) -> Self {
        self.page_size = page_size;
        self
    }

    /// Sets the total size.
    #[must_use]
    pub const fn with_total_size(mut self, total_size: u32) -> Self {
        self.total_size = total_size;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{ListTasksRequest, ListTasksResponse};
    use crate::domain::a2a::task::{Task, TaskState};

    type TestError = Box<dyn std::error::Error>;

    // --- ListTasksRequest --------------------------------------------

    #[test]
    fn list_request_with_no_fields_is_an_empty_object() -> Result<(), TestError> {
        // The default request serialises to
        // `{}` on the wire — every field is
        // `Option` and skipped when `None`,
        // and `metadata` is empty.
        let request = ListTasksRequest::default();
        let value = serde_json::to_value(&request)?;
        let obj = value
            .as_object()
            .ok_or("a ListTasksRequest must serialise to a JSON object")?;
        assert!(obj.is_empty(), "expected `{{}}`, got {value}");
        Ok(())
    }

    #[test]
    fn list_request_round_trips_with_all_filters() -> Result<(), TestError> {
        let request = ListTasksRequest::new()
            .with_context_id("ctx-1")
            .with_status(TaskState::Working)
            .with_page_size(50)
            .with_page_token("next")
            .with_history_length(10)
            .with_status_timestamp_after("2025-01-01T00:00:00Z");
        let json = serde_json::to_string(&request)?;
        let parsed: ListTasksRequest = serde_json::from_str(&json)?;
        assert_eq!(parsed, request);
        Ok(())
    }

    // --- ListTasksResponse -------------------------------------------

    #[test]
    fn list_response_with_no_next_page_uses_empty_string() -> Result<(), TestError> {
        // The spec uses an empty string
        // for `nextPageToken` to mean "no
        // more pages". The field is
        // required; the empty string is
        // the absence signal.
        let response = ListTasksResponse::new(Vec::<Task>::new());
        let value = serde_json::to_value(&response)?;
        let obj = value
            .as_object()
            .ok_or("a ListTasksResponse must serialise to a JSON object")?;
        assert_eq!(
            obj.get("nextPageToken").and_then(serde_json::Value::as_str),
            Some("")
        );
        Ok(())
    }

    #[test]
    fn list_response_round_trips_with_tasks_and_pagination() -> Result<(), TestError> {
        let response = ListTasksResponse::new([
            Task::new("task-1"),
            Task::new("task-2"),
        ])
        .with_next_page_token("next")
        .with_page_size(50)
        .with_total_size(123);
        let json = serde_json::to_string(&response)?;
        let parsed: ListTasksResponse = serde_json::from_str(&json)?;
        assert_eq!(parsed, response);
        Ok(())
    }
}
