//! Serving one `2026-07-28` request, from nothing but the request itself.
//!
//! # What the revision changed, as far as this transport sees it
//!
//! SEP-2575 removed sessions. A request carries its own revision and its own
//! client capabilities in `params._meta`, and the server answers it against a
//! profile resolved from that and nothing else — "Servers MUST NOT infer
//! capabilities from prior requests". So every request here gets a fresh peer,
//! a fresh session id, and a fresh event tail, and none of them outlives the
//! response.
//!
//! # The checks, and the order they run in
//!
//! The order is the specification's, and it decides which error a request with
//! several faults is told about:
//!
//! 1. `_meta` must be readable and carry both required keys: `-32602`, `400`.
//! 2. `MCP-Protocol-Version` must equal the `_meta` version, byte for byte:
//!    `-32020 HeaderMismatch`, `400`. Before the support check, because a
//!    mismatch is a malformed request whatever either value names.
//! 3. The version must be one this build serves statelessly:
//!    `-32022 UnsupportedProtocolVersion`, `400`, echoing what was asked.
//! 4. `Mcp-Method` must equal the body's method, and `Mcp-Name` the body's
//!    `name` or `uri` where the method has one (SEP-2243): `-32020`, `400`.
//! 5. The method must exist at that revision: `-32601`, `404`.
//!
//! Every refusal carries the request's own id. A client with requests in
//! flight matches answers by id, and an error it cannot match is an error it
//! cannot act on.
//!
//! # Why the response is decided after the handler starts
//!
//! A request that reports progress has to be answered with an event stream,
//! because that is the only place the notifications can go — and a request
//! whose handler fails with `-32021` has to be answered with a `400`, which a
//! stream that has already sent `200` can no longer say. So the handler runs
//! first, and the response takes whichever shape the first thing it produces
//! needs: a notification commits to a stream, and a final answer with nothing
//! before it is sent as one JSON document with its own status.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::http::{HeaderMap, StatusCode};
use serde_json::Value;

use crate::adapters::primary::transport::http::config::Streaming;
use crate::application::mcp::param_headers;
use crate::domain::registry::{Primitive, PrimitiveId, PrimitiveKind};
use crate::adapters::primary::transport::http::refusal::Refusal;
use crate::adapters::primary::transport::http::serve::{HttpHandler, KEEP_ALIVE, Tail, TailSink};
use crate::adapters::primary::transport::http::stream::Finishing;
use crate::application::mcp::request_meta::{RequestMeta, RequestMetaError, version_text};
use crate::application::server::context::{PeerSession, RequestContext};
use crate::application::server::dispatch::Dispatcher;
use crate::application::server::{DispatchError, LogGate};
use crate::application::session::frame_sink::{FrameSink, SinkError};
use crate::domain::ir::{IrErrorCode, JsonObject};
use crate::domain::jsonrpc::WireId;
use crate::domain::jsonrpc::reply;
use crate::domain::registry::AuthContext;
use crate::domain::session::{CancellationToken, ProgressToken};
use crate::domain::{
    ProtocolVersion, RequestId, SupportedVersions, client_method_defined_in, handshake_defined_in,
};

/// The headers a stateless request is checked against, read once.
///
/// Owned, because the router's header map does not outlive the call that
/// borrowed it and the checks run inside the handler's future. Surrounding
/// whitespace is trimmed, as HTTP field values allow; the values themselves
/// stay case-sensitive.
#[derive(Debug, Clone, Default)]
pub(super) struct StatelessHeaders {
    protocol: Option<String>,
    method: Option<String>,
    name: Option<String>,
    /// Every `Mcp-Param-*` header, name lowercased.
    params: Vec<(String, String)>,
}

impl StatelessHeaders {
    /// Reads the three from `headers`.
    ///
    /// A value that is not visible ASCII is kept as a replacement string
    /// rather than dropped, so it fails the comparison it would otherwise have
    /// slipped past as "absent".
    pub(super) fn read(headers: &HeaderMap) -> Self {
        let field = |name: &str| {
            headers.get(name).map(|value| {
                value
                    .to_str()
                    .map_or_else(|_| "\u{fffd}".to_owned(), |text| text.trim().to_owned())
            })
        };
        let params = headers
            .iter()
            .filter(|(name, _)| name.as_str().starts_with("mcp-param-"))
            .map(|(name, value)| {
                let value = value
                    .to_str()
                    .map_or_else(|_| "\u{fffd}".to_owned(), |text| text.trim().to_owned());
                (name.as_str().to_owned(), value)
            })
            .collect();
        Self {
            protocol: field("mcp-protocol-version"),
            method: field("mcp-method"),
            name: field("mcp-name"),
            params,
        }
    }

