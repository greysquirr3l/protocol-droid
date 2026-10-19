//! A [`Server`](crate::application::server::Server) answering over the
//! `2024-11-05` HTTP+SSE transport.
//!
//! [`LegacyRouter`](super::LegacyRouter) moves frames; this is what decides
//! what they mean. It is the legacy counterpart of the Streamable HTTP
//! handler and shares its dispatcher, so a tool registered on a server is the
//! same tool on either transport.
//!
//! ## One session, one stream
//!
//! A legacy session is a stream the client opened with `GET`. The dispatcher
//! keeps the negotiated peer beside that stream's tail, so a notification a
//! handler publishes (progress, a log record, `list_changed`) and a request it
//! makes of the client (sampling, elicitation) both go out on the stream the
//! client is already reading.
//!
//! ## Why requests are answered on the stream, not by the `POST`
//!
//! The transport answers every `POST` `202`. A request that waits on the
//! client, like a tool asking a question, would hold that `POST` open until the
//! client answered on a second `POST`, and a client that serialises its own
//! sends would never get to send the answer. So a request runs in a task and
//! its reply is written to the stream when it is ready; the `POST` is released
//! at once.

use core::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;

use crate::adapters::primary::transport::http::Refusal;
use crate::adapters::primary::transport::http::documents::{
    answer_of, id_as_i64, params_of, refusal_document, refusal_document_from_refusal,
    result_document,
};
use crate::adapters::primary::transport::http::{ExchangeTable, HttpExchange};
use crate::adapters::primary::transport::legacy_sse::{LegacyDispatcher, LegacyFrame, LegacyTail};
use crate::application::mcp::request_meta::progress_token;
use crate::application::server::context::{PeerSession, RequestContext};
use crate::application::server::dispatch::Dispatcher;
use crate::application::server::{ListKind, ListSubscription};
use crate::application::session::{DynFrameSink, FrameSink, SinkError};
use crate::domain::feature::resolve;
use crate::domain::ir::IrErrorCode;
use crate::domain::jsonrpc::reply;
use crate::domain::jsonrpc::{Frame, WireId, decode};
use crate::domain::registry::AuthContext;
use crate::domain::session::{CancellationToken, SessionId};
use crate::domain::{
    CapabilitySet, McpSessionId, Negotiation, ProtocolVersion, RequestId, TransportKind,
};

/// Writes a handler's frames to the session's stream.
#[derive(Debug, Clone)]
struct StreamSink {
    tail: LegacyTail,
}

impl FrameSink for StreamSink {
    /// # Errors
    ///
    /// [`SinkError`] when the frame is not JSON: the stream carries parsed
    /// messages, and a string that does not parse has nothing to send.
    fn send(&self, frame: &str) -> impl Future<Output = Result<(), SinkError>> + Send {
        let outcome = serde_json::from_str::<Value>(frame)
            .map(|value| {
                self.tail.push(LegacyFrame::Message(value));
            })
            .map_err(|_| SinkError);
        core::future::ready(outcome)
    }
}

/// What the dispatcher keeps for one legacy session.
#[derive(Debug)]
pub struct ServerSession {
    live: Mutex<Live>,
    tail: LegacyTail,
    exchange: ExchangeTable,
}

#[derive(Debug)]
struct Live {
    peer: PeerSession,
    /// Whether `initialize` has completed.
    handshaken: bool,
    /// Ends with the session, so the fan-out stops writing to a stream nobody
    /// reads.
    _lists: Option<ListSubscription>,
}

impl ServerSession {
    fn live(&self) -> std::sync::MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The sink a session's handlers publish through.
fn sink_for(tail: &LegacyTail) -> Arc<dyn DynFrameSink> {
    Arc::new(StreamSink { tail: tail.clone() })
}

/// The exchange a session's handlers ask the client questions through.
///
/// The table is the session's, so an answer posted later finds the request it
/// answers.
fn exchange_for(table: &ExchangeTable, tail: &LegacyTail) -> Arc<HttpExchange> {
    let tail = tail.clone();
    Arc::new(HttpExchange::publishing(table.clone(), move |request| {
        tail.push(LegacyFrame::Message(request));
    }))
}

/// Serves a [`Server`](crate::application::server::Server) to legacy HTTP+SSE
/// clients.
#[derive(Debug, Clone)]
pub struct ServerDispatcher {
    dispatcher: Arc<Dispatcher>,
    ceiling: ProtocolVersion,
}

impl ServerDispatcher {
    /// A dispatcher over `dispatcher`, gating a session with `ceiling` until it
    /// has negotiated.
    #[must_use]
    pub const fn new(dispatcher: Arc<Dispatcher>, ceiling: ProtocolVersion) -> Self {
        Self {
            dispatcher,
            ceiling,
        }
    }

    /// The peer a session has before `initialize` replaces it.
    fn seed(
        &self,
        id: SessionId,
        exchange: Arc<HttpExchange>,
        sink: Arc<dyn DynFrameSink>,
    ) -> PeerSession {
        let profile = resolve(Negotiation {
            version: self.ceiling,
            client_caps: CapabilitySet::empty(),
            server_caps: CapabilitySet::all_for(self.ceiling),
            transport: TransportKind::LegacySse,
        });
        PeerSession::new(
            self.ceiling,
            profile,
            id,
            TransportKind::LegacySse,
            exchange,
            sink,
        )
    }

