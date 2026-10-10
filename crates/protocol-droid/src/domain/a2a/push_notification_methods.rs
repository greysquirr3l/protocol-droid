//! A2A push-notification method envelopes —
//! the four methods that read and write a
//! task's push-notification configurations.
//!
//! # What these are
//!
//! [`CreateTaskPushNotificationConfigRequest`]
//! and
//! [`CreateTaskPushNotificationConfigResponse`]
//! are the typed shapes for the
//! `CreateTaskPushNotificationConfig` JSON-RPC
//! method — register a new webhook for a task.
//!
//! [`GetTaskPushNotificationConfigRequest`]
//! and
//! [`GetTaskPushNotificationConfigResponse`]
//! are the typed shapes for the
//! `GetTaskPushNotificationConfig` JSON-RPC
//! method — fetch a single config by id.
//!
//! [`ListTaskPushNotificationConfigsRequest`]
//! and
//! [`ListTaskPushNotificationConfigsResponse`]
//! are the typed shapes for the
//! `ListTaskPushNotificationConfigs`
//! JSON-RPC method — list all configs for a
//! task.
//!
//! [`DeleteTaskPushNotificationConfigRequest`]
//! and
//! [`DeleteTaskPushNotificationConfigResponse`]
//! are the typed shapes for the
//! `DeleteTaskPushNotificationConfig`
//! JSON-RPC method — remove a config by id.
//!
//! # Why the response is `TaskPushNotificationConfig`
//! directly, not a wrapper
//!
//! Each of the three read/write methods
//! returns a [`TaskPushNotificationConfig`]
//! value. The shape is the inner value
//! directly — no wrapper object — so the
//! wire form is the spec's
//! `{"id": "...", "taskId": "...", "url": "..."}`.
//! A newtype with `transparent` is the
//! cheapest shape that keeps the type
//! distinct from a raw
//! `TaskPushNotificationConfig` (and lets
//! the consumer pattern-match on the
//! response type if they want to).
//!
//! # Why the delete response is `{}`
//!
//! The spec uses `google.protobuf.Empty` for
//! the delete response, which serialises to
//! `{}` on the wire. A handler that wants to
//! signal success does not have to write a
//! payload; the JSON-RPC layer's
//! `result: null` or `result: {}` is the
//! success signal.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::push_notification::TaskPushNotificationConfig;

// --- CreateTaskPushNotificationConfig ---------------------------

/// A typed `CreateTaskPushNotificationConfig`
/// request. The wire form is the
/// [`TaskPushNotificationConfig`] object
/// directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CreateTaskPushNotificationConfigRequest(
    pub TaskPushNotificationConfig,
);

impl From<TaskPushNotificationConfig> for CreateTaskPushNotificationConfigRequest {
    fn from(config: TaskPushNotificationConfig) -> Self {
        Self(config)
    }
}

impl std::ops::Deref for CreateTaskPushNotificationConfigRequest {
    type Target = TaskPushNotificationConfig;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A typed `CreateTaskPushNotificationConfig`
/// response. The wire form is the
/// [`TaskPushNotificationConfig`] object
/// directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CreateTaskPushNotificationConfigResponse(
    pub TaskPushNotificationConfig,
);

impl From<TaskPushNotificationConfig>
    for CreateTaskPushNotificationConfigResponse
{
    fn from(config: TaskPushNotificationConfig) -> Self {
        Self(config)
    }
}

impl std::ops::Deref for CreateTaskPushNotificationConfigResponse {
    type Target = TaskPushNotificationConfig;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// --- GetTaskPushNotificationConfig -------------------------------

/// A typed `GetTaskPushNotificationConfig`
/// request. Wire form:
/// ```json
/// { "id": "config-1", "taskId": "task-1" }
/// ```
///
/// Both fields are required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTaskPushNotificationConfigRequest {
    /// The config's id.
    pub id: String,
    /// The parent task's id.
    pub task_id: String,
}

impl GetTaskPushNotificationConfigRequest {
    /// Starts a new request with `id` and
    /// `task_id`.
    #[must_use]
    pub fn new(id: impl Into<String>, task_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            task_id: task_id.into(),
        }
    }
}

/// A typed `GetTaskPushNotificationConfig`
/// response. The wire form is the
/// [`TaskPushNotificationConfig`] object
/// directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GetTaskPushNotificationConfigResponse(
    pub TaskPushNotificationConfig,
);

impl From<TaskPushNotificationConfig> for GetTaskPushNotificationConfigResponse {
    fn from(config: TaskPushNotificationConfig) -> Self {
        Self(config)
    }
}

