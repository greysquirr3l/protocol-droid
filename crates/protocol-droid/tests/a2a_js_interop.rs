//! A2A v1.0 interop test against a vanilla-Node reference server.
//!
//! ## What this proves
//!
//! The library's `A2aClient` speaks the same wire
//! format as a non-protocol-droid A2A server. The
//! reference server is a 200-line vanilla-Node
//! script that implements `SendMessage`, `GetTask`,
//! `ListTasks`, `CancelTask`, and the
//! `/.well-known/agent-card.json` discovery
//! endpoint. The interop test drives every
//! method the reference server implements and
//! asserts that the typed response the
//! `A2aClient` produces has the same shape the
//! server actually emitted.
//!
//! ## Why vanilla Node instead of @a2a-js/sdk
//!
//! The official JS SDK is the canonical
//! reference impl, but the SDK's sample agent is
//! distributed as part of a multi-MB npm monorepo
//! with a TypeScript build step, peer deps, and a
//! separate `npm --prefix src/samples install`.
//! That is the right shape for a production
//! deployment; it is too heavy for a CI-friendly
//! integration test, which is what this file
//! exists for. The interop question is "do
//! protocol-droid and a hand-rolled A2A server
//! agree on the wire?" — the answer is the same
//! whether the second server is the SDK's
//! sample or a 200-line Node script, because
//! the wire format is published.
//!
//! ## Skipping when Node is not on PATH
//!
//! The interop test is a CI tool, not a
//! runtime dep. A dev environment without Node
//! skips the test with a clear log line; a CI
//! environment with Node runs it. The gate is
//! `node` being executable on `PATH`, not a
//! versioned dep, because the reference server
//! uses only `node:http` and `node:crypto` —
//! the API surface is stable across Node 18+.
//!
//! ## How a test runs
//!
//! 1. `find_node()` returns `Some("node")` or
//!    `None`.
//! 2. If `None`, the test logs and returns
//!    `Ok(())` (a skipped test is a pass).
//! 3. If `Some`, the test spawns
//!    `tests/fixtures/a2a_reference_server.mjs`
//!    on an OS-assigned port, reads the
//!    `listening on <port>` line from the
//!    child's stderr, and starts driving
//!    requests.
//! 4. The child process is killed at the end
//!    of the test (or when the test panics —
//!    see the `Drop` impl on `Child`).

// See the corresponding comment in `mcp_stdio_interop.rs` for why
// every Node-spawning interop test on this runner is gated to Unix.
// The Windows stdio handle for a console-mode `node` process hangs
// the test on GitHub-hosted `windows-latest`; same root cause, same
// fix. Move the gate if the Windows console-mode story is
// understood, not before.
#![cfg(all(feature = "a2a", unix))]

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use protocol_droid::adapters::secondary::a2a::A2aClient;
use protocol_droid::domain::a2a::list_tasks::{ListTasksRequest, ListTasksResponse};
use protocol_droid::domain::a2a::message::{Message, MessageRole, Part, TextPart};
use protocol_droid::domain::a2a::send_message::SendMessageRequest;
use protocol_droid::domain::a2a::send_message::SendMessageResponse;
use protocol_droid::domain::a2a::task::TaskState;
use protocol_droid::domain::a2a::task_lifecycle::CancelTaskRequest;
use protocol_droid::domain::a2a::task_lifecycle::GetTaskRequest;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// The path to the in-tree JS reference server,
/// resolved relative to `CARGO_MANIFEST_DIR`.
/// Kept as a function so a future move of the
/// fixture file is a one-line edit.
fn reference_server_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/a2a_reference_server.mjs")
}

