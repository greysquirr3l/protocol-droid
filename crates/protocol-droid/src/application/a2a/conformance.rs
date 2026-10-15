//! A2A v1.0 conformance vector runner.
//!
//! The spec publishes a set of request/response examples; this
//! module is the loop that drives each one through an
//! [`A2aRequestHandler`] and reports the diff. The handler can
//! be the library's own reference implementation, a third-party
//! server a consumer is auditing, or a fixture for a test.
//!
//! # Vector format
//!
//! A vector is a JSON object with `name`, `request`, and
//! `expected`:
//!
//! ```text
//! {"name": "send-message-happy",
//!  "request": {"jsonrpc": "2.0", "id": 1, "method": "SendMessage",
//!              "params": {"message": {...}}},
//!  "expected": {"result": {...}}}
//! ```
//!
//! `expected` is one of `{"result": <value>}` for a successful
//! response or `{"error": {"code": <i64>, "message_contains": <str>}}`
//! for an error response. The `message_contains` form is the
//! forgiving one: a server is free to add detail, but a runner
//! that asserted the whole message verbatim would couple
//! vectors to a specific implementation's wording.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower::ServiceExt;

use crate::adapters::primary::a2a::jsonrpc_serve::serve_jsonrpc;
use crate::application::a2a::A2aRequestHandler;
use crate::domain::a2a::A2aVersion;

/// One conformance vector, exactly as it appears in a JSONL
/// fixture file. The shape is a JSON object, one per line.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Vector {
    /// A short, human-readable name the runner prints
    /// when reporting a diff. The fixture file is the
    /// authoritative list; an operator reading the
    /// report goes by name to find the failing case.
    pub name: String,
    /// The JSON-RPC envelope to send. Must carry
    /// `jsonrpc: "2.0"`, `id`, `method`, and
    /// `params`. The runner does not synthesise
    /// defaults.
    pub request: Value,
    /// The expected response: either a `result` or
    /// an `error` with the code and a substring the
    /// server's message must contain.
    pub expected: ExpectedResponse,
}

/// What the server is expected to answer. One of
/// `Result` (a successful response) or
/// `Error` (a JSON-RPC error with the code and a
/// substring the message must contain).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ExpectedResponse {
    /// A successful response, carrying the JSON the
    /// server is expected to return. Compared as
    /// `Value::eq` so a server that produces an
    /// equal but differently-ordered object is
    /// still a pass.
    Result {
        /// The JSON value the server is expected to
        /// return. Compared as `Value::eq` so
        /// key-order differences are tolerated.
        result: Value,
    },
    /// An error response. The runner accepts any
    /// message that contains the substring
    /// (case-sensitive) — see the module docs.
    Error {
        /// The code and a substring the message
        /// must contain. See [`ExpectedError`].
        error: ExpectedError,
    },
}

/// The expected error code and a substring the
/// message must contain. The code is exact; the
/// message is a substring match.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExpectedError {
    /// The JSON-RPC error code the server is
    /// expected to return. Compared with `==`.
    pub code: i64,
    /// A substring the server's `message` must
    /// contain. Case-sensitive.
    pub message_contains: String,
}

/// The outcome of running a single vector. A
/// `ConformanceReport` is a `Vec<VectorResult>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VectorResult {
    /// The vector's name, copied through from
    /// the input so a report can be grepped by
    /// the same string an operator used in the
    /// fixture.
    pub name: String,
    /// The observed outcome. A `Pass` is a
    /// short report; a `Fail` carries the
    /// expected and the observed so a
    /// structured consumer can diff without
    /// re-running.
    pub outcome: VectorOutcome,
}

/// What the runner saw when it ran a vector.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VectorOutcome {
    /// The vector's expected and observed responses
    /// matched on every field the runner checks.
    Pass,
    /// The vector's expected and observed responses
    /// did not match. The `expected` and `actual`
    /// carry the same shape, so a structured
    /// consumer can render a diff without re-running
    /// the request.
    Fail {
        /// A short, human-readable summary of the
        /// difference. Useful for an operator
        /// reading a CI log.
        diff: String,
        /// What the runner saw, in the same shape
        /// the fixture uses for `expected`.
        actual: ObservedResponse,
    },
}

