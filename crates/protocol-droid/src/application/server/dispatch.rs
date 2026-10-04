//! Routing a method name to the handler that serves it.
//!
//! # Why this is not on `Server`
//!
//! `Server::call` takes a tool name, because a tool call is the only thing a
//! handler can answer. A peer, though, sends a _method_ name, and several
//! methods never reach a tool handler at all — `tools/list` and `ping` have no
//! handler to dispatch to. The method table lives here rather than on
//! `Server` so the two stay apart: the builder is how a server is described,
//! this is how a request against it is routed.
//!
//! # What the result is, and is not
//!
//! The result is an **IR-level** [`JsonObject`], not a wire frame. There is no
//! request-side codec yet — `Codec` lifts and lowers a `ToolResult` and nothing
//! else — so a result built here is the canonical form that T13/T14's transport
//! will then lower for whichever revision negotiated. The in-process test that
//! exercises this says the same thing in its own doc comment rather than
//! pretending to cover the wire.
//!
//! # Which fields a listing carries
//!
//! Only the ones **every** revision expresses. `title` and `icons` arrived in
//! `2026-07-28`, so emitting them unconditionally would hand a `2024-11-05`
//! peer a field its revision has no name for. Gating a field on a revision is
//! the codec's job and belongs with it; this module stays the common
//! denominator, and the omission is recorded rather than papered over.

use std::sync::Arc;

use crate::application::registry::listing::Listing;
use crate::application::server::builder::{DispatchError, Server};
use crate::application::server::context::RequestContext;
use crate::domain::feature::{Feature, FeatureProfile};
use crate::domain::ir::JsonObject;
use crate::domain::registry::{Primitive, PrimitiveKind, PromptArgument, Registered};
use crate::domain::{
    Capability, ProtocolVersion, ServerDecision, SupportedVersions, handshake_defined_in,
    negotiate_server,
};

/// Routes methods to a server's handlers.
#[derive(Clone)]
pub struct Dispatcher {
    server: Arc<Server>,
}

impl core::fmt::Debug for Dispatcher {
    /// Prints the server's name and tool count.
    ///
    /// For the same reason `PeerSession` does: the registry behind it is a
    /// large snapshot, and a post-mortem wants to know which server it was.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Dispatcher")
            .field("name", &self.server.name())
            .field("tool_count", &self.server.tool_count())
            .finish_non_exhaustive()
    }
}

impl Dispatcher {
    /// Routes to `server`.
    #[must_use]
    pub const fn new(server: Arc<Server>) -> Self {
        Self { server }
    }

    /// The server requests are routed to.
    #[must_use]
    pub const fn server(&self) -> &Arc<Server> {
        &self.server
    }

    /// Routes one request.
    ///
    /// The gate runs before anything is read out of `params`, so a method the
    /// session cannot use does not even parse its arguments — the same order
    /// `RequestContext`'s own gated calls use, and for the same reason.
    ///
    /// # Errors
    ///
    /// [`DispatchError::UnknownMethod`] for a name no method answers, and
    /// [`DispatchError::NotCallable`] for a method whose feature the session
    /// does not have. Both are refusals that wrote nothing.
    pub async fn dispatch(
        &self,
        ctx: &RequestContext,
        method: &str,
        params: &JsonObject,
    ) -> Result<JsonObject, DispatchError> {
        match method {
            // `initialize` is the one method with no gate, and for the
            // opposite reason to `ping`. `ping` may be answered on a session
            // that already exists; `initialize` is the request that *creates*
            // the session, so consulting the feature profile to decide whether
            // to serve it would ask the session whether it exists and take the
            // answer from something that has not been built yet. The
            // negotiation that does decide it has already happened — the
            // revision in the result is the one the session settled on — so
            // the gate has nothing left to contribute here.
            //
            // The one thing it *does* still have to contribute is which
            // revision that is: on `2026-07-28` there is no handshake to
            // perform, and the handler refuses rather than inventing one.
            "initialize" => self.initialize(ctx, params),
            // `ping` is defined in every revision that has a session and
            // answers on an otherwise idle one, so gating it would refuse the
            // request it exists for.
            "ping" => Ok(JsonObject::default()),
            "tools/list" => self.list(ctx, PrimitiveKind::Tool, "tools"),
            "resources/list" => self.list(ctx, PrimitiveKind::Resource, "resources"),
            "resources/templates/list" => {
                self.list(ctx, PrimitiveKind::ResourceTemplate, "resourceTemplates")
            }
            "prompts/list" => self.list(ctx, PrimitiveKind::Prompt, "prompts"),
            "tools/call" => self.call_tool(ctx, params).await,
            _ => Err(DispatchError::UnknownMethod),
        }
    }

