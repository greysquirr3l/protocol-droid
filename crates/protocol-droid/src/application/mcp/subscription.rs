//! `subscriptions/listen`, independent of the transport that carries it.
//!
//! SEP-2575 (`2026-07-28`) replaced the HTTP `GET` stream and
//! `resources/subscribe` with one request: the client names the notifications
//! it wants, and the server acknowledges the subset it will honour and then
//! tags every notification with `_meta["io.modelcontextprotocol/subscriptionId"]`,
//! whose value is the JSON-RPC id of the `subscriptions/listen` request. On
//! Streamable HTTP the notifications ride that request's response stream; on
//! stdio they share the one channel and the id is how a client tells them
//! apart. The decisions — what to honour, how to tag, what to acknowledge —
//! are the same on both, so they live here once.
//!
//! - "The server MUST NOT send notification types the client has not
//!   explicitly requested."
//! - The acknowledgement lists "the subset of requested notification types the
//!   server agreed to honor", omitting any it does not support.
//! - It "MUST be the first message the server sends carrying the
//!   subscription's ID".

use std::sync::Arc;

use serde_json::{Map, Value};

use crate::application::server::context::PeerSession;
use crate::application::server::{ListKind, ListSubscription, Server};
use crate::application::session::DynFrameSink;
use crate::domain::Feature;

/// The `_meta` key every message on a subscription carries.
pub const SUBSCRIPTION_ID_KEY: &str = "io.modelcontextprotocol/subscriptionId";

/// The filter members, with the feature each needs and the kind it maps to.
const FILTER: [(&str, Feature, ListKind); 3] = [
    ("toolsListChanged", Feature::ToolsListChanged, ListKind::Tools),
    ("promptsListChanged", Feature::PromptsListChanged, ListKind::Prompts),
    (
        "resourcesListChanged",
        Feature::ResourcesListChanged,
        ListKind::Resources,
    ),
];

/// What this server agreed to send on one subscription.
#[derive(Debug, Clone, Default)]
pub struct Honoured {
    /// The `SubscriptionFilter` the acknowledgement reports.
    pub filter: Map<String, Value>,
    /// The list notifications to subscribe to.
    pub kinds: Vec<ListKind>,
    /// The resource URIs to watch for `notifications/resources/updated`.
    pub resources: Vec<String>,
}

/// Decides which of `requested` this server will honour for `peer`.
///
/// A member is honoured when the client asked for it and the request's own
/// profile allows it. A resource URI is honoured only for a resource this
/// server lists, because a watch on a URI it does not have is a wait for an
/// update that cannot come.
#[must_use]
pub fn honour(requested: &Map<String, Value>, peer: &PeerSession, server: &Server) -> Honoured {
    let mut honoured = Honoured::default();
    for (member, feature, kind) in FILTER {
        if requested.get(member).and_then(Value::as_bool) == Some(true) && peer.allows(feature) {
            honoured.filter.insert(member.to_owned(), Value::Bool(true));
            honoured.kinds.push(kind);
        }
    }
    if peer.allows(Feature::ResourcesSubscribe) {
        honoured.resources = requested
            .get("resourceSubscriptions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|uri| server.reader(uri).is_some())
            .map(ToOwned::to_owned)
            .collect();
    }
    if !honoured.resources.is_empty() {
        honoured.filter.insert(
            "resourceSubscriptions".to_owned(),
            Value::Array(
                honoured
                    .resources
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    honoured
}

/// The `_meta` object tagging a subscription opened by request `id`.
#[must_use]
pub fn tag(id: &Value) -> Value {
    serde_json::json!({ SUBSCRIPTION_ID_KEY: id })
}

/// The `notifications/subscriptions/acknowledged` frame.
#[must_use]
pub fn acknowledgement(honoured: &Honoured, meta: &Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/subscriptions/acknowledged",
        "params": {
            "notifications": honoured.filter,
            "_meta": meta,
        },
    })
}

/// Subscribes `sink` to everything `honoured` names, tagged with `meta`.
///
/// Call it only after the acknowledgement has been written, so no change can
/// reach the client ahead of it. The returned guard ends both the list and the
/// resource subscriptions when it is dropped.
#[must_use]
pub fn start(
    server: &Server,
    sink: &Arc<dyn DynFrameSink>,
    peer: &PeerSession,
    honoured: &Honoured,
    meta: &Value,
) -> ListSubscription {
    let handle = server.handle();
    let subscription = handle.subscribe(sink, peer.profile(), &honoured.kinds, Some(meta));
    for uri in &honoured.resources {
        handle.watch_resource(sink, uri, Some(meta));
    }
    subscription
}