/// What the runner actually saw on the wire, in
/// the same shape the fixture uses for
/// `expected`.
///
/// A runner that recorded a 200 but a wrong body
/// would still produce a `Fail`; a runner that
/// recorded a 500 would surface the
/// transport-level error here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ObservedResponse {
    /// The server answered with a `result`.
    Result {
        /// The `result` field of the JSON-RPC envelope.
        result: Value,
    },
    /// The server answered with an error envelope.
    Error {
        /// The error code and message the server returned.
        error: ObservedError,
    },
    /// The transport returned a non-2xx status
    /// with a body the JSON-RPC layer could not
    /// parse, or returned no body at all.
    Transport {
        /// The HTTP status code the transport observed.
        status: u16,
        /// The body the transport observed, or a
        /// description of why it could not be
        /// parsed.
        body: String,
    },
}

/// The shape of an observed error: the code
/// the server returned and the message it
/// carried. Compared against `ExpectedError`
/// with `==` on the code and substring on
/// the message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservedError {
    /// The JSON-RPC error code the server returned.
    pub code: i64,
    /// The message the server returned.
    pub message: String,
}

impl core::fmt::Display for ObservedResponse {
    /// Renders the observed response in a shape a
    /// human reading a CI log can scan. The exact
    /// format is not part of the contract; the
    /// `VectorOutcome::Fail::actual` field is.
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Result { result } => write!(formatter, "Result({result})"),
            Self::Error { error } => {
                write!(formatter, "Error({}, {})", error.code, error.message)
            }
            Self::Transport { status, body } => {
                write!(formatter, "Transport({status}, {body})")
            }
        }
    }
}

/// Runs a list of vectors against a handler, using
/// the in-process `serve_jsonrpc` adapter.
///
/// The handler is the same trait a real router
/// uses, so the vectors exercise the same code
/// path a third-party client would. `async`
/// because `tower::ServiceExt::oneshot` returns
/// a future; the test suite's `#[tokio::test]`
/// drives this directly, and a CLI subcommand
/// that wants the same can wrap it in a
/// `tokio::main` or in a
/// `tokio::runtime::Handle::block_on`.
///
/// # Errors
///
/// None. A runner that returned `Result` for a
/// transport-level error would force the caller
/// to abort on the first network blip; the
/// runner's contract is to produce a `VectorResult`
/// for every input, with the failure mode named
/// in the outcome. An error here would mean
/// the harness itself is broken, not the
/// server under test.
pub async fn run_against_in_process(
    handler: Arc<dyn A2aRequestHandler>,
    protocol_version: A2aVersion,
    vectors: &[Vector],
) -> Vec<VectorResult> {
    let router = serve_jsonrpc("/a2a", handler, protocol_version);
    let mut out = Vec::with_capacity(vectors.len());
    for vector in vectors {
        out.push(run_one(&router, vector).await);
    }
    out
}

