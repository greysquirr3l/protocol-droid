//! The dispatcher's own tests.
//!
//! Out of the router so that reading `dispatch` means reading the routing and
//! not the test module that happens to sit underneath it. The precedent is
//! already in the tree: `adapters/primary/stdio/serve/stateless/tests.rs`.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{DispatchError, Dispatcher};
use crate::application::server::context::{
    CallbackExchange, ExchangeError, PeerSession, RequestContext,
};
use crate::application::server::{Server, ToolHandler};
use crate::application::session::{FrameSink, SinkError};
use crate::domain::feature::{Negotiation, resolve};
use crate::domain::ir::{JsonObject, JsonObjectError};
use crate::domain::registry::{AuthContext, Primitive, Registered, Scope, ScopeSet, Tool};
use crate::domain::session::{CancellationToken, SessionId};
use crate::domain::{Capability, CapabilitySet, ProtocolVersion, RequestId, TransportKind};

/// What a test here can fail with.
#[derive(Debug)]
enum Failure {
    /// The test's own complaint.
    Message(String),
    /// A fixture was not a JSON object.
    Fixture(JsonObjectError),
    /// A result the dispatcher built did not read back as JSON.
    Result(serde_json::Error),
}

impl core::fmt::Display for Failure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Message(text) => f.write_str(text),
            Self::Fixture(error) => write!(f, "bad fixture: {error}"),
            Self::Result(error) => write!(f, "unreadable result: {error}"),
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

impl From<DispatchError> for Failure {
    fn from(error: DispatchError) -> Self {
        Self::Message(error.to_string())
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Self::Result(error)
    }
}

type TestError = Failure;

/// The test's own complaint.
///
/// A helper rather than `Err("...".into())` at each site, because that
/// spelling settles the module's error type on `&str` — which converts to
/// both itself and `Box<dyn Error>`, so the compiler picks it from the
/// first early return and every `?` in the body then has no valid
/// conversion. A vague error type is a compile error that points at the
/// `?` and not at the `Err` that caused it.
fn fail(message: impl Into<String>) -> TestError {
    Failure::Message(message.into())
}

/// An exchange that refuses, so no test here can accidentally pass by
/// having a callback answered.
struct NoExchange;

impl CallbackExchange for NoExchange {
    fn exchange(
        &self,
        method: &str,
        _params: &JsonObject,
    ) -> impl std::future::Future<Output = Result<JsonObject, ExchangeError>> + Send {
        let _ = method;
        std::future::ready(Err(ExchangeError::new("no callbacks here")))
    }
}

/// A sink that discards.
struct NoSink;

impl FrameSink for NoSink {
    fn send(
        &self,
        _frame: &str,
    ) -> impl std::future::Future<Output = Result<(), SinkError>> + Send {
        std::future::ready(Ok(()))
    }
}

/// A handler that answers with `answer`.
fn answering(answer: &'static str) -> ToolHandler {
    let answer: JsonObject = JsonObject::new(answer).unwrap_or_default();
    Arc::new(move |_ctx, _args| {
        let answer = answer.clone();
        Box::pin(async move { answer })
    })
}