    /// Runs `initialize`, and keeps the peer it negotiates.
    fn handshake(
        &self,
        auth: &AuthContext,
        id: McpSessionId,
        session: &ServerSession,
        request: &WireId<'_>,
        params: &crate::domain::ir::JsonObject,
    ) -> Value {
        let peer = session.live().peer.clone().with_auth(auth.clone());
        let context = RequestContext::for_request_with_state(
            &peer,
            RequestId::from_wire(Some(request)),
            CancellationToken::new(),
            None,
            Arc::clone(self.dispatcher.state()),
        );
        match self.dispatcher.handshake(&context, params) {
            Ok((result, negotiated)) => {
                let sink = sink_for(&session.tail);
                // The principal is the request's, not the session's: a token can
                // be refreshed or withdrawn between two `POST`s.
                let negotiated = negotiated
                    .without_auth()
                    .with_sink(Arc::clone(&sink))
                    .with_exchange(exchange_for(&session.exchange, &session.tail))
                    .with_session(id.session());
                let lists = self.dispatcher.server().handle().subscribe(
                    &sink,
                    negotiated.profile(),
                    &ListKind::ALL,
                    None,
                );
                *session.live() = Live {
                    peer: negotiated,
                    handshaken: true,
                    _lists: Some(lists),
                };
                result_document(Some(request), &result).unwrap_or_else(|refusal| {
                    refusal_document_from_refusal(Some(request), &refusal)
                })
            }
            Err(error) => refusal_document(Some(request), &error),
        }
    }
}

impl LegacyDispatcher for ServerDispatcher {
    type Session = ServerSession;

    fn open(&self, id: SessionId, tail: &LegacyTail) -> ServerSession {
        let table = ExchangeTable::readable();
        let peer = self.seed(id, exchange_for(&table, tail), sink_for(tail));
        ServerSession {
            live: Mutex::new(Live {
                peer,
                handshaken: false,
                _lists: None,
            }),
            tail: tail.clone(),
            exchange: table,
        }
    }

    fn dispatch(
        &self,
        auth: &AuthContext,
        id: &McpSessionId,
        session: &ServerSession,
        frame: &Value,
    ) -> impl Future<Output = Result<Vec<Value>, Refusal>> + Send {
        let served = serde_json::to_vec(frame)
            .map_err(|error| malformed(&error))
            .and_then(|body| self.serve(auth, *id, session, &body));
        core::future::ready(served)
    }
}

fn malformed(error: &serde_json::Error) -> Refusal {
    Refusal::MalformedBody {
        reason: error.to_string(),
    }
}

impl ServerDispatcher {
    /// Handles one decoded body.
    fn serve(
        &self,
        auth: &AuthContext,
        id: McpSessionId,
        session: &ServerSession,
        body: &[u8],
    ) -> Result<Vec<Value>, Refusal> {
        let decoded = decode(body).map_err(|error| Refusal::MalformedBody {
            reason: error.to_string(),
        })?;
        let Frame::Single(envelope) = decoded else {
            return Err(Refusal::MalformedBody {
                reason: "a legacy HTTP+SSE message carries one message".to_owned(),
            });
        };
        let request = envelope.id().map(WireId::to_owned);

        // An answer to a request this server made: an id and no method.
        let Some(method) = envelope.method_str() else {
            if let Some(request) = request.as_ref()
                && let Some(numeric) = id_as_i64(request)
            {
                session.exchange.resolve(numeric, answer_of(body));
            }
            return Ok(Vec::new());
        };
        let params = params_of(&envelope)?;

        if method == "initialize" {
            let Some(request) = request.as_ref() else {
                return Ok(Vec::new());
            };
            return Ok(vec![self.handshake(auth, id, session, request, &params)]);
        }

        let (peer, handshaken) = {
            let live = session.live();
            (live.peer.clone().with_auth(auth.clone()), live.handshaken)
        };
        if !handshaken && method != "ping" {
            // Nothing is negotiated, so no gate can say what this session may do.
            return Ok(request
                .as_ref()
                .map(|request| {
                    reply::error_reply(
                        request,
                        IrErrorCode::InvalidRequest,
                        "the session has not been initialized",
                    )
                })
                .into_iter()
                .collect());
        }

        let progress = progress_token(&params);
        let method = method.to_owned();
        let dispatcher = Arc::clone(&self.dispatcher);
        let tail = session.tail.clone();
        let notification = request.is_none();
        tokio::spawn(async move {
            let context = RequestContext::for_request_with_state(
                &peer,
                RequestId::from_wire(request.as_ref()),
                CancellationToken::new(),
                progress,
                Arc::clone(dispatcher.state()),
            );
            let outcome = dispatcher.dispatch(&context, &method, &params).await;
            if notification {
                return;
            }
            let document = match outcome {
                Ok(result) => {
                    result_document(request.as_ref(), &result).unwrap_or_else(|refusal| {
                        refusal_document_from_refusal(request.as_ref(), &refusal)
                    })
                }
                Err(error) => refusal_document(request.as_ref(), &error),
            };
            tail.push(LegacyFrame::Message(document));
        });
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests;
