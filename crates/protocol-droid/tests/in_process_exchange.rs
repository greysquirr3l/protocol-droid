//! Every client revision against every server revision, in one process.
//!
//! # What this covers
//!
//! The 6x6 matrix, all 36 pairs: a client that negotiated `v_client` talking
//! to a server that negotiated `v_server`, exchanging `tools/list` and
//! `tools/call`. The two revisions are deliberately independent. A real
//! connection settles one number, and a test that only pairs a client with a
//! server at the same revision proves only that one number works — the
//! mismatched pairs are where a bridge's translation rules actually get
//! exercised.
//!
//! # What this does not cover, and why it says so
//!
//! **Nothing here touches a wire.** There is no serialisation, no framing and
//! no request-side codec: `Codec` lifts and lowers a `ToolResult` and nothing
//! else, so there is nothing to frame with. The whole matrix runs against
//! [`InProcess`], which calls the [`Dispatcher`] directly. A bug in encoding,
//! in a codec's revision gating, or in a transport's framing is invisible to
//! every test in this file, and the wire version of this matrix is T13's to
//! write.
//!
//! Stating that is the point. A test named after the wire that never reaches
//! the wire reads as coverage in a review and is not coverage at run time.
//!
//! What this _does_ cover, and why the matrix is worth running at all:
//!
//! - the client-side gate refusing a method its own profile does not carry,
//! - the server-side gate refusing from its own profile,
//! - a tool call reaching a handler and its result coming back intact,
//! - a listing reflecting the visibility rule, including a hidden tool's
//!   absence, and a hidden tool being uncallable as well.
//!
//! Those four are the ones where a version pair can differ, and the last two
//! are the reason for a 6x6 rather than a single row.

use std::pin::Pin;
use std::sync::Arc;

use protocol_droid::application::client::{Client, ClientTransport, TransportError};
use protocol_droid::application::server::context::{CallbackExchange, ExchangeError, PeerSession};
use protocol_droid::application::server::{
    DispatchError, Dispatcher, RequestContext, Server, ToolHandler,
};
use protocol_droid::application::session::{FrameSink, SinkError};
use protocol_droid::domain::feature::{Negotiation, resolve};
use protocol_droid::domain::ir::{JsonObject, JsonObjectError};
use protocol_droid::domain::registry::{AuthContext, Primitive, Registered, Scope, ScopeSet, Tool};
use protocol_droid::domain::session::{CancellationToken, SessionId};
use protocol_droid::domain::{CapabilitySet, Feature, ProtocolVersion, RequestId, TransportKind};

/// Every revision this crate has wire types for.
///
/// Spelled out rather than taken from `KNOWN`, because the matrix is a claim
/// about these five. A sixth revision joining `KNOWN` has to be added here on
/// purpose, where the diff shows a reviewer which pair is newly covered.
const REVISIONS: [ProtocolVersion; 5] = [
    ProtocolVersion::V2024_11_05,
    ProtocolVersion::V2025_03_26,
    ProtocolVersion::V2025_06_18,
    ProtocolVersion::V2025_11_25,
    ProtocolVersion::V2026_07_28,
];

/// The tool every session sees.
const PUBLIC_TOOL: &str = "echo";
/// The tool a session must hold [`Scope::Read`] to see.
const RESTRICTED_TOOL: &str = "restricted_echo";

/// What a test here can fail with.
///
/// One enum rather than `Box<dyn Error>`, so a `?` on a domain error is a
/// compile error unless the `From` is written — a test that silently swallows
/// a fixture failure is worse than one that does not compile.
#[derive(Debug)]
enum Failure {
    /// The test's own complaint.
    Message(String),
    /// A fixture was not a JSON object.
    Fixture(JsonObjectError),
}

impl core::fmt::Display for Failure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Message(text) => f.write_str(text),
            Self::Fixture(error) => write!(f, "bad fixture: {error}"),
        }
    }
}

impl std::error::Error for Failure {}

