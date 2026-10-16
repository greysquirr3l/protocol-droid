//! A2A v1.0 conformance vector runner, in process.
//!
//! The runner at `application::a2a::conformance` is the
//! library side: it takes a list of `Vector` and an
//! `Arc<dyn A2aRequestHandler>`, runs each vector through
//! `serve_jsonrpc`, and returns a list of
//! `VectorResult` (one `Pass` or `Fail` per input).
//!
//! This file is the test that proves the runner works.
//! It uses a deterministic in-process agent that produces
//! the same responses every time, and the in-tree
//! `tests/fixtures/a2a_v1_0.jsonl` fixture as the
//! vector list. A pass here is a pass on every
//! supported platform, in every CI run, with no
//! external dependency.

// Reads `protocol_droid::a2a::*`; gated behind the `a2a` feature.
#![cfg(feature = "a2a")]
#![expect(
    clippy::expect_used,
    reason = "tests assert on fallible calls; a panic in a test is the right shape — propagate `?` would replace a named failure with `from()` noise"
)]

use std::sync::Arc;

use protocol_droid::adapters::primary::a2a::jsonrpc_serve::serve_jsonrpc;
use protocol_droid::adapters::secondary::a2a::A2aMethod;
use protocol_droid::application::a2a::conformance::{
    ExpectedError, ExpectedResponse, JsonlError, ObservedResponse, Vector, VectorOutcome,
    VectorResult, parse_jsonl, run_against_in_process, run_one,
};
use protocol_droid::application::a2a::{A2aError, A2aRequestContext, A2aRequestHandler};
use protocol_droid::domain::a2a::message::{Message, MessageRole, Part, TextPart};
use protocol_droid::domain::a2a::task::{Task, TaskState, TaskStatus};
use protocol_droid::domain::a2a::version::A2aVersion;
use serde_json::{Value, json};

/// A deterministic in-process agent. Every method that
/// the vectors exercise is implemented; the responses
/// are hard-coded so the expected `Vector` shapes are
/// stable across runs.
///
/// The vectors are written against this agent's wire
/// output, so the assertion is "what this agent
/// returns", not "what the spec says a generic agent
/// returns". A future extension could add a
/// `ReferenceAgent` that uses the `a2a_server` example's
/// `TaskStore` instead, and the same vectors would
/// pass against that as long as the wire output is the
/// same shape.
struct DeterministicAgent;

