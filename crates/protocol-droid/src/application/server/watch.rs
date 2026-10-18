//! `notifications/resources/updated`: which session asked about which resource.
//!
//! A server that advertises `resources.subscribe` is promising to tell a
//! subscriber when a resource it named changes. Through `2025-11-25` the
//! subscriber names it with `resources/subscribe`; at `2026-07-28` it lists it
//! under `resourceSubscriptions` on a `subscriptions/listen` stream. Both end
//! here, keyed by the sink the notification has to travel on, so one
//! `ServerHandle::notify_resource_updated` reaches every session that asked,
//! whichever revision it speaks.
//!
//! A subscription is not a promise the resource exists or will change: a
//! session watching a URI nothing ever updates simply hears nothing, which is
//! what the specification asks of a quiet resource.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use crate::application::registry::Fanout;
use crate::application::session::{DynFrameSink, SinkError};

/// One session's watched URIs and how to tag what it is told.
struct Watch {
    sink: Arc<dyn DynFrameSink>,
    uris: BTreeSet<String>,
    meta: Option<serde_json::Value>,
}

/// Every watch, behind one lock.
///
/// A lock rather than the registry's `ArcSwap`, because a watch set changes per
/// request and is read only when a resource changes — the opposite access
/// pattern from a listing — and the lock is never held across an `.await`: a
/// notification first copies out who to tell, then tells them.
#[derive(Default)]
pub(super) struct Watchers {
    entries: Mutex<Vec<Watch>>,
}

impl Watchers {
    /// Adds `uri` to the watches for `sink`.
    pub(super) fn watch(
        &self,
        sink: &Arc<dyn DynFrameSink>,
        uri: &str,
        meta: Option<&serde_json::Value>,
    ) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| Arc::ptr_eq(&entry.sink, sink))
        {
            entry.uris.insert(uri.to_owned());
            return;
        }
        entries.push(Watch {
            sink: Arc::clone(sink),
            uris: BTreeSet::from([uri.to_owned()]),
            meta: meta.cloned(),
        });
    }

    /// Drops `uri` from the watches for `sink`. Unwatching what was never
    /// watched is not an error: the state afterwards is the same.
    pub(super) fn unwatch(&self, sink: &Arc<dyn DynFrameSink>, uri: &str) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        for entry in entries.iter_mut() {
            if Arc::ptr_eq(&entry.sink, sink) {
                entry.uris.remove(uri);
            }
        }
        entries.retain(|entry| !entry.uris.is_empty());
    }

    /// Drops every watch on `sink`, for a session that has ended.
    pub(super) fn forget(&self, sink: &Arc<dyn DynFrameSink>) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|entry| !Arc::ptr_eq(&entry.sink, sink));
    }

    /// How many sessions watch anything at all.
    pub(super) fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Tells every session watching `uri` that it changed.
    pub(super) async fn notify(&self, uri: &str) -> Fanout {
        let targets: Vec<(Arc<dyn DynFrameSink>, String)> = {
            let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
            entries
                .iter()
                .filter(|entry| entry.uris.contains(uri))
                .map(|entry| (Arc::clone(&entry.sink), frame(uri, entry.meta.as_ref())))
                .collect()
        };
        let mut report = Fanout::default();
        for (sink, frame) in targets {
            match sink.send(&frame).await {
                Ok(()) => report.sent += 1,
                Err(SinkError) => report.failed += 1,
            }
        }
        report
    }
}

fn frame(uri: &str, meta: Option<&serde_json::Value>) -> String {
    let mut params = serde_json::json!({ "uri": uri });
    if let (Some(meta), Some(object)) = (meta, params.as_object_mut()) {
        object.insert("_meta".to_owned(), meta.clone());
    }
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/resources/updated",
        "params": params,
    })
    .to_string()
}