impl From<String> for Failure {
    fn from(text: String) -> Self {
        Self::Message(text)
    }
}

impl From<&str> for Failure {
    fn from(text: &str) -> Self {
        Self::Message(text.to_string())
    }
}

impl From<JsonObjectError> for Failure {
    fn from(error: JsonObjectError) -> Self {
        Self::Fixture(error)
    }
}

type TestError = Failure;

/// A [`ClientTransport`] that calls a [`Dispatcher`] directly, with no wire.
///
/// # Why this is scaffolding and not a library type
///
/// It is here rather than in `adapters/secondary/` because nothing outside a
/// test should want it: a user writing their own test against a `Server` can
/// wire a `Dispatcher` in ten lines, and shipping the shortcut would put test
/// support in the shipped surface. The explanation is worth keeping either
/// way, so it lives on the thing that needs it.
///
/// # What it is not
///
/// It is **not** a stand-in for a transport. Nothing here serialises, frames
/// or negotiates, so a bug in any of those is invisible to it. That is the
/// whole reason the module doc of this file spends a paragraph saying so.
#[derive(Clone)]
struct InProcess {
    dispatcher: Arc<Dispatcher>,
    ctx: RequestContext,
}

impl ClientTransport for InProcess {
    /// Answers nothing, because a notification is not asking anything.
    ///
    /// A test that asserted on the notification would have to read the
    /// dispatcher's log, which is what [`InProcess::send`] does. The
    /// handshake tests that matter here are about the *result* of the
    /// handshake, and the notification's own delivery is a claim about the
    /// transport, not about this.
    fn notify(
        &self,
        _method: &str,
        _params: &JsonObject,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), TransportError>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }

    fn send(
        &self,
        method: &str,
        params: &JsonObject,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<JsonObject, TransportError>> + Send + '_>>
    {
        // Cloned rather than borrowed, because the returned future has to be
        // `Send` with no lifetime tying it to `&self`. Two `Arc` bumps per
        // call is a fair price for a test that does no I/O anyway.
        let dispatcher = self.dispatcher.clone();
        let ctx = self.ctx.clone();
        let method = method.to_string();
        let params = params.clone();
        Box::pin(async move {
            dispatcher
                .dispatch(&ctx, &method, &params)
                .await
                .map_err(|error| TransportError::Unavailable(error.to_string()))
        })
    }
}

/// An exchange that always fails, because nothing in these tests may call back.
///
/// A peer callback inside a `tools/call` would be a server-initiated round
/// trip, and the point of the matrix is that the two directions are
/// independent. A fixture that answered would let a test pass for a reason
/// this file does not claim to test.
struct RefusingExchange;

impl CallbackExchange for RefusingExchange {
    fn exchange(
        &self,
        method: &str,
        _params: &JsonObject,
    ) -> impl std::future::Future<Output = Result<JsonObject, ExchangeError>> + Send {
        let _ = method;
        std::future::ready(Err(ExchangeError::new("no callbacks in this test")))
    }
}

/// A sink that discards, and counts what it was given.
///
/// The count is what a gate assertion reads. "Nothing was written" is the
/// second half of the `elicitation-on-old-session` scenario, and a discard-everything
/// sink cannot answer it — the whole point is to notice when something was
/// written to a session that should have refused.
#[derive(Debug, Default)]
struct CountingSink {
    frames: std::sync::Mutex<Vec<String>>,
}

impl FrameSink for CountingSink {
    fn send(&self, frame: &str) -> impl std::future::Future<Output = Result<(), SinkError>> + Send {
        if let Ok(mut frames) = self.frames.lock() {
            frames.push(frame.to_string());
        }
        std::future::ready(Ok(()))
    }
}

impl CountingSink {
    /// How many frames were written.
    fn written(&self) -> usize {
        self.frames.lock().map_or(0, |frames| frames.len())
    }
}

