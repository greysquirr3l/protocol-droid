//! HTTP serving for the A2A streaming JSON-RPC
//! endpoint.
//!
//! # What this is
//!
//! An A2A server exposes a streaming JSON-RPC
//! endpoint on the URL the agent card
//! advertises. The endpoint takes a JSON-RPC
//! 2.0 request, dispatches to the consumer's
//! [`A2aStreamingRequestHandler`], and answers
//! with a stream of JSON-RPC 2.0 responses as
//! Server-Sent Events.
//!
//! ```text
//! POST {agent-card-advertised-url}
//! Content-Type: application/json
//!
//! {"jsonrpc":"2.0","id":1,"method":"SendStreamingMessage","params":{...}}
//! ```
//!
//! ```text
//! HTTP/1.1 200 OK
//! Content-Type: text/event-stream
//!
//! data: {"jsonrpc":"2.0","id":1,"result":{"taskId":"t-1",...,"final":false}}
//!
//! data: {"jsonrpc":"2.0","id":1,"result":{"taskId":"t-1",...,"final":true}}
//! ```
//!
//! The serving adapter owns:
//! - The HTTP route (POST only; same as
//!   [`serve_jsonrpc`](super::jsonrpc_serve::serve_jsonrpc)).
//! - The JSON-RPC envelope decode (jsonrpc, id,
//!   method, params).
//! - The method name → [`A2aMethod`] lookup via
//!   [`A2aMethodTable`](crate::adapters::secondary::a2a::A2aMethodTable).
//! - The SSE response shape: each event is
//!   `data: <json-rpc-response>\n\n`.
//!
//! The consumer owns:
//! - The per-method streaming logic (the
//!   `dispatch_stream` impl).
//! - The per-revision param decoding.
//!
//! # Why the request side is the same as
//! `serve_jsonrpc`
//!
//! A streaming method is a regular JSON-RPC
//! method on the wire — the request envelope
//! is the same `{"jsonrpc", "id", "method",
//! "params"}` object. The only difference is
//! the response: a stream of JSON-RPC
//! responses instead of a single one. The
//! serving adapter reuses the envelope
//! decode; the response is what changes.
//!
//! # Why a separate handler trait
//!
//! A non-streaming handler returns
//! `Result<Value, A2aError>`. A streaming
//! handler returns a stream of
//! `Result<StreamingEvent, A2aError>`. The
//! two shapes are different enough that a
//! single trait would force every consumer
//! to implement a boxed-stream `pin`
//! projection they do not need. Separate
//! traits let a consumer pick the shape
//! they want; a server that needs both
//! implements both.
//!
//! # Feature gate
//!
//! `#[cfg(all(feature = "a2a", feature = "http"))]`.

use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use futures_core::Stream;
use futures_util::StreamExt;

use crate::adapters::secondary::a2a::A2aMethodTable;
use crate::application::a2a::handler::{A2aError, A2aRequestContext};
use crate::application::jsonrpc::MethodTable;
use crate::domain::a2a::streaming_events::StreamingEvent;
use crate::domain::a2a::version::A2aVersion;