/// A dispatcher over one plain tool and one scope-restricted tool.
fn dispatcher() -> Result<Dispatcher, TestError> {
    let restricted = Registered::from(Primitive::from(Tool::new(
        "secret",
        r#"{"type":"object"}"#,
    )?))
    .requiring_scope(Scope::Read);

    Ok(Dispatcher::new(Arc::new(
        Server::builder("test")
            .tool(Tool::new("echo", r#"{"type":"object"}"#)?, answering("{}"))
            .restricted_tool(restricted, answering("{}"))
            .build(),
    )))
}

/// A session at `version` with `scopes`, negotiating `server_caps`.
fn peer(version: ProtocolVersion, scopes: ScopeSet, server_caps: CapabilitySet) -> PeerSession {
    PeerSession::new(
        version,
        resolve(Negotiation {
            version,
            client_caps: CapabilitySet::empty(),
            server_caps,
            transport: TransportKind::StreamableHttp,
        }),
        SessionId::new(1),
        TransportKind::Stdio,
        Arc::new(NoExchange),
        Arc::new(NoSink),
    )
    .with_auth(AuthContext::new("tester", scopes))
}

/// A request on `peer`.
fn ctx(peer: &PeerSession) -> RequestContext {
    RequestContext::for_request(peer, RequestId::text("r1"), CancellationToken::new(), None)
}

/// The server capabilities a fully-featured session negotiates at
/// `version`.
fn caps(version: ProtocolVersion) -> CapabilitySet {
    CapabilitySet::all_for(version)
}

/// A session on session `id`, so a test can hold two at once.
///
/// The existing [`peer`] pins session 1, which is fine until a test needs
/// to prove that two sessions are not the same session.
fn peer_on(
    version: ProtocolVersion,
    scopes: ScopeSet,
    server_caps: CapabilitySet,
    id: u64,
) -> PeerSession {
    PeerSession::new(
        version,
        resolve(Negotiation {
            version,
            client_caps: CapabilitySet::empty(),
            server_caps,
            transport: TransportKind::StreamableHttp,
        }),
        SessionId::new(id),
        TransportKind::Stdio,
        Arc::new(NoExchange),
        Arc::new(NoSink),
    )
    .with_auth(AuthContext::new("tester", scopes))
}

/// The one tool the idempotency tests call.
fn charge_tool() -> Result<Tool, TestError> {
    Ok(Tool::new("charge", r#"{"type":"object"}"#)?)
}

/// `initialize` params for a client that offers `version`.
fn init_params(version: &str) -> Result<JsonObject, TestError> {
    Ok(JsonObject::new(
        r#"{"protocolVersion":""#.to_string()
            + version
            + r#"","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}"#,
    )?)
}

/// An `initialize` body whose client advertises `capabilities`.
fn init_params_with(version: &str, capabilities: &str) -> Result<JsonObject, TestError> {
    Ok(JsonObject::new(
        r#"{"protocolVersion":""#.to_string()
            + version
            + r#"","capabilities":"#
            + capabilities
            + r#","clientInfo":{"name":"probe","version":"0"}}"#,
    )?)
}

/// An IR object read back as a `serde_json` value.
fn parsed(object: &JsonObject) -> Result<serde_json::Value, TestError> {
    Ok(serde_json::from_str(object.as_str())?)
}

/// The advertised capabilities are resolved for the revision that was
/// negotiated, so an older client is never told it has something that
/// revision cannot express.
///
/// The session here is on `2025-06-18`, which is the newest revision in
/// this build's profile that gates `elicitation`. The client offers
/// `2024-11-05`, which has no such capability. Reading the profile off the
/// session — as this did — would have answered `elicitation: {}` to a
/// client on a revision whose schema has nowhere to put it.
#[tokio::test]
async fn advertised_capabilities_follow_the_negotiated_revision() -> Result<(), TestError> {
    let session_version = ProtocolVersion::V2025_06_18;
    let peer = peer(
        session_version,
        ScopeSet::all(),
        CapabilitySet::all_for(session_version),
    );
    let result = dispatcher()?
        .dispatch(&ctx(&peer), "initialize", &init_params("2024-11-05")?)
        .await
        .map_err(|error| Failure::Message(format!("the handshake refused: {error}")))?;

    let capabilities = parsed(&result)?
        .get("capabilities")
        .cloned()
        .ok_or("the handshake advertised no capabilities")?;
    assert!(
        capabilities.get("elicitation").is_none(),
        "2024-11-05 has no elicitation capability: {capabilities}"
    );
    Ok(())
}

/// A capability the client advertises narrows what the server answers it
/// can do, because the profile is resolved from what the client actually
/// sent rather than from an assumption that it sent everything.
#[tokio::test]
async fn the_client_capabilities_are_read_not_assumed() -> Result<(), TestError> {
    let session_version = ProtocolVersion::V2025_11_25;
    let peer = peer(
        session_version,
        ScopeSet::all(),
        CapabilitySet::all_for(session_version),
    );
    dispatcher()?
        .dispatch(
            &ctx(&peer),
            "initialize",
            &init_params_with("2025-11-25", r#"{"roots":{"listChanged":true}}"#)?,
        )
        .await
        .map_err(|error| Failure::Message(format!("the handshake refused: {error}")))?;
    Ok(())
}

/// The handshake answers with the revision the client negotiated.
///
/// The distinction is the test's whole point: a client offering a version
/// this build cannot speak has already been negotiated down by the time a
/// request reaches here, so echoing the *request* would tell the peer it
/// got the revision it asked for when it did not.
#[tokio::test]
async fn initialize_reports_the_negotiated_revision() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let peer = peer(version, ScopeSet::all(), CapabilitySet::all_for(version));
    // The session is on 2025-06-18 and the client offers 2024-11-05. The
    // answer must be the client's, not the session's: the session was
    // constructed before any frame arrived and knows nothing about who is
    // calling. Negotiating against it would have every client answered with
    // whatever revision the process happened to be built for.
    let result = dispatcher()?
        .dispatch(&ctx(&peer), "initialize", &init_params("2024-11-05")?)
        .await
        .map_err(|error| Failure::Message(format!("the handshake refused: {error}")))?;

    let reported = parsed(&result)?
        .get("protocolVersion")
        .and_then(serde_json::Value::as_str)
        .ok_or("the handshake did not report a revision")?
        .to_string();
    assert_eq!(reported, "2024-11-05");
    Ok(())
}

/// A client newer than this build gets a session on the newest revision
/// whose handshake exists, rather than a refusal.
///
/// This is the whole point of reading the offer. A build that answered from
/// its own pinned version could not tell this client from any other; one
/// that counter-offered `2026-07-28` would name a revision with no
/// `initialize` on it, so the peer would be told to use a protocol on which
/// the handshake it just attempted does not exist.
#[tokio::test]
async fn a_client_newer_than_this_build_gets_the_newest_handshake() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let peer = peer(version, ScopeSet::all(), CapabilitySet::all_for(version));
    let result = dispatcher()?
        .dispatch(&ctx(&peer), "initialize", &init_params("2099-01-01")?)
        .await
        .map_err(|error| Failure::Message(format!("a newer client was refused: {error}")))?;

    let reported = parsed(&result)?
        .get("protocolVersion")
        .and_then(serde_json::Value::as_str)
        .ok_or("the handshake did not report a revision")?
        .to_string();
    assert_eq!(
        reported,
        ProtocolVersion::V2025_11_25.to_string(),
        "the newest revision this build can actually complete a handshake on"
    );
    Ok(())
}

/// The reported capabilities are the features the profile allows, not a
/// second list kept beside it.
///
/// The server here declared one tool and negotiated everything, so `tools`
/// is present. What must *not* appear is anything client-side: a
/// `ServerCapabilities` object has no field for `roots` or `sampling`, and
/// a handshake that grew one would be promising a field the peer's schema
/// has no place to read.
#[tokio::test]
async fn the_handshake_advertises_only_server_capabilities() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let peer = peer(version, ScopeSet::all(), CapabilitySet::all_for(version));
    let result = dispatcher()?
        .dispatch(&ctx(&peer), "initialize", &init_params("2025-06-18")?)
        .await
        .map_err(|error| Failure::Message(format!("the handshake refused: {error}")))?;

    let capabilities = parsed(&result)?
        .get("capabilities")
        .cloned()
        .ok_or("the handshake advertised no capabilities")?;
    for forbidden in ["roots", "sampling", "elicitation"] {
        assert!(
            capabilities.get(forbidden).is_none(),
            "a client-side capability reached the server advertisement: {forbidden}"
        );
    }
    assert!(
        capabilities.get("tools").is_some(),
        "a session that negotiated tools did not advertise them"
    );
    Ok(())
}

/// The handshake is what establishes the session, so it answers on a peer
/// that negotiated no tools at all.
///
/// Gating it the way every other method is gated would mean a client
/// could never start a conversation, because a session begins with
/// capabilities rather than with any.
#[tokio::test]
async fn initialize_answers_on_a_session_with_no_features() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let peer = peer(version, ScopeSet::all(), CapabilitySet::empty());
    let outcome = dispatcher()?
        .dispatch(&ctx(&peer), "initialize", &init_params("2025-06-18")?)
        .await;

    assert!(outcome.is_ok(), "the handshake was gated off: {outcome:?}");
    Ok(())
}

/// Every refusal below is a body a client could not have meant, and they
/// are one error rather than three: a caller that distinguishes "no
/// `clientInfo`" from "no `name` inside it" is distinguishing two spellings
/// of the same mistake.
#[tokio::test]
async fn a_malformed_handshake_is_refused_whatever_is_missing() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let peer = peer(version, ScopeSet::all(), CapabilitySet::all_for(version));

    let bodies = [
        "{}",
        r#"{"capabilities":{},"clientInfo":{"name":"p","version":"0"}}"#,
        r#"{"protocolVersion":"2025-06-18","clientInfo":{"name":"p","version":"0"}}"#,
        r#"{"protocolVersion":"2025-06-18","capabilities":{}}"#,
        r#"{"protocolVersion":"2025-06-18","capabilities":[],"clientInfo":{"name":"p"}}"#,
        r#"{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{}}"#,
        r#"{"protocolVersion":"nonsense","capabilities":{},"clientInfo":{"name":"p"}}"#,
    ];
    for body in bodies {
        let outcome = dispatcher()?
            .dispatch(&ctx(&peer), "initialize", &JsonObject::new(body)?)
            .await;
        assert_eq!(
            outcome,
            Err(DispatchError::MalformedInitialize),
            "this body was accepted: {body}"
        );
    }
    Ok(())
}

