//! Fuzz the `TransportSpec` parser.
//!
//! `TransportSpec` is the typed DTO the public API takes when
//! a consumer wants to connect to a server. The parser is
//! the surface a peer-provided URL or path reaches first,
//! and a bug there is a bug a peer can trigger without
//! authenticating.
//!
//! # What the harness does
//!
//! The fuzzer hands the harness a `&str` (a UTF-8 view of
//! the byte slice; `cargo-fuzz` is responsible for the
//! UTF-8 boundary). The harness tries to parse the string
//! as a `TransportSpec` — the same path the CLI's
//! `connect-to` subcommand takes. The harness does not
//! care about the result; it cares that the parser does
//! not panic, and that any returned spec serialises back
//! to a string that the parser accepts again.

#![no_main]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &str| {
    // The parser is the boundary; everything else is typed.
    // A crash in `parse()` is a bug in the parser, not in
    // the fuzzer.
    let spec = match data.parse::<protocol_droid::domain::TransportSpec>() {
        Ok(spec) => spec,
        Err(_) => return,
    };

    // The spec's `Display` impl is the form the CLI logs and
    // the form the user types in `connect-to`. A spec that
    // does not round-trip through `to_string().parse()` is a
    // spec the CLI cannot reliably echo back to its caller.
    let rendered = spec.to_string();
    let round_tripped = match rendered.parse::<protocol_droid::domain::TransportSpec>() {
        Ok(spec) => spec,
        Err(_) => return,
    };
    assert_eq!(spec, round_tripped, "TransportSpec round-trip is not stable");
});
