//! HTTP serving for the A2A JSON-RPC endpoint.
//!
//! # What this is
//!
//! An A2A server exposes a JSON-RPC endpoint on the
//! URL the agent card advertises. The endpoint takes
//! a JSON-RPC 2.0 request, dispatches to the
//! consumer's [`A2aRequestHandler`], and answers
//! with a JSON-RPC 2.0 response.
//!
//! ```text
//! POST {agent-card-advertised-url}
//! Content-Type: application/json
//!
//! {"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{...}}
//! ```
//!
//! The serving adapter owns:
//! - The HTTP route (POST only; GET and DELETE
//!   are not part of the A2A wire contract).
//! - The JSON-RPC envelope decode (jsonrpc, id,
//!   method, params).
//! - The method name → [`A2aMethod`] lookup via
//!   [`A2aMethodTable`](crate::adapters::secondary::a2a::A2aMethodTable).
//! - The JSON-RPC response shape (result on
//!   success, error with the right code on
//!   failure).
//!
//! The consumer owns:
//! - The per-method logic (the `dispatch` impl).
//! - The per-revision param decoding (against the
//!   negotiated [`A2aVersion`]).
//!
//! # Why the consumer does not get typed params
//!
//! The single-dispatch `A2aRequestHandler` trait
//! receives the raw `serde_json::Value` params. A
//! future commit can introduce per-method typed
//! wrappers (a `SendMessageHandler` trait that
//! decodes to a `SendMessageRequest`), but
//! adding the wrapper now would lock the wire
//! shape of all 11 methods at the type level
//! before the IR types are stable. The raw-params
//! shape lets a consumer implement the handler
//! today, and the typed wrappers slot in
//! alongside it as a separate layer.
//!
//! # Feature gate
//!
//! `#[cfg(all(feature = "a2a", feature = "http"))]`.
//! The endpoint needs axum, the A2A value
//! objects, and the JSON-RPC dispatch from the
//! `A2aMethodTable`. A consumer who turns on
//! `a2a` but not `http` does not compile this
//! module.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;

use crate::adapters::secondary::a2a::A2aMethodTable;
use crate::application::a2a::handler::{A2aError, A2aRequestContext, A2aRequestHandler};
use crate::application::jsonrpc::MethodTable;
use crate::domain::a2a::version::A2aVersion;

/// Returns an axum [`Router`] that serves the A2A
/// JSON-RPC endpoint on `path`.
///
/// The router is `POST` only. A `GET` to the
/// endpoint returns axum's `405 Method Not
/// Allowed`; the A2A spec does not define any
/// read methods on the JSON-RPC endpoint (the
/// well-known path, served by
/// [`serve_well_known`](super::well_known_serve::serve_well_known),
/// is the read surface).
///
/// The path is a single static string — A2A does
/// not support per-tenant paths on the JSON-RPC
/// endpoint itself; tenants are a property of the
/// URL the agent card advertises.
///
/// # Example
///
/// ```no_run
/// use axum::Router;
/// use protocol_droid::adapters::primary::a2a::jsonrpc_serve::serve_jsonrpc;
/// use protocol_droid::application::a2a::A2aRequestHandler;
///
/// # fn make_handler() -> std::sync::Arc<dyn A2aRequestHandler> {
/// #     unimplemented!()
/// # }
/// let app: Router = Router::new().merge(serve_jsonrpc(
///     "/a2a",
///     make_handler(),
///     protocol_droid::domain::a2a::version::A2aVersion::V1_0,
/// ));
/// ```
pub fn serve_jsonrpc<H>(path: &str, handler: Arc<H>, protocol_version: A2aVersion) -> Router
where
    H: A2aRequestHandler + ?Sized + 'static,
{
    let state = JsonRpcState {
        handler,
        protocol_version,
    };
    Router::new()
        .route(path, post(handle_request::<H>))
        .with_state(Arc::new(state))
}

struct JsonRpcState<H: A2aRequestHandler + ?Sized + 'static> {
    handler: Arc<H>,
    protocol_version: A2aVersion,
}