impl std::ops::Deref for GetTaskPushNotificationConfigResponse {
    type Target = TaskPushNotificationConfig;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// --- ListTaskPushNotificationConfigs -----------------------------

/// A typed `ListTaskPushNotificationConfigs`
/// request. Wire form:
/// ```json
/// {
///   "taskId": "task-1",
///   "pageSize": 50,
///   "pageToken": "..."
/// }
/// ```
///
/// `taskId` is required; `pageSize` and
/// `pageToken` are optional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTaskPushNotificationConfigsRequest {
    /// The parent task's id. Required.
    pub task_id: String,
    /// The maximum number of configurations
    /// to return. A request with no
    /// page size lets the agent choose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u32>,
    /// A page token received from a
    /// previous
    /// `ListTaskPushNotificationConfigsResponse`
    /// call. The agent uses it to continue
    /// from the last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_token: Option<String>,
}

impl ListTaskPushNotificationConfigsRequest {
    /// Starts a new request with `task_id`.
    /// Use [`with_page_size`](Self::with_page_size)
    /// and
    /// [`with_page_token`](Self::with_page_token)
    /// to set the pagination fields.
    #[must_use]
    pub fn new(task_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            page_size: None,
            page_token: None,
        }
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
}

/// A typed `ListTaskPushNotificationConfigs`
/// response. Wire form:
/// ```json
/// {
///   "configs": [TaskPushNotificationConfig, ...],
///   "nextPageToken": "..."
/// }
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTaskPushNotificationConfigsResponse {
    /// The configurations. An empty list is
    /// a task that has no configs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configs: Vec<TaskPushNotificationConfig>,
    /// A token to retrieve the next page of
    /// results, or empty if there are no
    /// more results in the list. A response
    /// with no `nextPageToken` is the
    /// last page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

impl ListTaskPushNotificationConfigsResponse {
    /// Starts a new response with `configs`.
    /// Use
    /// [`with_next_page_token`](Self::with_next_page_token)
    /// to attach a token.
    #[must_use]
    pub fn new(
        configs: impl IntoIterator<Item = TaskPushNotificationConfig>,
    ) -> Self {
        Self {
            configs: configs.into_iter().collect(),
            next_page_token: None,
        }
    }

    /// Sets the next page token.
    #[must_use]
    pub fn with_next_page_token(
        mut self,
        next_page_token: impl Into<String>,
    ) -> Self {
        self.next_page_token = Some(next_page_token.into());
        self
    }
}

// --- DeleteTaskPushNotificationConfig ----------------------------

/// A typed `DeleteTaskPushNotificationConfig`
/// request. Wire form:
/// ```json
/// { "id": "config-1", "taskId": "task-1" }
/// ```
///
/// Both fields are required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteTaskPushNotificationConfigRequest {
    /// The config's id.
    pub id: String,
    /// The parent task's id.
    pub task_id: String,
}

impl DeleteTaskPushNotificationConfigRequest {
    /// Starts a new request with `id` and
    /// `task_id`.
    #[must_use]
    pub fn new(id: impl Into<String>, task_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            task_id: task_id.into(),
        }
    }
}