/// A handler for streaming A2A methods.
///
/// The trait is the streaming counterpart to
/// [`A2aRequestHandler`](crate::application::a2a::A2aRequestHandler):
/// the consumer's `dispatch_stream` returns a
/// stream of [`StreamingEvent`]s, and the
/// serving adapter wraps each event in a
/// JSON-RPC response and emits it as an SSE
/// event.
///
/// # Why the return type is a boxed stream
///
/// The stream's item type is fixed
/// (`StreamingEvent`), but the concrete stream
/// type varies (a `tokio::sync::mpsc::Receiver`,
/// a `futures::stream::iter`, an `async_stream`
/// generator). A boxed stream lets a
/// consumer return any stream type without
/// generic gymnastics. The per-call
/// allocation is one `Box` per dispatch —
/// acceptable on the streaming path, where
/// the cost is dominated by the events
/// themselves.
///
/// # Object safety
///
/// The trait is object-safe: every method
/// takes `&self` and returns owned types
/// (the `Pin<Box<...>>` is the same shape
/// `async-trait` produces). A consumer
/// who wants `Arc<dyn A2aStreamingRequestHandler>`
/// has it; a consumer who wants
/// `Arc<ConcreteHandler>` also has it.
pub trait A2aStreamingRequestHandler: Send + Sync + 'static {
    /// Dispatches a streaming method.
    ///
    /// The handler receives the routed
    /// method, the raw params, and the
    /// per-request context. The return
    /// value is a stream of events; the
    /// serving adapter wraps each event
    /// in a JSON-RPC response and emits
    /// it as an SSE event. The stream
    /// ends when the handler's stream
    /// returns `None`, or when the
    /// client disconnects.
    ///
    /// # Example
    ///
    /// A minimal handler that emits a
    /// single `working` event followed by
    /// a `completed` event for
    /// `SendStreamingMessage`. The full
    /// reference impl is in the
    /// `a2a_server` example.
    ///
    /// ```no_run
    ///     use std::pin::Pin;
    ///     use std::sync::Arc;
    ///
    ///     use futures_core::Stream;
    ///     use futures_util::stream;
    ///
    ///     use protocol_droid::adapters::primary::a2a::streaming_serve::A2aStreamingRequestHandler;
    ///     use protocol_droid::adapters::secondary::a2a::A2aMethod;
    ///     use protocol_droid::application::a2a::{
    ///         A2aError, A2aRequestContext,
    ///     };
    ///     use protocol_droid::domain::a2a::streaming_events::{
    ///         StreamingEvent, TaskStatusUpdateEvent,
    ///     };
    ///     use protocol_droid::domain::a2a::task::{TaskState, TaskStatus};
    ///
    ///     struct EchoAgent;
    ///
    ///     impl A2aStreamingRequestHandler for EchoAgent {
    ///         fn dispatch_stream(
    ///             &self,
    ///             method: A2aMethod,
    ///             _params: Option<serde_json::Value>,
    ///             _context: &A2aRequestContext,
    ///         ) -> Pin<Box<dyn Stream<Item = Result<StreamingEvent, A2aError>> + Send>> {
    ///             let events = match method {
    ///                 A2aMethod::SendStreamingMessage => vec![
    ///                     Ok(StreamingEvent::Status(TaskStatusUpdateEvent {
    ///                         task_id: "t-1".to_owned(),
    ///                         context_id: "c-1".to_owned(),
    ///                         status: TaskStatus {
    ///                             state: TaskState::Working,
    ///                             message: None,
    ///                             timestamp: None,
    ///                         },
    ///                         is_final: false,
    ///                         metadata: std::collections::BTreeMap::new(),
    ///                     })),
    ///                     Ok(StreamingEvent::Status(TaskStatusUpdateEvent {
    ///                         task_id: "t-1".to_owned(),
    ///                         context_id: "c-1".to_owned(),
    ///                         status: TaskStatus {
    ///                             state: TaskState::Completed,
    ///                             message: None,
    ///                             timestamp: None,
    ///                         },
    ///                         is_final: true,
    ///                         metadata: std::collections::BTreeMap::new(),
    ///                     })),
    ///                 ],
    ///                 _ => vec![
    ///                     Err(A2aError::MethodNotImplemented { method }),
    ///                 ],
    ///             };
    ///             Box::pin(stream::iter(events))
    ///         }
    ///     }
    ///
    ///     let _handler: Arc<dyn A2aStreamingRequestHandler> = Arc::new(EchoAgent);
    /// ```
    fn dispatch_stream(
        &self,
        method: crate::adapters::secondary::a2a::A2aMethod,
        params: Option<serde_json::Value>,
        context: &A2aRequestContext,
    ) -> Pin<Box<dyn Stream<Item = Result<StreamingEvent, A2aError>> + Send>>;
}

/// Returns an axum [`Router`] that serves the
/// A2A streaming JSON-RPC endpoint on `path`.
///
/// The router is `POST` only. The response is
/// a `text/event-stream` SSE response where
/// each event is a JSON-RPC 2.0 response
/// carrying a [`StreamingEvent`] in its
/// `result` field.
///
/// # Example
///
/// ```no_run
/// use axum::Router;
/// use protocol_droid::adapters::primary::a2a::streaming_serve::serve_streaming;
/// use protocol_droid::adapters::primary::a2a::streaming_serve::A2aStreamingRequestHandler;
///
/// # fn make_handler() -> std::sync::Arc<dyn A2aStreamingRequestHandler> {
/// #     unimplemented!()
/// # }
/// let app: Router = Router::new().merge(serve_streaming(
///     "/a2a/stream",
///     make_handler(),
///     protocol_droid::domain::a2a::version::A2aVersion::V1_0,
/// ));
/// ```
pub fn serve_streaming<H>(
    path: &str,
    handler: Arc<H>,
    protocol_version: A2aVersion,
) -> Router
where
    H: A2aStreamingRequestHandler + ?Sized + 'static,
{
    let state = StreamingState {
        handler,
        protocol_version,
    };
    Router::new()
        .route(path, post(handle_streaming_request::<H>))
        .with_state(Arc::new(state))
}

struct StreamingState<H: A2aStreamingRequestHandler + ?Sized + 'static> {
    handler: Arc<H>,
    protocol_version: A2aVersion,
}