async fn handle_request<H>(
    State(state): State<Arc<JsonRpcState<H>>>,
    request: Request<Body>,
) -> Response
where
    H: A2aRequestHandler + ?Sized + 'static,
{
    // Content-Type check. A2A v1.0 says the
    // request must be `application/json`. A
    // request with any other content type is
    // `-32000` on the wire.
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let Some(content_type) = content_type else {
        return error_response(
            None,
            A2aError::ContentTypeNotSupported {
                received: String::new(),
            },
        );
    };
    if !content_type.starts_with("application/json") {
        return error_response(
            None,
            A2aError::ContentTypeNotSupported {
                received: content_type,
            },
        );
    }

    // Body read. A bounded read; an over-limit
    // body is a 413. The serving adapter does
    // not own a request ceiling — that is the
    // caller's router layer. A consumer who
    // wants a hard limit puts a `axum::middleware`
    // on the merged router.
    let Ok(body) = axum::body::to_bytes(request.into_body(), 64 * 1024 * 1024).await else {
        return error_response(
            None,
            A2aError::InternalError {
                message: "failed to read request body".to_owned(),
            },
        );
    };

    // JSON-RPC envelope decode. The envelope is
    // a single JSON object with `jsonrpc`, `id`,
    // `method`, and `params`. A malformed
    // envelope is a 400 (the wire-level
    // transport error, not a JSON-RPC error —
    // we have no id to associate an error
    // response with).
    let envelope: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("invalid JSON-RPC envelope: {error}"),
            )
                .into_response();
        }
    };
    let Some(envelope_obj) = envelope.as_object() else {
        return (
            StatusCode::BAD_REQUEST,
            "JSON-RPC envelope must be an object",
        )
            .into_response();
    };
    let id = envelope_obj.get("id").cloned();
    let method_name = match envelope_obj
        .get("method")
        .and_then(serde_json::Value::as_str)
    {
        Some(m) => m.to_owned(),
        None => {
            return error_response(
                id,
                A2aError::InvalidParams {
                    reason: "missing or non-string `method`".to_owned(),
                },
            );
        }
    };
    let params = envelope_obj.get("params").cloned();

    // Method lookup. An unknown name is
    // `-32601`. The 0.3.x slash-separated form
    // (`message/send`) is *not* reachable from
    // here — the bridge (T28) is where the form
    // mismatch is resolved.
    let Some(method) = A2aMethodTable::TABLE.lookup(&method_name) else {
        return error_response(id, A2aError::MethodNotFound);
    };

    // Handler invocation. The handler receives
    // the routed method, the raw params, and
    // the per-request context.
    let context = A2aRequestContext {
        protocol_version: state.protocol_version,
    };
    let result = state.handler.dispatch(method, params, &context).await;

    match result {
        Ok(value) => success_response(id, value),
        Err(error) => error_response(id, error),
    }
}

fn success_response(id: Option<serde_json::Value>, result: serde_json::Value) -> Response {
    let envelope = serde_json::json!({
        "jsonrpc": "2.0",
        "id": &id,
        "result": &result,
    });
    drop((id, result));
    json_response(StatusCode::OK, envelope)
}

fn error_response(id: Option<serde_json::Value>, error: A2aError) -> Response {
    let envelope = serde_json::json!({
        "jsonrpc": "2.0",
        "id": &id,
        "error": {
            "code": error.code(),
            "message": error.message(),
        },
    });
    drop((id, error));
    json_response(StatusCode::OK, envelope)
}