    /// Answers the handshake: this server, the revision it speaks, and what it
    /// can do.
    ///
    /// The result is the IR form of `InitializeResult` — `protocolVersion`,
    /// `capabilities`, `serverInfo` — which are the three fields every
    /// revision that has a handshake marks required. `instructions` is omitted
    /// rather than emitted empty: it is optional, it is a human-facing hint
    /// with no source on a [`Server`], and an empty string is worse than
    /// absent, because a client that reads it has been told the server has no
    /// instructions when what it means is that this server supplies none.
    ///
    /// # What decides the revision
    ///
    /// The session's, not the request's. `params.protocolVersion` is what the
    /// client *offered*; by the time a request reaches a dispatcher the layer
    /// that owns negotiation has already settled the revision, and the session
    /// is running on it. Answering with the offer instead would let a client
    /// move a session onto a revision the transport never negotiated for it,
    /// and the profile gating every other method would be one revision ahead of
    /// the wire.
    ///
    /// The offer is still read and still checked, for the two reasons a
    /// malformed handshake matters. And the version this build can actually
    /// serve is settled by [`negotiate_server`], which is the function that
    /// already answers that question everywhere else in the crate — a
    /// `CounterOffered` decision here means the session is on a revision with
    /// no wire types behind it, which is a state worth reporting rather than
    /// answering around.
    ///
    /// # What decides `capabilities`
    ///
    /// [`FeatureProfile::advertised_by_server`] — the profile, read back. The
    /// advertisement is a fold over the same table every feature gate is a fold
    /// over, so it cannot claim a capability the session will not act on, and
    /// it cannot decline one it will.
    ///
    /// # Errors
    ///
    /// - [`DispatchError::UnnegotiableVersion`] when the session is on a
    ///   revision this build has no wire types for.
    /// - [`DispatchError::UnknownMethod`] when the session's revision has no
    ///   handshake at all — `2026-07-28` and `draft` removed `initialize` and
    ///   its result, so a request for it on one of those revisions is asking
    ///   for a method the revision does not define.
    /// - [`DispatchError::MalformedInitialize`] when `params` is not an object
    ///   with a string `protocolVersion` that parses, a `capabilities` object,
    ///   and a `clientInfo` carrying a `name`.
    fn initialize(
        &self,
        ctx: &RequestContext,
        params: &JsonObject,
    ) -> Result<JsonObject, DispatchError> {
        // `negotiate_server` is the crate's existing answer to "can this build
        // serve that revision", and the `Echoed` arm is the one a healthy
        // session takes. Reaching `CounterOffered` means the session is on a
        // version outside `SupportedVersions::all()` — an `Unknown`, which
        // `resolve` also treats as defining nothing.
        let supported = SupportedVersions::all().map_err(|_| DispatchError::UnnegotiableVersion)?;
        let version = match negotiate_server(ctx.peer().negotiated(), &supported) {
            ServerDecision::Echoed(version) => version,
            ServerDecision::CounterOffered { .. } => {
                return Err(DispatchError::UnnegotiableVersion);
            }
        };

        // SEP-2575 removed the handshake in `2026-07-28`. There is no result
        // shape to build, so this is the spec's own answer — the method is not
        // defined on this revision — and not a `FeatureUnavailable` dressed up
        // as one.
        if !handshake_defined_in(version) {
            return Err(DispatchError::UnknownMethod);
        }

        // Checked for shape, deliberately not read for value: see
        // `check_initialize_params` for why the answer is the server's own
        // rather than an echo of the request.
        check_initialize_params(params)?;

        let result = serde_json::json!({
            "protocolVersion": version.as_wire(),
            "capabilities": server_capabilities_json(ctx.peer().profile()),
            "serverInfo": {
                "name": self.server.name(),
                "version": self.server.version(),
            },
        });
        JsonObject::from_value(result).map_err(|_| DispatchError::MalformedInitialize)
    }

    /// One of the four listing methods.
    ///
    /// All four are the same operation against a different kind, so they are
    /// one function with the feature and the key as parameters rather than
    /// four near-identical bodies.
    fn list(
        &self,
        ctx: &RequestContext,
        kind: PrimitiveKind,
        key: &str,
    ) -> Result<JsonObject, DispatchError> {
        Self::require(ctx, kind.feature())?;
        let listing =
            self.server
                .registry()
                .list_visible(ctx.peer().auth(), &ctx.peer().profile(), kind);
        Ok(wrap_listing(key, &listing))
    }

