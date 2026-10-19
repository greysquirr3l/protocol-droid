//! The `2026-07-28` stateless lifecycle on stdio.
//!
//! A request whose `_meta` names its own revision is served from that `_meta`
//! alone, with no `initialize` behind it. The binding is the published stdio
//! page: metadata is carried inline ("There is no header layer"), the server
//! "MUST NOT write JSON-RPC requests to stdout", and a `subscriptions/listen`
//! stream shares the one channel, so its notifications are told apart by
//! `_meta["io.modelcontextprotocol/subscriptionId"]` and it ends when the
//! client sends `notifications/cancelled` naming it.
//! <https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio>
//!
//! The checks run in the order the HTTP transport uses, minus the headers this
//! binding does not have: a malformed `_meta` is `-32602`, a revision this
//! build cannot serve statelessly is `-32022` echoing what was asked, and a
//! method the revision does not define is `-32601`.

use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};

use super::ServeLoop;
use crate::application::jsonrpc::BatchPolicy;
use crate::application::mcp::request_meta::{
    RequestMeta, RequestMetaError, progress_token, version_text,
};
use crate::application::mcp::subscription;
use crate::application::server::{
    DispatchError, DynExchange, ExchangeError, LogGate, PeerSession, RequestContext,
};
use crate::application::session::DynFrameSink;
use crate::domain::ir::{IrErrorCode, JsonObject};
use crate::domain::jsonrpc::WireId;
use crate::domain::jsonrpc::reply::{self, ReplyError};
use crate::domain::session::{CancellationToken, SessionId};
use crate::domain::{
    CapabilitySet, Negotiation, RequestId, SupportedVersions, TransportKind,
    client_method_defined_in, handshake_defined_in, resolve,
};

use super::ServeError;

/// The exchange a stateless peer holds: it refuses, because at this revision
/// a server never sends a request of its own — interactions travel in an
/// `InputRequiredResult` instead.
struct NoServerRequests;

impl DynExchange for NoServerRequests {
    fn exchange_boxed<'a>(
        &'a self,
        _method: &'a str,
        _params: &'a JsonObject,
    ) -> core::pin::Pin<Box<dyn Future<Output = Result<JsonObject, ExchangeError>> + Send + 'a>>
    {
        Box::pin(core::future::ready(Err(ExchangeError::new(
            "a stateless server sends no requests; answer with an InputRequiredResult",
        ))))
    }
}

/// The error document for `error`, with the `data` its code requires.
pub(super) fn error_document(id: &WireId<'_>, error: &DispatchError) -> Value {
    let (code, message) = error.as_wire();
    error.wire_data().map_or_else(
        || reply::error_reply(id, code, &message),
        |data| reply::error_reply_with_data(id, code, &message, &data),
    )
}

/// The key a listen stream is filed under: its request id, as JSON text.
fn listen_key(id: &Value) -> String {
    id.to_string()
}

