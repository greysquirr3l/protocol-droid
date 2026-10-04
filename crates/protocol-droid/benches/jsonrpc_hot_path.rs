//! The JSON-RPC hot path, measured at the boundaries where it can regress.
//!
//! # What is worth a benchmark and what is not
//!
//! Everything in this module sits on the path a real request takes, and each
//! benchmark isolates one decision that a change could plausibly make worse:
//!
//! - `decode` — the envelope. Every frame in the system passes through this
//!   once, so a regression here is a regression in everything.
//! - `route_one` — the router, with the real MCP table. The point of the
//!   lazy-params arrangement is that a method name that is not in the table is
//!   refused on the name alone, and `an_unroutable_frame_never_reaches_the_params`
//!   is the benchmark that would catch that being undone: it routes
//!   `UNROUTABLE`, which carries byte-for-byte the same params as
//!   `TOOLS_CALL`, and it should cost what `a_known_method_routes` costs. The
//!   claim is that they are *close*, not that one is faster — the envelope
//!   decode dominates both, and the params are skipped in both. A change that
//!   parses params before looking the name up would show up as the unroutable
//!   one drifting *above* the routable one.
//! - `WireId::from_raw` and `WireId::to_json` — the id path, measured
//!   separately for the borrowed and the unescaped arm because only one of
//!   them allocates. The measured gap is about 6x — an escaped id is roughly
//!   `decode_escaped_id` at 750 ns against `decode_string_id` at 125 ns — so a
//!   change that made *every* id take the owned arm would be a large,
//!   obvious regression rather than a few percent of noise.
//!
//! # How to read a regression
//!
//! CI gates a >10% move against the committed baseline. Run
//! `cargo bench -p protocol-droid --bench jsonrpc_hot_path` locally to see the
//! breakdown before deciding whether a move is a regression or the machine
//! being busy: these benchmarks are allocation-sensitive, so a noisy machine
//! shows up as a uniform shift across all of them rather than one line moving.

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
use protocol_droid::adapters::secondary::mcp::McpMethodTable;
use protocol_droid::application::jsonrpc::{
    AllowBatching, MethodTable, RouteError, Routed, route_frame, route_one,
};
use protocol_droid::domain::feature::FeatureProfile;
use protocol_droid::domain::jsonrpc::{Envelope, Frame, WireId, decode};
use serde_json::value::RawValue;

/// A request with a numeric id and no params — the smallest thing that is
/// still a request.
const NUMERIC_ID: &[u8] = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;

/// A request with a string id. The overwhelmingly common shape, since a server
/// generates ids and generates them as strings.
const STRING_ID: &[u8] = br#"{"jsonrpc":"2.0","id":"a7f3-4c21-9e08","method":"ping"}"#;

/// A string id that needs unescaping, which is the arm that has to allocate.
/// The last character is written as a JSON escape so this file stays ASCII.
const ESCAPED_ID: &[u8] = br#"{"jsonrpc":"2.0","id":"a7f3\u00e9","method":"ping"}"#;
/// A `tools/call`, which is the heaviest method MCP has: a namespaced name to
/// miss in the table, and params to hand back undecoded.
const TOOLS_CALL: &[u8] = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"echo","arguments":{"text":"hello"}}}"#;

/// A notification: no id, so the smallest routable message there is.
const NOTIFICATION: &[u8] = br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

/// A reply, which the router must pass through rather than look up.
const REPLY: &[u8] = br#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#;

/// A two-message batch, the shape a peer sends when it pipelines.
const BATCH: &[u8] =
    br#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","id":2,"method":"tools/list"}]"#;

/// The same `tools/call` params, under a method the table does not have. The
/// pair with [`TOOLS_CALL`] is the lazy-decode measurement: byte-for-byte the
/// same params, and the only difference is whether the name is a method.
const UNROUTABLE: &[u8] = br#"{"jsonrpc":"2.0","id":1,"method":"tools/nope","params":{"name":"echo","arguments":{"text":"hello"}}}"#;

/// The single message out of a benchmark input.
fn single(input: &[u8]) -> Envelope<'_> {
    match decode(input) {
        Ok(Frame::Single(envelope)) => envelope,
        other => panic!("this benchmark input is one message, not {other:?}"),
    }
}

