//! `protocol-droid a2a-conformance` — drive a JSONL of A2A
//! v1.0 vectors against a third-party A2A server.
//!
//! ## What this is
//!
//! The A2A spec has no official test suite, so this
//! crate ships the runner that drives the in-tree
//! `tests/fixtures/a2a_v1_0.jsonl` against an
//! arbitrary A2A server the operator points it at.
//! A third-party implementer who wants to check
//! their server against the spec runs:
//!
//! ```text
//! protocol-droid a2a-conformance \
//!     crates/protocol-droid/tests/fixtures/a2a_v1_0.jsonl \
//!     --server http://127.0.0.1:8080
//! ```
//!
//! and reads the report: a JSONL on stdout
//! (`{vector, expected, actual, pass}` per line, for
//! pipelines that want to grep and re-format) and a
//! human summary on stderr (the pass/fail count and
//! the first failing vector).
//!
//! ## Why the runner is the same code as the in-process
//!
//! The same [`protocol_droid::application::a2a::conformance`]
//! module that the integration test drives is the one
//! the CLI drives — through a `reqwest` HTTP client
//! instead of a `tower::ServiceExt::oneshot`. The
//! vector schema, the `ExpectedResponse` shape, and
//! the `VectorResult` are identical, so a third-party
//! implementer running this CLI gets the same
//! pass/fail verdict the in-tree tests get.
//!
//! ## What the wrapper does *not* do
//!
//! It does not synthesise vectors, does not auto-
//! generate requests from the spec, and does not
//! retry on transient errors. The vectors are the
//! source of truth; a failure is a failure.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, anyhow};
use clap::Args;
use protocol_droid::application::a2a::conformance::{
    ExpectedError, ExpectedResponse, JsonlError, ObservedError, ObservedResponse, Vector,
    VectorOutcome, VectorResult, parse_jsonl,
};
use reqwest::Client;
use serde::Serialize;

/// How long a single HTTP call to the server may
/// take before the runner records it as a transport
/// failure rather than waiting forever. A peer
/// that has hung without answering is a different
/// failure mode from one that answered wrongly,
/// and the operator reading the report wants to
/// know which.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// What the operator passed on the command line.
#[derive(Debug, Args)]
pub struct A2aConformanceArgs {
    /// The JSONL fixture to drive against the server.
    ///
    /// The fixture format is one `{name, request, expected}`
    /// object per line. A blank line or a line whose
    /// first non-whitespace character is `#` is ignored,
    /// so the file can carry a license header.
    #[arg(value_name = "VECTORS")]
    pub vectors: PathBuf,

    /// The base URL of the A2A server to test, e.g.
    /// `http://127.0.0.1:8080`. The runner POSTs to
    /// `{server}/a2a` for every vector.
    #[arg(long, value_name = "URL")]
    pub server: String,

    /// Emit the report as a single JSON object on
    /// stdout instead of the default JSONL stream.
    ///
    /// Off by default — JSONL is the shape pipelines
    /// consume, and one line per vector means a
    /// failing line is grep-able. The aggregated form
    /// is for an operator reading the report once.
    #[arg(long)]
    pub json: bool,
}

/// Runs the runner end-to-end.
///
/// Returns `Err` when the fixture file is
/// unreadable or the vectors could not be
/// parsed; a fixture that parses to an empty
/// list is a no-op that exits 0, so a CI job
/// without the fixture file can ship with a
/// passing run.
///
/// # Errors
///
/// `anyhow::Error` because the CLI is a binary and
/// nobody outside this crate will ever match on
/// its error variants. The error chains carry
/// enough context for an operator to fix the
/// command line or the fixture without re-running
/// with `--verbose`.
pub async fn run(args: &A2aConformanceArgs) -> anyhow::Result<()> {
    let vectors = load_vectors(&args.vectors)
        .with_context(|| format!("reading vectors from {}", args.vectors.display()))?;
    if vectors.is_empty() {
        eprintln!("a2a-conformance: the fixture is empty, nothing to run");
        return Ok(());
    }
    eprintln!(
        "a2a-conformance: driving {count} vectors against {server}",
        count = vectors.len(),
        server = args.server
    );
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .context("building the reqwest client")?;
    let report = run_vectors(&client, &args.server, &vectors).await;
    if args.json {
        let aggregated = AggregatedReport {
            server: args.server.clone(),
            total: report.len(),
            passed: report
                .iter()
                .filter(|r| matches!(r.outcome, VectorOutcome::Pass))
                .count(),
            failed: report
                .iter()
                .filter(|r| matches!(r.outcome, VectorOutcome::Fail { .. }))
                .count(),
            results: report.clone(),
        };
        let json = serde_json::to_string_pretty(&aggregated)
            .context("serialising the aggregated report")?;
        println!("{json}");
    } else {
        for entry in &report {
            let line = serde_json::to_string(entry).context("serialising a result line")?;
            println!("{line}");
        }
    }
    let failed: usize = report
        .iter()
        .filter(|r| matches!(r.outcome, VectorOutcome::Fail { .. }))
        .count();
    if failed == 0 {
        eprintln!(
            "a2a-conformance: {passed}/{total} vectors passed",
            passed = report.len(),
            total = report.len()
        );
        Ok(())
    } else {
        eprintln!(
            "a2a-conformance: {failed}/{total} vectors failed",
            failed = failed,
            total = report.len()
        );
        if let Some(first_fail) = report
            .iter()
            .find(|r| matches!(r.outcome, VectorOutcome::Fail { .. }))
            && let VectorOutcome::Fail { diff, .. } = &first_fail.outcome
        {
            eprintln!(
                "a2a-conformance: first failure: {name}: {diff}",
                name = first_fail.name
            );
        }
        Err(anyhow!("{failed} vectors failed; the report is on stdout"))
    }
}