fn json_response(status: StatusCode, body: serde_json::Value) -> Response {
    let Ok(bytes) = serde_json::to_vec(&body) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to serialise response",
        )
            .into_response();
    };
    drop(body);
    (status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response()
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::serve_jsonrpc;
    use crate::adapters::secondary::a2a::A2aMethod;
    use crate::application::a2a::handler::{A2aError, A2aRequestContext, A2aRequestHandler};
    use crate::domain::a2a::version::A2aVersion;

    type TestError = Box<dyn std::error::Error>;
    type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

    /// A handler that returns the method name as
    /// the result. Used to test the dispatch
    /// path; a real consumer would do something
    /// meaningful per method.
    struct EchoHandler;

    impl A2aRequestHandler for EchoHandler {
        fn dispatch(
            &self,
            method: A2aMethod,
            _params: Option<serde_json::Value>,
            _context: &A2aRequestContext,
        ) -> BoxFuture<'_, Result<serde_json::Value, A2aError>> {
            Box::pin(async move { Ok(serde_json::json!({ "method": method.as_str() })) })
        }
    }

    /// A handler that always returns
    /// `MethodNotImplemented`. Used to test the
    /// "consumer did not implement" path.
    struct StubHandler;

    impl A2aRequestHandler for StubHandler {
        fn dispatch(
            &self,
            method: A2aMethod,
            _params: Option<serde_json::Value>,
            _context: &A2aRequestContext,
        ) -> BoxFuture<'_, Result<serde_json::Value, A2aError>> {
            Box::pin(async move { Err(A2aError::MethodNotImplemented { method }) })
        }
    }

    /// A handler that returns an internal error
    /// for one method and the result for the
    /// other. Used to test the error mapping
    /// path.
    struct FlakeyHandler;

    impl A2aRequestHandler for FlakeyHandler {
        fn dispatch(
            &self,
            method: A2aMethod,
            _params: Option<serde_json::Value>,
            _context: &A2aRequestContext,
        ) -> BoxFuture<'_, Result<serde_json::Value, A2aError>> {
            Box::pin(async move {
                if method == A2aMethod::GetTask {
                    Ok(serde_json::json!({ "task": "ok" }))
                } else {
                    Err(A2aError::InternalError {
                        message: "flakey".to_owned(),
                    })
                }
            })
        }
    }

    fn post(body: &str) -> Result<Request<Body>, TestError> {
        Request::builder()
            .method("POST")
            .uri("/a2a")
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })
    }

    // --- happy path ---------------------------------------------------

    #[tokio::test]
    async fn post_send_message_dispatches_to_handler() -> Result<(), TestError> {
        let router = serve_jsonrpc(
            "/a2a",
            Arc::new(EchoHandler) as Arc<dyn A2aRequestHandler>,
            A2aVersion::V1_0,
        );
        let response = router
            .oneshot(post(
                r#"{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{}}"#,
            )?)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let document: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let obj = document
            .as_object()
            .ok_or("the response must be a JSON object")?;
        assert_eq!(
            obj.get("jsonrpc").and_then(serde_json::Value::as_str),
            Some("2.0")
        );
        assert_eq!(obj.get("id").and_then(serde_json::Value::as_i64), Some(1));
        let result = obj
            .get("result")
            .and_then(serde_json::Value::as_object)
            .ok_or("`result` must be an object")?;
        assert_eq!(
            result.get("method").and_then(serde_json::Value::as_str),
            Some("SendMessage")
        );
        Ok(())
    }

    // --- method not found --------------------------------------------

    #[tokio::test]
    async fn unknown_method_returns_minus_32601() -> Result<(), TestError> {
        let router = serve_jsonrpc(
            "/a2a",
            Arc::new(EchoHandler) as Arc<dyn A2aRequestHandler>,
            A2aVersion::V1_0,
        );
        let response = router
            .oneshot(post(
                r#"{"jsonrpc":"2.0","id":2,"method":"NotARealMethod","params":{}}"#,
            )?)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let document: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let error = document
            .get("error")
            .and_then(serde_json::Value::as_object)
            .ok_or("`error` must be a JSON object")?;
        assert_eq!(
            error.get("code").and_then(serde_json::Value::as_i64),
            Some(-32601)
        );
        Ok(())
    }

    // --- method not implemented --------------------------------------

    #[tokio::test]
    async fn method_not_implemented_returns_minus_32601() -> Result<(), TestError> {
        // The method is in the A2A vocabulary,
        // but the consumer's handler returns
        // `MethodNotImplemented`. The wire
        // answer is the same `-32601` code as
        // an unknown method; the distinction is
        // for the consumer's logs.
        let router = serve_jsonrpc(
            "/a2a",
            Arc::new(StubHandler) as Arc<dyn A2aRequestHandler>,
            A2aVersion::V1_0,
        );
        let response = router
            .oneshot(post(
                r#"{"jsonrpc":"2.0","id":3,"method":"SendMessage","params":{}}"#,
            )?)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let document: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let error = document
            .get("error")
            .and_then(serde_json::Value::as_object)
            .ok_or("`error` must be a JSON object")?;
        assert_eq!(
            error.get("code").and_then(serde_json::Value::as_i64),
            Some(-32601)
        );
        Ok(())
    }

    // --- internal error ----------------------------------------------

    #[tokio::test]
    async fn internal_error_returns_minus_32603() -> Result<(), TestError> {
        let router = serve_jsonrpc(
            "/a2a",
            Arc::new(FlakeyHandler) as Arc<dyn A2aRequestHandler>,
            A2aVersion::V1_0,
        );
        let response = router
            .oneshot(post(
                r#"{"jsonrpc":"2.0","id":4,"method":"SendMessage","params":{}}"#,
            )?)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let document: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let error = document
            .get("error")
            .and_then(serde_json::Value::as_object)
            .ok_or("`error` must be a JSON object")?;
        assert_eq!(
            error.get("code").and_then(serde_json::Value::as_i64),
            Some(-32603)
        );
        Ok(())
    }

    // --- content-type -------------------------------------------------

    #[tokio::test]
    async fn non_json_content_type_returns_minus_32000() -> Result<(), TestError> {
        let router = serve_jsonrpc(
            "/a2a",
            Arc::new(EchoHandler) as Arc<dyn A2aRequestHandler>,
            A2aVersion::V1_0,
        );
        let request = Request::builder()
            .method("POST")
            .uri("/a2a")
            .header("content-type", "text/plain")
            .body(Body::from("{}"))
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let response = router
            .oneshot(request)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let document: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let error = document
            .get("error")
            .and_then(serde_json::Value::as_object)
            .ok_or("`error` must be a JSON object")?;
        assert_eq!(
            error.get("code").and_then(serde_json::Value::as_i64),
            Some(-32000)
        );
        Ok(())
    }

    // --- method not allowed ------------------------------------------

    #[tokio::test]
    async fn get_returns_method_not_allowed() -> Result<(), TestError> {
        let router = serve_jsonrpc(
            "/a2a",
            Arc::new(EchoHandler) as Arc<dyn A2aRequestHandler>,
            A2aVersion::V1_0,
        );
        let request = Request::builder()
            .method("GET")
            .uri("/a2a")
            .body(Body::empty())
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        let response = router
            .oneshot(request)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        Ok(())
    }
}
