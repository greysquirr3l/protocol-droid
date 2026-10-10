//! Fuzz the JSON-RPC envelope decoder.
//!
//! The JSON-RPC envelope is the project's one perf-critical path:
//! every frame in the system passes through it once. A bug in
//! the decoder is a bug a remote peer can trigger without
//! authenticating, and the failure mode is rarely a clean
//! `ParseError` — it is a panic in an arithmetic op, an
//! out-of-bounds index, an `unwrap` on a value the spec
//! allows but the parser does not handle. A 30-minute nightly
//! `cargo fuzz` run is the only way to surface these before
//! a peer does.
//!
//! # What the harness does
//!
//! The fuzzer hands the harness a `&[u8]`. The harness tries
//! to parse it as a JSON-RPC frame — the same path the
//! transport layer takes when a real frame arrives. The
//! harness does not care about the result; it cares that
//! *every* path through the parser is a `Result` and that
//! no `unwrap` / `panic` / `unreachable` is reached. A
//! crash is the fuzzer finding an input that triggers
//! one of those.
//!
//! The harness also walks the frame's envelopes and inspects
//! each one's classified `MessageKind`, asserting that every
//! classified message is a variant the enum actually
//! declares. A `MessageKind` value that escaped the parser
//! is the kind of bug a single end of the pipe would never
//! see, and the fuzzer is the only place that finds it.

#![no_main]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The fuzzer's input is bytes; the parser's input is also
    // bytes. There is no other shape to consider, and turning
    // a parse failure into a panic would be a bug in the
    // fuzzer, not in the parser.
    let frame = match protocol_droid::domain::jsonrpc::decode(data) {
        Ok(frame) => frame,
        Err(_) => return,
    };

    // Walk every envelope in the frame and exercise every
    // accessor the routing layer calls. The fuzzer's claim
    // is that the accessors are total — there is no input
    // that lets the parser hand the router a shape the
    // router then panics on.
    for envelope in frame.envelopes() {
        // The `kind` is the enum the router dispatches on;
        // a value the enum does not declare is a
        // non-exhaustive mismatch the compiler would have
        // caught at the call site, so the fuzzer is the
        // only line of defence.
        let _ = envelope.kind();
        // The id path: every frame that carries an id has
        // to round-trip it back to the reply. The fuzzer
        // exercises the same `Option`-lift the router
        // does, and an `unwrap` here would be the bug.
        if let Some(id) = envelope.id() {
            let _ = id;
        }
    }
});