    /// Whether the version header names a revision with no sessions.
    ///
    /// For the router's question about a body with no `_meta` at all: such a
    /// request is still a `2026-07-28` one if its header says so, and is owed
    /// that revision's `-32602` rather than a missing-session refusal.
    pub(super) fn names_stateless_revision(headers: &HeaderMap) -> bool {
        headers
            .get("mcp-protocol-version")
            .and_then(|value| value.to_str().ok())
            .and_then(|text| text.trim().parse::<ProtocolVersion>().ok())
            .is_some_and(|version| !handshake_defined_in(version))
    }
}

/// One stateless request, as the handler copied it out of the router's call.
#[derive(Debug)]
pub(super) struct StatelessRequest {
    pub(super) id: Option<WireId<'static>>,
    pub(super) method: String,
    pub(super) params: JsonObject,
    pub(super) headers: StatelessHeaders,
    pub(super) auth: AuthContext,
    pub(super) streaming: Streaming,
}

/// Why a stateless request was refused before its handler ran.
#[derive(Debug)]
enum Rejection {
    /// A protocol refusal, answered with the error's own code.
    Dispatch(DispatchError),
    /// A header disagreed with the body, or a required one was missing.
    HeaderMismatch(&'static str),
}

impl Rejection {
    fn respond(&self, id: Option<&WireId<'_>>) -> Result<axum::response::Response, Refusal> {
        match self {
            Self::Dispatch(error) => json_response(&error_document(id, error), status_for(error)),
            Self::HeaderMismatch(header) => {
                let message = format!("the {header} header does not match the request body");
                let document = id.map_or_else(
                    || reply::unidentified_error_reply(IrErrorCode::HEADER_MISMATCH, &message),
                    |id| reply::error_reply(id, IrErrorCode::HEADER_MISMATCH, &message),
                );
                json_response(&document, StatusCode::BAD_REQUEST)
            }
        }
    }
}

/// The HTTP status `2026-07-28` assigns to a refusal.
///
/// The revision gives four refusals a status of their own: an unknown method
/// is `404`, and a malformed `_meta`, an unsupported version and a missing
/// client capability are `400`. Everything else is a well-formed exchange whose
/// answer happens to be an error, and travels as `200`.
pub(super) const fn status_for(error: &DispatchError) -> StatusCode {
    match error {
        DispatchError::UnknownMethod => StatusCode::NOT_FOUND,
        DispatchError::MalformedRequestMeta
        | DispatchError::UnsupportedVersion { .. }
        | DispatchError::MissingClientCapability { .. } => StatusCode::BAD_REQUEST,
        _ => StatusCode::OK,
    }
}

/// The JSON-RPC error document for `error`, with the `data` its code requires.
pub(super) fn error_document(id: Option<&WireId<'_>>, error: &DispatchError) -> Value {
    let (code, message) = error.as_wire();
    let data = match error {
        DispatchError::UnsupportedVersion {
            requested,
            supported,
        } => Some(serde_json::json!({ "requested": requested, "supported": supported })),
        // A `ClientCapabilities` object keyed by what is missing — not an array
        // of names. The schema types `requiredCapabilities` as the same object
        // a client declares, so a client can merge it into its next request.
        DispatchError::MissingClientCapability { required } => {
            let capabilities: serde_json::Map<String, Value> = required
                .iter()
                .map(|name| ((*name).to_owned(), Value::Object(serde_json::Map::new())))
                .collect();
            Some(serde_json::json!({ "requiredCapabilities": capabilities }))
        }
        _ => None,
    };
    match (id, data.and_then(|data| JsonObject::from_value(data).ok())) {
        (Some(id), Some(data)) => reply::error_reply_with_data(id, code, &message, &data),
        (Some(id), None) => reply::error_reply(id, code, &message),
        (None, _) => reply::unidentified_error_reply(code, &message),
    }
}

/// A JSON response with `status`.
pub(super) fn json_response(
    document: &Value,
    status: StatusCode,
) -> Result<axum::response::Response, Refusal> {
    let encoded = serde_json::to_vec(document)
        .map_err(|error| Refusal::Handler(format!("the reply did not serialise: {error}")))?;
    axum::response::Response::builder()
        .status(status)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(encoded))
        .map_err(|error| Refusal::Handler(format!("the reply did not build: {error}")))
}

/// What `Mcp-Name` must equal for `method`, read from the body.
///
/// The three methods that address a named thing: a tool or a prompt by its
/// `name`, a resource by its `uri`. Any other method names nothing, and its
/// `Mcp-Name` is not checked.
fn routed_name(method: &str, params: &JsonObject) -> Option<String> {
    let member = match method {
        "tools/call" | "prompts/get" => "name",
        "resources/read" => "uri",
        _ => return None,
    };
    let parsed: Value = serde_json::from_str(params.as_str()).ok()?;
    parsed.get(member)?.as_str().map(ToOwned::to_owned)
}