    /// `tools/call`, through the server's own dispatch.
    async fn call_tool(
        &self,
        ctx: &RequestContext,
        params: &JsonObject,
    ) -> Result<JsonObject, DispatchError> {
        Self::require(ctx, Feature::Tools)?;
        let (name, arguments) = split_call_params(params)?;
        self.server.call(ctx, &name, arguments).await
    }

    /// The gate every method shares, in one place.
    ///
    /// Separate from `RequestContext`'s own private `require`, which returns a
    /// `GatedError` naming the caller. A method is not a tool, so the refusal
    /// here is a `DispatchError` and the two types do not merge — a caller
    /// matching on the distinction can tell "you may not call this method"
    /// from "this tool needs something you lack".
    ///
    /// An associated function rather than a method because it reads nothing
    /// from the dispatcher: it asks the session, and a gate that could also
    /// consult the server is a gate with a second opinion.
    fn require(ctx: &RequestContext, feature: Feature) -> Result<(), DispatchError> {
        if ctx.peer().allows(feature) {
            return Ok(());
        }
        Err(DispatchError::NotCallable)
    }
}

/// Checks that `initialize` params are the shape the handshake has to be.
///
/// Every revision that has a handshake marks `capabilities`, `clientInfo` and
/// `protocolVersion` required on `InitializeRequestParams`, and `Implementation`
/// marks `name` and `version` required inside `clientInfo`. Each is read and
/// type-checked here, which is what "not a silently-ignored field" means: a
/// missing or misshapen required field is a refusal, not a default.
///
/// # Why this returns nothing
///
/// A check, not a parse, and the distinction is worth stating because the
/// crate's rule is to parse rather than validate. It returns nothing because
/// nothing in the answer comes from the request: `protocolVersion` is the
/// session's revision, `capabilities` is the session's profile, and
/// `serverInfo` is this server. The offer is a statement about the *client*,
/// and the layer that owns that is the one that negotiated the session — it
/// already holds the client's capability set and, on a `NegotiatedSession`,
/// the client's [`PeerInfo`]. Reading it again here to build a value nobody
/// consumes would be the second source of truth, not the first.
///
/// The one field whose parse *is* a decision: `protocolVersion` is run through
/// [`ProtocolVersion`]'s `FromStr`, so a client offering a revision this build
/// has never heard of — which the specification explicitly allows, and which
/// [`negotiate_server`] answers with the server's own version rather than an
/// error — still gets a session, while a string that is not a `YYYY-MM-DD`
/// date is a typo and is refused.
///
/// # Errors
///
/// [`DispatchError::MalformedInitialize`] when the body is not an object,
/// when `protocolVersion` is absent, is not a string, or does not parse as a
/// protocol version; when `capabilities` is absent or is not an object; or
/// when `clientInfo` is absent, is not an object, or has no `name`.
///
/// `clientInfo.version` is the one required field not enforced. It is
/// free-form, nothing in this crate's answer depends on it, and refusing a
/// handshake over a missing version string would buy a stricter reading of one
/// schema at the cost of every client that omits it.
fn check_initialize_params(params: &JsonObject) -> Result<(), DispatchError> {
    let parsed: serde_json::Value =
        serde_json::from_str(params.as_str()).map_err(|_| DispatchError::MalformedInitialize)?;
    let object = parsed
        .as_object()
        .ok_or(DispatchError::MalformedInitialize)?;

    object
        .get("protocolVersion")
        .and_then(serde_json::Value::as_str)
        .ok_or(DispatchError::MalformedInitialize)?
        .parse::<ProtocolVersion>()
        .map_err(|_| DispatchError::MalformedInitialize)?;

    if !object
        .get("capabilities")
        .is_some_and(serde_json::Value::is_object)
    {
        return Err(DispatchError::MalformedInitialize);
    }

    object
        .get("clientInfo")
        .filter(|value| value.is_object())
        .and_then(|client| client.get("name"))
        .filter(|name| name.is_string())
        .ok_or(DispatchError::MalformedInitialize)?;

    Ok(())
}