/// Runs a single vector through the supplied router.
///
/// Public so a custom harness (one that wants to
/// drive against a real socket rather than
/// `tower::ServiceExt::oneshot`) can reuse the
/// response-shaping logic.
pub async fn run_one(router: &axum::Router, vector: &Vector) -> VectorResult {
    let body_bytes = match serde_json::to_vec(&vector.request) {
        Ok(bytes) => bytes,
        Err(error) => {
            return VectorResult {
                name: vector.name.clone(),
                outcome: VectorOutcome::Fail {
                    diff: format!("the request could not be serialised: {error}"),
                    actual: ObservedResponse::Transport {
                        status: 0,
                        body: format!("encode: {error}"),
                    },
                },
            };
        }
    };
    let request = match Request::builder()
        .method("POST")
        .uri("/a2a")
        .header("content-type", "application/json")
        .body(Body::from(body_bytes))
    {
        Ok(request) => request,
        Err(error) => {
            return VectorResult {
                name: vector.name.clone(),
                outcome: VectorOutcome::Fail {
                    diff: format!("the harness built a request the http layer refused: {error}"),
                    actual: ObservedResponse::Transport {
                        status: 0,
                        body: format!("build: {error}"),
                    },
                },
            };
        }
    };
    let response = match router.clone().oneshot(request).await {
        Ok(response) => response,
        // The router's error type is `Infallible`;
        // a 500 is a `Response`, not an `Err`. The
        // `match never {}` arm is the
        // type-system-asserted "this is impossible",
        // and a lint that says otherwise is reading
        // the future incompatibility wrong.
        Err(never) => match never {},
    };
    let observed = observe_response(response).await;
    compare(vector, &observed)
}

/// Reads a JSON-RPC response body, extracts the
/// envelope, and shapes it for the runner's
/// `ObservedResponse`. Status codes outside
/// the 200 range are recorded as `Transport`.
async fn observe_response(response: axum::response::Response) -> ObservedResponse {
    let status = response.status();
    let body_bytes = match axum::body::to_bytes(response.into_body(), 64 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return ObservedResponse::Transport {
                status: status.as_u16(),
                body: format!("read body: {error}"),
            };
        }
    };
    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();
    if status != StatusCode::OK {
        return ObservedResponse::Transport {
            status: status.as_u16(),
            body: body_text,
        };
    }
    let envelope: Value = match serde_json::from_slice(&body_bytes) {
        Ok(value) => value,
        Err(error) => {
            return ObservedResponse::Transport {
                status: status.as_u16(),
                body: format!("invalid JSON-RPC envelope: {error}; body: {body_text}"),
            };
        }
    };
    let Some(envelope_object) = envelope.as_object() else {
        return ObservedResponse::Transport {
            status: status.as_u16(),
            body: format!("envelope was not a JSON object: {envelope}"),
        };
    };
    envelope_object.get("result").map_or_else(
        || {
            envelope_object.get("error").map_or_else(
                || ObservedResponse::Transport {
                    status: status.as_u16(),
                    body: format!("envelope carried neither result nor error: {envelope}"),
                },
                |error| {
                    let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    ObservedResponse::Error {
                        error: ObservedError { code, message },
                    }
                },
            )
        },
        |result| ObservedResponse::Result {
            result: result.clone(),
        },
    )
}

/// Compares a vector's expected response against
/// what the runner observed. The check is exact
/// on the success path (`Value::eq` after
/// canonicalising) and exact on the code plus
/// substring on the message for the error path.
fn compare(vector: &Vector, observed: &ObservedResponse) -> VectorResult {
    let pass = match (&vector.expected, observed) {
        (ExpectedResponse::Result { result: expected }, ObservedResponse::Result { result }) => {
            expected == result
        }
        (
            ExpectedResponse::Error {
                error:
                    ExpectedError {
                        code,
                        message_contains,
                    },
            },
            ObservedResponse::Error {
                error:
                    ObservedError {
                        code: obs_code,
                        message,
                    },
            },
        ) => code == obs_code && message.contains(message_contains.as_str()),
        _ => false,
    };
    if pass {
        VectorResult {
            name: vector.name.clone(),
            outcome: VectorOutcome::Pass,
        }
    } else {
        VectorResult {
            name: vector.name.clone(),
            outcome: VectorOutcome::Fail {
                diff: format_diff(&vector.expected, observed),
                actual: observed.clone(),
            },
        }
    }
}

