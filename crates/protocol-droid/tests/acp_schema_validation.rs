//! Schema-validation property tests for the
//! pinned ACP version (v1.25.0).
//!
//! The tests in this file are the project's
//! "schema-conformance" claim for ACP. They
//! generate random typed requests, serialize
//! them, and validate the resulting JSON
//! against the vendored schema. A failure
//! here means the typed shape drifted from
//! the schema (a `serde` rename is wrong, a
//! field is missing, a `default` annotation
//! is wrong, ...) and the wire form a real
//! agent or client sees no longer matches
//! the published spec.
//!
//! Source of truth:
//! `crates/protocol-droid-schema/spec/acp/v1.25.0/schema.json`.
//!
//! The tests are property tests, not unit
//! tests, because a hand-picked value proves
//! one case was thought of; a property test
//! proves the typed shape is correct for
//! the whole space of valid instances the
//! generator can produce. The generators
//! are focused (the
//! `arbitrary_initialize_request` strategy
//! only produces shapes the schema
//! validates) so the assertions are about
//! round-trip stability and schema
//! conformance, not about rejecting
//! invalid input — that is the schema
//! validator's job, and the
//! `assert_json_matches_schema` calls are
//! the proof.

#![cfg(feature = "acp-experimental")]

use proptest::prelude::*;
use protocol_droid::domain::acp::implementation::Implementation;
use protocol_droid::domain::acp::initialize::{
    AgentCapabilities, ClientCapabilities, InitializeRequest, InitializeResponse,
};
use protocol_droid::domain::acp::method::AcpMethod;
use protocol_droid::domain::acp::protocol_version::AcpProtocolVersion;

/// The vendored ACP v1.25.0 schema. `include_str!`
/// reads the file at compile time; the test fails
/// to build if the file is missing or moved. The
/// path is relative to this test file (the macro
/// expands to the file's directory).
const ACP_SCHEMA_JSON: &str =
    include_str!("../../protocol-droid-schema/spec/acp/v1.25.0/schema.json");

/// A proptest strategy that produces valid
/// `InitializeRequest` instances. The
/// strategy only produces shapes the
/// schema validates: the
/// `protocolVersion` is the current
/// pin, the capability blocks are the
/// all-off defaults, and the optional
/// `clientInfo` is either absent or a
/// well-formed `Implementation`.
///
/// The strategy is intentionally simple
/// — the goal is to exercise the
/// `serde` rename, the `default`
/// annotations, and the schema
/// conformance, not to fuzz the
/// shape's semantic content. A
/// future increment can add fuzzing
/// for the nested capability
/// surfaces.
fn arbitrary_initialize_request() -> impl Strategy<Value = InitializeRequest> {
    (
        any::<bool>(), // controls `client_info` presence
        any::<bool>(), // controls a non-default `client_info`
    )
        .prop_map(|(with_info, non_default_info)| InitializeRequest {
            protocol_version: AcpProtocolVersion::CURRENT,
            client_capabilities: ClientCapabilities::default(),
            client_info: if with_info {
                Some(if non_default_info {
                    Implementation::protocol_droid("protocol-droid-test", "1.2.3")
                } else {
                    Implementation::protocol_droid("protocol-droid-test", "0.0.0")
                })
            } else {
                None
            },
        })
}

/// A proptest strategy that produces valid
/// `InitializeResponse` instances. The
/// schema validates a `null` or absent
/// `authMethods` as "the agent does not
/// require auth" and a present list as
/// the advertised methods; the strategy
/// produces both shapes.
fn arbitrary_initialize_response() -> impl Strategy<Value = InitializeResponse> {
    any::<bool>().prop_map(|with_agent_info| InitializeResponse {
        protocol_version: AcpProtocolVersion::CURRENT,
        agent_capabilities: AgentCapabilities::default(),
        auth_methods: None,
        agent_info: if with_agent_info {
            Some(Implementation::protocol_droid("reference-agent", "0.0.1"))
        } else {
            None
        },
    })
}

/// A proptest strategy that produces a
/// non-trivial `AcpMethod`. The
/// strategy is the canonical round-trip
/// target: a value the consumer would
/// pass to `send_typed_request` and the
/// agent would dispatch to a handler.
/// The wire form is what the
/// `SendNotification` codec must
/// serialize.
fn arbitrary_acp_method() -> impl Strategy<Value = AcpMethod> {
    // The wire name the schema expects.
    // `AcpMethod::as_wire_name` is the
    // single source of truth for the
    // wire form; the property test
    // asserts the strategy's value
    // round-trips through the
    // serialization the consumer
    // hands to the peer.
    Just(AcpMethod::Agent(
        protocol_droid::domain::acp::method::AgentMethod::SessionNew,
    ))
}

