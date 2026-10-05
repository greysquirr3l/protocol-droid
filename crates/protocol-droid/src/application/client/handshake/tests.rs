//! The handshake, over a transport that records what it was asked to do.
//!
//! A recording transport rather than a socket: everything this module decides
//! happens between the bytes going out and the answer coming back, and a test
//! that has to stand up a server to find out whether `initialize` was the
//! method sent is a test that will also pass when the *server* is what broke.
//!
//! Split by whether a runtime is needed. The two session readers and the params
//! builder are plain functions and are tested without one; driving
//! [`handshake`](super::handshake) needs to poll a future, so those live in
//! [`over_the_port`].
//!
//! The transport there is a test double that touches no real transport, so
//! nothing about it depends on `http` or `stdio` — and a gate that did would
//! leave the handshake's own entry points with no caller in a lean build, which
//! `dead_code` then turns into a build failure. The runtime comes from the
//! `tokio` dev-dependency instead, which is available in every feature
//! configuration and is absent from the published dependency set.

use crate::application::client::ClientConfig;
use crate::application::client::handshake::{
    HandshakeError, discover_session, initialize_params, initialize_session,
};
use crate::domain::ir::JsonObject;
use crate::domain::{
    Capability, CapabilitySet, ProtocolVersion, SERVER_CAPABILITY_WIRE, SupportedVersions,
    TransportKind,
};

/// A `2025-06-18` server that advertises tools, resources and logging.
const MODERN_SERVER: &str = r#"{
    "protocolVersion": "2025-06-18",
    "capabilities": {
        "tools": {"listChanged": true},
        "resources": {"subscribe": true},
        "logging": {}
    },
    "serverInfo": {"name": "test-server", "version": "9.9.9"}
}"#;

/// A `2026-07-28` server: no `serverInfo`, and a *set* of revisions.
const DISCOVERY_ANSWER: &str = r#"{
    "capabilities": {"tools": {}, "prompts": {}},
    "supportedVersions": ["2025-06-18", "2026-07-28"]
}"#;

/// A client at `version`, with nothing advertised.
fn client_at(version: ProtocolVersion) -> ClientConfig {
    ClientConfig::new(version, TransportKind::StreamableHttp)
}

fn supported(versions: &[ProtocolVersion]) -> Result<SupportedVersions, TestError> {
    Ok(SupportedVersions::from_slice(versions)?)
}

fn json(text: &str) -> Result<JsonObject, TestError> {
    Ok(JsonObject::new(text)?)
}

/// The value at a dotted `path`, or `None` if any step is missing.
///
/// Written rather than indexing because `indexing_slicing` is denied here, and
/// because a missing member should read as a failed `assert_eq!` rather than as
/// a panic from the test harness — the value under test is the document, and a
/// panic in the middle of it says nothing about which member was wrong.
fn at<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cursor = value;
    for key in path.split('.') {
        cursor = cursor.get(key)?;
    }
    Some(cursor)
}

// ---- reading an initialize answer ----

#[test]
fn initialize_session_takes_the_servers_revision() -> Result<(), TestError> {
    let config = client_at(ProtocolVersion::V2025_06_18);
    let set = supported(&[ProtocolVersion::V2025_06_18])?;

    let session = initialize_session(
        &json(MODERN_SERVER)?,
        ProtocolVersion::V2025_06_18,
        &set,
        &config,
    )?;

    assert_eq!(session.version, ProtocolVersion::V2025_06_18);
    Ok(())
}

#[test]
fn initialize_session_reads_the_servers_capabilities() -> Result<(), TestError> {
    let config = client_at(ProtocolVersion::V2025_06_18);
    let set = supported(&[ProtocolVersion::V2025_06_18])?;

    let session = initialize_session(
        &json(MODERN_SERVER)?,
        ProtocolVersion::V2025_06_18,
        &set,
        &config,
    )?;

    let caps = session.server_caps;
    assert!(caps.contains(Capability::Tools));
    assert!(caps.contains(Capability::ToolsListChanged));
    assert!(caps.contains(Capability::Resources));
    assert!(caps.contains(Capability::ResourcesSubscribe));
    assert!(caps.contains(Capability::Logging));
    Ok(())
}