/// A sink that remembers whether anything was published through it.
///
/// The signal the response shape waits on: the first notification a handler
/// emits is the moment a stream becomes the only possible answer.
#[derive(Debug)]
struct Watched {
    inner: TailSink,
    published: Arc<AtomicBool>,
    wake: Arc<tokio::sync::Notify>,
}

impl FrameSink for Watched {
    fn send(&self, frame: &str) -> impl core::future::Future<Output = Result<(), SinkError>> + Send {
        let sent = self.inner.push(frame);
        if sent.is_ok() {
            self.published.store(true, Ordering::SeqCst);
            self.wake.notify_one();
        }
        core::future::ready(sent)
    }
}

impl HttpHandler {
    /// Serves one stateless request.
    ///
    /// # Errors
    ///
    /// [`Refusal::Handler`] when a response cannot be assembled. Every protocol
    /// refusal is answered on the wire instead.
    pub(super) async fn serve_stateless(
        &self,
        request: StatelessRequest,
    ) -> Result<axum::response::Response, Refusal> {
        let StatelessRequest {
            id,
            method,
            params,
            headers,
            auth,
            streaming,
        } = request;
        let (peer, progress) = match self.stateless_peer(&method, &params, &headers) {
            Ok((peer, progress)) => (peer.with_auth(auth), progress),
            Err(rejection) => return rejection.respond(id.as_ref()),
        };

        // A notification wants no answer. At this revision a client sends
        // none the server acts on, so it is acknowledged and dropped.
        let Some(id) = id else {
            return accepted();
        };

        if method == "subscriptions/listen" {
            return self.open_listen(&id, &params, &peer);
        }

        if streaming == Streaming::Json {
            let context = self.context_for(&peer, &id, progress);
            let (document, status) = answer(&self.dispatcher, &context, &id, &method, &params).await;
            return json_response(&document, status);
        }
        self.answer_streamed(peer, id, progress, method, params).await
    }

    /// Validates the request and builds the peer it is served as.
    fn stateless_peer(
        &self,
        method: &str,
        params: &JsonObject,
        headers: &StatelessHeaders,
    ) -> Result<(PeerSession, Option<ProgressToken>), Rejection> {
        let meta = match RequestMeta::read(params) {
            Ok(meta) => Some(meta),
            // A version the build cannot parse is still a version the client
            // asked for. It is reported as unsupported below — once the header
            // has had its say — rather than as a malformed request.
            Err(RequestMetaError::MalformedVersion) => None,
            Err(_) => return Err(Rejection::Dispatch(DispatchError::MalformedRequestMeta)),
        };
        let requested = version_text(params).unwrap_or_default();

        if headers.protocol.as_deref() != Some(requested.as_str()) {
            return Err(Rejection::HeaderMismatch("MCP-Protocol-Version"));
        }

        let supported = SupportedVersions::all()
            .map_err(|_| Rejection::Dispatch(DispatchError::UnnegotiableVersion))?;
        let Some(meta) = meta.filter(|meta| {
            supported.contains(&meta.version) && !handshake_defined_in(meta.version)
        }) else {
            return Err(Rejection::Dispatch(DispatchError::UnsupportedVersion {
                requested,
                supported: supported
                    .into_iter()
                    .rev()
                    .map(|version| version.as_wire().into_owned())
                    .collect(),
            }));
        };

        if headers.method.as_deref() != Some(method) {
            return Err(Rejection::HeaderMismatch("Mcp-Method"));
        }
        if let Some(expected) = routed_name(method, params)
            && headers.name.as_deref().and_then(param_headers::decode).as_deref() != Some(expected.as_str())
        {
            return Err(Rejection::HeaderMismatch("Mcp-Name"));
        }

        if !client_method_defined_in(meta.version, method) {
            return Err(Rejection::Dispatch(DispatchError::UnknownMethod));
        }
        if method == "tools/call" {
            self.check_param_headers(params, headers)?;
        }

        // A session id of its own, so nothing keyed on the session — the
        // idempotency store above all — can confuse one client's request with
        // another's. Every stateless request used to share the seed's id, and a
        // second client calling the same tool with the same request id was
        // handed the first client's recorded answer.
        let peer = Self::session_for(meta.version, meta.capabilities)
            .peer()
            .with_session(self.sessions.next_id())
            .with_log_gate(LogGate::for_request(meta.log_level));
        let progress = crate::application::mcp::request_meta::progress_token(params);
        Ok((peer, progress))
    }