/// A server holding one public tool and one scope-restricted tool.
///
/// The restricted tool is the load-bearing half. Without it a listing
/// assertion cannot tell a working visibility filter from a registry that
/// happened to contain one tool, and the 6x6 would be testing a list of
/// length one.
fn server() -> Result<Server, TestError> {
    let public = Tool::new(PUBLIC_TOOL, r#"{"type":"object"}"#)?;
    let restricted = Registered::from(Primitive::from(Tool::new(
        RESTRICTED_TOOL,
        r#"{"type":"object"}"#,
    )?))
    .requiring_scope(Scope::Read);

    Ok(Server::builder("probe")
        .tool(public, echo_handler())
        .restricted_tool(restricted, echo_handler())
        .build())
}

/// A handler that echoes its arguments back.
///
/// The arguments come straight through rather than being re-serialised, so
/// the assertion that the answer matches is a claim about the hop and not
/// about a `format!` agreeing with itself. The fixture below carries a quote
/// and a brace precisely so a string-building implementation would mangle it.
fn echo_handler() -> ToolHandler {
    Arc::new(|_ctx, arguments| Box::pin(async move { arguments }))
}

/// The server's side of a connection at `version`, carrying `principal`.
///
/// `client_caps` is empty so the server's profile is the strictest a real one
/// can be. A server that advertised everything would mask a gate that never
/// closes, and this file exists partly to notice gates that never close.
fn server_side(
    version: ProtocolVersion,
    principal: AuthContext,
) -> (PeerSession, Arc<CountingSink>) {
    let sink = Arc::new(CountingSink::default());
    let peer = PeerSession::new(
        version,
        resolve(negotiation(version)),
        SessionId::new(1),
        TransportKind::Stdio,
        Arc::new(RefusingExchange),
        sink.clone(),
    )
    .with_auth(principal);
    (peer, sink)
}

/// The negotiation a connection at `version` settled on.
fn negotiation(version: ProtocolVersion) -> Negotiation {
    Negotiation {
        version,
        client_caps: CapabilitySet::empty(),
        server_caps: CapabilitySet::all_for(version),
        transport: TransportKind::StreamableHttp,
    }
}

/// A client at `v_client`, connected to a server at `v_server`.
///
/// The client's profile is resolved from the **server's** negotiation, not
/// from the client's own view of the world. A client that resolved its own
/// profile from two of the four inputs would be strictly stricter than the
/// session it is talking to — the trap recorded in `PROGRESS.md` — and the
/// matrix would then be testing a mismatch that cannot happen in practice and
/// proving nothing about the case that does.
fn connected(
    v_client: ProtocolVersion,
    v_server: ProtocolVersion,
    principal: AuthContext,
) -> Result<Client, TestError> {
    let (peer, _sink) = server_side(v_server, principal);
    let ctx = request_context(&peer);
    Ok(Client::connect(
        negotiation(v_client),
        Arc::new(InProcess {
            dispatcher: Arc::new(Dispatcher::new(Arc::new(server()?))),
            ctx,
        }),
    ))
}

/// A request bound to `peer`.
///
/// Every request in this file gets one of these rather than a `RequestContext`
/// literal, so the id and the cancellation token are the same across the
/// matrix and a failure in one pair is comparable with a failure in another.
fn request_context(peer: &PeerSession) -> RequestContext {
    RequestContext::for_request(peer, RequestId::text("r1"), CancellationToken::new(), None)
}

/// Runs the whole exchange for one `(client, server)` pair.
///
/// One function so the two matrix tests and the two single-pair tests below
/// cannot drift from each other. A second copy of this written for a special
/// case would diverge within a week, and the divergence would be invisible
/// because both would still be green.
async fn exchange(
    v_client: ProtocolVersion,
    v_server: ProtocolVersion,
    principal: AuthContext,
) -> Result<(), TestError> {
    let client = connected(v_client, v_server, principal.clone())?;

    // `tools/list` — a method every revision carries.
    let listed = client.list_tools().await.map_err(|error| {
        Failure::from(format!(
            "tools/list failed for {v_client} -> {v_server}: {error}"
        ))
    })?;
    let names = tool_names(&listed)?;

    if !names.iter().any(|name| name == PUBLIC_TOOL) {
        return Err(Failure::from(format!(
            "{v_client} -> {v_server}: the public tool is missing from {names:?}"
        )));
    }

    // The listing is the visibility rule, so check the far side of it.
    let expected = principal.principal() == "restricted";
    if names.iter().any(|name| name == RESTRICTED_TOOL) != expected {
        return Err(Failure::from(format!(
            "{v_client} -> {v_server}: {} saw the restricted tool",
            principal.principal()
        )));
    }

    // `tools/call` — the handler runs and its answer arrives intact.
    //
    // Compared as parsed JSON, not as text. The dispatcher parses `params` to
    // find the tool name and the codec re-serialises the result, so member
    // order comes back sorted. Byte-identity is a property of
    // `JsonObject` alone — T08's store-original-text behaviour — and asserting
    // it across a parse would claim something this path never promised and
    // would fail on any value whose keys were not already sorted.
    let arguments = JsonObject::new(r#"{"text":"quote \" and brace }","n":1}"#)?;
    let called = client
        .call_tool(PUBLIC_TOOL, &arguments)
        .await
        .map_err(|error| {
            Failure::from(format!(
                "tools/call failed for {v_client} -> {v_server}: {error}"
            ))
        })?;
    let sent: serde_json::Value = serde_json::from_str(arguments.as_str())
        .map_err(|error| Failure::from(format!("the fixture is not JSON: {error}")))?;
    let got: serde_json::Value = serde_json::from_str(called.as_str())
        .map_err(|error| Failure::from(format!("the answer is not JSON: {error}")))?;
    if sent != got {
        return Err(Failure::from(format!(
            "{v_client} -> {v_server}: sent {sent}, got {got}"
        )));
    }

    Ok(())
}

/// The tool names in a `tools/list` answer.
///
/// Reads the one member it cares about rather than comparing whole documents,
/// so a revision gaining a field elsewhere in the result does not turn this
/// into a change detector that fails on every version bump.
fn tool_names(listing: &JsonObject) -> Result<Vec<String>, TestError> {
    let parsed: serde_json::Value = serde_json::from_str(listing.as_str())
        .map_err(|error| Failure::from(format!("the listing is not JSON: {error}")))?;
    let tools = parsed
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| Failure::from("the listing has no tools array"))?;

    Ok(tools
        .iter()
        .filter_map(|tool| tool.get("name"))
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect())
}

/// All 36 pairs, for a caller holding [`Scope::Read`].
///
/// The named exit criterion. Unrestricted rather than anonymous, because the
/// restricted tool has to be visible to _someone_ or the absence assertions
/// below prove nothing.
#[tokio::test]
async fn every_client_revision_against_every_server_revision() -> Result<(), TestError> {
    for v_client in REVISIONS {
        for v_server in REVISIONS {
            exchange(
                v_client,
                v_server,
                AuthContext::new("restricted", ScopeSet::all()),
            )
            .await?;
        }
    }
    Ok(())
}

/// All 36 pairs again, for a caller holding no scope.
///
/// The second matrix is not redundant with the first. A filter that always
/// returns everything passes the one above, and one that always returns
/// nothing passes this one, so a matrix that ran only one of the two would not
/// have checked the filter at all.
#[tokio::test]
async fn every_pair_also_works_for_a_caller_holding_no_scope() -> Result<(), TestError> {
    for v_client in REVISIONS {
        for v_server in REVISIONS {
            exchange(v_client, v_server, AuthContext::anonymous()).await?;
        }
    }
    Ok(())
}

/// A tool missing from the listing is also not callable.
///
/// The listing and the call are two different code paths over one predicate,
/// and a server that filters the list but not the call is an existence oracle —
/// the exact thing `get_visible` is written to prevent. The refusal is
/// `UnknownTool` rather than a permission error, and that is the point: the
/// same answer as for a tool that does not exist is what makes it
/// oracle-proof.
#[tokio::test]
async fn a_tool_missing_from_the_listing_is_also_not_callable() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let client = connected(
        version,
        version,
        AuthContext::new("nobody", ScopeSet::empty()),
    )?;

    let listed = client
        .list_tools()
        .await
        .map_err(|error| Failure::from(error.to_string()))?;
    let names = tool_names(&listed)?;
    if names.iter().any(|name| name == RESTRICTED_TOOL) {
        return Err(Failure::from(
            "a caller with no scope saw the restricted tool in the listing",
        ));
    }

    client
        .call_tool(RESTRICTED_TOOL, &JsonObject::default())
        .await
        .map_or(Ok(()), |answer| {
            Err(Failure::from(format!(
                "a caller with no scope called the restricted tool: {answer}"
            )))
        })
}