#[test]
fn a_capability_the_server_did_not_advertise_is_not_in_the_session() -> Result<(), TestError> {
    // Prompts was not advertised. A reader that defaulted to "on" for anything
    // it did not see would put prompts in the profile, and the peer would be
    // told it may send prompts/list to a server that has none.
    let config = client_at(ProtocolVersion::V2025_06_18);
    let set = supported(&[ProtocolVersion::V2025_06_18])?;

    let session = initialize_session(
        &json(MODERN_SERVER)?,
        ProtocolVersion::V2025_06_18,
        &set,
        &config,
    )?;

    assert!(!session.server_caps.contains(Capability::Prompts));
    assert!(!session.server_caps.contains(Capability::Completions));
    Ok(())
}

#[test]
fn a_list_changed_of_false_is_not_the_same_as_no_tools() -> Result<(), TestError> {
    // A server with tools that will not announce changes is a real server, and
    // reading `false` as "absent" would make the client believe it has no tools
    // at all — refusing every tools/call against a server that would answer.
    let config = client_at(ProtocolVersion::V2025_06_18);
    let set = supported(&[ProtocolVersion::V2025_06_18])?;
    let answer = json(
        r#"{"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":false}},
            "serverInfo":{"name":"n","version":"1"}}"#,
    )?;

    let session = initialize_session(&answer, ProtocolVersion::V2025_06_18, &set, &config)?;

    assert!(session.server_caps.contains(Capability::Tools));
    assert!(!session.server_caps.contains(Capability::ToolsListChanged));
    Ok(())
}

#[test]
fn initialize_session_records_both_names() -> Result<(), TestError> {
    let config = client_at(ProtocolVersion::V2025_06_18);
    let set = supported(&[ProtocolVersion::V2025_06_18])?;

    let session = initialize_session(
        &json(MODERN_SERVER)?,
        ProtocolVersion::V2025_06_18,
        &set,
        &config,
    )?;

    let info = session.server_info.clone().ok_or(TestError::NoServerInfo)?;
    assert_eq!(info.name, "test-server");
    assert_eq!(info.version, "9.9.9");
    // The client half is a fact about this build, not something the peer told
    // us, so it is here whether or not the server asked for it.
    assert!(session.client_info.is_some());
    Ok(())
}

