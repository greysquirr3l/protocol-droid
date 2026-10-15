//! The A2A serving hot path, measured at the same boundaries the
//! MCP bench measures.
//!
//! # What this is
//!
//! The A2A protocol is the cousin of MCP — both are JSON-RPC 2.0
//! with a method table, a typed envelope, and a serving adapter —
//! and the A2A hot path is structurally the same shape: decode the
//! envelope, look the method up in the table, hand the raw params
//! to the handler. The benchmarks below mirror the MCP ones so a
//! regression on either side shows up in the same shape.
//!
//! # What is not in this bench
//!
//! - The streaming path (SSE). The streaming serving adapter
//!   builds a response stream, which is dominated by the
//!   `tokio::sync::mpsc` channel and the JSON-RPC envelope
//!   encode, not by the per-request dispatch. A streaming
//!   bench is a separate file.
//! - The push-notification delivery. The webhook delivery
//!   adapter is bounded by the network, not the dispatch; a
//!   bench would measure the loopback stack, which is the
//!   wrong number.
//!
//! # How to read a regression
//!
//! Same gate as the MCP bench: 10% move against the committed
//! baseline. The A2A envelope is heavier than the MCP one
//! (a `Message` with `parts` is the per-request cost), so the
//! absolute numbers will be larger; the relative shape is what
//! matters.

// divan's `#[bench]` macro matches its argument against a fn pointer written
// with an elided `Bencher`, and it does that before this crate's lints are
// resolved. Spelling the two lifetimes out is what the macro rejects, so the
// lint and the macro cannot both be satisfied at the signature — hence the
// expectation, scoped to this file rather than repeated on every benchmark.
#![expect(
    elided_lifetimes_in_paths,
    reason = "divan's `#[bench]` macro only matches a `Bencher` with the lifetimes elided; spelling them out makes the signature invisible to it"
)]
// Every input below is a `const` in this file. A decode failure therefore means
// the benchmark is timing a branch that never succeeds in production, and a
// green number for that would be worse than a loud one.
#![expect(
    clippy::panic,
    reason = "benchmark inputs are consts; a decode failure is a broken benchmark, not a condition to average over"
)]

use std::hint::black_box;

use divan::Bencher;
use protocol_droid::adapters::secondary::a2a::A2aMethodTable;
use protocol_droid::application::jsonrpc::{RouteError, Routed, route_one};
use protocol_droid::domain::jsonrpc::{Envelope, Frame, decode};
use serde_json::value::RawValue;

/// A `SendMessage` request — the heaviest
/// method the A2A v1.0 spec has. A
/// `Message` with one part and a
/// configuration block.
const SEND_MESSAGE: &[u8] = br#"{"jsonrpc":"2.0","id":"a7f3-4c21-9e08","method":"SendMessage","params":{"message":{"role":"user","messageId":"m-1","parts":[{"kind":"text","text":"hello"}]}}}"#;

/// A `GetTask` request — the smallest
/// request, just a task id.
const GET_TASK: &[u8] = br#"{"jsonrpc":"2.0","id":1,"method":"GetTask","params":{"id":"t-1"}}"#;

/// A `CancelTask` request — same
/// shape as `GetTask`.
const CANCEL_TASK: &[u8] =
    br#"{"jsonrpc":"2.0","id":2,"method":"CancelTask","params":{"id":"t-2"}}"#;

/// A `ListTasks` request — a
/// paginated list with no params
/// at the top level.
const LIST_TASKS: &[u8] = br#"{"jsonrpc":"2.0","id":3,"method":"ListTasks","params":{"limit":50}}"#;

/// A `SendStreamingMessage`
/// request — the streaming
/// counterpart of `SendMessage`.
const SEND_STREAMING_MESSAGE: &[u8] = br#"{"jsonrpc":"2.0","id":4,"method":"SendStreamingMessage","params":{"message":{"role":"user","messageId":"m-2","parts":[{"kind":"text","text":"hi"}]}}}"#;

/// A `SubscribeToTask` request.
const SUBSCRIBE_TO_TASK: &[u8] =
    br#"{"jsonrpc":"2.0","id":5,"method":"SubscribeToTask","params":{"id":"t-3"}}"#;

/// A method name that is not in
/// the A2A v1.0 table, with the
/// same `SendMessage` params.
/// The pair with [`SEND_MESSAGE`]
/// is the lazy-decode
/// measurement: byte-for-byte the
/// same params, and the only
/// difference is whether the
/// name is in the table.
const UNROUTABLE: &[u8] = br#"{"jsonrpc":"2.0","id":1,"method":"NotARealMethod","params":{"message":{"role":"user","messageId":"m-1","parts":[{"kind":"text","text":"hello"}]}}}"#;

/// A reply, which the router must
/// pass through rather than
/// look up.
const REPLY: &[u8] =
    br#"{"jsonrpc":"2.0","id":1,"result":{"kind":"task","id":"t-1","status":{"state":"working"}}}"#;

/// The single message out of a
/// benchmark input.
fn single(input: &[u8]) -> Envelope<'_> {
    match decode(input) {
        Ok(Frame::Single(envelope)) => envelope,
        other => panic!("this benchmark input is one message, not {other:?}"),
    }
}

/// Route one const through the
/// A2A method table, discarding
/// the route.
fn route(b: Bencher, input: &[u8]) {
    b.bench_local(|| {
        let routed: Result<Routed<'_, _, Option<&RawValue>, Option<&RawValue>>, RouteError<'_>> =
            route_one(single(black_box(input)), &A2aMethodTable::TABLE);
        black_box(routed)
    });
}

fn main() {
    divan::main();
}

#[divan::bench]
fn decode_send_message(b: Bencher) {
    b.bench_local(|| black_box(decode(black_box(SEND_MESSAGE))));
}

#[divan::bench]
fn decode_get_task(b: Bencher) {
    b.bench_local(|| black_box(decode(black_box(GET_TASK))));
}

#[divan::bench]
fn decode_cancel_task(b: Bencher) {
    b.bench_local(|| black_box(decode(black_box(CANCEL_TASK))));
}

#[divan::bench]
fn decode_list_tasks(b: Bencher) {
    b.bench_local(|| black_box(decode(black_box(LIST_TASKS))));
}

#[divan::bench]
fn decode_send_streaming_message(b: Bencher) {
    b.bench_local(|| black_box(decode(black_box(SEND_STREAMING_MESSAGE))));
}

#[divan::bench]
fn decode_subscribe_to_task(b: Bencher) {
    b.bench_local(|| black_box(decode(black_box(SUBSCRIBE_TO_TASK))));
}

#[divan::bench]
fn decode_reply(b: Bencher) {
    b.bench_local(|| black_box(decode(black_box(REPLY))));
}

#[divan::bench]
fn a_known_method_routes(b: Bencher) {
    route(b, SEND_MESSAGE);
}

#[divan::bench]
fn an_unroutable_frame_never_reaches_the_params(b: Bencher) {
    // The half of the lazy-decode
    // claim that costs
    // something. If this
    // converges on
    // `a_known_method_routes`,
    // the decoder is parsing
    // params it was supposed
    // to skip.
    route(b, UNROUTABLE);
}

#[divan::bench]
fn a_get_task_routes(b: Bencher) {
    route(b, GET_TASK);
}

#[divan::bench]
fn a_send_streaming_message_routes(b: Bencher) {
    route(b, SEND_STREAMING_MESSAGE);
}

#[divan::bench]
fn a_reply_routes_to_no_method(b: Bencher) {
    // A reply has no method
    // to look up; the router
    // passes it through.
    route(b, REPLY);
}