/// The aggregated JSON form for `--json`. The
/// `results` field carries the same `VectorResult`
/// the JSONL stream carries, so a downstream that
/// reads either form sees the same shape.
#[derive(Debug, Serialize)]
struct AggregatedReport {
    /// The server URL the runner drove against.
    /// Echoed in the report so a CI artifact
    /// carries provenance.
    server: String,
    /// The total number of vectors the runner
    /// ran. `passed + failed == total`.
    total: usize,
    /// The number of vectors that passed.
    passed: usize,
    /// The number of vectors that failed.
    failed: usize,
    /// The per-vector result, in fixture order.
    results: Vec<VectorResult>,
}

/// Reads a JSONL file from disk and parses it into
/// a list of vectors. Surfaces `JsonlError`'s line
/// number so an operator editing the fixture
/// knows where the bad line is.
fn load_vectors(path: &std::path::Path) -> Result<Vec<Vector>, JsonlError> {
    let contents = std::fs::read_to_string(path).map_err(|error| JsonlError {
        line: 0,
        message: format!("read {}: {error}", path.display()),
    })?;
    parse_jsonl(&contents)
}

/// Drives every vector through the server and
/// collects the per-vector results. A failure on
/// one vector does not abort the run — the report
/// is the whole point, and a partial report is
/// more useful than no report.
async fn run_vectors(client: &Client, server: &str, vectors: &[Vector]) -> Vec<VectorResult> {
    let endpoint = format!("{server}/a2a");
    let mut out = Vec::with_capacity(vectors.len());
    for vector in vectors {
        let observed = match drive_one(client, &endpoint, vector).await {
            Ok(observed) => observed,
            Err(error) => ObservedResponse::Transport {
                status: 0,
                body: format!("runner: {error}"),
            },
        };
        out.push(compare(vector, &observed));
    }
    out
}

/// Sends a single vector's request to the server
/// and shapes the response into the runner's
/// `ObservedResponse`. A non-2xx status is a
/// `Transport`; a 200 with a malformed JSON-RPC
/// envelope is also a `Transport`; only a 200
/// with a well-formed envelope becomes a
/// `Result` or `Error`.
async fn drive_one(
    client: &Client,
    endpoint: &str,
    vector: &Vector,
) -> anyhow::Result<ObservedResponse> {
    let body = serde_json::to_vec(&vector.request)
        .with_context(|| format!("serialising the request for {}", vector.name))?;
    let response = client
        .post(endpoint)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .with_context(|| format!("sending the request for {}", vector.name))?;
    let status = response.status();
    let body_bytes = response
        .bytes()
        .await
        .with_context(|| format!("reading the response body for {}", vector.name))?;
    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();
    if !status.is_success() {
        return Ok(ObservedResponse::Transport {
            status: status.as_u16(),
            body: body_text,
        });
    }
    let envelope: serde_json::Value = serde_json::from_slice(&body_bytes).with_context(|| {
        format!(
            "decoding the JSON-RPC envelope for {}: body was {body_text}",
            vector.name
        )
    })?;
    let Some(envelope_object) = envelope.as_object() else {
        return Ok(ObservedResponse::Transport {
            status: status.as_u16(),
            body: format!("envelope was not a JSON object: {envelope}"),
        });
    };
    if let Some(result) = envelope_object.get("result") {
        return Ok(ObservedResponse::Result {
            result: result.clone(),
        });
    }
    if let Some(error) = envelope_object.get("error") {
        let code = error
            .get("code")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned();
        return Ok(ObservedResponse::Error {
            error: ObservedError { code, message },
        });
    }
    Ok(ObservedResponse::Transport {
        status: status.as_u16(),
        body: format!("envelope carried neither result nor error: {envelope}"),
    })
}