/// Renders a short, human-readable summary of the
/// difference between an expected and an observed
/// response. A structured consumer should diff the
/// `actual` field instead of parsing this string,
/// but a CI log without a JSON-aware viewer needs
/// something it can grep.
fn format_diff(expected: &ExpectedResponse, observed: &ObservedResponse) -> String {
    match (expected, observed) {
        (_, ObservedResponse::Transport { status, body }) => {
            format!("the transport answered {status} with body {body}")
        }
        (ExpectedResponse::Error { error: expected }, ObservedResponse::Error { error }) => {
            format!(
                "expected an error with code {} (containing {:?}), got code {} message {}",
                expected.code, expected.message_contains, error.code, error.message
            )
        }
        (ExpectedResponse::Error { error: expected }, ObservedResponse::Result { result }) => {
            format!(
                "expected an error with code {} (containing {:?}), got result {}",
                expected.code, expected.message_contains, result
            )
        }
        (ExpectedResponse::Result { result: expected }, ObservedResponse::Result { result }) => {
            format!("expected result {expected}, got {result}")
        }
        (ExpectedResponse::Result { .. }, ObservedResponse::Error { error }) => {
            format!(
                "expected a result, got an error envelope with code {} message {}",
                error.code, error.message
            )
        }
    }
}

/// Parses a JSONL fixture file's contents into a
/// list of vectors.
///
/// The format is one JSON object per line; a blank
/// line or a line whose first non-whitespace
/// character is `#` is ignored, so fixtures can
/// carry a license header.
///
/// # Errors
///
/// Returns the line number (1-indexed) of the first
/// malformed line. An empty file is not an error;
/// a file with only comments and blank lines is the
/// same as empty. The runner treats an empty list
/// as "all pass" so a CI job that does not have the
/// fixture file can ship with a no-op run.
pub fn parse_jsonl(contents: &str) -> Result<Vec<Vector>, JsonlError> {
    let mut out = Vec::new();
    for (line_number, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let vector: Vector = serde_json::from_str(trimmed).map_err(|error| JsonlError {
            line: line_number + 1,
            message: format!("{error}"),
        })?;
        out.push(vector);
    }
    Ok(out)
}

/// Why a JSONL fixture could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonlError {
    /// The 1-indexed line number of the first
    /// malformed line in the fixture.
    pub line: usize,
    /// The serde error message, formatted with
    /// the standard Display impl.
    pub message: String,
}

impl core::fmt::Display for JsonlError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for JsonlError {}

/// The default protocol version the runner uses
/// when the caller does not specify one. A2A's
/// 1.0.1 is a patch (no schema changes), so 1.0
/// is the right default for vectors that pin the
/// spec's behaviour.
pub const DEFAULT_PROTOCOL_VERSION: A2aVersion = A2aVersion::V1_0;

#[cfg(test)]
mod tests {
    use super::*;