/// SEP-2575 removed the handshake in `2026-07-28`, so a session on that
/// revision refuses `initialize` rather than answering a request the
/// revision has no shape for.
///
/// The refusal is `UnknownMethod` and not `MalformedInitialize`: the method
/// is not refused because the *body* is wrong, it is refused because on
/// this revision the method does not exist, and a client that migrates its
/// code to the new revision should see a different error for the same
/// request than one that typos it on an old revision.
#[tokio::test]
async fn a_session_without_the_handshake_refuses_the_method() -> Result<(), TestError> {
    let version = ProtocolVersion::V2026_07_28;
    let peer = peer(version, ScopeSet::all(), CapabilitySet::all_for(version));
    let outcome = dispatcher()?
        .dispatch(&ctx(&peer), "initialize", &init_params("2026-07-28")?)
        .await;

    assert_eq!(outcome, Err(DispatchError::UnknownMethod));
    Ok(())
}

/// A method no revision defines.
#[tokio::test]
async fn an_undefined_method_is_refused() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let peer = peer(version, ScopeSet::all(), CapabilitySet::all_for(version));
    let outcome = dispatcher()?
        .dispatch(&ctx(&peer), "tools/frobnicate", &JsonObject::default())
        .await;
    assert_eq!(outcome, Err(DispatchError::UnknownMethod));
    Ok(())
}