struct ParsedRequest {
    id: Option<serde_json::Value>,
    method: crate::adapters::secondary::a2a::A2aMethod,
    params: Option<serde_json::Value>,
}

// `Result<ParsedRequest, Response>` is the
// cheap shape: the happy path is the parsed
// envelope, the error path is a 4xx response
// the caller returns. The `Response` type
// is large (axum), and clippy's
// `result_large_err` flags it; the function
// is the one place that has to return both
// shapes, so `#[expect]` is the honest answer.
#[expect(clippy::result_large_err, reason = "this is the one place that returns both shapes")]
async fn parse_envelope(
    request: Request<Body>,
) -> Result<ParsedRequest, Response> {
    // Content-Type check. A non-JSON
    // content type is a 415 — the
    // streaming transport cannot
    // carry an error envelope on the
    // same request, so we refuse
    // the request entirely.
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let Some(content_type) = content_type else {
        return Err((StatusCode::BAD_REQUEST, "missing content-type").into_response());
    };
    if !content_type.starts_with("application/json") {
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!("content-type must be application/json, got {content_type}"),
        )
            .into_response());
    }

    // Body read. Same 64 MiB ceiling
    // as `serve_jsonrpc`.
    let body = axum::body::to_bytes(request.into_body(), 64 * 1024 * 1024)
        .await
        .map_err(|_| {
            (StatusCode::BAD_REQUEST, "failed to read request body").into_response()
        })?;

    // JSON-RPC envelope decode.
    let envelope: serde_json::Value = serde_json::from_slice(&body).map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid JSON-RPC envelope: {error}"),
        )
            .into_response()
    })?;
    let envelope_obj = envelope.as_object().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            "JSON-RPC envelope must be an object",
        )
            .into_response()
    })?;
    let id = envelope_obj.get("id").cloned();
    let method_name = envelope_obj
        .get("method")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "missing or non-string `method`",
            )
                .into_response()
        })?
        .to_owned();
    let params = envelope_obj.get("params").cloned();

    // Method lookup. An unknown name
    // is a 400 — the client cannot
    // meaningfully receive a single
    // error event and then have the
    // stream close.
    let method = A2aMethodTable::TABLE.lookup(&method_name).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            format!("unknown method: {method_name}"),
        )
            .into_response()
    })?;

    Ok(ParsedRequest {
        id,
        method,
        params,
    })
}

