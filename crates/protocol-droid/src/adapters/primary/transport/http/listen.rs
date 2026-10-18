//! `subscriptions/listen`: the one `2026-07-28` request whose answer is a
//! stream that outlives it.
//!
//! # What the specification asks of the stream
//!
//! SEP-2575 replaced the `GET` endpoint and `resources/subscribe` with this
//! method. The client opts in to specific notification types, and the server:
//!
//! - acknowledges first: `notifications/subscriptions/acknowledged` "MUST be
//!   the first message the server sends carrying the subscription's ID";
//! - acknowledges honestly: the acknowledgement lists "the subset of requested
//!   notification types the server agreed to honor", omitting any it does not
//!   support;
//! - sends nothing outside the filter: the server "MUST NOT send notification
//!   types the client has not explicitly requested";
//! - tags every notification with `_meta["io.modelcontextprotocol/subscriptionId"]`,
//!   whose value is the JSON-RPC id of the `subscriptions/listen` request.
//!
//! The id is the request's own, not one this server mints. The schema says so
//! in as many words — "The value is the JSON-RPC ID of the
//! `subscriptions/listen` request that opened the stream" — and a client
//! correlating by that value would never match a server-chosen label.
//!
//! # How the stream ends
//!
//! When the client goes away. The subscription is held by a guard inside the
//! response body, so dropping the body — which is what a disconnect does —
//! withdraws the subscription in the same step. No result is sent while the
//! stream is open: `SubscriptionsListenResult` is defined as the answer the
//! server sends only when *it* tears the subscription down.

use core::num::NonZeroUsize;
use core::pin::Pin;
use core::task::{Context, Poll};
use core::time::Duration;

use axum::body::Bytes;
use axum::http::StatusCode;
use futures_core::Stream;
use serde_json::Value;

use crate::adapters::primary::transport::http::refusal::Refusal;
use crate::adapters::primary::transport::http::serve::{HttpHandler, Tail, TailSink};
use crate::adapters::primary::transport::http::stateless::{error_document, json_response};
use crate::adapters::primary::transport::http::stream::{EventStream, StreamError, stream_response};
use crate::application::server::context::PeerSession;
use crate::application::server::{DispatchError, ListKind, ListSubscription};
use crate::application::session::DynFrameSink;
use crate::domain::Feature;
use crate::domain::ir::JsonObject;
use crate::domain::jsonrpc::WireId;

/// The `_meta` key every message on a subscription carries.
pub(super) const SUBSCRIPTION_ID_KEY: &str = "io.modelcontextprotocol/subscriptionId";

/// How many undelivered notifications one subscription retains.
///
/// A listen stream carries list changes, which are idempotent — a client told
/// twice that the tool list changed re-lists once — so a slow reader losing
/// the oldest of several is harmless, and a deep ring would only be memory
/// held per open stream.
const LISTEN_CAPACITY: NonZeroUsize = match NonZeroUsize::new(16) {
    Some(capacity) => capacity,
    // Unreachable: 16 is not zero.
    None => NonZeroUsize::MIN,
};

/// How often an idle subscription sends an SSE comment.
///
/// A listen stream can be silent for hours, and an intermediary that sees no
/// bytes for long enough closes the connection. Fifteen seconds is inside
/// every common proxy idle timeout and costs one short line.
const LISTEN_KEEP_ALIVE: Duration = Duration::from_secs(15);

/// The filter members, with the feature each needs and the kind it maps to.
const FILTER: [(&str, Feature, ListKind); 3] = [
    (
        "toolsListChanged",
        Feature::ToolsListChanged,
        ListKind::Tools,
    ),
    (
        "promptsListChanged",
        Feature::PromptsListChanged,
        ListKind::Prompts,
    ),
    (
        "resourcesListChanged",
        Feature::ResourcesListChanged,
        ListKind::Resources,
    ),
];

/// An event stream that holds a subscription for exactly as long as it lives.
struct Guarded {
    stream: EventStream<Value>,
    _subscription: ListSubscription,
}

impl Stream for Guarded {
    type Item = Result<Bytes, StreamError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.get_mut().stream).poll_next(context)
    }
}

impl HttpHandler {
    /// Opens a subscription, acknowledges it, and streams what it asked for.
    ///
    /// # Errors
    ///
    /// [`Refusal::Handler`] when the stream cannot be assembled. A request
    /// without a filter is answered on the wire with `-32602`.
    pub(super) fn open_listen(
        &self,
        id: &WireId<'static>,
        params: &JsonObject,
        peer: &PeerSession,
    ) -> Result<axum::response::Response, Refusal> {
        let parsed: Value = serde_json::from_str(params.as_str()).unwrap_or(Value::Null);
        let Some(filter) = parsed.get("notifications").and_then(Value::as_object) else {
            return json_response(
                &error_document(Some(id), &DispatchError::MalformedParams),
                StatusCode::OK,
            );
        };

        // The subset this server will actually honour: requested, and
        // negotiable on this request's profile. Resource URIs are honoured only
        // for resources this server lists, for the reason `resources/subscribe`
        // refuses an unknown one: a watch on a URI the server does not have is
        // a wait for an update that cannot come.
        let mut honoured = serde_json::Map::new();
        let mut kinds = Vec::new();
        for (member, feature, kind) in FILTER {
            let requested = filter.get(member).and_then(Value::as_bool) == Some(true);
            if requested && peer.allows(feature) {
                honoured.insert(member.to_owned(), Value::Bool(true));
                kinds.push(kind);
            }
        }

        let server = self.dispatcher.server();
        let resources: Vec<String> = if peer.allows(Feature::ResourcesSubscribe) {
            filter
                .get("resourceSubscriptions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|uri| server.reader(uri).is_some())
                .map(ToOwned::to_owned)
                .collect()
        } else {
            Vec::new()
        };
        if !resources.is_empty() {
            honoured.insert(
                "resourceSubscriptions".to_owned(),
                Value::Array(resources.iter().cloned().map(Value::String).collect()),
            );
        }

        let subscription_id = serde_json::from_str::<Value>(&id.to_json()).unwrap_or(Value::Null);
        let meta = serde_json::json!({ SUBSCRIPTION_ID_KEY: subscription_id });
        let tail = Tail::new(LISTEN_CAPACITY);
        tail.push(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/subscriptions/acknowledged",
            "params": {
                "notifications": honoured,
                "_meta": meta,
            },
        }));

        // Subscribed only after the acknowledgement is in the ring, so no
        // change can be queued ahead of it.
        let sink: std::sync::Arc<dyn DynFrameSink> = std::sync::Arc::new(TailSink { tail: tail.clone() });
        let handle = server.handle();
        let subscription = handle.subscribe(&sink, peer.profile(), &kinds, Some(&meta));
        for uri in &resources {
            handle.watch_resource(&sink, uri, Some(&meta));
        }

        let stream = tail
            .plan(None)
            .try_stream(LISTEN_KEEP_ALIVE)
            .map_err(|_| Refusal::Handler("the subscription stream could not open".to_owned()))?;
        stream_response(Guarded {
            stream,
            _subscription: subscription,
        })
        .map_err(|error| Refusal::Handler(error.to_string()))
    }
}

#[cfg(test)]
mod tests;