/// A `tools/call` naming nothing is a malformed request, not a missing tool.
///
/// The distinction is the test's whole point: `UnknownTool` would send a
/// caller looking for a tool that exists and works.
#[tokio::test]
async fn a_call_with_no_name_is_malformed_rather_than_unknown() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let peer = peer(version, ScopeSet::all(), CapabilitySet::all_for(version));
    let params = JsonObject::new(r#"{"arguments":{}}"#)?;
    let outcome = dispatcher()?
        .dispatch(&ctx(&peer), "tools/call", &params)
        .await;
    assert_eq!(outcome, Err(DispatchError::MalformedParams));
    Ok(())
}

/// `arguments` is optional, so a call with a name alone reaches the handler.
#[tokio::test]
async fn a_call_without_arguments_still_reaches_the_handler() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let peer = peer(version, ScopeSet::all(), CapabilitySet::all_for(version));
    let params = JsonObject::new(r#"{"name":"echo"}"#)?;
    let outcome = dispatcher()?
        .dispatch(&ctx(&peer), "tools/call", &params)
        .await;
    assert_eq!(
        outcome.map(|answer| answer.as_str().to_string()),
        Ok("{}".to_string())
    );
    Ok(())
}

/// A method the session may not use is refused before its params are read.
#[tokio::test]
async fn a_method_the_session_lacks_is_refused() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let peer = peer(version, ScopeSet::all(), CapabilitySet::empty());
    let outcome = dispatcher()?
        .dispatch(&ctx(&peer), "tools/list", &JsonObject::default())
        .await;
    assert_eq!(outcome, Err(DispatchError::NotCallable));
    Ok(())
}

/// `server/discover` answers on `2026-07-28` and nowhere else.
///
/// Both halves, because SEP-2575 defines it in the one revision that
/// removed the handshake and a build that answered it everywhere would be
/// offering a method no earlier spec has.
///
/// The five required members are checked by name and by presence, which is
/// what the schema requires and the only thing a result can be wrong about
/// before anything reads its contents.
#[tokio::test]
async fn discover_answers_only_where_sep_2575_defines_it() -> Result<(), TestError> {
    let modern = ProtocolVersion::V2026_07_28;
    let answered = dispatcher()?
        .dispatch(
            &ctx(&peer(modern, ScopeSet::empty(), caps(modern))),
            "server/discover",
            &JsonObject::default(),
        )
        .await
        .map_err(|error| Failure::from(error.to_string()))?;
    let parsed: serde_json::Value = serde_json::from_str(answered.as_str())
        .map_err(|error| Failure::from(error.to_string()))?;
    for member in [
        "resultType",
        "supportedVersions",
        "ttlMs",
        "cacheScope",
        "capabilities",
    ] {
        assert!(
            parsed.get(member).is_some(),
            "DiscoverResult is missing required member `{member}`",
        );
    }
    assert_eq!(
        parsed.get("resultType").and_then(serde_json::Value::as_str),
        Some("complete")
    );

    let older = ProtocolVersion::V2025_11_25;
    let refused = dispatcher()?
        .dispatch(
            &ctx(&peer(older, ScopeSet::empty(), caps(older))),
            "server/discover",
            &JsonObject::default(),
        )
        .await;
    assert_eq!(
        refused.map(|answer| answer.as_str().to_string()),
        Err(DispatchError::UnknownMethod),
        "SEP-2575 defines server/discover only from 2026-07-28",
    );
    Ok(())
}