async fn handle_streaming_request<H>(
    State(state): State<Arc<StreamingState<H>>>,
    request: Request<Body>,
) -> Response
where
    H: A2aStreamingRequestHandler + ?Sized + 'static,
{
    let ParsedRequest {
        id,
        method,
        params,
    } = match parse_envelope(request).await {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };

    let context = A2aRequestContext {
        protocol_version: state.protocol_version,
    };
    let event_stream = state
        .handler
        .dispatch_stream(method, params, &context);

    // Wrap the event stream in an
    // SSE response. Each event is
    // serialised as a JSON-RPC
    // response and emitted as
    // `data: <json>\n\n`. Errors
    // in the stream become a
    // single error event followed
    // by stream end.
    let id_for_sse = id;
    let sse_stream: Pin<
        Box<dyn Stream<Item = Result<axum::body::Bytes, std::io::Error>> + Send>,
    > = Box::pin(event_stream.map(move |event_result| {
        let envelope = match event_result {
            Ok(event) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": &id_for_sse,
                "result": event,
            }),
            Err(error) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": &id_for_sse,
                "error": {
                    "code": error.code(),
                    "message": error.message(),
                },
            }),
        };
        let line = match serde_json::to_string(&envelope) {
            Ok(s) => format!("data: {s}\n\n"),
            Err(e) => format!("data: {{\"error\":\"failed to serialise: {e}\"}}\n\n"),
        };
        Ok(axum::body::Bytes::from(line))
    }));

    let body = Body::from_stream(sse_stream);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .unwrap_or_else(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, "failed to build response").into_response()
        })
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Arc;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use futures_core::Stream;
    use futures_util::stream;
    use tower::ServiceExt;

    use super::{
        A2aStreamingRequestHandler, serve_streaming,
    };
    use crate::application::a2a::handler::{A2aError, A2aRequestContext};
    use crate::domain::a2a::streaming_events::{
        StreamingEvent, TaskStatusUpdateEvent,
    };
    use crate::domain::a2a::task::{TaskState, TaskStatus};
    use crate::domain::a2a::version::A2aVersion;
    use crate::adapters::secondary::a2a::A2aMethod;

    type TestError = Box<dyn std::error::Error>;
    type TestStream =
        Pin<Box<dyn Stream<Item = Result<StreamingEvent, A2aError>> + Send>>;

    fn sample_status_event(final_flag: bool) -> TaskStatusUpdateEvent {
        let mut event = TaskStatusUpdateEvent::new(
            "t-1",
            "c-1",
            TaskStatus {
                state: TaskState::Working,
                message: None,
                timestamp: None,
            },
        );
        if final_flag {
            event = event.with_final();
        }
        event
    }

    /// A handler that returns a fixed
    /// stream of events. Used to test
    /// the dispatch path; a real
    /// consumer would do something
    /// meaningful per method.
    struct FixedStreamHandler {
        events: Vec<StreamingEvent>,
    }

    impl A2aStreamingRequestHandler for FixedStreamHandler {
        fn dispatch_stream(
            &self,
            _method: A2aMethod,
            _params: Option<serde_json::Value>,
            _context: &A2aRequestContext,
        ) -> TestStream {
            let events: Vec<Result<StreamingEvent, A2aError>> = self
                .events
                .iter()
                .cloned()
                .map(Ok)
                .collect();
            Box::pin(stream::iter(events))
        }
    }

    async fn read_response_body(
        response: axum::response::Response,
    ) -> Result<String, TestError> {
        let body = response.into_body();
        let bytes = axum::body::to_bytes(body, usize::MAX)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        String::from_utf8(bytes.to_vec())
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })
    }

    #[tokio::test]
    async fn streaming_response_emits_events_as_sse() -> Result<(), TestError> {
        let handler: Arc<dyn A2aStreamingRequestHandler> = Arc::new(
            FixedStreamHandler {
                events: vec![
                    StreamingEvent::Status(sample_status_event(false)),
                    StreamingEvent::Status(sample_status_event(true)),
                ],
            },
        );
        let app: Router = serve_streaming(
            "/stream",
            handler,
            A2aVersion::V1_0,
        );
        let request = Request::builder()
            .method("POST")
            .uri("/stream")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","id":1,"method":"SendStreamingMessage","params":{}}"#,
            ))
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let response = app
            .oneshot(request)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("text/event-stream")
        );
        let body = read_response_body(response).await?;
        // Each event is `data: <json>\n\n`.
        let events: Vec<&str> = body.split("\n\n").filter(|s| !s.is_empty()).collect();
        assert_eq!(events.len(), 2, "two events");
        // Every event starts with `data: `.
        for event in &events {
            assert!(
                event.starts_with("data: "),
                "expected SSE `data:` prefix, got {event:?}"
            );
        }
        // The first event is a status event
        // with `final: false`.
        let first = events.first().copied().unwrap_or("");
        assert!(first.contains(r#""final":false"#));
        // The second event is a status
        // event with `final: true`.
        let second = events.get(1).copied().unwrap_or("");
        assert!(second.contains(r#""final":true"#));
        Ok(())
    }

    #[tokio::test]
    async fn streaming_response_uses_the_request_id() -> Result<(), TestError> {
        let handler: Arc<dyn A2aStreamingRequestHandler> = Arc::new(
            FixedStreamHandler {
                events: vec![StreamingEvent::Status(sample_status_event(false))],
            },
        );
        let app: Router = serve_streaming(
            "/stream",
            handler,
            A2aVersion::V1_0,
        );
        let request = Request::builder()
            .method("POST")
            .uri("/stream")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","id":42,"method":"SendStreamingMessage","params":{}}"#,
            ))
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let response = app
            .oneshot(request)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let body = read_response_body(response).await?;
        assert!(body.contains(r#""id":42"#), "the response must carry the request id");
        Ok(())
    }

    #[tokio::test]
    async fn non_json_content_type_returns_415() -> Result<(), TestError> {
        let handler: Arc<dyn A2aStreamingRequestHandler> = Arc::new(
            FixedStreamHandler { events: vec![] },
        );
        let app: Router = serve_streaming(
            "/stream",
            handler,
            A2aVersion::V1_0,
        );
        let request = Request::builder()
            .method("POST")
            .uri("/stream")
            .header("content-type", "text/plain")
            .body(Body::from("hello"))
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let response = app
            .oneshot(request)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        Ok(())
    }

    #[tokio::test]
    async fn unknown_method_returns_400() -> Result<(), TestError> {
        let handler: Arc<dyn A2aStreamingRequestHandler> = Arc::new(
            FixedStreamHandler { events: vec![] },
        );
        let app: Router = serve_streaming(
            "/stream",
            handler,
            A2aVersion::V1_0,
        );
        let request = Request::builder()
            .method("POST")
            .uri("/stream")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","id":1,"method":"NotARealMethod","params":{}}"#,
            ))
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let response = app
            .oneshot(request)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        Ok(())
    }
}
