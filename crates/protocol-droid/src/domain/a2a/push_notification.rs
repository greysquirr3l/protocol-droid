//! A2A push-notification configuration value
//! objects — the wire form.
//!
//! # What these are
//!
//! [`TaskPushNotificationConfig`] is the wire
//! shape a client sends to register a webhook
//! for task updates. It is the value the four
//! push-notification JSON-RPC methods
//! (`CreateTaskPushNotificationConfig`,
//! `GetTaskPushNotificationConfig`,
//! `ListTaskPushNotificationConfigs`,
//! `DeleteTaskPushNotificationConfig`) read and
//! write.
//!
//! [`PushNotificationAuthenticationInfo`] is
//! the authentication block inside a
//! `TaskPushNotificationConfig`. The spec
//! names it `AuthenticationInfo`; the
//! `PushNotification` prefix disambiguates it
//! from the agent card's `SecurityScheme` and
//! `SecurityRequirement` types.
//!
//! # Why `String` for the URL, not `url::Url`
//!
//! The wire form is a string. The `url::Url`
//! type would be a more typed alternative but
//! adds a dependency and a `Result` on every
//! read. A consumer who wants a parsed URL
//! does the `Url::parse` on demand; the wire
//! form stays the spec's string.
//!
//! # Why the URL is a separate `String`
//!
//! The URL is the SSRF surface the A2A SSRF
//! denylist filters on the server side. A
//! client that sends a URL is trusting the
//! server to either fetch it (after
//! validation) or refuse. The wire form is a
//! raw string; the
//! [`NotificationConfig`](crate::application::a2a::NotificationConfig)
//! is the post-validation form the delivery
//! port sees.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A registered push-notification configuration
/// for a task.
///
/// Wire form:
/// ```json
/// {
///   "id": "config-1",
///   "taskId": "task-1",
///   "url": "https://example.com/webhook",
///   "token": "auth-token",
///   "authentication": { "scheme": "Bearer", "credentials": "..." }
/// }
/// ```
///
/// All fields are required except `token` and
/// `authentication`. The spec lets a client
/// register a config with no token (a webhook
/// the agent POSTs to without authentication)
/// and no authentication block (a webhook
/// that uses a header the agent does not need
/// to know about, or an unauthenticated
/// webhook).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskPushNotificationConfig {
    /// The configuration's id. Required —
    /// the spec uses it to address a config
    /// within a task.
    pub id: String,
    /// The task's id. Required — a config is
    /// scoped to one task.
    pub task_id: String,
    /// The webhook the server POSTs to. The
    /// URL is the SSRF surface the SSRF
    /// denylist filters on the server side.
    /// Required.
    pub url: String,
    /// An optional token the server includes
    /// in the `X-A2A-Notification-Token`
    /// header. A receiver without a token is
    /// a receiver that does not need to
    /// authenticate the delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Optional authentication info. A
    /// webhook that needs a Bearer token
    /// (or Basic, or Digest) sets this; a
    /// webhook that does not leaves it off
    /// the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<PushNotificationAuthenticationInfo>,
    /// Per-config metadata.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl TaskPushNotificationConfig {
    /// Starts a new config with `id`, `task_id`,
    /// and `url`. Use [`with_token`](Self::with_token)
    /// and [`with_authentication`](Self::with_authentication)
    /// to set the optional fields.
    #[must_use]
    pub fn new(id: impl Into<String>, task_id: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            task_id: task_id.into(),
            url: url.into(),
            token: None,
            authentication: None,
            metadata: BTreeMap::new(),
        }
    }

    /// The token to send.
    ///
    /// Absent on the wire unless this method is called.
    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// The authentication info to send.
    ///
    /// Absent on the wire unless this method is called.
    #[must_use]
    pub fn with_authentication(
        mut self,
        authentication: PushNotificationAuthenticationInfo,
    ) -> Self {
        self.authentication = Some(authentication);
        self
    }
}