#[test]
fn an_answer_of_no_protocol_version_is_refused_by_name() -> Result<(), TestError> {
    let config = client_at(ProtocolVersion::V2025_06_18);
    let set = supported(&[ProtocolVersion::V2025_06_18])?;
    let answer = json(r#"{"capabilities":{},"serverInfo":{"name":"n","version":"1"}}"#)?;

    let error = expect_err(initialize_session(
        &answer,
        ProtocolVersion::V2025_06_18,
        &set,
        &config,
    ))?;

    let HandshakeError::MalformedResult { field, .. } = &error else {
        return Err(TestError::WrongShape { got: error });
    };
    assert_eq!(*field, "protocolVersion");
    Ok(())
}

#[test]
fn an_answer_of_no_capabilities_is_refused_by_name() -> Result<(), TestError> {
    let config = client_at(ProtocolVersion::V2025_06_18);
    let set = supported(&[ProtocolVersion::V2025_06_18])?;
    let answer =
        json(r#"{"protocolVersion":"2025-06-18","serverInfo":{"name":"n","version":"1"}}"#)?;

    let error = expect_err(initialize_session(
        &answer,
        ProtocolVersion::V2025_06_18,
        &set,
        &config,
    ))?;

    let HandshakeError::MalformedResult { field, .. } = &error else {
        return Err(TestError::WrongShape { got: error });
    };
    assert_eq!(*field, "capabilities");
    Ok(())
}

#[test]
fn a_revision_this_build_cannot_speak_is_refused() -> Result<(), TestError> {
    let config = client_at(ProtocolVersion::V2025_06_18);
    let set = supported(&[ProtocolVersion::V2025_06_18])?;
    let answer = json(
        r#"{"protocolVersion":"2099-01-01","capabilities":{},
            "serverInfo":{"name":"n","version":"1"}}"#,
    )?;

    let error = expect_err(initialize_session(
        &answer,
        ProtocolVersion::V2025_06_18,
        &set,
        &config,
    ))?;

    // The refusal has to name the revision, because "handshake failed" sends
    // the reader to the transport, and the transport is fine.
    assert!(
        matches!(&error, HandshakeError::Negotiation(_)),
        "expected a negotiation refusal, got {error:?}"
    );
    Ok(())
}

#[test]
fn an_answer_cannot_be_something_that_is_not_an_object() {
    // The reason the handshake's reader has no "that was not an object" branch:
    // the port hands over a `JsonObject`, which cannot hold an array or a
    // scalar, so the check would be unreachable. Asserted on the type so that
    // stays true — the day someone relaxes `JsonObject` to carry any JSON, this
    // fails, and the reader is where the new check then has to go.
    // The whole `Result` rather than the error half after a `?`, because the
    // claim here is that construction *fails* — propagating the failure out of
    // the test would assert the opposite.
    assert!(matches!(json("[1, 2, 3]"), Err(TestError::Json(_))));
    assert!(matches!(json("not json at all"), Err(TestError::Json(_))));
    assert!(matches!(json("\"a bare string\""), Err(TestError::Json(_))));
}

// ---- reading a discovery answer ----

#[test]
fn discovery_picks_the_newest_revision_both_sides_speak() -> Result<(), TestError> {
    // The server listed them oldest-first and the client's preference is the
    // *older* revision. Neither of those may decide: the newest they share is
    // what the negotiation asks for.
    let config = client_at(ProtocolVersion::V2025_06_18);
    let set = supported(&[ProtocolVersion::V2025_06_18, ProtocolVersion::V2026_07_28])?;

    let session = discover_session(&json(DISCOVERY_ANSWER)?, &set, &config)?;

    assert_eq!(session.version, ProtocolVersion::V2026_07_28);
    Ok(())
}

#[test]
fn a_revision_this_build_has_never_heard_of_is_skipped() -> Result<(), TestError> {
    // A server that has added a revision this build predates. Refusing the
    // whole answer over one unfamiliar entry would mean a server cannot add a
    // revision without breaking every older client.
    let config = client_at(ProtocolVersion::V2026_07_28);
    let set = supported(&[ProtocolVersion::V2026_07_28])?;
    let answer = json(r#"{"capabilities":{},"supportedVersions":["2099-01-01","2026-07-28"]}"#)?;

    let session = discover_session(&answer, &set, &config)?;

    assert_eq!(session.version, ProtocolVersion::V2026_07_28);
    Ok(())
}

#[test]
fn a_server_offering_only_unknown_revisions_is_refused() -> Result<(), TestError> {
    let config = client_at(ProtocolVersion::V2026_07_28);
    let set = supported(&[ProtocolVersion::V2026_07_28])?;
    let answer = json(r#"{"capabilities":{},"supportedVersions":["2099-01-01"]}"#)?;

    let error = expect_err(discover_session(&answer, &set, &config))?;

    let HandshakeError::MalformedResult { field, .. } = &error else {
        return Err(TestError::WrongShape { got: error });
    };
    assert_eq!(*field, "supportedVersions");
    Ok(())
}

#[test]
fn an_answer_of_no_supported_versions_is_refused() -> Result<(), TestError> {
    let config = client_at(ProtocolVersion::V2026_07_28);
    let set = supported(&[ProtocolVersion::V2026_07_28])?;

    let error = expect_err(discover_session(
        &json(r#"{"capabilities":{}}"#)?,
        &set,
        &config,
    ))?;

    let HandshakeError::MalformedResult { field, .. } = &error else {
        return Err(TestError::WrongShape { got: error });
    };
    assert_eq!(*field, "supportedVersions");
    Ok(())
}

#[test]
fn discovery_carries_no_server_name_because_the_schema_has_no_field_for_it() -> Result<(), TestError>
{
    let config = client_at(ProtocolVersion::V2026_07_28);
    let set = supported(&[ProtocolVersion::V2026_07_28])?;

    let session = discover_session(&json(DISCOVERY_ANSWER)?, &set, &config)?;

    // Absent rather than invented. A fabricated `serverInfo` would put a name
    // in a log line that no peer ever claimed.
    assert!(session.server_info.is_none());
    assert!(session.client_info.is_some());
    Ok(())
}

#[test]
fn discovery_reads_capabilities_out_of_the_same_wire_shape() -> Result<(), TestError> {
    let config = client_at(ProtocolVersion::V2026_07_28);
    let set = supported(&[ProtocolVersion::V2026_07_28])?;

    let session = discover_session(&json(DISCOVERY_ANSWER)?, &set, &config)?;

    assert!(session.server_caps.contains(Capability::Tools));
    assert!(session.server_caps.contains(Capability::Prompts));
    assert!(!session.server_caps.contains(Capability::Resources));
    Ok(())
}

// ---- the client's own offer on the wire ----

#[test]
fn the_offer_names_the_revision_and_identifies_the_client() -> Result<(), TestError> {
    let config = client_at(ProtocolVersion::V2025_03_26);

    let params = initialize_params(ProtocolVersion::V2025_03_26, &config)?;

    let value: serde_json::Value = serde_json::from_str(params.as_str())?;
    assert_eq!(
        at(&value, "protocolVersion"),
        Some(&serde_json::json!("2025-03-26"))
    );
    // `clientInfo` is required by the schema with both members required, so
    // leaving it out is a request the peer rejects, not an anonymous one.
    assert!(at(&value, "clientInfo.name").is_some_and(serde_json::Value::is_string));
    assert!(at(&value, "clientInfo.version").is_some_and(serde_json::Value::is_string));
    Ok(())
}

#[test]
fn a_capability_with_no_children_renders_as_a_bare_object() -> Result<(), TestError> {
    let config = client_at(ProtocolVersion::V2025_06_18)
        .with_capabilities(CapabilitySet::empty().with(Capability::Roots));

    let params = initialize_params(ProtocolVersion::V2025_06_18, &config)?;

    // `"roots": true` is not a `ClientCapabilities`. The schema wants an
    // object, and a server validating strictly rejects the whole request.
    let value: serde_json::Value = serde_json::from_str(params.as_str())?;
    assert_eq!(
        at(&value, "capabilities"),
        Some(&serde_json::json!({ "roots": {} }))
    );
    Ok(())
}

#[test]
fn a_nested_capability_renders_under_its_own_key() -> Result<(), TestError> {
    let config = client_at(ProtocolVersion::V2025_06_18).with_capabilities(
        CapabilitySet::empty()
            .with(Capability::Roots)
            .with(Capability::RootsListChanged),
    );

    let params = initialize_params(ProtocolVersion::V2025_06_18, &config)?;

    // The two levels are the point. `Capability::as_str` is "roots.listChanged",
    // and using it as a key would send one flat key the schema does not have.
    let value: serde_json::Value = serde_json::from_str(params.as_str())?;
    assert_eq!(
        at(&value, "capabilities"),
        Some(&serde_json::json!({ "roots": { "listChanged": true } }))
    );
    Ok(())
}

#[test]
fn a_three_level_capability_renders_at_all_three_levels() -> Result<(), TestError> {
    let config = client_at(ProtocolVersion::V2025_11_25).with_capabilities(
        CapabilitySet::empty().with(Capability::TasksRequestsSamplingCreateMessage),
    );

    let params = initialize_params(ProtocolVersion::V2025_11_25, &config)?;

    // This is the row that made the wire table a key *path* rather than a key
    // and an optional child: a two-level row cannot express this shape.
    let value: serde_json::Value = serde_json::from_str(params.as_str())?;
    assert_eq!(
        at(&value, "capabilities"),
        Some(
            &serde_json::json!({ "tasks": { "requests": { "sampling": { "createMessage": true } } } })
        )
    );
    Ok(())
}

#[test]
fn an_undeclared_capability_is_not_advertised() -> Result<(), TestError> {
    let config = client_at(ProtocolVersion::V2025_06_18);

    let params = initialize_params(ProtocolVersion::V2025_06_18, &config)?;

    let value: serde_json::Value = serde_json::from_str(params.as_str())?;
    assert_eq!(at(&value, "capabilities"), Some(&serde_json::json!({})));
    Ok(())
}

// ---- the wire table, walked ----

#[test]
fn every_server_capability_row_reads_back_from_the_wire() -> Result<(), TestError> {
    // Builds, for each row, exactly the object a server would write, and reads
    // it back. A row whose key does not round-trip is a capability the client
    // can never detect, and the failure is silent: the feature simply never
    // turns on, with nothing anywhere reporting it.
    let config = client_at(ProtocolVersion::V2025_11_25);
    let set = supported(&[ProtocolVersion::V2025_11_25])?;

    for (capability, path) in SERVER_CAPABILITY_WIRE {
        let object = wire_object_for(path)?;
        let session = discover_session(&json(&object)?, &set, &config)?;
        if !session.server_caps.contains(*capability) {
            return Err(TestError::RowDidNotRoundTrip {
                capability: *capability,
                path,
            });
        }
    }
    Ok(())
}

#[test]
fn no_two_server_capabilities_share_a_key_path() -> Result<(), TestError> {
    // Two capabilities on one path would make the writer unable to say which of
    // them it meant, so the second would silently overwrite the first.
    let mut paths: Vec<_> = SERVER_CAPABILITY_WIRE
        .iter()
        .map(|(_, path)| *path)
        .collect();
    paths.sort_unstable();
    let before = paths.len();
    paths.dedup();
    if paths.len() != before {
        return Err(TestError::DuplicateWirePath);
    }
    Ok(())
}

/// The `capabilities` object a server would write to assert `path`.
///
/// A path of one key is the object itself, because that is what the schema has
/// for a parent capability; a longer path ends in `true`.
fn wire_object_for(path: &[&str]) -> Result<String, TestError> {
    let Some((last, parents)) = path.split_last() else {
        return Err(TestError::EmptyWirePath);
    };
    let mut root = serde_json::Map::new();
    let mut cursor = &mut root;
    for key in parents {
        let entry = cursor
            .entry((*key).to_owned())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        let serde_json::Value::Object(parent) = entry else {
            return Err(TestError::NotAnObject);
        };
        cursor = parent;
    }
    cursor.insert((*last).to_owned(), serde_json::Value::Bool(true));
    let mut answer = serde_json::Map::new();
    answer.insert("capabilities".to_owned(), serde_json::Value::Object(root));
    // `discover_session` reads this, and it is the function the row is walked
    // through because it takes the result whole rather than a capabilities
    // object. A fixture with only the half under test would fail on the other
    // half's absence and prove nothing about the row.
    answer.insert(
        "supportedVersions".to_owned(),
        serde_json::Value::from(vec!["2025-11-25"]),
    );
    Ok(serde_json::Value::Object(answer).to_string())
}

/// Everything this module can go wrong by, other than a wrong answer.
#[derive(Debug, thiserror::Error)]
pub(super) enum TestError {
    #[error("the test expected an error and got a session")]
    ExpectedAnError,
    #[error("the error was not the shape under test: {got:?}")]
    WrongShape { got: HandshakeError },
    #[error("the answer carried no serverInfo")]
    NoServerInfo,
    #[error("a key path ran into something that is not an object")]
    NotAnObject,
    #[error("a key path was empty")]
    EmptyWirePath,
    #[error("the row for {capability:?} at {path:?} did not read back")]
    RowDidNotRoundTrip {
        capability: Capability,
        path: &'static [&'static str],
    },
    #[error("two capabilities share one key path")]
    DuplicateWirePath,
    #[error(transparent)]
    Json(#[from] crate::domain::ir::JsonObjectError),
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
    #[error(transparent)]
    Versions(#[from] crate::domain::SupportedVersionsError),
    #[error(transparent)]
    Handshake(#[from] HandshakeError),
    #[error(transparent)]
    Client(#[from] crate::application::client::ClientError),
    #[error("call {0} was never made")]
    NoCallAt(usize),
}

#[path = "tests/over_the_port.rs"]
mod over_the_port;

/// The error half of a result, for a test that expects the call to fail.
///
/// A function rather than a `.map` at each call site because the shape under
/// test is the *error*, and a `TestError` that wrapped one would have the test
/// matching on its own wrapper instead of on what the handshake produced.
fn expect_err<T>(result: Result<T, HandshakeError>) -> Result<HandshakeError, TestError> {
    match result {
        Ok(_) => Err(TestError::ExpectedAnError),
        Err(error) => Ok(error),
    }
}