/// The advertised capabilities are the ones this server honours.
///
/// A second list here would be a second opinion, and the suite's
/// `discover-capabilities-match-handlers` check exists precisely because
/// servers get that wrong: a client that discovers `prompts` and then
/// finds `prompts/get` refused has been told something untrue.
#[tokio::test]
async fn discover_advertises_the_profile_the_server_honours() -> Result<(), TestError> {
    let version = ProtocolVersion::V2026_07_28;
    let server_caps = CapabilitySet::empty().with(Capability::Tools);
    let peer = peer(version, ScopeSet::empty(), server_caps);
    let answered = dispatcher()?
        .dispatch(&ctx(&peer), "server/discover", &JsonObject::default())
        .await
        .map_err(|error| Failure::from(error.to_string()))?;
    let parsed: serde_json::Value = serde_json::from_str(answered.as_str())
        .map_err(|error| Failure::from(error.to_string()))?;
    let capabilities = parsed
        .get("capabilities")
        .ok_or("the result carried no capabilities")?;

    assert!(
        capabilities.get("tools").is_some(),
        "tools was negotiated and not advertised: {capabilities}",
    );
    assert!(
        capabilities.get("prompts").is_none(),
        "prompts was not negotiated and was advertised anyway: {capabilities}",
    );
    Ok(())
}

/// SEP-2549's envelope appears at `2026-07-28` and nowhere else.
///
/// Both directions, and the second is the one that matters: a build that
/// stamped `resultType` everywhere would pass the first check and break the
/// four revisions it already answers correctly, because `resultType`
/// appears in no pre-2026 result definition.
///
/// The cacheable half is checked on a listing and the non-cacheable half on
/// a call, because those are exactly the two sides of [`Cacheable`] and a
/// test that only listed would not notice a `ttlMs` on a side effect.
#[tokio::test]
async fn the_result_envelope_is_stamped_only_from_2026_07_28() -> Result<(), TestError> {
    let modern = ProtocolVersion::V2026_07_28;
    let listed = dispatcher()?
        .dispatch(
            &ctx(&peer(modern, ScopeSet::empty(), caps(modern))),
            "tools/list",
            &JsonObject::default(),
        )
        .await
        .map_err(|error| Failure::from(error.to_string()))?;
    let listed: serde_json::Value =
        serde_json::from_str(listed.as_str()).map_err(|error| Failure::from(error.to_string()))?;
    for member in ["resultType", "ttlMs", "cacheScope"] {
        assert!(
            listed.get(member).is_some(),
            "a cacheable result is missing `{member}`: {listed}",
        );
    }

    let older = ProtocolVersion::V2025_11_25;
    let legacy = dispatcher()?
        .dispatch(
            &ctx(&peer(older, ScopeSet::empty(), caps(older))),
            "tools/list",
            &JsonObject::default(),
        )
        .await
        .map_err(|error| Failure::from(error.to_string()))?;
    let legacy: serde_json::Value =
        serde_json::from_str(legacy.as_str()).map_err(|error| Failure::from(error.to_string()))?;
    for member in ["resultType", "ttlMs", "cacheScope"] {
        assert!(
            legacy.get(member).is_none(),
            "`{member}` is not defined before 2026-07-28 and was stamped anyway: {legacy}",
        );
    }
    Ok(())
}

/// `ping` answers on a session with nothing else, because that is what a
/// liveness probe on an idle session is for.
#[tokio::test]
async fn ping_answers_with_no_features_at_all() -> Result<(), TestError> {
    let version = ProtocolVersion::V2024_11_05;
    let peer = peer(version, ScopeSet::empty(), CapabilitySet::empty());
    let outcome = dispatcher()?
        .dispatch(&ctx(&peer), "ping", &JsonObject::default())
        .await;
    assert_eq!(
        outcome.map(|answer| answer.as_str().to_string()),
        Ok("{}".to_string())
    );
    Ok(())
}

/// The listing shows what the caller may see, and hides what it may not.
///
/// The caller here holds **no** scope, which is the half that makes the
/// assertion mean something — a filter that showed everything would pass
/// the next test, and one that showed nothing would pass this one.
#[tokio::test]
async fn the_listing_hides_a_tool_the_caller_may_not_see() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let peer = peer(version, ScopeSet::empty(), CapabilitySet::all_for(version));
    let listed = dispatcher()?
        .dispatch(&ctx(&peer), "tools/list", &JsonObject::default())
        .await
        .map_err(|error| Failure::from(error.to_string()))?;
    let parsed: serde_json::Value =
        serde_json::from_str(listed.as_str()).map_err(|error| Failure::from(error.to_string()))?;
    let names: Vec<&str> = parsed
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .ok_or("the listing has no tools array")?
        .iter()
        .filter_map(|tool| tool.get("name"))
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert_eq!(names, ["echo"]);
    Ok(())
}