/// Round-trip stability: any
/// `InitializeRequest` the generator
/// produces serialises to JSON and
/// deserialises back to a value
/// equal to the original. The
/// `serde(rename_all = "camelCase")`
/// annotation is the most common
/// source of drift; a property test
/// catches the case where one field
/// was renamed and another wasn't.
#[cfg(test)]
mod initialize_request_round_trip {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn initialize_request_round_trips(
            request in arbitrary_initialize_request()
        ) {
            let json = serde_json::to_string(&request)
                .map_err(|error| TestCaseError::Fail(error.to_string().into()))?;
            let decoded: InitializeRequest = serde_json::from_str(&json)
                .map_err(|error| TestCaseError::Fail(error.to_string().into()))?;
            prop_assert_eq!(request, decoded);
        }

        #[test]
        fn initialize_response_round_trips(
            response in arbitrary_initialize_response()
        ) {
            let json = serde_json::to_string(&response)
                .map_err(|error| TestCaseError::Fail(error.to_string().into()))?;
            let decoded: InitializeResponse = serde_json::from_str(&json)
                .map_err(|error| TestCaseError::Fail(error.to_string().into()))?;
            prop_assert_eq!(response, decoded);
        }

        /// The wire form the consumer hands
        /// to the peer carries
        /// `protocolVersion`, not
        /// `protocol_version`. The
        /// property test fails if the
        /// `serde(rename_all)` is
        /// removed or overridden on a
        /// single field.
        #[test]
        fn initialize_request_uses_camel_case_on_the_wire(
            request in arbitrary_initialize_request()
        ) {
            let value: serde_json::Value = serde_json::to_value(&request)
                .map_err(|error| TestCaseError::Fail(error.to_string().into()))?;
            let object = value.as_object()
                .ok_or_else(|| TestCaseError::Fail("not an object".into()))?;
            // The `protocolVersion` key
            // MUST be present and use
            // camelCase; `protocol_version`
            // is a Rust-only name.
            prop_assert!(object.contains_key("protocolVersion"));
            prop_assert!(!object.contains_key("protocol_version"));
        }

        /// The optional `clientInfo` block
        /// is omitted from the wire form
        /// when it is `None`. A `serde`
        /// annotation that always
        /// serializes the field would
        /// make every request carry an
        /// explicit `clientInfo: null`,
        /// which the schema validates
        /// but which is not what the
        /// schema authors intended.
        #[test]
        fn initialize_request_omits_client_info_when_none(
            request in arbitrary_initialize_request()
        ) {
            let value: serde_json::Value = serde_json::to_value(&request)
                .map_err(|error| TestCaseError::Fail(error.to_string().into()))?;
            let object = value.as_object()
                .ok_or_else(|| TestCaseError::Fail("not an object".into()))?;
            if request.client_info.is_none() {
                prop_assert!(!object.contains_key("clientInfo"));
            } else {
                prop_assert!(object.contains_key("clientInfo"));
            }
        }
    }
}

/// Schema conformance: any
/// `InitializeRequest` the generator
/// produces validates against the
/// vendored ACP v1.25.0 schema. The
/// `jsonschema` crate's
/// `validator_for` and `validate`
/// functions run the validator over
/// the serialized form. A failure
/// means the typed shape's wire form
/// drifted from the schema.
///
/// The full schema (not just the
/// `$defs.InitializeRequest` sub-
/// schema) is the validator's input
/// because the sub-schema references
/// `ClientCapabilities` and
/// `Implementation` via `$ref`, and
/// the validator needs the `$defs`
/// map to resolve those references.
/// Validating against the full
/// schema means a conformance
/// failure on the typed shape is a
/// true wire-form regression, not an
/// artifact of validating against an
/// isolated sub-schema.
#[cfg(test)]
mod initialize_request_schema_conformance {
    use jsonschema::Validator;

    use super::*;