    /// SEP-2243's `Mcp-Param-*` check, against the named tool's own schema.
    ///
    /// A tool this server does not list has no annotations to enforce; the
    /// dispatcher answers that call as an unknown tool.
    fn check_param_headers(&self, params: &JsonObject, headers: &StatelessHeaders) -> Result<(), Rejection> {
        let parsed: Value = serde_json::from_str(params.as_str()).unwrap_or(Value::Null);
        let Some(name) = parsed.get("name").and_then(Value::as_str) else {
            return Ok(());
        };
        let snapshot = self.dispatcher.server().registry().snapshot();
        let Some(registered) = snapshot.get(PrimitiveKind::Tool, &PrimitiveId::from(name)) else {
            return Ok(());
        };
        let Primitive::Tool(tool) = registered.primitive() else {
            return Ok(());
        };
        let schema: Value = serde_json::from_str(tool.input_schema.as_str()).unwrap_or(Value::Null);
        let arguments = parsed.get("arguments").cloned().unwrap_or(Value::Null);
        param_headers::check(&schema, &arguments, &headers.params)
            .map_err(|_| Rejection::HeaderMismatch("Mcp-Param"))
    }

    fn context_for(
        &self,
        peer: &PeerSession,
        id: &WireId<'static>,
        progress: Option<ProgressToken>,
    ) -> RequestContext {
        RequestContext::for_request_with_state(
            peer,
            RequestId::from_wire(Some(id)),
            CancellationToken::new(),
            progress,
            Arc::clone(self.dispatcher.state()),
        )
    }

    /// Runs the request beside a tail, and answers in whichever shape fits.
    async fn answer_streamed(
        &self,
        peer: PeerSession,
        id: WireId<'static>,
        progress: Option<ProgressToken>,
        method: String,
        params: JsonObject,
    ) -> Result<axum::response::Response, Refusal> {
        let tail = Tail::new(super::serve::TAIL_CAPACITY);
        let published = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(tokio::sync::Notify::new());
        let sink = Arc::new(Watched {
            inner: TailSink { tail: tail.clone() },
            published: Arc::clone(&published),
            wake: Arc::clone(&wake),
        });
        let peer = peer.with_sink(sink);
        let context = self.context_for(&peer, &id, progress);
        let finished = Finishing::new();

        let (sender, mut outcome) = tokio::sync::oneshot::channel();
        let dispatcher = Arc::clone(&self.dispatcher);
        let task_tail = tail.clone();
        let task_finished = finished.clone();
        let task_id = id.clone();
        let task = tokio::spawn(async move {
            let (document, status) =
                answer(&dispatcher, &context, &task_id, &method, &params).await;
            task_tail.push(document.clone());
            task_finished.fire();
            let _ = sender.send((document, status));
        });
        // The response owns the task from here; dropping it does not cancel a
        // handler that has already started work the client asked for.
        let _task = task;

        tokio::select! {
            biased;
            answered = &mut outcome => {
                let Ok((document, status)) = answered else {
                    return Err(Refusal::Handler("the handler ended without answering".to_owned()));
                };
                if !published.load(Ordering::SeqCst) {
                    return json_response(&document, status);
                }
                tail.plan(None)
                    .into_draining_response(KEEP_ALIVE)
                    .map_err(|error| Refusal::Handler(error.to_string()))
            }
            () = wake.notified() => tail
                .plan(None)
                .into_finishable_response(finished, KEEP_ALIVE)
                .map_err(|error| Refusal::Handler(error.to_string())),
        }
    }
}

/// Runs the request to its final document and that document's status.
async fn answer(
    dispatcher: &Dispatcher,
    context: &RequestContext,
    id: &WireId<'static>,
    method: &str,
    params: &JsonObject,
) -> (Value, StatusCode) {
    match dispatcher.dispatch(context, method, params).await {
        Ok(result) => {
            let document = reply::result_reply(id, &result).unwrap_or_else(|_| {
                reply::error_reply(
                    id,
                    IrErrorCode::InternalError,
                    "the dispatcher's result has no JSON form",
                )
            });
            (document, StatusCode::OK)
        }
        Err(error) => (error_document(Some(id), &error), status_for(&error)),
    }
}

/// `202 Accepted`, for a message that wants no answer.
fn accepted() -> Result<axum::response::Response, Refusal> {
    axum::response::Response::builder()
        .status(StatusCode::ACCEPTED)
        .body(axum::body::Body::empty())
        .map_err(|_| Refusal::Handler("the acknowledgement did not build".to_owned()))
}

#[cfg(test)]
mod tests;