/// A typed `DeleteTaskPushNotificationConfig`
/// response. The wire form is `{}` (the
/// spec's `google.protobuf.Empty`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteTaskPushNotificationConfigResponse {
    /// A catch-all for the metadata. The
    /// spec does not name a field, but a
    /// server that wants to include a
    /// trailing note (e.g. `"deleted_at"`:
    /// `"2025-01-01T00:00:00Z"`) can use
    /// this map.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::{
        CreateTaskPushNotificationConfigRequest,
        CreateTaskPushNotificationConfigResponse,
        DeleteTaskPushNotificationConfigRequest,
        DeleteTaskPushNotificationConfigResponse,
        GetTaskPushNotificationConfigRequest,
        GetTaskPushNotificationConfigResponse,
        ListTaskPushNotificationConfigsRequest,
        ListTaskPushNotificationConfigsResponse,
    };
    use crate::domain::a2a::push_notification::TaskPushNotificationConfig;

    type TestError = Box<dyn std::error::Error>;

    fn sample_config() -> TaskPushNotificationConfig {
        TaskPushNotificationConfig::new("c-1", "t-1", "https://example.com/wh")
    }

    // --- Create -----------------------------------------------------

    #[test]
    fn create_request_serialises_as_config_object() -> Result<(), TestError> {
        let request = CreateTaskPushNotificationConfigRequest::from(sample_config());
        let value = serde_json::to_value(&request)?;
        let obj = value
            .as_object()
            .ok_or("a CreateTaskPushNotificationConfigRequest must serialise to a JSON object")?;
        assert_eq!(
            obj.get("id").and_then(serde_json::Value::as_str),
            Some("c-1")
        );
        assert_eq!(
            obj.get("taskId").and_then(serde_json::Value::as_str),
            Some("t-1")
        );
        // The wire form is the inner
        // config — no wrapper.
        assert!(!obj.contains_key("config"));
        Ok(())
    }

    #[test]
    fn create_response_from_config_via_into_works() {
        let config = sample_config();
        let response: CreateTaskPushNotificationConfigResponse = config.clone().into();
        assert_eq!(response.0, config);
    }

    // --- Get --------------------------------------------------------

    #[test]
    fn get_request_serialises_with_id_and_task_id() -> Result<(), TestError> {
        let request = GetTaskPushNotificationConfigRequest::new("c-1", "t-1");
        let value = serde_json::to_value(&request)?;
        let obj = value
            .as_object()
            .ok_or("a GetTaskPushNotificationConfigRequest must serialise to a JSON object")?;
        assert_eq!(
            obj.get("id").and_then(serde_json::Value::as_str),
            Some("c-1")
        );
        assert_eq!(
            obj.get("taskId").and_then(serde_json::Value::as_str),
            Some("t-1")
        );
        Ok(())
    }

    #[test]
    fn get_response_serialises_as_config_object() -> Result<(), TestError> {
        let response = GetTaskPushNotificationConfigResponse::from(sample_config());
        let value = serde_json::to_value(&response)?;
        let obj = value
            .as_object()
            .ok_or("a GetTaskPushNotificationConfigResponse must serialise to a JSON object")?;
        assert_eq!(
            obj.get("id").and_then(serde_json::Value::as_str),
            Some("c-1")
        );
        Ok(())
    }

    // --- List -------------------------------------------------------

    #[test]
    fn list_request_serialises_with_task_id_only() -> Result<(), TestError> {
        let request = ListTaskPushNotificationConfigsRequest::new("t-1");
        let value = serde_json::to_value(&request)?;
        let obj = value
            .as_object()
            .ok_or("a ListTaskPushNotificationConfigsRequest must serialise to a JSON object")?;
        assert_eq!(
            obj.get("taskId").and_then(serde_json::Value::as_str),
            Some("t-1")
        );
        assert!(!obj.contains_key("pageSize"));
        assert!(!obj.contains_key("pageToken"));
        Ok(())
    }

    #[test]
    fn list_request_round_trips_with_pagination() -> Result<(), TestError> {
        let request = ListTaskPushNotificationConfigsRequest::new("t-1")
            .with_page_size(50)
            .with_page_token("next-page");
        let json = serde_json::to_string(&request)?;
        let parsed: ListTaskPushNotificationConfigsRequest =
            serde_json::from_str(&json)?;
        assert_eq!(parsed, request);
        Ok(())
    }

    #[test]
    fn list_response_with_no_next_page_token_omits_the_field() -> Result<(), TestError> {
        // A response that is the last
        // page has no `nextPageToken` —
        // the absence is the signal.
        let response = ListTaskPushNotificationConfigsResponse::new([sample_config()]);
        let value = serde_json::to_value(&response)?;
        let obj = value
            .as_object()
            .ok_or("a ListTaskPushNotificationConfigsResponse must serialise to a JSON object")?;
        assert!(!obj.contains_key("nextPageToken"));
        let configs = obj
            .get("configs")
            .and_then(serde_json::Value::as_array)
            .ok_or("`configs` must be a JSON array")?;
        assert_eq!(configs.len(), 1);
        Ok(())
    }

    #[test]
    fn list_response_round_trips_with_token_and_configs() -> Result<(), TestError> {
        let response = ListTaskPushNotificationConfigsResponse::new([
            sample_config(),
            TaskPushNotificationConfig::new("c-2", "t-1", "https://example.com/wh2"),
        ])
        .with_next_page_token("next");
        let json = serde_json::to_string(&response)?;
        let parsed: ListTaskPushNotificationConfigsResponse =
            serde_json::from_str(&json)?;
        assert_eq!(parsed, response);
        Ok(())
    }

    // --- Delete -----------------------------------------------------

    #[test]
    fn delete_request_serialises_with_id_and_task_id() -> Result<(), TestError> {
        let request = DeleteTaskPushNotificationConfigRequest::new("c-1", "t-1");
        let value = serde_json::to_value(&request)?;
        let obj = value
            .as_object()
            .ok_or("a DeleteTaskPushNotificationConfigRequest must serialise to a JSON object")?;
        assert_eq!(
            obj.get("id").and_then(serde_json::Value::as_str),
            Some("c-1")
        );
        assert_eq!(
            obj.get("taskId").and_then(serde_json::Value::as_str),
            Some("t-1")
        );
        Ok(())
    }

    #[test]
    fn delete_response_with_no_metadata_is_an_empty_object() -> Result<(), TestError> {
        // The spec uses
        // `google.protobuf.Empty` for the
        // delete response, which
        // serialises to `{}` on the
        // wire. A response that adds no
        // metadata is exactly `{}`.
        let response = DeleteTaskPushNotificationConfigResponse::default();
        let value = serde_json::to_value(&response)?;
        let obj = value
            .as_object()
            .ok_or("a DeleteTaskPushNotificationConfigResponse must serialise to a JSON object")?;
        assert!(obj.is_empty(), "expected `{{}}`, got {value}");
        Ok(())
    }
}