/// A guard that kills the child process on
/// `drop`, so a test that panics still cleans
/// up the Node process. The drop log is silent
/// because test output is the report, not
/// the cleanup.
struct ChildGuard {
    child: tokio::process::Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

/// Returns the path to a `node` executable on
/// `PATH`, or `None` if there isn't one. The
/// interop test is a CI tool, not a runtime
/// dep; a dev box without Node skips the test
/// rather than failing it.
fn find_node() -> Option<String> {
    // `which` is the standard, but a manual
    // PATH walk is portable to every platform
    // this crate supports without adding a
    // dep. The walk checks each entry on
    // `PATH` for a `node` executable; on
    // Windows the binary is `node.exe`, on
    // Unix-like systems it is just `node`.
    let path = std::env::var_os("PATH")?;
    let candidates: &[&str] = if cfg!(windows) {
        &["node.exe", "node.cmd", "node.bat"]
    } else {
        &["node"]
    };
    for directory in std::env::split_paths(&path) {
        for candidate in candidates {
            let full = directory.join(candidate);
            if full.is_file() {
                return Some(full.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// Spawns the reference server and waits for
/// the `listening on <port>` line on stderr.
/// The line-based read is the cleanest way to
/// synchronize: a TCP connect would also work
/// but would race with the server's bind, and
/// the line is a contract the server
/// documents.
async fn spawn_reference_server(node: &str) -> Result<(String, ChildGuard), String> {
    let server_path = reference_server_path();
    if !server_path.is_file() {
        return Err(format!(
            "the reference server is missing: {}",
            server_path.display()
        ));
    }
    let mut command = Command::new(node);
    command
        .arg(&server_path)
        // Port 0 means "ask the OS for a free one", and the child reports
        // the port it actually got on its "listening" line.
        //
        // The test used to reserve a port itself and hand the number over,
        // which raced: between the reservation being released and the
        // child binding it, another process could take the port, and the
        // child would die with `EADDRINUSE`. The symptom was a test
        // failure that said nothing about the thing under test — "the
        // reference server closed stderr before listening" — and it fired
        // often enough on a busy CI runner to be worth removing rather
        // than retrying around.
        .env("PORT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| format!("spawning node {}: {error}", server_path.display()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "the child's stderr was not piped".to_owned())?;
    let mut reader = BufReader::new(stderr).lines();
    let expected_prefix = "listening on ";
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err("the reference server did not start within 5 seconds".to_owned());
        }
        let remaining = deadline - now;
        let line = match tokio::time::timeout(remaining, reader.next_line()).await {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => {
                return Err("the reference server closed stderr before listening".to_owned());
            }
            Ok(Err(error)) => return Err(format!("reading stderr: {error}")),
            Err(_) => return Err("the reference server did not start within 5 seconds".to_owned()),
        };
        if let Some(stripped) = line.strip_prefix(expected_prefix) {
            let bound_port: u16 = stripped
                .trim()
                .parse()
                .map_err(|error| format!("the listening line is malformed: {error}"))?;
            // The child asked for port 0, so the port it reports is the one
            // the OS assigned and the one it is listening on. Port 0 would
            // mean the fixture printed the port it was asked for rather
            // than the one it got, which is the bug this replaced.
            if bound_port == 0 {
                return Err("the reference server reported port 0".to_owned());
            }
            let base = format!("http://127.0.0.1:{bound_port}");
            return Ok((base, ChildGuard { child }));
        }
    }
}

/// A helper that builds a minimal text message.
/// The interop test does not exercise the
/// `Message` configuration or metadata fields
/// — those are covered in-process by the
/// `typed_dispatch` tests. The interop test
/// proves the wire shape, not the type
/// richness.
fn user_message(text: &str) -> Message {
    Message::new(MessageRole::User).with_part(Part::Text(TextPart {
        text: text.to_owned(),
        metadata: std::collections::BTreeMap::new(),
    }))
}

/// Asserts that a `serde_json::Value` carries the
/// expected field with the expected value. The
/// helper is used in place of `assert_eq!` on
/// the typed response because the typed
/// response carries a newtype wrapper
/// (`SendMessageResponse::Task(Task)`,
/// `CancelTaskResponse(Task)`) and the interop
/// test wants to read the *raw* fields, not
/// the newtype. The `Value` is the
/// post-deserialisation view of the wire.
fn assert_field_equals(value: &Value, pointer: &str, expected: &Value) -> Result<(), String> {
    let actual = value
        .pointer(pointer)
        .ok_or_else(|| format!("missing field {pointer}"))?;
    if actual != expected {
        return Err(format!(
            "field {pointer}: expected {expected}, got {actual}"
        ));
    }
    Ok(())
}

/// The whole interop test is gated on
/// `find_node()` returning `Some`. A clean
/// skip is `Ok(())`; a real failure is
/// `Err(...)`.
fn ensure_node() -> Result<String, String> {
    find_node().map_or_else(
        || {
            eprintln!(
                "a2a-js interop: node is not on PATH; the in-tree reference \
                 server is a JS script and the test is skipped. Install Node 18+ \
                 to run the interop test."
            );
            Err("node is not on PATH; skipping".to_owned())
        },
        Ok,
    )
}

/// `card()` deserialises the reference
/// server's static card into the typed
/// `AgentCard`. The card is the most
/// shape-sensitive endpoint — it is a GET,
/// not a JSON-RPC POST, and the field names
/// are `camelCase` per the spec.
#[tokio::test]
async fn card_deserialises_against_the_vanilla_js_server() -> Result<(), Box<dyn std::error::Error>>
{
    let node = match ensure_node() {
        Ok(node) => node,
        Err(_skip) => return Ok(()),
    };
    let (base, _guard) = spawn_reference_server(&node).await?;
    let client = A2aClient::new(&base)?;
    let card = client.card().await?;
    assert_eq!(card.name.as_ref(), "a2a-reference-server");
    Ok(())
}

/// `send_message()` POSTs to `/a2a` and
/// decodes the response into
/// `SendMessageResponse::Task(Task)`. The
/// interop assertion is on the wire fields,
/// not the typed newtype.
#[tokio::test]
async fn send_message_round_trips_against_the_vanilla_js_server()
-> Result<(), Box<dyn std::error::Error>> {
    let node = match ensure_node() {
        Ok(node) => node,
        Err(_skip) => return Ok(()),
    };
    let (base, _guard) = spawn_reference_server(&node).await?;
    let client = A2aClient::new(&base)?;
    let request = SendMessageRequest::new(user_message("hi"));
    let response = client.send_message(request).await?;
    let SendMessageResponse::Task(task) = response else {
        return Err("the reference server returns a Task, not a Message".into());
    };
    let value = serde_json::to_value(&task).map_err(|e| format!("to_value: {e}"))?;
    assert_field_equals(
        &value,
        "/status/state",
        &Value::String("completed".to_owned()),
    )?;
    let text = value
        .pointer("/status/message/parts/0/text")
        .and_then(Value::as_str)
        .ok_or("the reference server did not echo the user message")?;
    assert!(
        text.contains("hi"),
        "the response should mention the input: {text}"
    );
    Ok(())
}

/// `get_task()` reads a task by id. The
/// reference server stores tasks in an
/// in-memory map; the interop test creates a
/// task, reads it back, and asserts the id
/// round-trips. This is the test that proves
/// the server's id field is the same shape
/// the client deserialises — a server that
/// returned an integer id would fail here.
#[tokio::test]
async fn get_task_reads_back_what_send_message_wrote() -> Result<(), Box<dyn std::error::Error>> {
    let node = match ensure_node() {
        Ok(node) => node,
        Err(_skip) => return Ok(()),
    };
    let (base, _guard) = spawn_reference_server(&node).await?;
    let client = A2aClient::new(&base)?;
    let request = SendMessageRequest::new(user_message("hi"));
    let response = client.send_message(request).await?;
    let SendMessageResponse::Task(task) = response else {
        return Err("the reference server returns a Task, not a Message".into());
    };
    let task_id = task.id.to_string();
    let get = client
        .get_task(GetTaskRequest::new(task_id.clone()))
        .await?;
    assert_eq!(get.0.id.as_ref(), task_id.as_str());
    assert_eq!(get.0.status.state, TaskState::Completed);
    Ok(())
}

/// `list_tasks()` returns the spec's paginated
/// envelope. The interop test asserts the
/// `totalSize`, `pageSize`, and
/// `nextPageToken` fields are present and
/// have the right type — the reference
/// server uses an empty-string `nextPageToken`
/// to signal "no more pages", which is the
/// spec's required field.
#[tokio::test]
async fn list_tasks_returns_the_paginated_envelope() -> Result<(), Box<dyn std::error::Error>> {
    let node = match ensure_node() {
        Ok(node) => node,
        Err(_skip) => return Ok(()),
    };
    let (base, _guard) = spawn_reference_server(&node).await?;
    let client = A2aClient::new(&base)?;
    // Create one task so the list is non-empty.
    let _ = client
        .send_message(SendMessageRequest::new(user_message("hi")))
        .await?;
    let list = client.list_tasks(ListTasksRequest::new()).await?;
    let ListTasksResponse {
        tasks,
        next_page_token,
        page_size,
        total_size,
    } = list;
    assert_eq!(total_size, 1);
    assert!(
        page_size >= 1,
        "pageSize must be at least 1, got {page_size}"
    );
    assert!(
        next_page_token.is_empty(),
        "the absent signal is the empty string"
    );
    assert_eq!(tasks.len(), 1);
    Ok(())
}

/// `cancel_task()` flips the state to
/// `cancelled`. The interop test creates a
/// task, cancels it, and asserts the state
/// round-trip. The reference server mutates
/// the in-memory task in place, so a
/// subsequent `get_task()` would also see
/// `cancelled`; the test checks the
/// `cancel_task` response itself, which is
/// the more direct assertion.
#[tokio::test]
async fn cancel_task_flips_state_to_cancelled() -> Result<(), Box<dyn std::error::Error>> {
    let node = match ensure_node() {
        Ok(node) => node,
        Err(_skip) => return Ok(()),
    };
    let (base, _guard) = spawn_reference_server(&node).await?;
    let client = A2aClient::new(&base)?;
    let request = SendMessageRequest::new(user_message("hi"));
    let response = client.send_message(request).await?;
    let SendMessageResponse::Task(task) = response else {
        return Err("the reference server returns a Task, not a Message".into());
    };
    let task_id = task.id.to_string();
    let cancelled = client
        .cancel_task(CancelTaskRequest::new(task_id.clone()))
        .await?;
    assert_eq!(cancelled.0.id.as_ref(), task_id.as_str());
    assert_eq!(cancelled.0.status.state, TaskState::Cancelled);
    Ok(())
}