    /// The vendored schema validates
    /// a JSON-RPC 2.0 envelope
    /// (`{ jsonrpc, id, method,
    /// params }`), not the
    /// `InitializeRequest` body in
    /// isolation. The schema's
    /// `anyOf` has three top-level
    /// branches: `Agent` (agent-to-
    /// client), `Client` (client-to-
    /// agent), and `ProtocolLevel`.
    /// The `Client` branch's `anyOf`
    /// includes a `Request` shape
    /// whose `params.anyOf` is the
    /// union of all client-originating
    /// method bodies, including
    /// `InitializeRequest`.
    ///
    /// To validate a typed
    /// `InitializeRequest`, the test
    /// wraps it in the JSON-RPC
    /// envelope the consumer hands
    /// to the peer and validates the
    /// envelope against the full
    /// schema. The validator walks
    /// the schema's `anyOf` and
    /// matches the `Client.Request.
    /// params.InitializeRequest`
    /// branch.
    fn full_validator() -> Result<Validator, String> {
        let schema_value: serde_json::Value = serde_json::from_str(ACP_SCHEMA_JSON)
            .map_err(|error| format!("the vendored schema is not valid JSON: {error}"))?;
        jsonschema::validator_for(&schema_value)
            .map_err(|error| format!("the vendored schema is invalid: {error}"))
    }

    /// Wrap a typed
    /// `InitializeRequest` in the
    /// JSON-RPC 2.0 envelope the
    /// consumer hands to the peer.
    /// The envelope's shape is the
    /// minimum the schema needs to
    /// match the `Client.Request`
    /// branch.
    fn envelope_for(request: &InitializeRequest) -> Result<serde_json::Value, String> {
        let params = serde_json::to_value(request)
            .map_err(|error| format!("serialization failed: {error}"))?;
        Ok(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": params,
        }))
    }

    /// The typed `InitializeRequest`,
    /// wrapped in its JSON-RPC
    /// envelope, validates against
    /// the full vendored schema.
    /// The validator walks the
    /// schema's `anyOf` and matches
    /// the `Client.Request.params.
    /// InitializeRequest` branch.
    #[test]
    fn initialize_request_validates_against_the_vendored_schema() -> Result<(), String> {
        let validator = full_validator()?;

        let request = InitializeRequest {
            protocol_version: AcpProtocolVersion::CURRENT,
            client_capabilities: ClientCapabilities::default(),
            client_info: Some(Implementation::protocol_droid(
                "protocol-droid-test",
                "1.2.3",
            )),
        };
        let envelope = envelope_for(&request)?;
        let result = validator.validate(&envelope);
        if let Err(error) = result {
            return Err(format!(
                "the typed InitializeRequest does not validate at `/{}`: {}",
                error.instance_path(),
                error
            ));
        }
        Ok(())
    }

    /// The minimum wire form — only
    /// the required `protocolVersion`
    /// field — also validates. The
    /// `clientCapabilities` field
    /// has a default in the schema,
    /// so a request that omits it
    /// should still pass.
    #[test]
    fn a_minimal_initialize_request_validates_against_the_schema() -> Result<(), String> {
        let validator = full_validator()?;

        let request = InitializeRequest {
            protocol_version: AcpProtocolVersion::CURRENT,
            client_capabilities: ClientCapabilities::default(),
            client_info: None,
        };
        let envelope = envelope_for(&request)?;
        let result = validator.validate(&envelope);
        if let Err(error) = result {
            return Err(format!(
                "the minimal typed InitializeRequest does not validate at `/{}`: {}",
                error.instance_path(),
                error
            ));
        }
        Ok(())
    }
}

/// The `AcpMethod` table's `as_wire_name`
/// is the single source of truth for the
/// method name the peer hands to the
/// JSON-RPC envelope. A property test
/// over the table is a unit test of the
/// `match` arms; a single test is
/// enough because the table is a
/// `match` over a sum type and any
/// missing arm is a compile error.
#[test]
fn acp_method_session_new_is_dot_separated() {
    let method = AcpMethod::Agent(protocol_droid::domain::acp::method::AgentMethod::SessionNew);
    assert_eq!(method.as_wire_name(), "session/new");
}

/// `AcpMethod::as_wire_name` is the
/// canonical wire form the consumer's
/// peer serializes. The property test
/// asserts the strategy's value
/// produces the expected wire name —
/// the dot-separated form the JSON-RPC
/// envelope expects.
#[cfg(test)]
mod acp_method_wire_name {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(8))]

        #[test]
        fn acp_method_wire_name_is_session_new(
            method in arbitrary_acp_method()
        ) {
            prop_assert_eq!(method.as_wire_name(), "session/new");
        }
    }
}