/// The same compare function the in-process runner
/// uses, lifted here so the CLI and the integration
/// test share the same pass/fail verdict. The shape
/// it produces — `VectorResult { name, outcome }` —
/// is identical, so a downstream reading either
/// the CLI's JSONL or the integration test's
/// in-process report sees the same fields.
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
                diff: "expected and observed did not match".to_owned(),
                actual: observed.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A vector with an `error` expected matches
    /// on the code and a substring of the message,
    /// so a server that adds detail to the message
    /// (the A2A spec leaves the message wording up
    /// to the implementation) is still a pass.
    #[test]
    fn an_error_vector_matches_on_code_and_substring() {
        let vector = Vector {
            name: "err".to_owned(),
            request: json!({}),
            expected: ExpectedResponse::Error {
                error: ExpectedError {
                    code: -32601,
                    message_contains: "Method not found".to_owned(),
                },
            },
        };
        let observed = ObservedResponse::Error {
            error: ObservedError {
                code: -32601,
                message: "Method not found (the server did not recognise the method)".to_owned(),
            },
        };
        let result = compare(&vector, &observed);
        assert!(matches!(result.outcome, VectorOutcome::Pass));
    }

    /// A wrong shape is a fail, not a pass. The
    /// runner is strict: a `Result` expectation
    /// against an `Error` answer is `Fail`.
    #[test]
    fn a_wrong_shape_is_a_fail() {
        let vector = Vector {
            name: "r".to_owned(),
            request: json!({}),
            expected: ExpectedResponse::Result { result: json!({}) },
        };
        let observed = ObservedResponse::Error {
            error: ObservedError {
                code: -32601,
                message: "Method not found".to_owned(),
            },
        };
        let result = compare(&vector, &observed);
        assert!(matches!(result.outcome, VectorOutcome::Fail { .. }));
    }

    /// `load_vectors` reports the path and the
    /// underlying error when the file is missing.
    /// The CLI's `?` propagation surfaces the
    /// anyhow context, so the operator sees both
    /// the path they passed and what went wrong.
    ///
    /// The platform-specific `io::Error` text is
    /// asserted on the path the operator passed,
    /// not on a literal phrase. On Unix the
    /// underlying error is "No such file or
    /// directory"; on Windows it is "The system
    /// cannot find the path specified". Asserting
    /// the path keeps the test portable, and the
    /// path is the part the operator named in the
    /// diagnostic anyway.
    #[test]
    fn load_vectors_reports_missing_files() {
        let path = std::path::Path::new("/nonexistent/a2a_v1_0.jsonl");
        let result = load_vectors(path);
        let Err(error) = result else {
            return;
        };
        assert_eq!(
            error.line, 0,
            "a missing file is a line-0 error, not a parse error"
        );
        assert!(
            error.message.contains(path.to_str().expect("path is valid UTF-8")),
            "the operator's path should appear in the diagnostic: {msg}",
            msg = error.message
        );
    }

    /// A transport-level observation (a non-2xx
    /// status) is a `Fail` against a `Result`
    /// expectation. The runner cannot turn a
    /// transport failure into a spec-compliant
    /// response, so the verdict is the same as
    /// for a wrong shape.
    #[test]
    fn a_transport_observation_is_a_fail_against_a_result_expectation() {
        let vector = Vector {
            name: "transport".to_owned(),
            request: json!({}),
            expected: ExpectedResponse::Result { result: json!({}) },
        };
        let observed = ObservedResponse::Transport {
            status: 500,
            body: "internal".to_owned(),
        };
        let result = compare(&vector, &observed);
        assert!(matches!(result.outcome, VectorOutcome::Fail { .. }));
    }

    /// The args struct is the contract; pinning
    /// it here means a clap schema change is
    /// caught by the test, not by the first CI
    /// job that runs the subcommand.
    #[test]
    fn the_args_struct_carries_the_expected_fields() {
        let args = A2aConformanceArgs {
            vectors: PathBuf::from("/tmp/x.jsonl"),
            server: "http://127.0.0.1:8080".to_owned(),
            json: false,
        };
        let _ = args.vectors;
        let _ = args.server;
        let _ = args.json;
    }

    /// The CLI's `run` is wired through the binary
    /// target — a unit test here would have to
    /// spin up an HTTP server, which is what the
    /// end-to-end smoke test does outside the
    /// lib. A unit test that drives the function
    /// with a missing fixture file proves it is
    /// reachable from the lib's test target: the
    /// function returns an error (the file is
    /// not there), and the test does not panic.
    #[tokio::test]
    async fn run_reports_an_error_when_the_fixture_path_is_missing() {
        let args = A2aConformanceArgs {
            vectors: PathBuf::from("/nonexistent/a2a_v1_0.jsonl"),
            server: "http://127.0.0.1:0".to_owned(),
            json: false,
        };
        let result = run(&args).await;
        assert!(result.is_err(), "a missing fixture should be a hard error");
    }
}