/// The authentication block inside a
/// [`TaskPushNotificationConfig`].
///
/// Wire form:
/// ```json
/// { "scheme": "Bearer", "credentials": "..." }
/// ```
///
/// `scheme` is required; `credentials` is
/// optional. The spec lets a config carry
/// just a scheme name (e.g. `"Bearer"`) and
/// rely on a side channel for the
/// credentials; the agent reads the
/// `credentials` field when present and
/// falls back to the side channel when it
/// is not.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushNotificationAuthenticationInfo {
    /// The HTTP authentication scheme. Per
    /// [IANA registry](https://www.iana.org/assignments/http-authschemes/),
    /// examples are `Bearer`, `Basic`,
    /// `Digest`. Scheme names are
    /// case-insensitive per RFC 9110
    /// §11.1; the wire form is the
    /// canonical casing the client sends,
    /// and the agent normalises on read.
    pub scheme: String,
    /// The credentials. The format depends
    /// on the scheme (a token for `Bearer`,
    /// a base64-encoded `user:pass` for
    /// `Basic`, etc.). Optional — a config
    /// that names a scheme without
    /// credentials is valid per the spec
    /// and relies on a side channel for
    /// the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<String>,
}

impl PushNotificationAuthenticationInfo {
    /// Starts a new authentication block with
    /// `scheme`. Use [`with_credentials`](Self::with_credentials)
    /// to set the credentials.
    #[must_use]
    pub fn new(scheme: impl Into<String>) -> Self {
        Self {
            scheme: scheme.into(),
            credentials: None,
        }
    }

    /// The credentials to send.
    ///
    /// Absent on the wire unless this method is called.
    #[must_use]
    pub fn with_credentials(mut self, credentials: impl Into<String>) -> Self {
        self.credentials = Some(credentials.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{PushNotificationAuthenticationInfo, TaskPushNotificationConfig};

    type TestError = Box<dyn std::error::Error>;

    // --- TaskPushNotificationConfig wire form -----------------------

    #[test]
    fn config_serialises_with_required_fields_only() -> Result<(), TestError> {
        let config = TaskPushNotificationConfig::new("c-1", "t-1", "https://example.com/wh");
        let value = serde_json::to_value(&config)?;
        let obj = value
            .as_object()
            .ok_or("a TaskPushNotificationConfig must serialise to a JSON object")?;
        assert_eq!(
            obj.get("id").and_then(serde_json::Value::as_str),
            Some("c-1")
        );
        assert_eq!(
            obj.get("taskId").and_then(serde_json::Value::as_str),
            Some("t-1")
        );
        assert_eq!(
            obj.get("url").and_then(serde_json::Value::as_str),
            Some("https://example.com/wh")
        );
        // The optional fields are absent
        // when they are `None` / empty.
        assert!(!obj.contains_key("token"));
        assert!(!obj.contains_key("authentication"));
        assert!(!obj.contains_key("metadata"));
        Ok(())
    }

    #[test]
    fn config_round_trips_with_token_and_authentication() -> Result<(), TestError> {
        let config = TaskPushNotificationConfig::new("c-1", "t-1", "https://example.com/wh")
            .with_token("auth-token")
            .with_authentication(
                PushNotificationAuthenticationInfo::new("Bearer").with_credentials("xyz"),
            );
        let json = serde_json::to_string(&config)?;
        let parsed: TaskPushNotificationConfig = serde_json::from_str(&json)?;
        assert_eq!(parsed, config);
        Ok(())
    }

    // --- PushNotificationAuthenticationInfo wire form ---------------

    #[test]
    fn auth_info_serialises_with_scheme_only() -> Result<(), TestError> {
        let info = PushNotificationAuthenticationInfo::new("Bearer");
        let value = serde_json::to_value(&info)?;
        let obj = value
            .as_object()
            .ok_or("a PushNotificationAuthenticationInfo must serialise to a JSON object")?;
        assert_eq!(
            obj.get("scheme").and_then(serde_json::Value::as_str),
            Some("Bearer")
        );
        // `credentials` is absent when `None`.
        assert!(!obj.contains_key("credentials"));
        Ok(())
    }

    #[test]
    fn auth_info_round_trips_with_credentials() -> Result<(), TestError> {
        let info =
            PushNotificationAuthenticationInfo::new("Basic").with_credentials("dXNlcjpwYXNz");
        let json = serde_json::to_string(&info)?;
        let parsed: PushNotificationAuthenticationInfo = serde_json::from_str(&json)?;
        assert_eq!(parsed, info);
        Ok(())
    }
}