    /// The vector types are reachable from the
    /// library's own test target. Without this
    /// test, the lib's `unused_pub` lint would
    /// flag the public API as unused even though
    /// the integration test in
    /// `tests/a2a_conformance.rs` is the actual
    /// consumer.
    #[test]
    fn the_public_types_are_constructible() {
        let vector = Vector {
            name: "sample".to_owned(),
            request: serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}),
            expected: ExpectedResponse::Result {
                result: serde_json::json!({}),
            },
        };
        let result = VectorResult {
            name: vector.name,
            outcome: VectorOutcome::Pass,
        };
        let _ = ObservedResponse::Result {
            result: serde_json::json!({}),
        };
        let _ = ObservedResponse::Error {
            error: ObservedError {
                code: -32601,
                message: "Method not found".to_owned(),
            },
        };
        let _ = ObservedResponse::Transport {
            status: 500,
            body: "internal".to_owned(),
        };
        assert_eq!(result.name, "sample");
        assert_eq!(DEFAULT_PROTOCOL_VERSION, A2aVersion::V1_0);
    }

    /// `parse_jsonl` round-trips a known-good
    /// fixture, so the lib has direct coverage
    /// independent of the integration test's
    /// fixture file.
    #[test]
    fn parse_jsonl_round_trips_a_known_good_fixture() -> Result<(), JsonlError> {
        let fixture = "{\"name\": \"a\", \"request\": {}, \"expected\": {\"result\": {}}}\n{\"name\": \"b\", \"request\": {}, \"expected\": {\"error\": {\"code\": -1, \"message_contains\": \"x\"}}}\n";
        let vectors = parse_jsonl(fixture)?;
        assert_eq!(vectors.len(), 2);
        assert_eq!(vectors.first().map(|v| v.name.as_str()), Some("a"));
        assert_eq!(vectors.get(1).map(|v| v.name.as_str()), Some("b"));
        Ok(())
    }

    /// `parse_jsonl` returns the line number of
    /// the malformed entry, 1-indexed.
    #[test]
    fn parse_jsonl_names_the_offending_line() -> Result<(), String> {
        let fixture =
            "{\"name\": \"ok\", \"request\": {}, \"expected\": {\"result\": {}}}\nbad json\n";
        match parse_jsonl(fixture) {
            Ok(vectors) => Err(format!(
                "a malformed fixture should be an error, got {vectors_len} vectors",
                vectors_len = vectors.len()
            )),
            Err(error) => {
                if error.line == 2 {
                    Ok(())
                } else {
                    Err(format!(
                        "the line number should be 2, got {line}",
                        line = error.line
                    ))
                }
            }
        }
    }

    /// `JsonlError`'s `Display` impl carries the
    /// line number and the serde message in a
    /// single line, so a CI log without a JSON
    /// viewer can still grep the failure.
    #[test]
    fn jsonl_error_renders_one_line() {
        let error = JsonlError {
            line: 3,
            message: "expected `,` at line 1".to_owned(),
        };
        let text = format!("{error}");
        assert!(text.contains('3'), "the line number should appear: {text}");
        assert!(
            text.contains("expected"),
            "the serde message should appear: {text}"
        );
    }

    /// `run_against_in_process` is exercised in
    /// the integration test, but a one-vector
    /// unit test here proves the function is
    /// reachable from the lib's test target.
    #[tokio::test]
    async fn run_against_in_process_runs_zero_vectors() -> Result<(), &'static str> {
        let report = run_against_in_process(
            Arc::new(DeterministicAgentForUnit) as Arc<dyn A2aRequestHandler>,
            A2aVersion::V1_0,
            &[],
        )
        .await;
        if report.is_empty() {
            Ok(())
        } else {
            Err("an empty input should produce an empty report")
        }
    }

    /// `run_one` is exercised in the integration
    /// test, but a one-vector unit test here
    /// proves the function is reachable from the
    /// lib's test target.
    #[tokio::test]
    async fn run_one_produces_a_pass_for_a_well_known_response() {
        let vector = Vector {
            name: "sample".to_owned(),
            request: serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "NonExistent",
                "params": {}
            }),
            expected: ExpectedResponse::Error {
                error: ExpectedError {
                    code: -32601,
                    message_contains: "Method not found".to_owned(),
                },
            },
        };
        let router = crate::adapters::primary::a2a::jsonrpc_serve::serve_jsonrpc(
            "/a2a",
            Arc::new(DeterministicAgentForUnit) as Arc<dyn A2aRequestHandler>,
            A2aVersion::V1_0,
        );
        let result = run_one(&router, &vector).await;
        assert!(matches!(result.outcome, VectorOutcome::Pass));
    }

    /// A unit-test agent. Every method returns a
    /// `MethodNotImplemented` so the lib's test
    /// target can drive `run_one` with a known
    /// error shape without having to build a
    /// full reference agent.
    struct DeterministicAgentForUnit;

    impl A2aRequestHandler for DeterministicAgentForUnit {
        fn dispatch(
            &self,
            _method: crate::adapters::secondary::a2a::A2aMethod,
            _params: Option<Value>,
            _context: &crate::application::a2a::A2aRequestContext,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Value, crate::application::a2a::A2aError>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async move { Err(crate::application::a2a::A2aError::MethodNotFound) })
        }
    }
}