/// A JSON string literal for `text`, so the id benchmarks can hand
/// `WireId::from_raw` real bytes without allocating inside the measured region.
///
/// Takes the *bare* text: `to_raw_value` serialises the `str`, so the quotes are
/// its job and passing them in here would produce `""a""`.
fn raw_value(text: &str) -> Box<RawValue> {
    serde_json::value::to_raw_value(text).unwrap_or_else(|_| panic!("{text} is serialisable"))
}

/// Route one const through the real table, discarding the route.
fn route(b: Bencher, input: &[u8]) {
    b.bench_local(|| {
        let routed: Result<Routed<'_, _, _, _>, RouteError<'_>> =
            route_one(single(black_box(input)), &McpMethodTable::TABLE);
        black_box(routed)
    });
}

fn main() {
    divan::main();
}

#[divan::bench]
fn decode_numeric_id(b: Bencher) {
    b.bench_local(|| black_box(decode(black_box(NUMERIC_ID))));
}

#[divan::bench]
fn decode_string_id(b: Bencher) {
    b.bench_local(|| black_box(decode(black_box(STRING_ID))));
}

#[divan::bench]
fn decode_escaped_id(b: Bencher) {
    // The one arm that has to allocate, so it is measured on its own: a change
    // to the string path shows up here and nowhere else.
    b.bench_local(|| black_box(decode(black_box(ESCAPED_ID))));
}

#[divan::bench]
fn decode_tools_call(b: Bencher) {
    b.bench_local(|| black_box(decode(black_box(TOOLS_CALL))));
}

#[divan::bench]
fn decode_notification(b: Bencher) {
    b.bench_local(|| black_box(decode(black_box(NOTIFICATION))));
}

#[divan::bench]
fn decode_reply(b: Bencher) {
    b.bench_local(|| black_box(decode(black_box(REPLY))));
}

#[divan::bench]
fn decode_batch(b: Bencher) {
    b.bench_local(|| black_box(decode(black_box(BATCH))));
}

#[divan::bench]
fn a_known_method_routes(b: Bencher) {
    route(b, TOOLS_CALL);
}

#[divan::bench]
fn an_unroutable_frame_never_reaches_the_params(b: Bencher) {
    // The half of the lazy-decode claim that costs something. If this converges
    // on `a_known_method_routes`, the decoder is parsing params it was
    // supposed to skip.
    route(b, UNROUTABLE);
}

#[divan::bench]
fn an_unknown_method_without_params_is_refused(b: Bencher) {
    // The cheap half of the same claim: a name miss with no params at all.
    route(b, br#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#);
}

#[divan::bench]
fn a_notification_routes(b: Bencher) {
    route(b, NOTIFICATION);
}

#[divan::bench]
fn a_reply_routes_to_no_method(b: Bencher) {
    route(b, REPLY);
}

#[divan::bench]
fn a_batch_routes_every_element(b: Bencher) {
    let profile = FeatureProfile::all();
    b.bench_local(|| {
        black_box(route_frame(
            black_box(BATCH),
            &McpMethodTable::TABLE,
            &profile,
            &AllowBatching,
        ))
    });
}

#[divan::bench]
fn a_known_method_is_found_in_the_table(b: Bencher) {
    // Isolated from the decode, so a regression in the `match` is
    // distinguishable from a regression in serde.
    b.bench_local(|| black_box(McpMethodTable::TABLE.lookup(black_box("tools/call"))));
}

#[divan::bench]
fn an_unknown_method_misses_in_the_table(b: Bencher) {
    b.bench_local(|| black_box(McpMethodTable::TABLE.lookup(black_box("nope"))));
}

#[divan::bench]
fn a_borrowed_id_is_not_copied(b: Bencher) {
    let raw = raw_value("a7f3-4c21-9e08");
    b.bench_local(|| black_box(WireId::from_raw(black_box(&raw))));
}

#[divan::bench]
fn an_escaped_id_is_unescaped(b: Bencher) {
    let raw = raw_value("a7f3é");
    b.bench_local(|| black_box(WireId::from_raw(black_box(&raw))));
}

#[divan::bench]
fn a_numeric_id_is_written_without_quotes(b: Bencher) {
    b.bench_local(|| black_box(WireId::Number(1).to_json()));
}

#[divan::bench]
fn a_string_id_is_escaped_on_the_way_out(b: Bencher) {
    // Round-trips through serde_json's escaper rather than `Debug`'s, which
    // writes Rust's `\u{7f}` where JSON wants `\u007f`. The benchmark is here
    // to keep that difference from being "simplified" back.
    b.bench_local(|| black_box(WireId::text("a7f3\u{7f}").to_json()));
}