impl A2aRequestHandler for DeterministicAgent {
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per A2A method the test agent supports"
    )]
    fn dispatch(
        &self,
        method: A2aMethod,
        params: Option<Value>,
        _context: &A2aRequestContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, A2aError>> + Send + '_>>
    {
        match method {
            A2aMethod::SendMessage => {
                let request = match decode_params::<SendMessageLike>(params) {
                    Ok(request) => request,
                    Err(error) => return Box::pin(async move { Err(error) }),
                };
                let text = request
                    .message
                    .parts
                    .first()
                    .and_then(|part| match part {
                        Part::Text(text) => Some(text.text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let task = Task::new("t-1".to_owned()).with_status(TaskStatus {
                    state: TaskState::Completed,
                    message: Some(Message::new(MessageRole::Agent).with_part(Part::Text(
                        TextPart {
                            text,
                            metadata: std::collections::BTreeMap::new(),
                        },
                    ))),
                    timestamp: None,
                });
                let value = serde_json::to_value(&task).expect("Task is JSON");
                Box::pin(async move { Ok(value) })
            }
            A2aMethod::GetTask => {
                let request = match decode_params::<IdParam>(params) {
                    Ok(request) => request,
                    Err(error) => return Box::pin(async move { Err(error) }),
                };
                if request.id != "t-1" {
                    return Box::pin(async move {
                        Err(A2aError::InvalidParams {
                            reason: format!("no task with id '{}'", request.id),
                        })
                    });
                }
                let task = Task::new("t-1".to_owned()).with_status(TaskStatus {
                    state: TaskState::Completed,
                    message: None,
                    timestamp: None,
                });
                let value = serde_json::to_value(&task).expect("Task is JSON");
                Box::pin(async move { Ok(value) })
            }
            A2aMethod::CancelTask => {
                let request = match decode_params::<IdParam>(params) {
                    Ok(request) => request,
                    Err(error) => return Box::pin(async move { Err(error) }),
                };
                if request.id != "t-1" {
                    return Box::pin(async move {
                        Err(A2aError::InvalidParams {
                            reason: format!("no task with id '{}'", request.id),
                        })
                    });
                }
                let task = Task::new("t-1".to_owned()).with_status(TaskStatus {
                    state: TaskState::Cancelled,
                    message: None,
                    timestamp: None,
                });
                let value = serde_json::to_value(&task).expect("Task is JSON");
                Box::pin(async move { Ok(value) })
            }
            A2aMethod::ListTasks => {
                let value = json!({
                    "tasks": [],
                    "totalSize": 0,
                    "pageSize": 50,
                    "nextPageToken": null,
                });
                Box::pin(async move { Ok(value) })
            }
            A2aMethod::GetTaskPushNotificationConfig => {
                let request = match decode_params::<TaskConfigIdParam>(params) {
                    Ok(request) => request,
                    Err(error) => return Box::pin(async move { Err(error) }),
                };
                let value = json!({
                    "taskId": request.task_id,
                    "configId": request.config_id,
                });
                Box::pin(async move { Ok(value) })
            }
            A2aMethod::ListTaskPushNotificationConfigs => {
                let request = match decode_params::<TaskIdParam>(params) {
                    Ok(request) => request,
                    Err(error) => return Box::pin(async move { Err(error) }),
                };
                let value = json!({
                    "configs": [],
                    "taskId": request.task_id,
                });
                Box::pin(async move { Ok(value) })
            }
            A2aMethod::DeleteTaskPushNotificationConfig => {
                let _ = match decode_params::<TaskConfigIdParam>(params) {
                    Ok(request) => request,
                    Err(error) => return Box::pin(async move { Err(error) }),
                };
                let value = json!({});
                Box::pin(async move { Ok(value) })
            }
            A2aMethod::SendStreamingMessage
            | A2aMethod::CreateTaskPushNotificationConfig
            | A2aMethod::SubscribeToTask
            | A2aMethod::GetExtendedAgentCard => {
                Box::pin(async move { Err(A2aError::MethodNotImplemented { method }) })
            }
        }
    }
}

/// The shape the runner decodes `SendMessage` params
/// into. A subset of the typed
/// `protocol_droid::domain::a2a::send_message::SendMessageRequest`
/// — the field the agent actually reads. Defining a
/// small local struct keeps the test from depending
/// on the typed request's full shape, which is the
/// point of the runner: it works against any
/// `A2aRequestHandler`, not just the typed one.
#[derive(serde::Deserialize)]
struct SendMessageLike {
    message: MessageLike,
}

#[derive(serde::Deserialize)]
struct MessageLike {
    #[allow(dead_code, reason = "decoded for shape validation only")]
    role: MessageRole,
    parts: Vec<Part>,
}

#[derive(serde::Deserialize)]
struct IdParam {
    id: String,
}

#[derive(serde::Deserialize)]
struct TaskIdParam {
    #[serde(rename = "taskId")]
    task_id: String,
}

#[derive(serde::Deserialize)]
struct TaskConfigIdParam {
    #[serde(rename = "taskId")]
    task_id: String,
    #[serde(rename = "configId")]
    config_id: String,
}

/// Decodes the optional `params` object into a typed
/// shape. A `None` here means the request had no
/// `params` field, which the spec allows for
/// notification calls but the test's typed shapes
/// require a `Value::Null` to satisfy `serde`.
fn decode_params<T: serde::de::DeserializeOwned>(params: Option<Value>) -> Result<T, A2aError> {
    serde_json::from_value(params.unwrap_or(Value::Null)).map_err(|error| A2aError::InvalidParams {
        reason: format!("params do not match the method shape: {error}"),
    })
}

/// The full in-tree fixture passes through the runner
/// with every vector green. A failure here is a
/// regression either in the runner, the agent, or
/// the fixture.
#[tokio::test]
async fn the_in_tree_fixture_passes_end_to_end() -> Result<(), Box<dyn std::error::Error>> {
    let vectors = fixture()?;
    let report =
        run_against_in_process(Arc::new(DeterministicAgent), A2aVersion::V1_0, &vectors).await;
    let failed: Vec<&VectorResult> = report
        .iter()
        .filter(|result| matches!(result.outcome, VectorOutcome::Fail { .. }))
        .collect();
    if !failed.is_empty() {
        use std::fmt::Write as _;
        let mut message = String::new();
        for entry in &failed {
            let _ = writeln!(message, "\n- {}: ", entry.name);
            if let VectorOutcome::Fail { diff, actual } = &entry.outcome {
                message.push_str(diff);
                let _ = writeln!(message, "\n  actual: {actual}");
            }
        }
        return Err(format!(
            "{failed_len} vectors failed: {message}",
            failed_len = failed.len()
        )
        .into());
    }
    assert_eq!(
        report.len(),
        vectors.len(),
        "every vector should produce a result"
    );
    Ok(())
}

/// A vector with an `error` expected and a server
/// that returns an `error` matches on the code plus
/// a substring of the message — the test pins both.
#[tokio::test]
async fn a_missing_param_vector_matches_on_code_and_substring()
-> Result<(), Box<dyn std::error::Error>> {
    let vector = Vector {
        name: "missing-param".to_owned(),
        request: json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "GetTask",
            "params": {}
        }),
        expected: ExpectedResponse::Error {
            error: ExpectedError {
                code: -32602,
                message_contains: "id".to_owned(),
            },
        },
    };
    let router = serve_jsonrpc("/a2a", Arc::new(DeterministicAgent), A2aVersion::V1_0);
    let result = run_one(&router, &vector).await;
    assert_eq!(result.name, "missing-param");
    assert!(
        matches!(result.outcome, VectorOutcome::Pass),
        "expected Pass, got {:?}",
        result.outcome
    );
    Ok(())
}

/// A vector that expects a `result` and the server
/// answers with an `error` is a `Fail`, not a Pass.
/// The runner is strict about the shape.
#[tokio::test]
async fn a_wrong_shape_answer_is_a_fail_not_a_pass() -> Result<(), Box<dyn std::error::Error>> {
    let vector = Vector {
        name: "expects-result-but-server-errored".to_owned(),
        request: json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "NonExistent",
            "params": {}
        }),
        expected: ExpectedResponse::Result {
            result: json!({ "id": "t-1" }),
        },
    };
    let router = serve_jsonrpc("/a2a", Arc::new(DeterministicAgent), A2aVersion::V1_0);
    let result = run_one(&router, &vector).await;
    let VectorOutcome::Fail { diff: _, actual } = result.outcome else {
        return Err("expected a Fail for a wrong-shape answer".into());
    };
    assert!(
        matches!(actual, ObservedResponse::Error { .. }),
        "the actual should be the error the server returned, not a synthetic shape: {actual:?}"
    );
    Ok(())
}

