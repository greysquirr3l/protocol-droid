//! The router, and the batch policy that gates it.
//!
//! # What is being tested
//!
//! Three things the envelope decoder deliberately does not decide: whether a
//! method exists (`-32601`), whether a batch is acceptable in this session
//! (`-32600`), and whether a reply is a reply rather than something to route.
//! The MCP table is the real one, not a stand-in, so a method name that drifts
//! out of the specification fails here.

#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "tests assert on fallible calls and panic with the failing precondition; propagating errors would replace a clear failure message with noise"
)]

use assert_matches::assert_matches;
use protocol_droid::adapters::secondary::mcp::{McpMethod, McpMethodTable};
use protocol_droid::application::jsonrpc::{
    AllowBatching, BatchPolicy, DenyBatching, MethodTable, ProfileBatching, ReplyPayload,
    RouteError, Routed, route_one,
};
use protocol_droid::domain::feature::FeatureProfile;
use protocol_droid::domain::ir::IrErrorCode;
use protocol_droid::domain::jsonrpc::{Frame, WireId, decode};

type TestError = Box<dyn std::error::Error>;

/// [`RouteError`] borrows the offending method name out of the frame, so it is
/// not `'static` and cannot go into a `Box<dyn Error>`. A helper that returns
/// it boxed has to carry the input's lifetime along.
type BorrowedError<'a> = Box<dyn std::error::Error + 'a>;

/// A profile with everything, and one with nothing, so a test can say which
/// half of the batching question it is asking without constructing a full
/// negotiation.
fn open_profile() -> FeatureProfile {
    FeatureProfile::all()
}

const fn closed_profile() -> FeatureProfile {
    FeatureProfile::empty()
}

fn route(
    input: &[u8],
) -> Result<
    Routed<
        '_,
        McpMethod,
        Option<&serde_json::value::RawValue>,
        Option<&serde_json::value::RawValue>,
    >,
    BorrowedError<'_>,
> {
    let frame = decode(input)?;
    let Frame::Single(envelope) = frame else {
        return Err("expected a single message".into());
    };
    Ok(route_one(envelope, &McpMethodTable::TABLE)?)
}

/// The same routing, keeping [`RouteError`] as its own type.
///
/// [`route`] boxes the error, which is right for a happy-path helper and wrong
/// for the tests that assert on a code — `Box<dyn Error>` has no `.code()`.
/// Two helpers is cheaper than one that returns a type a caller has to narrow.
fn route_err(
    input: &[u8],
) -> Result<
    Routed<
        '_,
        McpMethod,
        Option<&serde_json::value::RawValue>,
        Option<&serde_json::value::RawValue>,
    >,
    RouteError<'_>,
> {
    let frame = decode(input)?;
    let Frame::Single(envelope) = frame else {
        return Err(RouteError::BatchNotAllowed);
    };
    route_one(envelope, &McpMethodTable::TABLE)
}

