//! The decoder must never panic, whatever bytes it is handed.
//!
//! # What this replaces, and why
//!
//! The task asked for a `cargo-fuzz` target. A fuzz target is a second crate
//! with its own `Cargo.toml`, a corpus directory, and a `nightly` requirement —
//! and this workspace is stable-only, with a toolchain file pinning 1.98.1
//! exactly and a standing rule against adding a crate without written
//! justification. T23 is the fuzzing campaign.
//!
//! So the *property* is delivered here and the *search* is T23's. A property
//! test over arbitrary bytes and a fuzzer's coverage-guided search are
//! different tools: this one runs in `cargo test` on every commit and on every
//! platform in the matrix, which the campaign does not. Neither replaces the
//! other, and the honest description of the state is "the property is checked
//! continuously, the search is scheduled for T23" rather than "fuzzed".
//!
//! # Why this is a security property
//!
//! The decoder takes bytes from the network. An `unwrap`, a panicking slice
//! index, or an arithmetic assumption on this path is a remote denial of
//! service, so it is checked rather than reviewed.
//!
//! # The shape of every body here
//!
//! `proptest!`'s `name in strategy` form generates `let (): () = $body;` — a
//! body that has to evaluate to `()` and may not `return` out of it. So nothing
//! here propagates with `?`, ends with `Ok(())`, or uses `return`. A fallible
//! step is an `if let` whose body carries the assertions, and a step that has
//! to fail is a `prop_assert!` followed by an `if let` that narrows to the
//! success case. More verbose than `?`, and it compiles under the denied test
//! lints without a single suppression.

use proptest::prelude::*;
use protocol_droid::adapters::secondary::mcp::McpMethodTable;
use protocol_droid::application::jsonrpc::{
    AllowBatching, MethodTable, ProfileBatching, route_frame,
};
use protocol_droid::domain::RequestId;
use protocol_droid::domain::feature::FeatureProfile;
use protocol_droid::domain::jsonrpc::{Frame, WireId, decode};

/// A profile with every feature, so the batch policy is not what is under test.
fn open() -> FeatureProfile {
    FeatureProfile::all()
}