impl<R, W, DW, P> ServeLoop<R, W, DW, P>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send + 'static,
    DW: AsyncWrite + Unpin + Send,
    P: BatchPolicy + Send + Sync,
{
    /// Serves one self-describing request, or refuses it.
    pub(super) async fn answer_stateless(
        &mut self,
        id: Option<&WireId<'_>>,
        method: &str,
        params: &JsonObject,
    ) -> Result<Option<Value>, ServeError> {
        let peer = match self.stateless_peer(method, params) {
            Ok(peer) => peer,
            Err(error) => return Ok(id.map(|id| error_document(id, &error))),
        };
        if method == "subscriptions/listen" {
            return self.open_listen(id, params, &peer).await;
        }
        let context = RequestContext::for_request_with_state(
            &peer,
            RequestId::from_wire(id),
            CancellationToken::new(),
            progress_token(params),
            Arc::clone(self.dispatcher.state()),
        );
        let (outcome, cancelled) = self.run_handler(&context, method, params).await;
        let Some(id) = id else {
            return Ok(None);
        };
        if cancelled {
            return Ok(None);
        }
        Ok(Some(match outcome {
            Ok(result) => match reply::result_reply(id, &result) {
                Ok(document) => document,
                Err(ReplyError::UnreadableResult) => reply::error_reply(
                    id,
                    IrErrorCode::InternalError,
                    "the dispatcher's result has no JSON form",
                ),
            },
            Err(error) => error_document(id, &error),
        }))
    }

    /// Validates a request's `_meta` and builds the peer it is served as.
    fn stateless_peer(
        &mut self,
        method: &str,
        params: &JsonObject,
    ) -> Result<PeerSession, DispatchError> {
        let meta = match RequestMeta::read(params) {
            Ok(meta) => Some(meta),
            Err(RequestMetaError::MalformedVersion) => None,
            Err(_) => return Err(DispatchError::MalformedRequestMeta),
        };
        let supported = SupportedVersions::all().map_err(|_| DispatchError::UnnegotiableVersion)?;
        let Some(meta) = meta.filter(|meta| {
            supported.contains(&meta.version) && !handshake_defined_in(meta.version)
        }) else {
            return Err(DispatchError::UnsupportedVersion {
                requested: version_text(params).unwrap_or_default(),
                supported: supported
                    .into_iter()
                    .rev()
                    .map(|version| version.as_wire().into_owned())
                    .collect(),
            });
        };
        if !client_method_defined_in(meta.version, method) {
            return Err(DispatchError::UnknownMethod);
        }
        let profile = resolve(Negotiation {
            version: meta.version,
            client_caps: meta.capabilities,
            server_caps: CapabilitySet::all_for(meta.version),
            transport: TransportKind::Stdio,
        });
        // A session id per request, so nothing keyed on the session — the
        // idempotency store above all — can treat two requests as one.
        self.stateless_ids = self.stateless_ids.wrapping_add(1);
        Ok(PeerSession::new(
            meta.version,
            profile,
            SessionId::new(self.stateless_ids),
            TransportKind::Stdio,
            Arc::new(NoServerRequests),
            Arc::clone(&self.sink) as Arc<dyn DynFrameSink>,
        )
        .with_auth(self.peer.auth().clone())
        .with_log_gate(LogGate::for_request(meta.log_level)))
    }

    /// Opens a listen stream on the shared channel and acknowledges it.
    ///
    /// No response is written while the subscription is open: the result of
    /// `subscriptions/listen` is defined as the answer the server sends when
    /// it ends the subscription, and on stdio the client ends it.
    async fn open_listen(
        &mut self,
        id: Option<&WireId<'_>>,
        params: &JsonObject,
        peer: &PeerSession,
    ) -> Result<Option<Value>, ServeError> {
        let Some(id) = id else {
            return Ok(None);
        };
        let parsed: Value = serde_json::from_str(params.as_str()).unwrap_or(Value::Null);
        let Some(filter) = parsed.get("notifications").and_then(Value::as_object) else {
            return Ok(Some(error_document(id, &DispatchError::MalformedParams)));
        };
        let server = Arc::clone(self.dispatcher.server());
        let honoured = subscription::honour(filter, peer, &server);
        let id_value: Value = serde_json::from_str(&id.to_json()).unwrap_or(Value::Null);
        let meta = subscription::tag(&id_value);
        self.sink
            .send(&subscription::acknowledgement(&honoured, &meta).to_string())
            .await
            .map_err(ServeError::Reply)?;
        let sink = Arc::clone(&self.sink) as Arc<dyn DynFrameSink>;
        let guard = subscription::start(&server, &sink, peer, &honoured, &meta);
        self.listens.insert(listen_key(&id_value), guard);
        Ok(None)
    }

    /// Ends the listen stream `notifications/cancelled` names, if it is one.
    ///
    /// Reports whether it was: a cancellation naming anything else is an
    /// ordinary cancellation, which for a request that is not in flight is a
    /// no-op.
    pub(super) fn cancel_listen(&mut self, params: &JsonObject) -> bool {
        let parsed: Value = serde_json::from_str(params.as_str()).unwrap_or(Value::Null);
        let Some(target) = parsed.get("requestId") else {
            return false;
        };
        self.listens.remove(&listen_key(target)).is_some()
    }
}

#[cfg(test)]
mod tests;