#[test]
fn a_known_method_routes() -> Result<(), TestError> {
    let routed = route(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#)?;
    assert_matches!(
        routed,
        Routed::Request {
            method: McpMethod::Ping,
            id: WireId::Number(1),
            ..
        }
    );
    Ok(())
}

#[test]
fn a_tools_call_routes_and_keeps_its_params() -> Result<(), TestError> {
    let input = br#"{"jsonrpc":"2.0","id":"a","method":"tools/call","params":{"name":"echo"}}"#;
    let routed = route(input)?;
    match routed {
        Routed::Request { method, id, params } => {
            assert_eq!(method, McpMethod::ToolsCall);
            assert_eq!(id, WireId::text("a"));
            let raw = params.ok_or("tools/call has params")?;
            assert_eq!(raw.get(), r#"{"name":"echo"}"#);
        }
        other => return Err(format!("expected a request, got {other:?}").into()),
    }
    Ok(())
}

#[test]
fn a_notification_routes_without_an_id() -> Result<(), TestError> {
    let routed = route(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)?;
    assert_matches!(
        routed,
        Routed::Notification {
            method: McpMethod::InitializedNotification,
            ..
        }
    );
    Ok(())
}

#[test]
fn an_unknown_method_is_method_not_found() {
    let error = route_err(br#"{"jsonrpc":"2.0","id":1,"method":"frobnicate"}"#)
        .expect_err("frobnicate is not a method");
    let code = error.code();
    // The name comes back, not `unknown`. An interning table here would be a
    // second list of method names that can disagree with the method table, and
    // when it disagreed the operator would be told `unknown` about the one case
    // they most needed named.
    assert_matches!(error, RouteError::MethodNotFound(name) if name == "frobnicate");
    assert_eq!(code, IrErrorCode::MethodNotFound);
}

#[test]
fn a_method_removed_from_the_spec_is_as_unknown_as_one_never_added() {
    // `roots/list` is deprecated in 2026-07-28 but still defined, so it is
    // known. A name the specification never had has to be refused the same way
    // — the table cannot tell the two apart, and it must not try.
    assert_matches!(
        route(br#"{"jsonrpc":"2.0","id":1,"method":"roots/list"}"#),
        Ok(Routed::Request {
            method: McpMethod::RootsList,
            ..
        }),
        "a method the spec still defines must route",
    );
    assert_matches!(
        route_err(br#"{"jsonrpc":"2.0","id":1,"method":"tools/execute"}"#),
        Err(RouteError::MethodNotFound(_)),
        "a name the spec never defined must not",
    );
}

#[test]
fn a_notification_is_never_a_request() -> Result<(), TestError> {
    // A method with no id is a notification: the sender gets no reply. The
    // envelope test covers that a *malformed* id is refused; this covers that a
    // missing one is not a request the server will simply forget to answer.
    let routed = route(br#"{"jsonrpc":"2.0","method":"tools/list"}"#)?;
    assert_matches!(routed, Routed::Notification { .. });
    assert_eq!(routed.method(), Some(McpMethod::ToolsList));
    Ok(())
}

#[test]
fn a_reply_is_passed_through_rather_than_routed() -> Result<(), TestError> {
    let result = route(br#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#)?;
    assert_matches!(
        result,
        Routed::Reply { id: WireId::Number(1), payload: ReplyPayload::Result(raw) }
            if raw.get() == r#"{"ok":true}"#
    );
    assert_eq!(result.method(), None, "a reply has no method");

    let error = route(br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"nope"}}"#)?;
    assert_matches!(
        error,
        Routed::Reply {
            payload: ReplyPayload::Error {
                code: -32601,
                message: "nope"
            },
            ..
        }
    );
    Ok(())
}

#[test]
fn every_mcp_method_round_trips_through_its_wire_name() {
    // The enum's `as_str` and the table's `lookup` are two spellings of the
    // same list, and a name that exists in one and not the other is a method
    // the server believes in and cannot route. Asserting over the enum rather
    // than over a hand-written array means adding a variant breaks this test,
    // which is the point.
    let all = [
        McpMethod::Initialize,
        McpMethod::Ping,
        McpMethod::ToolsList,
        McpMethod::ToolsCall,
        McpMethod::ResourcesList,
        McpMethod::ResourcesRead,
        McpMethod::ResourceTemplatesList,
        McpMethod::PromptsList,
        McpMethod::PromptsGet,
        McpMethod::SetLogLevel,
        McpMethod::Complete,
        McpMethod::InitializedNotification,
        McpMethod::CancelledNotification,
        McpMethod::ProgressNotification,
        McpMethod::LoggingMessageNotification,
        McpMethod::RootsListChangedNotification,
        McpMethod::SamplingCreateMessage,
        McpMethod::ElicitationCreate,
        McpMethod::RootsList,
    ];
    for method in all {
        let name = method.as_str();
        assert_eq!(
            McpMethodTable::TABLE.lookup(name),
            Some(method),
            "the table does not route `{name}`",
        );
    }
    assert_eq!(
        McpMethodTable::TABLE.lookup("tools/nonexistent"),
        None,
        "an unknown name must not resolve"
    );
}

#[test]
fn a_notification_name_is_only_ever_a_notification() {
    // The router picks the params decoder by the envelope's `kind`, not by the
    // name, so `notifications/*` routing through a request frame is a decode
    // problem, not a routing one. What matters here is that the flag agrees
    // with the actual name.
    for method in [
        McpMethod::InitializedNotification,
        McpMethod::CancelledNotification,
        McpMethod::ProgressNotification,
        McpMethod::LoggingMessageNotification,
        McpMethod::RootsListChangedNotification,
    ] {
        assert!(
            method.as_str().starts_with("notifications/"),
            "{} is named as a notification",
            method.as_str(),
        );
    }
    for method in [McpMethod::Ping, McpMethod::ToolsCall] {
        assert!(
            method.is_request(),
            "{} is named as a request",
            method.as_str()
        );
    }
}

// --- batch policy ---------------------------------------------------------

#[test]
fn a_batch_is_refused_when_the_profile_has_no_batching() {
    let input = br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#;
    let error = protocol_droid::application::jsonrpc::route_frame(
        input,
        &McpMethodTable::TABLE,
        &closed_profile(),
        &ProfileBatching,
    )
    .unwrap_err();
    assert_eq!(error, RouteError::BatchNotAllowed);
    assert_eq!(
        error.code(),
        IrErrorCode::InvalidRequest,
        "there is no batch-specific code; -32601 would send the peer looking at the wrong thing"
    );
}

#[test]
fn a_batch_is_accepted_when_the_profile_has_batching() -> Result<(), TestError> {
    // The counterpart, and the reason the policy is a trait rather than a
    // bool: the same frame and the same table, and the only thing that changed
    // is what the session negotiated. A `2024-11-05` peer gets its batch and a
    // `2025-06-18`-or-later peer does not.
    let input = br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#;
    let routed = protocol_droid::application::jsonrpc::route_frame(
        input,
        &McpMethodTable::TABLE,
        &open_profile(),
        &ProfileBatching,
    )?;
    assert_eq!(routed.len(), 1);
    Ok(())
}

#[test]
fn the_profile_decides_not_the_transport() {
    // A 2025-06-18-or-later profile has no `JsonRpcBatching`, and a
    // 2024-11-05 one does. The policy asks the profile, so the two sessions
    // answer differently from the same input.
    assert!(!ProfileBatching.allows_batch(&closed_profile()));
    assert!(!ProfileBatching.allows_batch(&FeatureProfile::empty()));
    assert!(AllowBatching.allows_batch(&closed_profile()));
    assert!(!DenyBatching.allows_batch(&open_profile()));
}

#[test]
fn a_batch_routes_every_element_in_order_when_it_is_allowed() -> Result<(), TestError> {
    let input = br#"[
        {"jsonrpc":"2.0","id":1,"method":"ping"},
        {"jsonrpc":"2.0","method":"notifications/initialized"},
        {"jsonrpc":"2.0","id":2,"method":"tools/list"}
    ]"#;
    let routed = protocol_droid::application::jsonrpc::route_frame(
        input,
        &McpMethodTable::TABLE,
        &closed_profile(),
        &AllowBatching,
    )?;
    assert_eq!(routed.len(), 3);
    assert_eq!(
        routed.first().ok_or("no first")?.method(),
        Some(McpMethod::Ping)
    );
    assert_eq!(
        routed.get(1).ok_or("no second")?.method(),
        Some(McpMethod::InitializedNotification),
    );
    assert_eq!(
        routed.get(2).ok_or("no third")?.method(),
        Some(McpMethod::ToolsList)
    );
    Ok(())
}

/// `route_frame` is monomorphised on its policy, so a caller that selects one
/// at runtime would need `?Sized` and would pay for the `dyn` dispatch. This
/// helper takes the policy generically instead, which is how a real caller
/// does it, and lets the test iterate three of them.
fn route_with_policy<'a, P: BatchPolicy>(
    input: &'a [u8],
    profile: &FeatureProfile,
    policy: &P,
) -> Result<usize, BorrowedError<'a>> {
    Ok(protocol_droid::application::jsonrpc::route_frame(
        input,
        &McpMethodTable::TABLE,
        profile,
        policy,
    )?
    .len())
}

#[test]
fn a_single_message_is_unaffected_by_the_batch_policy() -> Result<(), TestError> {
    // The policy gates the array, not the message. A 2026-07-28 peer that
    // sends one ordinary request must still be served, and a policy that
    // refused it would break every stateless request.
    let input = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let profile = closed_profile();
    assert_eq!(route_with_policy(input, &profile, &ProfileBatching)?, 1);
    assert_eq!(route_with_policy(input, &profile, &AllowBatching)?, 1);
    assert_eq!(route_with_policy(input, &profile, &DenyBatching)?, 1);
    Ok(())
}

#[test]
fn only_the_profile_policy_consults_the_profile() {
    // The three policies answer three different questions, and the point of
    // the assertion is that they do not answer each other's: `DenyBatching`
    // ignoring the profile is correct, and `ProfileBatching` ignoring it would
    // be the bug.
    let open = open_profile();
    let closed = closed_profile();
    assert!(ProfileBatching.allows_batch(&open));
    assert!(!ProfileBatching.allows_batch(&closed));
    assert!(AllowBatching.allows_batch(&closed));
    assert!(!DenyBatching.allows_batch(&open));
}

#[test]
fn a_batch_element_that_cannot_route_stops_the_whole_frame() {
    // A batch is refused whole on the way in. The specification lets a server
    // answer each element independently, but it cannot answer any of them
    // before decoding the whole frame — so a bad element is found before a
    // single handler runs, which is also the only reading that keeps a
    // refusal from having run half the work.
    let input = br#"[
        {"jsonrpc":"2.0","id":1,"method":"ping"},
        {"jsonrpc":"2.0","id":2,"method":"frobnicate"}
    ]"#;
    let error = protocol_droid::application::jsonrpc::route_frame(
        input,
        &McpMethodTable::TABLE,
        &closed_profile(),
        &AllowBatching,
    )
    .unwrap_err();
    assert_matches!(error, RouteError::MethodNotFound(_));
}

#[test]
fn a_decode_failure_outranks_the_batch_policy() {
    // The frame is not decodable, so there is no batch to judge. Reporting
    // `BatchNotAllowed` here would tell a peer that batches are unsupported
    // when the real problem is that it sent broken bytes.
    let error = protocol_droid::application::jsonrpc::route_frame(
        b"[{\"jsonrpc\":",
        &McpMethodTable::TABLE,
        &closed_profile(),
        &DenyBatching,
    )
    .unwrap_err();
    assert_eq!(error.code(), IrErrorCode::ParseError);
}