/// How one server capability appears in a `ServerCapabilities` object.
///
/// The vendored schema nests — `{"tools": {"listChanged": true}}`, not a flat
/// `{"tools": true, "toolsListChanged": true}` — so each row is the key it
/// sits under plus the key inside that object, if it has one. A parent with
/// no child still emits its own empty object, because `tools` is present
/// exactly when the server has tools and the presence is the whole signal.
///
/// Data rather than a `match` for the same reason [`FEATURES`] is data: a
/// capability added to the domain without a row here would otherwise be
/// dropped silently, and the test that walks the table against every
/// server-side capability is what keeps the two in step.
///
/// [`FEATURES`]: crate::domain::FEATURES
const SERVER_CAPABILITY_WIRE: &[(Capability, &str, Option<&str>)] = &[
    (Capability::Experimental, "experimental", None),
    (Capability::Tools, "tools", None),
    (Capability::ToolsListChanged, "tools", Some("listChanged")),
    (Capability::Resources, "resources", None),
    (
        Capability::ResourcesSubscribe,
        "resources",
        Some("subscribe"),
    ),
    (
        Capability::ResourcesListChanged,
        "resources",
        Some("listChanged"),
    ),
    (Capability::Prompts, "prompts", None),
    (
        Capability::PromptsListChanged,
        "prompts",
        Some("listChanged"),
    ),
    (Capability::Completions, "completions", None),
    (Capability::Logging, "logging", None),
    (Capability::Tasks, "tasks", None),
];

