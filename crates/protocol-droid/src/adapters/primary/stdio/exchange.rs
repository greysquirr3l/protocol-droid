//! Server-initiated requests over stdio: `sampling/createMessage`,
//! `elicitation/create`, `roots/list`.
//!
//! # The shape of the problem
//!
//! A tool that asks the client to sample writes a request to stdout and waits
//! for the answer — which arrives on stdin, the same stream the serve loop is
//! reading requests from. So the loop cannot simply await the tool: nothing
//! would read the answer, and the tool would wait forever. The loop keeps
//! reading while a handler runs (see `ServeLoop::run_handler`), and a frame that
//! answers one of this table's requests is handed here.
//!
//! Requests the client sends meanwhile are queued, not served concurrently:
//! replies on this transport go out in the order requests arrived, which is
//! what every existing client of this loop has been able to rely on.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::AsyncWrite;
use tokio::sync::oneshot;

use super::StdioSink;
use crate::application::server::context::{DynExchange, ExchangeError};
use crate::domain::ir::JsonObject;

/// What a waiting handler is handed: the result, or why there is none.
type Answer = Result<JsonObject, ExchangeError>;

/// The requests this server has sent and not yet heard back about.
pub(super) struct StdioExchange<W> {
    sink: Arc<StdioSink<W>>,
    pending: Mutex<HashMap<i64, oneshot::Sender<Answer>>>,
    next: AtomicI64,
    /// Set when the peer has closed stdin: nothing can answer any more, so a
    /// new request fails at once instead of waiting on a stream that is gone.
    closed: AtomicBool,
}

impl<W> StdioExchange<W> {
    pub(super) fn new(sink: Arc<StdioSink<W>>) -> Self {
        Self {
            sink,
            pending: Mutex::new(HashMap::new()),
            next: AtomicI64::new(1),
            closed: AtomicBool::new(false),
        }
    }

    /// Delivers the peer's answer to the request numbered `id`.
    ///
    /// An answer to a request this table never made, or already answered, is
    /// dropped: there is nobody to give it to, and JSON-RPC asks a receiver to
    /// discard a response it cannot correlate.
    pub(super) fn resolve(&self, id: i64, answer: Answer) {
        let waiter = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
        if let Some(waiter) = waiter {
            let _ = waiter.send(answer);
        }
    }

    /// Fails every outstanding request, for a peer that has gone away.
    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let waiting: Vec<_> = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
            .collect();
        for (_, waiter) in waiting {
            let _ = waiter.send(Err(ExchangeError::new("the peer closed its input")));
        }
    }
}

/// Reads a peer's reply document as the answer a handler is waiting for.
///
/// An `error` member is the peer's refusal, kept in its own words; a `result`
/// that is not an object is not an answer this crate can hand a handler.
pub(super) fn answer_of(reply: &serde_json::Value) -> Answer {
    if let Some(failure) = reply.get("error") {
        return Err(ExchangeError::new(failure.to_string()));
    }
    reply.get("result").map_or_else(
        || {
            Err(ExchangeError::new(
                "the answer carried neither a result nor an error",
            ))
        },
        |result| {
            JsonObject::from_value(result.clone())
                .map_err(|error| ExchangeError::new(error.to_string()))
        },
    )
}

impl<W> DynExchange for StdioExchange<W>
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    /// Writes the request and waits for the loop to hand back its answer.
    fn exchange_boxed<'a>(
        &'a self,
        method: &'a str,
        params: &'a JsonObject,
    ) -> core::pin::Pin<Box<dyn Future<Output = Answer> + Send + 'a>> {
        Box::pin(async move {
            if self.closed.load(Ordering::SeqCst) {
                return Err(ExchangeError::new("the peer closed its input"));
            }
            let id = self.next.fetch_add(1, Ordering::Relaxed);
            let (sender, receiver) = oneshot::channel();
            self.pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(id, sender);
            let params: serde_json::Value =
                serde_json::from_str(params.as_str()).unwrap_or(serde_json::Value::Null);
            let request = serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            })
            .to_string();
            if self.sink.send(&request).await.is_err() {
                self.pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&id);
                return Err(ExchangeError::new("the request could not be written"));
            }
            receiver
                .await
                .unwrap_or_else(|_| Err(ExchangeError::new("the request was abandoned")))
        })
    }
}

#[cfg(test)]
mod tests;