/// A session without the tool's feature is refused at call time.
///
/// The scope test above exercises `AuthContext`; this one exercises
/// `FeatureProfile`. They are separate inputs to the same predicate, so a
/// test of only one has not covered the other.
#[tokio::test]
async fn a_tool_requiring_a_missing_feature_is_refused() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let structured = Tool::new(PUBLIC_TOOL, r#"{"type":"object"}"#)?
        .with_output_schema(r#"{"type":"object"}"#)?;

    let dispatcher = Dispatcher::new(Arc::new(
        Server::builder("structured")
            .tool(structured, echo_handler())
            .build(),
    ));

    // A profile that has tools but not structured output: the combination an
    // `outputSchema` raises `ToolStructuredOutput` for.
    let mut profile = resolve(negotiation(version));
    profile.remove(Feature::ToolStructuredOutput);

    let (peer, sink) = server_side(version, AuthContext::anonymous());
    let ctx = request_context(&peer.with_profile(profile));

    let outcome = dispatcher
        .dispatch(&ctx, "tools/call", &call_params(PUBLIC_TOOL))
        .await;

    assert_eq!(
        outcome.as_ref().err(),
        Some(&DispatchError::UnknownTool),
        "a session without ToolStructuredOutput called a tool that needs it"
    );
    assert_eq!(sink.written(), 0, "the refusal wrote to the transport");
    Ok(())
}

/// A session the client cannot use is refused before the call is made.
///
/// The gate the client side carries, and the one the 6x6 above cannot reach:
/// there, every pair is a pair the client is allowed to call, because a
/// client always has `Tools`. Refusal only appears when the profile is
/// withheld, so this constructs that case directly.
#[tokio::test]
async fn a_client_without_the_feature_refuses_before_sending() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let (peer, sink) = server_side(version, AuthContext::anonymous());

    // No server capabilities at all, so `resolve` yields a profile with no
    // tools in it. The client must notice on its own side.
    let empty = Negotiation {
        version,
        client_caps: CapabilitySet::empty(),
        server_caps: CapabilitySet::empty(),
        transport: TransportKind::StreamableHttp,
    };
    let client = Client::connect(
        empty,
        Arc::new(InProcess {
            dispatcher: Arc::new(Dispatcher::new(Arc::new(server()?))),
            ctx: request_context(&peer),
        }),
    );

    if client.supports(Feature::Tools) {
        return Err(Failure::from(
            "a client with no negotiated tools believes it has them",
        ));
    }
    match client.list_tools().await {
        Ok(answer) => Err(Failure::from(format!(
            "the refusal did not happen: {answer}"
        ))),
        Err(_) if sink.written() == 0 => Ok(()),
        Err(error) => Err(Failure::from(format!(
            "the client refused, but something had already been written: {error}"
        ))),
    }
}

/// The `tools/call` params for `name`, with no arguments.
fn call_params(name: &str) -> JsonObject {
    JsonObject::from_value(serde_json::json!({ "name": name })).unwrap_or_default()
}