/// Renders the session's server capabilities as a `ServerCapabilities` object.
///
/// Reads [`FeatureProfile::advertised_by_server`] rather than being handed a
/// set, so there is no way to reach this function with a capability set that
/// did not come from the profile. That is the drift this exists to prevent: a
/// caller that passed in its own set would be free to advertise a flag the
/// gates behind it would refuse, and the peer would have been told otherwise
/// at connect time.
///
/// A capability in the derived set with no row in [`SERVER_CAPABILITY_WIRE`]
/// is not emitted. Two are in that position and neither is reachable: the
/// client-side ones, which [`advertised_by_server`] cannot produce because
/// their feature rows are `Side::Client`, and
/// [`Capability::Extensions`][extensions], which only `2026-07-28` defines —
/// a revision the handler has already refused before getting here, because
/// that revision has no `InitializeResult` to put it in.
fn server_capabilities_json(profile: FeatureProfile) -> serde_json::Value {
    let advertised = profile.advertised_by_server();
    let mut root = serde_json::Map::new();

    for (capability, key, child) in SERVER_CAPABILITY_WIRE {
        if !advertised.contains(*capability) {
            continue;
        }
        let entry = root
            .entry((*key).to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        let serde_json::Value::Object(parent) = entry else {
            continue;
        };
        if let Some(child) = child {
            parent.insert((*child).to_string(), serde_json::Value::Bool(true));
        }
    }

    serde_json::Value::Object(root)
}

/// Reads `name` and `arguments` out of a `tools/call` params object.
///
/// A missing `name` is a malformed request rather than an unknown tool: the
/// call named nothing at all, and reporting `UnknownTool` for it would send a
/// caller hunting for a tool that exists and works.
///
/// # Errors
///
/// [`DispatchError::MalformedParams`] when the body is not an object, when
/// `name` is absent or is not a string, and when `arguments` is present but is
/// not an object. `arguments` is optional — a tool taking no parameters is
/// called with none.
fn split_call_params(params: &JsonObject) -> Result<(String, JsonObject), DispatchError> {
    let parsed: serde_json::Value =
        serde_json::from_str(params.as_str()).map_err(|_| DispatchError::MalformedParams)?;
    let object = parsed.as_object().ok_or(DispatchError::MalformedParams)?;

    let name = object
        .get("name")
        .and_then(serde_json::Value::as_str)
        .ok_or(DispatchError::MalformedParams)?
        .to_string();

    let arguments = match object.get("arguments") {
        None | Some(serde_json::Value::Null) => JsonObject::default(),
        Some(arguments) => {
            JsonObject::from_value(arguments.clone()).map_err(|_| DispatchError::MalformedParams)?
        }
    };

    Ok((name, arguments))
}

/// Wraps a listing's visible entries under `key`.
///
/// The document is assembled from values this module built, so the only way it
/// could be invalid is a bug here. It falls back to `{}` rather than
/// propagating because a listing that cannot be encoded should read as empty
/// to the caller rather than take the whole response down.
fn wrap_listing(key: &str, listing: &Listing) -> JsonObject {
    let entries: Vec<serde_json::Value> = listing
        .visible()
        .iter()
        .map(|entry| listing_entry(entry))
        .collect();
    JsonObject::from_value(serde_json::json!({ key: entries })).unwrap_or_default()
}

/// The common-denominator JSON for one registered primitive.
///
/// The match is exhaustive, which is the whole point: a new `Primitive` variant
/// fails to compile in this function rather than silently vanishing from every
/// listing. That is what the exhaustiveness buys, and it needs no `Option` to
/// buy it — an `Option` here would only invite a `filter_map` to swallow a
/// future `None` quietly, undoing the check.
fn listing_entry(registered: &Registered) -> serde_json::Value {
    match registered.primitive() {
        Primitive::Tool(tool) => {
            let mut entry = serde_json::json!({
                "name": tool.name.as_str(),
                "description": tool.description,
            });
            if let (serde_json::Value::Object(map), Ok(schema)) = (
                &mut entry,
                serde_json::from_str::<serde_json::Value>(tool.input_schema.as_str()),
            ) {
                // Embedded as a value, not as a string. `json!` will not do
                // this for us: `"inputSchema": text.as_str()` produces a JSON
                // *string* whose characters happen to be JSON, so every peer
                // would read a schema where the specification says an object,
                // and the failure would surface in the client as a tool with
                // no arguments rather than here.
                map.insert("inputSchema".to_string(), schema);
            }
            entry
        }
        Primitive::Resource(resource) => serde_json::json!({
            "uri": resource.uri.as_str(),
            "name": resource.name,
            "description": resource.description,
            "mimeType": resource.mime_type,
        }),
        Primitive::ResourceTemplate(template) => serde_json::json!({
            "uriTemplate": template.uri_template.as_str(),
            "name": template.name,
            "description": template.description,
            "mimeType": template.mime_type,
        }),
        Primitive::Prompt(prompt) => serde_json::json!({
            "name": prompt.name.as_str(),
            "description": prompt.description,
            "arguments": prompt.arguments.iter().map(prompt_argument).collect::<Vec<_>>(),
        }),
    }
}

/// One prompt argument, as JSON.
///
/// Written out rather than derived, because `PromptArgument` is a domain type
/// with no `Serialize`: it exists to be lowered by a codec, and deriving
/// `Serialize` on it here would make the wire shape the domain's own opinion
/// instead of the codec's. `title` is skipped for the same reason the
/// other listings skip it — `2026-07-28` only.
fn prompt_argument(argument: &PromptArgument) -> serde_json::Value {
    serde_json::json!({
        "name": argument.name,
        "description": argument.description,
        "required": argument.required,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

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
    use crate::domain::{CapabilitySet, ProtocolVersion, RequestId, TransportKind};

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

    impl From<serde_json::Error> for Failure {
        fn from(error: serde_json::Error) -> Self {
            Self::Result(error)
        }
    }

    type TestError = Failure;

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
            Arc::new(NoExchange),
            Arc::new(NoSink),
        )
        .with_auth(AuthContext::new("tester", scopes))
    }

    /// A request on `peer`.
    fn ctx(peer: &PeerSession) -> RequestContext {
        RequestContext::for_request(peer, RequestId::text("r1"), CancellationToken::new(), None)
    }

    /// `initialize` params for a client that offers `version`.
    fn init_params(version: &str) -> Result<JsonObject, TestError> {
        Ok(JsonObject::new(
            r#"{"protocolVersion":""#.to_string()
                + version
                + r#"","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}"#,
        )?)
    }

    /// An IR object read back as a `serde_json` value.
    fn parsed(object: &JsonObject) -> Result<serde_json::Value, TestError> {
        Ok(serde_json::from_str(object.as_str())?)
    }

    /// The handshake answers with the revision the session negotiated, not the
    /// one the client asked for.
    ///
    /// The distinction is the test's whole point: a client offering a version
    /// this build cannot speak has already been negotiated down by the time a
    /// request reaches here, so echoing the *request* would tell the peer it
    /// got the revision it asked for when it did not.
    #[tokio::test]
    async fn initialize_reports_the_negotiated_revision() -> Result<(), TestError> {
        let version = ProtocolVersion::V2025_06_18;
        let peer = peer(version, ScopeSet::all(), CapabilitySet::all_for(version));
        // The client offers 2024-11-05; the session is on 2025-06-18.
        let result = dispatcher()?
            .dispatch(&ctx(&peer), "initialize", &init_params("2024-11-05")?)
            .await
            .map_err(|error| Failure::Message(format!("the handshake refused: {error}")))?;

        let reported = parsed(&result)?
            .get("protocolVersion")
            .and_then(serde_json::Value::as_str)
            .ok_or("the handshake did not report a revision")?
            .to_string();
        assert_eq!(reported, version.to_string());
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
        let parsed: serde_json::Value = serde_json::from_str(listed.as_str())
            .map_err(|error| Failure::from(error.to_string()))?;
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
}