/// The same listing, for a caller that does hold the scope.
#[tokio::test]
async fn the_listing_shows_the_tool_to_a_caller_holding_its_scope() -> Result<(), TestError> {
    let version = ProtocolVersion::V2025_06_18;
    let peer = peer(version, ScopeSet::all(), CapabilitySet::all_for(version));
    let listed = dispatcher()?
        .dispatch(&ctx(&peer), "tools/list", &JsonObject::default())
        .await
        .map_err(|error| Failure::from(error.to_string()))?;
    assert!(
        listed.as_str().contains("secret"),
        "an unrestricted caller could not see the restricted tool: {}",
        listed.as_str()
    );
    Ok(())
}

// -----------------------------------------------------------------------
// A retried call runs the tool once
// -----------------------------------------------------------------------

/// A dispatcher whose tool counts how many times it ran.
///
/// A counter rather than a `Cell<bool>`: the claim is that the tool runs
/// _once_, and a boolean cannot distinguish one from two. A handler that
/// returned a different answer each time would also work, and counting is
/// the version that says what it means.
fn counting(count: &Arc<AtomicUsize>, answer: &'static str) -> Result<Dispatcher, TestError> {
    let answer = JsonObject::new(answer).unwrap_or_default();
    let seen = Arc::clone(count);
    let handler: ToolHandler = Arc::new(move |_ctx, _args| {
        let count = Arc::clone(&seen);
        let answer = answer.clone();
        Box::pin(async move {
            count.fetch_add(1, Ordering::SeqCst);
            answer
        })
    });
    Ok(Dispatcher::new(Arc::new(
        Server::builder("test")
            .tool(charge_tool()?, handler)
            .build(),
    )))
}