proptest! {
    // Whatever `decode` returns, it does not panic — and if it is a frame,
    // every accessor on it is safe too.
    #[test]
    fn the_decoder_never_panics_on_arbitrary_bytes(
        bytes in prop::collection::vec(any::<u8>(), 0..512),
    ) {
        if let Ok(frame) = decode(&bytes) {
            // Touching the result is part of the claim: a panic in a `Debug`
            // impl or an accessor is as much a failure as one in the parse.
            let _ = format!("{frame:?}");
            for envelope in frame.envelopes() {
                let _ = envelope.kind();
                let _ = envelope.method();
                let _ = envelope.id();
                let _ = envelope.params().map(|params| params.get().len());
                let _ = envelope.outcome();
                let _ = envelope.routeable();
                if let Some(id) = envelope.id() {
                    let _ = id.to_json();
                }
            }
        }
    }

    // Routing never panics, and every refusal carries a code.
    //
    // A refusal with no code would leave a transport inventing one, which is
    // where wrong codes come from.
    #[test]
    fn routing_never_panics_on_arbitrary_bytes(
        bytes in prop::collection::vec(any::<u8>(), 0..512),
    ) {
        let profile = open();
        let refusals = [
            route_frame(&bytes, &McpMethodTable::TABLE, &profile, &AllowBatching).err(),
            route_frame(&bytes, &McpMethodTable::TABLE, &profile, &ProfileBatching).err(),
        ];
        for error in refusals.into_iter().flatten() {
            let _ = error.code().code();
        }
    }

    // The paths that run *after* a successful parse never panic, for every way
    // a member can be spelled.
    #[test]
    fn routing_never_panics_on_arbitrary_member_values(
        method in prop::sample::select(vec![
            "ping",
            "tools/call",
            "tools/list",
            "notifications/initialized",
            "frobnicate",
            "",
        ]),
        id in prop::sample::select(vec![
            r#""1""#, "1", "-1", "null", "true", r#"{"a":1}"#, "[1]", "1.5", "0", r#""""#,
        ]),
        params in prop::option::of(prop::sample::select(vec![
            "null", "{}", r#"{"name":"x"}"#, "[]", r#""text""#, "1",
        ])),
    ) {
        let params = params.map_or_else(String::new, |value| format!(r#","params":{value}"#));
        let input = format!(r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}"{params}}}"#);
        let _ = decode(input.as_bytes());
        let _ = route_frame(input.as_bytes(), &McpMethodTable::TABLE, &open(), &AllowBatching);
    }

    // A decoded numeric id renders back to the integer the peer sent.
    //
    // The rendered id has to parse as the same integer, or a response built
    // from it names a request the peer never sent.
    #[test]
    fn a_numeric_id_survives_the_round_trip(number in any::<i64>()) {
        let input = format!(r#"{{"jsonrpc":"2.0","id":{number},"method":"ping"}}"#);
        if let Ok(Frame::Single(envelope)) = decode(input.as_bytes()) {
            if let Some(value) = envelope.id().and_then(WireId::as_number) {
                prop_assert_eq!(value, number);
                let rendered = value.to_string();
                let parsed = serde_json::from_str::<i64>(&rendered);
                prop_assert!(parsed.is_ok(), "a rendered i64 is a JSON number");
                prop_assert_eq!(parsed.unwrap_or(0), number);
            } else {
                prop_assert!(false, "a numeric id must decode as a number");
            }
        } else {
            prop_assert!(false, "a request with a valid id must decode");
        }
    }

    // A string id survives the borrowed-to-owned conversion with no
    // re-escaping.
    #[test]
    fn a_string_id_survives_the_round_trip(text in ".{0,64}") {
        if let Ok(encoded) = serde_json::to_string(&text) {
            let input = format!(r#"{{"jsonrpc":"2.0","id":{encoded},"method":"ping"}}"#);
            if let Ok(Frame::Single(envelope)) = decode(input.as_bytes()) {
                if let Some(WireId::Text(borrowed)) = envelope.id() {
                    prop_assert_eq!(borrowed, text.as_str());
                    prop_assert_eq!(RequestId::text(borrowed), RequestId::text(text.as_str()));
                } else {
                    prop_assert!(false, "a string id must decode as a string");
                }
            } else {
                prop_assert!(false, "a request with a valid id must decode");
            }
        } else {
            prop_assert!(false, "a string always serialises");
        }
    }

    // A text id is written back quoted, so a response built from it is valid
    // JSON.
    #[test]
    fn a_written_id_is_always_valid_json(text in ".{0,64}") {
        if let Ok(encoded) = serde_json::to_string(&text) {
            let input = format!(r#"{{"jsonrpc":"2.0","id":{encoded},"method":"ping"}}"#);
            if let Ok(Frame::Single(envelope)) = decode(input.as_bytes()) {
                if let Some(id) = envelope.id() {
                    let parsed = serde_json::from_str::<serde_json::Value>(&id.to_json());
                    prop_assert!(parsed.is_ok(), "a rendered id must be valid JSON");
                    prop_assert_eq!(
                        parsed.unwrap_or(serde_json::Value::Null),
                        serde_json::Value::String(text),
                    );
                } else {
                    prop_assert!(false, "the envelope has an id");
                }
            } else {
                prop_assert!(false, "a request with a valid id must decode");
            }
        } else {
            prop_assert!(false, "a string always serialises");
        }
    }

    // A method is either in the table or is refused with `-32601`. Never both,
    // never neither.
    //
    // This is the property behind the unknown-method code: the refusal has to
    // be total over the name space, not merely correct for the names somebody
    // thought to test.
    #[test]
    fn a_method_is_known_or_refused(name in ".{0,64}") {
        let known = McpMethodTable::TABLE.lookup(&name).is_some();
        if let Ok(encoded) = serde_json::to_string(&name) {
            let input = format!(r#"{{"jsonrpc":"2.0","id":1,"method":{encoded}}}"#);
            let profile = open();
            match route_frame(input.as_bytes(), &McpMethodTable::TABLE, &profile, &AllowBatching) {
                Ok(_) => prop_assert!(known, "a method the table lacks must be refused"),
                Err(error) => {
                    prop_assert!(!known, "a method the table has must not be refused");
                    prop_assert_eq!(error.code().code(), -32601);
                }
            }
        } else {
            prop_assert!(false, "a string always serialises");
        }
    }

    // A batch is routed whole or refused whole.
    //
    // An element that is individually unroutable stops the frame, so a
    // partially routed batch is not something the decoder can produce.
    #[test]
    fn a_batch_is_all_or_nothing(
        methods in prop::collection::vec(
            prop::sample::select(vec!["ping", "tools/list", "frobnicate", ""]),
            1..4,
        ),
    ) {
        let elements: Vec<String> = methods
            .iter()
            .map(|method| format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}"}}"#))
            .collect();
        let input = format!("[{}]", elements.join(","));
        let profile = open();

        if let Ok(routed) = route_frame(
            input.as_bytes(),
            &McpMethodTable::TABLE,
            &profile,
            &AllowBatching,
        ) {
            prop_assert_eq!(routed.len(), methods.len());
            for method in &methods {
                prop_assert!(
                    McpMethodTable::TABLE.lookup(method).is_some(),
                    "a routed batch contained `{method}`, which is not a method",
                );
            }
        }
    }
}
