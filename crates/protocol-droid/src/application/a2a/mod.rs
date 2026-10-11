//! A2A's application layer — use cases the wire codec routes into.
//!
//! Each handler is the *consumer* of a narrow port trait declared here. The
//! port trait's return type is a domain error, not an infrastructure one, so
//! the handler can be tested without a socket and the adapter can map
//! `reqwest` / `tokio` failures into the domain shape at the boundary.

pub(crate) mod handler;
mod push_notification;
mod typed_dispatch;
mod url_filter;

pub use handler::{A2aError, A2aRequestContext, A2aRequestHandler};
pub use push_notification::{
    NotificationConfig, NotificationConfigError, PushNotificationDelivery, WebhookUrl,
};
pub use typed_dispatch::A2aRequestHandlerExt;
pub use url_filter::{FilterDenial, UrlFilter};
