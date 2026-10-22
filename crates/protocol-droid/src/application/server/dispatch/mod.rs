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

use crate::application::mcp::initialize::server_capabilities_json;
use crate::application::mcp::input_required::{InputRound, capability_for, is_input_required};
use crate::application::server::builder::{DispatchError, Server};
use crate::application::server::context::{PeerSession, RequestContext};
use crate::domain::feature::{Feature, FeatureProfile};
use crate::domain::idempotency::{Claim, IdempotencyKey};
use crate::domain::ir::JsonObject;
use crate::domain::ir::LogLevel;
use crate::domain::registry::PrimitiveKind;
use crate::domain::{
    ProtocolVersion, SupportedVersions, client_method_defined_in, discover_defined_in,
    handshake_defined_in,
};

mod negotiate;
mod params;
mod shape;

#[cfg(test)]
mod tests;

use negotiate::negotiate;
use params::{parsed_param, split_call_params, string_param};
use shape::{stamp_result, wrap_listing};

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

/// Which of the two subscription methods a request was.
///
/// They share every step — the same gate, the same existence check, the same
/// empty answer — so one handler takes this instead of a `bool`, which would
/// make a reader guess at the argument's polarity, and two near-identical
/// handlers, which would be two places for the check to drift.
#[derive(Clone, Copy)]
enum SubscribeOp {
    /// `resources/subscribe`.
    Subscribe,
    /// `resources/unsubscribe`.
    Unsubscribe,
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

    /// The server's state map, for a transport handler that needs to
    /// hand it to a [`RequestContext`].
    ///
    /// Cloning the `Arc` is one bump; the contents of the map are not
    /// touched on the read path.
    #[must_use]
    pub fn state(&self) -> &Arc<crate::application::server::context::StateMap> {
        self.server.state()
    }

    /// Runs the handshake and returns the session narrowed to what was
    /// negotiated.
    ///
    /// [`dispatch`](Self::dispatch) answers `initialize` and discards the
    /// negotiated profile, which leaves the caller gating the rest of the
    /// connection on the profile the session was *constructed* with — a
    /// revision chosen before the peer existed. That is the right answer only
    /// when the peer happens to want the newest revision this build serves.
    ///
    /// The returned session is what the transport must carry forward: it holds
    /// the profile resolved for the revision the client actually asked for, so
    /// every later gate reads that. Transport-specific policy still belongs to
    /// the transport; the profile does not belong to the caller.
    ///
    /// # Errors
    ///
    /// As [`dispatch`](Self::dispatch) for `initialize`, and
    /// [`DispatchError::MalformedInitialize`] when the body does not carry a
    /// readable version and capability block.
    pub fn handshake(
        &self,
        ctx: &RequestContext,
        params: &JsonObject,
    ) -> Result<(JsonObject, PeerSession), DispatchError> {
        let (version, profile) = negotiate(ctx, params)?;
        let reply = self.initialize_result(version, profile)?;
        Ok((reply, ctx.peer().clone().with_profile(profile)))
    }

    /// The `InitializeResult` body for a settled negotiation.
    fn initialize_result(
        &self,
        version: ProtocolVersion,
        profile: FeatureProfile,
    ) -> Result<JsonObject, DispatchError> {
        // SEP-2575 removed the handshake in `2026-07-28`. There is no result
        // shape to build, so this is the spec's own answer — the method is not
        // defined on this revision — and not a `FeatureUnavailable` dressed up
        // as one.
        if !handshake_defined_in(version) {
            return Err(DispatchError::UnknownMethod);
        }
        let result = serde_json::json!({
            "protocolVersion": version.as_wire(),
            "capabilities": server_capabilities_json(profile),
            "serverInfo": {
                "name": self.server.name(),
                "version": self.server.version(),
            },
        });
        JsonObject::from_value(result).map_err(|_| DispatchError::MalformedInitialize)
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
        let version = ctx.peer().negotiated();
        let answered = self.route(ctx, method, params).await?;
        // `resultType` belongs on every result at 2026-07-28 and on none
        // before it, so it is stamped here rather than in each method: there
        // are ten of them and a per-method call is a call someone forgets.
        let stamped = stamp_result(answered, Cacheable::No, version);
        Ok(self.identify(stamped, version))
    }