/// `parse_jsonl` skips blank lines and lines that
/// start with `#`, so a fixture file can carry a
/// license header without breaking the runner.
#[test]
fn parse_jsonl_ignores_blank_and_comment_lines() -> Result<(), JsonlError> {
    let fixture = "\
# License header. The runner ignores this line.
{\"name\": \"first\", \"request\": {}, \"expected\": {\"result\": {}}}

{\"name\": \"second\", \"request\": {}, \"expected\": {\"result\": {}}}
";
    let vectors = parse_jsonl(fixture)?;
    assert_eq!(vectors.len(), 2);
    assert_eq!(vectors.first().map(|v| v.name.as_str()), Some("first"));
    assert_eq!(vectors.get(1).map(|v| v.name.as_str()), Some("second"));
    Ok(())
}

/// A malformed JSONL line names its line number,
/// so a fixture author can fix the file in seconds
/// rather than hunting for the bad line.
#[test]
fn parse_jsonl_reports_the_line_number_of_a_malformed_entry() -> Result<(), String> {
    let fixture =
        "{\"name\": \"ok\", \"request\": {}, \"expected\": {\"result\": {}}}\nthis is not json\n";
    match parse_jsonl(fixture) {
        Ok(vectors) => Err(format!(
            "a malformed line should be an error, got {vectors_len} vectors",
            vectors_len = vectors.len()
        )),
        Err(error) => {
            if error.line != 2 {
                return Err(format!(
                    "the bad line is line 2, but the error said {line}",
                    line = error.line
                ));
            }
            if !error.message.contains("expected") {
                return Err(format!(
                    "the error should mention the serde expectation: {msg}",
                    msg = error.message
                ));
            }
            Ok(())
        }
    }
}

/// Loads the in-tree fixture. The path is the
/// `include_str!` in `conformance::STARTER_FIXTURE`
/// re-read from disk; the test file is in `tests/`
/// so the relative path is one directory up.
fn fixture() -> Result<Vec<Vector>, Box<dyn std::error::Error>> {
    let contents = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/a2a_v1_0.jsonl"),
    )?;
    Ok(parse_jsonl(&contents)?)
}
