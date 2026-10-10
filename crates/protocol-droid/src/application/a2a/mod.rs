//! A2A's application layer — use cases the wire codec routes into.
//!
//! Each handler is the *consumer* of a narrow port trait declared here. The
//! port trait's return type is a domain error, not an infrastructure one, so
//! the handler can be tested without a socket and the adapter can map
//! `reqwest` / `tokio` failures into the domain shape at the boundary.

mod push_notification;
mod url_filter;

pub use push_notification::{
    NotificationConfig, NotificationConfigError, PushNotificationDelivery, WebhookUrl,
};
pub use url_filter::{FilterDenial, UrlFilter};
// The submodule is re-exported above through the type names that are
// publicly part of the A2A surface (`FilterDenial`, `UrlFilter`). The
// internal module path is private so a downstream crate cannot reach
// into the `url_filter` internals; an adapter that needs the types goes
// through the re-exports.