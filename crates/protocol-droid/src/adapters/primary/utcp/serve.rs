//! Serving a manual at `GET /utcp`.
//!
//! # Why the manual is behind a swap
//!
//! The registry changes while the server runs: a tool is registered, a tool
//! is removed. A manual built from it is therefore a snapshot, and a
//! snapshot that is rebuilt in place would be a document a reader can see
//! half-written. The swap makes the publish one atomic store, which is the
//! same transaction boundary the registry itself uses.
//!
//! # Why the route carries the security headers
//!
//! The manual is a document this server publishes about itself. The headers
//! that keep a browser from doing something clever with it — a content
//! security policy, a frame denial, and a strict transport — are the same
//! ones the MCP router sets, and they are set here for the same reason: a
//! response that leaves the server is a response the server is responsible
//! for.

use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header::{CONTENT_TYPE, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::application::utcp::publish::{PublishCommand, publish};
use crate::domain::utcp::Manual;

/// The path the specification puts a manual at.
pub const MANUAL_PATH: &str = "/utcp";

/// A manual in front of an HTTP route.
///
/// Cloneable, because the router holds it as state and the consumer keeps
/// the publisher to publish into it.
#[derive(Debug, Clone)]
pub struct ManualPublisher {
    manual: Arc<ArcSwap<Manual>>,
}

impl ManualPublisher {
    /// A publisher holding the manual for a set of tools.
    #[must_use]
    pub fn new(command: &PublishCommand) -> Self {
        Self {
            manual: Arc::new(ArcSwap::from_pointee(publish(command))),
        }
    }

    /// Rebuilds the manual and swaps it in.
    ///
    /// One atomic store, so a reader sees the old manual or the new one and
    /// never a mixture of the two.
    pub fn publish(&self, command: &PublishCommand) {
        self.manual.store(Arc::new(publish(command)));
    }

    /// The manual being served.
    #[must_use]
    pub fn manual(&self) -> Arc<Manual> {
        self.manual.load_full()
    }

    /// The router that serves the manual.
    ///
    /// The path is [`MANUAL_PATH`]. A consumer mounts it beside the MCP
    /// endpoint:
    ///
    /// ```text
    /// let app = mcp_router.merge(utcp_publisher.router());
    /// ```
    pub fn router(&self) -> Router {
        Router::new()
            .route(MANUAL_PATH, get(serve_manual))
            .with_state(Arc::clone(&self.manual))
    }
}

/// The route handler: the manual, as a JSON response.
async fn serve_manual(State(manual): State<Arc<ArcSwap<Manual>>>) -> Response {
    let manual = manual.load_full();
    serve_body(&manual)
}

/// The manual, as a JSON response.
fn serve_body(manual: &Arc<Manual>) -> Response {
    let mut response = manual.to_json().map_or_else(
        |_| {
            // The manual was built by this crate, so a failure to serialise
            // it is a defect rather than a peer's fault. The body says so
            // without naming an internal path.
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(CONTENT_TYPE, "application/json")],
                r#"{"error":"the manual could not be serialised"}"#.to_owned(),
            )
                .into_response()
        },
        |body| {
            (StatusCode::OK, [(CONTENT_TYPE, "application/json")], body).into_response()
        },
    );
    // The same three the MCP router sets. A manual is a document the server
    // publishes about itself, and a browser that is told nothing will happily
    // frame it.
    let headers = response.headers_mut();
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );
    headers.insert(
        "strict-transport-security",
        HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    response
}
