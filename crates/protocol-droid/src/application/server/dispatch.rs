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
use crate::domain::feature::Feature;
use crate::domain::ir::JsonObject;
use crate::domain::registry::{Primitive, PrimitiveKind, PromptArgument, Registered};

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
            // `ping` is defined in every revision and answers on an otherwise
            // idle session, so gating it would refuse the request it exists
            // for. That is why it is the one method here with no gate.
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