    /// Adds `_meta["io.modelcontextprotocol/serverInfo"]` at 2026-07-28.
    ///
    /// SEP-2575: with no handshake there is no `InitializeResult` to carry
    /// `serverInfo`, so a server "SHOULD identify itself in each result's
    /// `_meta`". Older revisions carry it once, in the handshake, and their
    /// result schemas have no such key.
    fn identify(&self, result: JsonObject, version: ProtocolVersion) -> JsonObject {
        if !result_envelope_defined_in(version) {
            return result;
        }
        let Ok(serde_json::Value::Object(mut members)) =
            serde_json::from_str::<serde_json::Value>(result.as_str())
        else {
            return result;
        };
        let meta = members
            .entry("_meta")
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if let Some(meta) = meta.as_object_mut() {
            meta.insert(
                "io.modelcontextprotocol/serverInfo".to_owned(),
                serde_json::json!({
                    "name": self.server.name(),
                    "version": self.server.version(),
                }),
            );
        }
        JsonObject::from_value(serde_json::Value::Object(members)).unwrap_or(result)
    }

    /// The SEP-2322 round a retry carries, verified.
    ///
    /// Both members are optional and both are read only here. A `requestState`
    /// that does not open under this server's key is refused before any
    /// handler sees it — the handler is only ever handed a state this server
    /// issued.
    fn input_round(&self, params: &JsonObject) -> Result<InputRound, DispatchError> {
        let responses = match parsed_param(params, "inputResponses")? {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::Object(map)) => Some(map),
            Some(_) => return Err(DispatchError::MalformedParams),
        };
        let state = match parsed_param(params, "requestState")? {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(sealed)) => {
                let key = self
                    .server
                    .request_state_key()
                    .ok_or(DispatchError::TamperedRequestState)?;
                Some(
                    key.open(&sealed)
                        .ok_or(DispatchError::TamperedRequestState)?
                        .to_owned(),
                )
            }
            Some(_) => return Err(DispatchError::MalformedParams),
        };
        Ok(InputRound {
            responses,
            state,
            key: self.server.request_state_key().cloned(),
        })
    }

    /// Checks an interim result against what the request may receive.
    ///
    /// Two rules from SEP-2322 and one from SEP-2575. An `InputRequiredResult`
    /// exists only at `2026-07-28` and later, and only on `tools/call`,
    /// `prompts/get` and `resources/read` — "servers MUST NOT send
    /// `InputRequiredResult` responses on any client requests other than the
    /// supported ones". And every input request it carries needs the client
    /// capability for that request, which the client must have declared on
    /// *this* request: otherwise the answer is `-32021`.
    fn check_interim(
        ctx: &RequestContext,
        method: &str,
        result: JsonObject,
    ) -> Result<JsonObject, DispatchError> {
        let Ok(serde_json::Value::Object(members)) =
            serde_json::from_str::<serde_json::Value>(result.as_str())
        else {
            return Ok(result);
        };
        if !is_input_required(&members) {
            return Ok(result);
        }
        if !accepts_input_round(method) || !result_envelope_defined_in(ctx.peer().negotiated()) {
            return Err(DispatchError::HandlerFailed(
                "an InputRequiredResult is not defined for this request".to_owned(),
            ));
        }
        let mut required: Vec<&'static str> = Vec::new();
        if let Some(requests) = members
            .get("inputRequests")
            .and_then(serde_json::Value::as_object)
        {
            for request in requests.values() {
                let Some(capability) = request
                    .get("method")
                    .and_then(serde_json::Value::as_str)
                    .and_then(capability_for)
                else {
                    continue;
                };
                let feature = match capability {
                    "sampling" => Feature::Sampling,
                    "elicitation" => Feature::Elicitation,
                    _ => Feature::Roots,
                };
                if !ctx.peer().allows(feature) && !required.contains(&capability) {
                    required.push(capability);
                }
            }
        }
        if !required.is_empty() {
            return Err(DispatchError::MissingClientCapability { required });
        }
        Ok(result)
    }

    /// Routes one method to its handler.
    ///
    /// Split from [`dispatch`](Self::dispatch) so the result envelope is added
    /// once, on the way out, instead of at each of the ten places a result is
    /// built.
    async fn route(
        &self,
        ctx: &RequestContext,
        method: &str,
        params: &JsonObject,
    ) -> Result<JsonObject, DispatchError> {
        let version = ctx.peer().negotiated();
        // A method the negotiated revision does not define is `-32601`
        // whatever this server could otherwise do with it: answering `ping` on
        // `2026-07-28` would tell a client the method exists there.
        if !client_method_defined_in(version, method) {
            return Err(DispatchError::UnknownMethod);
        }
        let round;
        let ctx = if accepts_input_round(method) && result_envelope_defined_in(version) {
            round = ctx.clone().with_input_round(self.input_round(params)?);
            &round
        } else {
            ctx
        };
        let answered = self.route_defined(ctx, method, params).await?;
        Self::check_interim(ctx, method, answered)
    }

    /// Routes a method the negotiated revision defines.
    async fn route_defined(
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
            // SEP-2575's replacement for the handshake, and defined only where
            // the handshake is not. The two are complementary, so a revision
            // never has both.
            "server/discover" => Self::discover(ctx),
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
            "resources/read" => self.read_resource(ctx, params).await,
            "prompts/get" => self.get_prompt(ctx, params).await,
            "logging/setLevel" => Self::set_log_level(ctx, params),
            "completion/complete" => Self::complete(ctx, params),
            "resources/subscribe" => self.subscribe(ctx, params, SubscribeOp::Subscribe),
            "resources/unsubscribe" => self.subscribe(ctx, params, SubscribeOp::Unsubscribe),
            _ => Err(DispatchError::UnknownMethod),
        }
    }

    /// Serves `server/discover`: what this server speaks, and for how long a
    /// client may keep the answer.
    ///
    /// SEP-2575 removed `initialize` from `2026-07-28` and made every request
    /// carry its own version and capabilities in `_meta`. That is enough to
    /// *use* the server and not enough to *choose* one, which is the gap this
    /// fills: a client with no session and no `initialize` has no other way to
    /// learn the revisions on offer.
    ///
    /// The capabilities are the profile's own advertisement rather than a
    /// separate list, because a second list is a second opinion: a client that
    /// discovered `tools` and then found `tools/call` refused would have been
    /// told something this crate does not do.
    ///
    /// `ttlMs` is `0`. This server has no cache and re-derives the answer on
    /// every call, so advertising a lifetime it cannot honour would tell a
    /// client to reuse a result whose backing registry may since have changed.
    /// Zero is the honest value and the specification permits it: "should be
    /// considered immediately stale".
    ///
    /// `cacheScope` is `private`. The advertisement describes what this
    /// process can do, and there is nothing user-specific in it, but a
    /// capability set is also a map of what an operator has turned on, and
    /// `"public"` invites an intermediary to share it across authorization
    /// contexts. `"private"` is the safe reading of the same answer.
    fn discover(ctx: &RequestContext) -> Result<JsonObject, DispatchError> {
        if !discover_defined_in(ctx.peer().negotiated()) {
            return Err(DispatchError::UnknownMethod);
        }
        let result = serde_json::json!({
            "resultType": "complete",
            "supportedVersions": SupportedVersions::all()
                .map_err(|_| DispatchError::UnnegotiableVersion)?
                .into_iter()
                .rev()
                .map(|version| version.as_wire().into_owned())
                .collect::<Vec<_>>(),
            "ttlMs": 0,
            "cacheScope": "private",
            "capabilities": server_capabilities_json(ctx.peer().profile()),
        });
        JsonObject::from_value(result).map_err(|_| DispatchError::MalformedParams)
    }

    /// Serves `resources/subscribe` and `resources/unsubscribe`.
    ///
    /// Both answer the empty object. A subscription is recorded against the
    /// session's own sink, so [`ServerHandle::notify_resource_updated`] reaches
    /// exactly the sessions that asked, and it ends with the session.
    ///
    /// [`ServerHandle::notify_resource_updated`]: crate::application::server::ServerHandle::notify_resource_updated
    fn subscribe(
        &self,
        ctx: &RequestContext,
        params: &JsonObject,
        op: SubscribeOp,
    ) -> Result<JsonObject, DispatchError> {
        Self::require(ctx, Feature::ResourcesSubscribe)?;
        let uri = string_param(params, "uri")?;
        // The URI must be one this server lists. Accepting a subscription for
        // a resource that does not exist is how a client ends up waiting for
        // an update that can never come, and the peer cannot tell that from a
        // server that simply has nothing to report.
        if self.server.reader(&uri).is_none() {
            return Err(DispatchError::UnknownResource);
        }
        let handle = self.server.handle();
        match op {
            SubscribeOp::Subscribe => handle.watch_resource(ctx.peer().sink(), &uri, None),
            SubscribeOp::Unsubscribe => handle.unwatch_resource(ctx.peer().sink(), &uri),
        }
        Ok(JsonObject::default())
    }

    /// Serves `resources/read`.
    ///
    /// Not idempotency-keyed, unlike `tools/call`. A read has no side effect
    /// the peer can observe, so a retried read costs another read and nothing
    /// else — while a key would make two legitimate concurrent reads of the
    /// same URI collide into `DuplicateInFlight`, which is exactly wrong for a
    /// request whose whole purpose is to be repeatable.
    async fn read_resource(
        &self,
        ctx: &RequestContext,
        params: &JsonObject,
    ) -> Result<JsonObject, DispatchError> {
        Self::require(ctx, Feature::Resources)?;
        let uri = string_param(params, "uri")?;
        let reader = self
            .server
            .reader(&uri)
            .ok_or(DispatchError::UnknownResource)?;
        // One of the five cacheable results at 2026-07-28, and the only one
        // that is not a listing — which is exactly why it is marked here
        // rather than inferred from the method name.
        Ok(stamp_result(
            reader(ctx.clone(), uri).await?,
            Cacheable::Yes,
            ctx.peer().negotiated(),
        ))
    }

    /// Serves `prompts/get`.
    ///
    /// Also not idempotency-keyed, for the same reason as `read_resource`: a
    /// prompt getter produces a message list, and running it twice produces the
    /// same list twice at no cost to anybody.
    async fn get_prompt(
        &self,
        ctx: &RequestContext,
        params: &JsonObject,
    ) -> Result<JsonObject, DispatchError> {
        Self::require(ctx, Feature::Prompts)?;
        let name = string_param(params, "name")?;
        let arguments = match parsed_param(params, "arguments")? {
            None | Some(serde_json::Value::Null) => JsonObject::default(),
            Some(arguments) => {
                JsonObject::from_value(arguments).map_err(|_| DispatchError::MalformedParams)?
            }
        };
        let getter = self
            .server
            .getter(&name)
            .ok_or(DispatchError::UnknownPrompt)?;
        getter(ctx.clone(), arguments).await
    }

    /// Serves `logging/setLevel`.
    ///
    /// Answers the empty object the method's own result type is defined as,
    /// after checking the requested `level` is one the spec names. The check
    /// is here rather than ignored because a server that accepts any string
    /// reports a level it cannot honour: the client believes a filter is in
    /// place and stops sending the records it would otherwise have expected,
    /// and the peer has no way to tell "set to debug" from "set to a typo".
    ///
    /// The level is stored on the session's [`LogGate`], which every later
    /// request's context shares, so [`RequestContext::log`] filters against
    /// it from the next record on.
    ///
    /// [`LogGate`]: crate::application::server::LogGate
    fn set_log_level(
        ctx: &RequestContext,
        params: &JsonObject,
    ) -> Result<JsonObject, DispatchError> {
        Self::require(ctx, Feature::Logging)?;
        let level = string_param(params, "level")?;
        // A `level` outside the spec's own set is a client error, and answers
        // `InvalidParams` — the same answer as a missing member, because a
        // peer cannot act on either and the difference is not one it can use.
        let level = level
            .parse::<LogLevel>()
            .map_err(|_| DispatchError::MalformedParams)?;
        ctx.peer().log_gate().set(level);
        Ok(JsonObject::default())
    }

    /// Serves `completion/complete`.
    ///
    /// Returns an empty `values` array: the crate registers no completable
    /// `PromptArgument` values, so the reference is well-defined — there is
    /// nothing to offer, and the suite documents an empty array as a
    /// conforming answer.
    ///
    /// `completion` is an object holding `values`, not a bare array. An empty
    /// *array* at the top level answers `{"completion":[]}`, which is the
    /// shape a first reading of the method name suggests and the one that
    /// scores `0 passed, 2 failed` — the scenario reads
    /// `r.completion.values`, finds `[]` has no member `values`, and reports
    /// `Missing values array in completion`. The empty case is the one where
    /// the distinction is invisible: both a missing `values` and an empty one
    /// mean "no completions", so the error names a missing array that was
    /// never absent, only wrapped wrongly.
    fn complete(ctx: &RequestContext, _params: &JsonObject) -> Result<JsonObject, DispatchError> {
        Self::require(ctx, Feature::Completions)?;
        JsonObject::new(r#"{"completion":{"values":[]}}"#)
            .map_err(|_| DispatchError::MalformedParams)
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
    /// malformed handshake matters. The version the session speaks is settled
    /// by [`negotiate_server`] against **the client's own offer**, which is
    /// why [`check_initialize_params`] returns it rather than discarding it.
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
        let (version, profile) = negotiate(ctx, params)?;
        self.initialize_result(version, profile)
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
        Ok(stamp_result(
            wrap_listing(key, &listing),
            Cacheable::Yes,
            ctx.peer().negotiated(),
        ))
    }

    /// `tools/call`, through the server's own dispatch.
    /// Runs one `tools/call`, at most once per request id.
    ///
    /// The order here is not a style choice; it is the claim:
    ///
    /// 1. **Check.** Claim the key before anything runs. Claiming after would
    ///    leave a window in which two callers both see an absent key.
    /// 2. **Execute.** Only the caller that was handed the token runs the
    ///    tool. A caller that finds a completed key answers from the recording
    ///    and never reaches the handler, and one that finds an in-flight key
    ///    is refused rather than queued — see [`DispatchError::DuplicateInFlight`].
    /// 3. **Persist.** Record the result, or release the claim, before
    ///    returning. A result that is returned but not recorded is a result a
    ///    retry will not find, and a key that is neither recorded nor released
    ///    is a key nobody can ever claim again.
    ///
    /// # Why a failed call releases rather than records
    ///
    /// An `Err` here means the tool produced no result — the gate refused it,
    /// or the handler failed. There is nothing to replay, so recording it would
    /// hand every future retry the same refusal for a condition that may have
    /// been fixed, and would make a transient failure permanent. Releasing
    /// makes the next attempt a fresh one, which is what a caller sending the
    /// same id again is asking for.
    ///
    /// The consequence is that a tool which *ran* and then failed is retried.
    /// That is the honest answer: this layer cannot tell a tool that had no
    /// effect from one that charged a card and then failed to answer, and
    /// guessing "no effect" would silently drop work. A tool that must not
    /// run twice needs a key of its own — which is the tool's own port, not
    /// this one.
    ///
    /// # What a dropped connection costs
    ///
    /// A client whose POST never got an answer retries with the same request
    /// id. It gets the recorded result, and the tool ran once. This is the
    /// only reason the store is on `Server` rather than left to the transport:
    /// the transport sees a dropped socket and knows nothing about what the
    /// tool did, and only the layer that ran it can answer.
    async fn call_tool(
        &self,
        ctx: &RequestContext,
        params: &JsonObject,
    ) -> Result<JsonObject, DispatchError> {
        Self::require(ctx, Feature::Tools)?;
        let (name, arguments) = split_call_params(params)?;

        // A request id is only unique within a session — every client numbers
        // its first request `1` — so the key carries both. Keying on the id
        // alone would answer one session's call with another's result.
        //
        // It also has to carry *what* was asked. A request id is a label for
        // one request, not a promise that the same id always names the same
        // operation: JSON-RPC 2.0 lets a peer reuse ids across a sequence, and
        // the conformance suite's client restarts its counter per scenario
        // against a connection it never closes. Keyed on the id alone, the
        // second `tools/call` a session receives is answered with the first
        // one's recording — a valid `CallToolResult` carrying another tool's
        // output, which the caller has no way to detect. Repeating the *same*
        // tool still replays, which is the case a retry needs.
        let key = IdempotencyKey::for_method_call(
            &ctx.peer().session(),
            ctx.request_id(),
            "tools/call",
            &name,
        );
        let token = match self.server.idempotency().claim(&key)? {
            // Recorded already. The answer is the recording, and the tool does
            // not run.
            Claim::Completed(recorded) => return Ok(recorded),
            // Someone else holds it. Refusing is what keeps one side effect
            // from running twice; the holder is a real request.
            Claim::InFlight => {
                return Err(DispatchError::DuplicateInFlight(
                    ctx.request_id().to_string(),
                ));
            }
            Claim::Claimed { token } => token,
        };

        let outcome = self.server.call(ctx, &name, arguments).await;
        match outcome {
            Ok(result) => {
                // The result is both the answer and the recording, and
                // `complete` takes it by value, so it is cloned out. `JsonObject`
                // holds its bytes in an `Arc`, so this is a pointer bump and
                // not a copy of the document.
                self.server.idempotency().complete(&token, result.clone())?;
                Ok(result)
            }
            Err(error) => {
                // Release before returning, so the key is claimable again. A
                // failed record or release is not swallowed: it is reported,
                // because a key stuck in `Claimed` is invisible to every later
                // caller and looks exactly like a server that stopped working.
                self.server.idempotency().abandon(&token)?;
                Err(error)
            }
        }
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




/// Wraps a listing's visible entries under `key`.
///
/// The document is assembled from values this module built, so the only way it
/// could be invalid is a bug here. It falls back to `{}` rather than
/// propagating because a listing that cannot be encoded should read as empty
/// to the caller rather than take the whole response down.
/// Whether a result carries SEP-2549's caching hints.
///
/// A second parameter to [`stamp_result`] rather than a flag on every method:
/// the schema draws the line itself, and this names which side a result is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cacheable {
    /// `ttlMs` and `cacheScope` belong on this result.
    Yes,
    /// They do not — a `CallToolResult` is not something to cache.
    No,
}


/// Whether `method` may answer with a SEP-2322 `InputRequiredResult`.
///
/// `tasks/result` is the fourth the SEP names; it belongs to the tasks
/// extension, which this crate does not serve.
fn accepts_input_round(method: &str) -> bool {
    matches!(method, "tools/call" | "prompts/get" | "resources/read")
}


/// Whether SEP-2549's result envelope exists in this revision.
#[must_use]
pub const fn result_envelope_defined_in(version: ProtocolVersion) -> bool {
    matches!(
        version,
        ProtocolVersion::V2026_07_28 | ProtocolVersion::Unknown(_)
    )
}