/// The tool a dropped connection leaves behind, on a session that asks
/// twice with the same request id.
///
/// This is the scenario the whole store exists for. A client whose POST
/// got no answer retries, because it cannot tell a dropped connection from
/// a call that never arrived — and the retry must not be a second charge.
#[tokio::test]
async fn a_retry_after_a_dropped_connection_runs_the_tool_once() -> Result<(), TestError> {
    let count = Arc::new(AtomicUsize::new(0));
    let dispatcher = counting(&count, r#"{"charged":true}"#)?;
    let peer = peer(
        ProtocolVersion::V2025_06_18,
        ScopeSet::all(),
        caps(ProtocolVersion::V2025_06_18),
    );
    let params = JsonObject::new(r#"{"name":"charge"}"#)?;

    // The first attempt: the server runs the tool and records the answer.
    // The client never sees it — the connection dropped — which is not
    // something this layer can observe, and is the point.
    let first = dispatcher
        .dispatch(&ctx(&peer), "tools/call", &params)
        .await?;
    // The retry, same id.
    let second = dispatcher
        .dispatch(&ctx(&peer), "tools/call", &params)
        .await?;

    if count.load(Ordering::SeqCst) != 1 {
        return Err(fail(format!(
            "the tool ran {} times",
            count.load(Ordering::SeqCst)
        )));
    }
    if first != second {
        return Err(fail("the retry was not answered from the recording"));
    }
    Ok(())
}

/// The same request id on a *different* session is a different call.
///
/// Both sessions send id `r1`, which is what every client does — ids are
/// only unique per connection. A key built from the request id alone would
/// answer the second session with the first one's result, and the test
/// would still be green on every other case in this module.
#[tokio::test]
async fn the_same_request_id_on_another_session_is_not_a_retry() -> Result<(), TestError> {
    let count = Arc::new(AtomicUsize::new(0));
    let dispatcher = counting(&count, r#"{"charged":true}"#)?;
    let version = ProtocolVersion::V2025_06_18;
    let params = JsonObject::new(r#"{"name":"charge"}"#)?;

    let first_peer = peer(version, ScopeSet::all(), caps(version));
    let second_peer = peer_on(version, ScopeSet::all(), caps(version), 2);

    let _ = dispatcher
        .dispatch(&ctx(&first_peer), "tools/call", &params)
        .await?;
    let _ = dispatcher
        .dispatch(&ctx(&second_peer), "tools/call", &params)
        .await?;

    if count.load(Ordering::SeqCst) != 2 {
        return Err(fail(format!(
            "two sessions ran the tool {} time(s); the second was treated as a retry",
            count.load(Ordering::SeqCst)
        )));
    }
    Ok(())
}

/// A store that remembers the order it was driven in.
///
/// The black-box tests above prove what a retry gets; this proves the
/// *sequence* that produces it, which is the thing the ordering rule is
/// actually about. It is white-box on purpose: "the claim is released on
/// failure" has no black-box observable here, because a tool that fails at
/// the gate fails the same way whether its key was released or recorded,
/// and a test that cannot tell those apart is not testing the release.
#[derive(Debug, Default)]
struct Journal {
    log: std::sync::Mutex<Vec<&'static str>>,
}

impl Journal {
    fn entries(&self) -> Vec<&'static str> {
        self.log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// A capacity for a test store.
///
/// `unwrap_or` rather than `unwrap`, which the crate denies, and rather
/// than a `match`, which `option_if_let_else` rejects — a `NonZeroUsize`
/// has no other readable spelling. The fallback is unreachable for any
/// count a caller writes, and `NonZeroUsize::MIN` is the honest answer if
/// one does arrive: a zero-capacity store rather than a panic in a test.
fn room_for(keys: usize) -> NonZeroUsize {
    NonZeroUsize::new(keys).unwrap_or(NonZeroUsize::MIN)
}

/// A store over the real one, logging each operation.
struct Recording {
    inner: crate::adapters::secondary::idempotency::InMemoryIdempotencyStore<JsonObject>,
    log: Arc<Journal>,
}

impl crate::application::idempotency::IdempotencyStore for Recording {
    type Result = JsonObject;

    fn claim(
        &self,
        key: &crate::domain::idempotency::IdempotencyKey,
    ) -> Result<
        crate::domain::idempotency::Claim<Self::Result>,
        crate::domain::idempotency::IdempotencyError,
    > {
        self.log
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push("claim");
        self.inner.claim(key)
    }

    fn complete(
        &self,
        token: &crate::domain::idempotency::ClaimToken,
        result: Self::Result,
    ) -> Result<(), crate::domain::idempotency::IdempotencyError> {
        self.log
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push("complete");
        self.inner.complete(token, result)
    }

    fn abandon(
        &self,
        token: &crate::domain::idempotency::ClaimToken,
    ) -> Result<(), crate::domain::idempotency::IdempotencyError> {
        self.log
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push("abandon");
        self.inner.abandon(token)
    }
}

/// A recorded call is `claim` then `complete`, and nothing else.
///
/// The order is the claim: recording before executing would answer a
/// concurrent caller with a result that does not exist yet, and never
/// recording would answer a retry by running the tool again.
#[tokio::test]
async fn a_successful_call_claims_then_records() -> Result<(), TestError> {
    let journal = Arc::new(Journal::default());
    let store = Arc::new(Recording {
        inner: crate::adapters::secondary::idempotency::InMemoryIdempotencyStore::with_capacity(
            room_for(8),
        ),
        log: Arc::clone(&journal),
    });
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    let handler: ToolHandler = Arc::new(move |_ctx, _args| {
        let count = Arc::clone(&seen);
        Box::pin(async move {
            count.fetch_add(1, Ordering::SeqCst);
            JsonObject::new(r#"{"charged":true}"#).unwrap_or_default()
        })
    });
    let dispatcher = Dispatcher::new(Arc::new(
        Server::builder("test")
            .tool(charge_tool()?, handler)
            .idempotency(store)
            .build(),
    ));
    let version = ProtocolVersion::V2025_06_18;
    let peer = peer(version, ScopeSet::all(), caps(version));
    let params = JsonObject::new(r#"{"name":"charge"}"#)?;

    let _ = dispatcher
        .dispatch(&ctx(&peer), "tools/call", &params)
        .await?;
    // The retry claims again — the key is still there — and then stops,
    // because the answer is already recorded.
    let _ = dispatcher
        .dispatch(&ctx(&peer), "tools/call", &params)
        .await?;

    if journal.entries() != vec!["claim", "complete", "claim"] {
        return Err(fail(format!("the store saw {:?}", journal.entries())));
    }
    if count.load(Ordering::SeqCst) != 1 {
        return Err(fail(format!(
            "the tool ran {} times",
            count.load(Ordering::SeqCst)
        )));
    }
    Ok(())
}

/// A call that fails is `claim` then `abandon`, and the key is reusable.
///
/// The gate is the only place a `tools/call` can fail — a handler that
/// wants to report a problem returns `isError` content, which is a
/// *result*, not an error — so the session below is one that cannot see
/// the tool. The observable is what happens on the second call: a released
/// key is claimable and the gate is consulted again, and a recorded one
/// would come straight back as the recorded refusal without ever reaching
/// a handler.
#[tokio::test]
async fn a_refused_call_releases_the_key() -> Result<(), TestError> {
    let journal = Arc::new(Journal::default());
    let store = Arc::new(Recording {
        inner: crate::adapters::secondary::idempotency::InMemoryIdempotencyStore::with_capacity(
            room_for(8),
        ),
        log: Arc::clone(&journal),
    });
    let secret = Registered::from(Primitive::from(Tool::new(
        "secret",
        r#"{"type":"object"}"#,
    )?))
    .requiring_scope(Scope::Read);
    let handler: ToolHandler = Arc::new(|_ctx, _args| {
        Box::pin(async { JsonObject::new(r#"{"leaked":true}"#).unwrap_or_default() })
    });
    let dispatcher = Dispatcher::new(Arc::new(
        Server::builder("test")
            .restricted_tool(secret, handler)
            .idempotency(store)
            .build(),
    ));
    // A session with no scopes cannot see a `Read`-restricted tool.
    let version = ProtocolVersion::V2025_06_18;
    let peer = peer(version, ScopeSet::empty(), caps(version));
    let params = JsonObject::new(r#"{"name":"secret"}"#)?;

    for _ in 0..2 {
        let outcome = dispatcher
            .dispatch(&ctx(&peer), "tools/call", &params)
            .await;
        if outcome != Err(DispatchError::UnknownTool) {
            return Err(fail(format!("a hidden tool answered {outcome:?}")));
        }
    }
    // A released key is claimable, so both attempts claim it. A recorded
    // one would show `claim` twice and then nothing — but with the
    // recorded refusal returned before the handler, and the log would
    // still read the same, which is why the refusal itself is asserted
    // above rather than inferred from the log.
    if journal.entries() != vec!["claim", "abandon", "claim", "abandon"] {
        return Err(fail(format!("the store saw {:?}", journal.entries())));
    }
    Ok(())
}

/// A second request with an id the first still holds is refused, not run.
///
/// The handler parks until released, so the claim is checked while the
/// first call is genuinely mid-flight — which is the only state where
/// `InFlight` exists, and the only state where running twice is possible.
#[tokio::test]
async fn a_duplicate_while_the_first_is_running_is_refused() -> Result<(), TestError> {
    let count = Arc::new(AtomicUsize::new(0));
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
    let gate_rx = Arc::new(tokio::sync::Mutex::new(Some(gate_rx)));
    let seen = Arc::clone(&count);
    let handler: ToolHandler = Arc::new(move |_ctx, _args| {
        let count = Arc::clone(&seen);
        let gate_rx = Arc::clone(&gate_rx);
        Box::pin(async move {
            // Park until the test releases it, so the first call is
            // provably still holding its key. The guard is dropped
            // before the `if let`, not at the end of it: held across the
            // inner `await` it would be a lock this task does not
            // release, which is the deadlock `significant_drop_in_scrutinee`
            // exists to catch rather than a style note.
            let waiting = { gate_rx.lock().await.take() };
            if let Some(receiver) = waiting {
                let _ = receiver.await;
            }
            count.fetch_add(1, Ordering::SeqCst);
            JsonObject::new(r#"{"charged":true}"#).unwrap_or_default()
        })
    });
    let dispatcher = Dispatcher::new(Arc::new(
        Server::builder("test")
            .tool(charge_tool()?, handler)
            .build(),
    ));
    let peer = peer(
        ProtocolVersion::V2025_06_18,
        ScopeSet::all(),
        caps(ProtocolVersion::V2025_06_18),
    );
    let params = JsonObject::new(r#"{"name":"charge"}"#)?;

    // The spawned call borrows nothing: `Dispatcher` is `Clone` and
    // `ctx` builds a fresh context over the same session, which is what
    // makes this a genuine race rather than two calls one after another.
    let running = tokio::spawn({
        let dispatcher = Dispatcher::new(Arc::clone(dispatcher.server()));
        let peer = peer_on(
            ProtocolVersion::V2025_06_18,
            ScopeSet::all(),
            caps(ProtocolVersion::V2025_06_18),
            1,
        );
        let params = JsonObject::new(r#"{"name":"charge"}"#)?;
        async move {
            dispatcher
                .dispatch(&ctx(&peer), "tools/call", &params)
                .await
        }
    });
    // Let the spawned call reach the handler, which is where it parks.
    // Without this the duplicate could be issued first and be the one
    // that claims the key, and the test would pass for the wrong reason.
    tokio::task::yield_now().await;

    let duplicate = dispatcher
        .dispatch(&ctx(&peer), "tools/call", &params)
        .await;
    let _ = gate_tx.send(());
    let _ = running.await;

    match duplicate {
        Err(DispatchError::DuplicateInFlight(_)) => {}
        other => return Err(fail(format!("the duplicate answered {other:?}"))),
    }
    if count.load(Ordering::SeqCst) != 1 {
        return Err(fail(format!(
            "the tool ran {} time(s) while a duplicate was refused",
            count.load(Ordering::SeqCst)
        )));
    }
    Ok(())
}
